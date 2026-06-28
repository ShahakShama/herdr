//! Locate the plan file a Claude agent is currently working on.
//!
//! Claude Code writes a plan-mode plan to `~/.claude/plans/<slug>.md` and records
//! that path in its session transcript as a structured `"planFilePath"` field
//! (the same path Claude's own ctrl+g opens). We find the agent's transcript by
//! its session id — the transcript file is named `<session-id>.jsonl` — and
//! return the last `planFilePath` it recorded. Keying off the session id makes
//! the lookup deterministic and correct per-agent, with no last-modified
//! guessing across concurrently-running agents.

use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

/// The plan file the Claude agent with `session_id` is currently working on, or
/// `None` when it can't be resolved: unknown/empty session, no transcript on
/// disk, no plan recorded yet, or the recorded file no longer exists.
pub(crate) fn current_plan_file(session_id: &str) -> Option<PathBuf> {
    let transcript = session_transcript(session_id)?;
    let contents = std::fs::read_to_string(&transcript).ok()?;
    let path = PathBuf::from(last_plan_file_path(&contents)?);
    path.is_file().then_some(path)
}

/// Find `~/.claude/projects/*/<session_id>.jsonl`. Globbing by the session-id
/// filename avoids re-deriving Claude's cwd→projects-dir encoding; there is
/// exactly one transcript per session.
fn session_transcript(session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() || session_id.contains(['/', '\\', '.']) {
        return None;
    }
    let projects = crate::integration::claude_dir().ok()?.join("projects");
    let file_name = format!("{session_id}.jsonl");
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let candidate = entry.path().join(&file_name);
            candidate.is_file().then_some(candidate)
        })
}

/// The last `"planFilePath":"…"` value in a session transcript. Parsing this
/// specific field — rather than any `.claude/plans/*.md` mention — ignores
/// incidental path mentions in pasted text, and the last one wins because a
/// session may have produced several plans.
fn last_plan_file_path(contents: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#""planFilePath":"([^"]*)""#).expect("valid regex"));
    re.captures_iter(contents)
        .last()
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::last_plan_file_path;

    #[test]
    fn takes_the_last_plan_file_path() {
        let transcript = concat!(
            r#"{"sessionId":"abc","planFilePath":"/home/u/.claude/plans/first-one.md"}"#,
            "\n",
            r#"{"type":"user","content":"see /home/u/.claude/plans/pasted-mention.md please"}"#,
            "\n",
            r#"{"sessionId":"abc","planFilePath":"/home/u/.claude/plans/current-one.md"}"#,
            "\n",
        );
        assert_eq!(
            last_plan_file_path(transcript).as_deref(),
            Some("/home/u/.claude/plans/current-one.md"),
        );
    }

    #[test]
    fn none_when_no_plan_field_present() {
        let transcript = r#"{"type":"user","content":"talked about /home/u/.claude/plans/x.md"}"#;
        assert_eq!(last_plan_file_path(transcript), None);
    }
}
