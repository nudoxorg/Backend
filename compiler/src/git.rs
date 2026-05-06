use std::{collections::HashSet, fs, path::Path};

use gix::{Repository, bstr::ByteSlice, progress::Discard, remote};
use semver::Version;
use tracing::{debug, instrument, warn};
use url::Url;

use crate::error::GitError;

#[instrument(skip_all, fields(remote = %remote, path = %out_path.display()))]
pub fn open_or_clone_repository(out_path: &Path, remote: &Url) -> Result<Repository, GitError> {
	if out_path.exists() {
		let repo = gix::open(out_path)
			.map_err(|source| GitError::Open { path: out_path.to_path_buf(), source: source.into() })?;

		if repository_matches_remote(&repo, remote)? {
			return Ok(repo);
		}

		fs::remove_dir_all(out_path)
			.map_err(|source| GitError::Open { path: out_path.to_path_buf(), source: source.into() })?;
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

fn repository_matches_remote(repo: &Repository, remote: &Url) -> Result<bool, GitError> {
	let fetch_remote = repo
		.find_fetch_remote(Some("origin".as_bytes().as_bstr()))
		.or_else(|_| repo.find_fetch_remote(None))
		.map_err(|source| GitError::Fetch(source.into()))?;
	let Some(configured_url) = fetch_remote.url(remote::Direction::Fetch) else {
		return Ok(false);
	};

	Ok(configured_url.to_string() == remote.as_str())
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

			match extract_package_version(repo, &tree, package_name) {
				Ok(Some(v)) if &v == target_version => {
					debug!(commit = %detached, "found matching commit");
					return Some(detached);
				}
				_ => continue,
			}
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
		.map_err(|source| GitError::TreeLookup { path: path.into(), source: source.into() })?
		.filter(|entry| entry.mode().is_blob())
	else {
		return Ok(toml::Value::Table(Default::default()));
	};

	let blob = repo
		.find_blob(entry.oid())
		.map_err(|source| GitError::TreeLookup { path: path.into(), source: source.into() })?;
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
			.map_err(|source| GitError::TreeLookup { path: prefix.into(), source: source.into() })?;
		let name = entry.filename().to_string();
		let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };

		if entry.mode().is_tree() {
			let subtree = repo
				.find_tree(entry.oid())
				.map_err(|source| GitError::TreeLookup { path: path.clone(), source: source.into() })?;
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

fn version_search_start_points(
	repo: &gix::Repository,
	start: Option<gix::ObjectId>,
) -> Result<Vec<gix::ObjectId>, Box<dyn std::error::Error + Send + Sync>> {
	let mut starts = Vec::new();
	let mut seen = HashSet::new();

	if let Some(start) = start {
		seen.insert(start);
		starts.push(start);
	} else {
		let head = repo.head()?.peel_to_object()?.id().detach();
		seen.insert(head);
		starts.push(head);
	}

	let refs = repo.references()?;
	for reference in refs.all()?.peeled()? {
		let mut reference = reference?;
		let Ok(id) = reference.peel_to_id() else {
			continue;
		};
		let id = id.detach();
		if seen.insert(id) {
			starts.push(id);
		}
	}

	Ok(starts)
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

#[cfg(test)]
mod tests {
	use std::{fs, path::Path, process::Command};

	use color_eyre::eyre::WrapErr;
	use tempfile::TempDir;

	use super::*;

	#[test]
	fn check_package_version_uses_workspace_inherited_version() -> color_eyre::Result<()> {
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
	fn resolve_workspace_members_supports_nested_globs_and_excludes() -> color_eyre::Result<()> {
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
	fn extract_package_version_reads_workspace_member_version_from_manifest() -> color_eyre::Result<()>
	{
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
	fn extract_package_version_falls_back_to_non_workspace_nested_crates() -> color_eyre::Result<()> {
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
	fn find_commit_for_version_scans_all_refs_not_just_head_history() -> color_eyre::Result<()> {
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
		run_git(repo_dir.path(), [
			"-c",
			"user.name=Codex",
			"-c",
			"user.email=codex@example.com",
			"commit",
			"-m",
			"release 0.2.0",
		])?;

		run_git(repo_dir.path(), ["checkout", "main"])?;
		fs::write(
			repo_dir.path().join("Cargo.toml"),
			"[package]\nname = \"widget\"\nversion = \"0.1.1\"\n",
		)?;
		run_git(repo_dir.path(), ["add", "."])?;
		run_git(repo_dir.path(), [
			"-c",
			"user.name=Codex",
			"-c",
			"user.email=codex@example.com",
			"commit",
			"-m",
			"main 0.1.1",
		])?;

		let repo = gix::open(repo_dir.path())?;
		let found = find_commit_for_version(&repo, &Version::parse("0.2.0")?, "widget", None);

		assert!(found.is_some(), "expected to find version on non-head branch");
		Ok(())
	}

	#[test]
	fn open_or_clone_repository_reclones_when_cached_remote_differs() -> color_eyre::Result<()> {
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
	) -> color_eyre::Result<gix::Repository> {
		let dir = git_fixture_dir(files, extra_commits)?;
		let path = dir.keep();
		Ok(gix::open(path)?)
	}

	fn git_fixture_dir(
		files: &[(&str, &str)],
		extra_commits: &[Vec<(&str, &str)>],
	) -> color_eyre::Result<TempDir> {
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

	fn write_file(root: &Path, relative: &str, contents: &str) -> color_eyre::Result<()> {
		let path = root.join(relative);
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		fs::write(path, contents)?;
		Ok(())
	}

	fn commit(cwd: &Path, message: &str) -> color_eyre::Result<()> {
		run_git(cwd, [
			"-c",
			"user.name=Codex",
			"-c",
			"user.email=codex@example.com",
			"commit",
			"-m",
			message,
		])
	}

	fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> color_eyre::Result<()> {
		let status =
			Command::new("git").args(args).current_dir(cwd).status().wrap_err("failed to spawn git")?;

		if !status.success() {
			color_eyre::eyre::bail!("git {:?} failed with status {}", args, status);
		}

		Ok(())
	}
}
