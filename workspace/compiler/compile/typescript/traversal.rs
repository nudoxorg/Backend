//! Version resolution over git for TypeScript packages.
//!
//! Walks newest to oldest across every ref tip and returns the first commit
//! whose root `package.json` declares the requested `name`/`version`. The git
//! plumbing itself lives in [`crate::languages::vcs`].

use semver::Version;
use tracing::instrument;

use crate::languages::vcs::find_commit_with_extractor;
pub use crate::languages::vcs::{materialize_commit, open_or_clone_repository, GitError};

/// Walk newest→oldest. First commit whose `package.json` has `package_name` at
/// `target_version` is the latest commit for that version.
///
/// `start` pins the walk to a specific commit (e.g. the remote tracking tip
/// after a fetch). Falls back to local HEAD when `None`.
#[instrument(skip(repo), fields(package = %package_name, version = %target_version))]
pub fn find_commit_for_version(
	repo: &gix::Repository,
	target_version: &Version,
	package_name: &str,
	start: Option<gix::ObjectId>,
) -> Option<gix::ObjectId> {
	find_commit_with_extractor(
		repo,
		target_version,
		package_name,
		start,
		extract_typescript_package_version,
	)
}

/// Read the `version` the root `package.json` in `tree` declares, when its
/// `name` matches `package_name`.
fn extract_typescript_package_version(
	repo: &gix::Repository,
	tree: &gix::Tree<'_>,
	package_name: &str,
) -> Option<Version> {
	let entry = tree.lookup_entry_by_path("package.json").ok()??;
	if !entry.mode().is_blob() {
		return None;
	}

	let blob = repo.find_blob(entry.oid()).ok()?;
	let content = std::str::from_utf8(&blob.data).ok()?;
	let manifest: serde_json::Value = serde_json::from_str(content).ok()?;
	let pkg_name = manifest.get("name").and_then(serde_json::Value::as_str)?;
	if pkg_name != package_name {
		return None;
	}

	let version = manifest.get("version").and_then(serde_json::Value::as_str)?;
	Version::parse(version).ok()
}
