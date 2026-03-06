use std::path::PathBuf;

use gix::{Repository, progress::Discard, remote};
use tracing::{debug, instrument, warn};
use url::Url;

use crate::error::GitError;

#[instrument(skip_all, fields(remote = %remote))]
pub fn clone_repository(out_path: &PathBuf, remote: &Url) -> Result<Repository, GitError> {
	let mut fetch_handle = gix::prepare_clone(remote.to_string(), &out_path)
		.map_err(|e| GitError::Clone(e.into()))?
		.with_fetch_options(remote::ref_map::Options::default());

	let (mut checkout_handle, _) = fetch_handle
		.fetch_then_checkout(Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|e| GitError::Checkout(e.into()))?;

	let (repo, _) = checkout_handle
		.main_worktree(Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|e| GitError::Checkout(e.into()))?;

	Ok(repo)
}

/// Walk newest→oldest. First commit whose Cargo.toml has `package_name` at
/// `target_version` is the latest commit for that version.
#[instrument(skip(repo), fields(package = %package_name, version = %target_version))]
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
				debug!(commit = %commit_id.id(), "found matching commit");
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
) -> Result<Option<semver::Version>, GitError> {
	let Some(entry) = tree
		.lookup_entry_by_path("Cargo.toml")
		.map_err(|e| GitError::TreeLookup { path: "Cargo.toml".into(), source: e.into() })?
		.filter(|e| e.mode().is_blob())
	else {
		return Ok(None);
	};

	let blob = repo
		.find_blob(entry.oid())
		.map_err(|e| GitError::TreeLookup { path: "Cargo.toml".into(), source: e.into() })?;
	let content = std::str::from_utf8(&blob.data)
		.map_err(|e| GitError::BlobEncoding { path: "Cargo.toml".into(), source: e })?;
	let manifest: toml::Value = toml::from_str(content)
		.map_err(|e| GitError::TomlParse { path: "Cargo.toml".into(), source: e })?;

	if let Some(workspace) = manifest.get("workspace") {
		let members: Vec<String> = workspace
			.get("members")
			.and_then(|m| m.as_array())
			.map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
			.unwrap_or_default();

		for member_path in resolve_workspace_members(repo, tree, &members)? {
			let cargo_path = format!("{}/Cargo.toml", member_path);
			let Some(member_entry) = tree
				.lookup_entry_by_path(&cargo_path)
				.map_err(|e| GitError::TreeLookup { path: cargo_path.clone(), source: e.into() })?
				.filter(|e| e.mode().is_blob())
			else {
				continue;
			};

			let blob = repo
				.find_blob(member_entry.oid())
				.map_err(|e| GitError::TreeLookup { path: cargo_path.clone(), source: e.into() })?;
			let content = std::str::from_utf8(&blob.data)
				.map_err(|e| GitError::BlobEncoding { path: cargo_path.clone(), source: e })?;
			let member_manifest: toml::Value = toml::from_str(content)
				.map_err(|e| GitError::TomlParse { path: cargo_path.clone(), source: e })?;

			if let Some(v) = check_package_version(&member_manifest, package_name, &cargo_path)? {
				return Ok(Some(v));
			}
		}

		Ok(None)
	} else {
		check_package_version(&manifest, package_name, "Cargo.toml")
	}
}

/// Resolve workspace member globs against the live tree.
/// Handles the common `crates/*` pattern; warns on anything more exotic.
pub fn resolve_workspace_members(
	repo: &gix::Repository,
	tree: &gix::Tree,
	members: &[String],
) -> Result<Vec<String>, GitError> {
	let mut resolved = Vec::new();

	for member in members {
		if let Some(prefix) = member.strip_suffix("/*") {
			let Some(dir_entry) = tree
				.lookup_entry_by_path(prefix)
				.map_err(|e| GitError::TreeLookup { path: prefix.into(), source: e.into() })?
			else {
				continue;
			};

			if !dir_entry.mode().is_tree() {
				continue;
			}

			let subtree = repo
				.find_tree(dir_entry.oid())
				.map_err(|e| GitError::TreeLookup { path: prefix.into(), source: e.into() })?;
			for child in subtree.iter() {
				let child = child.map_err(|e| GitError::TreeLookup {
					path: prefix.into(),
					source: e.into(),
				})?;
				if child.mode().is_tree() {
					resolved.push(format!("{}/{}", prefix, child.filename()));
				}
			}
		} else if member.contains('*') {
			warn!(pattern = member, "skipping unsupported workspace glob; only `prefix/*` is resolved");
		} else {
			resolved.push(member.clone());
		}
	}

	Ok(resolved)
}

pub fn check_package_version(
	manifest: &toml::Value,
	package_name: &str,
	manifest_path: &str,
) -> Result<Option<semver::Version>, GitError> {
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

	let version = semver::Version::parse(ver_str).map_err(|e| GitError::VersionParse {
		path:    manifest_path.into(),
		version: ver_str.into(),
		source:  e,
	})?;

	Ok(Some(version))
}
