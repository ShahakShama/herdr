//! Drift detection for PR-review worktrees.
//!
//! A workspace opened to review someone else's PR is checked out on the PR head
//! branch and its review row diffs that worktree against `origin/<base>`. Both
//! the remote head (the PR author keeps pushing) and the remote base (someone
//! merges into the base branch) move out from under it over time, so a review
//! that was current when opened silently goes stale.
//!
//! This module powers the periodic `git fetch origin <base> <head>` (driven off
//! the same cadence as the PR-status poll) that freshens the remote-tracking
//! refs and then reports, per review worktree:
//!   * **head drift** — the worktree HEAD has fallen *behind* `origin/<head>`
//!     (the PR target advanced upstream); surfaced as a badge in the agents pane.
//!     Local commits *ahead* of the remote are the reviewer's own in-progress
//!     work, not drift, and never raise the badge.
//!   * **base moved** — `origin/<base>` advanced past the commit the open review
//!     row's `vimrev` was launched against; surfaced as a "press alt+R" badge.
//!
//! All git here is read-only (`fetch`/`rev-parse`/`rev-list`); it never touches
//! the worktree's working tree, so in-flight `CLAUDE:` notes are safe.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How far a PR-review worktree has drifted from the remote, as computed by the
/// periodic fetch. The all-false value means "in sync"; callers store `None`
/// rather than a clean drift so the agents pane only badges genuine drift.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrReviewDrift {
    /// `origin/<head>` is ahead of the worktree HEAD by this many commits (the
    /// PR author pushed since the review opened). 0 when not behind.
    pub head_behind: usize,
    /// The worktree HEAD is behind `origin/<head>` (the PR target advanced
    /// upstream and the review no longer reflects it). Equivalent to
    /// `head_behind > 0`; local commits *ahead* of the remote are the reviewer's
    /// own in-progress work and deliberately do not set this.
    pub head_drifted: bool,
    /// `origin/<base>` advanced past the commit the open review row diffs
    /// against — the diff base is stale; a fresh `alt+R` re-targets it.
    pub base_moved: bool,
}

impl PrReviewDrift {
    /// Whether the worktree is fully in sync (no badge needed).
    pub fn is_clean(&self) -> bool {
        !self.head_drifted && !self.base_moved
    }
}

/// One PR-review worktree to check, captured on the UI thread so the worker
/// thread touches no shared state.
#[derive(Debug, Clone)]
pub struct PrReviewDriftItem {
    pub workspace_id: String,
    /// The repo's main worktree (where `origin/*` refs live and `fetch` runs).
    pub repo_root: PathBuf,
    /// The review worktree (where HEAD is the PR head checkout).
    pub checkout_path: PathBuf,
    pub base_branch: String,
    pub head_branch: String,
    /// `origin/<base>` oid the open review row's `vimrev` was launched against
    /// (`None` when no review row is open or the spawn oid is unknown — then
    /// base drift can't be judged and is reported as not-moved).
    pub review_base_oid: Option<String>,
}

/// Result for one item: the workspace it belongs to plus its computed drift.
#[derive(Debug, Clone)]
pub struct PrReviewDriftOutcome {
    pub workspace_id: String,
    pub drift: PrReviewDrift,
}

/// Fetch `origin <base> <head>` for each item, then compute its drift locally.
/// Blocking — call off the UI thread. A per-item git failure yields a clean
/// (no-drift) outcome rather than a false alarm.
pub fn refresh_pr_review_drift(items: &[PrReviewDriftItem]) -> Vec<PrReviewDriftOutcome> {
    items
        .iter()
        .map(|item| PrReviewDriftOutcome {
            workspace_id: item.workspace_id.clone(),
            drift: drift_for(item),
        })
        .collect()
}

/// Freshen the remote refs for one review worktree and read back the oids that
/// decide its drift. Git failures degrade to "unknown" (a `None` oid), which
/// [`compute_drift`] treats as no drift.
fn drift_for(item: &PrReviewDriftItem) -> PrReviewDrift {
    // Best-effort fetch; if it fails (offline, fork head not on origin) we still
    // compare against whatever the last fetch left in the remote-tracking refs.
    let _ = git(
        &item.repo_root,
        &["fetch", "origin", &item.base_branch, &item.head_branch],
    );
    let origin_head = format!("origin/{}", item.head_branch);
    let origin_base = format!("origin/{}", item.base_branch);
    let local_head = git(&item.checkout_path, &["rev-parse", "HEAD"]);
    let remote_head = git(&item.repo_root, &["rev-parse", &origin_head]);
    let remote_base = git(&item.repo_root, &["rev-parse", &origin_base]);
    // Count only how far origin/<head> is *ahead* of the worktree (HEAD..origin):
    // commits the worktree is missing. Local commits ahead of the remote are the
    // reviewer's own work and don't count as drift, so they're never measured.
    let head_behind = match (&local_head, &remote_head) {
        (Some(local), Some(remote)) if local != remote => git(
            &item.checkout_path,
            &["rev-list", "--count", &format!("HEAD..{origin_head}")],
        )
        .and_then(|out| out.parse().ok())
        .unwrap_or(0),
        _ => 0,
    };
    compute_drift(
        head_behind,
        item.review_base_oid.as_deref(),
        remote_base.as_deref(),
    )
}

/// Pure drift decision, split out so it can be unit-tested without git. Head
/// drift is reported only when the worktree is *behind* `origin/<head>` (the PR
/// target advanced upstream); a worktree that is merely *ahead* with the
/// reviewer's own unpushed commits is normal work in progress, not drift. A
/// `None` remote base (fetch/rev-parse failed) is treated as "can't tell" → no
/// base drift, so a transient git error never raises a false badge.
fn compute_drift(
    head_behind: usize,
    review_base_oid: Option<&str>,
    remote_base: Option<&str>,
) -> PrReviewDrift {
    let head_drifted = head_behind > 0;
    let base_moved = matches!((review_base_oid, remote_base), (Some(a), Some(b)) if a != b);
    PrReviewDrift {
        head_behind,
        head_drifted,
        base_moved,
    }
}

/// Run a git command in `cwd`, returning trimmed stdout on success or `None` on
/// any failure (spawn error, non-zero exit).
fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").current_dir(cwd).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_when_head_and_base_match() {
        let drift = compute_drift(0, Some("bbb"), Some("bbb"));
        assert!(drift.is_clean());
        assert_eq!(drift, PrReviewDrift::default());
    }

    #[test]
    fn head_drift_carries_behind_count() {
        let drift = compute_drift(3, Some("bbb"), Some("bbb"));
        assert!(drift.head_drifted);
        assert_eq!(drift.head_behind, 3);
        assert!(!drift.base_moved);
        assert!(!drift.is_clean());
    }

    #[test]
    fn local_commits_ahead_do_not_drift() {
        // The reviewer's own unpushed commits leave the worktree ahead of, not
        // behind, origin/<head> (head_behind == 0), so no head-drift badge.
        let drift = compute_drift(0, Some("bbb"), Some("bbb"));
        assert!(!drift.head_drifted);
        assert!(drift.is_clean());
    }

    #[test]
    fn base_moved_when_origin_base_advances_past_spawn_oid() {
        let drift = compute_drift(0, Some("base_old"), Some("base_new"));
        assert!(drift.base_moved);
        assert!(!drift.head_drifted);
        assert!(!drift.is_clean());
    }

    #[test]
    fn unknown_remote_base_is_not_drift() {
        // A failed fetch/rev-parse (None remote base) must never raise a false badge.
        let drift = compute_drift(0, Some("base_old"), None);
        assert!(drift.is_clean());
    }

    #[test]
    fn missing_spawn_base_oid_means_base_not_judged() {
        let drift = compute_drift(0, None, Some("base_new"));
        assert!(!drift.base_moved);
    }
}
