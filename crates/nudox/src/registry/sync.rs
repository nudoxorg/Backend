use tempfile::TempDir;
use tokio::task::spawn_blocking;
use url::Url;

use crate::{
	config::PipelineConfig,
	core::{backend::{LanguageBackend, RustBackend, TypeScriptBackend}, ts::TsPackage},
	git,
	http::error::{AppError, PackageError, RegistryLookupError},
	ingest::{IngestTargets, run_pipeline},
	storage::StorageLayout,
	sync_progress::{PackageSyncPhase, ProgressReporter},
};

use super::{MonitorExecution, PackageHandle, PackageId, PackageSpec, SyncExecution};
use crate::core::ts::entry_point::typescript_package_uses_repository;

struct GitCheckout {
	commit_hex:           String,
	latest_remote_commit: Option<String>,
	workspace:            TempDir,
}

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

	Ok(GitCheckout {
		commit_hex: target_commit.to_hex().to_string(),
		latest_remote_commit,
		workspace,
	})
}

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

async fn spawn_git_checkout<B: LanguageBackend>(
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
			run_git_backed_sync::<RustBackend>(storage, pipeline, spec, package, progress, targets)
				.await
		}
		PackageHandle::TypeScript(package) if typescript_package_uses_repository(&package) => {
			run_git_backed_sync::<TypeScriptBackend>(
				storage, pipeline, spec, package, progress, targets,
			)
			.await
		}
		PackageHandle::TypeScript(package) => {
			run_npm_backed_sync(storage, pipeline, spec, package, progress, targets).await
		}
	}
}

async fn run_git_backed_sync<B: LanguageBackend>(
	storage: StorageLayout,
	pipeline: PipelineConfig,
	spec: PackageSpec,
	package: B::Package,
	progress: ProgressReporter,
	targets: &IngestTargets,
) -> Result<SyncExecution, AppError> {
	let checkout = spawn_git_checkout::<B>(
		storage,
		spec.clone(),
		B::package_source(&package).clone(),
		B::package_name(&package).to_owned(),
		progress.clone(),
	)
	.await?;

	let package = B::prepare_workspace(package, checkout.workspace.path())?;
	let summary = run_pipeline::<B>(
		&package,
		&spec.version,
		checkout.workspace.path(),
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

async fn run_npm_backed_sync(
	storage: StorageLayout,
	pipeline: PipelineConfig,
	spec: PackageSpec,
	package: TsPackage,
	progress: ProgressReporter,
	targets: &IngestTargets,
) -> Result<SyncExecution, AppError> {
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

	let summary = run_pipeline::<TypeScriptBackend>(
		&materialized_package,
		&spec.version,
		workspace.path(),
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
