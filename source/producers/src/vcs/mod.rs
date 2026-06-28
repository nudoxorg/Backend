mod cargo;
mod core;
mod typescript;

pub mod error;

pub use error::GitError;
pub use cargo::{check_package_version, extract_package_version, find_commit_for_version, resolve_workspace_members};
pub use typescript::find_typescript_commit_for_version;
pub use core::{clone_repository, fetch_remote_updates, materialize_commit, open_or_clone_repository, remote_branch_commit};
