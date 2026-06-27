//! The sync and monitor engine: check out the right commit for a tracked
//! package version, run the ingest pipeline, and (for monitoring) detect whether
//! the remote has advanced past the tracked commit.
//!
//! The git dance — open/clone the cached repo, fetch origin, resolve the remote
//! head, find the commit for the tracked version, and (for sync) materialize a
//! workspace — is identical across languages. The *only* per-language
//! difference is how a semver version maps to a commit (crates.io vs npm/JSR tag
//! conventions), so that is the single thing a [`LanguageBackend`] supplies;
//! [`materialize_git_checkout`] and [`resolve_git_head`] are shared by both
//! languages and by the repository-backed and registry-backed paths.

use gix::{ObjectId, Repository};
use semver::Version;
use tempfile::TempDir;
use tokio::task::spawn_blocking;
use url::Url;

use crate::{config::PipelineConfig, error::{AppError, PackageError, RegistryLookupError}, git, ingest::{IngestTargets, run_rust_pipeline, run_typescript_pipeline}, storage::StorageLayout, sync_progress::{PackageSyncPhase, ProgressReporter}};

use super::{MonitorExecution, PackageHandle, PackageId, PackageSpec, SyncExecution, ts_entry_point::{resolve_typescript_repository_entry_point, typescript_package_uses_repository, typescript_repository_entry_hint}};

/// Per-language strategy for the git-backed sync/monitor engine.
///
/// Everything on the git path — fetch, remote-head resolution, materialize — is
/// shared; the only thing that differs between languages is how a tracked semver
/// `version` resolves to a commit. A backend supplies exactly that.
trait LanguageBackend {
	/// Resolve `version` to a commit using this language's tag conventions.
	fn find_commit(
		repository: &Repository,
		version: &Version,
		name: &str,
		remote_head: Option<ObjectId>,
	) -> Option<ObjectId>;
}

/// Rust packages: crates.io / cargo tag conventions.
struct RustBackend;

impl LanguageBackend for RustBackend {
	fn find_commit(
		repository: &Repository,
		version: &Version,
		name: &str,
		remote_head: Option<ObjectId>,
	) -> Option<ObjectId> {
		git::find_commit_for_version(repository, version, name, remote_head)
	}
}

/// TypeScript packages: npm / JSR tag conventions.
struct TypeScriptBackend;

impl LanguageBackend for TypeScriptBackend {
	fn find_commit(
		repository: &Repository,
		version: &Version,
		name: &str,
		remote_head: Option<ObjectId>,
	) -> Option<ObjectId> {
		git::find_typescript_commit_for_version(repository, version, name, remote_head)
	}
}

/// A materialized git checkout: the resolved commit, the latest remote head for
/// the tracked branch, and the workspace it was checked out into.
struct GitCheckout {
	commit_hex:           String,
	latest_remote_commit: Option<String>,
	workspace:            TempDir,
}

/// Open (or clone) the cached repo, fetch origin, resolve the commit for the
/// tracked version via `B`, and materialize it into a fresh workspace.
///
/// Blocking (git + filesystem I/O); callers run it under `spawn_blocking`.
fn materialize_git_checkout<B: LanguageBackend>(
	storage: &StorageLayout,
	spec: &PackageSpec,
	source: &Url,
	name: &str,
	progress: &ProgressReporter,
) -> Result<GitCheckout, AppError> {
	progress.phase_with_detail(
		PackageSyncPhase::Resolving,
		Some(format!("opening repository for {name}")),
	);
	let repo_dir = storage
		.prepare_repository_dir(spec.language, &spec.slug, source)
		.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
	let repository = git::open_or_clone_repository(&repo_dir, source)?;

	progress.phase_with_detail(PackageSyncPhase::Fetching, Some(format!("fetching {}", spec.branch)));
	git::fetch_remote_updates(&repository, Some("origin"))?;

	let remote_head = git::remote_branch_commit(&repository, "origin", &spec.branch);
	let latest_remote_commit = remote_head.map(|id| id.to_hex().to_string());
	let target_commit = B::find_commit(&repository, &spec.version, name, remote_head)
		.ok_or_else(|| PackageError::VersionNotFound(spec.version.clone()))?;

	progress.phase_with_detail(
		PackageSyncPhase::Materializing,
		Some(format!("materializing {}", target_commit.to_hex())),
	);
	let workspace = storage
		.create_workspace()
		.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
	git::materialize_commit(&repository, target_commit, workspace.path())?;

	Ok(GitCheckout { commit_hex: target_commit.to_hex().to_string(), latest_remote_commit, workspace })
}

/// The monitor variant: resolve the tracked commit and the latest remote head
/// *without* materializing a workspace, and report whether the remote advanced.
///
/// Blocking; the caller ([`run_monitor_refresh`]) already runs under
/// `spawn_blocking`.
fn resolve_git_head<B: LanguageBackend>(
	storage: &StorageLayout,
	spec: &PackageSpec,
	source: &Url,
	name: &str,
) -> Result<MonitorExecution, AppError> {
	let repo_dir = storage
		.prepare_repository_dir(spec.language, &spec.slug, source)
		.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
	let repository = git::open_or_clone_repository(&repo_dir, source)?;
	git::fetch_remote_updates(&repository, Some("origin"))?;

	let remote_head = git::remote_branch_commit(&repository, "origin", &spec.branch);
	let latest_remote_commit = remote_head.map(|id| id.to_hex().to_string());
	let tracked_commit = B::find_commit(&repository, &spec.version, name, remote_head)
		.ok_or_else(|| PackageError::VersionNotFound(spec.version.clone()))?
		.to_hex()
		.to_string();

	Ok(MonitorExecution {
		remote_update_available: compute_remote_update_available(
			latest_remote_commit.as_ref(),
			Some(&tracked_commit),
		),
		latest_remote_commit,
	})
}

/// Run a git checkout off the async executor, mapping the join error.
async fn spawn_git_checkout<B: LanguageBackend + Send + 'static>(
	storage: StorageLayout,
	spec: PackageSpec,
	source: Url,
	name: String,
	progress: ProgressReporter,
) -> Result<GitCheckout, AppError> {
	spawn_blocking(move || materialize_git_checkout::<B>(&storage, &spec, &source, &name, &progress))
		.await
		.map_err(|source| AppError::TaskJoin { action: "git operations", source })?
}

pub(crate) async fn run_sync(
	storage: StorageLayout,
	pipeline: PipelineConfig,
	_package_id: PackageId,
	handle: PackageHandle,
	spec: PackageSpec,
	progress: ProgressReporter,
	targets: &IngestTargets,
) -> Result<SyncExecution, AppError> {
	match handle {
		PackageHandle::Rust(package) => {
			let checkout = spawn_git_checkout::<RustBackend>(
				storage.clone(),
				spec.clone(),
				package.source.clone(),
				package.name.clone(),
				progress.clone(),
			)
			.await?;

			let summary = run_rust_pipeline(
				&package,
				&spec.version,
				checkout.workspace.path(),
				&pipeline,
				&progress,
				targets,
			)
			.await?;

			Ok(SyncExecution {
				tracked_version_commit: checkout.commit_hex,
				latest_remote_commit:   checkout.latest_remote_commit,
				summary,
			})
		}
		PackageHandle::TypeScript(package) if typescript_package_uses_repository(&package) => {
			let checkout = spawn_git_checkout::<TypeScriptBackend>(
				storage.clone(),
				spec.clone(),
				package.source.clone(),
				package.name.clone(),
				progress.clone(),
			)
			.await?;

			let entry_point = resolve_typescript_repository_entry_point(
				checkout.workspace.path(),
				typescript_repository_entry_hint(&package),
			)?;
			let mut materialized_package = package.clone();
			materialized_package.entry_point = entry_point.display().to_string();

			progress.phase_with_detail(
				PackageSyncPhase::GeneratingIr,
				Some("generating TypeScript IR".to_owned()),
			);
			let summary = run_typescript_pipeline(
				&materialized_package,
				&spec.version,
				&pipeline,
				&progress,
				targets,
			)
			.await?;
			drop(checkout.workspace);

			Ok(SyncExecution {
				tracked_version_commit: checkout.commit_hex,
				latest_remote_commit:   checkout.latest_remote_commit,
				summary,
			})
		}
		// npm registry-backed TypeScript: no git, materialize the published tarball.
		PackageHandle::TypeScript(package) => {
			progress.phase_with_detail(
				PackageSyncPhase::Resolving,
				Some(format!("resolving npm package {}", package.name)),
			);
			let workspace = storage
				.create_workspace()
				.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
			progress.phase_with_detail(
				PackageSyncPhase::Materializing,
				Some(format!("materializing npm package {}@{}", package.name, spec.version)),
			);
			let entry_point = crate::core::ts::Npm::default()
				.materialize_package_version(&package.name, &spec.version, workspace.path())
				.await
				.map_err(|source| AppError::RegistryLookup(RegistryLookupError::Npm {
					language: spec.language,
					package:  package.name.clone(),
					source,
				}))?;
			let mut materialized_package = package.clone();
			materialized_package.entry_point = entry_point.display().to_string();

			progress.phase_with_detail(
				PackageSyncPhase::GeneratingIr,
				Some("generating TypeScript IR".to_owned()),
			);
			let summary = run_typescript_pipeline(
				&materialized_package,
				&spec.version,
				&pipeline,
				&progress,
				targets,
			)
			.await?;
			drop(workspace);

			Ok(SyncExecution {
				tracked_version_commit: format!("npm:{}@{}", package.name, spec.version),
				latest_remote_commit:   None,
				summary,
			})
		}
	}
}

pub(crate) fn run_monitor_refresh(
	storage: StorageLayout,
	_package_id: PackageId,
	handle: PackageHandle,
	spec: PackageSpec,
) -> Result<MonitorExecution, AppError> {
	match handle {
		PackageHandle::Rust(package) => {
			resolve_git_head::<RustBackend>(&storage, &spec, &package.source, &package.name)
		}
		PackageHandle::TypeScript(package) if typescript_package_uses_repository(&package) => {
			resolve_git_head::<TypeScriptBackend>(&storage, &spec, &package.source, &package.name)
		}
		// Registry-backed TypeScript has no upstream branch to monitor.
		PackageHandle::TypeScript(_) => {
			Ok(MonitorExecution { latest_remote_commit: None, remote_update_available: false })
		}
	}
}

pub(crate) fn compute_remote_update_available(
	latest_remote_commit: Option<&String>,
	tracked_version_commit: Option<&String>,
) -> bool {
	match (latest_remote_commit, tracked_version_commit) {
		(Some(latest), Some(tracked)) => latest != tracked,
		(Some(_), None) => true,
		(None, _) => false,
	}
}
