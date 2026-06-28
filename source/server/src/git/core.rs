use std::{collections::HashSet, fs, path::Path, process::Command};

use semver::Version;
use gix::{Repository, bstr::ByteSlice, progress::Discard, remote};
use url::Url;
use tracing::{debug, instrument};

use crate::http::error::GitError;

#[instrument(skip_all, fields(remote = %remote, path = %out_path.display()))]
pub fn open_or_clone_repository(out_path: &Path, remote: &Url) -> Result<Repository, GitError> {
	if out_path.exists() {
		let repo = gix::open(out_path)
			.map_err(|source| GitError::Open { path: out_path.to_path_buf(), source })?;

		if repository_matches_remote(&repo, remote)? {
			return Ok(repo);
		}

		fs::remove_dir_all(out_path)
			.map_err(|source| GitError::Io { path: out_path.to_path_buf(), source })?;
	}

	clone_repository(out_path, remote)
}

#[instrument(skip_all, fields(remote = %remote, path = %out_path.display()))]
pub fn clone_repository(out_path: &Path, remote: &Url) -> Result<Repository, GitError> {
	if let Some(parent) = out_path.parent() {
		std::fs::create_dir_all(parent)
			.map_err(|source| GitError::Io { path: parent.to_path_buf(), source })?;
	}

	let mut fetch_handle = gix::prepare_clone(remote.to_string(), out_path)
		.map_err(|source| GitError::Clone { url: remote.to_string(), source })?
		.with_fetch_options(remote::ref_map::Options::default());

	let (mut checkout_handle, _) = fetch_handle
		.fetch_then_checkout(Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|source| GitError::FetchCheckout { source })?;

	let (repo, _) = checkout_handle
		.main_worktree(Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|source| GitError::WorktreeCheckout { source })?;

	Ok(repo)
}

fn repository_matches_remote(repo: &Repository, remote: &Url) -> Result<bool, GitError> {
	let fetch_remote = repo
		.find_fetch_remote(Some("origin".as_bytes().as_bstr()))
		.or_else(|_| repo.find_fetch_remote(None))
		.map_err(|source| GitError::FindRemote { source })?;
	let Some(configured_url) = fetch_remote.url(remote::Direction::Fetch) else {
		return Ok(false);
	};

	Ok(configured_url.to_string() == remote.as_str())
}

#[instrument(skip(repo), fields(remote = remote_name.unwrap_or("origin")))]
pub fn fetch_remote_updates(repo: &Repository, remote_name: Option<&str>) -> Result<(), GitError> {
	let remote = repo
		.find_fetch_remote(remote_name.map(|name| name.as_bytes().as_bstr()))
		.map_err(|source| GitError::FindRemote { source })?;

	let fetch_result: Result<(), GitError> = (|| {
		let connection =
			remote.connect(remote::Direction::Fetch).map_err(|source| GitError::Connect { source })?;
		let prepare = connection
			.prepare_fetch(Discard, remote::ref_map::Options::default())
			.map_err(|source| GitError::PrepareFetch { source })?;

		prepare
			.with_write_packed_refs_only(true)
			.with_reflog_message(gix::remote::fetch::RefLogMessage::Prefixed {
				action: "fetch".to_owned(),
			})
			.receive(Discard, &gix::interrupt::IS_INTERRUPTED)
			.map_err(|source| GitError::ReceiveFetch { source })?;

		Ok(())
	})();

	if fetch_result.is_ok() {
		return Ok(());
	}

	let repo_dir = repo.workdir().unwrap_or_else(|| repo.path());
	let remote_name = remote_name.unwrap_or("origin");
	let output = Command::new("git")
		.arg("-C")
		.arg(repo_dir)
		.arg("fetch")
		.arg("--prune")
		.arg(remote_name)
		.output()
		.map_err(|source| GitError::Io { path: repo_dir.to_path_buf(), source })?;

	if output.status.success() {
		return Ok(());
	}

	let stderr = String::from_utf8_lossy(&output.stderr);
	let stdout = String::from_utf8_lossy(&output.stdout);
	Err(GitError::Io {
		path:   repo_dir.to_path_buf(),
		source: std::io::Error::other(format!(
			"gix fetch failed and git fetch fallback exited with status {}: stdout: {} stderr: {}",
			output.status,
			stdout.trim(),
			stderr.trim()
		)),
	})
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
	std::fs::create_dir_all(destination).map_err(|source| GitError::Io {
		path:   destination.to_path_buf(),
		source,
	})?;

	let tree_id = repo
		.find_commit(commit)
		.map_err(|source| GitError::Reference {
			name:   commit.to_hex().to_string(),
			source,
		})?
		.tree_id()
		.map_err(|source| GitError::CommitDecode {
			name:   commit.to_hex().to_string(),
			source,
		})?;

	let mut index = repo.index_from_tree(&tree_id).map_err(|source| GitError::IndexFromTree {
		tree:   tree_id.to_string(),
		source,
	})?;
	let mut options = repo
		.checkout_options(gix_worktree::stack::state::attributes::Source::IdMapping)
		.map_err(GitError::CheckoutOptions)?;
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
	.map_err(GitError::Materialize)?;

	Ok(())
}

pub(crate) fn version_search_start_points(
	repo: &gix::Repository,
	start: Option<gix::ObjectId>,
) -> Result<Vec<gix::ObjectId>, GitError> {
	let mut starts = Vec::new();
	let mut seen = HashSet::new();

	if let Some(start) = start {
		seen.insert(start);
		starts.push(start);
	} else {
		let head = repo
			.head()
			.map_err(|source| GitError::Head { source })?
			.peel_to_object()
			.map_err(|source| GitError::ObjectLookup { source })?
			.id()
			.detach();
		seen.insert(head);
		starts.push(head);
	}

	let refs = repo
		.references()
		.map_err(|source| GitError::ReferencesOpen { source })?;
	for reference in refs
		.all()
		.map_err(|source| GitError::ReferencesAll { source })?
		.peeled()
		.map_err(|source| GitError::ReferencesPeeled { source })?
	{
		let Ok(mut reference) = reference else { continue; };
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

/// Shared commit-walk loop used by both Cargo and TypeScript commit finders.
/// Walks newest→oldest across all ref tips; returns the first commit for which
/// `extract_version` returns `Some(v)` where `v == target_version`.
pub(super) fn find_commit_with_extractor<F>(
	repo: &gix::Repository,
	target_version: &Version,
	package_name: &str,
	start: Option<gix::ObjectId>,
	extract_version: F,
) -> Option<gix::ObjectId>
where
	F: Fn(&gix::Repository, &gix::Tree<'_>, &str) -> Option<Version>,
{
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

			if let Some(v) = extract_version(repo, &tree, package_name) {
				if &v == target_version {
					debug!(commit = %detached, "found matching commit");
					return Some(detached);
				}
			}
		}
	}

	None
}
