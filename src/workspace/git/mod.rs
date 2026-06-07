mod config;
#[cfg(test)]
mod config_tests;
mod discovery;
mod repos;
mod status;
#[cfg(test)]
mod test_support;

pub use self::{
    discovery::{derive_label_from_cwd, git_branch, git_space_metadata, GitSpaceMetadata},
    // `default_branch`, `graphite_parent`, `list_branches`, `review_base`, and
    // `Branch` are defined for Phase 4 (review) and re-exported when consumed.
    repos::{default_scan_root, scan_repositories, Repository},
    status::{git_status_cache_key, git_status_snapshot_for_cwd, GitStatusCacheEntry},
};

#[cfg(test)]
pub(super) use self::status::git_ahead_behind;
