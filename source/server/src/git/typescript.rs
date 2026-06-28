use std::collections::HashSet;

use semver::Version;
use tracing::{debug, instrument};

use super::core::version_search_start_points;

/// Walk newest→oldest. First commit whose package.json has `package_name` at
/// `target_version` is the latest commit for that version.
///
/// `start` pins the walk to a specific commit (e.g. the remote tracking tip
/// after a fetch). Falls back to local HEAD when `None`.
#[instrument(skip(repo), fields(package = %package_name, version = %target_version))]
pub fn find_typescript_commit_for_version(
	repo: &gix::Repository,
	target_version: &Version,
	package_name: &str,
	start: Option<gix::ObjectId>,
) -> Option<gix::ObjectId> {
	let mut visited = HashSet::new();

	for start_id in version_search_start_points(repo, start).ok()? {
		let revwalk = repo.rev_walk([start_id]);

		for commit_id in revwalk.all().ok()? {
			let commit_id = commit_id.ok()?;
			let detached = commit_id.id().detach();
			if !visited.insert(detached) {
				continue;
			}

			let commit = repo.find_commit(detached).ok()?;
			let tree = commit.tree().ok()?;

			match extract_typescript_package_version(repo, &tree, package_name) {
				Some(version) if &version == target_version => {
					debug!(commit = %detached, "found matching TypeScript package version");
					return Some(detached);
				}
				_ => continue,
			}
		}
	}

	None
}

fn extract_typescript_package_version(
	repo: &gix::Repository,
	tree: &gix::Tree,
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
