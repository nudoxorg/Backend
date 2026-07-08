//! Version resolution over git.
//!
//! Walks newest to oldest across every ref tip and returns the first commit
//! whose manifest (`Cargo.toml`) declares the requested
//! version, then materializes that commit's tree into a workspace for parsing.
//!
//! Cargo specifics handled here: the manifest may be a plain package, a
//! workspace root (whose members — including glob patterns with excludes —
//! are probed), or a member inheriting `version = { workspace = true }` from
//! the root's `[workspace.package]`. Monorepos that keep publishable crates
//! outside the member list fall back to scanning every `Cargo.toml` in the
//! tree. The git plumbing itself lives in [`crate::languages::vcs`].

use std::collections::HashSet;

use semver::Version;
use tracing::{instrument, warn};

use crate::languages::vcs::{
	find_commit_with_extractor, GitError, ManifestParseError, TreeError,
};
pub use crate::languages::vcs::{
	GitError, ManifestParseError, materialize_commit, open_or_clone_repository, TreeError,
};

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
	find_commit_with_extractor(repo, target_version, package_name, start, |r, t, n| {
		extract_package_version(r, t, n).ok().flatten()
	})
}

/// Read the version `package_name` declares in `tree`, if any.
///
/// Handles plain packages, workspace roots (probing members resolved from
/// `[workspace] members`/`exclude` globs, then falling back to every manifest
/// in the tree), and `version = { workspace = true }` inheritance.
pub fn extract_package_version(
	repo: &gix::Repository,
	tree: &gix::Tree,
	package_name: &str,
) -> Result<Option<Version>, GitError> {
	let Some(entry) = tree
		.lookup_entry_by_path("Cargo.toml")
		.map_err(|source| TreeError::LookupEntry { path: "Cargo.toml".into(), source })?
		.filter(|entry| entry.mode().is_blob())
	else {
		return Ok(None);
	};

	let blob = repo
		.find_blob(entry.oid())
		.map_err(|source| TreeError::FindBlob { path: "Cargo.toml".into(), source })?;
	let content = std::str::from_utf8(&blob.data)
		.map_err(|source| ManifestParseError::Utf8 { path: "Cargo.toml".into(), source })?;
	let manifest: toml::Value = toml::from_str(content)
		.map_err(|source| ManifestParseError::Toml { path: "Cargo.toml".into(), source })?;

	if let Some(workspace) = manifest.get("workspace") {
		// A workspace root can also be a package itself ([workspace] + [package]).
		if let Some(version) =
			check_package_version(&manifest, Some(&manifest), package_name, "Cargo.toml")?
		{
			return Ok(Some(version));
		}

		let members: Vec<String> = workspace
			.get("members")
			.and_then(|members| members.as_array())
			.map(|members| members.iter().filter_map(|value| value.as_str().map(String::from)).collect())
			.unwrap_or_default();
		let excludes: Vec<String> = workspace
			.get("exclude")
			.and_then(|members| members.as_array())
			.map(|members| members.iter().filter_map(|value| value.as_str().map(String::from)).collect())
			.unwrap_or_default();

		for member_path in resolve_workspace_members(repo, tree, &members, &excludes)? {
			let cargo_path = format!("{member_path}/Cargo.toml");
			let member_manifest = read_toml(repo, tree, &cargo_path)?;

			if let Some(version) =
				check_package_version(&member_manifest, Some(&manifest), package_name, &cargo_path)?
			{
				return Ok(Some(version));
			}
		}

		// Some monorepos publish additional crates that are not listed as workspace
		// members at the root (for example, archived or umbrella crates kept under
		// nested subdirectories). Fall back to scanning every manifest in the tree so
		// explicit repository sources can still resolve those versions.
		for manifest_dir in collect_manifest_directories(repo, tree, "", &mut Vec::new())? {
			let cargo_path = format!("{manifest_dir}/Cargo.toml");
			let member_manifest = read_toml(repo, tree, &cargo_path)?;

			if let Some(version) =
				check_package_version(&member_manifest, Some(&manifest), package_name, &cargo_path)?
			{
				return Ok(Some(version));
			}
		}

		Ok(None)
	} else {
		check_package_version(&manifest, None, package_name, "Cargo.toml")
	}
}

/// Read `path` from `tree` as TOML, or an empty table when the entry is
/// missing/not a blob.
fn read_toml(
	repo: &gix::Repository,
	tree: &gix::Tree,
	path: &str,
) -> Result<toml::Value, GitError> {
	let Some(entry) = tree
		.lookup_entry_by_path(path)
		.map_err(|source| TreeError::LookupEntry { path: path.into(), source })?
		.filter(|entry| entry.mode().is_blob())
	else {
		return Ok(toml::Value::Table(Default::default()));
	};

	let blob = repo
		.find_blob(entry.oid())
		.map_err(|source| TreeError::FindBlob { path: path.into(), source })?;
	let content = std::str::from_utf8(&blob.data)
		.map_err(|source| ManifestParseError::Utf8 { path: path.into(), source })?;

	toml::from_str(content)
		.map_err(|source| ManifestParseError::Toml { path: path.into(), source })?
}

/// Expand `[workspace] members` patterns (including globs) against the
/// directories in `tree` that contain a `Cargo.toml`, dropping any directory
/// matched by an `exclude` pattern.
pub fn resolve_workspace_members(
	repo: &gix::Repository,
	tree: &gix::Tree,
	members: &[String],
	excludes: &[String],
) -> Result<Vec<String>, GitError> {
	let mut resolved = Vec::new();
	let mut seen = HashSet::new();
	let candidates = collect_manifest_directories(repo, tree, "", &mut Vec::new())?;

	for member in members {
		let is_glob = member.contains('*');
		let mut matched = false;

		for candidate in &candidates {
			let is_match =
				if is_glob { path_matches_pattern(candidate, member) } else { candidate == member };

			if !is_match || excludes.iter().any(|exclude| path_matches_pattern(candidate, exclude)) {
				continue;
			}

			matched = true;
			if seen.insert(candidate.clone()) {
				resolved.push(candidate.clone());
			}
		}

		if is_glob && !matched {
			warn!(pattern = member, "workspace member glob did not match any Cargo manifests");
		}
	}

	Ok(resolved)
}

/// If `manifest` declares `[package] name = package_name`, parse and return
/// its version — consulting `workspace_manifest`'s `[workspace.package]` when
/// the member declares `version = { workspace = true }`.
pub fn check_package_version(
	manifest: &toml::Value,
	workspace_manifest: Option<&toml::Value>,
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

	let Some(version_string) = resolve_package_version_string(manifest, workspace_manifest) else {
		return Ok(None);
	};

	let version = Version::parse(version_string).map_err(|source| ManifestParseError::Version {
		path: manifest_path.into(),
		version: version_string.into(),
		source,
	})?;

	Ok(Some(version))
}

/// The literal version string for `[package] version`, following
/// `{ workspace = true }` indirection into the workspace root manifest.
fn resolve_package_version_string<'a>(
	manifest: &'a toml::Value,
	workspace_manifest: Option<&'a toml::Value>,
) -> Option<&'a str> {
	let pkg = manifest.get("package")?;
	let version = pkg.get("version")?;

	if let Some(version) = version.as_str() {
		return Some(version);
	}

	let workspace_inherited = version
		.as_table()
		.and_then(|table| table.get("workspace"))
		.and_then(|value| value.as_bool())
		.unwrap_or(false);

	if !workspace_inherited {
		return None;
	}

	workspace_manifest
		.and_then(|manifest| manifest.get("workspace"))
		.and_then(|workspace| workspace.get("package"))
		.and_then(|package| package.get("version"))
		.and_then(|value| value.as_str())
}

/// Every directory (recursively) under `tree` that contains a `Cargo.toml`,
/// excluding the root itself.
fn collect_manifest_directories(
	repo: &gix::Repository,
	tree: &gix::Tree,
	prefix: &str,
	out: &mut Vec<String>,
) -> Result<Vec<String>, GitError> {
	for entry in tree.iter() {
		let entry = entry.map_err(|source| TreeError::Traverse { source })?;
		let name = entry.filename().to_string();
		let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };

		if entry.mode().is_tree() {
			let subtree = repo
				.find_tree(entry.oid())
				.map_err(|source| TreeError::FindTree { path: path.clone(), source })?;
			collect_manifest_directories(repo, &subtree, &path, out)?;
			continue;
		}

		if entry.mode().is_blob() && name == "Cargo.toml" {
			let parent = path.strip_suffix("/Cargo.toml").unwrap_or("").to_string();
			if !parent.is_empty() {
				out.push(parent);
			}
		}
	}

	Ok(out.clone())
}

/// Whether `path` matches a Cargo workspace glob `pattern` (segment-wise `*`,
/// with `**` spanning any number of segments).
fn path_matches_pattern(path: &str, pattern: &str) -> bool {
	let path_segments =
		if path.is_empty() { Vec::new() } else { path.split('/').collect::<Vec<_>>() };
	let pattern_segments =
		if pattern.is_empty() { Vec::new() } else { pattern.split('/').collect::<Vec<_>>() };

	match_path_segments(&path_segments, &pattern_segments)
}

fn match_path_segments(path: &[&str], pattern: &[&str]) -> bool {
	match pattern.split_first() {
		None => path.is_empty(),
		Some((&"**", rest)) => {
			match_path_segments(path, rest)
				|| (!path.is_empty() && match_path_segments(&path[1..], pattern))
		}
		Some((segment, rest)) => {
			!path.is_empty() && segment_matches(path[0], segment) && match_path_segments(&path[1..], rest)
		}
	}
}

fn segment_matches(value: &str, pattern: &str) -> bool {
	if pattern == "*" {
		return true;
	}

	let value = value.as_bytes();
	let pattern = pattern.as_bytes();
	let (mut value_idx, mut pattern_idx) = (0usize, 0usize);
	let (mut wildcard_idx, mut wildcard_value_idx) = (None, 0usize);

	while value_idx < value.len() {
		if pattern_idx < pattern.len() && (pattern[pattern_idx] == value[value_idx]) {
			value_idx += 1;
			pattern_idx += 1;
		} else if pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
			wildcard_idx = Some(pattern_idx);
			pattern_idx += 1;
			wildcard_value_idx = value_idx;
		} else if let Some(wildcard_idx) = wildcard_idx {
			pattern_idx = wildcard_idx + 1;
			wildcard_value_idx += 1;
			value_idx = wildcard_value_idx;
		} else {
			return false;
		}
	}

	while pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
		pattern_idx += 1;
	}

	pattern_idx == pattern.len()
}
