use std::{collections::HashMap, num::NonZeroU64, sync::{Arc, atomic::{AtomicU64, Ordering}}};

use jiff::Timestamp;
use lang_types::Language;
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::{sync::{Mutex, RwLock}, task::spawn_blocking, time::{Duration, MissedTickBehavior}};
use tracing::{info, instrument, warn};
use url::Url;

use crate::{config::PipelineConfig, core::{rust::RustPackage, ts::TsPackage}, error::{AppError, PackageError}, git, ingest::{IngestionSummary, run_rust_pipeline, run_typescript_pipeline}, storage::StorageLayout, traits::{builder::get_registry, registry::Registry}};

#[derive(Debug, Clone, Deserialize)]
pub struct NewPackageRequest {
	pub language:    Language,
	pub name:        String,
	pub version:     Version,
	#[serde(default)]
	pub branch:      Option<String>,
	#[serde(default = "default_sync_on_add")]
	pub sync_on_add: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageSnapshot {
	pub id:       u64,
	pub language: Language,
	pub name:     String,
	pub slug:     String,
	pub source:   Url,
	pub version:  Version,
	pub branch:   String,
	pub state:    PackageStateSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageStateSnapshot {
	pub health:                  PackageHealth,
	pub last_checked_at:         Option<Timestamp>,
	pub last_synced_at:          Option<Timestamp>,
	pub tracked_version_commit:  Option<String>,
	pub latest_remote_commit:    Option<String>,
	pub remote_update_available: bool,
	pub entry_count:             usize,
	pub document_count:          usize,
	pub vector_count:            usize,
	pub last_error:              Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageHealth {
	Pending,
	Healthy,
	Degraded,
}

pub struct LocalRegistry {
	storage:          StorageLayout,
	monitor_interval: Duration,
	pipeline:         PipelineConfig,
	next_id:          AtomicU64,
	packages:         Arc<RwLock<HashMap<PackageId, Arc<TrackedPackage>>>>,
	keys:             Arc<RwLock<HashMap<PackageKey, PackageId>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PackageId(NonZeroU64);

impl PackageId {
	fn get(self) -> u64 { self.0.get() }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PackageKey {
	language: Language,
	name:     String,
	version:  Version,
	branch:   String,
}

#[derive(Debug, Clone)]
struct PackageSpec {
	language: Language,
	name:     String,
	slug:     String,
	version:  Version,
	branch:   String,
	source:   Url,
}

#[derive(Debug, Clone)]
enum PackageHandle {
	Rust(RustPackage),
	TypeScript(TsPackage),
}

struct TrackedPackage {
	id:        PackageId,
	spec:      PackageSpec,
	handle:    PackageHandle,
	state:     RwLock<TrackedPackageState>,
	sync_lock: Mutex<()>,
}

#[derive(Debug, Clone)]
struct TrackedPackageState {
	health:                  PackageHealth,
	last_checked_at:         Option<Timestamp>,
	last_synced_at:          Option<Timestamp>,
	tracked_version_commit:  Option<String>,
	latest_remote_commit:    Option<String>,
	remote_update_available: bool,
	entry_count:             usize,
	document_count:          usize,
	vector_count:            usize,
	last_error:              Option<String>,
}

#[derive(Debug, Clone)]
struct SyncExecution {
	tracked_version_commit: String,
	latest_remote_commit:   Option<String>,
	summary:                IngestionSummary,
}

#[derive(Debug, Clone)]
struct MonitorExecution {
	latest_remote_commit:    Option<String>,
	remote_update_available: bool,
}

fn default_sync_on_add() -> bool { true }

impl LocalRegistry {
	pub fn new(storage: StorageLayout, monitor_interval: Duration, pipeline: PipelineConfig) -> Self {
		Self {
			storage,
			monitor_interval,
			pipeline,
			next_id: AtomicU64::new(1),
			packages: Arc::new(RwLock::new(HashMap::new())),
			keys: Arc::new(RwLock::new(HashMap::new())),
		}
	}

	pub async fn package_count(&self) -> usize { self.packages.read().await.len() }

	pub async fn list_packages(&self) -> Vec<PackageSnapshot> {
		let packages: Vec<Arc<TrackedPackage>> = self.packages.read().await.values().cloned().collect();
		let mut snapshots = Vec::with_capacity(packages.len());
		for package in packages {
			snapshots.push(package.snapshot().await);
		}
		snapshots.sort_by(|left, right| left.id.cmp(&right.id));
		snapshots
	}

	pub async fn get_package(&self, id: u64) -> Result<PackageSnapshot, AppError> {
		let tracked = self.get_tracked(PackageId::try_from(id)?).await?;
		Ok(tracked.snapshot().await)
	}

	#[instrument(skip_all, fields(language = ?request.language, package = %request.name, version = %request.version))]
	pub async fn add_package(&self, request: NewPackageRequest) -> Result<PackageSnapshot, AppError> {
		let branch = request
			.branch
			.clone()
			.unwrap_or_else(|| default_tracking_branch(request.language).to_owned());
		let key = PackageKey {
			language: request.language,
			name:     request.name.clone(),
			version:  request.version.clone(),
			branch:   branch.clone(),
		};

		if let Some(existing_id) = self.keys.read().await.get(&key).copied() {
			let existing = self.get_tracked(existing_id).await?;
			if request.sync_on_add {
				return self.sync_existing(existing).await;
			}
			return Ok(existing.snapshot().await);
		}

		let handle = resolve_package_handle(&request).await?;
		let package_id = self.allocate_id()?;
		let spec = PackageSpec {
			language: request.language,
			name: handle.name().to_owned(),
			slug: handle.slug().to_owned(),
			version: request.version.clone(),
			branch,
			source: handle.source().clone(),
		};

		let tracked = Arc::new(TrackedPackage::new(package_id, spec, handle));
		self.packages.write().await.insert(package_id, Arc::clone(&tracked));
		self.keys.write().await.insert(key, package_id);

		if request.sync_on_add {
			self.sync_existing(tracked).await
		} else {
			Ok(tracked.snapshot().await)
		}
	}

	pub async fn sync_package(&self, id: u64) -> Result<PackageSnapshot, AppError> {
		let tracked = self.get_tracked(PackageId::try_from(id)?).await?;
		self.sync_existing(tracked).await
	}

	pub async fn run_monitor(self: Arc<Self>) {
		let mut interval = tokio::time::interval(self.monitor_interval);
		interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

		loop {
			interval.tick().await;
			let packages: Vec<Arc<TrackedPackage>> =
				self.packages.read().await.values().cloned().collect();

			for package in packages {
				if let Err(error) = self.refresh_existing(package).await {
					warn!(error = %error, "package monitor iteration failed");
				}
			}
		}
	}

	async fn sync_existing(&self, tracked: Arc<TrackedPackage>) -> Result<PackageSnapshot, AppError> {
		let _guard = tracked.sync_lock.lock().await;
		let blocking_storage = self.storage.clone();
		let blocking_pipeline = self.pipeline.clone();
		let blocking_handle = tracked.handle.clone();
		let blocking_spec = tracked.spec.clone();
		let blocking_id = tracked.id;
		let runtime = tokio::runtime::Handle::current();

		let result = spawn_blocking(move || {
			run_sync(
				blocking_storage,
				blocking_pipeline,
				blocking_id,
				blocking_handle,
				blocking_spec,
				runtime,
			)
		})
		.await
		.map_err(|source| AppError::TaskJoin { action: "sync package", source })?;

		match result {
			Ok(execution) => {
				tracked.record_sync_success(execution).await;
				info!(package = %tracked.spec.name, version = %tracked.spec.version, "package sync complete");
			}
			Err(error) => {
				tracked.record_failure(error.to_string()).await;
				return Err(error);
			}
		}

		Ok(tracked.snapshot().await)
	}

	async fn refresh_existing(&self, tracked: Arc<TrackedPackage>) -> Result<(), AppError> {
		let _guard = tracked.sync_lock.lock().await;
		let blocking_storage = self.storage.clone();
		let blocking_handle = tracked.handle.clone();
		let blocking_spec = tracked.spec.clone();
		let blocking_id = tracked.id;

		let result = spawn_blocking(move || {
			run_monitor_refresh(blocking_storage, blocking_id, blocking_handle, blocking_spec)
		})
		.await
		.map_err(|source| AppError::TaskJoin { action: "monitor package", source })?;

		match result {
			Ok(execution) => tracked.record_monitor_success(execution).await,
			Err(error) => tracked.record_failure(error.to_string()).await,
		}

		Ok(())
	}

	fn allocate_id(&self) -> Result<PackageId, AppError> {
		let next = self.next_id.fetch_add(1, Ordering::Relaxed);
		let next = NonZeroU64::new(next)
			.ok_or_else(|| AppError::Internal { message: "package id counter overflowed".to_owned() })?;
		Ok(PackageId(next))
	}

	async fn get_tracked(&self, id: PackageId) -> Result<Arc<TrackedPackage>, AppError> {
		self.packages.read().await.get(&id).cloned().ok_or(AppError::PackageNotTracked { id: id.get() })
	}
}

impl TrackedPackage {
	fn new(id: PackageId, spec: PackageSpec, handle: PackageHandle) -> Self {
		Self {
			id,
			spec,
			handle,
			state: RwLock::new(TrackedPackageState::new()),
			sync_lock: Mutex::new(()),
		}
	}

	async fn snapshot(&self) -> PackageSnapshot {
		let state = self.state.read().await.clone();
		PackageSnapshot {
			id:       self.id.get(),
			language: self.spec.language,
			name:     self.spec.name.clone(),
			slug:     self.spec.slug.clone(),
			source:   self.spec.source.clone(),
			version:  self.spec.version.clone(),
			branch:   self.spec.branch.clone(),
			state:    PackageStateSnapshot {
				health:                  state.health,
				last_checked_at:         state.last_checked_at,
				last_synced_at:          state.last_synced_at,
				tracked_version_commit:  state.tracked_version_commit,
				latest_remote_commit:    state.latest_remote_commit,
				remote_update_available: state.remote_update_available,
				entry_count:             state.entry_count,
				document_count:          state.document_count,
				vector_count:            state.vector_count,
				last_error:              state.last_error,
			},
		}
	}

	async fn record_sync_success(&self, execution: SyncExecution) {
		let now = Timestamp::now();
		let mut state = self.state.write().await;
		state.health = PackageHealth::Healthy;
		state.last_checked_at = Some(now);
		state.last_synced_at = Some(now);
		state.tracked_version_commit = Some(execution.tracked_version_commit);
		state.latest_remote_commit = execution.latest_remote_commit;
		state.remote_update_available =
			state.latest_remote_commit.as_ref() != state.tracked_version_commit.as_ref();
		state.entry_count = execution.summary.entry_count;
		state.document_count = execution.summary.document_count;
		state.vector_count = execution.summary.vector_count;
		state.last_error = None;
	}

	async fn record_monitor_success(&self, execution: MonitorExecution) {
		let mut state = self.state.write().await;
		state.last_checked_at = Some(Timestamp::now());
		state.latest_remote_commit = execution.latest_remote_commit;
		state.remote_update_available = execution.remote_update_available;
		if !matches!(state.health, PackageHealth::Degraded) {
			state.health = PackageHealth::Healthy;
		}
	}

	async fn record_failure(&self, message: String) {
		let mut state = self.state.write().await;
		state.health = PackageHealth::Degraded;
		state.last_checked_at = Some(Timestamp::now());
		state.last_error = Some(message);
	}
}

impl TrackedPackageState {
	fn new() -> Self {
		Self {
			health:                  PackageHealth::Pending,
			last_checked_at:         None,
			last_synced_at:          None,
			tracked_version_commit:  None,
			latest_remote_commit:    None,
			remote_update_available: false,
			entry_count:             0,
			document_count:          0,
			vector_count:            0,
			last_error:              None,
		}
	}
}

impl PackageHandle {
	fn name(&self) -> &str {
		match self {
			Self::Rust(package) => &package.name,
			Self::TypeScript(package) => &package.name,
		}
	}

	fn slug(&self) -> &str {
		match self {
			Self::Rust(package) => &package.slug,
			Self::TypeScript(package) => &package.slug,
		}
	}

	fn source(&self) -> &Url {
		match self {
			Self::Rust(package) => &package.source,
			Self::TypeScript(package) => &package.source,
		}
	}
}

impl TryFrom<u64> for PackageId {
	type Error = AppError;

	fn try_from(value: u64) -> Result<Self, Self::Error> {
		let id = NonZeroU64::new(value).ok_or(AppError::PackageNotTracked { id: value })?;
		Ok(Self(id))
	}
}

async fn resolve_package_handle(request: &NewPackageRequest) -> Result<PackageHandle, AppError> {
	match request.language {
		Language::Rust => {
			let registry = get_registry(Language::Rust);
			let packages = registry.get_packages_by_name(&request.name).await.map_err(|source| {
				AppError::RegistryLookup {
					language: request.language,
					package:  request.name.clone(),
					message:  source.to_string(),
				}
			})?;

			let package = packages.into_iter().next().ok_or_else(|| AppError::RegistryLookup {
				language: request.language,
				package:  request.name.clone(),
				message:  "package not found".to_owned(),
			})?;
			Ok(PackageHandle::Rust(package))
		}
		Language::TypeScript => Ok(PackageHandle::TypeScript(TsPackage {
			slug:        request.name.to_ascii_lowercase().replace('/', "__"),
			name:        request.name.clone(),
			uuid:        0,
			source:      Url::parse("https://www.npmjs.com")
				.unwrap()
				.join(&format!("package/{}", request.name))
				.unwrap_or_else(|_| Url::parse("https://www.npmjs.com").unwrap()),
			description: None,
			entry_point: format!("npm:{}@{}", request.name, request.version),
		})),
		other => Err(AppError::UnsupportedLanguage { language: other }),
	}
}

fn run_sync(
	storage: StorageLayout,
	pipeline: PipelineConfig,
	package_id: PackageId,
	handle: PackageHandle,
	spec: PackageSpec,
	runtime: tokio::runtime::Handle,
) -> Result<SyncExecution, AppError> {
	match handle {
		PackageHandle::Rust(package) => {
			let repo_dir = storage.repository_dir(package_id.get());
			let repository = git::open_or_clone_repository(&repo_dir, &package.source)?;
			git::fetch_remote_updates(&repository, Some("origin"))?;

			let latest_remote_commit = git::remote_branch_commit(&repository, "origin", &spec.branch)
				.map(|object_id| object_id.to_hex().to_string());
			let target_commit = git::find_commit_for_version(&repository, &spec.version, &package.name)
				.ok_or_else(|| PackageError::VersionNotFound(spec.version.clone()))?;

			let workspace = storage
				.create_workspace()
				.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
			git::materialize_commit(&repository, target_commit, workspace.path())?;
			let summary =
				run_rust_pipeline(&package, &spec.version, workspace.path(), &pipeline, &runtime)?;

			Ok(SyncExecution {
				tracked_version_commit: target_commit.to_hex().to_string(),
				latest_remote_commit,
				summary,
			})
		}
		PackageHandle::TypeScript(package) => {
			let summary = run_typescript_pipeline(&package, &spec.version, &pipeline, &runtime)?;
			Ok(SyncExecution {
				tracked_version_commit: format!("npm:{}@{}", package.name, spec.version),
				latest_remote_commit: None,
				summary,
			})
		}
	}
}

fn run_monitor_refresh(
	storage: StorageLayout,
	package_id: PackageId,
	handle: PackageHandle,
	spec: PackageSpec,
) -> Result<MonitorExecution, AppError> {
	match handle {
		PackageHandle::Rust(package) => {
			let repo_dir = storage.repository_dir(package_id.get());
			let repository = git::open_or_clone_repository(&repo_dir, &package.source)?;
			git::fetch_remote_updates(&repository, Some("origin"))?;

			let latest_remote_commit = git::remote_branch_commit(&repository, "origin", &spec.branch)
				.map(|object_id| object_id.to_hex().to_string());
			let tracked_commit = git::find_commit_for_version(&repository, &spec.version, &package.name)
				.ok_or_else(|| PackageError::VersionNotFound(spec.version.clone()))?
				.to_hex()
				.to_string();

			Ok(MonitorExecution {
				remote_update_available: latest_remote_commit.as_deref() != Some(tracked_commit.as_str()),
				latest_remote_commit,
			})
		}
		PackageHandle::TypeScript(_) => {
			Ok(MonitorExecution { latest_remote_commit: None, remote_update_available: false })
		}
	}
}

fn default_tracking_branch(language: Language) -> &'static str {
	match language {
		Language::Rust | Language::TypeScript => "main",
		_ => "main",
	}
}
