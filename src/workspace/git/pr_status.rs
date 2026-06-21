//! PR-status data layer backing the keyboard-first PR pane.
//!
//! For every repository discovered under `~/workspace` this fetches — in ONE
//! `gh api graphql` call per repo — the open PRs the user authored
//! (`author:@me`) and the open PRs the user is reviewing
//! (`review-requested:@me` OR `reviewed-by:@me`), together with each PR's review
//! threads (last replier per thread), review submissions (approvals), and the
//! current user's login. Each PR is then classified into one of three buckets
//! (RED waiting-for-me / GREEN lgtm'd / GREY waiting-on-the-other-side) and the
//! PRs are grouped per author for display, with their Graphite stack structure
//! retained so the pane can render parents/children.
//!
//! GitHub code search ANDs its qualifiers (there is no in-query OR), so the
//! "reviewing" set is fetched as two aliased searches (`requested` +
//! `reviewedBy`) and unioned in Rust — collapsing them into one search would
//! silently under-return.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;

use super::discovery::git_trimmed_stdout;
use super::{Repository, ReviewPr};

/// Which review state a PR is in, from the current user's point of view.
/// Priority when a PR could fit more than one: RED > GREEN > GREY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrBucket {
    /// Waiting for me: an unresolved thread where I wasn't the last to reply,
    /// or (for others' PRs) an unreviewed PR with no threads at all.
    Red,
    /// Lgtm'd: approved (others' PRs: by me; my PRs: by any reviewer).
    Green,
    /// Waiting on the other side: every unresolved thread has me as the last
    /// replier, or (for my PRs) it is unreviewed.
    Grey,
}

/// The last comment's author on one GitHub review thread, plus whether the
/// thread is resolved (resolved threads need no reply, so they don't make a PR
/// red).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    pub is_resolved: bool,
    /// Login of the author of the thread's most recent comment (`None` only for
    /// the degenerate empty thread).
    pub last_comment_author: Option<String>,
}

/// One submitted review on a PR (`APPROVED` / `CHANGES_REQUESTED` / `COMMENTED`
/// / `DISMISSED` / `PENDING`) and who submitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSubmission {
    pub state: String,
    pub author: String,
}

/// A fetched PR with everything classification and stacking need. `is_mine` is
/// not stored — it is derived from `author == viewer` wherever needed, so the
/// viewer login only has to be known by aggregation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The PR author's login.
    pub author: String,
    pub is_draft: bool,
    /// Base branch (`baseRefName`) — the parent edge in a Graphite stack.
    pub base_ref: String,
    /// Head branch (`headRefName`).
    pub head_ref: String,
    /// Owning repository's stable key ([`Repository::key`]); scopes stack edges.
    pub repo_key: String,
    pub threads: Vec<ReviewThread>,
    pub reviews: Vec<ReviewSubmission>,
    /// Whether the PR's latest commit has a failing CI status-check rollup.
    pub ci_failing: bool,
}

impl FetchedPr {
    /// Adapt to the [`ReviewPr`] the reviewer-mode flow consumes.
    pub fn to_review_pr(&self) -> ReviewPr {
        ReviewPr {
            number: self.number,
            title: self.title.clone(),
            author: self.author.clone(),
            head_branch: self.head_ref.clone(),
            base_branch: self.base_ref.clone(),
            url: self.url.clone(),
            graph_prefix: String::new(),
        }
    }
}

/// Classify one PR for the current user (`me`). Priority RED > GREEN > GREY.
///
/// Resolved threads need no reply, so only UNRESOLVED threads drive the
/// last-replier logic; a PR with no review threads at all is "unreviewed".
pub fn classify_pr(pr: &FetchedPr, me: &str) -> PrBucket {
    let is_mine = pr.author == me;
    let waiting_on_me = pr
        .threads
        .iter()
        .filter(|t| !t.is_resolved)
        .any(|t| t.last_comment_author.as_deref() != Some(me));
    let unreviewed = pr.threads.is_empty();

    // RED: a thread awaits my reply, or someone else's PR is untouched.
    if waiting_on_me {
        return PrBucket::Red;
    }
    if !is_mine && unreviewed {
        return PrBucket::Red;
    }
    // GREEN: approved (mine: by anyone; others': by me).
    if pr_is_approved(pr, me, is_mine) {
        return PrBucket::Green;
    }
    // GREY: all threads me-last, or my own unreviewed PR.
    PrBucket::Grey
}

/// Whether the PR counts as approved. For my PRs, any reviewer's latest
/// meaningful review being `APPROVED` qualifies; for others' PRs, only my own.
fn pr_is_approved(pr: &FetchedPr, me: &str, is_mine: bool) -> bool {
    let latest = latest_state_by_author(pr);
    if is_mine {
        latest.values().any(|state| state == "APPROVED")
    } else {
        latest.get(me).map(|state| state == "APPROVED").unwrap_or(false)
    }
}

/// Each author's most recent *meaningful* review state. `reviews(last:50)`
/// arrives oldest-first, so a later insert wins; `COMMENTED`/`PENDING` are
/// ignored so a stray comment doesn't override a real APPROVED/CHANGES_REQUESTED.
fn latest_state_by_author(pr: &FetchedPr) -> HashMap<String, String> {
    let mut latest = HashMap::new();
    for review in &pr.reviews {
        if review.state == "COMMENTED" || review.state == "PENDING" {
            continue;
        }
        latest.insert(review.author.clone(), review.state.clone());
    }
    latest
}

// ---------------------------------------------------------------------------
// Per-person aggregation + snapshot
// ---------------------------------------------------------------------------

/// One PR under a person, with its computed bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonPr {
    pub pr: FetchedPr,
    pub bucket: PrBucket,
}

/// All of one person's PRs (mine, or one reviewing-author's), with bucket tallies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonPrs {
    pub login: String,
    pub is_me: bool,
    pub prs: Vec<PersonPr>,
    pub red: usize,
    pub green: usize,
    pub grey: usize,
    /// PRs with failing CI — independent of red/green/grey (a PR can be both).
    pub ci: usize,
}

/// The immutable snapshot the PR pane renders. Built off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatusSnapshot {
    pub viewer_login: String,
    /// Me first, then reviewing authors (most-red first).
    pub people: Vec<PersonPrs>,
    /// Full parent/child graph across all fetched PRs, independent of buckets.
    pub stacks: StackGraph,
    pub generated_at: SystemTime,
    /// Per-repo fetch failures (repo label + message); drives a "stale" badge.
    pub errors: Vec<String>,
}

impl PrStatusSnapshot {
    /// Stack-ordered visible PRs for one person, honoring the green/grey toggles
    /// (red is always shown). Returns `(connector_prefix, PersonPr)` rows where
    /// the prefix is the box-drawing stack art (`├ `, `└─┬ `, `│ └ `, …); a
    /// hidden PR's visible descendants re-parent onto the nearest visible
    /// ancestor (req 10), and roots are grouped by repo.
    pub fn visible_person_rows(
        &self,
        login: &str,
        show_green: bool,
        show_grey: bool,
    ) -> Vec<(String, PersonPr)> {
        let Some(person) = self.people.iter().find(|p| p.login == login) else {
            return Vec::new();
        };
        let by_key: HashMap<PrKey, &PersonPr> = person
            .prs
            .iter()
            .map(|pp| ((pp.pr.repo_key.clone(), pp.pr.number), pp))
            .collect();
        let visible = |key: &PrKey| {
            by_key.get(key).is_some_and(|pp| match pp.bucket {
                PrBucket::Red => true,
                PrBucket::Green => show_green,
                PrBucket::Grey => show_grey,
            })
        };
        self.stacks
            .visible_forest(&visible)
            .into_iter()
            .filter_map(|row| by_key.get(&row.key).map(|pp| (row.prefix, (*pp).clone())))
            .collect()
    }
}

/// Group PRs by author (me first, others most-red first), classifying each.
fn aggregate_people(prs: &[FetchedPr], viewer: &str) -> Vec<PersonPrs> {
    let mut by_login: HashMap<String, PersonPrs> = HashMap::new();
    for pr in prs {
        let bucket = classify_pr(pr, viewer);
        let entry = by_login
            .entry(pr.author.clone())
            .or_insert_with(|| PersonPrs {
                login: pr.author.clone(),
                is_me: pr.author == viewer,
                prs: Vec::new(),
                red: 0,
                green: 0,
                grey: 0,
                ci: 0,
            });
        match bucket {
            PrBucket::Red => entry.red += 1,
            PrBucket::Green => entry.green += 1,
            PrBucket::Grey => entry.grey += 1,
        }
        if pr.ci_failing {
            entry.ci += 1;
        }
        entry.prs.push(PersonPr { pr: pr.clone(), bucket });
    }

    let mut me: Option<PersonPrs> = None;
    let mut others: Vec<PersonPrs> = Vec::new();
    for person in by_login.into_values() {
        if person.is_me {
            me = Some(person);
        } else {
            others.push(person);
        }
    }
    others.sort_by(|a, b| b.red.cmp(&a.red).then_with(|| a.login.cmp(&b.login)));

    let mut people = Vec::with_capacity(others.len() + 1);
    if let Some(me) = me {
        people.push(me);
    }
    people.extend(others);
    people
}

// ---------------------------------------------------------------------------
// Graphite stack graph
// ---------------------------------------------------------------------------

/// Identity of a PR within a snapshot: `(repo_key, number)`.
pub type PrKey = (String, u64);

#[derive(Debug, Clone, PartialEq, Eq)]
struct StackNode {
    parent: Option<PrKey>,
    children: Vec<PrKey>,
    /// First-appearance index, for deterministic root/child ordering.
    order: usize,
}

/// A row in a rendered stack tree: the PR key plus the box-drawing connector
/// prefix (`├ `, `└─┬ `, `│ └ `, …) that draws the stack shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackRow {
    pub key: PrKey,
    pub prefix: String,
}

/// The parent/child graph across every fetched PR, derived from
/// `base_ref → head_ref` chaining (scoped per repo). Built once and never
/// mutated by the pane's bucket toggles, so descendant links survive when an
/// intermediate PR is filtered out of view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StackGraph {
    nodes: HashMap<PrKey, StackNode>,
}

impl StackGraph {
    /// Build the graph from all fetched PRs. An edge forms when one PR's
    /// `base_ref` equals another PR's `head_ref` *in the same repo*.
    pub fn build(prs: &[FetchedPr]) -> Self {
        let mut by_head: HashMap<(&str, &str), PrKey> = HashMap::new();
        let mut nodes: HashMap<PrKey, StackNode> = HashMap::new();
        for (order, pr) in prs.iter().enumerate() {
            let key = (pr.repo_key.clone(), pr.number);
            by_head.insert((pr.repo_key.as_str(), pr.head_ref.as_str()), key.clone());
            nodes.insert(
                key,
                StackNode {
                    parent: None,
                    children: Vec::new(),
                    order,
                },
            );
        }
        for pr in prs {
            let key = (pr.repo_key.clone(), pr.number);
            if let Some(parent) = by_head.get(&(pr.repo_key.as_str(), pr.base_ref.as_str())) {
                if *parent != key {
                    let parent = parent.clone();
                    nodes.get_mut(&key).expect("node exists").parent = Some(parent.clone());
                    nodes.get_mut(&parent).expect("parent exists").children.push(key);
                }
            }
        }
        // Order each node's children by first appearance.
        let order_of: HashMap<PrKey, usize> =
            nodes.iter().map(|(k, n)| (k.clone(), n.order)).collect();
        for node in nodes.values_mut() {
            node.children.sort_by_key(|child| order_of.get(child).copied().unwrap_or(usize::MAX));
        }
        StackGraph { nodes }
    }

    /// Render the visible subset as a forest of box-drawing stack trees. A
    /// hidden node (failing `visible`) emits nothing but its visible descendants
    /// re-parent onto the nearest visible ancestor (req 10). Roots are grouped by
    /// repo (so each repo's stacks are contiguous), then by first-appearance.
    /// Each emitted row carries its connector prefix (`├ `, `└─┬ `, `│ └ `, …).
    pub fn visible_forest(&self, visible: &dyn Fn(&PrKey) -> bool) -> Vec<StackRow> {
        // Nearest visible ancestor of `key` (climbing through hidden parents).
        let visible_parent = |key: &PrKey| -> Option<PrKey> {
            let mut cur = self.nodes.get(key).and_then(|node| node.parent.clone());
            while let Some(parent) = cur {
                if visible(&parent) {
                    return Some(parent);
                }
                cur = self.nodes.get(&parent).and_then(|node| node.parent.clone());
            }
            None
        };
        // Build the visible-only parent/child structure.
        let mut vis_children: HashMap<PrKey, Vec<PrKey>> = HashMap::new();
        let mut roots: Vec<PrKey> = Vec::new();
        for key in self.nodes.keys() {
            if !visible(key) {
                continue;
            }
            match visible_parent(key) {
                Some(parent) => vis_children.entry(parent).or_default().push(key.clone()),
                None => roots.push(key.clone()),
            }
        }
        let order = |key: &PrKey| self.nodes.get(key).map(|node| node.order).unwrap_or(usize::MAX);
        // Group roots by repo, then by appearance, so repos render contiguously.
        roots.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| order(a).cmp(&order(b))));
        for children in vis_children.values_mut() {
            children.sort_by_key(|key| order(key));
        }

        let mut out = Vec::new();
        let mut visited = HashSet::new();
        for root in &roots {
            self.walk_tree(root, &[], true, true, &vis_children, &mut out, &mut visited);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_tree(
        &self,
        key: &PrKey,
        ancestors: &[bool],
        is_last: bool,
        is_root: bool,
        vis_children: &HashMap<PrKey, Vec<PrKey>>,
        out: &mut Vec<StackRow>,
        visited: &mut HashSet<PrKey>,
    ) {
        if !visited.insert(key.clone()) {
            return; // cycle guard
        }
        let children = vis_children.get(key).cloned().unwrap_or_default();
        let mut prefix = String::new();
        if !is_root {
            for &more in ancestors {
                prefix.push_str(if more { "│ " } else { "  " });
            }
            prefix.push(if is_last { '└' } else { '├' });
            if !children.is_empty() {
                prefix.push_str("─┬");
            }
            prefix.push(' ');
        }
        out.push(StackRow { key: key.clone(), prefix });
        for (i, child) in children.iter().enumerate() {
            let child_is_last = i + 1 == children.len();
            // The root sits at column 0 with no spine; deeper levels inherit a
            // `│`/space column per ancestor depending on whether it has more
            // siblings below.
            let child_ancestors: Vec<bool> = if is_root {
                ancestors.to_vec()
            } else {
                let mut a = ancestors.to_vec();
                a.push(!is_last);
                a
            };
            self.walk_tree(child, &child_ancestors, child_is_last, false, vis_children, out, visited);
        }
    }
}

// ---------------------------------------------------------------------------
// Fetching (runs on a worker thread, never the UI thread)
// ---------------------------------------------------------------------------

/// Fetch a fresh snapshot across all `repos`. Per-repo failures are collected
/// into `snapshot.errors` rather than aborting the whole refresh; repos with no
/// GitHub `origin` are skipped silently. Blocking — call off the UI thread.
pub fn fetch_pr_status_snapshot(repos: &[Repository]) -> PrStatusSnapshot {
    // Map each scanned repo's GitHub "owner/name" (lowercased) -> repo key. This
    // is local `git config` only (no API). It lets us run a few GLOBAL searches
    // and filter to ~/workspace client-side: per-repo searches cost 3 requests
    // PER REPO every refresh and blow GitHub's search rate limit, whereas global
    // searches are 3 requests total regardless of repo count.
    let mut repo_by_owner_name: HashMap<String, String> = HashMap::new();
    for repo in repos {
        if let Some(owner_name) = github_owner_name(&repo.root) {
            repo_by_owner_name.insert(owner_name.to_lowercase(), repo.key.clone());
        }
    }
    let generated_at = SystemTime::now();
    if repo_by_owner_name.is_empty() {
        return PrStatusSnapshot {
            viewer_login: viewer_login().unwrap_or_default(),
            people: Vec::new(),
            stacks: StackGraph::default(),
            generated_at,
            errors: Vec::new(),
        };
    }
    let (viewer, prs, errors) = match fetch_global_prs(&repo_by_owner_name) {
        Ok((viewer, prs)) => (viewer, prs, Vec::new()),
        Err(err) => (String::new(), Vec::new(), vec![err]),
    };
    let viewer = if viewer.is_empty() {
        viewer_login().unwrap_or_default()
    } else {
        viewer
    };
    PrStatusSnapshot {
        people: aggregate_people(&prs, &viewer),
        stacks: StackGraph::build(&prs),
        viewer_login: viewer,
        generated_at,
        errors,
    }
}

/// `owner/name` for a repo's GitHub `origin`, or `None` for a non-GitHub remote.
pub fn github_owner_name(repo_root: &Path) -> Option<String> {
    let url = git_trimmed_stdout(repo_root, &["config", "--get", "remote.origin.url"])?;
    parse_github_owner_name(&url)
}

/// Parse `owner/name` out of a GitHub remote URL (ssh or https, with/without
/// `.git`). Returns `None` for non-GitHub hosts.
fn parse_github_owner_name(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest).trim_matches('/');
    let (owner, name) = rest.split_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// Generous cap on a single `gh` fetch so a slow/hung call can't block the
/// background refresh forever — `gh` exposes no timeout of its own, and a plain
/// `.output()` waits indefinitely. Normal graphql calls finish in a few
/// seconds; this only fires on a genuinely stuck call, after which the refresh
/// self-heals on the next 30s cycle.
const PR_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Run a `gh` command with a timeout, draining stdout on a side thread so a
/// large (>64KB) response can't deadlock the child on a full pipe while we poll.
/// Returns stdout on success; on timeout the child is killed and an error
/// returned.
fn run_gh_capped(
    repo_root: Option<&Path>,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut command = Command::new("gh");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = repo_root {
        command.current_dir(root);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("gh not available: {err}"))?;
    let mut stdout_pipe = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(format!("gh timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                let _ = reader.join();
                return Err(format!("gh wait failed: {err}"));
            }
        }
    };
    let stdout = reader.join().unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(if stderr.trim().is_empty() {
        format!("gh exited unsuccessfully ({status})")
    } else {
        stderr.trim().to_string()
    })
}

/// One global `gh api graphql` call: three searches across all of GitHub (not
/// per repo), filtered to the scanned repos via `repo_by_owner_name`. Returns
/// `(viewer_login, prs)`.
fn fetch_global_prs(
    repo_by_owner_name: &HashMap<String, String>,
) -> Result<(String, Vec<FetchedPr>), String> {
    let query = format!("query={GRAPHQL_QUERY}");
    let mine = "mineQuery=is:pr is:open author:@me sort:updated-desc".to_string();
    let requested =
        "requestedQuery=is:pr is:open review-requested:@me sort:updated-desc".to_string();
    let reviewed = "reviewedQuery=is:pr is:open reviewed-by:@me sort:updated-desc".to_string();
    let stdout = run_gh_capped(
        None,
        &["api", "graphql", "-f", &query, "-f", &mine, "-f", &requested, "-f", &reviewed],
        PR_FETCH_TIMEOUT,
    )?;
    parse_global_response(&stdout, repo_by_owner_name)
}

/// Look up (and process-cache) the current GitHub user's login. Stable for the
/// process, so a set-once [`OnceLock`] is the right primitive across worker
/// threads.
pub fn viewer_login() -> Option<String> {
    static VIEWER_LOGIN: OnceLock<String> = OnceLock::new();
    if let Some(login) = VIEWER_LOGIN.get() {
        return Some(login.clone());
    }
    let login = run_gh_viewer_login()?;
    Some(VIEWER_LOGIN.get_or_init(|| login).clone())
}

fn run_gh_viewer_login() -> Option<String> {
    let stdout = run_gh_capped(
        None,
        &["api", "graphql", "-f", "query=query{viewer{login}}", "-q", ".data.viewer.login"],
        PR_FETCH_TIMEOUT,
    )
    .ok()?;
    let login = String::from_utf8_lossy(&stdout).trim().to_string();
    (!login.is_empty()).then_some(login)
}

const GRAPHQL_QUERY: &str = "\
query($mineQuery: String!, $requestedQuery: String!, $reviewedQuery: String!) {
  viewer { login }
  mine: search(type: ISSUE, first: 50, query: $mineQuery) { ...prFields }
  requested: search(type: ISSUE, first: 50, query: $requestedQuery) { ...prFields }
  reviewedBy: search(type: ISSUE, first: 50, query: $reviewedQuery) { ...prFields }
}
fragment prFields on SearchResultItemConnection {
  nodes {
    ... on PullRequest {
      number
      title
      url
      isDraft
      baseRefName
      headRefName
      author { login }
      repository { nameWithOwner }
      reviews(last: 50) { nodes { state author { login } } }
      reviewThreads(first: 50) {
        nodes { isResolved comments(last: 1) { nodes { author { login } } } }
      }
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
    }
  }
}";

fn parse_global_response(
    stdout: &[u8],
    repo_by_owner_name: &HashMap<String, String>,
) -> Result<(String, Vec<FetchedPr>), String> {
    let envelope: GraphqlEnvelope =
        serde_json::from_slice(stdout).map_err(|err| format!("unexpected gh output: {err}"))?;
    // GitHub GraphQL often returns partial `errors` ALONGSIDE valid `data`
    // (e.g. one PR's field timed out). Use the data whenever it's present; only
    // treat the call as failed when there's no data at all — discarding good
    // data on a partial error is what made refreshes spuriously "fail".
    let data = match envelope.data {
        Some(data) => data,
        None => {
            let message = envelope
                .errors
                .map(|errors| {
                    errors
                        .into_iter()
                        .map(|e| e.message)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| "no data in gh response".to_string());
            return Err(message);
        }
    };
    let viewer = data.viewer.login;

    let mut prs = Vec::new();
    let mut seen: HashSet<PrKey> = HashSet::new();
    for node in data
        .mine
        .nodes
        .into_iter()
        .chain(data.requested.nodes)
        .chain(data.reviewed_by.nodes)
    {
        if node.number == 0 {
            continue; // empty (non-PR) node
        }
        // Keep only PRs in a scanned ~/workspace repo, mapping to its repo key.
        let Some(repo_key) = repo_by_owner_name
            .get(&node.repository.name_with_owner.to_lowercase())
            .cloned()
        else {
            continue;
        };
        if !seen.insert((repo_key.clone(), node.number)) {
            continue; // already taken from another search
        }
        prs.push(node.into_fetched(&repo_key));
    }
    Ok((viewer, prs))
}

// --- Raw GraphQL response shapes -------------------------------------------

#[derive(Deserialize)]
struct GraphqlEnvelope {
    data: Option<GraphqlData>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Deserialize)]
struct GraphqlData {
    viewer: RawViewer,
    #[serde(default)]
    mine: RawSearch,
    #[serde(default)]
    requested: RawSearch,
    #[serde(rename = "reviewedBy", default)]
    reviewed_by: RawSearch,
}

#[derive(Deserialize)]
struct RawViewer {
    login: String,
}

#[derive(Deserialize, Default)]
struct RawSearch {
    #[serde(default)]
    nodes: Vec<RawNode>,
}

/// One search node. `#[serde(default)]` tolerates the empty `{}` a non-PR result
/// would yield (filtered out later by `number == 0`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawNode {
    number: u64,
    title: String,
    url: String,
    is_draft: bool,
    base_ref_name: String,
    head_ref_name: String,
    author: Option<RawLogin>,
    repository: RawRepo,
    reviews: RawReviews,
    review_threads: RawThreads,
    commits: RawCommits,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawRepo {
    name_with_owner: String,
}

impl RawNode {
    fn into_fetched(self, repo_key: &str) -> FetchedPr {
        let threads = self
            .review_threads
            .nodes
            .into_iter()
            .map(|thread| ReviewThread {
                is_resolved: thread.is_resolved,
                last_comment_author: thread
                    .comments
                    .nodes
                    .into_iter()
                    .last()
                    .and_then(|comment| comment.author)
                    .map(|author| author.login),
            })
            .collect();
        let reviews = self
            .reviews
            .nodes
            .into_iter()
            .map(|review| ReviewSubmission {
                state: review.state,
                author: review.author.map(|a| a.login).unwrap_or_default(),
            })
            .collect();
        let ci_failing = self
            .commits
            .nodes
            .into_iter()
            .last()
            .and_then(|node| node.commit.status_check_rollup)
            .map(|rollup| rollup.state == "FAILURE" || rollup.state == "ERROR")
            .unwrap_or(false);
        FetchedPr {
            number: self.number,
            title: self.title,
            url: self.url,
            author: self.author.map(|a| a.login).unwrap_or_else(|| "ghost".to_string()),
            is_draft: self.is_draft,
            base_ref: self.base_ref_name,
            head_ref: self.head_ref_name,
            repo_key: repo_key.to_string(),
            threads,
            reviews,
            ci_failing,
        }
    }
}

#[derive(Deserialize, Default)]
struct RawCommits {
    #[serde(default)]
    nodes: Vec<RawCommitNode>,
}

#[derive(Deserialize, Default)]
struct RawCommitNode {
    #[serde(default)]
    commit: RawCommit,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawCommit {
    status_check_rollup: Option<RawRollup>,
}

#[derive(Deserialize, Default)]
struct RawRollup {
    #[serde(default)]
    state: String,
}

#[derive(Deserialize, Default)]
struct RawLogin {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize, Default)]
struct RawReviews {
    #[serde(default)]
    nodes: Vec<RawReview>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawReview {
    state: String,
    author: Option<RawLogin>,
}

#[derive(Deserialize, Default)]
struct RawThreads {
    #[serde(default)]
    nodes: Vec<RawThread>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawThread {
    is_resolved: bool,
    comments: RawComments,
}

#[derive(Deserialize, Default)]
struct RawComments {
    #[serde(default)]
    nodes: Vec<RawComment>,
}

#[derive(Deserialize, Default)]
struct RawComment {
    #[serde(default)]
    author: Option<RawLogin>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, author: &str, base: &str, head: &str) -> FetchedPr {
        FetchedPr {
            number,
            title: format!("pr {number}"),
            url: format!("https://github.com/acme/proj/pull/{number}"),
            author: author.to_string(),
            is_draft: false,
            base_ref: base.to_string(),
            head_ref: head.to_string(),
            repo_key: "acme/proj".to_string(),
            threads: Vec::new(),
            reviews: Vec::new(),
            ci_failing: false,
        }
    }

    fn thread(resolved: bool, last: Option<&str>) -> ReviewThread {
        ReviewThread {
            is_resolved: resolved,
            last_comment_author: last.map(str::to_string),
        }
    }

    fn review(state: &str, author: &str) -> ReviewSubmission {
        ReviewSubmission {
            state: state.to_string(),
            author: author.to_string(),
        }
    }

    #[test]
    fn red_when_a_thread_awaits_my_reply() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(false, Some("alice"))]; // they replied last
        assert_eq!(classify_pr(&p, "me"), PrBucket::Red);
    }

    #[test]
    fn others_unreviewed_pr_is_red() {
        let p = pr(1, "alice", "main", "alice/x"); // no threads
        assert_eq!(classify_pr(&p, "me"), PrBucket::Red);
    }

    #[test]
    fn my_unreviewed_pr_is_grey() {
        let p = pr(1, "me", "main", "me/x"); // no threads, not approved
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey);
    }

    #[test]
    fn grey_when_all_threads_me_last() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(false, Some("me")), thread(false, Some("me"))];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey);
    }

    #[test]
    fn resolved_thread_does_not_make_it_red() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(true, Some("alice"))]; // resolved -> ignored
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey);
    }

    #[test]
    fn green_others_pr_only_when_i_approved() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(false, Some("me"))]; // not red
        p.reviews = vec![review("APPROVED", "bob")];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey, "bob's approval isn't mine");
        p.reviews = vec![review("APPROVED", "me")];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Green);
    }

    #[test]
    fn green_my_pr_when_anyone_approved() {
        let mut p = pr(1, "me", "main", "me/x");
        p.threads = vec![thread(false, Some("me"))];
        p.reviews = vec![review("APPROVED", "alice")];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Green);
    }

    #[test]
    fn red_beats_green() {
        let mut p = pr(1, "alice", "main", "alice/x");
        p.threads = vec![thread(false, Some("alice"))]; // awaits me -> red
        p.reviews = vec![review("APPROVED", "me")]; // also approved
        assert_eq!(classify_pr(&p, "me"), PrBucket::Red);
    }

    #[test]
    fn later_changes_requested_unapproves() {
        let mut p = pr(1, "me", "main", "me/x");
        p.threads = vec![thread(false, Some("me"))];
        p.reviews = vec![review("APPROVED", "alice"), review("CHANGES_REQUESTED", "alice")];
        assert_eq!(classify_pr(&p, "me"), PrBucket::Grey);
    }

    #[test]
    fn stack_visible_rows_keep_descendant_under_hidden_parent() {
        // A (red) -> B (green) -> C (red); hide B, C must stay under A.
        let prs = vec![
            pr(1, "alice", "main", "a"),
            pr(2, "alice", "a", "b"),
            pr(3, "alice", "b", "c"),
        ];
        let graph = StackGraph::build(&prs);
        let visible_keys: HashSet<PrKey> =
            [("acme/proj".to_string(), 1), ("acme/proj".to_string(), 3)]
                .into_iter()
                .collect();
        let rows = graph.visible_forest(&|key| visible_keys.contains(key));
        let view: Vec<(u64, &str)> = rows.iter().map(|r| (r.key.1, r.prefix.as_str())).collect();
        // #2 hidden, so #3 attaches directly under #1 (the root).
        assert_eq!(view, vec![(1, ""), (3, "└ ")]);
    }

    #[test]
    fn stack_visible_forest_draws_box_connectors() {
        let prs = vec![
            pr(1, "alice", "main", "a"),
            pr(2, "alice", "a", "b"),
            pr(3, "alice", "b", "c"),
        ];
        let graph = StackGraph::build(&prs);
        let rows = graph.visible_forest(&|_| true);
        let view: Vec<(u64, &str)> = rows.iter().map(|r| (r.key.1, r.prefix.as_str())).collect();
        // Linear A->B->C: root flush, B is A's only child (└─┬), C under B.
        assert_eq!(view, vec![(1, ""), (2, "└─┬ "), (3, "  └ ")]);
    }

    #[test]
    fn aggregate_puts_me_first_then_most_red() {
        let mut mine = pr(1, "me", "main", "me/x");
        mine.threads = vec![thread(false, Some("me"))]; // grey
        let mut alice = pr(2, "alice", "main", "alice/x"); // unreviewed -> red
        let _ = &mut alice;
        let bob1 = pr(3, "bob", "main", "bob/x"); // red
        let bob2 = pr(4, "bob", "main", "bob/y"); // red
        let people = aggregate_people(&[mine, alice, bob1, bob2], "me");
        assert_eq!(people[0].login, "me");
        assert!(people[0].is_me);
        assert_eq!(people[1].login, "bob", "bob has more red than alice");
        assert_eq!(people[1].red, 2);
        assert_eq!(people[2].login, "alice");
    }

    #[test]
    fn parse_github_owner_name_handles_url_shapes() {
        assert_eq!(
            parse_github_owner_name("git@github.com:acme/proj.git").as_deref(),
            Some("acme/proj")
        );
        assert_eq!(
            parse_github_owner_name("https://github.com/acme/proj").as_deref(),
            Some("acme/proj")
        );
        assert_eq!(parse_github_owner_name("git@gitlab.com:acme/proj.git"), None);
    }

    #[test]
    fn parse_global_response_filters_to_local_repos_unions_and_dedups() {
        // PR #1 (acme/proj) is local; #2 appears in two searches; #9 is in a
        // repo NOT in ~/workspace and must be dropped.
        let raw = br#"{"data":{
            "viewer":{"login":"me"},
            "mine":{"nodes":[{"number":1,"title":"mine","url":"u","isDraft":false,
                "baseRefName":"main","headRefName":"me/x","author":{"login":"me"},
                "repository":{"nameWithOwner":"acme/proj"},
                "reviews":{"nodes":[]},"reviewThreads":{"nodes":[]}}]},
            "requested":{"nodes":[
                {"number":2,"title":"rev","url":"u","isDraft":false,
                 "baseRefName":"main","headRefName":"a/x","author":{"login":"alice"},
                 "repository":{"nameWithOwner":"Acme/Proj"},
                 "reviews":{"nodes":[]},
                 "reviewThreads":{"nodes":[{"isResolved":false,"comments":{"nodes":[{"author":{"login":"alice"}}]}}]}},
                {"number":9,"title":"foreign","url":"u","isDraft":false,
                 "baseRefName":"main","headRefName":"x","author":{"login":"bob"},
                 "repository":{"nameWithOwner":"other/repo"},
                 "reviews":{"nodes":[]},"reviewThreads":{"nodes":[]}}]},
            "reviewedBy":{"nodes":[{"number":2,"title":"rev","url":"u","isDraft":false,
                "baseRefName":"main","headRefName":"a/x","author":{"login":"alice"},
                "repository":{"nameWithOwner":"acme/proj"},
                "reviews":{"nodes":[]},"reviewThreads":{"nodes":[]}}]}
        }}"#;
        let map: HashMap<String, String> =
            [("acme/proj".to_string(), "acme/proj".to_string())].into_iter().collect();
        let (viewer, prs) = parse_global_response(raw, &map).unwrap();
        assert_eq!(viewer, "me");
        // #1 and #2 kept (case-insensitive owner match), #2 deduped, #9 dropped.
        assert_eq!(prs.len(), 2, "local PRs only, deduped; foreign repo dropped");
        let pr2 = prs.iter().find(|p| p.number == 2).unwrap();
        assert_eq!(pr2.repo_key, "acme/proj");
        assert_eq!(pr2.threads.len(), 1);
        assert_eq!(pr2.threads[0].last_comment_author.as_deref(), Some("alice"));
    }
}
