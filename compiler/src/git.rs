use std::path::PathBuf;

use gix::{Repository, progress::Discard, remote, trace::warn};
use url::Url;

pub fn clone_repository(out_path: &PathBuf, remote: &Url) -> Repository {
	// Clone the repository
	let mut fetch_handle = gix::prepare_clone(remote.to_string(), &out_path)
		.unwrap()
		.with_fetch_options(remote::ref_map::Options::default());

	// Get the repository, ignoring the progress object, and monitoring for
	// interruptions, then stripping the returned Outcome object, taking only
	// repository.
	let (mut checkout_handle, _) =
		fetch_handle.fetch_then_checkout(Discard, &gix::interrupt::IS_INTERRUPTED).unwrap();

	// Checkout the tree into disk, again ignoring details for streamlined process
	checkout_handle.main_worktree(Discard, &gix::interrupt::IS_INTERRUPTED).unwrap().0
}

/// Walk newest→oldest. First commit whose Cargo.toml has `package_name` at
/// `target_version` is the latest commit for that version.
pub fn find_commit_for_version(
	repo: &gix::Repository,
	target_version: &semver::Version,
	package_name: &str,
) -> Option<gix::ObjectId> {
	let head_id = repo.head().ok()?.peel_to_object().ok()?.id();
	let revwalk = repo.rev_walk([head_id]);

	for commit_id in revwalk.all().ok()? {
		let commit_id = commit_id.ok()?;
		let commit = repo.find_commit(commit_id.id()).ok()?;
		let tree = commit.tree().ok()?;

		match extract_package_version(repo, &tree, package_name) {
			Ok(Some(v)) if &v == target_version => {
				return Some(commit_id.id().detach());
			}
			_ => continue,
		}
	}

	None
}

pub fn extract_package_version(
	repo: &gix::Repository,
	tree: &gix::Tree,
	package_name: &str,
) -> Result<Option<semver::Version>, Box<dyn std::error::Error>> {
	let Some(entry) = tree.lookup_entry_by_path("Cargo.toml")?.filter(|e| e.mode().is_blob()) else {
		return Ok(None);
	};

	let blob = repo.find_blob(entry.oid())?;
	let content = std::str::from_utf8(&blob.data)?;
	let manifest: toml::Value = toml::from_str(content)?;

	if let Some(workspace) = manifest.get("workspace") {
		// Workspace root — don't check [package] here, descend into members
		let members: Vec<String> = workspace
			.get("members")
			.and_then(|m| m.as_array())
			.map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
			.unwrap_or_default();

		for member_path in resolve_workspace_members(repo, tree, &members)? {
			let cargo_path = format!("{}/Cargo.toml", member_path);
			let Some(member_entry) =
				tree.lookup_entry_by_path(&cargo_path)?.filter(|e| e.mode().is_blob())
			else {
				continue;
			};

			let blob = repo.find_blob(member_entry.oid())?;
			let content = std::str::from_utf8(&blob.data)?;
			let member_manifest: toml::Value = toml::from_str(content)?;

			if let Some(v) = check_package_version(&member_manifest, package_name)? {
				return Ok(Some(v));
			}
		}

		Ok(None)
	} else {
		// Single-crate repo
		check_package_version(&manifest, package_name)
	}
}

/// Resolve workspace member globs against the live tree.
/// Handles the common `crates/*` pattern; warns on anything more exotic.
pub fn resolve_workspace_members(
	repo: &gix::Repository,
	tree: &gix::Tree,
	members: &[String],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
	let mut resolved = Vec::new();

	for member in members {
		if let Some(prefix) = member.strip_suffix("/*") {
			// e.g. "crates/*" — list direct children of the directory
			let Some(dir_entry) = tree.lookup_entry_by_path(prefix)? else {
				continue;
			};

			if !dir_entry.mode().is_tree() {
				continue;
			}

			let subtree = repo.find_tree(dir_entry.oid())?;
			for child in subtree.iter() {
				let child = child?;
				if child.mode().is_tree() {
					resolved.push(format!("{}/{}", prefix, child.filename()));
				}
			}
		} else if member.contains('*') {
			// Multi-level globs (e.g. `crates/*/*`) aren't handled — log and skip.
			warn!(pattern = member, "Skipping unsupported workspace glob; only `prefix/*` is resolved");
		} else {
			resolved.push(member.clone());
		}
	}

	Ok(resolved)
}

pub fn check_package_version(
	manifest: &toml::Value,
	package_name: &str,
) -> Result<Option<semver::Version>, Box<dyn std::error::Error>> {
	let Some(pkg) = manifest.get("package") else {
		return Ok(None);
	};

	let name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("");
	if name != package_name {
		return Ok(None);
	}

	let Some(ver_str) = pkg.get("version").and_then(|v| v.as_str()) else {
		return Ok(None);
	};

	Ok(Some(semver::Version::parse(ver_str)?))
}
