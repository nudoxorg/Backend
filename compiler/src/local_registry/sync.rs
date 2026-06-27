//! The sync and monitor engine: check out the right commit for a tracked
//! package version, run the ingest pipeline, and (for monitoring) detect whether
//! the remote has advanced past the tracked commit.

use std::sync::Arc;

use tokio::task::spawn_blocking;

use crate::{config::PipelineConfig, core::ts::TsPackage, error::{AppError, PackageError, RegistryLookupError}, git, ingest::{run_rust_pipeline, run_typescript_pipeline}, storage::StorageLayout, sync_progress::{PackageSyncPhase, ProgressReporter}, text_index::SymbolTextIndex};

use super::{MonitorExecution, PackageHandle, PackageId, PackageSpec, SyncExecution, ts_entry_point::{resolve_typescript_repository_entry_point, typescript_package_uses_repository, typescript_repository_entry_hint}};

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_sync(
	storage: StorageLayout,
	pipeline: PipelineConfig,
	package_id: PackageId,
	handle: PackageHandle,
	spec: PackageSpec,
	progress: ProgressReporter,
	nudox_store: Option<Arc<nudox_store::NudoxStore>>,
	text_index: Option<Arc<SymbolTextIndex>>,
	orchestrator: Option<Arc<nudox_orchestrator::Orchestrator>>,
) -> Result<SyncExecution, AppError> {
	match handle {
		PackageHandle::Rust(package) => {
			let git_storage = storage.clone();
			let git_spec = spec.clone();
			let git_package = package.clone();
			let git_progress = progress.clone();

			let (target_commit_hex, latest_remote_commit, workspace) = spawn_blocking(move || {
				git_progress.phase_with_detail(
					PackageSyncPhase::Resolving,
					Some(format!("opening repository for {}", git_package.name)),
				);
				let repo_dir = git_storage
					.prepare_repository_dir(
						git_spec.language,
						&git_spec.slug,
						&git_package.source,
					)
					.map_err(|source| AppError::Storage {
						path: git_storage.root().to_path_buf(),
						source,
					})?;
				let repository =
					git::open_or_clone_repository(&repo_dir, &git_package.source)?;
				git_progress.phase_with_detail(
					PackageSyncPhase::Fetching,
					Some(format!("fetching {}", git_spec.branch)),
				);
				git::fetch_remote_updates(&repository, Some("origin"))?;

				let remote_head =
					git::remote_branch_commit(&repository, "origin", &git_spec.branch);
				let latest_remote_commit = remote_head.map(|id| id.to_hex().to_string());
				let target_commit = git::find_commit_for_version(
					&repository,
					&git_spec.version,
					&git_package.name,
					remote_head,
				)
				.ok_or_else(|| PackageError::VersionNotFound(git_spec.version.clone()))?;

				git_progress.phase_with_detail(
					PackageSyncPhase::Materializing,
					Some(format!("materializing {}", target_commit.to_hex())),
				);
				let workspace = git_storage
					.create_workspace()
					.map_err(|source| AppError::Storage {
						path: git_storage.root().to_path_buf(),
						source,
					})?;
				git::materialize_commit(&repository, target_commit, workspace.path())?;

				Ok::<_, AppError>((
					target_commit.to_hex().to_string(),
					latest_remote_commit,
					workspace,
				))
			})
			.await
			.map_err(|source| AppError::TaskJoin { action: "git operations", source })??;

			let summary = run_rust_pipeline(
				&package,
				&spec.version,
				workspace.path(),
				&pipeline,
				&progress,
				nudox_store.as_ref(),
				text_index.as_ref(),
				orchestrator.as_ref(),
			)
			.await?;

			Ok(SyncExecution { tracked_version_commit: target_commit_hex, latest_remote_commit, summary })
		}
		PackageHandle::TypeScript(package) => {
			if typescript_package_uses_repository(&package) {
				run_repository_backed_typescript_sync(
					storage,
					pipeline,
					package_id,
					package,
					spec,
					progress,
					nudox_store,
					text_index,
					orchestrator,
				)
				.await
			} else {
				progress.phase_with_detail(
					PackageSyncPhase::Resolving,
					Some(format!("resolving npm package {}", package.name)),
				);
				let workspace = storage
					.create_workspace()
					.map_err(|source| AppError::Storage {
						path: storage.root().to_path_buf(),
						source,
					})?;
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
					nudox_store.as_ref(),
					text_index.as_ref(),
					orchestrator.as_ref(),
				)
				.await?;
				drop(workspace);
				Ok(SyncExecution {
					tracked_version_commit: format!("npm:{}@{}", package.name, spec.version),
					latest_remote_commit: None,
					summary,
				})
			}
		}
	}
}

pub(crate) fn run_monitor_refresh(
	storage: StorageLayout,
	package_id: PackageId,
	handle: PackageHandle,
	spec: PackageSpec,
) -> Result<MonitorExecution, AppError> {
	match handle {
		PackageHandle::Rust(package) => {
			let repo_dir = storage
				.prepare_repository_dir(spec.language, &spec.slug, &package.source)
				.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
			let repository = git::open_or_clone_repository(&repo_dir, &package.source)?;
			git::fetch_remote_updates(&repository, Some("origin"))?;

			let remote_head = git::remote_branch_commit(&repository, "origin", &spec.branch);
			let latest_remote_commit = remote_head.map(|id| id.to_hex().to_string());
			let tracked_commit =
				git::find_commit_for_version(&repository, &spec.version, &package.name, remote_head)
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
		PackageHandle::TypeScript(package) => {
			if typescript_package_uses_repository(&package) {
				run_repository_backed_typescript_monitor(storage, package_id, package, spec)
			} else {
				Ok(MonitorExecution { latest_remote_commit: None, remote_update_available: false })
			}
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

#[allow(clippy::too_many_arguments)]
async fn run_repository_backed_typescript_sync(
	storage: StorageLayout,
	pipeline: PipelineConfig,
	_package_id: PackageId,
	package: TsPackage,
	spec: PackageSpec,
	progress: ProgressReporter,
	nudox_store: Option<Arc<nudox_store::NudoxStore>>,
	text_index: Option<Arc<SymbolTextIndex>>,
	orchestrator: Option<Arc<nudox_orchestrator::Orchestrator>>,
) -> Result<SyncExecution, AppError> {
	let git_storage = storage.clone();
	let git_spec = spec.clone();
	let git_package = package.clone();
	let git_progress = progress.clone();

	let (target_commit_hex, latest_remote_commit, workspace, entry_point_str) =
		spawn_blocking(move || {
			git_progress.phase_with_detail(
				PackageSyncPhase::Resolving,
				Some(format!("opening repository for {}", git_package.name)),
			);
			let repo_dir = git_storage
				.prepare_repository_dir(git_spec.language, &git_spec.slug, &git_package.source)
				.map_err(|source| AppError::Storage {
					path: git_storage.root().to_path_buf(),
					source,
				})?;
			let repository = git::open_or_clone_repository(&repo_dir, &git_package.source)?;
			git_progress.phase_with_detail(
				PackageSyncPhase::Fetching,
				Some(format!("fetching {}", git_spec.branch)),
			);
			git::fetch_remote_updates(&repository, Some("origin"))?;

			let remote_head =
				git::remote_branch_commit(&repository, "origin", &git_spec.branch);
			let latest_remote_commit = remote_head.map(|id| id.to_hex().to_string());
			let target_commit = git::find_typescript_commit_for_version(
				&repository,
				&git_spec.version,
				&git_package.name,
				remote_head,
			)
			.ok_or_else(|| PackageError::VersionNotFound(git_spec.version.clone()))?;

			git_progress.phase_with_detail(
				PackageSyncPhase::Materializing,
				Some(format!("materializing {}", target_commit.to_hex())),
			);
			let workspace = git_storage
				.create_workspace()
				.map_err(|source| AppError::Storage {
					path: git_storage.root().to_path_buf(),
					source,
				})?;
			git::materialize_commit(&repository, target_commit, workspace.path())?;

			let entry_point = resolve_typescript_repository_entry_point(
				workspace.path(),
				typescript_repository_entry_hint(&git_package),
			)?;

			Ok::<_, AppError>((
				target_commit.to_hex().to_string(),
				latest_remote_commit,
				workspace,
				entry_point.display().to_string(),
			))
		})
		.await
		.map_err(|source| AppError::TaskJoin { action: "git operations", source })??;

	let mut materialized_package = package.clone();
	materialized_package.entry_point = entry_point_str;

	progress
		.phase_with_detail(PackageSyncPhase::GeneratingIr, Some("generating TypeScript IR".to_owned()));
	let summary = run_typescript_pipeline(
		&materialized_package,
		&spec.version,
		&pipeline,
		&progress,
		nudox_store.as_ref(),
		text_index.as_ref(),
		orchestrator.as_ref(),
	)
	.await?;
	drop(workspace);

	Ok(SyncExecution { tracked_version_commit: target_commit_hex, latest_remote_commit, summary })
}

fn run_repository_backed_typescript_monitor(
	storage: StorageLayout,
	_package_id: PackageId,
	package: TsPackage,
	spec: PackageSpec,
) -> Result<MonitorExecution, AppError> {
	let repo_dir = storage
		.prepare_repository_dir(spec.language, &spec.slug, &package.source)
		.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
	let repository = git::open_or_clone_repository(&repo_dir, &package.source)?;
	git::fetch_remote_updates(&repository, Some("origin"))?;

	let remote_head = git::remote_branch_commit(&repository, "origin", &spec.branch);
	let latest_remote_commit = remote_head.map(|id| id.to_hex().to_string());
	let tracked_commit =
		git::find_typescript_commit_for_version(&repository, &spec.version, &package.name, remote_head)
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
