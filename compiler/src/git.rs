use std::path::Path;

use gix::{Repository, bstr::ByteSlice, progress::Discard, remote};
use semver::Version;
use tracing::{debug, instrument, warn};
use url::Url;

use crate::error::GitError;

#[instrument(skip_all, fields(remote = %remote, path = %out_path.display()))]
pub fn open_or_clone_repository(out_path: &Path, remote: &Url) -> Result<Repository, GitError> {
	if out_path.exists() {
		return gix::open(out_path)
			.map_err(|source| GitError::Open { path: out_path.to_path_buf(), source: source.into() });
	}

	clone_repository(out_path, remote)
}

#[instrument(skip_all, fields(remote = %remote, path = %out_path.display()))]
pub fn clone_repository(out_path: &Path, remote: &Url) -> Result<Repository, GitError> {
	if let Some(parent) = out_path.parent() {
		std::fs::create_dir_all(parent)
			.map_err(|source| GitError::Open { path: parent.to_path_buf(), source: source.into() })?;
	}

	let mut fetch_handle = gix::prepare_clone(remote.to_string(), out_path)
		.map_err(|source| GitError::Clone(source.into()))?
		.with_fetch_options(remote::ref_map::Options::default());

	let (mut checkout_handle, _) = fetch_handle
		.fetch_then_checkout(Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|source| GitError::Checkout(source.into()))?;

	let (repo, _) = checkout_handle
		.main_worktree(Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|source| GitError::Checkout(source.into()))?;

	Ok(repo)
}

#[instrument(skip(repo), fields(remote = remote_name.unwrap_or("origin")))]
pub fn fetch_remote_updates(repo: &Repository, remote_name: Option<&str>) -> Result<(), GitError> {
	let remote = repo
		.find_fetch_remote(remote_name.map(|name| name.as_bytes().as_bstr()))
		.map_err(|source| GitError::Fetch(source.into()))?;

	let connection =
		remote.connect(remote::Direction::Fetch).map_err(|source| GitError::Fetch(source.into()))?;
	let prepare = connection
		.prepare_fetch(Discard, remote::ref_map::Options::default())
		.map_err(|source| GitError::Fetch(source.into()))?;

	prepare
		.with_write_packed_refs_only(true)
		.with_reflog_message(gix::remote::fetch::RefLogMessage::Prefixed { action: "fetch".to_owned() })
		.receive(Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|source| GitError::Fetch(source.into()))?;

	Ok(())
}

#[instrument(skip(repo), fields(remote = %remote_name, branch = %branch))]
pub fn remote_branch_commit(
	repo: &Repository,
	remote_name: &str,
	branch: &str,
) -> Option<gix::ObjectId> {
	let ref_name = format!("refs/remotes/{remote_name}/{branch}");
	repo.find_reference(&ref_name).ok()?.peel_to_id().ok().map(|id| id.detach())
}

#[instrument(skip_all, fields(commit = %commit, destination = %destination.display()))]
pub fn materialize_commit(
	repo: &Repository,
	commit: gix::ObjectId,
	destination: &Path,
) -> Result<(), GitError> {
	std::fs::create_dir_all(destination).map_err(|source| GitError::Open {
		path:   destination.to_path_buf(),
		source: source.into(),
	})?;

	let tree_id = repo
		.find_commit(commit)
		.map_err(|source| GitError::Reference {
			name:   commit.to_hex().to_string(),
			source: source.into(),
		})?
		.tree_id()
		.map_err(|source| GitError::Reference {
			name:   commit.to_hex().to_string(),
			source: source.into(),
		})?;

	let mut index = repo.index_from_tree(&tree_id).map_err(|source| GitError::IndexFromTree {
		tree:   tree_id.to_string(),
		source: source.into(),
	})?;
	let mut options = repo
		.checkout_options(gix_worktree::stack::state::attributes::Source::IdMapping)
		.map_err(|source| GitError::CheckoutOptions(source.into()))?;
	options.destination_is_initially_empty = true;
	options.overwrite_existing = true;

	gix_worktree_state::checkout(
		&mut index,
		destination,
		repo.objects.clone().into_arc().map_err(|source| GitError::OpenArcObjects { source })?,
		&Discard,
		&Discard,
		&gix::interrupt::IS_INTERRUPTED,
		options,
	)
	.map_err(|source| GitError::Materialize(source.into()))?;

	Ok(())
}

/// Walk newest→oldest. First commit whose Cargo.toml has `package_name` at
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
	let head_id = match start {
		Some(id) => id,
		None => repo.head().ok()?.peel_to_object().ok()?.id(),
	};
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
	let head_id = match start {
		Some(id) => id,
		None => repo.head().ok()?.peel_to_object().ok()?.id(),
	};
	let revwalk = repo.rev_walk([head_id]);

	for commit_id in revwalk.all().ok()? {
		let commit_id = commit_id.ok()?;
		let commit = repo.find_commit(commit_id.id()).ok()?;
		let tree = commit.tree().ok()?;

		match extract_typescript_package_version(repo, &tree, package_name) {
			Some(version) if &version == target_version => {
				debug!(commit = %commit_id.id(), "found matching TypeScript package version");
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
) -> Result<Option<Version>, GitError> {
	let Some(entry) = tree
		.lookup_entry_by_path("Cargo.toml")
		.map_err(|source| GitError::TreeLookup { path: "Cargo.toml".into(), source: source.into() })?
		.filter(|entry| entry.mode().is_blob())
	else {
		return Ok(None);
	};

	let blob = repo.find_blob(entry.oid()).map_err(|source| GitError::TreeLookup {
		path:   "Cargo.toml".into(),
		source: source.into(),
	})?;
	let content = std::str::from_utf8(&blob.data)
		.map_err(|source| GitError::BlobEncoding { path: "Cargo.toml".into(), source })?;
	let manifest: toml::Value = toml::from_str(content)
		.map_err(|source| GitError::TomlParse { path: "Cargo.toml".into(), source })?;

	if let Some(workspace) = manifest.get("workspace") {
		// A workspace root can also be a package itself ([workspace] + [package]).
		if let Some(version) = check_package_version(&manifest, package_name, "Cargo.toml")? {
			return Ok(Some(version));
		}

		let members: Vec<String> = workspace
			.get("members")
			.and_then(|members| members.as_array())
			.map(|members| members.iter().filter_map(|value| value.as_str().map(String::from)).collect())
			.unwrap_or_default();

		for member_path in resolve_workspace_members(repo, tree, &members)? {
			let cargo_path = format!("{member_path}/Cargo.toml");
			let Some(member_entry) = tree
				.lookup_entry_by_path(&cargo_path)
				.map_err(|source| GitError::TreeLookup {
					path:   cargo_path.clone(),
					source: source.into(),
				})?
				.filter(|entry| entry.mode().is_blob())
			else {
				continue;
			};

			let blob = repo.find_blob(member_entry.oid()).map_err(|source| GitError::TreeLookup {
				path:   cargo_path.clone(),
				source: source.into(),
			})?;
			let content = std::str::from_utf8(&blob.data)
				.map_err(|source| GitError::BlobEncoding { path: cargo_path.clone(), source })?;
			let member_manifest: toml::Value = toml::from_str(content)
				.map_err(|source| GitError::TomlParse { path: cargo_path.clone(), source })?;

			if let Some(version) = check_package_version(&member_manifest, package_name, &cargo_path)? {
				return Ok(Some(version));
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
				.map_err(|source| GitError::TreeLookup { path: prefix.into(), source: source.into() })?
			else {
				continue;
			};

			if !dir_entry.mode().is_tree() {
				continue;
			}

			let subtree = repo
				.find_tree(dir_entry.oid())
				.map_err(|source| GitError::TreeLookup { path: prefix.into(), source: source.into() })?;
			for child in subtree.iter() {
				let child = child.map_err(|source| GitError::TreeLookup {
					path:   prefix.into(),
					source: source.into(),
				})?;
				if child.mode().is_tree() {
					resolved.push(format!("{prefix}/{}", child.filename()));
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
) -> Result<Option<Version>, GitError> {
	let Some(pkg) = manifest.get("package") else {
		return Ok(None);
	};

	let name = pkg.get("name").and_then(|name| name.as_str()).unwrap_or("");
	if name != package_name {
		return Ok(None);
	}

	let Some(version_string) = pkg.get("version").and_then(|version| version.as_str()) else {
		return Ok(None);
	};

	let version = Version::parse(version_string).map_err(|source| GitError::VersionParse {
		path: manifest_path.into(),
		version: version_string.into(),
		source,
	})?;

	Ok(Some(version))
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
