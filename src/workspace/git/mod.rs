mod config;
#[cfg(test)]
mod config_tests;
mod discovery;
mod prs;
mod repos;
mod status;
#[cfg(test)]
mod test_support;

pub use self::{
    discovery::{derive_label_from_cwd, git_branch, git_space_metadata, GitSpaceMetadata},
    prs::{list_prs_for_my_review, pr_by_number, ReviewPr},
    repos::{
        default_scan_root, list_review_branches, review_base, scan_repositories, Branch,
        Repository,
    },
    status::{git_status_cache_key, git_status_snapshot_for_cwd, GitStatusCacheEntry},
};

#[cfg(test)]
pub(super) use self::status::git_ahead_behind;
