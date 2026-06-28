use std::collections::HashSet;

use semver::Version;
use tracing::{instrument, warn};

use crate::http::error::GitError;

use super::core::find_commit_with_extractor;

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

pub fn extract_package_version(
	repo: &gix::Repository,
	tree: &gix::Tree,
	package_name: &str,
) -> Result<Option<Version>, GitError> {
	let Some(entry) = tree
		.lookup_entry_by_path("Cargo.toml")
		.map_err(|source| GitError::TreeLookupEntry { path: "Cargo.toml".into(), source })?
		.filter(|entry| entry.mode().is_blob())
	else {
		return Ok(None);
	};

	let blob = repo.find_blob(entry.oid()).map_err(|source| GitError::FindBlob {
		path:   "Cargo.toml".into(),
		source,
	})?;
	let content = std::str::from_utf8(&blob.data)
		.map_err(|source| GitError::BlobEncoding { path: "Cargo.toml".into(), source })?;
	let manifest: toml::Value = toml::from_str(content)
		.map_err(|source| GitError::TomlParse { path: "Cargo.toml".into(), source })?;

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

fn read_toml(
	repo: &gix::Repository,
	tree: &gix::Tree,
	path: &str,
) -> Result<toml::Value, GitError> {
	let Some(entry) = tree
		.lookup_entry_by_path(path)
		.map_err(|source| GitError::TreeLookupEntry { path: path.into(), source })?
		.filter(|entry| entry.mode().is_blob())
	else {
		return Ok(toml::Value::Table(Default::default()));
	};

	let blob = repo
		.find_blob(entry.oid())
		.map_err(|source| GitError::FindBlob { path: path.into(), source })?;
	let content = std::str::from_utf8(&blob.data)
		.map_err(|source| GitError::BlobEncoding { path: path.into(), source })?;

	toml::from_str(content).map_err(|source| GitError::TomlParse { path: path.into(), source })
}

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

	let version = Version::parse(version_string).map_err(|source| GitError::VersionParse {
		path: manifest_path.into(),
		version: version_string.into(),
		source,
	})?;

	Ok(Some(version))
}

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

fn collect_manifest_directories(
	repo: &gix::Repository,
	tree: &gix::Tree,
	prefix: &str,
	out: &mut Vec<String>,
) -> Result<Vec<String>, GitError> {
	for entry in tree.iter() {
		let entry = entry
			.map_err(|source| GitError::TreeTraverse { source })?;
		let name = entry.filename().to_string();
		let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };

		if entry.mode().is_tree() {
			let subtree = repo
				.find_tree(entry.oid())
				.map_err(|source| GitError::FindTree { path: path.clone(), source })?;
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

#[cfg(test)]
mod tests {
	use std::{fs, path::Path, process::Command};

	use tempfile::TempDir;
	use url::Url;

	use super::*;
	use crate::git::open_or_clone_repository;

	#[test]
	fn check_package_version_uses_workspace_inherited_version() -> Result<(), Box<dyn std::error::Error>> {
		let workspace_manifest: toml::Value = toml::from_str(
			r#"
[workspace]
members = ["crates/*"]

[workspace.package]
version = "0.14.0"
"#,
		)?;
		let member_manifest: toml::Value = toml::from_str(
			r#"
[package]
name = "iced"
version = { workspace = true }
"#,
		)?;

		let version = check_package_version(
			&member_manifest,
			Some(&workspace_manifest),
			"iced",
			"crates/iced/Cargo.toml",
		)?;

		assert_eq!(version, Some(Version::parse("0.14.0")?));
		Ok(())
	}

	#[test]
	fn resolve_workspace_members_supports_nested_globs_and_excludes() -> Result<(), Box<dyn std::error::Error>> {
		let repo = git_fixture(
			&[
				(
					"Cargo.toml",
					"[workspace]\nmembers = [\"packages/*/*\"]\nexclude = [\"packages/gui/internal\"]\n",
				),
				("packages/gui/public/Cargo.toml", "[package]\nname = \"public\"\nversion = \"0.1.0\"\n"),
				(
					"packages/gui/internal/Cargo.toml",
					"[package]\nname = \"internal\"\nversion = \"0.1.0\"\n",
				),
				("packages/core/model/Cargo.toml", "[package]\nname = \"model\"\nversion = \"0.1.0\"\n"),
			],
			&[],
		)?;
		let head = repo.head()?.peel_to_commit()?;
		let tree = head.tree()?;

		let members = resolve_workspace_members(&repo, &tree, &["packages/*/*".to_string()], &[
			"packages/gui/internal".to_string(),
		])?;

		assert_eq!(members, vec!["packages/core/model".to_string(), "packages/gui/public".to_string()]);
		Ok(())
	}

	#[test]
	fn extract_package_version_reads_workspace_member_version_from_manifest()
	-> Result<(), Box<dyn std::error::Error>> {
		let repo = git_fixture(
			&[
				(
					"Cargo.toml",
					"[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nversion = \"0.14.0\"\n",
				),
				("crates/iced/Cargo.toml", "[package]\nname = \"iced\"\nversion = { workspace = true }\n"),
			],
			&[],
		)?;
		let head = repo.head()?.peel_to_commit()?;
		let tree = head.tree()?;

		let version = extract_package_version(&repo, &tree, "iced")?;

		assert_eq!(version, Some(Version::parse("0.14.0")?));
		Ok(())
	}

	#[test]
	fn extract_package_version_falls_back_to_non_workspace_nested_crates() -> Result<(), Box<dyn std::error::Error>> {
		let repo = git_fixture(
			&[
				(
					"Cargo.toml",
					"[workspace]\nmembers = [\"cranelift\"]\n[workspace.package]\nversion = \"46.0.0\"\n",
				),
				("cranelift/Cargo.toml", "[package]\nname = \"cranelift-tools\"\nversion = \"0.0.0\"\n"),
				(
					"cranelift/umbrella/Cargo.toml",
					"[package]\nname = \"cranelift\"\nversion = \"0.131.1\"\n",
				),
			],
			&[],
		)?;
		let head = repo.head()?.peel_to_commit()?;
		let tree = head.tree()?;

		let version = extract_package_version(&repo, &tree, "cranelift")?;

		assert_eq!(version, Some(Version::parse("0.131.1")?));
		Ok(())
	}

	#[test]
	fn find_commit_for_version_scans_all_refs_not_just_head_history() -> Result<(), Box<dyn std::error::Error>> {
		let repo_dir = git_fixture_dir(
			&[("Cargo.toml", "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n")],
			&[],
		)?;

		run_git(repo_dir.path(), ["checkout", "-b", "release-0.2"])?;
		fs::write(
			repo_dir.path().join("Cargo.toml"),
			"[package]\nname = \"widget\"\nversion = \"0.2.0\"\n",
		)?;
		run_git(repo_dir.path(), ["add", "."])?;
		commit(repo_dir.path(), "release 0.2.0")?;

		run_git(repo_dir.path(), ["checkout", "main"])?;
		fs::write(
			repo_dir.path().join("Cargo.toml"),
			"[package]\nname = \"widget\"\nversion = \"0.1.1\"\n",
		)?;
		run_git(repo_dir.path(), ["add", "."])?;
		commit(repo_dir.path(), "main 0.1.1")?;

		let repo = gix::open(repo_dir.path())?;
		let found = find_commit_for_version(&repo, &Version::parse("0.2.0")?, "widget", None);

		assert!(found.is_some(), "expected to find version on non-head branch");
		Ok(())
	}

	#[test]
	fn open_or_clone_repository_reclones_when_cached_remote_differs() -> Result<(), Box<dyn std::error::Error>> {
		let first_remote =
			git_fixture_dir(&[("Cargo.toml", "[package]\nname = \"first\"\nversion = \"0.1.0\"\n")], &[
			])?;
		let second_remote = git_fixture_dir(
			&[("Cargo.toml", "[package]\nname = \"second\"\nversion = \"0.2.0\"\n")],
			&[],
		)?;
		let cache_dir = tempfile::tempdir()?;
		let checkout_path = cache_dir.path().join("repo");
		let first_url = Url::from_directory_path(first_remote.path()).unwrap();
		let second_url = Url::from_directory_path(second_remote.path()).unwrap();

		open_or_clone_repository(&checkout_path, &first_url)?;
		let recloned = open_or_clone_repository(&checkout_path, &second_url)?;
		let head = recloned.head()?.peel_to_commit()?;
		let tree = head.tree()?;

		assert_eq!(
			extract_package_version(&recloned, &tree, "second")?,
			Some(Version::parse("0.2.0")?)
		);
		assert_eq!(extract_package_version(&recloned, &tree, "first")?, None);
		Ok(())
	}

	fn git_fixture(
		files: &[(&str, &str)],
		extra_commits: &[Vec<(&str, &str)>],
	) -> Result<gix::Repository, Box<dyn std::error::Error>> {
		let dir = git_fixture_dir(files, extra_commits)?;
		let path = dir.keep();
		Ok(gix::open(path)?)
	}

	fn git_fixture_dir(
		files: &[(&str, &str)],
		extra_commits: &[Vec<(&str, &str)>],
	) -> Result<TempDir, Box<dyn std::error::Error>> {
		let dir = tempfile::tempdir()?;
		for (path, contents) in files {
			write_file(dir.path(), path, contents)?;
		}

		run_git(dir.path(), ["init", "-b", "main"])?;
		run_git(dir.path(), ["add", "."])?;
		commit(dir.path(), "fixture")?;

		for (idx, commit_files) in extra_commits.iter().enumerate() {
			for (path, contents) in commit_files {
				write_file(dir.path(), path, contents)?;
			}
			run_git(dir.path(), ["add", "."])?;
			commit(dir.path(), &format!("fixture-{idx}"))?;
		}

		Ok(dir)
	}

	fn write_file(root: &Path, relative: &str, contents: &str) -> Result<(), Box<dyn std::error::Error>> {
		let path = root.join(relative);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(path, contents)?;
		Ok(())
	}

	fn commit(cwd: &Path, message: &str) -> Result<(), Box<dyn std::error::Error>> {
		run_git(cwd, [
			"-c",
			"user.name=Codex",
			"-c",
			"user.email=codex@example.com",
			"-c",
			"commit.gpgsign=false",
			"-c",
			"tag.gpgsign=false",
			"commit",
			"-m",
			message,
		])
	}

	fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<(), Box<dyn std::error::Error>> {
		let status = Command::new("git").args(args).current_dir(cwd).status()?;

		if !status.success() {
			return Err(format!("git {:?} failed with status {}", args, status).into());
		}

		Ok(())
	}
}
