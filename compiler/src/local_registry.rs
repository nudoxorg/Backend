use std::{collections::HashMap, fs, num::NonZeroU64, sync::{Arc, atomic::{AtomicU64, Ordering}}};

use jiff::Timestamp;
use lang_types::Language;
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::{sync::{Mutex, RwLock, Semaphore}, task::spawn_blocking, time::{Duration, MissedTickBehavior}};
use tracing::{error, info, instrument, warn};
use url::Url;

use crate::{config::PipelineConfig, core::{rust::RustPackage, ts::TsPackage}, error::AppError, ingest::IngestionSummary, storage::StorageLayout, sync_progress::{PackageSyncPhase, PackageSyncStatus, ProgressReporter}, text_index::SymbolTextIndex, util::retry::Transient};

mod resolve;
mod sync;
mod ts_entry_point;

use resolve::resolve_package_handle;
use sync::{compute_remote_update_available, run_monitor_refresh, run_sync};

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

fn deserialize_language<'de, D: Deserializer<'de>>(d: D) -> Result<Language, D::Error> {
	let s = String::deserialize(d)?;
	s.parse::<Language>().map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewPackageRequest {
	#[serde(deserialize_with = "deserialize_language")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageStateSnapshot {
	pub health:                  PackageHealth,
	#[serde(default)]
	pub sync_status:             PackageSyncStatus,
	#[serde(default)]
	pub sync_phase:              Option<PackageSyncPhase>,
	#[serde(default)]
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
	sync_slots:       Arc<Semaphore>,
	next_id:          AtomicU64,
	packages:         Arc<RwLock<HashMap<PackageId, Arc<TrackedPackage>>>>,
	keys:             Arc<RwLock<HashMap<PackageKey, PackageId>>>,
	/// SQLite occurrence store. When set, every successful library parse
	/// registers symbols so deferred occurrence blobs can be resolved.
	nudox_store:      Option<Arc<nudox_store::NudoxStore>>,
	/// Local Tantivy text search index. When set, symbols are indexed after
	/// every successful library parse for standalone full-text search.
	text_index:       Option<Arc<SymbolTextIndex>>,
	/// nudox-search Orchestrator. When set, every ingested symbol is fed
	/// through `Orchestrator::ingest` so `/symbol-search` returns results.
	orchestrator:     Option<Arc<nudox_orchestrator::Orchestrator>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRegistry {
	packages: Vec<PersistedPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPackage {
	id:     u64,
	spec:   PackageSpec,
	handle: PackageHandle,
	state:  PackageStateSnapshot,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackageSpec {
	language: Language,
	name:     String,
	slug:     String,
	version:  Version,
	branch:   String,
	source:   Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PackageHandle {
	Rust(RustPackage),
	TypeScript(TsPackage),
}

struct TrackedPackage {
	id:        PackageId,
	spec:      PackageSpec,
	handle:    PackageHandle,
	state:     RwLock<PackageStateSnapshot>,
	sync_lock: Mutex<()>,
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
const MAX_CONCURRENT_SYNCS: usize = 2;
const SYNC_MAX_ATTEMPTS: usize = 3;
const SYNC_RETRY_BASE_DELAY: Duration = Duration::from_secs(5);


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
			sync_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_SYNCS)),
			next_id: AtomicU64::new(next_id),
			packages: Arc::new(RwLock::new(packages)),
			keys: Arc::new(RwLock::new(keys)),
			nudox_store: None,
			text_index: None,
			orchestrator: None,
		}
	}

	/// Attach an SQLite occurrence store. When set, every successful library
	/// parse will register symbols so deferred occurrence blobs can be resolved.
	pub fn with_nudox_store(mut self, store: Arc<nudox_store::NudoxStore>) -> Self {
		self.nudox_store = Some(store);
		self
	}

	/// Attach a local Tantivy text search index. When set, symbols are indexed
	/// after every successful library parse so `/text-search` works standalone.
	pub fn with_text_index(mut self, index: Arc<SymbolTextIndex>) -> Self {
		self.text_index = Some(index);
		self
	}

	/// Attach the nudox-search Orchestrator. When set, every ingested symbol is
	/// fed through `Orchestrator::ingest` so `/symbol-search` returns results.
	pub fn with_orchestrator(mut self, orch: Arc<nudox_orchestrator::Orchestrator>) -> Self {
		self.orchestrator = Some(orch);
		self
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
		let branch = request.branch.clone().unwrap_or_else(|| "main".to_owned());
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
		let _slot = self.sync_slots.clone().acquire_owned().await.map_err(|source| {
			AppError::SyncShutdown { source }
		})?;

		let progress = {
			let tracked = Arc::clone(&tracked);
			let package = tracked.spec.name.clone();
			let version = tracked.spec.version.clone();
			ProgressReporter::new(move |phase, detail| {
				let tracked = Arc::clone(&tracked);
				let detail_for_log = detail.clone();
				let package = package.clone();
				let version = version.clone();
				tokio::spawn(async move {
					info!(
						package = %package,
						version = %version,
						phase = ?phase,
						detail = detail_for_log.as_deref().unwrap_or(""),
						"package sync progress"
					);
					tracked.update_progress(phase, detail).await;
				});
			})
		};
		tracked.mark_sync_running(PackageSyncPhase::Resolving, Some("starting sync".to_owned())).await;
		self.persist().await?;

		let mut result = None;
		for attempt in 1..=SYNC_MAX_ATTEMPTS {
			let sync_result = run_sync(
				self.storage.clone(),
				self.pipeline.clone(),
				tracked.id,
				tracked.handle.clone(),
				tracked.spec.clone(),
				progress.clone(),
				self.nudox_store.clone(),
				self.text_index.clone(),
				self.orchestrator.clone(),
			)
			.await;

			match sync_result {
				Ok(execution) => {
					result = Some(Ok(execution));
					break;
				}
				Err(error) if attempt < SYNC_MAX_ATTEMPTS && error.is_transient() => {
					let retry_delay = SYNC_RETRY_BASE_DELAY * attempt as u32;
					warn!(
						package = %tracked.spec.name,
						version = %tracked.spec.version,
						attempt,
						max_attempts = SYNC_MAX_ATTEMPTS,
						delay_seconds = retry_delay.as_secs(),
						error = %error,
						"transient package sync failure, retrying"
					);
					tracked
						.update_progress(
							PackageSyncPhase::Resolving,
							Some(format!(
								"transient backend error, retrying in {}s (attempt {}/{})",
								retry_delay.as_secs(),
								attempt + 1,
								SYNC_MAX_ATTEMPTS
							)),
						)
						.await;
					tokio::time::sleep(retry_delay).await;
				}
				Err(error) => {
					result = Some(Err(error));
					break;
				}
			}
		}

		let result = result.expect("sync attempt loop always yields a result");

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
		.ok_or_else(|| AppError::IdExhausted)?;
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
			state: RwLock::new(PackageStateSnapshot::new()),
			sync_lock: Mutex::new(()),
		}
	}

	fn rehydrated(
		id: PackageId,
		spec: PackageSpec,
		handle: PackageHandle,
		state: PackageStateSnapshot,
	) -> Self {
		Self { id, spec, handle, state: RwLock::new(state), sync_lock: Mutex::new(()) }
	}

	async fn snapshot(&self) -> PackageSnapshot {
		PackageSnapshot {
			id:       self.id.get(),
			language: self.spec.language,
			name:     self.spec.name.clone(),
			slug:     self.spec.slug.clone(),
			source:   self.spec.source.clone(),
			version:  self.spec.version.clone(),
			branch:   self.spec.branch.clone(),
			state:    self.state.read().await.clone(),
		}
	}

	async fn persisted(&self) -> PersistedPackage {
		PersistedPackage {
			id:     self.id.get(),
			spec:   self.spec.clone(),
			handle: self.handle.clone(),
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

impl PackageStateSnapshot {
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
		let key = PackageKey {
			language: package.spec.language,
			name:     package.spec.name.clone(),
			version:  package.spec.version.clone(),
			branch:   package.spec.branch.clone(),
		};
		let tracked = Arc::new(TrackedPackage::rehydrated(
			id,
			package.spec,
			package.handle,
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

#[cfg(test)]
mod tests {
	use std::{num::NonZeroU64, time::Duration};

	use lang_types::Language;
	use semver::Version;
	use tempfile::TempDir;
	use url::Url;

	use super::*;
	use super::ts_entry_point::resolve_typescript_repository_entry_point;
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
	async fn resolves_explicit_rust_repository_handle() {
		let mut req = request(Language::Rust, "inko", "0.18.1");
		req.source = Some("https://github.com/inko-lang/inko".to_owned());

		let handle = resolve_package_handle(&req).await.unwrap();
		let PackageHandle::Rust(package) = handle else {
			panic!("expected Rust package handle");
		};

		assert_eq!(package.name, "inko");
		assert_eq!(package.slug, "inko");
		assert_eq!(package.source.as_str(), "https://github.com/inko-lang/inko");
	}

	#[test]
	fn resolve_typescript_repository_entry_point_falls_back_to_source_for_old_repo_layouts() {
		let workspace = tempfile::tempdir().unwrap();
		std::fs::write(
			workspace.path().join("package.json"),
			r#"{
				"main":"./lib/src/index.js",
				"types":"./lib/src/index.d.ts"
			}"#,
		)
		.unwrap();
		std::fs::create_dir_all(workspace.path().join("src")).unwrap();
		std::fs::write(workspace.path().join("src").join("index.ts"), "export const z = 1;\n").unwrap();

		let resolved = resolve_typescript_repository_entry_point(workspace.path(), None).unwrap();
		assert_eq!(resolved, workspace.path().join("src").join("index.ts"));
	}

	#[test]
	fn retryable_sync_errors_include_backend_connection_pressure() {
		use crate::util::retry::is_transient_message;
		assert!(is_transient_message("Backend DB connection error"));
		assert!(is_transient_message(
			"error sending request for url (http://127.0.0.1:6363/api/document/admin/main)"
		));
		assert!(is_transient_message("operation timed out"));
		assert!(!is_transient_message("version 1.0.0 not found"));
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
				direct_repo: true,
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
		tracked
			.record_sync_success(SyncExecution {
				tracked_version_commit: "7071a42b0101e39d21111da72e13c46dc8ae596d".to_owned(),
				latest_remote_commit:   Some("5423cf56499c1ea33ea4bd9fbaab1723083cb659".to_owned()),
				summary:                IngestionSummary {
					entry_count:          9,
					document_count:       18,
					vector_count:         9,
					symbols_registered:   0,
					symbols_text_indexed: 0,
					symbols_orchestrated: 0,
				},
			})
			.await;
		tracked
			.update_progress(PackageSyncPhase::GeneratingIr, Some("generating TypeScript IR".to_owned()))
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
