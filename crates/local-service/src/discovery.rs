//! Transactional, source-only package discovery journal.
//!
//! Each append frame contains the admitted source facts and its next opaque
//! cursor in one checksummed transaction. A torn final frame is discarded on
//! reopen; a complete corrupt frame fails closed. Search projections retain
//! one claim per `(source, coordinate)`, so competing registries never become
//! one synthetic acquisition record.

use crate::process::RegistryDiscoveryConfig;
#[cfg(test)]
use backend_engine::registry::DiscoveryTimestamp;
use backend_engine::registry::{
    DISCOVERY_BATCH_ENVELOPE_VERSION, DiscoveryBatch, DiscoveryBatchDraft, DiscoveryCompleteness,
    DiscoveryCursor, DiscoveryError, DiscoveryFacet, DiscoveryFact, DiscoveryMetadata,
    DiscoveryObservedAt, DiscoveryPackageRetraction, DiscoveryReleaseObservation,
    DiscoverySourceEvent, DiscoverySourceIdentity, DiscoveryStanding,
    MAX_DISCOVERY_BATCH_ENCODED_BYTES, MAX_DISCOVERY_PAGE_ITEMS, MAX_NPM_PACKUMENT_BYTES,
    MAX_PYPI_PROJECT_INDEX_BYTES, NugetCatalogEvent, RegistryEcosystem, RegistryEndpoint,
    crates_sparse_index_path, discovery_source_identity, parse_conan_recipe_tree,
    parse_conan_recipe_versions, parse_crates_recent_page, parse_crates_sparse_package,
    parse_go_module_index_page, parse_maven_search_page, parse_npm_changes_page,
    parse_npm_packument_document, parse_nuget_catalog_index, parse_nuget_catalog_leaf,
    parse_nuget_catalog_page, parse_pypi_project_list, parse_pypi_project_metadata,
};
use backend_platform::durable;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::TryLockError;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod package_metadata;

const JOURNAL_MAGIC: &[u8; 8] = b"DISCOV01";
const JOURNAL_VERSION: u16 = DISCOVERY_BATCH_ENVELOPE_VERSION;
const MAX_FRAME_BYTES: usize = MAX_DISCOVERY_BATCH_ENCODED_BYTES;
const MAX_FACTS_PER_SOURCE: usize = 2_000_000;
const MAX_SEARCH_DELTA_ROWS: usize = 8192;
const MAX_SOURCE_BODY_BYTES: usize = 32 * 1024 * 1024;
const CARGO_DISCOVERY_PAGE_SIZE: usize = 16;
const CARGO_SPARSE_CONCURRENCY: usize = 4;
const NPM_CHANGES_PAGE_SIZE: usize = 32;
const PYPI_PROJECTS_PER_REFRESH: usize = 4;
const PYPI_VERSIONS_PER_PROJECT: usize = 256;
const MAVEN_SEARCH_PAGE_SIZE: usize = 100;
const GO_INDEX_PAGE_SIZE: usize = 1000;
const CONAN_RECIPES_PER_REFRESH: usize = 8;
const DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const DISCOVERY_FRESHNESS_MILLIS: u64 = 60_000;
const DISCOVERY_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const DISCOVERY_REFRESH_BUDGET: Duration = Duration::from_secs(20);
// PEP 691 has no page cursor: its global project list must be downloaded
// before any bounded per-project metadata can be selected.
const PYPI_PROJECT_INDEX_TIMEOUT: Duration = Duration::from_secs(60);
const PYPI_DISCOVERY_REFRESH_BUDGET: Duration = Duration::from_secs(90);
const DISCOVERY_SOURCE_LIMIT: usize = 8;
const DISCOVERY_MESSAGE_CAPACITY: usize = 8;
const MAX_CACHED_SPARSE_RELEASES: usize = 4096;

/// Typed failure from opening or advancing the discovery journal.
#[derive(Debug)]
pub(crate) enum DiscoveryStoreError {
    Io(io::Error),
    Decode,
    Corrupt,
    UnsupportedVersion(u16),
    Conflict,
    Busy,
    Bounds,
    Cancelled,
}

impl From<io::Error> for DiscoveryStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DiscoveryError> for DiscoveryStoreError {
    fn from(error: DiscoveryError) -> Self {
        match error {
            DiscoveryError::Bounds => Self::Bounds,
            DiscoveryError::Protocol | DiscoveryError::InvalidIdentity => Self::Decode,
        }
    }
}

/// Result of committing a source page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiscoveryCommit {
    Committed,
    AlreadyCommitted,
}

#[derive(Clone, Debug)]
struct DiscoveryProgressToken {
    sequence: u64,
    cursor: DiscoveryCursor,
}

#[derive(Clone, Debug, Default)]
struct SourceProgress {
    sequence: u64,
    cursor: DiscoveryCursor,
    source_high_watermark: DiscoveryCursor,
    caught_up: bool,
    completeness: Option<DiscoveryCompleteness>,
    latest_observed_at: Option<u64>,
    historical: bool,
    refresh_failed: bool,
    last_batch_fingerprint: Option<[u8; 32]>,
    facts: BTreeMap<String, DiscoveryFact>,
    package_releases: BTreeMap<String, BTreeSet<String>>,
}

enum MaterializedDiscoveryBatch<'a> {
    Borrowed(&'a DiscoveryBatch),
    Owned(DiscoveryBatch),
}

struct PreparedFactAction {
    fact_index: usize,
    key: String,
    transition: DiscoveryFactTransition,
    package_name: Option<String>,
    package_release_key: Option<String>,
    search_document: Option<DiscoverySearchDocument>,
}

struct PreparedDiscoveryApply {
    next_sequence: u64,
    actions: Vec<PreparedFactAction>,
    search_update_count: usize,
    search_documents: Vec<DiscoverySearchDocument>,
}

impl MaterializedDiscoveryBatch<'_> {
    fn as_ref(&self) -> &DiscoveryBatch {
        match self {
            Self::Borrowed(batch) => batch,
            Self::Owned(batch) => batch,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoverySearchDocument {
    pub(crate) source: DiscoverySourceIdentity,
    pub(crate) coordinate: backend_engine::registry::PackageCoordinate,
    pub(crate) lineage: String,
}

#[derive(Clone, Debug)]
struct DiscoverySearchChange {
    revision: u64,
    document: DiscoverySearchDocument,
}

/// Durable append-only owner for package discovery claims.
pub(crate) struct DiscoveryStore {
    path: PathBuf,
    sources: BTreeMap<DiscoverySourceIdentity, SourceProgress>,
    journal: File,
    _lease: File,
    poisoned: bool,
    search_revision: u64,
    observation_revision: u64,
    search_changes: VecDeque<DiscoverySearchChange>,
}

/// Opt-in source-only refresh owner. Its HTTP client is instantiated only for
/// configured online discovery feeds and only requests catalog metadata URLs.
pub(crate) struct DiscoveryGateway {
    store: DiscoveryStore,
    receiver: Receiver<DiscoveryWorkerMessage>,
    workers: Vec<JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    metadata_sources: Vec<RegistryEndpoint>,
    metadata_offline: bool,
    package_metadata: package_metadata::PackageMetadataCache,
}

enum DiscoveryWorkerMessage {
    Batch {
        batch: DiscoveryBatch,
        acknowledgement: SyncSender<Option<(bool, DiscoveryProgressToken)>>,
    },
    Failed {
        source: DiscoverySourceIdentity,
    },
}

impl DiscoveryGateway {
    pub(crate) fn open(root: PathBuf, config: RegistryDiscoveryConfig) -> Result<Self, String> {
        if config.sources.len() > DISCOVERY_SOURCE_LIMIT {
            return Err(format!(
                "registry discovery supports at most {DISCOVERY_SOURCE_LIMIT} sources"
            ));
        }
        let store = DiscoveryStore::open(root.join("catalog.journal"))
            .map_err(|error| format!("open discovery journal: {error:?}"))?;
        let (sender, receiver) = mpsc::sync_channel(DISCOVERY_MESSAGE_CAPACITY);
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();
        let mut seen = BTreeSet::new();
        let mut sources = Vec::new();
        let metadata_sources = config.sources.clone();
        let metadata_offline = config.offline;
        for endpoint in config.sources {
            let source = discovery_source_identity(&endpoint);
            if seen.insert(source) {
                sources.push((endpoint, source));
            }
        }
        if !config.offline {
            for (endpoint, source) in sources {
                let worker_sender = sender.clone();
                let worker_cancelled = Arc::clone(&cancelled);
                let max_pages = config.max_pages;
                let progress = store.progress_token(source);
                let source_id = source.id();
                let worker = thread::Builder::new()
                    .name(format!(
                        "registry-discovery-{:02x}{:02x}",
                        source_id[0], source_id[1]
                    ))
                    .spawn(move || {
                        discovery_worker(
                            endpoint,
                            progress,
                            max_pages,
                            worker_sender,
                            worker_cancelled,
                        );
                    });
                match worker {
                    Ok(worker) => workers.push(worker),
                    Err(error) => {
                        cancelled.store(true, Ordering::Release);
                        for worker in &workers {
                            worker.thread().unpark();
                        }
                        for worker in workers.drain(..) {
                            let _ = worker.join();
                        }
                        return Err(format!("spawn registry discovery worker: {error}"));
                    }
                }
            }
        }
        drop(sender);
        Ok(Self {
            store,
            receiver,
            workers,
            cancelled,
            metadata_sources,
            metadata_offline,
            package_metadata: package_metadata::PackageMetadataCache::default(),
        })
    }

    /// Applies already-fetched source transactions on the single owner thread.
    /// Search calls never wait for feed I/O.
    pub(crate) fn apply_pending(&mut self) {
        loop {
            match self.receiver.try_recv() {
                Ok(DiscoveryWorkerMessage::Batch {
                    batch,
                    acknowledgement,
                }) => {
                    let source = batch.source;
                    let committed = self.store.commit(batch).is_ok();
                    self.store.mark_failed(source, !committed);
                    let latest = Some((committed, self.store.progress_token(source)));
                    let _ = acknowledgement.send(latest);
                }
                Ok(DiscoveryWorkerMessage::Failed { source }) => {
                    self.store.mark_failed(source, true);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    pub(crate) fn store(&self) -> &DiscoveryStore {
        &self.store
    }
}

impl Drop for DiscoveryGateway {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        while let Ok(message) = self.receiver.try_recv() {
            if let DiscoveryWorkerMessage::Batch {
                acknowledgement, ..
            } = message
            {
                let _ = acknowledgement.send(None);
            }
        }
        for worker in &self.workers {
            worker.thread().unpark();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn discovery_worker(
    endpoint: RegistryEndpoint,
    mut progress: DiscoveryProgressToken,
    max_pages: usize,
    sender: SyncSender<DiscoveryWorkerMessage>,
    cancelled: Arc<AtomicBool>,
) {
    let source = discovery_source_identity(&endpoint);
    let mut cursor = progress.cursor.clone();
    let mut retry_delay = DISCOVERY_REFRESH_INTERVAL;
    let mut sparse_cache = BTreeMap::new();
    while !cancelled.load(Ordering::Acquire) {
        let refresh_budget = match endpoint.ecosystem() {
            RegistryEcosystem::Pypi => PYPI_DISCOVERY_REFRESH_BUDGET,
            _ => DISCOVERY_REFRESH_BUDGET,
        };
        let deadline = Instant::now() + refresh_budget;
        match build_source_batch(
            &endpoint,
            cursor.clone(),
            max_pages,
            deadline,
            &cancelled,
            &mut sparse_cache,
        ) {
            Ok(draft) => {
                let batch = match draft.admit_for_sequence(progress.sequence) {
                    Ok(batch) => batch,
                    Err(_) => {
                        let _ = send_worker_message(
                            &sender,
                            DiscoveryWorkerMessage::Failed { source },
                            &cancelled,
                        );
                        wait_for_retry(retry_delay, &cancelled);
                        continue;
                    }
                };
                let (acknowledgement, response) = mpsc::sync_channel(1);
                if !send_worker_message(
                    &sender,
                    DiscoveryWorkerMessage::Batch {
                        batch,
                        acknowledgement,
                    },
                    &cancelled,
                ) {
                    return;
                }
                loop {
                    if cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    match response.try_recv() {
                        Ok(Some((committed, latest))) => {
                            progress = latest;
                            cursor = progress.cursor.clone();
                            if committed {
                                retry_delay = DISCOVERY_REFRESH_INTERVAL;
                            } else {
                                retry_delay = retry_delay
                                    .saturating_mul(2)
                                    .min(Duration::from_secs(15 * 60));
                            }
                            break;
                        }
                        Ok(None) | Err(TryRecvError::Disconnected) => break,
                        Err(TryRecvError::Empty) => thread::park_timeout(Duration::from_millis(50)),
                    }
                }
            }
            Err(DiscoveryStoreError::Cancelled) => return,
            Err(_) => {
                if !send_worker_message(
                    &sender,
                    DiscoveryWorkerMessage::Failed { source },
                    &cancelled,
                ) {
                    return;
                }
                wait_for_retry(retry_delay, &cancelled);
                retry_delay = retry_delay
                    .saturating_mul(2)
                    .min(Duration::from_secs(15 * 60));
                continue;
            }
        }
        wait_for_retry(retry_delay, &cancelled);
    }
}

fn send_worker_message(
    sender: &SyncSender<DiscoveryWorkerMessage>,
    mut message: DiscoveryWorkerMessage,
    cancelled: &AtomicBool,
) -> bool {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(message) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(returned)) => {
                message = returned;
                thread::park_timeout(Duration::from_millis(50));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn wait_for_retry(delay: Duration, cancelled: &AtomicBool) {
    let until = Instant::now() + delay;
    while !cancelled.load(Ordering::Acquire) {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::park_timeout(remaining.min(Duration::from_millis(100)));
    }
}

fn build_source_batch(
    endpoint: &RegistryEndpoint,
    previous: DiscoveryCursor,
    max_pages: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
    sparse_cache: &mut BTreeMap<String, CachedSparsePackage>,
) -> Result<DiscoveryBatchDraft, DiscoveryStoreError> {
    match endpoint.ecosystem() {
        RegistryEcosystem::Cargo => {
            refresh_crates(endpoint, previous, deadline, cancelled, sparse_cache)
        }
        RegistryEcosystem::Npm => refresh_npm(endpoint, previous, max_pages, deadline, cancelled),
        RegistryEcosystem::Pypi => refresh_pypi(endpoint, previous, max_pages, deadline, cancelled),
        RegistryEcosystem::Maven => {
            refresh_maven(endpoint, previous, max_pages, deadline, cancelled)
        }
        RegistryEcosystem::Nuget => {
            refresh_nuget(endpoint, previous, max_pages, deadline, cancelled)
        }
        RegistryEcosystem::Golang => {
            refresh_go_modules(endpoint, previous, max_pages, deadline, cancelled)
        }
        RegistryEcosystem::Cpp => {
            refresh_conan_center(endpoint, previous, max_pages, deadline, cancelled)
        }
    }
}

fn refresh_crates(
    endpoint: &RegistryEndpoint,
    previous: DiscoveryCursor,
    deadline: Instant,
    cancelled: &AtomicBool,
    sparse_cache: &mut BTreeMap<String, CachedSparsePackage>,
) -> Result<DiscoveryBatchDraft, DiscoveryStoreError> {
    let source = discovery_source_identity(endpoint);
    let page_number = if previous.is_empty() {
        1
    } else {
        let bytes = previous.as_bytes();
        if bytes.len() != 4 {
            return Err(DiscoveryStoreError::Corrupt);
        }
        u32::from_be_bytes(bytes.try_into().map_err(|_| DiscoveryStoreError::Corrupt)?).max(1)
    };
    let url = format!(
        "{}/api/v1/crates?page={page_number}&per_page={CARGO_DISCOVERY_PAGE_SIZE}&sort=recent-updates",
        endpoint.as_str().trim_end_matches('/')
    );
    let bytes = fetch_metadata(&url, 16 * 1024 * 1024, deadline, cancelled)?;
    let parsed = parse_crates_recent_page(
        &bytes,
        page_number,
        CARGO_DISCOVERY_PAGE_SIZE,
        CARGO_DISCOVERY_PAGE_SIZE,
    )
    .map_err(DiscoveryStoreError::from)?;
    let observed_at = DiscoveryObservedAt::from_unix_millis(discovery_now());
    let mut facts = Vec::new();
    let mut packages = BTreeSet::new();
    let mut recent_metadata = BTreeMap::new();
    for release in parsed.releases {
        packages.insert(discovered_package_name(release.coordinate.as_str()).to_owned());
        recent_metadata.insert(release.coordinate.as_str().to_owned(), release.metadata);
    }
    let packages = packages.into_iter().collect::<Vec<_>>();
    for package_batch in packages.chunks(CARGO_SPARSE_CONCURRENCY) {
        let requests = package_batch
            .iter()
            .map(|package_name| {
                let sparse_path =
                    crates_sparse_index_path(package_name).map_err(DiscoveryStoreError::from)?;
                let url = format!(
                    "{}/index/{sparse_path}",
                    cargo_sparse_base_url(endpoint.as_str())
                );
                let etag = sparse_cache
                    .get(package_name)
                    .and_then(|cached| cached.etag.clone());
                Ok((package_name.clone(), url, etag))
            })
            .collect::<Result<Vec<_>, DiscoveryStoreError>>()?;
        let responses = thread::scope(|scope| {
            let handles = requests
                .into_iter()
                .map(|(package_name, url, etag)| {
                    scope.spawn(move || {
                        let result = fetch_sparse_metadata(
                            &url,
                            16 * 1024 * 1024,
                            deadline,
                            cancelled,
                            etag.as_deref(),
                        );
                        (package_name, result)
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle.join().map_err(|_| {
                        DiscoveryStoreError::Io(io::Error::other("sparse metadata worker panicked"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })?;
        for (package_name, response) in responses {
            let (sparse_bytes, etag) = response?;
            let package_facts = if let Some(sparse_bytes) = sparse_bytes {
                let package = parse_crates_sparse_package(
                    &sparse_bytes,
                    &package_name,
                    MAX_DISCOVERY_PAGE_ITEMS,
                )
                .map_err(DiscoveryStoreError::from)?;
                let package_facts = package
                    .releases
                    .into_iter()
                    .map(|release| {
                        let coordinate = release.coordinate;
                        let metadata = recent_metadata
                            .remove(coordinate.as_str())
                            .map_or(release.metadata.clone(), |metadata| {
                                merge_discovery_metadata(release.metadata.clone(), metadata)
                            });
                        DiscoveryFact {
                            source,
                            coordinate,
                            standing: release.standing,
                            observed_at,
                            source_event: DiscoverySourceEvent::Unordered,
                            source_event_time: None,
                            proof: release.proof,
                            metadata,
                        }
                    })
                    .collect::<Vec<_>>();
                if let Some(etag) = etag {
                    let other_cached_releases = sparse_cache
                        .iter()
                        .filter(|(name, _)| *name != &package_name)
                        .map(|(_, cached)| cached.facts.len())
                        .sum::<usize>();
                    if other_cached_releases.saturating_add(package_facts.len())
                        <= MAX_CACHED_SPARSE_RELEASES
                    {
                        sparse_cache.insert(
                            package_name.clone(),
                            CachedSparsePackage {
                                etag: Some(etag),
                                facts: package_facts.clone(),
                            },
                        );
                    } else {
                        sparse_cache.remove(&package_name);
                    }
                } else {
                    sparse_cache.remove(&package_name);
                }
                package_facts
            } else {
                let cached = sparse_cache
                    .get(&package_name)
                    .ok_or(DiscoveryStoreError::Corrupt)?;
                let mut cached_facts = cached.facts.clone();
                if let Some(etag) = etag
                    && let Some(cached) = sparse_cache.get_mut(&package_name)
                {
                    cached.etag = Some(etag);
                }
                cached_facts.iter_mut().for_each(|fact| {
                    fact.observed_at = observed_at;
                    if let Some(metadata) = recent_metadata.remove(fact.coordinate.as_str()) {
                        fact.metadata = merge_discovery_metadata(fact.metadata.clone(), metadata);
                    }
                });
                cached_facts
            };
            if facts.len().saturating_add(package_facts.len()) > MAX_DISCOVERY_PAGE_ITEMS {
                return Err(DiscoveryStoreError::Bounds);
            }
            facts.extend(package_facts);
        }
    }
    let batch = DiscoveryBatchDraft {
        source,
        previous_cursor: previous,
        next_cursor: parsed.next_cursor.clone(),
        source_high_watermark: parsed.next_cursor,
        caught_up: false,
        observed_at,
        completeness: DiscoveryCompleteness::Windowed,
        facts,
        package_retractions: Vec::new(),
    };
    batch.admit().map_err(DiscoveryStoreError::from)?;
    Ok(batch)
}

fn refresh_npm(
    endpoint: &RegistryEndpoint,
    previous: DiscoveryCursor,
    max_pages: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<DiscoveryBatchDraft, DiscoveryStoreError> {
    let source = discovery_source_identity(endpoint);
    let since = parse_npm_cursor(&previous)?;
    let root_bytes = fetch_metadata(endpoint.as_str(), 1024 * 1024, deadline, cancelled)?;
    let root: serde_json::Value =
        serde_json::from_slice(&root_bytes).map_err(|_| DiscoveryStoreError::Decode)?;
    let high = root
        .as_object()
        .and_then(|object| object.get("update_seq"))
        .ok_or(DiscoveryStoreError::Decode)?;
    let high = match high {
        serde_json::Value::Number(value) => value.as_u64(),
        serde_json::Value::String(value) => value.parse::<u64>().ok(),
        _ => None,
    }
    .ok_or(DiscoveryStoreError::Decode)?;
    if high < since {
        return Err(DiscoveryStoreError::Conflict);
    }
    let source_high_watermark = npm_sequence_cursor(high)?;
    let limit = max_pages
        .saturating_mul(NPM_CHANGES_PAGE_SIZE)
        .clamp(1, MAX_DISCOVERY_PAGE_ITEMS);
    let url = format!(
        "{}/_changes?since={since}&limit={limit}",
        endpoint.as_str().trim_end_matches('/')
    );
    let bytes = fetch_metadata(&url, MAX_SOURCE_BODY_BYTES, deadline, cancelled)?;
    let page = parse_npm_changes_page(&bytes, &previous, &source_high_watermark, limit)
        .map_err(DiscoveryStoreError::from)?;
    let observed_at = DiscoveryObservedAt::from_unix_millis(discovery_now());
    let mut facts = Vec::new();
    let mut package_retractions = Vec::new();
    let mut completeness = page.completeness;
    // A bounded page can contain several revisions of the same package.
    // Resolve its last row once so we fetch one current packument and avoid
    // creating contradictory per-version claims from superseded revisions.
    let mut latest_by_package = BTreeMap::new();
    for package in page.packages {
        latest_by_package.insert(package.name.clone(), package);
    }
    for package in latest_by_package.into_values() {
        let sequence = package
            .source_event_time
            .parse::<u64>()
            .map_err(|_| DiscoveryStoreError::Corrupt)?;
        let source_event = DiscoverySourceEvent::NpmChange {
            sequence,
            revision: package.revision.clone(),
            change_proof: package.proof,
        };
        if package.deleted {
            package_retractions.push(DiscoveryPackageRetraction {
                package_name: package.name,
                source_event_time: package.source_event_time,
                source_event,
                proof: package.proof,
            });
            continue;
        }
        if package.requires_packument {
            let base = npm_packument_base(endpoint.as_str());
            let name = percent_encode_component(&package.name);
            let packument_url = format!("{}/{name}", base.trim_end_matches('/'));
            let packument_bytes =
                fetch_metadata(&packument_url, MAX_NPM_PACKUMENT_BYTES, deadline, cancelled)?;
            let sequence = package
                .source_event_time
                .parse::<u64>()
                .map_err(|_| DiscoveryStoreError::Corrupt)?;
            let packument = parse_npm_packument_document(
                &packument_bytes,
                &package.name,
                sequence,
                MAX_DISCOVERY_PAGE_ITEMS,
            )
            .map_err(DiscoveryStoreError::from)?;
            if package.revision.as_deref() != packument.revision.as_deref() {
                return Err(DiscoveryStoreError::Conflict);
            }
            if packument.is_truncated {
                completeness = DiscoveryCompleteness::Incomplete;
            }
            for mut release in packument.releases {
                release.source_event_time = Some(package.source_event_time.clone());
                if facts.len() >= MAX_DISCOVERY_PAGE_ITEMS {
                    return Err(DiscoveryStoreError::Bounds);
                }
                facts.push(discovery_fact(
                    source,
                    release,
                    observed_at,
                    None,
                    source_event.clone(),
                ));
            }
        } else {
            facts.extend(package.releases.into_iter().map(|release| {
                discovery_fact(source, release, observed_at, None, source_event.clone())
            }));
        }
        if facts.len() > MAX_DISCOVERY_PAGE_ITEMS {
            return Err(DiscoveryStoreError::Bounds);
        }
    }
    let caught_up = page.pending == 0 && page.next_cursor == source_high_watermark;
    let batch = DiscoveryBatchDraft {
        source,
        previous_cursor: page.previous_cursor,
        next_cursor: page.next_cursor,
        source_high_watermark,
        caught_up,
        observed_at,
        completeness,
        facts,
        package_retractions,
    };
    batch.admit().map_err(DiscoveryStoreError::from)?;
    Ok(batch)
}

fn refresh_pypi(
    endpoint: &RegistryEndpoint,
    previous: DiscoveryCursor,
    max_pages: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<DiscoveryBatchDraft, DiscoveryStoreError> {
    let source = discovery_source_identity(endpoint);
    let previous_project = parse_pypi_cursor(&previous)?;
    let list_url = format!("{}/simple/", endpoint.as_str().trim_end_matches('/'));
    let list_bytes = fetch_metadata_with_accept(
        &list_url,
        MAX_PYPI_PROJECT_INDEX_BYTES,
        deadline,
        cancelled,
        "application/vnd.pypi.simple.v1+json",
        PYPI_PROJECT_INDEX_TIMEOUT,
    )?;
    let list = parse_pypi_project_list(
        &list_bytes,
        backend_engine::registry::MAX_DISCOVERY_PROJECTS,
    )
    .map_err(DiscoveryStoreError::from)?;
    let start = previous_project.as_ref().map_or(0, |last| {
        list.projects
            .partition_point(|project| project.canonical_name.as_str() <= last.as_str())
            .min(list.projects.len())
    });
    let projects_per_refresh = max_pages
        .max(1)
        .min(PYPI_PROJECTS_PER_REFRESH)
        .min(list.projects.len());
    let selected = if list.projects.is_empty() {
        Vec::new()
    } else {
        (0..projects_per_refresh)
            .map(|index| &list.projects[(start + index) % list.projects.len()])
            .collect::<Vec<_>>()
    };
    let observed_at = DiscoveryObservedAt::from_unix_millis(discovery_now());
    let mut facts = Vec::new();
    let mut metadata_truncated = false;
    for project in &selected {
        let escaped_name = percent_encode_component(&project.name);
        let url = format!(
            "{}/pypi/{escaped_name}/json",
            endpoint.as_str().trim_end_matches('/')
        );
        let bytes = fetch_metadata(&url, 8 * 1024 * 1024, deadline, cancelled)?;
        let metadata =
            parse_pypi_project_metadata(&bytes, &project.name, PYPI_VERSIONS_PER_PROJECT)
                .map_err(DiscoveryStoreError::from)?;
        metadata_truncated |= metadata.is_truncated;
        facts.extend(metadata.releases.into_iter().map(|release| {
            // PEP 691 project serials do not establish release order. Upload
            // time remains only in the exact release metadata facet.
            discovery_fact(
                source,
                release,
                observed_at,
                None,
                DiscoverySourceEvent::Unordered,
            )
        }));
        if facts.len() > MAX_DISCOVERY_PAGE_ITEMS {
            return Err(DiscoveryStoreError::Bounds);
        }
    }
    let next_cursor = selected.last().map_or_else(
        || Ok(previous.clone()),
        |project| pypi_cursor(&project.canonical_name),
    )?;
    let snapshot = list
        .projects
        .iter()
        .map(|project| project.canonical_name.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let source_high_watermark = DiscoveryCursor::new(
        format!(
            "pypi-snapshot:{}",
            blake3::hash(snapshot.as_bytes()).to_hex()
        )
        .into_bytes(),
    )
    .map_err(DiscoveryStoreError::from)?;
    // PEP 691 defines a mutable, unordered project set, not an event feed.
    // The rotating normalized-name cursor is fair across polls, but cannot
    // certify a complete global snapshot.
    let caught_up = false;
    let completeness = if list.is_truncated || metadata_truncated {
        DiscoveryCompleteness::Incomplete
    } else {
        DiscoveryCompleteness::Windowed
    };
    let batch = DiscoveryBatchDraft {
        source,
        previous_cursor: previous,
        next_cursor,
        source_high_watermark,
        caught_up,
        observed_at,
        completeness,
        facts,
        package_retractions: Vec::new(),
    };
    batch.admit().map_err(DiscoveryStoreError::from)?;
    Ok(batch)
}

fn refresh_maven(
    endpoint: &RegistryEndpoint,
    previous: DiscoveryCursor,
    _max_pages: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<DiscoveryBatchDraft, DiscoveryStoreError> {
    let source = discovery_source_identity(endpoint);
    let rows = MAVEN_SEARCH_PAGE_SIZE.min(MAX_DISCOVERY_PAGE_ITEMS);
    // Maven Central's timestamp-sorted offset view is mutable: inserting or
    // updating a document can shift every later offset without changing the
    // total count. Observe one bounded newest window on every poll instead of
    // persisting an offset that could permanently skip shifted rows.
    let url = format!(
        "{}/solrsearch/select?q=*%3A*&rows={rows}&start=0&wt=json&sort=timestamp%20desc%2Cg%20asc%2Ca%20asc%2Cv%20asc",
        endpoint.as_str().trim_end_matches('/')
    );
    let bytes = fetch_metadata(&url, MAX_SOURCE_BODY_BYTES, deadline, cancelled)?;
    let page = parse_maven_search_page(&bytes, 0, rows).map_err(DiscoveryStoreError::from)?;
    if page.offset != 0 || page.next_offset > rows as u64 {
        return Err(DiscoveryStoreError::Conflict);
    }
    let observed_at = DiscoveryObservedAt::from_unix_millis(discovery_now());
    let facts = page
        .releases
        .into_iter()
        .map(|release| {
            discovery_fact(
                source,
                release,
                observed_at,
                None,
                DiscoverySourceEvent::Unordered,
            )
        })
        .collect::<Vec<_>>();
    let next_cursor = next_maven_window_cursor(&previous)?;
    let batch = DiscoveryBatchDraft {
        source,
        previous_cursor: previous,
        next_cursor: next_cursor.clone(),
        // The opaque monotone cursor identifies this local bounded observation
        // generation. Maven does not expose a stable change-feed watermark.
        source_high_watermark: next_cursor,
        caught_up: false,
        observed_at,
        completeness: DiscoveryCompleteness::Windowed,
        facts,
        package_retractions: Vec::new(),
    };
    batch.admit().map_err(DiscoveryStoreError::from)?;
    Ok(batch)
}

fn refresh_go_modules(
    endpoint: &RegistryEndpoint,
    previous: DiscoveryCursor,
    max_pages: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<DiscoveryBatchDraft, DiscoveryStoreError> {
    let source = discovery_source_identity(endpoint);
    let since =
        std::str::from_utf8(previous.as_bytes()).map_err(|_| DiscoveryStoreError::Corrupt)?;
    let limit = GO_INDEX_PAGE_SIZE
        .saturating_mul(max_pages.max(1).min(4))
        .min(MAX_DISCOVERY_PAGE_ITEMS);
    let url = format!(
        "{}/index?limit={limit}&since={}",
        endpoint.as_str().trim_end_matches('/'),
        percent_encode_component(since)
    );
    let bytes = fetch_metadata(&url, 16 * 1024 * 1024, deadline, cancelled)?;
    let page =
        parse_go_module_index_page(&bytes, &previous, limit).map_err(DiscoveryStoreError::from)?;
    let observed_at = DiscoveryObservedAt::from_unix_millis(discovery_now());
    let facts = page
        .releases
        .into_iter()
        .map(|release| {
            discovery_fact(
                source,
                release,
                observed_at,
                None,
                DiscoverySourceEvent::Unordered,
            )
        })
        .collect::<Vec<_>>();
    let caught_up = facts.len() < limit;
    let batch = DiscoveryBatchDraft {
        source,
        previous_cursor: page.previous_cursor,
        next_cursor: page.next_cursor,
        source_high_watermark: page.source_high_watermark,
        caught_up,
        observed_at,
        completeness: DiscoveryCompleteness::Windowed,
        facts,
        package_retractions: Vec::new(),
    };
    batch.admit().map_err(DiscoveryStoreError::from)?;
    Ok(batch)
}

fn refresh_conan_center(
    endpoint: &RegistryEndpoint,
    previous: DiscoveryCursor,
    max_pages: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<DiscoveryBatchDraft, DiscoveryStoreError> {
    let source = discovery_source_identity(endpoint);
    let (previous_generation, previous_sha, previous_offset) = parse_conan_cursor(&previous)?;
    let tree_url = format!(
        "{}/git/trees/master?recursive=1",
        endpoint.as_str().trim_end_matches('/')
    );
    let tree_bytes = fetch_metadata(&tree_url, MAX_SOURCE_BODY_BYTES, deadline, cancelled)?;
    let tree = parse_conan_recipe_tree(&tree_bytes, MAX_DISCOVERY_PAGE_ITEMS)
        .map_err(DiscoveryStoreError::from)?;
    let same_tree = previous_sha == tree.tree_sha;
    let generation = if same_tree {
        previous_generation
    } else {
        previous_generation
            .checked_add(1)
            .ok_or(DiscoveryStoreError::Bounds)?
    };
    let offset = if same_tree {
        previous_offset.min(tree.recipes.len())
    } else {
        0
    };
    let recipe_limit = CONAN_RECIPES_PER_REFRESH
        .saturating_mul(max_pages.max(1))
        .min(MAX_DISCOVERY_PAGE_ITEMS);
    let selected = tree
        .recipes
        .iter()
        .skip(offset)
        .take(recipe_limit)
        .collect::<Vec<_>>();
    let selected_count = selected.len();
    let observed_at = DiscoveryObservedAt::from_unix_millis(discovery_now());
    let mut facts = Vec::new();
    for recipe in selected {
        let path = recipe
            .config_path
            .split('/')
            .map(percent_encode_component)
            .collect::<Vec<_>>()
            .join("/");
        let url = format!(
            "{}/contents/{path}?ref={}",
            endpoint.as_str().trim_end_matches('/'),
            tree.tree_sha
        );
        let bytes = fetch_metadata(&url, 2 * 1024 * 1024, deadline, cancelled)?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| DiscoveryStoreError::Decode)?;
        let object = value.as_object().ok_or(DiscoveryStoreError::Decode)?;
        if object.get("encoding").and_then(serde_json::Value::as_str) != Some("base64") {
            return Err(DiscoveryStoreError::Decode);
        }
        let content = object
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or(DiscoveryStoreError::Decode)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(
                content
                    .bytes()
                    .filter(|byte| !byte.is_ascii_whitespace())
                    .collect::<Vec<_>>(),
            )
            .map_err(|_| DiscoveryStoreError::Decode)?;
        let releases =
            parse_conan_recipe_versions(&decoded, &recipe.name, MAX_DISCOVERY_PAGE_ITEMS)
                .map_err(DiscoveryStoreError::from)?;
        facts.extend(releases.into_iter().map(|release| {
            discovery_fact(
                source,
                release,
                observed_at,
                None,
                DiscoverySourceEvent::Unordered,
            )
        }));
        if facts.len() > MAX_DISCOVERY_PAGE_ITEMS {
            return Err(DiscoveryStoreError::Bounds);
        }
    }
    let next_offset = offset.saturating_add(selected_count);
    let next_cursor = conan_cursor(generation, &tree.tree_sha, next_offset)?;
    let source_high_watermark = conan_cursor(generation, &tree.tree_sha, tree.recipes.len())?;
    let caught_up = next_offset >= tree.recipes.len() && !tree.truncated;
    let batch = DiscoveryBatchDraft {
        source,
        previous_cursor: previous,
        next_cursor,
        source_high_watermark,
        caught_up,
        observed_at,
        completeness: if tree.truncated {
            DiscoveryCompleteness::Incomplete
        } else {
            DiscoveryCompleteness::Windowed
        },
        facts,
        package_retractions: Vec::new(),
    };
    batch.admit().map_err(DiscoveryStoreError::from)?;
    Ok(batch)
}

fn discovery_fact(
    source: DiscoverySourceIdentity,
    release: DiscoveryReleaseObservation,
    observed_at: DiscoveryObservedAt,
    source_event_override: Option<String>,
    source_event: DiscoverySourceEvent,
) -> DiscoveryFact {
    DiscoveryFact {
        source,
        coordinate: release.coordinate,
        standing: release.standing,
        observed_at,
        source_event,
        source_event_time: source_event_override.or(release.source_event_time),
        proof: release.proof,
        metadata: release.metadata,
    }
}

fn parse_npm_cursor(cursor: &DiscoveryCursor) -> Result<u64, DiscoveryStoreError> {
    if cursor.is_empty() {
        return Ok(0);
    }
    let value = std::str::from_utf8(cursor.as_bytes()).map_err(|_| DiscoveryStoreError::Corrupt)?;
    value
        .strip_prefix("npm-v1:")
        .unwrap_or(value)
        .parse::<u64>()
        .map_err(|_| DiscoveryStoreError::Corrupt)
}

fn npm_sequence_cursor(sequence: u64) -> Result<DiscoveryCursor, DiscoveryStoreError> {
    DiscoveryCursor::new(format!("npm-v1:{sequence:020}").into_bytes())
        .map_err(DiscoveryStoreError::from)
}

fn next_maven_window_cursor(
    previous: &DiscoveryCursor,
) -> Result<DiscoveryCursor, DiscoveryStoreError> {
    let generation = if previous.is_empty() {
        0
    } else {
        let value =
            std::str::from_utf8(previous.as_bytes()).map_err(|_| DiscoveryStoreError::Corrupt)?;
        if let Some(generation) = value.strip_prefix("maven-window-v1:") {
            generation
                .parse::<u64>()
                .map_err(|_| DiscoveryStoreError::Corrupt)?
        } else {
            // Migrate the old numeric offset cursor to a monotone observation
            // generation. The numeric value is never reused as a new offset.
            value
                .parse::<u64>()
                .map_err(|_| DiscoveryStoreError::Corrupt)?
        }
    };
    let next = generation
        .checked_add(1)
        .ok_or(DiscoveryStoreError::Bounds)?;
    DiscoveryCursor::new(format!("maven-window-v1:{next:020}").into_bytes())
        .map_err(DiscoveryStoreError::from)
}

fn npm_packument_base(replication_endpoint: &str) -> String {
    if replication_endpoint == "https://replicate.npmjs.com/registry" {
        "https://registry.npmjs.org".to_owned()
    } else {
        replication_endpoint.trim_end_matches('/').to_owned()
    }
}

fn parse_pypi_cursor(cursor: &DiscoveryCursor) -> Result<Option<String>, DiscoveryStoreError> {
    if cursor.is_empty() {
        return Ok(None);
    }
    let text = std::str::from_utf8(cursor.as_bytes()).map_err(|_| DiscoveryStoreError::Corrupt)?;
    let Some(name) = text.strip_prefix("pypi-v1:") else {
        // Migrate older serial:offset cursors by restarting the fair scan.
        return Ok(None);
    };
    if name.is_empty()
        || name.len() > 256
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
    {
        return Err(DiscoveryStoreError::Corrupt);
    }
    Ok(Some(name.to_owned()))
}

fn pypi_cursor(name: &str) -> Result<DiscoveryCursor, DiscoveryStoreError> {
    DiscoveryCursor::new(format!("pypi-v1:{name}").into_bytes()).map_err(DiscoveryStoreError::from)
}

fn parse_conan_cursor(
    cursor: &DiscoveryCursor,
) -> Result<(u64, String, usize), DiscoveryStoreError> {
    if cursor.is_empty() {
        return Ok((0, String::new(), 0));
    }
    let value = std::str::from_utf8(cursor.as_bytes()).map_err(|_| DiscoveryStoreError::Corrupt)?;
    if let Some(value) = value.strip_prefix("z-conan-v1:") {
        let (generation, rest) = value.split_once(':').ok_or(DiscoveryStoreError::Corrupt)?;
        let (sha, offset) = rest.split_once(':').ok_or(DiscoveryStoreError::Corrupt)?;
        return Ok((
            generation
                .parse()
                .map_err(|_| DiscoveryStoreError::Corrupt)?,
            sha.to_owned(),
            offset.parse().map_err(|_| DiscoveryStoreError::Corrupt)?,
        ));
    }
    // Migrate the original SHA:offset cursor format. The versioned cursor
    // prefix sorts after hexadecimal SHAs, preserving the journal's monotone
    // byte-order fence when Git moves to a different tree.
    let (sha, offset) = value.split_once(':').ok_or(DiscoveryStoreError::Corrupt)?;
    Ok((
        0,
        sha.to_owned(),
        offset.parse().map_err(|_| DiscoveryStoreError::Corrupt)?,
    ))
}

fn conan_cursor(
    generation: u64,
    sha: &str,
    offset: usize,
) -> Result<DiscoveryCursor, DiscoveryStoreError> {
    DiscoveryCursor::new(format!("z-conan-v1:{generation:020}:{sha}:{offset:020}").into_bytes())
        .map_err(DiscoveryStoreError::from)
}

fn percent_encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[derive(Clone)]
struct CachedSparsePackage {
    etag: Option<String>,
    facts: Vec<DiscoveryFact>,
}

fn refresh_nuget(
    endpoint: &RegistryEndpoint,
    previous: DiscoveryCursor,
    max_pages: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<DiscoveryBatchDraft, DiscoveryStoreError> {
    let source = discovery_source_identity(endpoint);
    let index = fetch_metadata(endpoint.as_str(), 16 * 1024 * 1024, deadline, cancelled)?;
    let plan = parse_nuget_catalog_index(&index, endpoint.as_str(), &previous, max_pages)
        .map_err(DiscoveryStoreError::from)?;
    let mut events = Vec::new();
    for page in &plan.pages {
        let page_bytes = fetch_metadata(&page.url, MAX_SOURCE_BODY_BYTES, deadline, cancelled)?;
        let leaves = parse_nuget_catalog_page(
            &page_bytes,
            endpoint.as_str(),
            &previous,
            &plan.source_high_watermark,
            &page.commit_id,
            MAX_DISCOVERY_PAGE_ITEMS,
        )
        .map_err(DiscoveryStoreError::from)?;
        if leaves.len() != page.count {
            return Err(DiscoveryStoreError::Conflict);
        }
        if events.len().saturating_add(leaves.len()) > MAX_DISCOVERY_PAGE_ITEMS {
            return Err(DiscoveryStoreError::Bounds);
        }
        for leaf in leaves {
            let leaf_bytes = fetch_metadata(&leaf.url, 4 * 1024 * 1024, deadline, cancelled)?;
            let event = parse_nuget_catalog_leaf(&leaf_bytes).map_err(DiscoveryStoreError::from)?;
            if event.commit_timestamp != leaf.commit_timestamp || event.commit_id != leaf.commit_id
            {
                return Err(DiscoveryStoreError::Corrupt);
            }
            events.push(event);
        }
    }
    events.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.coordinate.cmp(&right.coordinate))
    });
    let observed_at = DiscoveryObservedAt::from_unix_millis(discovery_now());
    let facts = events
        .into_iter()
        .map(|event: NugetCatalogEvent| DiscoveryFact {
            source,
            coordinate: event.coordinate,
            standing: event.standing,
            observed_at,
            source_event: DiscoverySourceEvent::NugetCatalog {
                timestamp: event.timestamp,
                commit_id: event.commit_id,
            },
            source_event_time: Some(event.commit_timestamp),
            proof: event.proof,
            metadata: event.metadata,
        })
        .collect();
    let batch = DiscoveryBatchDraft {
        source,
        previous_cursor: plan.previous_cursor,
        next_cursor: plan.next_cursor,
        source_high_watermark: plan.source_high_watermark,
        caught_up: plan.is_caught_up,
        observed_at,
        completeness: plan.completeness,
        facts,
        package_retractions: Vec::new(),
    };
    batch.admit().map_err(DiscoveryStoreError::from)?;
    Ok(batch)
}

fn discovered_package_name(coordinate: &str) -> &str {
    let tail = coordinate.rsplit('/').next().unwrap_or(coordinate);
    tail.split_once('@').map_or(tail, |(name, _)| name)
}

fn search_document(
    source: DiscoverySourceIdentity,
    fact: &DiscoveryFact,
) -> Result<DiscoverySearchDocument, DiscoveryStoreError> {
    let coordinate = backend_engine::registry::admit_registry_coordinate(&fact.coordinate)
        .map_err(|_| DiscoveryStoreError::Decode)?;
    Ok(DiscoverySearchDocument {
        source,
        coordinate: fact.coordinate.clone(),
        lineage: coordinate.qualified_name().as_str().to_owned(),
    })
}

fn discovered_package_qualified_name(fact: &DiscoveryFact) -> Result<String, DiscoveryStoreError> {
    backend_engine::registry::admit_registry_coordinate(&fact.coordinate)
        .map(|coordinate| coordinate.qualified_name().as_str().to_owned())
        .map_err(|_| DiscoveryStoreError::Decode)
}

fn same_search_projection(left: &DiscoveryMetadata, right: &DiscoveryMetadata) -> bool {
    left.aliases == right.aliases
        && left.description == right.description
        && left.keywords == right.keywords
        && left.advisories == right.advisories
        && left.cargo_sparse == right.cargo_sparse
}

fn merge_discovery_metadata(
    existing: DiscoveryMetadata,
    observed: DiscoveryMetadata,
) -> DiscoveryMetadata {
    fn overlay<T>(existing: DiscoveryFacet<T>, observed: DiscoveryFacet<T>) -> DiscoveryFacet<T> {
        match observed {
            DiscoveryFacet::Unknown => existing,
            known_or_absent => known_or_absent,
        }
    }
    DiscoveryMetadata {
        aliases: overlay(existing.aliases, observed.aliases),
        description: overlay(existing.description, observed.description),
        keywords: overlay(existing.keywords, observed.keywords),
        license: overlay(existing.license, observed.license),
        published_at: overlay(existing.published_at, observed.published_at),
        deprecation: overlay(existing.deprecation, observed.deprecation),
        yanked: overlay(existing.yanked, observed.yanked),
        advisories: overlay(existing.advisories, observed.advisories),
        downloads: overlay(existing.downloads, observed.downloads),
        cargo_sparse: overlay(existing.cargo_sparse, observed.cargo_sparse),
    }
}

fn cargo_sparse_base_url(api_root: &str) -> String {
    if let Some(suffix) = api_root.strip_prefix("https://crates.io")
        && (suffix.is_empty() || suffix.starts_with('/'))
    {
        return "https://index.crates.io".to_owned();
    }
    api_root.trim_end_matches('/').to_owned()
}

fn fetch_metadata(
    url: &str,
    maximum: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, DiscoveryStoreError> {
    fetch_metadata_with_accept(
        url,
        maximum,
        deadline,
        cancelled,
        "application/json",
        DISCOVERY_REQUEST_TIMEOUT,
    )
}

fn fetch_metadata_with_accept(
    url: &str,
    maximum: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
    accept: &str,
    request_timeout: Duration,
) -> Result<Vec<u8>, DiscoveryStoreError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(DiscoveryStoreError::Cancelled);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(DiscoveryStoreError::Io(io::Error::other(
            "metadata source refresh budget exhausted",
        )));
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(remaining.min(request_timeout)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .header("accept-encoding", "identity")
        .header("accept", accept)
        .header("user-agent", "backend-index-compiler-discovery/1")
        .call()
        .map_err(|_| DiscoveryStoreError::Io(io::Error::other("metadata request failed")))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(DiscoveryStoreError::Io(io::Error::other(format!(
            "metadata source returned HTTP {status}"
        ))));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(DiscoveryStoreError::Bounds);
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(DiscoveryStoreError::Cancelled);
    }
    Ok(bytes)
}

fn fetch_sparse_metadata(
    url: &str,
    maximum: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
    cached_etag: Option<&str>,
) -> Result<(Option<Vec<u8>>, Option<String>), DiscoveryStoreError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(DiscoveryStoreError::Cancelled);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(DiscoveryStoreError::Io(io::Error::other(
            "metadata source refresh budget exhausted",
        )));
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(remaining.min(DISCOVERY_REQUEST_TIMEOUT)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut request = agent.get(url).header("accept-encoding", "identity");
    if let Some(etag) = cached_etag {
        request = request.header("if-none-match", etag);
    }
    let mut response = request
        .call()
        .map_err(|_| DiscoveryStoreError::Io(io::Error::other("metadata request failed")))?;
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 1024)
        .map(str::to_owned)
        .or_else(|| cached_etag.map(str::to_owned));
    let status = response.status().as_u16();
    if status == 304 {
        return if cached_etag.is_some() {
            Ok((None, etag))
        } else {
            Err(DiscoveryStoreError::Corrupt)
        };
    }
    if !(200..300).contains(&status) {
        return Err(DiscoveryStoreError::Io(io::Error::other(format!(
            "metadata source returned HTTP {status}"
        ))));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(DiscoveryStoreError::Bounds);
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(DiscoveryStoreError::Cancelled);
    }
    Ok((Some(bytes), etag))
}

fn discovery_now() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

impl DiscoveryStore {
    /// Opens or creates a discovery journal and replays complete transactions.
    /// Claims recovered from disk are historical until a later successful
    /// source observation is committed.
    pub(crate) fn open(path: PathBuf) -> Result<Self, DiscoveryStoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut lock_name = path.as_os_str().to_owned();
        lock_name.push(".lock");
        let lock_path = PathBuf::from(lock_name);
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        match lease.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(DiscoveryStoreError::Busy),
            Err(TryLockError::Error(error)) => return Err(DiscoveryStoreError::Io(error)),
        }
        let journal = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        let mut store = Self {
            path,
            sources: BTreeMap::new(),
            journal,
            _lease: lease,
            poisoned: false,
            search_revision: 0,
            observation_revision: 0,
            search_changes: VecDeque::new(),
        };
        let valid_end = store.replay()?;
        for source in store.sources.values_mut() {
            source.historical = true;
        }
        let file_len = store.journal.metadata()?.len();
        if valid_end < file_len {
            store.journal.set_len(valid_end)?;
            store.journal.sync_all()?;
        }
        store.journal.seek(SeekFrom::End(0))?;
        Ok(store)
    }

    /// Stable sibling directory for a rebuildable durable search projection.
    ///
    /// The journal remains the authority. A projection can be deleted and
    /// rebuilt from this store, but it must live beside the journal so a
    /// restart does not silently create a new ephemeral index each time.
    pub(crate) fn search_projection_path(&self) -> PathBuf {
        self.path.with_file_name("catalog-search-v1")
    }

    /// Atomically records facts and their cursor transition as one durable
    /// append transaction. The source cursor is checked against the current
    /// owner state before any bytes are written.
    pub(crate) fn commit(
        &mut self,
        batch: DiscoveryBatch,
    ) -> Result<DiscoveryCommit, DiscoveryStoreError> {
        if self.poisoned {
            return Err(DiscoveryStoreError::Corrupt);
        }
        let transaction_payload_bytes = batch.admitted_transaction_encoded_len()?;
        let input_fingerprint = batch_fingerprint(&batch)?;
        if self
            .sources
            .get(&batch.source)
            .is_some_and(|state| state.last_batch_fingerprint == Some(input_fingerprint))
        {
            return Ok(DiscoveryCommit::AlreadyCommitted);
        }
        let current = self
            .sources
            .get(&batch.source)
            .map_or_else(DiscoveryCursor::default, |state| state.cursor.clone());
        if self.sequence(batch.source).unwrap_or(0) != batch.expected_base_sequence
            || current != batch.previous_cursor
        {
            return Err(DiscoveryStoreError::Conflict);
        }
        let materialized = self.materialize_package_retractions(&batch)?;
        let prepared_apply = self.validate_fact_transition(materialized.as_ref())?;
        let next_search_revision = self
            .search_revision
            .checked_add(
                u64::try_from(prepared_apply.search_update_count)
                    .map_err(|_| DiscoveryStoreError::Bounds)?,
            )
            .ok_or(DiscoveryStoreError::Bounds)?;
        let next_observation_revision = self
            .observation_revision
            .checked_add(1)
            .ok_or(DiscoveryStoreError::Bounds)?;
        let previous_search_revision = self.search_revision;

        if transaction_payload_bytes > MAX_FRAME_BYTES {
            return Err(DiscoveryStoreError::Bounds);
        }
        let frame_payload_length = u32::try_from(transaction_payload_bytes)
            .map_err(|_| DiscoveryStoreError::Bounds)?
            .to_be_bytes();
        let start = self.journal.seek(SeekFrom::End(0))?;
        let durable_result = (|| -> Result<(), DiscoveryStoreError> {
            self.journal.write_all(JOURNAL_MAGIC)?;
            self.journal.write_all(&JOURNAL_VERSION.to_be_bytes())?;
            self.journal.write_all(&frame_payload_length)?;
            let checksum = {
                let mut payload_writer = FramePayloadWriter::new(&mut self.journal);
                serde_json::to_writer(
                    &mut payload_writer,
                    &JournalWriteTransaction {
                        version: JOURNAL_VERSION,
                        batch: &batch,
                    },
                )
                .map_err(map_json_write_error)?;
                payload_writer.finish()
            };
            self.journal.write_all(&checksum)?;
            self.journal.sync_all()?;
            durable::sync_parent(&self.path)?;
            Ok(())
        })();
        if let Err(error) = durable_result {
            if self.journal.set_len(start).is_err()
                || self.journal.sync_all().is_err()
                || durable::sync_parent(&self.path).is_err()
                || self.journal.seek(SeekFrom::End(0)).is_err()
            {
                self.poisoned = true;
            }
            return Err(error.into());
        }
        let applied_batch = match materialized {
            MaterializedDiscoveryBatch::Borrowed(_) => batch,
            MaterializedDiscoveryBatch::Owned(batch) => batch,
        };
        let search_documents = self.apply(applied_batch, input_fingerprint, prepared_apply);
        self.search_revision = next_search_revision;
        self.observation_revision = next_observation_revision;
        self.record_search_changes(search_documents, previous_search_revision);
        Ok(DiscoveryCommit::Committed)
    }

    /// Current durable source cursor.
    pub(crate) fn cursor(&self, source: DiscoverySourceIdentity) -> DiscoveryCursor {
        self.sources
            .get(&source)
            .map_or_else(DiscoveryCursor::default, |state| state.cursor.clone())
    }

    /// Local monotonic source progress captured before starting another feed
    /// request. Cursors remain adapter-owned hints and do not replace this CAS.
    pub(crate) fn sequence(&self, source: DiscoverySourceIdentity) -> Option<u64> {
        self.sources.get(&source).map(|state| state.sequence)
    }

    fn progress_token(&self, source: DiscoverySourceIdentity) -> DiscoveryProgressToken {
        DiscoveryProgressToken {
            sequence: self.sequence(source).unwrap_or(0),
            cursor: self.cursor(source),
        }
    }

    /// Completeness last committed for this source, if it has been observed.
    pub(crate) fn completeness(
        &self,
        source: DiscoverySourceIdentity,
    ) -> Option<DiscoveryCompleteness> {
        self.sources
            .get(&source)
            .and_then(|state| state.completeness)
    }

    /// Whether the currently selected source claims come from a live
    /// observation in this process. Reopened journal facts are historical.
    pub(crate) fn is_historical(&self, source: DiscoverySourceIdentity) -> bool {
        self.sources
            .get(&source)
            .is_none_or(|state| state.historical)
    }

    /// Whether the last poll attempt failed while durable claims remain
    /// available for search.
    pub(crate) fn is_unavailable(&self, source: DiscoverySourceIdentity) -> bool {
        self.sources
            .get(&source)
            .is_some_and(|state| state.refresh_failed)
    }

    fn mark_failed(&mut self, source: DiscoverySourceIdentity, failed: bool) {
        self.sources.entry(source).or_default().refresh_failed = failed;
    }

    /// Iterates distinct source claims without constructing a merged catalog.
    pub(crate) fn facts(&self) -> impl Iterator<Item = (&DiscoverySourceIdentity, &DiscoveryFact)> {
        self.sources
            .iter()
            .flat_map(|(source, state)| state.facts.values().map(move |fact| (source, fact)))
    }

    /// Last local wall-clock observation for one source.
    pub(crate) fn latest_observed_at(&self, source: DiscoverySourceIdentity) -> Option<u64> {
        self.sources
            .get(&source)
            .and_then(|state| state.latest_observed_at)
    }

    /// Revision of the immutable search key/text projection. Mutating standing,
    /// proof, cursor, or observation time does not bump it.
    pub(crate) const fn search_revision(&self) -> u64 {
        self.search_revision
    }

    /// Durable observation sequence, including overlay-only standing and
    /// freshness changes. Replayed frames recover the same sequence.
    pub(crate) const fn observation_revision(&self) -> u64 {
        self.observation_revision
    }

    /// Returns bounded structural search deltas since `revision`. `None`
    /// means the caller fell behind the retained window and should rebuild.
    pub(crate) fn search_changes_since(
        &self,
        revision: u64,
    ) -> Option<(u64, Vec<DiscoverySearchDocument>)> {
        if revision > self.search_revision {
            return None;
        }
        if revision == self.search_revision {
            return Some((self.search_revision, Vec::new()));
        }
        let earliest = self.search_changes.front()?.revision;
        if revision.saturating_add(1) < earliest {
            return None;
        }
        Some((
            self.search_revision,
            self.search_changes
                .iter()
                .filter(|change| change.revision > revision)
                .map(|change| change.document.clone())
                .collect(),
        ))
    }

    pub(crate) fn fact(
        &self,
        source: DiscoverySourceIdentity,
        coordinate: &str,
    ) -> Option<&DiscoveryFact> {
        self.sources
            .get(&source)
            .and_then(|progress| progress.facts.get(coordinate))
    }

    /// Whether this source has reached its last captured high watermark.
    pub(crate) fn is_caught_up(&self, source: DiscoverySourceIdentity) -> bool {
        self.sources
            .get(&source)
            .is_some_and(|progress| progress.caught_up)
    }

    fn replay(&mut self) -> Result<u64, DiscoveryStoreError> {
        self.journal.seek(SeekFrom::Start(0))?;
        let mut offset = 0_u64;
        loop {
            let mut header = [0_u8; 14];
            match self.journal.read_exact(&mut header) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(offset),
                Err(error) => return Err(error.into()),
            }
            if &header[..8] != JOURNAL_MAGIC {
                return Err(DiscoveryStoreError::Corrupt);
            }
            let version = u16::from_be_bytes([header[8], header[9]]);
            if version != JOURNAL_VERSION {
                return Err(DiscoveryStoreError::UnsupportedVersion(version));
            }
            let length =
                u32::from_be_bytes([header[10], header[11], header[12], header[13]]) as usize;
            if length > MAX_FRAME_BYTES {
                return Err(DiscoveryStoreError::Bounds);
            }
            let mut payload = vec![0_u8; length];
            let mut checksum = [0_u8; 32];
            if self.journal.read_exact(&mut payload).is_err()
                || self.journal.read_exact(&mut checksum).is_err()
            {
                return Ok(offset);
            }
            if checksum != frame_checksum(&payload) {
                return Err(DiscoveryStoreError::Corrupt);
            }
            let transaction: JournalTransaction =
                serde_json::from_slice(&payload).map_err(|_| DiscoveryStoreError::Corrupt)?;
            if transaction.version != JOURNAL_VERSION {
                return Err(DiscoveryStoreError::UnsupportedVersion(transaction.version));
            }
            transaction
                .batch
                .admit()
                .map_err(|_| DiscoveryStoreError::Corrupt)?;
            self.apply_recovered(transaction.batch)?;
            offset = offset
                .checked_add(
                    u64::try_from(header.len() + length + checksum.len())
                        .map_err(|_| DiscoveryStoreError::Bounds)?,
                )
                .ok_or(DiscoveryStoreError::Bounds)?;
        }
    }

    fn apply_recovered(&mut self, batch: DiscoveryBatch) -> Result<(), DiscoveryStoreError> {
        let expected = self.cursor(batch.source);
        if batch.previous_cursor != expected
            || self.sequence(batch.source).unwrap_or(0) != batch.expected_base_sequence
        {
            return Err(DiscoveryStoreError::Corrupt);
        }
        let input_fingerprint = batch_fingerprint(&batch)?;
        let materialized = self.materialize_package_retractions(&batch)?;
        let prepared_apply = self.validate_fact_transition(materialized.as_ref())?;
        let next_search_revision = self
            .search_revision
            .checked_add(
                u64::try_from(prepared_apply.search_update_count)
                    .map_err(|_| DiscoveryStoreError::Bounds)?,
            )
            .ok_or(DiscoveryStoreError::Bounds)?;
        let next_observation_revision = self
            .observation_revision
            .checked_add(1)
            .ok_or(DiscoveryStoreError::Bounds)?;
        let previous_search_revision = self.search_revision;
        let applied_batch = match materialized {
            MaterializedDiscoveryBatch::Borrowed(_) => batch,
            MaterializedDiscoveryBatch::Owned(batch) => batch,
        };
        let search_documents = self.apply(applied_batch, input_fingerprint, prepared_apply);
        self.search_revision = next_search_revision;
        self.observation_revision = next_observation_revision;
        self.record_search_changes(search_documents, previous_search_revision);
        Ok(())
    }

    fn apply(
        &mut self,
        batch: DiscoveryBatch,
        fingerprint: [u8; 32],
        prepared: PreparedDiscoveryApply,
    ) -> Vec<DiscoverySearchDocument> {
        let PreparedDiscoveryApply {
            next_sequence,
            actions,
            mut search_documents,
            ..
        } = prepared;
        let source_identity = batch.source;
        let source = self.sources.entry(source_identity).or_default();
        let mut actions = actions.into_iter().peekable();
        for (fact_index, fact) in batch.facts.into_iter().enumerate() {
            if actions
                .peek()
                .is_some_and(|action| action.fact_index == fact_index)
            {
                let Some(action) = actions.next() else {
                    continue;
                };
                match action.transition {
                    DiscoveryFactTransition::Newer => {
                        if let (Some(package_name), Some(release_key)) =
                            (action.package_name, action.package_release_key)
                        {
                            source
                                .package_releases
                                .entry(package_name)
                                .or_default()
                                .insert(release_key);
                        }
                        source.facts.insert(action.key, fact);
                    }
                    DiscoveryFactTransition::Identical => {
                        if let Some(selected) = source.facts.get_mut(&action.key) {
                            selected.observed_at = fact.observed_at;
                        }
                    }
                    DiscoveryFactTransition::Stale => {}
                }
                if let Some(document) = action.search_document {
                    search_documents.push(document);
                }
            }
        }
        source.sequence = next_sequence;
        source.cursor = batch.next_cursor;
        source.source_high_watermark = batch.source_high_watermark;
        source.caught_up = batch.caught_up;
        source.completeness = Some(batch.completeness);
        source.latest_observed_at = Some(batch.observed_at.as_unix_millis());
        source.historical = false;
        source.refresh_failed = false;
        source.last_batch_fingerprint = Some(fingerprint);
        search_documents
    }

    fn materialize_package_retractions<'batch>(
        &self,
        batch: &'batch DiscoveryBatch,
    ) -> Result<MaterializedDiscoveryBatch<'batch>, DiscoveryStoreError> {
        if batch.package_retractions.is_empty() {
            return Ok(MaterializedDiscoveryBatch::Borrowed(batch));
        }
        self.preflight_package_retraction_materialization(batch)?;
        if batch.source.ecosystem() != RegistryEcosystem::Npm {
            return Err(DiscoveryStoreError::Decode);
        }
        let mut materialized = batch.clone();
        let Some(source) = self.sources.get(&batch.source) else {
            materialized.package_retractions.clear();
            return Ok(MaterializedDiscoveryBatch::Owned(materialized));
        };
        for retraction in &batch.package_retractions {
            let Some(coordinates) = source.package_releases.get(&retraction.package_name) else {
                continue;
            };
            if materialized.facts.len().saturating_add(coordinates.len()) > MAX_DISCOVERY_PAGE_ITEMS
            {
                return Err(DiscoveryStoreError::Bounds);
            }
            for coordinate in coordinates {
                let previous = source
                    .facts
                    .get(coordinate)
                    .ok_or(DiscoveryStoreError::Corrupt)?;
                let mut withdrawn = previous.clone();
                withdrawn.standing = DiscoveryStanding::Withdrawn;
                withdrawn.observed_at = batch.observed_at;
                withdrawn.source_event_time = Some(retraction.source_event_time.clone());
                withdrawn.source_event = retraction.source_event.clone();
                withdrawn.proof = retraction.proof;
                materialized.facts.push(withdrawn);
            }
        }
        materialized.package_retractions.clear();
        materialized.admit().map_err(DiscoveryStoreError::from)?;
        Ok(MaterializedDiscoveryBatch::Owned(materialized))
    }

    fn preflight_package_retraction_materialization(
        &self,
        batch: &DiscoveryBatch,
    ) -> Result<(), DiscoveryStoreError> {
        let input_bytes = batch.admitted_transaction_encoded_len()?;
        let Some(source) = self.sources.get(&batch.source) else {
            return Ok(());
        };
        let mut expanded_upper_bound = input_bytes;
        let mut expanded_facts = 0usize;
        for retraction in &batch.package_retractions {
            let Some(coordinates) = source.package_releases.get(&retraction.package_name) else {
                continue;
            };
            expanded_facts = expanded_facts
                .checked_add(coordinates.len())
                .ok_or(DiscoveryStoreError::Bounds)?;
            if batch.facts.len().saturating_add(expanded_facts) > MAX_DISCOVERY_PAGE_ITEMS {
                return Err(DiscoveryStoreError::Bounds);
            }
            let event_bytes = serialized_value_len_bounded(&retraction.source_event)?;
            let event_time_bytes = serialized_value_len_bounded(&retraction.source_event_time)?;
            for coordinate in coordinates {
                let previous = source
                    .facts
                    .get(coordinate)
                    .ok_or(DiscoveryStoreError::Corrupt)?;
                let previous_fact_bytes = serialized_value_len_bounded(previous)?;
                // The expanded fact preserves all bounded metadata from its
                // prior selected version. The new event and exact source-time
                // spelling replace old fields; counting both complete values
                // and a fixed JSON/array margin is deliberately conservative.
                let addition = previous_fact_bytes
                    .checked_add(event_bytes)
                    .and_then(|bytes| bytes.checked_add(event_time_bytes))
                    .and_then(|bytes| bytes.checked_add(128))
                    .ok_or(DiscoveryStoreError::Bounds)?;
                expanded_upper_bound = expanded_upper_bound
                    .checked_add(addition)
                    .ok_or(DiscoveryStoreError::Bounds)?;
                if expanded_upper_bound > MAX_FRAME_BYTES {
                    return Err(DiscoveryStoreError::Bounds);
                }
            }
        }
        // `batch` remains live while the materialized copy is built and later
        // written. Both encoded-size upper bounds are checked before either
        // the batch clone or any retained metadata clone is allocated.
        let aggregate_upper_bound = input_bytes
            .checked_add(expanded_upper_bound)
            .ok_or(DiscoveryStoreError::Bounds)?;
        if aggregate_upper_bound > MAX_FRAME_BYTES.saturating_mul(2) {
            return Err(DiscoveryStoreError::Bounds);
        }
        Ok(())
    }

    fn record_search_changes(
        &mut self,
        documents: Vec<DiscoverySearchDocument>,
        previous_revision: u64,
    ) {
        for (offset, document) in documents.into_iter().enumerate() {
            let revision = previous_revision
                .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX))
                .saturating_add(1);
            self.search_changes
                .push_back(DiscoverySearchChange { revision, document });
            if self.search_changes.len() > MAX_SEARCH_DELTA_ROWS {
                self.search_changes.pop_front();
            }
        }
    }

    fn validate_fact_transition(
        &self,
        batch: &DiscoveryBatch,
    ) -> Result<PreparedDiscoveryApply, DiscoveryStoreError> {
        let next_sequence = batch
            .expected_base_sequence
            .checked_add(1)
            .ok_or(DiscoveryStoreError::Bounds)?;
        let existing = self.sources.get(&batch.source);
        let mut pending = BTreeMap::<&str, (usize, &DiscoveryFact)>::new();
        for (fact_index, fact) in batch.facts.iter().enumerate() {
            let key = fact.coordinate.as_str();
            let previous = pending
                .get(key)
                .map(|(_, fact)| *fact)
                .or_else(|| existing.and_then(|state| state.facts.get(key)));
            let transition = match previous {
                Some(previous) => classify_fact_transition(previous, fact)?,
                None => DiscoveryFactTransition::Newer,
            };
            if transition != DiscoveryFactTransition::Stale {
                pending.insert(key, (fact_index, fact));
            }
        }
        let new_coordinates = pending
            .keys()
            .copied()
            .filter(|key| existing.is_none_or(|state| !state.facts.contains_key(*key)))
            .count();
        if existing
            .map_or(0, |state| state.facts.len())
            .saturating_add(new_coordinates)
            > MAX_FACTS_PER_SOURCE
        {
            return Err(DiscoveryStoreError::Bounds);
        }
        let mut actions = Vec::with_capacity(pending.len());
        for (key, (fact_index, incoming)) in pending {
            let previous = existing.and_then(|state| state.facts.get(key));
            let transition = match previous {
                Some(previous) => classify_fact_transition(previous, incoming)?,
                None => DiscoveryFactTransition::Newer,
            };
            if transition == DiscoveryFactTransition::Stale {
                continue;
            }
            let search_document = if previous.is_none_or(|previous| {
                !same_search_projection(&previous.metadata, &incoming.metadata)
            }) {
                Some(search_document(batch.source, incoming)?)
            } else {
                None
            };
            let package_name = if previous.is_none() {
                Some(discovered_package_qualified_name(incoming)?)
            } else {
                None
            };
            actions.push(PreparedFactAction {
                fact_index,
                key: key.to_owned(),
                transition,
                package_name,
                package_release_key: previous.is_none().then(|| key.to_owned()),
                search_document,
            });
        }
        actions.sort_by_key(|action| action.fact_index);
        let search_update_count = actions
            .iter()
            .filter(|action| action.search_document.is_some())
            .count();
        Ok(PreparedDiscoveryApply {
            next_sequence,
            actions,
            search_update_count,
            search_documents: Vec::with_capacity(search_update_count),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalTransaction {
    version: u16,
    batch: DiscoveryBatch,
}

#[derive(Serialize)]
struct JournalWriteTransaction<'a> {
    version: u16,
    batch: &'a DiscoveryBatch,
}

struct FramePayloadWriter<'a> {
    journal: &'a mut File,
    hasher: blake3::Hasher,
}

struct BoundedCountingWriter {
    bytes: usize,
    limit: usize,
}

impl Write for BoundedCountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(bytes) = self.bytes.checked_add(buffer.len()) else {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "discovery serialization bound",
            ));
        };
        if bytes > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "discovery serialization bound",
            ));
        }
        self.bytes = bytes;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_value_len_bounded<T: Serialize>(value: &T) -> Result<usize, DiscoveryStoreError> {
    let mut writer = BoundedCountingWriter {
        bytes: 0,
        limit: MAX_FRAME_BYTES,
    };
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(writer.bytes),
        Err(error) if error.io_error_kind() == Some(io::ErrorKind::WriteZero) => {
            Err(DiscoveryStoreError::Bounds)
        }
        Err(_) => Err(DiscoveryStoreError::Decode),
    }
}

impl<'a> FramePayloadWriter<'a> {
    fn new(journal: &'a mut File) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.registry.discovery.transaction.v2\0");
        Self { journal, hasher }
    }

    fn finish(self) -> [u8; 32] {
        *self.hasher.finalize().as_bytes()
    }
}

impl Write for FramePayloadWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.journal.write_all(buffer)?;
        self.hasher.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.journal.flush()
    }
}

fn map_json_write_error(error: serde_json::Error) -> DiscoveryStoreError {
    match error.io_error_kind() {
        Some(kind) => DiscoveryStoreError::Io(io::Error::new(kind, error)),
        None => DiscoveryStoreError::Decode,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiscoveryFactTransition {
    Identical,
    Stale,
    Newer,
}

fn classify_fact_transition(
    previous: &DiscoveryFact,
    incoming: &DiscoveryFact,
) -> Result<DiscoveryFactTransition, DiscoveryStoreError> {
    let same_content = || {
        previous.source == incoming.source
            && previous.coordinate == incoming.coordinate
            && previous.standing == incoming.standing
            && previous.source_event == incoming.source_event
            && previous.source_event_time == incoming.source_event_time
            && previous.proof == incoming.proof
            && previous.metadata == incoming.metadata
    };
    match (&previous.source_event, &incoming.source_event) {
        (DiscoverySourceEvent::Snapshot, DiscoverySourceEvent::NpmChange { .. })
        | (DiscoverySourceEvent::Snapshot, DiscoverySourceEvent::NugetCatalog { .. }) => {
            Ok(DiscoveryFactTransition::Newer)
        }
        (DiscoverySourceEvent::NpmChange { .. }, DiscoverySourceEvent::Snapshot)
        | (DiscoverySourceEvent::NugetCatalog { .. }, DiscoverySourceEvent::Snapshot) => {
            Err(DiscoveryStoreError::Conflict)
        }
        (
            DiscoverySourceEvent::NugetCatalog {
                timestamp: old_timestamp,
                commit_id: old_commit,
            },
            DiscoverySourceEvent::NugetCatalog {
                timestamp: new_timestamp,
                commit_id: new_commit,
            },
        ) => {
            if old_commit == new_commit {
                if old_timestamp != new_timestamp || !same_content() {
                    return Err(DiscoveryStoreError::Conflict);
                }
                return Ok(DiscoveryFactTransition::Identical);
            }
            match new_timestamp.cmp(old_timestamp) {
                std::cmp::Ordering::Greater => Ok(DiscoveryFactTransition::Newer),
                std::cmp::Ordering::Less => Ok(DiscoveryFactTransition::Stale),
                std::cmp::Ordering::Equal => Err(DiscoveryStoreError::Conflict),
            }
        }
        (
            DiscoverySourceEvent::NpmChange {
                sequence: old_sequence,
                ..
            },
            DiscoverySourceEvent::NpmChange {
                sequence: new_sequence,
                ..
            },
        ) => match new_sequence.cmp(old_sequence) {
            std::cmp::Ordering::Greater => Ok(DiscoveryFactTransition::Newer),
            std::cmp::Ordering::Less => Ok(DiscoveryFactTransition::Stale),
            std::cmp::Ordering::Equal if same_content() => Ok(DiscoveryFactTransition::Identical),
            std::cmp::Ordering::Equal => Err(DiscoveryStoreError::Conflict),
        },
        (
            DiscoverySourceEvent::Unordered | DiscoverySourceEvent::Snapshot,
            DiscoverySourceEvent::Unordered | DiscoverySourceEvent::Snapshot,
        ) => {
            if same_content() {
                Ok(DiscoveryFactTransition::Identical)
            } else {
                // Unsequenced snapshots do not establish source-event order.
                // Their only freshness fence is the exact local source CAS
                // checked by `commit`, so a later accepted observation may
                // replace any prior unsequenced snapshot for every ecosystem.
                Ok(DiscoveryFactTransition::Newer)
            }
        }
        _ => Err(DiscoveryStoreError::Conflict),
    }
}

#[cfg(test)]
fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, DiscoveryStoreError> {
    let length = u32::try_from(payload.len()).map_err(|_| DiscoveryStoreError::Bounds)?;
    let mut frame = Vec::with_capacity(14 + payload.len() + 32);
    frame.extend_from_slice(JOURNAL_MAGIC);
    frame.extend_from_slice(&JOURNAL_VERSION.to_be_bytes());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&frame_checksum(payload));
    Ok(frame)
}

fn frame_checksum(payload: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.registry.discovery.transaction.v2\0");
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

fn batch_fingerprint(batch: &DiscoveryBatch) -> Result<[u8; 32], DiscoveryStoreError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.registry.discovery.batch.v1\0");
    serde_json::to_writer(&mut Blake3Writer(&mut hasher), batch)
        .map_err(|_| DiscoveryStoreError::Decode)?;
    Ok(*hasher.finalize().as_bytes())
}

struct Blake3Writer<'a>(&'a mut blake3::Hasher);

impl Write for Blake3Writer<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::registry::{DiscoveryObservedAt, RegistryEcosystem, RegistryEndpoint};

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "backend-registry-discovery-{name}-{}-{}.journal",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn source() -> DiscoverySourceIdentity {
        let endpoint = RegistryEndpoint::new(
            RegistryEcosystem::Nuget,
            "https://api.nuget.org/v3/index.json",
        )
        .expect("admitted source");
        discovery_source_identity(&endpoint)
    }

    fn batch(
        previous: DiscoveryCursor,
        next: DiscoveryCursor,
        version: &str,
        status: DiscoveryStanding,
        event_time: &str,
    ) -> DiscoveryBatch {
        let source = source();
        let observed = DiscoveryObservedAt::from_unix_millis(100);
        DiscoveryBatch {
            source,
            expected_base_sequence: 0,
            previous_cursor: previous,
            next_cursor: next.clone(),
            source_high_watermark: next,
            caught_up: true,
            observed_at: observed,
            completeness: DiscoveryCompleteness::CompleteThroughCursor,
            facts: vec![DiscoveryFact {
                source,
                coordinate: backend_engine::ProductPackageCoordinate::parse(format!(
                    "pkg:nuget/Widget@{version}"
                ))
                .expect("coordinate"),
                standing: status,
                observed_at: observed,
                source_event: DiscoverySourceEvent::NugetCatalog {
                    timestamp: DiscoveryTimestamp::parse_nuget_catalog_timestamp(event_time)
                        .expect("NuGet event timestamp"),
                    commit_id: format!("test-{event_time}"),
                },
                source_event_time: Some(event_time.to_owned()),
                proof: [u8::from(status as u8); 32],
                metadata: DiscoveryMetadata::default(),
            }],
            package_retractions: Vec::new(),
        }
    }

    fn fence_batch(
        mut batch: DiscoveryBatch,
        expected_base_sequence: u64,
        previous_cursor: DiscoveryCursor,
        next_cursor: DiscoveryCursor,
        observed_at: u64,
    ) -> DiscoveryBatch {
        let observed_at = DiscoveryObservedAt::from_unix_millis(observed_at);
        batch.expected_base_sequence = expected_base_sequence;
        batch.previous_cursor = previous_cursor;
        batch.next_cursor = next_cursor.clone();
        batch.source_high_watermark = next_cursor;
        batch.caught_up = true;
        batch.observed_at = observed_at;
        for fact in &mut batch.facts {
            fact.observed_at = observed_at;
        }
        batch
    }

    fn event_fact(
        ecosystem: RegistryEcosystem,
        event: DiscoverySourceEvent,
        source_event_time: Option<&str>,
        standing: DiscoveryStanding,
    ) -> DiscoveryFact {
        let source = DiscoverySourceIdentity::from_parts(ecosystem, [31; 32]);
        let package_type = match ecosystem {
            RegistryEcosystem::Npm => "npm",
            RegistryEcosystem::Nuget => "nuget",
            RegistryEcosystem::Cargo => "cargo",
            RegistryEcosystem::Pypi => "pypi",
            RegistryEcosystem::Golang => "golang",
            RegistryEcosystem::Maven => "maven",
            RegistryEcosystem::Cpp => "generic",
        };
        DiscoveryFact {
            source,
            coordinate: backend_engine::ProductPackageCoordinate::parse(format!(
                "pkg:{package_type}/example@1.0.0"
            ))
            .expect("coordinate"),
            standing,
            observed_at: DiscoveryObservedAt::from_unix_millis(10),
            source_event: event,
            source_event_time: source_event_time.map(str::to_owned),
            proof: [7; 32],
            metadata: DiscoveryMetadata::default(),
        }
    }

    fn snapshot_batch(
        ecosystem: RegistryEcosystem,
        expected_base_sequence: u64,
        previous_cursor: DiscoveryCursor,
        next_cursor: DiscoveryCursor,
        observed_at: u64,
        standing: DiscoveryStanding,
        description: &str,
    ) -> DiscoveryBatch {
        let (endpoint, coordinate) = match ecosystem {
            RegistryEcosystem::Npm => ("https://registry.npmjs.org", "pkg:npm/example@1.0.0"),
            RegistryEcosystem::Nuget => (
                "https://api.nuget.org/v3/index.json",
                "pkg:nuget/Widget@1.0.0",
            ),
            _ => panic!("snapshot fixture requires npm or NuGet"),
        };
        let endpoint = RegistryEndpoint::new(ecosystem, endpoint).expect("admitted source");
        let source = discovery_source_identity(&endpoint);
        let observed_at = DiscoveryObservedAt::from_unix_millis(observed_at);
        DiscoveryBatch {
            source,
            expected_base_sequence,
            previous_cursor,
            next_cursor: next_cursor.clone(),
            source_high_watermark: next_cursor,
            caught_up: true,
            observed_at,
            completeness: DiscoveryCompleteness::CompleteThroughCursor,
            facts: vec![DiscoveryFact {
                source,
                coordinate: backend_engine::ProductPackageCoordinate::parse(coordinate)
                    .expect("coordinate"),
                standing,
                observed_at,
                source_event: DiscoverySourceEvent::Snapshot,
                source_event_time: None,
                proof: [17; 32],
                metadata: DiscoveryMetadata {
                    description: DiscoveryFacet::Known(description.to_owned()),
                    ..DiscoveryMetadata::default()
                },
            }],
            package_retractions: Vec::new(),
        }
    }

    fn cursor(value: &str) -> DiscoveryCursor {
        DiscoveryCursor::new(value.as_bytes().to_vec()).expect("cursor")
    }

    #[test]
    fn pypi_global_project_index_download_exceeds_package_metadata_budget() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        listener
            .set_nonblocking(true)
            .expect("bounded fixture accept");
        let address = listener.local_addr().expect("fixture address");
        let mut project_index = br#"{"meta":{},"projects":[{"name":"requests"}]}"#.to_vec();
        project_index.resize(MAX_SOURCE_BODY_BYTES + 1, b' ');
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for body in [
                project_index,
                br#"{"info":{"name":"requests","version":"2.32.5"},"releases":{"2.32.5":[{"yanked":false}]}}"#.to_vec(),
            ] {
                let accept_deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            if Instant::now() >= accept_deadline {
                                return requests;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("fixture request: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("bounded fixture read");
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).expect("read fixture request");
                    request.push(byte[0]);
                    assert!(request.len() <= 16 * 1024, "bounded fixture headers");
                }
                requests.push(String::from_utf8(request).expect("HTTP request"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                )
                .expect("write fixture headers");
                if stream.write_all(&body).is_err() {
                    return requests;
                }
            }
            requests
        });
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Pypi, format!("http://{address}"))
            .expect("loopback PyPI source");
        let cancelled = AtomicBool::new(false);
        let batch = refresh_pypi(
            &endpoint,
            DiscoveryCursor::default(),
            1,
            Instant::now() + Duration::from_secs(20),
            &cancelled,
        );
        let requests = server.join().expect("fixture server");
        let batch = batch.expect("full project index, bounded package metadata");
        assert_eq!(batch.facts.len(), 1);
        assert_eq!(
            batch.facts[0].coordinate.as_str(),
            "pkg:pypi/requests@2.32.5"
        );
        assert_eq!(batch.completeness, DiscoveryCompleteness::Windowed);
        assert_eq!(batch.next_cursor.as_bytes(), b"pypi-v1:requests");
        assert!(requests[0].starts_with("GET /simple/ "));
        assert!(requests[0].contains("application/vnd.pypi.simple.v1+json"));
        assert!(requests[1].starts_with("GET /pypi/requests/json "));
    }

    #[test]
    fn maven_window_restarts_at_the_newest_page_after_an_insertion() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let snapshots = [
            br#"{"response":{"numFound":2,"docs":[{"g":"org.example","a":"alpha","latestVersion":"1.0.0","timestamp":100},{"g":"org.example","a":"beta","latestVersion":"1.0.0","timestamp":90}]}}"#.to_vec(),
            br#"{"response":{"numFound":3,"docs":[{"g":"org.example","a":"newest","latestVersion":"1.0.0","timestamp":110},{"g":"org.example","a":"alpha","latestVersion":"1.0.0","timestamp":100},{"g":"org.example","a":"beta","latestVersion":"1.0.0","timestamp":90}]}}"#.to_vec(),
        ];
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for body in snapshots {
                let (mut stream, _) = listener.accept().expect("fixture request");
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).expect("read fixture request");
                requests.push(String::from_utf8_lossy(&request[..read]).to_string());
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                )
                .expect("write fixture headers");
                stream.write_all(&body).expect("write fixture body");
            }
            requests
        });
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Maven, format!("http://{address}"))
            .expect("loopback Maven source");
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = refresh_maven(
            &endpoint,
            DiscoveryCursor::default(),
            8,
            deadline,
            &cancelled,
        )
        .expect("first newest window");
        assert_eq!(first.completeness, DiscoveryCompleteness::Windowed);
        assert!(!first.caught_up);
        assert_eq!(first.facts.len(), 2);
        let second = refresh_maven(
            &endpoint,
            first.next_cursor.clone(),
            8,
            deadline,
            &cancelled,
        )
        .expect("newest window after source insertion");
        assert!(second.next_cursor.as_bytes() > first.next_cursor.as_bytes());

        // Independent source oracle: the new artifact is now first in the
        // timestamp-descending snapshot and must be present after the poll.
        let expected = vec![
            "pkg:maven/org.example/newest@1.0.0",
            "pkg:maven/org.example/alpha@1.0.0",
            "pkg:maven/org.example/beta@1.0.0",
        ];
        let actual = second
            .facts
            .iter()
            .map(|fact| fact.coordinate.as_str())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        let requests = server.join().expect("fixture server");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.contains("start=0")));
    }

    #[test]
    fn maven_observation_cursor_is_deterministic_and_migrates_numeric_offsets() {
        let initial = next_maven_window_cursor(&DiscoveryCursor::default()).expect("initial");
        let second = next_maven_window_cursor(&initial).expect("second");
        let migrated = next_maven_window_cursor(&cursor("100")).expect("legacy migration");
        assert!(second.as_bytes() > initial.as_bytes());
        assert!(migrated.as_bytes() > &b"100"[..]);
        assert_eq!(
            initial.as_bytes(),
            next_maven_window_cursor(&DiscoveryCursor::default())
                .expect("repeat initial")
                .as_bytes()
        );
    }

    #[test]
    fn source_facts_and_cursor_recover_together_and_are_historical() {
        let path = temp_path("reopen");
        let first = batch(
            DiscoveryCursor::default(),
            cursor("2026-09-01T00:00:00Z"),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let source = first.source;
        {
            let mut store = DiscoveryStore::open(path.clone()).expect("open");
            assert_eq!(
                store.commit(first).expect("commit"),
                DiscoveryCommit::Committed
            );
            assert_eq!(store.cursor(source), cursor("2026-09-01T00:00:00Z"));
            assert!(!store.is_historical(source));
        }
        let store = DiscoveryStore::open(path.clone()).expect("reopen");
        assert_eq!(store.cursor(source), cursor("2026-09-01T00:00:00Z"));
        assert!(store.is_historical(source));
        assert_eq!(store.facts().count(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replayed_batch_is_idempotent_and_does_not_append_a_second_frame() {
        let path = temp_path("idempotent");
        let batch = batch(
            DiscoveryCursor::default(),
            cursor("2026-09-01T00:00:00Z"),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let mut store = DiscoveryStore::open(path.clone()).expect("open");
        assert_eq!(
            store.commit(batch.clone()).expect("commit"),
            DiscoveryCommit::Committed
        );
        let length = store.journal.metadata().expect("journal metadata").len();
        assert_eq!(
            store.commit(batch).expect("idempotent replay"),
            DiscoveryCommit::AlreadyCommitted
        );
        assert_eq!(
            store.journal.metadata().expect("journal metadata").len(),
            length
        );
        assert_eq!(store.search_revision(), 1);
        drop(store);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn same_coordinate_events_fold_against_the_provisional_selected_head() {
        let path = temp_path("provisional-head-fold");
        let cursor_one = cursor("position-1");
        let cursor_two = cursor("position-2");
        let initial = batch(
            DiscoveryCursor::default(),
            cursor_one.clone(),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let source = initial.source;
        let mut store = DiscoveryStore::open(path.clone()).expect("open");
        store.commit(initial).expect("commit initial head");
        let initial_search_revision = store.search_revision();

        let mut newest = batch(
            cursor_one.clone(),
            cursor_two.clone(),
            "1.0.0",
            DiscoveryStanding::Yanked,
            "2026-09-03T00:00:00Z",
        );
        newest.expected_base_sequence = 1;
        newest.facts[0].metadata.aliases = DiscoveryFacet::Known(vec!["newest".to_owned()]);
        let mut older = batch(
            cursor_one,
            cursor_two.clone(),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-02T00:00:00Z",
        );
        older.facts[0].metadata.aliases = DiscoveryFacet::Known(vec!["older".to_owned()]);
        newest.facts.extend(older.facts);

        store
            .commit(newest)
            .expect("fold source events in one CAS batch");
        let selected = store
            .fact(source, "pkg:nuget/Widget@1.0.0")
            .expect("selected head");
        assert_eq!(selected.standing, DiscoveryStanding::Yanked);
        assert_eq!(
            selected.source_event_time.as_deref(),
            Some("2026-09-03T00:00:00Z")
        );
        assert_eq!(
            selected.metadata.aliases,
            DiscoveryFacet::Known(vec!["newest".to_owned()])
        );
        assert_eq!(store.search_revision(), initial_search_revision + 1);
        assert_eq!(store.sequence(source), Some(2));

        drop(store);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_journal_schema_is_refused_with_its_version_and_left_intact() {
        let path = temp_path("unsupported-schema");
        let legacy_batch = batch(
            DiscoveryCursor::default(),
            cursor("legacy-position"),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let mut payload = Vec::new();
        serde_json::to_writer(
            &mut payload,
            &JournalWriteTransaction {
                version: 1,
                batch: &legacy_batch,
            },
        )
        .expect("serialize valid legacy-version transaction shape");
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.registry.discovery.transaction.v1\0");
        hasher.update(&payload);
        let checksum = *hasher.finalize().as_bytes();
        let mut frame = Vec::new();
        frame.extend_from_slice(JOURNAL_MAGIC);
        frame.extend_from_slice(&1_u16.to_be_bytes());
        frame.extend_from_slice(
            &u32::try_from(payload.len())
                .expect("small frame")
                .to_be_bytes(),
        );
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&checksum);
        fs::write(&path, &frame).expect("write valid legacy-format frame");

        assert!(matches!(
            DiscoveryStore::open(path.clone()),
            Err(DiscoveryStoreError::UnsupportedVersion(1))
        ));
        assert_eq!(
            fs::metadata(&path).expect("legacy file remains").len(),
            u64::try_from(frame.len()).expect("file length")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejected_search_projection_does_not_leave_a_durable_commit() {
        let path = temp_path("projection-preflight");
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://index.crates.io")
            .expect("admitted Cargo source");
        let source = discovery_source_identity(&endpoint);
        let mut store = DiscoveryStore::open(path.clone()).expect("open journal");
        let before = store.journal.metadata().expect("journal metadata").len();
        let observed_at = DiscoveryObservedAt::from_unix_millis(10);
        let batch = DiscoveryBatch {
            source,
            expected_base_sequence: 0,
            previous_cursor: DiscoveryCursor::default(),
            next_cursor: DiscoveryCursor::new(b"cursor-1".to_vec()).expect("next cursor"),
            source_high_watermark: DiscoveryCursor::new(b"cursor-1".to_vec())
                .expect("high watermark"),
            caught_up: true,
            observed_at,
            completeness: DiscoveryCompleteness::CompleteThroughCursor,
            facts: vec![DiscoveryFact {
                source,
                coordinate: backend_engine::registry::PackageCoordinate::parse(
                    "pkg:cargo/example%00name@1.0.0",
                )
                .expect("semantically parsed coordinate"),
                standing: DiscoveryStanding::Published,
                observed_at,
                source_event: DiscoverySourceEvent::Unordered,
                source_event_time: None,
                proof: [3; 32],
                metadata: DiscoveryMetadata::default(),
            }],
            package_retractions: Vec::new(),
        };

        assert!(matches!(
            store.commit(batch),
            Err(DiscoveryStoreError::Decode)
        ));
        assert_eq!(
            store.journal.metadata().expect("journal metadata").len(),
            before,
            "all projection errors must be found before fsync"
        );
        assert_eq!(store.sequence(source), None);
        assert_eq!(store.facts().count(), 0);

        drop(store);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn same_typed_event_key_with_changed_fact_content_conflicts_atomically() {
        for mutation in ["standing", "metadata"] {
            let path = temp_path("same-event-conflict");
            let cursor_one = cursor("position-1");
            let cursor_two = cursor("position-2");
            let first = batch(
                DiscoveryCursor::default(),
                cursor_one.clone(),
                "1.0.0",
                DiscoveryStanding::Published,
                "2026-09-01T00:00:00Z",
            );
            let source = first.source;
            let mut store = DiscoveryStore::open(path.clone()).expect("open");
            store.commit(first).expect("commit first event");

            let mut changed = fence_batch(
                batch(
                    cursor_one.clone(),
                    cursor_two,
                    "1.0.0",
                    DiscoveryStanding::Published,
                    "2026-09-01T00:00:00Z",
                ),
                1,
                cursor_one.clone(),
                cursor("position-2"),
                200,
            );
            if mutation == "standing" {
                changed.facts[0].standing = DiscoveryStanding::Yanked;
            } else {
                changed.facts[0].metadata.description =
                    DiscoveryFacet::Known("changed on same event".to_owned());
            }
            assert!(matches!(
                store.commit(changed),
                Err(DiscoveryStoreError::Conflict)
            ));
            assert_eq!(store.sequence(source), Some(1));
            assert_eq!(store.cursor(source), cursor_one);
            assert_eq!(
                store
                    .fact(source, "pkg:nuget/Widget@1.0.0")
                    .expect("original selected fact")
                    .standing,
                DiscoveryStanding::Published
            );
            assert_eq!(store.search_revision(), 1);
            drop(store);
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn identical_events_refresh_by_local_sequence_while_stale_events_do_not() {
        let path = temp_path("event-freshness");
        let cursor_one = cursor("position-1");
        let cursor_two = cursor("position-2");
        let cursor_three = cursor("position-3");
        let first = batch(
            DiscoveryCursor::default(),
            cursor_one.clone(),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-02T00:00:00Z",
        );
        let source = first.source;
        let mut store = DiscoveryStore::open(path.clone()).expect("open");
        store.commit(first).expect("commit first event");
        assert_eq!(store.search_revision(), 1);

        let identical = fence_batch(
            batch(
                cursor_one.clone(),
                cursor_two.clone(),
                "1.0.0",
                DiscoveryStanding::Published,
                "2026-09-02T00:00:00Z",
            ),
            1,
            cursor_one.clone(),
            cursor_two.clone(),
            50,
        );
        store.commit(identical).expect("identical event refresh");
        assert_eq!(store.sequence(source), Some(2));
        assert_eq!(
            store
                .fact(source, "pkg:nuget/Widget@1.0.0")
                .expect("selected fact")
                .observed_at
                .as_unix_millis(),
            50,
            "accepted identical content refreshes even when wall time regresses"
        );
        assert_eq!(
            store.search_revision(),
            1,
            "freshness does not rewrite search"
        );

        let stale = fence_batch(
            batch(
                cursor_two.clone(),
                cursor_three.clone(),
                "1.0.0",
                DiscoveryStanding::Yanked,
                "2026-09-01T00:00:00Z",
            ),
            2,
            cursor_two,
            cursor_three,
            75,
        );
        store
            .commit(stale)
            .expect("stale event still advances source progress");
        assert_eq!(store.sequence(source), Some(3));
        assert_eq!(
            store
                .fact(source, "pkg:nuget/Widget@1.0.0")
                .expect("selected fact")
                .observed_at
                .as_unix_millis(),
            50,
            "stale source history cannot refresh selected-head freshness"
        );
        assert_eq!(
            store
                .fact(source, "pkg:nuget/Widget@1.0.0")
                .expect("selected fact")
                .standing,
            DiscoveryStanding::Published
        );
        assert_eq!(store.search_revision(), 1);
        drop(store);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn snapshot_baselines_and_unsequenced_snapshot_updates_follow_source_contract() {
        let npm_snapshot = event_fact(
            RegistryEcosystem::Npm,
            DiscoverySourceEvent::Snapshot,
            None,
            DiscoveryStanding::Published,
        );
        let npm_event = event_fact(
            RegistryEcosystem::Npm,
            DiscoverySourceEvent::NpmChange {
                sequence: 1,
                revision: Some("1-rev".to_owned()),
                change_proof: [8; 32],
            },
            Some("00000000000000000001"),
            DiscoveryStanding::Published,
        );
        assert!(
            matches!(
                classify_fact_transition(&npm_snapshot, &npm_event),
                Ok(DiscoveryFactTransition::Newer)
            ),
            "a typed feed event may replace a local snapshot baseline under CAS"
        );
        assert!(
            matches!(
                classify_fact_transition(&npm_event, &npm_snapshot),
                Err(DiscoveryStoreError::Conflict)
            ),
            "typed provenance may never be downgraded to a snapshot"
        );
        let mut changed_snapshot = npm_snapshot.clone();
        changed_snapshot.metadata.description =
            DiscoveryFacet::Known("changed snapshot".to_owned());
        assert!(
            matches!(
                classify_fact_transition(&npm_snapshot, &changed_snapshot),
                Ok(DiscoveryFactTransition::Newer)
            ),
            "the accepted local sequence CAS orders successive unsequenced snapshots"
        );

        let cargo_snapshot = event_fact(
            RegistryEcosystem::Cargo,
            DiscoverySourceEvent::Snapshot,
            None,
            DiscoveryStanding::Published,
        );
        let mut changed_cargo_snapshot = cargo_snapshot.clone();
        changed_cargo_snapshot.metadata.description =
            DiscoveryFacet::Known("new snapshot body".to_owned());
        assert!(
            matches!(
                classify_fact_transition(&cargo_snapshot, &changed_cargo_snapshot),
                Ok(DiscoveryFactTransition::Newer)
            ),
            "a genuine unsequenced snapshot source advances only under its accepted local CAS"
        );
        assert!(matches!(
            classify_fact_transition(&cargo_snapshot, &cargo_snapshot),
            Ok(DiscoveryFactTransition::Identical)
        ));
    }

    #[test]
    fn npm_and_nuget_snapshots_update_under_local_cas_and_reopen() {
        for ecosystem in [RegistryEcosystem::Npm, RegistryEcosystem::Nuget] {
            let path = temp_path(match ecosystem {
                RegistryEcosystem::Npm => "npm-snapshot-reopen",
                RegistryEcosystem::Nuget => "nuget-snapshot-reopen",
                _ => unreachable!(),
            });
            let initial_cursor = cursor("snapshot-position");
            let first = snapshot_batch(
                ecosystem,
                0,
                DiscoveryCursor::default(),
                initial_cursor.clone(),
                200,
                DiscoveryStanding::Published,
                "initial snapshot metadata",
            );
            let source = first.source;
            {
                let mut store = DiscoveryStore::open(path.clone()).expect("open journal");
                store.commit(first).expect("commit initial snapshot");

                // The source cursor is intentionally unchanged. The captured
                // local sequence is the CAS that orders this fresh snapshot.
                let second = snapshot_batch(
                    ecosystem,
                    1,
                    initial_cursor.clone(),
                    initial_cursor.clone(),
                    150,
                    DiscoveryStanding::Yanked,
                    "updated snapshot metadata",
                );
                store
                    .commit(second)
                    .expect("commit fresh snapshot under CAS");
                assert_eq!(store.sequence(source), Some(2));
                assert_eq!(store.cursor(source), initial_cursor);
                assert_eq!(
                    store.search_revision(),
                    2,
                    "the changed searchable metadata is projected once"
                );
            }

            let reopened = DiscoveryStore::open(path.clone()).expect("reopen journal");
            let selected = reopened
                .fact(
                    source,
                    match ecosystem {
                        RegistryEcosystem::Npm => "pkg:npm/example@1.0.0",
                        RegistryEcosystem::Nuget => "pkg:nuget/Widget@1.0.0",
                        _ => unreachable!(),
                    },
                )
                .expect("latest snapshot fact survives reopen");
            assert_eq!(selected.standing, DiscoveryStanding::Yanked);
            assert_eq!(
                selected.metadata.description,
                DiscoveryFacet::Known("updated snapshot metadata".to_owned())
            );
            assert_eq!(selected.observed_at.as_unix_millis(), 150);
            assert_eq!(reopened.sequence(source), Some(2));
            drop(reopened);
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn typed_head_cannot_be_downgraded_to_snapshot_under_a_fresh_cas() {
        let path = temp_path("typed-to-snapshot-downgrade");
        let initial_cursor = cursor("typed-position");
        let first = batch(
            DiscoveryCursor::default(),
            initial_cursor.clone(),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let source = first.source;
        {
            let mut store = DiscoveryStore::open(path.clone()).expect("open journal");
            store.commit(first).expect("commit typed event");
            let snapshot = snapshot_batch(
                RegistryEcosystem::Nuget,
                1,
                initial_cursor.clone(),
                cursor("snapshot-position"),
                250,
                DiscoveryStanding::Yanked,
                "unsequenced replacement",
            );
            assert!(matches!(
                store.commit(snapshot),
                Err(DiscoveryStoreError::Conflict)
            ));
            assert_eq!(store.sequence(source), Some(1));
            assert_eq!(store.cursor(source), initial_cursor);
            assert_eq!(
                store
                    .fact(source, "pkg:nuget/Widget@1.0.0")
                    .expect("typed head remains selected")
                    .source_event,
                DiscoverySourceEvent::NugetCatalog {
                    timestamp: DiscoveryTimestamp::parse_nuget_catalog_timestamp(
                        "2026-09-01T00:00:00Z"
                    )
                    .expect("timestamp"),
                    commit_id: "test-2026-09-01T00:00:00Z".to_owned(),
                }
            );
        }
        let reopened = DiscoveryStore::open(path.clone()).expect("reopen journal");
        assert_eq!(reopened.sequence(source), Some(1));
        assert_eq!(reopened.cursor(source), initial_cursor);
        assert_eq!(
            reopened
                .fact(source, "pkg:nuget/Widget@1.0.0")
                .expect("typed event survived reopen")
                .standing,
            DiscoveryStanding::Published
        );
        drop(reopened);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn torn_tail_after_a_committed_transaction_is_truncated_on_reopen() {
        let path = temp_path("torn");
        let batch = batch(
            DiscoveryCursor::default(),
            cursor("2026-09-01T00:00:00Z"),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let source = batch.source;
        let length = {
            let mut store = DiscoveryStore::open(path.clone()).expect("open");
            store.commit(batch).expect("commit complete transaction");
            store.journal.metadata().expect("journal metadata").len()
        };
        let partial_frame = encode_frame(b"incomplete transaction").expect("encode frame");
        let mut journal = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open journal tail");
        journal
            .write_all(&partial_frame[..8])
            .expect("write torn frame prefix");
        journal.sync_all().expect("sync torn frame prefix");
        drop(journal);

        let reopened = DiscoveryStore::open(path.clone()).expect("recover committed prefix");
        assert_eq!(
            reopened.journal.metadata().expect("journal metadata").len(),
            length
        );
        assert_eq!(reopened.cursor(source), cursor("2026-09-01T00:00:00Z"));
        assert_eq!(reopened.facts().count(), 1);
        assert!(reopened.is_historical(source));
        drop(reopened);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cursor_conflict_does_not_advance_and_source_claims_do_not_merge() {
        let path = temp_path("conflict");
        let first = batch(
            DiscoveryCursor::default(),
            cursor("2026-09-01T00:00:00Z"),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let source = first.source;
        let mut store = DiscoveryStore::open(path.clone()).expect("open");
        store.commit(first).expect("commit");
        let stale = batch(
            DiscoveryCursor::default(),
            cursor("2026-09-02T00:00:00Z"),
            "1.0.1",
            DiscoveryStanding::Yanked,
            "2026-09-02T00:00:00Z",
        );
        assert!(matches!(
            store.commit(stale),
            Err(DiscoveryStoreError::Conflict)
        ));
        assert_eq!(store.cursor(source), cursor("2026-09-01T00:00:00Z"));
        assert_eq!(store.facts().count(), 1);
        drop(store);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn torn_tail_is_discarded_without_losing_the_last_transaction() {
        let path = temp_path("torn");
        let first = batch(
            DiscoveryCursor::default(),
            cursor("2026-09-01T00:00:00Z"),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let source = first.source;
        {
            let mut store = DiscoveryStore::open(path.clone()).expect("open");
            store.commit(first).expect("commit");
        }
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open torn tail")
            .write_all(b"DISCOV")
            .expect("write torn tail");
        let store = DiscoveryStore::open(path.clone()).expect("recover");
        assert_eq!(store.cursor(source), cursor("2026-09-01T00:00:00Z"));
        assert_eq!(store.facts().count(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn identical_durable_retry_is_idempotent() {
        let path = temp_path("retry");
        let first = batch(
            DiscoveryCursor::default(),
            cursor("2026-09-01T00:00:00Z"),
            "1.0.0",
            DiscoveryStanding::Published,
            "2026-09-01T00:00:00Z",
        );
        let repeated = first.clone();
        let mut store = DiscoveryStore::open(path.clone()).expect("open");
        store.commit(first).expect("commit");
        assert_eq!(
            store.commit(repeated).expect("idempotent retry"),
            DiscoveryCommit::AlreadyCommitted
        );
        assert_eq!(store.facts().count(), 1);
        drop(store);
        let _ = fs::remove_file(path);
    }
}
