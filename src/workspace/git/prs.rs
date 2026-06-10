//! GitHub pull-request queries via the `gh` CLI: list the open PRs awaiting
//! the user's review, for the branch picker's "reviewing" list.

use std::path::Path;

/// An open pull request awaiting the user's review.
///
/// Also stored on a [`crate::workspace::Workspace`] (as `reviewing_pr`) once the
/// PR is opened for review, so it serializes with the session snapshot.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReviewPr {
    pub number: u64,
    pub title: String,
    /// The PR author's login.
    pub author: String,
    /// The PR's head branch (what `gh pr checkout` checks out).
    pub head_branch: String,
    /// The PR's base branch (what the review diff is taken against).
    pub base_branch: String,
    pub url: String,
}

/// List the open PRs in `repo_root`'s repository where the user's review is
/// requested, via `gh pr list --search "review-requested:@me"`.
///
/// Errors carry a user-facing message (gh missing, not authenticated, …).
pub fn list_prs_for_my_review(repo_root: &Path) -> Result<Vec<ReviewPr>, String> {
    let output = std::process::Command::new("gh")
        .current_dir(repo_root)
        .args([
            "pr",
            "list",
            "--search",
            "review-requested:@me",
            "--json",
            "number,title,author,headRefName,baseRefName,url",
        ])
        .output()
        .map_err(|err| format!("gh not available: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parse_pr_list(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `gh pr list --json number,title,author,headRefName,baseRefName,url`
/// output into [`ReviewPr`]s.
fn parse_pr_list(raw: &str) -> Result<Vec<ReviewPr>, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawPr {
        number: u64,
        title: String,
        author: RawAuthor,
        head_ref_name: String,
        base_ref_name: String,
        url: String,
    }
    #[derive(serde::Deserialize)]
    struct RawAuthor {
        login: String,
    }

    let prs: Vec<RawPr> =
        serde_json::from_str(raw).map_err(|err| format!("unexpected gh output: {err}"))?;
    Ok(prs
        .into_iter()
        .map(|pr| ReviewPr {
            number: pr.number,
            title: pr.title,
            author: pr.author.login,
            head_branch: pr.head_ref_name,
            base_branch: pr.base_ref_name,
            url: pr.url,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gh_pr_list_json() {
        let raw = r#"[
            {
                "author": {"id": "u1", "is_bot": false, "login": "alice", "name": "Alice"},
                "baseRefName": "master",
                "headRefName": "alice/fix-parser",
                "number": 412,
                "title": "Fix parser panic on empty input",
                "url": "https://github.com/acme/proj/pull/412"
            },
            {
                "author": {"login": "bob"},
                "baseRefName": "main",
                "headRefName": "bob/feature",
                "number": 7,
                "title": "Add feature",
                "url": "https://github.com/acme/proj/pull/7"
            }
        ]"#;
        let prs = parse_pr_list(raw).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(
            prs[0],
            ReviewPr {
                number: 412,
                title: "Fix parser panic on empty input".to_string(),
                author: "alice".to_string(),
                head_branch: "alice/fix-parser".to_string(),
                base_branch: "master".to_string(),
                url: "https://github.com/acme/proj/pull/412".to_string(),
            }
        );
        assert_eq!(prs[1].author, "bob");
    }

    #[test]
    fn empty_list_parses() {
        assert_eq!(parse_pr_list("[]").unwrap(), Vec::new());
    }

    #[test]
    fn malformed_output_is_an_error() {
        assert!(parse_pr_list("not json").is_err());
    }
}
