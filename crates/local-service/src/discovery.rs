//! Transactional, source-only package discovery journal.
//!
//! Each append frame contains the admitted source facts and its next opaque
//! cursor in one checksummed transaction. A torn final frame is discarded on
//! reopen; a complete corrupt frame fails closed. Search projections retain
//! one claim per `(source, coordinate)`, so competing registries never become
//! one synthetic acquisition record.

use crate::process::RegistryDiscoveryConfig;
use backend_engine::registry::{
    DiscoveryBatch, DiscoveryCompleteness, DiscoveryCursor, DiscoveryError, DiscoveryFacet,
    DiscoveryFact, DiscoveryMetadata, DiscoveryObservedAt, DiscoveryPackageRetraction,
    DiscoveryReleaseObservation, DiscoverySourceIdentity, DiscoveryStanding,
    MAX_DISCOVERY_PAGE_ITEMS, NugetCatalogEvent, RegistryEcosystem, RegistryEndpoint,
    crates_sparse_index_path, parse_conan_recipe_tree, parse_conan_recipe_versions,
    parse_crates_recent_page, parse_crates_sparse_package, parse_go_module_index_page,
    parse_maven_search_page, parse_npm_changes_page, parse_npm_packument_document,
    parse_nuget_catalog_index, parse_nuget_catalog_leaf, parse_nuget_catalog_page,
    parse_pypi_project_list, parse_pypi_project_metadata,
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

const JOURNAL_MAGIC: &[u8; 8] = b"DISCOV01";
const JOURNAL_VERSION: u16 = 1;
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
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
const DISCOVERY_SOURCE_LIMIT: usize = 8;
const DISCOVERY_MESSAGE_CAPACITY: usize = 8;
const MAX_CACHED_SPARSE_RELEASES: usize = 4096;

/// Typed failure from opening or advancing the discovery journal.
#[derive(Debug)]
pub(crate) enum DiscoveryStoreError {
    Io(io::Error),
    Decode,
    Corrupt,
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

#[derive(Clone, Debug, Default)]
struct SourceProgress {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoverySearchDocument {
    pub(crate) source: DiscoverySourceIdentity,
    pub(crate) coordinate: backend_engine::registry::PackageCoordinate,
    pub(crate) lineage: String,
    pub(crate) metadata: DiscoveryMetadata,
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
}

enum DiscoveryWorkerMessage {
    Batch {
        batch: DiscoveryBatch,
        acknowledgement: SyncSender<bool>,
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
        for endpoint in config.sources {
            let source = DiscoverySourceIdentity::from_endpoint(&endpoint);
            if seen.insert(source) {
                sources.push((endpoint, source));
            }
        }
        if !config.offline {
            for (endpoint, source) in sources {
                let worker_sender = sender.clone();
                let worker_cancelled = Arc::clone(&cancelled);
                let max_pages = config.max_pages;
                let cursor = store.cursor(source);
                let source_id = source.id();
                let worker = thread::Builder::new()
                    .name(format!(
                        "registry-discovery-{:02x}{:02x}",
                        source_id[0], source_id[1]
                    ))
                    .spawn(move || {
                        discovery_worker(
                            endpoint,
                            cursor,
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
                    let _ = acknowledgement.send(committed);
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
                let _ = acknowledgement.send(false);
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
    mut cursor: DiscoveryCursor,
    max_pages: usize,
    sender: SyncSender<DiscoveryWorkerMessage>,
    cancelled: Arc<AtomicBool>,
) {
    let source = DiscoverySourceIdentity::from_endpoint(&endpoint);
    let mut retry_delay = DISCOVERY_REFRESH_INTERVAL;
    let mut sparse_cache = BTreeMap::new();
    while !cancelled.load(Ordering::Acquire) {
        let deadline = Instant::now() + DISCOVERY_REFRESH_BUDGET;
        match build_source_batch(
            &endpoint,
            cursor.clone(),
            max_pages,
            deadline,
            &cancelled,
            &mut sparse_cache,
        ) {
            Ok(batch) => {
                let next_cursor = batch.next_cursor.clone();
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
                        Ok(true) => {
                            cursor = next_cursor;
                            retry_delay = DISCOVERY_REFRESH_INTERVAL;
                            break;
                        }
                        Ok(false) | Err(TryRecvError::Disconnected) => break,
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
) -> Result<DiscoveryBatch, DiscoveryStoreError> {
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
) -> Result<DiscoveryBatch, DiscoveryStoreError> {
    let source = DiscoverySourceIdentity::from_endpoint(endpoint);
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
        let responses = std::thread::scope(|scope| {
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
    let batch = DiscoveryBatch {
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
) -> Result<DiscoveryBatch, DiscoveryStoreError> {
    let source = DiscoverySourceIdentity::from_endpoint(endpoint);
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
        if package.deleted {
            package_retractions.push(DiscoveryPackageRetraction {
                package_name: package.name,
                source_event_time: package.source_event_time,
                proof: package.proof,
            });
            continue;
        }
        if package.requires_packument {
            let base = npm_packument_base(endpoint.as_str());
            let name = percent_encode_component(&package.name);
            let packument_url = format!("{}/{name}", base.trim_end_matches('/'));
            let packument_bytes =
                fetch_metadata(&packument_url, MAX_SOURCE_BODY_BYTES, deadline, cancelled)?;
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
                facts.push(discovery_fact(source, release, observed_at, None));
            }
        } else {
            facts.extend(
                package
                    .releases
                    .into_iter()
                    .map(|release| discovery_fact(source, release, observed_at, None)),
            );
        }
        if facts.len() > MAX_DISCOVERY_PAGE_ITEMS {
            return Err(DiscoveryStoreError::Bounds);
        }
    }
    let caught_up = page.pending == 0 && page.next_cursor == source_high_watermark;
    let batch = DiscoveryBatch {
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
) -> Result<DiscoveryBatch, DiscoveryStoreError> {
    let source = DiscoverySourceIdentity::from_endpoint(endpoint);
    let previous_project = parse_pypi_cursor(&previous)?;
    let list_url = format!("{}/simple/", endpoint.as_str().trim_end_matches('/'));
    let list_bytes = fetch_metadata_with_accept(
        &list_url,
        MAX_SOURCE_BODY_BYTES,
        deadline,
        cancelled,
        "application/vnd.pypi.simple.v1+json",
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
        let event_serial = project.serial.map(|serial| format!("{serial:020}"));
        facts.extend(metadata.releases.into_iter().map(|mut release| {
            if event_serial.is_none() {
                // PEP 691 does not define a source event serial. In that case
                // the observation time orders repeated facts locally, while
                // `source_event_time` remains genuinely unknown.
                release.source_event_time = None;
            }
            discovery_fact(source, release, observed_at, event_serial.clone())
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
    let batch = DiscoveryBatch {
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
) -> Result<DiscoveryBatch, DiscoveryStoreError> {
    let source = DiscoverySourceIdentity::from_endpoint(endpoint);
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
        .map(|release| discovery_fact(source, release, observed_at, None))
        .collect::<Vec<_>>();
    let next_cursor = next_maven_window_cursor(&previous)?;
    let batch = DiscoveryBatch {
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
) -> Result<DiscoveryBatch, DiscoveryStoreError> {
    let source = DiscoverySourceIdentity::from_endpoint(endpoint);
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
        .map(|release| discovery_fact(source, release, observed_at, None))
        .collect::<Vec<_>>();
    let caught_up = facts.len() < limit;
    let batch = DiscoveryBatch {
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
) -> Result<DiscoveryBatch, DiscoveryStoreError> {
    let source = DiscoverySourceIdentity::from_endpoint(endpoint);
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
        facts.extend(
            releases
                .into_iter()
                .map(|release| discovery_fact(source, release, observed_at, None)),
        );
        if facts.len() > MAX_DISCOVERY_PAGE_ITEMS {
            return Err(DiscoveryStoreError::Bounds);
        }
    }
    let next_offset = offset.saturating_add(selected_count);
    let next_cursor = conan_cursor(generation, &tree.tree_sha, next_offset)?;
    let source_high_watermark = conan_cursor(generation, &tree.tree_sha, tree.recipes.len())?;
    let caught_up = next_offset >= tree.recipes.len() && !tree.truncated;
    let batch = DiscoveryBatch {
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
) -> DiscoveryFact {
    DiscoveryFact {
        source,
        coordinate: release.coordinate,
        standing: release.standing,
        observed_at,
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
) -> Result<DiscoveryBatch, DiscoveryStoreError> {
    let source = DiscoverySourceIdentity::from_endpoint(endpoint);
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
        left.commit_timestamp
            .cmp(&right.commit_timestamp)
            .then_with(|| left.commit_id.cmp(&right.commit_id))
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
            source_event_time: Some(format!("{}|{}", event.commit_timestamp, event.commit_id)),
            proof: event.proof,
            metadata: event.metadata,
        })
        .collect();
    let batch = DiscoveryBatch {
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
        metadata: fact.metadata.clone(),
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
    fetch_metadata_with_accept(url, maximum, deadline, cancelled, "application/json")
}

fn fetch_metadata_with_accept(
    url: &str,
    maximum: usize,
    deadline: Instant,
    cancelled: &AtomicBool,
    accept: &str,
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
        .timeout_global(Some(remaining.min(DISCOVERY_REQUEST_TIMEOUT)))
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
        batch.admit()?;
        let current = self
            .sources
            .get(&batch.source)
            .map_or_else(DiscoveryCursor::default, |state| state.cursor.clone());
        if current != batch.previous_cursor {
            if current == batch.next_cursor && self.is_replay_of_committed_batch(&batch)? {
                return Ok(DiscoveryCommit::AlreadyCommitted);
            }
            return Err(DiscoveryStoreError::Conflict);
        }
        if batch.source.ecosystem() == backend_engine::RegistryEcosystem::Nuget
            && (batch.next_cursor.as_bytes() < batch.previous_cursor.as_bytes()
                || batch.source_high_watermark.as_bytes() < batch.previous_cursor.as_bytes()
                || self.sources.get(&batch.source).is_some_and(|state| {
                    batch.source_high_watermark.as_bytes() < state.source_high_watermark.as_bytes()
                }))
        {
            return Err(DiscoveryStoreError::Conflict);
        }
        let input_fingerprint = batch_fingerprint(&batch)?;
        let materialized = self.materialize_package_retractions(&batch)?;
        let new_documents = self.validate_fact_transition(&materialized)?;
        let next_search_revision = self
            .search_revision
            .checked_add(u64::try_from(new_documents).map_err(|_| DiscoveryStoreError::Bounds)?)
            .ok_or(DiscoveryStoreError::Bounds)?;
        let next_observation_revision = self
            .observation_revision
            .checked_add(1)
            .ok_or(DiscoveryStoreError::Bounds)?;
        let previous_search_revision = self.search_revision;

        let encoded = serde_json::to_vec(&JournalTransaction {
            version: JOURNAL_VERSION,
            batch: batch.clone(),
        })
        .map_err(|_| DiscoveryStoreError::Decode)?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(DiscoveryStoreError::Bounds);
        }
        let frame = encode_frame(&encoded)?;
        let start = self.journal.seek(SeekFrom::End(0))?;
        let durable_result = self
            .journal
            .write_all(&frame)
            .and_then(|()| self.journal.sync_all())
            .and_then(|()| durable::sync_parent(&self.path));
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
        let search_documents = self.apply(materialized, input_fingerprint)?;
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
            if &header[..8] != JOURNAL_MAGIC
                || u16::from_be_bytes([header[8], header[9]]) != JOURNAL_VERSION
            {
                return Err(DiscoveryStoreError::Corrupt);
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
                return Err(DiscoveryStoreError::Corrupt);
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
        if batch.previous_cursor != expected {
            return Err(DiscoveryStoreError::Corrupt);
        }
        if batch.source.ecosystem() == backend_engine::RegistryEcosystem::Nuget
            && (batch.next_cursor.as_bytes() < batch.previous_cursor.as_bytes()
                || batch.source_high_watermark.as_bytes() < batch.previous_cursor.as_bytes()
                || self.sources.get(&batch.source).is_some_and(|state| {
                    batch.source_high_watermark.as_bytes() < state.source_high_watermark.as_bytes()
                }))
        {
            return Err(DiscoveryStoreError::Corrupt);
        }
        let input_fingerprint = batch_fingerprint(&batch)?;
        let materialized = self.materialize_package_retractions(&batch)?;
        let new_documents = self.validate_fact_transition(&materialized)?;
        let next_search_revision = self
            .search_revision
            .checked_add(u64::try_from(new_documents).map_err(|_| DiscoveryStoreError::Bounds)?)
            .ok_or(DiscoveryStoreError::Bounds)?;
        let next_observation_revision = self
            .observation_revision
            .checked_add(1)
            .ok_or(DiscoveryStoreError::Bounds)?;
        let previous_search_revision = self.search_revision;
        let search_documents = self.apply(materialized, input_fingerprint)?;
        self.search_revision = next_search_revision;
        self.observation_revision = next_observation_revision;
        self.record_search_changes(search_documents, previous_search_revision);
        Ok(())
    }

    fn apply(
        &mut self,
        batch: DiscoveryBatch,
        fingerprint: [u8; 32],
    ) -> Result<Vec<DiscoverySearchDocument>, DiscoveryStoreError> {
        let source_identity = batch.source;
        let source = self.sources.entry(source_identity).or_default();
        let mut search_documents = Vec::new();
        for fact in batch.facts {
            let key = fact.coordinate.as_str().to_owned();
            if let Some(previous) = source.facts.get(&key) {
                if is_newer(previous, &fact)? {
                    if !same_search_projection(&previous.metadata, &fact.metadata) {
                        search_documents.push(search_document(source_identity, &fact)?);
                    }
                    source.facts.insert(key, fact);
                }
            } else {
                search_documents.push(search_document(source_identity, &fact)?);
                let package_name = discovered_package_qualified_name(&fact)?;
                source
                    .package_releases
                    .entry(package_name)
                    .or_default()
                    .insert(key.clone());
                source.facts.insert(key, fact);
            }
        }
        source.cursor = batch.next_cursor;
        source.source_high_watermark = batch.source_high_watermark;
        source.caught_up = batch.caught_up;
        source.completeness = Some(batch.completeness);
        source.latest_observed_at = Some(batch.observed_at.as_unix_millis());
        source.historical = false;
        source.refresh_failed = false;
        source.last_batch_fingerprint = Some(fingerprint);
        Ok(search_documents)
    }

    fn materialize_package_retractions(
        &self,
        batch: &DiscoveryBatch,
    ) -> Result<DiscoveryBatch, DiscoveryStoreError> {
        if batch.package_retractions.is_empty() {
            return Ok(batch.clone());
        }
        if batch.source.ecosystem() != backend_engine::RegistryEcosystem::Npm {
            return Err(DiscoveryStoreError::Decode);
        }
        let mut materialized = batch.clone();
        let Some(source) = self.sources.get(&batch.source) else {
            materialized.package_retractions.clear();
            return Ok(materialized);
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
                withdrawn.proof = retraction.proof;
                materialized.facts.push(withdrawn);
            }
        }
        materialized.package_retractions.clear();
        materialized.admit().map_err(DiscoveryStoreError::from)?;
        Ok(materialized)
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

    fn is_replay_of_committed_batch(
        &self,
        batch: &DiscoveryBatch,
    ) -> Result<bool, DiscoveryStoreError> {
        let Some(state) = self.sources.get(&batch.source) else {
            return Ok(false);
        };
        Ok(state.last_batch_fingerprint == Some(batch_fingerprint(batch)?))
    }

    fn validate_fact_transition(
        &self,
        batch: &DiscoveryBatch,
    ) -> Result<usize, DiscoveryStoreError> {
        let existing = self.sources.get(&batch.source);
        let mut pending = BTreeMap::<String, DiscoveryFact>::new();
        for fact in &batch.facts {
            let key = fact.coordinate.as_str().to_owned();
            let previous = pending
                .get(&key)
                .or_else(|| existing.and_then(|state| state.facts.get(&key)));
            let accept = match previous {
                Some(previous) => is_newer(previous, fact)?,
                None => true,
            };
            if accept {
                pending.insert(key, fact.clone());
            }
        }
        let new_coordinates = pending
            .keys()
            .filter(|key| existing.is_none_or(|state| !state.facts.contains_key(*key)))
            .count();
        if existing
            .map_or(0, |state| state.facts.len())
            .saturating_add(new_coordinates)
            > MAX_FACTS_PER_SOURCE
        {
            return Err(DiscoveryStoreError::Bounds);
        }
        let search_updates = pending
            .iter()
            .filter(|(key, incoming)| {
                existing
                    .and_then(|state| state.facts.get(*key))
                    .is_none_or(|previous| {
                        !same_search_projection(&previous.metadata, &incoming.metadata)
                    })
            })
            .count();
        Ok(search_updates)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalTransaction {
    version: u16,
    batch: DiscoveryBatch,
}

fn is_newer(
    previous: &DiscoveryFact,
    incoming: &DiscoveryFact,
) -> Result<bool, DiscoveryStoreError> {
    match (
        previous.source_event_time.as_deref(),
        incoming.source_event_time.as_deref(),
    ) {
        (Some(old), Some(new)) if old == new => {
            if previous.proof != incoming.proof || previous.standing != incoming.standing {
                return Err(DiscoveryStoreError::Conflict);
            }
            Ok(incoming.observed_at > previous.observed_at)
        }
        (Some(old), Some(new)) if new > old => Ok(true),
        (Some(old), Some(new)) if new < old => Ok(false),
        (Some(_), Some(_)) => {
            if previous.standing != incoming.standing || previous.proof != incoming.proof {
                return Err(DiscoveryStoreError::Conflict);
            }
            Ok(incoming.observed_at > previous.observed_at)
        }
        (None, Some(_)) => Ok(true),
        (Some(_), None) => Ok(false),
        (None, None) => Ok(incoming.observed_at > previous.observed_at),
    }
}

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
    let mut hasher = backend_engine::blake3::Hasher::new();
    hasher.update(b"backend.registry.discovery.transaction.v1\0");
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

fn batch_fingerprint(batch: &DiscoveryBatch) -> Result<[u8; 32], DiscoveryStoreError> {
    let payload = serde_json::to_vec(batch).map_err(|_| DiscoveryStoreError::Decode)?;
    let mut hasher = backend_engine::blake3::Hasher::new();
    hasher.update(b"backend.registry.discovery.batch.v1\0");
    hasher.update(&payload);
    Ok(*hasher.finalize().as_bytes())
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
        DiscoverySourceIdentity::from_endpoint(&endpoint)
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
                source_event_time: Some(event_time.to_owned()),
                proof: [u8::from(status as u8); 32],
                metadata: DiscoveryMetadata::default(),
            }],
            package_retractions: Vec::new(),
        }
    }

    fn cursor(value: &str) -> DiscoveryCursor {
        DiscoveryCursor::new(value.as_bytes().to_vec()).expect("cursor")
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
