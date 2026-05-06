use std::{collections::HashMap, fs, num::NonZeroU64, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicU64, Ordering}}};

use jiff::Timestamp;
use lang_types::Language;
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::{sync::{Mutex, RwLock}, task::spawn_blocking, time::{Duration, MissedTickBehavior}};
use tracing::{error, info, instrument, warn};
use url::Url;

use crate::{config::PipelineConfig, core::{rust::RustPackage, ts::TsPackage}, error::{AppError, PackageError}, git, ingest::{IngestionSummary, run_rust_pipeline, run_typescript_pipeline}, storage::StorageLayout, sync_progress::{PackageSyncPhase, PackageSyncStatus, ProgressReporter}, traits::{builder::get_registry, registry::Registry}};

fn deserialize_lenient_version<'de, D: Deserializer<'de>>(d: D) -> Result<Version, D::Error> {
	let s = String::deserialize(d)?;
	if let Ok(v) = Version::parse(&s) {
		return Ok(v);
	}
	let padded = match s.matches('.').count() {
		0 => format!("{s}.0.0"),
		1 => format!("{s}.0"),
		_ => s.clone(),
	};
	Version::parse(&padded)
		.map_err(|e| serde::de::Error::custom(format!("invalid version `{s}`: {e}")))
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewPackageRequest {
	pub language:    Language,
	pub name:        String,
	#[serde(deserialize_with = "deserialize_lenient_version")]
	pub version:     Version,
	#[serde(default)]
	pub source:      Option<String>,
	#[serde(default)]
	pub entry_point: Option<String>,
	#[serde(default)]
	pub branch:      Option<String>,
}

#[derive(Debug, Clone)]
pub enum AddPackageOutcome {
	Created(PackageSnapshot),
	Existing(PackageSnapshot),
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
	pub sync_status:             PackageSyncStatus,
	pub sync_phase:              Option<PackageSyncPhase>,
	pub sync_detail:             Option<String>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRegistry {
	packages: Vec<PersistedPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPackage {
	id:     u64,
	spec:   PersistedPackageSpec,
	handle: PersistedPackageHandle,
	state:  TrackedPackageState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPackageSpec {
	language: Language,
	name:     String,
	slug:     String,
	version:  Version,
	branch:   String,
	source:   Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PersistedPackageHandle {
	Rust {
		name:        String,
		slug:        String,
		source:      Url,
		description: Option<String>,
	},
	TypeScript {
		name:        String,
		slug:        String,
		uuid:        u64,
		source:      Url,
		description: Option<String>,
		entry_point: String,
	},
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrackedPackageState {
	health:                  PackageHealth,
	#[serde(default)]
	sync_status:             PackageSyncStatus,
	#[serde(default)]
	sync_phase:              Option<PackageSyncPhase>,
	#[serde(default)]
	sync_detail:             Option<String>,
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

const TYPESCRIPT_REPOSITORY_ENTRY_PREFIX: &str = "repo:";

impl LocalRegistry {
	pub fn new(storage: StorageLayout, monitor_interval: Duration, pipeline: PipelineConfig) -> Self {
		let persisted = load_persisted_registry(&storage);
		let next_id = persisted
			.as_ref()
			.map(|registry| registry.packages.iter().map(|package| package.id).max().unwrap_or(0) + 1)
			.unwrap_or(1);
		let (packages, keys) =
			persisted.map(rehydrate_registry).unwrap_or_else(|| (HashMap::new(), HashMap::new()));

		Self {
			storage,
			monitor_interval,
			pipeline,
			next_id: AtomicU64::new(next_id),
			packages: Arc::new(RwLock::new(packages)),
			keys: Arc::new(RwLock::new(keys)),
		}
	}

	pub async fn package_count(&self) -> usize { self.packages.read().await.len() }

	pub async fn list_packages(&self) -> Vec<PackageSnapshot> {
		let packages: Vec<Arc<TrackedPackage>> = self.packages.read().await.values().cloned().collect();
		let mut snapshots = Vec::with_capacity(packages.len());
		for package in packages {
			snapshots.push(package.snapshot().await);
		}
		snapshots.sort_by_key(|left| left.id);
		snapshots
	}

	pub async fn get_package(&self, id: u64) -> Result<PackageSnapshot, AppError> {
		let tracked = self.get_tracked(PackageId::try_from(id)?).await?;
		Ok(tracked.snapshot().await)
	}

	#[instrument(skip_all, fields(language = ?request.language, package = %request.name, version = %request.version))]
	pub async fn add_package(
		self: &Arc<Self>,
		request: NewPackageRequest,
	) -> Result<AddPackageOutcome, AppError> {
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
			return Ok(AddPackageOutcome::Existing(existing.snapshot().await));
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
		self.persist().await?;

		Ok(AddPackageOutcome::Created(self.enqueue_sync(tracked).await?))
	}

	pub async fn sync_package(self: &Arc<Self>, id: u64) -> Result<PackageSnapshot, AppError> {
		let tracked = self.get_tracked(PackageId::try_from(id)?).await?;
		self.enqueue_sync(tracked).await
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

	async fn enqueue_sync(
		self: &Arc<Self>,
		tracked: Arc<TrackedPackage>,
	) -> Result<PackageSnapshot, AppError> {
		if tracked.is_sync_active().await {
			return Ok(tracked.snapshot().await);
		}

		tracked.mark_sync_queued().await;
		self.persist().await?;
		let snapshot = tracked.snapshot().await;
		self.spawn_sync_task(tracked);
		Ok(snapshot)
	}

	fn spawn_sync_task(self: &Arc<Self>, tracked: Arc<TrackedPackage>) {
		let registry = Arc::clone(self);
		tokio::spawn(async move {
			if let Err(error) = registry.sync_existing(tracked).await {
				error!(error = %error, "background package sync failed");
			}
		});
	}

	async fn sync_existing(
		self: &Arc<Self>,
		tracked: Arc<TrackedPackage>,
	) -> Result<PackageSnapshot, AppError> {
		let _guard = tracked.sync_lock.lock().await;
		let blocking_storage = self.storage.clone();
		let blocking_pipeline = self.pipeline.clone();
		let blocking_handle = tracked.handle.clone();
		let blocking_spec = tracked.spec.clone();
		let blocking_id = tracked.id;
		let runtime = tokio::runtime::Handle::current();
		let progress_runtime = runtime.clone();
		let progress = {
			let registry = Arc::clone(self);
			let tracked = Arc::clone(&tracked);
			let package = tracked.spec.name.clone();
			let version = tracked.spec.version.clone();
			ProgressReporter::new(move |phase, detail| {
				let registry = Arc::clone(&registry);
				let tracked = Arc::clone(&tracked);
				let detail_for_log = detail.clone();
				let package = package.clone();
				let version = version.clone();
				let runtime = progress_runtime.clone();
				runtime.spawn(async move {
					info!(
						package = %package,
						version = %version,
						phase = ?phase,
						detail = detail_for_log.as_deref().unwrap_or(""),
						"package sync progress"
					);
					tracked.update_progress(phase, detail).await;
					if let Err(error) = registry.persist().await {
						warn!(error = %error, "failed to persist package progress");
					}
				});
			})
		};
		tracked.mark_sync_running(PackageSyncPhase::Resolving, Some("starting sync".to_owned())).await;
		self.persist().await?;

		let result = spawn_blocking(move || {
			run_sync(
				blocking_storage,
				blocking_pipeline,
				blocking_id,
				blocking_handle,
				blocking_spec,
				runtime,
				progress,
			)
		})
		.await
		.map_err(|source| AppError::TaskJoin { action: "sync package", source })?;

		match result {
			Ok(execution) => {
				tracked.record_sync_success(execution).await;
				self.persist().await?;
				info!(package = %tracked.spec.name, version = %tracked.spec.version, "package sync complete");
			}
			Err(error) => {
				tracked.record_failure(error.to_string()).await;
				self.persist().await?;
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
		self.persist().await?;

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

	async fn persist(&self) -> Result<(), AppError> {
		let packages: Vec<Arc<TrackedPackage>> = self.packages.read().await.values().cloned().collect();
		let mut persisted = Vec::with_capacity(packages.len());
		for package in packages {
			persisted.push(package.persisted().await);
		}
		persisted.sort_by_key(|package| package.id);

		let payload = PersistedRegistry { packages: persisted };
		let bytes = serde_json::to_vec_pretty(&payload)?;
		fs::write(self.storage.packages_file(), bytes).map_err(|source| AppError::Storage {
			path: self.storage.packages_file().to_path_buf(),
			source,
		})?;
		Ok(())
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

	fn rehydrated(
		id: PackageId,
		spec: PackageSpec,
		handle: PackageHandle,
		state: TrackedPackageState,
	) -> Self {
		Self { id, spec, handle, state: RwLock::new(state), sync_lock: Mutex::new(()) }
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
				sync_status:             state.sync_status,
				sync_phase:              state.sync_phase,
				sync_detail:             state.sync_detail,
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

	async fn persisted(&self) -> PersistedPackage {
		PersistedPackage {
			id:     self.id.get(),
			spec:   PersistedPackageSpec::from(&self.spec),
			handle: PersistedPackageHandle::from(&self.handle),
			state:  self.state.read().await.clone(),
		}
	}

	async fn record_sync_success(&self, execution: SyncExecution) {
		let now = Timestamp::now();
		let mut state = self.state.write().await;
		state.health = PackageHealth::Healthy;
		state.sync_status = PackageSyncStatus::Idle;
		state.sync_phase = None;
		state.sync_detail = None;
		state.last_checked_at = Some(now);
		state.last_synced_at = Some(now);
		state.tracked_version_commit = Some(execution.tracked_version_commit);
		state.latest_remote_commit = execution.latest_remote_commit;
		state.remote_update_available = compute_remote_update_available(
			state.latest_remote_commit.as_ref(),
			state.tracked_version_commit.as_ref(),
		);
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
		state.sync_status = PackageSyncStatus::Idle;
		state.sync_phase = None;
		state.sync_detail = None;
		state.last_checked_at = Some(Timestamp::now());
		state.last_error = Some(message);
	}

	async fn is_sync_active(&self) -> bool {
		matches!(
			self.state.read().await.sync_status,
			PackageSyncStatus::Queued | PackageSyncStatus::Running
		)
	}

	async fn mark_sync_queued(&self) {
		let mut state = self.state.write().await;
		state.sync_status = PackageSyncStatus::Queued;
		state.sync_phase = Some(PackageSyncPhase::Resolving);
		state.sync_detail = Some("queued".to_owned());
	}

	async fn mark_sync_running(&self, phase: PackageSyncPhase, detail: Option<String>) {
		let mut state = self.state.write().await;
		state.sync_status = PackageSyncStatus::Running;
		state.sync_phase = Some(phase);
		state.sync_detail = detail;
	}

	async fn update_progress(&self, phase: PackageSyncPhase, detail: Option<String>) {
		let mut state = self.state.write().await;
		state.sync_status = PackageSyncStatus::Running;
		state.sync_phase = Some(phase);
		state.sync_detail = detail;
	}
}

impl TrackedPackageState {
	fn new() -> Self {
		Self {
			health:                  PackageHealth::Pending,
			sync_status:             PackageSyncStatus::Idle,
			sync_phase:              None,
			sync_detail:             None,
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

	fn normalize_rehydrated(mut self) -> Self {
		if !matches!(self.sync_status, PackageSyncStatus::Idle) {
			self.sync_status = PackageSyncStatus::Idle;
			self.sync_phase = None;
			self.sync_detail = None;
		}
		self
	}
}

impl From<&PackageSpec> for PersistedPackageSpec {
	fn from(value: &PackageSpec) -> Self {
		Self {
			language: value.language,
			name:     value.name.clone(),
			slug:     value.slug.clone(),
			version:  value.version.clone(),
			branch:   value.branch.clone(),
			source:   value.source.clone(),
		}
	}
}

impl From<&PersistedPackageSpec> for PackageSpec {
	fn from(value: &PersistedPackageSpec) -> Self {
		Self {
			language: value.language,
			name:     value.name.clone(),
			slug:     value.slug.clone(),
			version:  value.version.clone(),
			branch:   value.branch.clone(),
			source:   value.source.clone(),
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

impl From<&PackageHandle> for PersistedPackageHandle {
	fn from(value: &PackageHandle) -> Self {
		match value {
			PackageHandle::Rust(package) => Self::Rust {
				name:        package.name.clone(),
				slug:        package.slug.clone(),
				source:      package.source.clone(),
				description: package.description.clone(),
			},
			PackageHandle::TypeScript(package) => Self::TypeScript {
				name:        package.name.clone(),
				slug:        package.slug.clone(),
				uuid:        package.uuid,
				source:      package.source.clone(),
				description: package.description.clone(),
				entry_point: package.entry_point.clone(),
			},
		}
	}
}

impl From<PersistedPackageHandle> for PackageHandle {
	fn from(value: PersistedPackageHandle) -> Self {
		match value {
			PersistedPackageHandle::Rust { name, slug, source, description } => {
				PackageHandle::Rust(RustPackage {
					slug,
					name,
					language: Language::Rust,
					uuid: 0,
					source,
					description,
				})
			}
			PersistedPackageHandle::TypeScript { name, slug, uuid, source, description, entry_point } => {
				PackageHandle::TypeScript(TsPackage { slug, name, uuid, source, description, entry_point })
			}
		}
	}
}

fn load_persisted_registry(storage: &StorageLayout) -> Option<PersistedRegistry> {
	let path = storage.packages_file();
	if !path.is_file() {
		return None;
	}

	let bytes = match fs::read(path) {
		Ok(bytes) => bytes,
		Err(error) => {
			warn!(path = %path.display(), error = %error, "failed to read persisted package registry");
			return None;
		}
	};

	match serde_json::from_slice::<PersistedRegistry>(&bytes) {
		Ok(registry) => Some(registry),
		Err(error) => {
			warn!(path = %path.display(), error = %error, "failed to parse persisted package registry");
			None
		}
	}
}

fn rehydrate_registry(
	registry: PersistedRegistry,
) -> (HashMap<PackageId, Arc<TrackedPackage>>, HashMap<PackageKey, PackageId>) {
	let mut packages = HashMap::new();
	let mut keys = HashMap::new();

	for package in registry.packages {
		let Some(id) = NonZeroU64::new(package.id).map(PackageId) else {
			continue;
		};
		let spec = PackageSpec::from(&package.spec);
		let key = PackageKey {
			language: spec.language,
			name:     spec.name.clone(),
			version:  spec.version.clone(),
			branch:   spec.branch.clone(),
		};
		let tracked = Arc::new(TrackedPackage::rehydrated(
			id,
			spec,
			package.handle.into(),
			package.state.normalize_rehydrated(),
		));
		keys.insert(key, id);
		packages.insert(id, tracked);
	}

	(packages, keys)
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
			resolve_rust_package_handle(request, packages)
		}
		Language::TypeScript => resolve_typescript_package_handle(request).await,
		other => Err(AppError::UnsupportedLanguage { language: other }),
	}
}

fn resolve_rust_package_handle(
	request: &NewPackageRequest,
	packages: Vec<RustPackage>,
) -> Result<PackageHandle, AppError> {
	let package = packages.into_iter().next().ok_or_else(|| AppError::RegistryLookup {
		language: request.language,
		package:  request.name.clone(),
		message:  "package not found".to_owned(),
	})?;
	Ok(PackageHandle::Rust(package))
}

async fn resolve_typescript_package_handle(
	request: &NewPackageRequest,
) -> Result<PackageHandle, AppError> {
	if request.source.is_some() {
		return resolve_explicit_typescript_package_handle(request);
	}

	let registry = crate::core::ts::Npm::default();
	let package =
		registry.resolve_package_version(&request.name, &request.version).await.map_err(|source| {
			AppError::RegistryLookup {
				language: request.language,
				package:  request.name.clone(),
				message:  source.to_string(),
			}
		})?;
	Ok(PackageHandle::TypeScript(package))
}

fn resolve_explicit_typescript_package_handle(
	request: &NewPackageRequest,
) -> Result<PackageHandle, AppError> {
	let source = request.source.as_deref().ok_or_else(|| AppError::Internal {
		message: "missing explicit TypeScript source".to_owned(),
	})?;
	let source = parse_explicit_typescript_source(request, source)?;
	let entry_point = format!(
		"{TYPESCRIPT_REPOSITORY_ENTRY_PREFIX}{}",
		request.entry_point.as_deref().unwrap_or_default()
	);

	Ok(PackageHandle::TypeScript(TsPackage {
		slug: typescript_slug(&request.name),
		name: request.name.clone(),
		uuid: 0,
		source,
		description: None,
		entry_point,
	}))
}

fn run_sync(
	storage: StorageLayout,
	pipeline: PipelineConfig,
	package_id: PackageId,
	handle: PackageHandle,
	spec: PackageSpec,
	runtime: tokio::runtime::Handle,
	progress: ProgressReporter,
) -> Result<SyncExecution, AppError> {
	match handle {
		PackageHandle::Rust(package) => {
			progress.phase_with_detail(
				PackageSyncPhase::Resolving,
				Some(format!("opening repository for {}", package.name)),
			);
			let repo_dir = storage.repository_dir(spec.language, &spec.slug, &package.source);
			let repository = git::open_or_clone_repository(&repo_dir, &package.source)?;
			progress
				.phase_with_detail(PackageSyncPhase::Fetching, Some(format!("fetching {}", spec.branch)));
			git::fetch_remote_updates(&repository, Some("origin"))?;

			let remote_head = git::remote_branch_commit(&repository, "origin", &spec.branch);
			let latest_remote_commit = remote_head.map(|id| id.to_hex().to_string());
			let target_commit =
				git::find_commit_for_version(&repository, &spec.version, &package.name, remote_head)
					.ok_or_else(|| PackageError::VersionNotFound(spec.version.clone()))?;

			progress.phase_with_detail(
				PackageSyncPhase::Materializing,
				Some(format!("materializing {}", target_commit.to_hex())),
			);
			let workspace = storage
				.create_workspace()
				.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
			git::materialize_commit(&repository, target_commit, workspace.path())?;
			progress
				.phase_with_detail(PackageSyncPhase::GeneratingIr, Some("generating Rust IR".to_owned()));
			let summary = run_rust_pipeline(
				&package,
				&spec.version,
				workspace.path(),
				&pipeline,
				&runtime,
				Some(&progress),
			)?;

			Ok(SyncExecution {
				tracked_version_commit: target_commit.to_hex().to_string(),
				latest_remote_commit,
				summary,
			})
		}
		PackageHandle::TypeScript(package) => {
			if typescript_package_uses_repository(&package) {
				run_repository_backed_typescript_sync(
					storage, pipeline, package_id, package, spec, runtime, progress,
				)
			} else {
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
				let entry_point = runtime
					.block_on(crate::core::ts::Npm::default().materialize_package_version(
						&package.name,
						&spec.version,
						workspace.path(),
					))
					.map_err(|source| AppError::RegistryLookup {
						language: spec.language,
						package:  package.name.clone(),
						message:  source.to_string(),
					})?;
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
					&runtime,
					Some(&progress),
				)?;
				Ok(SyncExecution {
					tracked_version_commit: format!("npm:{}@{}", package.name, spec.version),
					latest_remote_commit: None,
					summary,
				})
			}
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
			let repo_dir = storage.repository_dir(spec.language, &spec.slug, &package.source);
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

fn default_tracking_branch(language: Language) -> &'static str {
	match language {
		Language::Rust | Language::TypeScript => "main",
		_ => "main",
	}
}

fn compute_remote_update_available(
	latest_remote_commit: Option<&String>,
	tracked_version_commit: Option<&String>,
) -> bool {
	match (latest_remote_commit, tracked_version_commit) {
		(Some(latest), Some(tracked)) => latest != tracked,
		(Some(_), None) => true,
		(None, _) => false,
	}
}

fn run_repository_backed_typescript_sync(
	storage: StorageLayout,
	pipeline: PipelineConfig,
	_package_id: PackageId,
	package: TsPackage,
	spec: PackageSpec,
	runtime: tokio::runtime::Handle,
	progress: ProgressReporter,
) -> Result<SyncExecution, AppError> {
	progress.phase_with_detail(
		PackageSyncPhase::Resolving,
		Some(format!("opening repository for {}", package.name)),
	);
	let repo_dir = storage.repository_dir(spec.language, &spec.slug, &package.source);
	let repository = git::open_or_clone_repository(&repo_dir, &package.source)?;
	progress.phase_with_detail(PackageSyncPhase::Fetching, Some(format!("fetching {}", spec.branch)));
	git::fetch_remote_updates(&repository, Some("origin"))?;

	let remote_head = git::remote_branch_commit(&repository, "origin", &spec.branch);
	let latest_remote_commit = remote_head.map(|id| id.to_hex().to_string());
	let target_commit =
		git::find_typescript_commit_for_version(&repository, &spec.version, &package.name, remote_head)
			.ok_or_else(|| PackageError::VersionNotFound(spec.version.clone()))?;

	progress.phase_with_detail(
		PackageSyncPhase::Materializing,
		Some(format!("materializing {}", target_commit.to_hex())),
	);
	let workspace = storage
		.create_workspace()
		.map_err(|source| AppError::Storage { path: storage.root().to_path_buf(), source })?;
	git::materialize_commit(&repository, target_commit, workspace.path())?;

	let entry_point = resolve_typescript_repository_entry_point(
		workspace.path(),
		typescript_repository_entry_hint(&package),
	)?;
	let mut materialized_package = package.clone();
	materialized_package.entry_point = entry_point.display().to_string();

	progress
		.phase_with_detail(PackageSyncPhase::GeneratingIr, Some("generating TypeScript IR".to_owned()));
	let summary = run_typescript_pipeline(
		&materialized_package,
		&spec.version,
		&pipeline,
		&runtime,
		Some(&progress),
	)?;

	Ok(SyncExecution {
		tracked_version_commit: target_commit.to_hex().to_string(),
		latest_remote_commit,
		summary,
	})
}

fn run_repository_backed_typescript_monitor(
	storage: StorageLayout,
	_package_id: PackageId,
	package: TsPackage,
	spec: PackageSpec,
) -> Result<MonitorExecution, AppError> {
	let repo_dir = storage.repository_dir(spec.language, &spec.slug, &package.source);
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

fn typescript_package_uses_repository(package: &TsPackage) -> bool {
	package.entry_point.starts_with(TYPESCRIPT_REPOSITORY_ENTRY_PREFIX)
}

fn typescript_repository_entry_hint(package: &TsPackage) -> Option<&str> {
	package
		.entry_point
		.strip_prefix(TYPESCRIPT_REPOSITORY_ENTRY_PREFIX)
		.filter(|entry_point| !entry_point.is_empty())
}

fn parse_explicit_typescript_source(
	request: &NewPackageRequest,
	source: &str,
) -> Result<Url, AppError> {
	if let Ok(url) = Url::parse(source) {
		return Ok(url);
	}

	let path = PathBuf::from(source);
	let canonical = path.canonicalize().map_err(|error| AppError::RegistryLookup {
		language: request.language,
		package:  request.name.clone(),
		message:  format!("failed to resolve `{source}` as a local path: {error}"),
	})?;

	Url::from_directory_path(&canonical).or_else(|()| Url::from_file_path(&canonical)).map_err(|()| {
		AppError::RegistryLookup {
			language: request.language,
			package:  request.name.clone(),
			message:  format!("`{}` is not a valid repository path or URL", canonical.display()),
		}
	})
}

fn typescript_slug(name: &str) -> String { name.to_ascii_lowercase().replace('/', "__") }

fn resolve_typescript_repository_entry_point(
	repository_root: &Path,
	entry_hint: Option<&str>,
) -> Result<PathBuf, AppError> {
	if let Some(entry_hint) = entry_hint {
		let candidate = repository_root.join(entry_hint);
		return ensure_typescript_entry_point(candidate);
	}

	if let Some(candidate) = read_typescript_entry_point_from_package_json(repository_root)? {
		return ensure_typescript_entry_point(candidate);
	}

	for candidate in ["mod.ts", "index.ts", "src/mod.ts", "src/index.ts"] {
		let candidate = repository_root.join(candidate);
		if candidate.is_file() {
			return Ok(candidate);
		}
	}

	Err(AppError::Internal {
		message: format!(
			"could not determine a TypeScript entry point in `{}`",
			repository_root.display()
		),
	})
}

fn ensure_typescript_entry_point(candidate: PathBuf) -> Result<PathBuf, AppError> {
	if candidate.is_file() {
		return Ok(candidate);
	}

	Err(AppError::Internal {
		message: format!("TypeScript entry point `{}` does not exist", candidate.display()),
	})
}

fn read_typescript_entry_point_from_package_json(
	repository_root: &Path,
) -> Result<Option<PathBuf>, AppError> {
	let manifest_path = repository_root.join("package.json");
	if !manifest_path.is_file() {
		return Ok(None);
	}

	let content = fs::read_to_string(&manifest_path)
		.map_err(|source| AppError::Storage { path: manifest_path.clone(), source })?;
	let manifest: serde_json::Value = serde_json::from_str(&content)?;

	for key in ["types", "typings", "module", "main"] {
		if let Some(value) = manifest.get(key).and_then(serde_json::Value::as_str) {
			return Ok(Some(repository_root.join(value)));
		}
	}

	if let Some(exports) = manifest.get("exports")
		&& let Some(value) = [
			exports.get(".").and_then(serde_json::Value::as_str),
			exports
				.get(".")
				.and_then(serde_json::Value::as_object)
				.and_then(|entry| entry.get("types"))
				.and_then(serde_json::Value::as_str),
			exports
				.get(".")
				.and_then(serde_json::Value::as_object)
				.and_then(|entry| entry.get("default"))
				.and_then(serde_json::Value::as_str),
			exports.get("types").and_then(serde_json::Value::as_str),
		]
		.into_iter()
		.flatten()
		.next()
	{
		return Ok(Some(repository_root.join(value)));
	}

	Ok(None)
}

#[cfg(test)]
mod tests {
	use std::{num::NonZeroU64, time::Duration};

	use lang_types::Language;
	use semver::Version;
	use tempfile::TempDir;
	use url::Url;

	use super::*;
	use crate::config::PipelineConfig;

	fn request(language: Language, name: &str, version: &str) -> NewPackageRequest {
		NewPackageRequest {
			language,
			name: name.to_owned(),
			version: Version::parse(version).unwrap(),
			source: None,
			entry_point: None,
			branch: None,
		}
	}

	#[tokio::test]
	async fn resolves_typescript_package_handle_snapshot() {
		let handle = resolve_package_handle(&request(Language::TypeScript, "@types/node", "24.0.0"))
			.await
			.unwrap();
		insta::assert_debug_snapshot!(handle);
	}

	#[tokio::test]
	async fn resolves_rust_package_handle_snapshot() {
		let handle =
			resolve_package_handle(&request(Language::Rust, "serde", "1.0.228")).await.unwrap();
		insta::assert_debug_snapshot!(handle);
	}

	#[tokio::test]
	async fn persists_registry_across_restart() {
		let tempdir = TempDir::new().unwrap();
		let storage = StorageLayout::new(tempdir.path());
		storage.ensure().unwrap();

		let registry =
			LocalRegistry::new(storage.clone(), Duration::from_secs(60), test_pipeline_config());
		let package_id = PackageId(NonZeroU64::new(7).unwrap());
		let version = Version::parse("0.1.1").unwrap();
		let tracked = Arc::new(TrackedPackage::new(
			package_id,
			PackageSpec {
				language: Language::Rust,
				name:     "any-tts".to_owned(),
				slug:     "any-tts".to_owned(),
				version:  version.clone(),
				branch:   "main".to_owned(),
				source:   Url::parse("https://github.com/example/any-tts").unwrap(),
			},
			PackageHandle::Rust(RustPackage {
				slug:        "any-tts".to_owned(),
				name:        "any-tts".to_owned(),
				language:    Language::Rust,
				uuid:        0,
				source:      Url::parse("https://github.com/example/any-tts").unwrap(),
				description: Some("fixture".to_owned()),
			}),
		));
		tracked.record_failure("fixture failure".to_owned()).await;

		registry.packages.write().await.insert(package_id, Arc::clone(&tracked));
		registry.keys.write().await.insert(
			PackageKey {
				language: Language::Rust,
				name:     "any-tts".to_owned(),
				version:  version.clone(),
				branch:   "main".to_owned(),
			},
			package_id,
		);
		registry.persist().await.unwrap();

		let rehydrated =
			LocalRegistry::new(storage.clone(), Duration::from_secs(60), test_pipeline_config());
		let packages = rehydrated.list_packages().await;

		assert_eq!(packages.len(), 1);
		assert_eq!(packages[0].id, 7);
		assert_eq!(packages[0].name, "any-tts");
		assert_eq!(packages[0].version, version);
		assert!(matches!(packages[0].state.health, PackageHealth::Degraded));
		assert_eq!(packages[0].state.last_error.as_deref(), Some("fixture failure"));

		let next_id = rehydrated.allocate_id().unwrap();
		assert_eq!(next_id.get(), 8);
	}

	#[tokio::test]
	async fn rehydrate_clears_transient_sync_progress() {
		let tempdir = TempDir::new().unwrap();
		let storage = StorageLayout::new(tempdir.path());
		storage.ensure().unwrap();

		let registry =
			LocalRegistry::new(storage.clone(), Duration::from_secs(60), test_pipeline_config());
		let package_id = PackageId(NonZeroU64::new(9).unwrap());
		let version = Version::parse("5.1.6").unwrap();
		let tracked = Arc::new(TrackedPackage::new(
			package_id,
			PackageSpec {
				language: Language::TypeScript,
				name:     "nanoid".to_owned(),
				slug:     "nanoid".to_owned(),
				version:  version.clone(),
				branch:   "main".to_owned(),
				source:   Url::parse("https://github.com/ai/nanoid.git").unwrap(),
			},
			PackageHandle::TypeScript(TsPackage {
				slug:        "nanoid".to_owned(),
				name:        "nanoid".to_owned(),
				uuid:        9,
				source:      Url::parse("https://github.com/ai/nanoid.git").unwrap(),
				description: Some("fixture".to_owned()),
				entry_point: "index.d.ts".to_owned(),
			}),
		));
		tracked.record_sync_success(SyncExecution {
			tracked_version_commit: "7071a42b0101e39d21111da72e13c46dc8ae596d".to_owned(),
			latest_remote_commit:   Some("5423cf56499c1ea33ea4bd9fbaab1723083cb659".to_owned()),
			summary:                IngestionSummary {
				entry_count:    9,
				document_count: 18,
				vector_count:   9,
			},
		})
		.await;
		tracked
			.update_progress(
				PackageSyncPhase::GeneratingIr,
				Some("generating TypeScript IR".to_owned()),
			)
			.await;

		registry.packages.write().await.insert(package_id, Arc::clone(&tracked));
		registry.keys.write().await.insert(
			PackageKey {
				language: Language::TypeScript,
				name:     "nanoid".to_owned(),
				version:  version.clone(),
				branch:   "main".to_owned(),
			},
			package_id,
		);
		registry.persist().await.unwrap();

		let rehydrated =
			LocalRegistry::new(storage.clone(), Duration::from_secs(60), test_pipeline_config());
		let packages = rehydrated.list_packages().await;

		assert_eq!(packages.len(), 1);
		assert!(matches!(packages[0].state.health, PackageHealth::Healthy));
		assert!(matches!(packages[0].state.sync_status, PackageSyncStatus::Idle));
		assert_eq!(packages[0].state.sync_phase, None);
		assert_eq!(packages[0].state.sync_detail, None);
		assert_eq!(packages[0].state.entry_count, 9);
		assert_eq!(packages[0].state.document_count, 18);
		assert_eq!(packages[0].state.vector_count, 9);
	}

	#[test]
	fn remote_update_is_false_when_remote_head_is_unknown() {
		let tracked = "abc".to_owned();
		assert!(!compute_remote_update_available(None, Some(&tracked)));
	}

	#[test]
	fn remote_update_is_true_when_remote_head_advances() {
		let tracked = "old".to_owned();
		let latest = "new".to_owned();
		assert!(compute_remote_update_available(Some(&latest), Some(&tracked)));
	}

	fn test_pipeline_config() -> PipelineConfig {
		PipelineConfig {
			terminus:        None,
			qdrant:          None,
			embedding_model: "text-embedding-3-small".to_owned(),
			upload_schema:   false,
		}
	}
}
