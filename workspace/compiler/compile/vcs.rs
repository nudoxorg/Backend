//! Shared git plumbing for VCS version resolution, built on `gix`.
//!
//! The per-language `traversal` modules resolve "which commit declares
//! version X of package Y" by walking commits newest→oldest across every
//! peeled ref tip and probing each commit's tree with a language-specific
//! manifest extractor ([`find_commit_with_extractor`]). This module owns the
//! language-agnostic pieces:
//!
//! - [`open_or_clone_repository`]: reuse a cached clone, or wipe + re-clone
//!   when its configured fetch remote no longer matches the requested URL.
//! - [`fetch_remote_updates`] / [`remote_branch_commit`]: refresh remote
//!   tracking refs and pin a walk to a remote branch tip.
//! - [`find_commit_with_extractor`]: the shared newest→oldest commit walk,
//!   generic over the manifest-version extractor.
//! - [`materialize_commit`]: check a commit's tree out into a destination
//!   directory so the language toolchains can parse real source.
//!
//! Errors are collected in the self-contained [`GitError`] taxonomy.

use std::{collections::HashSet, fs, io, path::{Path, PathBuf}, process::Command};

use gix::{Repository, bstr::ByteSlice, progress::Discard, remote};
use semver::Version;
use thiserror::Error;
use tracing::{debug, instrument};
use url::Url;

/// A failure in the git plumbing layer (clone, fetch, ref walking, manifest
/// blob reads, or worktree materialization).
#[derive(Debug, Error)]
pub enum GitError {
	/// Cloning the remote repository failed.
	#[error("clone from `{url}` failed: {source}")]
	Clone {
		url:    String,
		#[source]
		source: gix::clone::Error,
	},

	/// Opening an existing repository on disk failed.
	#[error("failed to open repository at `{path}`: {source}")]
	Open {
		path:   PathBuf,
		#[source]
		source: gix::open::Error,
	},

	/// A filesystem operation failed.
	#[error("IO error at `{path}`: {source}")]
	Io {
		path:   PathBuf,
		#[source]
		source: io::Error,
	},

	/// Fetch-related failure (remote lookup, connect, prepare, receive, or shell fallback).
	#[error(transparent)]
	Fetch(#[from] FetchError),

	/// Checkout / worktree materialization failure.
	#[error(transparent)]
	Checkout(#[from] CheckoutError),

	/// Reference / commit / HEAD resolution failure.
	#[error(transparent)]
	Reference(#[from] ReferenceError),

	/// Tree traversal / entry lookup failure.
	#[error(transparent)]
	Tree(#[from] TreeError),

	/// Manifest (Cargo.toml / package.json etc) parse failure (utf8 / toml / json / semver).
	#[error(transparent)]
	ManifestParse(#[from] ManifestParseError),
}

/// Fetch / clone transport and fallback failures.
#[derive(Debug, Error)]
pub enum FetchError {
	/// The repository has no usable fetch remote.
	#[error("failed to find git remote: {source}")]
	FindRemote {
		#[source]
		source: gix::remote::find::for_fetch::Error,
	},

	/// Connecting to the remote for fetching failed.
	#[error("failed to connect to remote: {source}")]
	Connect {
		#[source]
		source: gix::remote::connect::Error,
	},

	/// Negotiating the fetch with the remote failed.
	#[error("failed to prepare fetch: {source}")]
	PrepareFetch {
		#[source]
		source: gix::remote::fetch::prepare::Error,
	},

	/// Receiving the fetched pack failed.
	#[error("failed to receive fetch: {source}")]
	ReceiveFetch {
		#[source]
		source: gix::remote::fetch::Error,
	},

	/// The fetch phase of a clone failed.
	#[error("fetch/checkout failed: {source}")]
	FetchCheckout {
		#[source]
		source: gix::clone::fetch::Error,
	},

	/// `git fetch --prune` shell fallback (after gix transport error) failed.
	/// Captures status + stdout/stderr explicitly (no format! into Io source).
	#[error("git fetch fallback failed at `{path}` with status {status}: stdout={stdout:?} stderr={stderr:?}")]
	Fallback {
		path:   PathBuf,
		status: std::process::ExitStatus,
		stdout: String,
		stderr: String,
	},
}

/// Worktree checkout and materialization failures.
#[derive(Debug, Error)]
pub enum CheckoutError {
	/// Checking out the main worktree after a clone failed.
	#[error("worktree checkout failed: {source}")]
	WorktreeCheckout {
		#[source]
		source: gix::clone::checkout::main_worktree::Error,
	},

	/// Deriving checkout options from the repository config failed.
	#[error("failed to obtain checkout options: {source}")]
	CheckoutOptions {
		#[source]
		source: gix::config::checkout_options::Error,
	},

	/// Writing the tree contents to the destination directory failed.
	#[error("failed to materialize worktree: {source}")]
	Materialize {
		#[source]
		source: gix_worktree_state::checkout::Error,
	},

	/// Converting the object database into its `Arc`-backed form failed.
	#[error("failed to open Arc-backed object database: {source}")]
	OpenArcObjects {
		#[source]
		source: io::Error,
	},

	/// Building an in-memory index from a tree failed.
	#[error("failed to build index from tree `{tree}`: {source}")]
	IndexFromTree {
		tree:   String,
		#[source]
		source: gix::repository::index_from_tree::Error,
	},
}

/// Reference, HEAD, object, and packed-ref resolution failures.
#[derive(Debug, Error)]
pub enum ReferenceError {
	/// A commit/reference could not be resolved to an object.
	#[error("failed to resolve git reference `{name}`: {source}")]
	Resolve {
		name:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	/// A commit object could not be decoded to obtain its tree id.
	#[error("failed to decode commit tree id for `{name}`: {source}")]
	CommitDecode {
		name:   String,
		#[source]
		source: gix_object::decode::Error,
	},

	/// HEAD could not be looked up.
	#[error("failed to lookup git HEAD: {source}")]
	Head {
		#[source]
		source: gix::reference::find::existing::Error,
	},

	/// HEAD could not be peeled to an object.
	#[error("failed to find git object: {source}")]
	ObjectLookup {
		#[source]
		source: gix::head::peel::to_object::Error,
	},

	/// The reference store could not be opened for enumeration.
	#[error("failed to enumerate git references: {source}")]
	ReferencesOpen {
		#[source]
		source: gix::reference::iter::Error,
	},

	/// Iterating all references failed.
	#[error("failed to iterate all git references: {source}")]
	ReferencesAll {
		#[source]
		source: gix::reference::iter::init::Error,
	},

	/// Peeling packed references failed.
	#[error("failed to peel git references: {source}")]
	ReferencesPeeled {
		#[source]
		source: gix_ref::packed::buffer::open::Error,
	},
}

/// Tree walking and entry/blob/tree lookup failures.
#[derive(Debug, Error)]
pub enum TreeError {
	/// Looking up an entry by path within a tree failed.
	#[error("failed to find tree entry at `{path}`: {source}")]
	LookupEntry {
		path:   String,
		#[source]
		source: gix::object::find::existing::Error,
	},

	/// A blob object referenced by a tree entry could not be loaded.
	#[error("failed to find blob at `{path}`: {source}")]
	FindBlob {
		path:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	/// A subtree object referenced by a tree entry could not be loaded.
	#[error("failed to find tree at `{path}`: {source}")]
	FindTree {
		path:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	/// Iterating a tree's entries failed.
	#[error("failed to traverse tree: {source}")]
	Traverse {
		#[source]
		source: gix::diff::object::decode::Error,
	},
}

/// Failures parsing language manifests inside a commit tree (utf8, format, version).
/// Used by Rust (Cargo.toml) and potentially others; explicit variants avoid
/// format strings for parse data.
#[derive(Debug, Error)]
pub enum ManifestParseError {
	/// A manifest blob was not valid UTF-8.
	#[error("invalid utf-8 in blob at `{path}`: {source}")]
	Utf8 {
		path:   String,
		#[source]
		source: std::str::Utf8Error,
	},

	/// A manifest blob was not valid TOML.
	#[error("failed to parse `{path}` as TOML: {source}")]
	Toml {
		path:   String,
		#[source]
		source: toml::de::Error,
	},

	/// A manifest declared a version that is not valid semver.
	#[error("invalid version `{version}` in `{path}`: {source}")]
	Version {
		path:    String,
		version: String,
		#[source]
		source:  semver::Error,
	},
}

/// Open the repository cached at `out_path`, or (re-)clone `remote` into it.
///
/// A cached clone is only reused when its configured fetch remote matches
/// `remote`; on mismatch the directory is wiped and the repository is cloned
/// fresh, so a cache path can never serve source for the wrong origin.
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

/// Clone `remote` into `out_path` (creating parent directories) and check out
/// the main worktree.
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
		.map_err(|source| FetchError::FetchCheckout { source })?;

	let (repo, _) = checkout_handle
		.main_worktree(Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|source| CheckoutError::WorktreeCheckout { source })?;

	Ok(repo)
}

/// Whether the repository's configured fetch remote (preferring `origin`)
/// matches `remote`.
fn repository_matches_remote(repo: &Repository, remote: &Url) -> Result<bool, GitError> {
	let fetch_remote = repo
		.find_fetch_remote(Some("origin".as_bytes().as_bstr()))
		.or_else(|_| repo.find_fetch_remote(None))
		.map_err(|source| FetchError::FindRemote { source })?;
	let Some(configured_url) = fetch_remote.url(remote::Direction::Fetch) else {
		return Ok(false);
	};

	Ok(configured_url.to_string() == remote.as_str())
}

/// Fetch updated refs from `remote_name` (default `origin`), falling back to
/// shelling out to `git fetch --prune` when the `gix` transport fails.
#[instrument(skip(repo), fields(remote = remote_name.unwrap_or("origin")))]
pub fn fetch_remote_updates(repo: &Repository, remote_name: Option<&str>) -> Result<(), GitError> {
	let remote = repo
		.find_fetch_remote(remote_name.map(|name| name.as_bytes().as_bstr()))
		.map_err(|source| FetchError::FindRemote { source })?;

	let fetch_result: Result<(), GitError> = (|| {
		let connection = remote.connect(remote::Direction::Fetch).map_err(|source| FetchError::Connect { source })?;
		let prepare = connection
			.prepare_fetch(Discard, remote::ref_map::Options::default())
			.map_err(|source| FetchError::PrepareFetch { source })?;

		prepare
			.with_write_packed_refs_only(true)
			.with_reflog_message(gix::remote::fetch::RefLogMessage::Prefixed {
				action: "fetch".to_owned(),
			})
			.receive(Discard, &gix::interrupt::IS_INTERRUPTED)
			.map_err(|source| FetchError::ReceiveFetch { source })?;

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

	// Capture stdout/stderr explicitly into structured Fallback (no format! stuffing
	// into a generic Io source; all data fields are typed/explicit).
	let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
	let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
	Err(FetchError::Fallback {
		path:   repo_dir.to_path_buf(),
		status: output.status,
		stdout,
		stderr,
	}
	.into())
}

/// The commit at the tip of `refs/remotes/{remote_name}/{branch}`, if that
/// tracking ref exists. Useful as the `start` pin for a version walk after a
/// fetch.
#[instrument(skip(repo), fields(remote = %remote_name, branch = %branch))]
pub fn remote_branch_commit(
	repo: &Repository,
	remote_name: &str,
	branch: &str,
) -> Option<gix::ObjectId> {
	let ref_name = format!("refs/remotes/{remote_name}/{branch}");
	repo.find_reference(&ref_name).ok()?.peel_to_id().ok().map(|id| id.detach())
}

/// Check the tree of `commit` out into `destination` (created if missing),
/// overwriting existing files, so language toolchains can parse real source.
#[instrument(skip_all, fields(commit = %commit, destination = %destination.display()))]
pub fn materialize_commit(
	repo: &Repository,
	commit: gix::ObjectId,
	destination: &Path,
) -> Result<(), GitError> {
	std::fs::create_dir_all(destination)
		.map_err(|source| GitError::Io { path: destination.to_path_buf(), source })?;

	let tree_id = repo
		.find_commit(commit)
		.map_err(|source| ReferenceError::Resolve { name: commit.to_hex().to_string(), source })?
		.tree_id()
		.map_err(|source| ReferenceError::CommitDecode { name: commit.to_hex().to_string(), source })?;

	let mut index = repo
		.index_from_tree(&tree_id)
		.map_err(|source| CheckoutError::IndexFromTree { tree: tree_id.to_string(), source })?;
	let mut options = repo
		.checkout_options(gix_worktree::stack::state::attributes::Source::IdMapping)
		.map_err(|source| CheckoutError::CheckoutOptions { source })?;
	options.destination_is_initially_empty = true;
	options.overwrite_existing = true;

	let objects = repo
		.objects
		.clone()
		.into_arc()
		.map_err(|source| CheckoutError::OpenArcObjects { source })?;

	gix_worktree_state::checkout(
		&mut index,
		destination,
		objects,
		&Discard,
		&Discard,
		&gix::interrupt::IS_INTERRUPTED,
		options,
	)
	.map_err(|source| CheckoutError::Materialize { source })?;

	Ok(())
}

/// The set of commit ids a version walk should start from: `start` when
/// pinned (falling back to HEAD otherwise), plus every peeled ref tip, so
/// versions only reachable on non-HEAD branches/tags are still found.
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
			.map_err(|source| ReferenceError::Head { source })?
			.peel_to_object()
			.map_err(|source| ReferenceError::ObjectLookup { source })?
			.id()
			.detach();
		seen.insert(head);
		starts.push(head);
	}

	let refs = repo.references().map_err(|source| ReferenceError::ReferencesOpen { source })?;
	for reference in refs
		.all()
		.map_err(|source| ReferenceError::ReferencesAll { source })?
		.peeled()
		.map_err(|source| ReferenceError::ReferencesPeeled { source })?
	{
		let Ok(mut reference) = reference else {
			continue;
		};
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

/// Shared commit-walk loop used by the per-language commit finders.
///
/// Walks newest→oldest across all ref tips (see
/// [`version_search_start_points`]); returns the first commit for which
/// `extract_version` returns `Some(v)` where `v == target_version`.
///
/// `start` pins the walk to a specific commit (e.g. the remote tracking tip
/// after a fetch). Falls back to local HEAD when `None`.
pub fn find_commit_with_extractor<F>(
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
