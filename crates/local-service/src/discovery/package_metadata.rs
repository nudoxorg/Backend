//! Exact package metadata reads do not depend on namespace crawl position.
//! The cache retains only admitted source observations. A 304 is usable only
//! with the exact prior object, parser identity, endpoint and queried PURL.

use super::*;
use backend_engine::registry::{PackageCoordinate, admit_registry_coordinate};
use backend_library::{DiscoveryAdvisory, ProductText, RegistryPackageDiscoveryObservation};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CACHE_OBJECTS: usize = 32;
const MAX_CACHE_ENCODED_BYTES: usize = 64 * 1024 * 1024;
const PARSER_IDENTITY: &str = "exact-registry-package-metadata-v5";
const MAX_NUGET_SERVICE_INDEX_BYTES: usize = 1024 * 1024;
const MAX_NUGET_REGISTRATION_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_NUGET_REGISTRATION_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_NUGET_REGISTRATION_PAGES: usize = 128;
const MAX_NUGET_REGISTRATION_DEPTH: usize = 16;
const MAX_NUGET_REGISTRATION_LEAVES: usize = 8192;

#[derive(Clone, Copy, Eq, PartialEq)]
enum ExactVersionPresence {
    Absent,
    Present,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ObjectKey {
    endpoint: String,
    parser: &'static str,
    package: String,
}

struct CachedObject {
    etag: Option<String>,
    releases: Vec<DiscoveryReleaseObservation>,
    /// Permits exact absence only when the requested version is absent from
    /// the admitted document. A recorded PyPI release without files cannot
    /// become a negative version observation.
    complete: bool,
    proof: [u8; 32],
    observed: Instant,
    observed_at_millis: u64,
    encoded_bytes: usize,
    admitted_sequence: u64,
}

#[derive(Default)]
pub(super) struct PackageMetadataCache {
    objects: BTreeMap<ObjectKey, Arc<CachedObject>>,
    encoded_bytes: usize,
}

enum ObjectResponse {
    Modified {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
    Unchanged,
    Missing {
        proof: [u8; 32],
    },
    Nuget {
        releases: Vec<DiscoveryReleaseObservation>,
        complete: bool,
        proof: [u8; 32],
    },
}

pub(crate) enum PackageMetadataPreparation {
    Cached(Option<RegistryPackageDiscoveryObservation>),
    Fetch(PreparedPackageMetadata),
}

pub(crate) struct PreparedPackageMetadata {
    key: ObjectKey,
    source: DiscoverySourceIdentity,
    progress: DiscoveryProgressToken,
    pub(crate) package: PackageCoordinate,
    pub(crate) owner_epoch: [u8; 16],
    pub(crate) request_id: u64,
    request_identity: [u8; 32],
    ecosystem: RegistryEcosystem,
    name: String,
    maximum: usize,
    cached: Option<Arc<CachedObject>>,
    cancelled: Arc<AtomicBool>,
    request_started_at: Instant,
    queued_at: Option<Instant>,
}

pub(crate) struct FetchedPackageMetadata {
    pub(crate) request: PreparedPackageMetadata,
    result: Result<CachedObject, DiscoveryStoreError>,
}

/// Opt-in phase timing contains no endpoint URL, token, package, or body.
/// `queue` covers accepted submission through worker start; `total` covers
/// preparation through the parsed fetch result, including queue, fetch, and
/// parse time. Neither is the full command-serialization duration.
fn trace_metadata(request_id: u64, phase: &str, elapsed: Duration, outcome: &str) {
    if std::env::var_os("BACKEND_PACKAGE_METADATA_TRACE").is_some_and(|value| value == "1") {
        eprintln!(
            "locald package-metadata request={request_id} phase={phase} outcome={outcome} elapsed_us={}",
            elapsed.as_micros()
        );
    }
}

impl PreparedPackageMetadata {
    pub(crate) fn mark_queued(&mut self) {
        self.queued_at = Some(Instant::now());
    }

    #[cfg(test)]
    pub(crate) fn fetch(self, cancelled: impl Fn() -> bool) -> FetchedPackageMetadata {
        self.fetch_with_budget(cancelled, || REQUEST_TIMEOUT)
    }

    pub(crate) fn fetch_with_budget(
        self,
        cancelled: impl Fn() -> bool,
        remaining_budget: impl Fn() -> Duration,
    ) -> FetchedPackageMetadata {
        let request_id = self.request_id;
        let request_started_at = self.request_started_at;
        if let Some(queued_at) = self.queued_at {
            trace_metadata(request_id, "queue", queued_at.elapsed(), "started");
        }
        let result = (|| {
            if cancelled() || self.cancelled.load(Ordering::Acquire) {
                return Err(DiscoveryStoreError::Cancelled);
            }
            let etag = self
                .cached
                .as_ref()
                .and_then(|cached| cached.etag.as_deref());
            let fetch_started = Instant::now();
            let response = if self.ecosystem == RegistryEcosystem::Nuget {
                // A NuGet point observation is a linked service-index and
                // registration-page transcript. Conditional validation of
                // only one link would not validate the whole proof, so this
                // path uses the bounded freshness cache but fetches the chain
                // unconditionally after expiry.
                fetch_nuget_registration(
                    &self.key.endpoint,
                    &self.name,
                    &self.package,
                    || cancelled() || self.cancelled.load(Ordering::Acquire),
                    &remaining_budget,
                )
            } else {
                fetch_object(
                    &self.key.endpoint,
                    self.maximum,
                    || cancelled() || self.cancelled.load(Ordering::Acquire),
                    &remaining_budget,
                    etag,
                )
            };
            trace_metadata(
                self.request_id,
                "fetch",
                fetch_started.elapsed(),
                if response.is_ok() {
                    "received"
                } else {
                    "failed"
                },
            );
            let response = response?;
            let parse_started = Instant::now();
            let parsed = (|| {
                Ok::<_, DiscoveryStoreError>(match response {
                    ObjectResponse::Modified { bytes, etag } => {
                        let proof = *blake3::hash(&bytes).as_bytes();
                        let (releases, complete) =
                            parse_object(self.ecosystem, &bytes, &self.name, &self.package)?;
                        (releases, complete, proof, etag)
                    }
                    ObjectResponse::Unchanged => {
                        let cached = self.cached.as_ref().ok_or(DiscoveryStoreError::Corrupt)?;
                        (
                            cached.releases.clone(),
                            cached.complete,
                            cached.proof,
                            cached.etag.clone(),
                        )
                    }
                    ObjectResponse::Missing { proof } => (Vec::new(), true, proof, None),
                    ObjectResponse::Nuget {
                        releases,
                        complete,
                        proof,
                    } => (releases, complete, proof, None),
                })
            })();
            trace_metadata(
                self.request_id,
                "parse",
                parse_started.elapsed(),
                if parsed.is_ok() {
                    "admitted-shape"
                } else {
                    "failed"
                },
            );
            let (releases, complete, proof, etag) = parsed?;
            if cancelled() || self.cancelled.load(Ordering::Acquire) {
                return Err(DiscoveryStoreError::Cancelled);
            }
            Ok(CachedObject {
                etag,
                releases,
                complete,
                proof,
                observed: Instant::now(),
                observed_at_millis: discovery_now(),
                encoded_bytes: 0,
                admitted_sequence: 0,
            })
        })();
        trace_metadata(
            request_id,
            "total",
            request_started_at.elapsed(),
            if result.is_ok() { "complete" } else { "failed" },
        );
        FetchedPackageMetadata {
            request: self,
            result,
        }
    }
}

impl DiscoveryGateway {
    fn prepare_request(
        &self,
        package: &PackageCoordinate,
        owner_epoch: [u8; 16],
        request_id: u64,
    ) -> Result<PreparedPackageMetadata, RegistryPackageDiscoveryObservation> {
        let coordinate = match admit_registry_coordinate(package) {
            Ok(coordinate) => coordinate,
            Err(_) => {
                return Err(unavailable(
                    None,
                    "package has no registry metadata authority",
                ));
            }
        };
        let Some(endpoint) = self
            .metadata_sources
            .iter()
            .find(|endpoint| endpoint.ecosystem() == coordinate.ecosystem())
        else {
            return Err(unavailable(
                None,
                "no metadata source is configured for this ecosystem",
            ));
        };
        let name = coordinate.qualified_name().as_str();
        let (url, maximum) = match coordinate.ecosystem() {
            RegistryEcosystem::Npm => (
                format!(
                    "{}/{}",
                    npm_packument_base(endpoint.as_str()),
                    percent_encode_component(name)
                ),
                MAX_NPM_PACKUMENT_BYTES,
            ),
            RegistryEcosystem::Pypi => (
                format!(
                    "{}/pypi/{}/json",
                    endpoint.as_str().trim_end_matches('/'),
                    percent_encode_component(name)
                ),
                MAX_SOURCE_BODY_BYTES,
            ),
            RegistryEcosystem::Golang => {
                if name
                    .split('/')
                    .any(|segment| matches!(segment, "" | "." | ".."))
                {
                    return Err(unavailable(None, "Go module path has an invalid segment"));
                }
                let root = if endpoint.as_str() == "https://index.golang.org" {
                    "https://proxy.golang.org"
                } else {
                    endpoint.as_str().trim_end_matches('/')
                };
                (
                    format!(
                        "{}/{}/@v/{}.info",
                        root,
                        name.split('/')
                            .map(|segment| percent_encode_component(&go_proxy_escape(segment)))
                            .collect::<Vec<_>>()
                            .join("/"),
                        percent_encode_component(&go_proxy_escape(coordinate.version().as_str()))
                    ),
                    1024 * 1024,
                )
            }
            RegistryEcosystem::Nuget => (
                nuget_service_index_url(endpoint.as_str()),
                MAX_NUGET_SERVICE_INDEX_BYTES,
            ),
            _ => {
                return Err(unavailable(
                    None,
                    "exact metadata lookup is not supported by this source protocol",
                ));
            }
        };
        let point_endpoint = match RegistryEndpoint::new(coordinate.ecosystem(), url.clone()) {
            Ok(endpoint) => endpoint,
            Err(_) => {
                return Err(unavailable(
                    None,
                    "package metadata endpoint failed admission",
                ));
            }
        };
        let source = discovery_source_identity(&point_endpoint);
        let key = ObjectKey {
            endpoint: url.clone(),
            parser: PARSER_IDENTITY,
            package: package.as_str().to_owned(),
        };

        let mut identity = blake3::Hasher::new();
        identity.update(b"backend.registry.package-metadata.request.v1\0");
        identity.update(&owner_epoch);
        identity.update(&request_id.to_le_bytes());
        identity.update(&missing_proof(&key, [0; 32]));
        Ok(PreparedPackageMetadata {
            source,
            progress: self.store.progress_token(source),
            package: package.clone(),
            owner_epoch,
            request_id,
            request_identity: *identity.finalize().as_bytes(),
            ecosystem: coordinate.ecosystem(),
            name: name.to_owned(),
            maximum,
            cached: self.package_metadata.objects.get(&key).cloned(),
            key,
            cancelled: Arc::clone(&self.cancelled),
            request_started_at: Instant::now(),
            queued_at: None,
        })
    }

    /// Captures a narrow source/owner witness, without performing I/O on the owner.
    pub(crate) fn prepare_package(
        &self,
        package: &PackageCoordinate,
        owner_epoch: [u8; 16],
        request_id: u64,
    ) -> PackageMetadataPreparation {
        if self.cancelled.load(Ordering::Acquire) {
            return PackageMetadataPreparation::Cached(Some(unavailable(
                None,
                "registry metadata request was cancelled",
            )));
        }
        let request = match self.prepare_request(package, owner_epoch, request_id) {
            Ok(request) => request,
            Err(observation) => return PackageMetadataPreparation::Cached(Some(observation)),
        };
        if let Some(cached) = &request.cached
            && cached.admitted_sequence == request.progress.sequence
            && cached.observed.elapsed() < DISCOVERY_REFRESH_INTERVAL
        {
            let observation = if cached
                .releases
                .iter()
                .any(|release| &release.coordinate == package)
            {
                None
            } else if cached.complete {
                Some(RegistryPackageDiscoveryObservation::Missing {
                    source: request.source.id(),
                    proof: missing_proof(&request.key, cached.proof),
                    observed_at_millis: cached.observed_at_millis,
                })
            } else {
                Some(unavailable(
                    Some(request.source.id()),
                    "the registry records this version without distribution files",
                ))
            };
            trace_metadata(request_id, "cache", Duration::ZERO, "hit");
            return PackageMetadataPreparation::Cached(observation);
        }
        if self.metadata_offline {
            trace_metadata(request_id, "cache", Duration::ZERO, "offline-miss");
            return PackageMetadataPreparation::Cached(Some(unavailable(
                Some(request.source.id()),
                "registry metadata is offline and no current admitted cached observation is available",
            )));
        }
        trace_metadata(request_id, "cache", Duration::ZERO, "miss");
        PackageMetadataPreparation::Fetch(request)
    }

    /// Only already-observed evidence may be read synchronously by a surface.
    pub(crate) fn cached_package_observation(
        &self,
        package: &PackageCoordinate,
    ) -> Option<RegistryPackageDiscoveryObservation> {
        match self.prepare_package(package, [0; 16], 0) {
            PackageMetadataPreparation::Cached(observation) => observation,
            PackageMetadataPreparation::Fetch(_) => Some(unavailable(
                None,
                "registry package metadata has not been observed yet",
            )),
        }
    }

    /// Admits a detached result under its captured point-source CAS. Neither
    /// namespace crawl progress nor unrelated workspace revisions participate.
    pub(crate) fn admit_package_completion(
        &mut self,
        fetched: FetchedPackageMetadata,
        owner_epoch: [u8; 16],
    ) -> Option<RegistryPackageDiscoveryObservation> {
        let FetchedPackageMetadata {
            mut request,
            result,
        } = fetched;
        let source = request.source;
        let selected = self.prepare_request(&request.package, owner_epoch, request.request_id);
        if self.metadata_offline
            || request.owner_epoch != owner_epoch
            || selected.as_ref().map_or(true, |selected| {
                selected.key != request.key
                    || selected.source != source
                    || selected.request_identity != request.request_identity
            })
        {
            return Some(unavailable(
                Some(source.id()),
                "registry metadata authority changed during the request; retry",
            ));
        }
        // This authority check cloned the cache witness. It must not masquerade
        // as a still-queued read and prevent replacement of the validated object.
        drop(selected);
        let current = self.store.progress_token(source);
        if current.sequence != request.progress.sequence
            || current.cursor != request.progress.cursor
        {
            return Some(unavailable(
                Some(source.id()),
                "a newer point observation was committed before this response; retry",
            ));
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Some(unavailable(
                Some(source.id()),
                "registry metadata request was cancelled",
            ));
        }
        let mut object = match result {
            Ok(object) => object,
            Err(error) => {
                self.store.mark_failed(source, true);
                return Some(unavailable(
                    Some(source.id()),
                    &format!("metadata request failed: {error:?}"),
                ));
            }
        };
        let observed_at = DiscoveryObservedAt::from_unix_millis(object.observed_at_millis);
        let mut facts = object
            .releases
            .iter()
            .cloned()
            .map(|mut release| {
                release.source_event_time = None;
                discovery_fact(
                    source,
                    release,
                    observed_at,
                    None,
                    DiscoverySourceEvent::Snapshot,
                )
            })
            .collect::<Vec<_>>();
        if object.complete {
            for (_, old) in self
                .store
                .facts()
                .filter(|(old_source, _)| **old_source == source)
            {
                if old.coordinate == request.package
                    && !facts.iter().any(|fact| fact.coordinate == old.coordinate)
                {
                    facts.push(DiscoveryFact {
                        source,
                        coordinate: old.coordinate.clone(),
                        standing: DiscoveryStanding::Withdrawn,
                        observed_at,
                        source_event: DiscoverySourceEvent::Snapshot,
                        source_event_time: None,
                        proof: object.proof,
                        metadata: DiscoveryMetadata::default(),
                    });
                }
            }
        }
        object.encoded_bytes = facts
            .iter()
            .try_fold(0_usize, |total, fact| {
                serde_json::to_vec(fact)
                    .ok()
                    .and_then(|bytes| total.checked_add(bytes.len()))
            })
            .unwrap_or(usize::MAX);
        if object.encoded_bytes > MAX_CACHE_ENCODED_BYTES {
            return Some(unavailable(
                Some(source.id()),
                "admitted package metadata exceeds the bounded object cache",
            ));
        }
        let draft = DiscoveryBatchDraft {
            source,
            previous_cursor: current.cursor.clone(),
            next_cursor: current.cursor.clone(),
            source_high_watermark: current.cursor,
            caught_up: false,
            observed_at,
            completeness: if object.complete {
                DiscoveryCompleteness::Windowed
            } else {
                DiscoveryCompleteness::Incomplete
            },
            facts,
            package_retractions: Vec::new(),
        };
        if let Err(error) = draft
            .admit_for_sequence(current.sequence)
            .map_err(DiscoveryStoreError::from)
            .and_then(|batch| {
                if self.cancelled.load(Ordering::Acquire) {
                    return Err(DiscoveryStoreError::Cancelled);
                }
                self.store.commit(batch)
            })
        {
            self.store.mark_failed(source, true);
            return Some(unavailable(
                Some(source.id()),
                &format!("metadata observation was not committed: {error:?}"),
            ));
        }
        self.store.mark_failed(source, false);
        object.admitted_sequence = self.store.progress_token(source).sequence;
        if self.cancelled.load(Ordering::Acquire) {
            return Some(unavailable(
                Some(source.id()),
                "registry metadata request was cancelled before cache publication",
            ));
        }
        let found = object
            .releases
            .iter()
            .any(|release| release.coordinate == request.package);
        let outcome = if found {
            None
        } else if object.complete {
            Some(RegistryPackageDiscoveryObservation::Missing {
                source: source.id(),
                proof: missing_proof(&request.key, object.proof),
                observed_at_millis: object.observed_at_millis,
            })
        } else {
            Some(unavailable(
                Some(source.id()),
                "the registry records this version without distribution files",
            ))
        };
        // Releasing the current task witness allows replacement while queued
        // witnesses pin earlier objects inside the accounted cache ceiling.
        request.cached = None;
        self.package_metadata.insert(request.key, object);
        outcome
    }

    #[cfg(test)]
    fn observe_package(
        &mut self,
        package: &PackageCoordinate,
    ) -> Option<RegistryPackageDiscoveryObservation> {
        match self.prepare_package(package, [1; 16], 1) {
            PackageMetadataPreparation::Cached(observation) => observation,
            PackageMetadataPreparation::Fetch(request) => {
                self.admit_package_completion(request.fetch(|| false), [1; 16])
            }
        }
    }
}

fn missing_proof(key: &ObjectKey, response_proof: [u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.registry.package-metadata.absence.v1\0");
    for identity in [&key.endpoint, key.parser, &key.package] {
        hasher.update(&(identity.len() as u64).to_le_bytes());
        hasher.update(identity.as_bytes());
    }
    hasher.update(&response_proof);
    *hasher.finalize().as_bytes()
}

impl PackageMetadataCache {
    fn insert(&mut self, key: ObjectKey, object: CachedObject) {
        if self
            .objects
            .get(&key)
            .is_some_and(|value| Arc::strong_count(value) > 1)
        {
            return;
        }
        if let Some(previous) = self.objects.remove(&key) {
            self.encoded_bytes = self.encoded_bytes.saturating_sub(previous.encoded_bytes);
        }
        while !self.objects.is_empty()
            && (self.objects.len() >= MAX_CACHE_OBJECTS
                || self.encoded_bytes.saturating_add(object.encoded_bytes)
                    > MAX_CACHE_ENCODED_BYTES)
        {
            let oldest = self
                .objects
                .iter()
                .filter(|(_, value)| Arc::strong_count(value) == 1)
                .min_by_key(|(_, value)| value.observed)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest
                && let Some(old) = self.objects.remove(&oldest)
            {
                self.encoded_bytes = self.encoded_bytes.saturating_sub(old.encoded_bytes);
            } else {
                return;
            }
        }
        self.encoded_bytes = self.encoded_bytes.saturating_add(object.encoded_bytes);
        self.objects.insert(key, Arc::new(object));
    }
}

fn unavailable(source: Option<[u8; 32]>, reason: &str) -> RegistryPackageDiscoveryObservation {
    RegistryPackageDiscoveryObservation::Unavailable {
        source,
        reason: ProductText::new(reason.to_owned())
            .expect("metadata failure explanations are bounded"),
    }
}

fn go_proxy_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if character.is_ascii_uppercase() {
                vec!['!', character.to_ascii_lowercase()]
            } else {
                vec![character]
            }
        })
        .collect()
}

/// Fetches NuGet point metadata only through the selected source's V3 service
/// index. The full per-package registration is walked under strict aggregate
/// bounds so only a fully traversed registration can establish exact absence.
fn fetch_nuget_registration(
    service_index_url: &str,
    package_name: &str,
    package: &PackageCoordinate,
    cancelled: impl Fn() -> bool,
    remaining_budget: impl Fn() -> Duration,
) -> Result<ObjectResponse, DiscoveryStoreError> {
    let service_index = fetch_object(
        service_index_url,
        MAX_NUGET_SERVICE_INDEX_BYTES,
        &cancelled,
        &remaining_budget,
        None,
    )?;
    let ObjectResponse::Modified {
        bytes: service_bytes,
        ..
    } = service_index
    else {
        // A missing or not-modified service index is not evidence that a
        // package is absent. In particular, do not fall back to nuget.org.
        return Err(DiscoveryStoreError::Corrupt);
    };
    let registration_base = nuget_registration_base(&service_bytes, service_index_url)?;
    let registration_index_url = format!(
        "{}/{}/index.json",
        registration_base.trim_end_matches('/'),
        package_name.to_ascii_lowercase()
    );
    require_nuget_resource_url(&registration_index_url, &registration_base)?;
    require_nuget_path_under(&registration_index_url, &registration_base)?;

    let registration = fetch_nuget_document(
        &registration_index_url,
        MAX_NUGET_REGISTRATION_DOCUMENT_BYTES,
        &cancelled,
        &remaining_budget,
    )?;
    let registration_bytes = match registration {
        ObjectResponse::Modified { bytes, .. } => bytes,
        ObjectResponse::Missing { proof } => {
            return Ok(ObjectResponse::Missing {
                proof: nuget_proof(&service_bytes, &registration_index_url, &proof),
            });
        }
        ObjectResponse::Unchanged | ObjectResponse::Nuget { .. } => {
            return Err(DiscoveryStoreError::Corrupt);
        }
    };

    let root: serde_json::Value =
        serde_json::from_slice(&registration_bytes).map_err(|_| DiscoveryStoreError::Decode)?;
    let root_object = root.as_object().ok_or(DiscoveryStoreError::Decode)?;
    if root_object
        .get("@id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|id| id != registration_index_url)
    {
        return Err(DiscoveryStoreError::Corrupt);
    }

    let mut proof = blake3::Hasher::new();
    proof.update(b"backend.nuget.registration-point.v1\0");
    nuget_hash_part(&mut proof, service_index_url.as_bytes());
    nuget_hash_part(&mut proof, &service_bytes);
    nuget_hash_part(&mut proof, registration_index_url.as_bytes());
    nuget_hash_part(&mut proof, &registration_bytes);

    let items = root_object
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryStoreError::Decode)?;
    if root_object
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|count| usize::try_from(count).ok() != Some(items.len()))
    {
        return Err(DiscoveryStoreError::Corrupt);
    }
    let mut leaves = Vec::new();
    let mut visited_pages = BTreeSet::new();
    let mut total_bytes = service_bytes
        .len()
        .checked_add(registration_bytes.len())
        .ok_or(DiscoveryStoreError::Bounds)?;
    collect_nuget_registration_items(
        items,
        &registration_index_url,
        &registration_base,
        &mut visited_pages,
        &mut leaves,
        &mut total_bytes,
        &mut proof,
        0,
        &cancelled,
        &remaining_budget,
    )?;
    if leaves.len() > MAX_NUGET_REGISTRATION_LEAVES {
        return Err(DiscoveryStoreError::Bounds);
    }
    let proof = *proof.finalize().as_bytes();
    let (releases, exact_complete) =
        parse_nuget_registration_observation(&leaves, package_name, package, proof)?;
    Ok(ObjectResponse::Nuget {
        releases,
        complete: exact_complete,
        proof,
    })
}

fn collect_nuget_registration_items(
    items: &[serde_json::Value],
    registration_index_url: &str,
    registration_base: &str,
    visited_pages: &mut BTreeSet<String>,
    leaves: &mut Vec<serde_json::Value>,
    total_bytes: &mut usize,
    proof: &mut blake3::Hasher,
    depth: usize,
    cancelled: &impl Fn() -> bool,
    remaining_budget: &impl Fn() -> Duration,
) -> Result<(), DiscoveryStoreError> {
    if depth > MAX_NUGET_REGISTRATION_DEPTH {
        return Err(DiscoveryStoreError::Bounds);
    }
    for item in items {
        if cancelled() {
            return Err(DiscoveryStoreError::Cancelled);
        }
        if leaves.len() >= MAX_NUGET_REGISTRATION_LEAVES {
            return Err(DiscoveryStoreError::Bounds);
        }
        let object = item.as_object().ok_or(DiscoveryStoreError::Decode)?;
        if object.contains_key("catalogEntry") {
            leaves.push(item.clone());
            continue;
        }
        if let Some(inline) = object.get("items").and_then(serde_json::Value::as_array) {
            if let Some(count) = object.get("count").and_then(serde_json::Value::as_u64)
                && usize::try_from(count).ok() != Some(inline.len())
            {
                return Err(DiscoveryStoreError::Corrupt);
            }
            collect_nuget_registration_items(
                inline,
                registration_index_url,
                registration_base,
                visited_pages,
                leaves,
                total_bytes,
                proof,
                depth + 1,
                cancelled,
                remaining_budget,
            )?;
            continue;
        }

        let page_url = object
            .get("@id")
            .and_then(serde_json::Value::as_str)
            .ok_or(DiscoveryStoreError::Decode)?;
        require_nuget_resource_url(page_url, registration_base)?;
        require_nuget_path_under(page_url, registration_base)?;
        if !visited_pages.insert(page_url.to_owned()) {
            return Err(DiscoveryStoreError::Corrupt);
        }
        if visited_pages.len() > MAX_NUGET_REGISTRATION_PAGES {
            return Err(DiscoveryStoreError::Bounds);
        }
        let page_response = fetch_nuget_document(
            page_url,
            MAX_NUGET_REGISTRATION_DOCUMENT_BYTES,
            cancelled,
            remaining_budget,
        )?;
        let ObjectResponse::Modified { bytes, .. } = page_response else {
            // A missing page cannot establish package absence.
            return Err(DiscoveryStoreError::Corrupt);
        };
        *total_bytes = total_bytes
            .checked_add(bytes.len())
            .filter(|size| *size <= MAX_NUGET_REGISTRATION_TOTAL_BYTES)
            .ok_or(DiscoveryStoreError::Bounds)?;
        nuget_hash_part(proof, page_url.as_bytes());
        nuget_hash_part(proof, &bytes);
        let page: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| DiscoveryStoreError::Decode)?;
        let page_object = page.as_object().ok_or(DiscoveryStoreError::Decode)?;
        if page_object
            .get("@id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id != page_url)
            || page_object
                .get("parent")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|parent| parent != registration_index_url)
        {
            return Err(DiscoveryStoreError::Corrupt);
        }
        let page_items = page_object
            .get("items")
            .and_then(serde_json::Value::as_array)
            .ok_or(DiscoveryStoreError::Decode)?;
        if page_object
            .get("count")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| usize::try_from(count).ok() != Some(page_items.len()))
        {
            return Err(DiscoveryStoreError::Corrupt);
        }
        collect_nuget_registration_items(
            page_items,
            registration_index_url,
            registration_base,
            visited_pages,
            leaves,
            total_bytes,
            proof,
            depth + 1,
            cancelled,
            remaining_budget,
        )?;
    }
    Ok(())
}

fn parse_nuget_registration_observation(
    leaves: &[serde_json::Value],
    package_name: &str,
    package: &PackageCoordinate,
    proof: [u8; 32],
) -> Result<(Vec<DiscoveryReleaseObservation>, bool), DiscoveryStoreError> {
    if leaves.len() > MAX_NUGET_REGISTRATION_LEAVES {
        return Err(DiscoveryStoreError::Bounds);
    }
    let admitted = admit_registry_coordinate(package).map_err(|_| DiscoveryStoreError::Decode)?;
    let wanted_version =
        normalize_nuget_version(admitted.version().as_str()).ok_or(DiscoveryStoreError::Decode)?;
    let mut selected = None;
    let mut seen_versions = BTreeSet::new();
    let mut complete = true;
    for leaf in leaves {
        let Some(entry) = leaf
            .get("catalogEntry")
            .and_then(serde_json::Value::as_object)
        else {
            complete = false;
            continue;
        };
        let (Some(id), Some(version)) = (
            entry.get("id").and_then(serde_json::Value::as_str),
            entry.get("version").and_then(serde_json::Value::as_str),
        ) else {
            complete = false;
            continue;
        };
        if !id.eq_ignore_ascii_case(package_name) {
            return Err(DiscoveryStoreError::Corrupt);
        }
        let Some(normalized_version) = normalize_nuget_version(version) else {
            complete = false;
            continue;
        };
        if !seen_versions.insert(normalized_version.clone()) {
            return Err(DiscoveryStoreError::Corrupt);
        }
        if normalized_version != wanted_version {
            continue;
        }
        let Some(listed) = entry.get("listed").and_then(serde_json::Value::as_bool) else {
            complete = false;
            continue;
        };
        let standing = if listed {
            DiscoveryStanding::Published
        } else {
            DiscoveryStanding::Yanked
        };
        let mut metadata = DiscoveryMetadata::default();
        metadata.aliases = DiscoveryFacet::Absent;
        metadata.description = nuget_text_facet(entry, "description");
        metadata.keywords = nuget_tags_facet(entry);
        metadata.license = nuget_text_facet(entry, "licenseExpression");
        metadata.published_at = nuget_text_facet(entry, "published");
        metadata.deprecation = nuget_deprecation_facet(entry);
        metadata.yanked = DiscoveryFacet::Known(!listed);
        metadata.advisories = nuget_advisories_facet(entry)?;
        metadata.downloads = entry
            .get("downloads")
            .and_then(serde_json::Value::as_u64)
            .map_or(DiscoveryFacet::Unknown, DiscoveryFacet::Known);
        metadata.admit().map_err(|_| DiscoveryStoreError::Decode)?;
        if selected.is_some() {
            return Err(DiscoveryStoreError::Corrupt);
        }
        selected = Some(DiscoveryReleaseObservation {
            coordinate: package.clone(),
            standing,
            source_event_time: None,
            proof,
            metadata,
        });
    }
    Ok((selected.into_iter().collect(), complete))
}

fn nuget_text_facet(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> DiscoveryFacet<String> {
    match object.get(key) {
        Some(serde_json::Value::String(value)) if value.len() <= 4096 && !value.contains('\0') => {
            DiscoveryFacet::Known(value.clone())
        }
        Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
        Some(serde_json::Value::String(_)) => DiscoveryFacet::Unknown,
        Some(_) | None => DiscoveryFacet::Unknown,
    }
}

fn nuget_tags_facet(
    object: &serde_json::Map<String, serde_json::Value>,
) -> DiscoveryFacet<Vec<String>> {
    match object.get("tags") {
        Some(serde_json::Value::String(value)) if value.len() <= 32 * 1024 => {
            let tags = value
                .split_ascii_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            (tags.len() <= 128)
                .then_some(DiscoveryFacet::Known(tags))
                .unwrap_or(DiscoveryFacet::Unknown)
        }
        Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
        Some(serde_json::Value::String(_)) => DiscoveryFacet::Unknown,
        Some(_) | None => DiscoveryFacet::Unknown,
    }
}

fn nuget_deprecation_facet(
    object: &serde_json::Map<String, serde_json::Value>,
) -> DiscoveryFacet<String> {
    match object.get("deprecation") {
        Some(serde_json::Value::Null) => DiscoveryFacet::Absent,
        Some(serde_json::Value::Object(deprecation)) => match deprecation.get("message") {
            Some(serde_json::Value::String(message))
                if message.len() <= 4096 && !message.contains('\0') =>
            {
                DiscoveryFacet::Known(message.clone())
            }
            _ => DiscoveryFacet::Unknown,
        },
        Some(_) | None => DiscoveryFacet::Unknown,
    }
}

fn nuget_advisories_facet(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<DiscoveryFacet<Vec<DiscoveryAdvisory>>, DiscoveryStoreError> {
    let Some(value) = object.get("vulnerabilities") else {
        return Ok(DiscoveryFacet::Unknown);
    };
    if value.is_null() {
        return Ok(DiscoveryFacet::Unknown);
    }
    let Some(values) = value.as_array() else {
        return Err(DiscoveryStoreError::Decode);
    };
    if values.len() > 128 {
        return Err(DiscoveryStoreError::Bounds);
    }
    let mut advisories = Vec::with_capacity(values.len());
    for value in values {
        let advisory = value.as_object().ok_or(DiscoveryStoreError::Decode)?;
        let url = advisory
            .get("advisoryUrl")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or(DiscoveryStoreError::Decode)?;
        let severity = match advisory.get("severity") {
            Some(serde_json::Value::String(value))
                if matches!(value.as_str(), "0" | "1" | "2" | "3") =>
            {
                value.clone()
            }
            Some(serde_json::Value::Number(value))
                if value.as_u64().is_some_and(|value| value <= 3) =>
            {
                value.to_string()
            }
            _ => return Err(DiscoveryStoreError::Decode),
        };
        advisories.push(DiscoveryAdvisory {
            id: url.to_owned(),
            aliases: DiscoveryFacet::Absent,
            summary: DiscoveryFacet::Unknown,
            severity: DiscoveryFacet::Known(severity),
            fixed_in: DiscoveryFacet::Unknown,
        });
    }
    Ok(DiscoveryFacet::Known(advisories))
}

fn normalize_nuget_version(value: &str) -> Option<String> {
    let (version, _metadata) = value
        .split_once('+')
        .map_or((value, None), |(v, m)| (v, Some(m)));
    let (core, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(v, p)| (v, Some(p)));
    if let Some(metadata) = _metadata
        && (metadata.is_empty()
            || !metadata
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))
    {
        return None;
    }
    let components = core.split('.').collect::<Vec<_>>();
    if components.is_empty() || components.len() > 4 {
        return None;
    }
    let mut numbers = components
        .iter()
        .map(|component| {
            component
                .parse::<u64>()
                .ok()
                .map(|number| number.to_string())
        })
        .collect::<Option<Vec<_>>>()?;
    while numbers.len() < 3 {
        numbers.push("0".to_owned());
    }
    if numbers.len() == 4 && numbers[3] == "0" {
        numbers.pop();
    }
    let mut normalized = numbers.join(".");
    if let Some(prerelease) = prerelease {
        if prerelease.is_empty()
            || !prerelease
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            return None;
        }
        normalized.push('-');
        normalized.push_str(&prerelease.to_ascii_lowercase());
    }
    Some(normalized)
}

fn nuget_service_index_url(endpoint: &str) -> String {
    if endpoint.ends_with("/index.json") {
        endpoint.to_owned()
    } else if endpoint.ends_with("/v3") {
        format!("{endpoint}/index.json")
    } else {
        format!("{}/v3/index.json", endpoint.trim_end_matches('/'))
    }
}

fn nuget_registration_base(
    bytes: &[u8],
    service_index_url: &str,
) -> Result<String, DiscoveryStoreError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryStoreError::Decode)?;
    let resources = value
        .get("resources")
        .and_then(serde_json::Value::as_array)
        .ok_or(DiscoveryStoreError::Decode)?;
    let mut registration = None;
    for resource in resources {
        let Some(id) = resource.get("@id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let types = match resource.get("@type") {
            Some(serde_json::Value::String(value)) => vec![value.as_str()],
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect(),
            _ => continue,
        };
        if types
            .iter()
            .any(|value| *value == "RegistrationsBaseUrl/3.6.0")
        {
            require_nuget_resource_url(id, service_index_url)?;
            if registration.replace(id.to_owned()).is_some() {
                return Err(DiscoveryStoreError::Corrupt);
            }
        }
    }
    registration.ok_or(DiscoveryStoreError::Corrupt)
}

fn require_nuget_resource_url(resource: &str, parent: &str) -> Result<(), DiscoveryStoreError> {
    let resource_uri = resource
        .parse::<ureq::http::Uri>()
        .map_err(|_| DiscoveryStoreError::Corrupt)?;
    let parent_uri = parent
        .parse::<ureq::http::Uri>()
        .map_err(|_| DiscoveryStoreError::Corrupt)?;
    let (
        Some(resource_scheme),
        Some(resource_authority),
        Some(parent_scheme),
        Some(parent_authority),
    ) = (
        resource_uri.scheme_str(),
        resource_uri.authority(),
        parent_uri.scheme_str(),
        parent_uri.authority(),
    )
    else {
        return Err(DiscoveryStoreError::Corrupt);
    };
    let same_authority = resource_authority
        .host()
        .eq_ignore_ascii_case(parent_authority.host())
        && resource_authority.port_u16() == parent_authority.port_u16();
    let safe_scheme = resource_scheme == "https"
        || (resource_scheme == "http"
            && resource_authority
                .host()
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback()));
    if !safe_scheme
        || resource_scheme != parent_scheme
        || !same_authority
        || resource_authority.as_str().contains('@')
        || resource_uri.query().is_some()
        || resource_uri.path().is_empty()
        || resource.contains('#')
    {
        return Err(DiscoveryStoreError::Corrupt);
    }
    Ok(())
}

fn require_nuget_path_under(resource: &str, base: &str) -> Result<(), DiscoveryStoreError> {
    let resource_uri = resource
        .parse::<ureq::http::Uri>()
        .map_err(|_| DiscoveryStoreError::Corrupt)?;
    let base_uri = base
        .parse::<ureq::http::Uri>()
        .map_err(|_| DiscoveryStoreError::Corrupt)?;
    let base_path = base_uri.path().trim_end_matches('/');
    let expected_prefix = format!("{base_path}/");
    if !resource_uri.path().starts_with(&expected_prefix) {
        return Err(DiscoveryStoreError::Corrupt);
    }
    Ok(())
}

fn nuget_hash_part(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn nuget_proof(service_index: &[u8], registration_url: &str, missing: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.nuget.registration-absence.v1\0");
    nuget_hash_part(&mut hasher, service_index);
    nuget_hash_part(&mut hasher, registration_url.as_bytes());
    hasher.update(missing);
    *hasher.finalize().as_bytes()
}

fn parse_object(
    ecosystem: RegistryEcosystem,
    bytes: &[u8],
    name: &str,
    package: &PackageCoordinate,
) -> Result<(Vec<DiscoveryReleaseObservation>, bool), DiscoveryStoreError> {
    match ecosystem {
        RegistryEcosystem::Npm => {
            let version =
                admit_registry_coordinate(package).map_err(|_| DiscoveryStoreError::Decode)?;
            let (projected, _) = exact_version_document(
                bytes,
                "versions",
                version.version().as_str(),
                MAX_NPM_PACKUMENT_BYTES,
            )?;
            let mut parsed = parse_npm_packument_document(&projected, name, 0, 1)?;
            for release in &mut parsed.releases {
                release.proof = *blake3::hash(bytes).as_bytes();
            }
            Ok((parsed.releases, true))
        }
        RegistryEcosystem::Pypi => {
            let version =
                admit_registry_coordinate(package).map_err(|_| DiscoveryStoreError::Decode)?;
            let (projected, presence) = exact_version_document(
                bytes,
                "releases",
                version.version().as_str(),
                MAX_SOURCE_BODY_BYTES,
            )?;
            let mut parsed = parse_pypi_project_metadata(&projected, name, 1)?;
            for release in &mut parsed.releases {
                release.proof = *blake3::hash(bytes).as_bytes();
            }
            let admits_absence =
                presence == ExactVersionPresence::Absent || !parsed.releases.is_empty();
            Ok((parsed.releases, admits_absence))
        }
        RegistryEcosystem::Golang => {
            let value: serde_json::Value =
                serde_json::from_slice(bytes).map_err(|_| DiscoveryStoreError::Decode)?;
            let version = value
                .get("Version")
                .and_then(serde_json::Value::as_str)
                .ok_or(DiscoveryStoreError::Decode)?;
            let time = value
                .get("Time")
                .and_then(serde_json::Value::as_str)
                .ok_or(DiscoveryStoreError::Decode)?;
            let admitted =
                admit_registry_coordinate(package).map_err(|_| DiscoveryStoreError::Decode)?;
            if version != admitted.version().as_str() || time.is_empty() {
                return Err(DiscoveryStoreError::Decode);
            }
            backend_engine::registry::DiscoveryTimestamp::parse_nuget_catalog_timestamp(time)?;
            let metadata = DiscoveryMetadata {
                published_at: DiscoveryFacet::Known(time.to_owned()),
                downloads: DiscoveryFacet::Absent,
                ..DiscoveryMetadata::default()
            };
            metadata.admit()?;
            Ok((
                vec![DiscoveryReleaseObservation {
                    coordinate: package.clone(),
                    standing: DiscoveryStanding::Published,
                    source_event_time: None,
                    proof: *blake3::hash(bytes).as_bytes(),
                    metadata,
                }],
                true,
            ))
        }
        _ => Err(DiscoveryStoreError::Decode),
    }
}

/// Parse the whole bounded JSON object, then retain only the exact target for
/// the existing ecosystem schema/facet admission. Namespace listing limits
/// cannot determine whether a version-pinned request exists.
fn exact_version_document(
    bytes: &[u8],
    field: &str,
    version: &str,
    maximum: usize,
) -> Result<(Vec<u8>, ExactVersionPresence), DiscoveryStoreError> {
    if bytes.len() > maximum {
        return Err(DiscoveryStoreError::Bounds);
    }
    let mut value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| DiscoveryStoreError::Decode)?;
    let object = value.as_object_mut().ok_or(DiscoveryStoreError::Decode)?;
    let Some(serde_json::Value::Object(mut versions)) = object.remove(field) else {
        return Err(DiscoveryStoreError::Decode);
    };
    let mut selected = serde_json::Map::new();
    let mut presence = ExactVersionPresence::Absent;
    if let Some(metadata) = versions.remove(version) {
        presence = ExactVersionPresence::Present;
        if field == "versions" {
            let release = metadata.as_object().ok_or(DiscoveryStoreError::Decode)?;
            if release
                .get("version")
                .is_some_and(|actual| actual.as_str() != Some(version))
            {
                return Err(DiscoveryStoreError::Decode);
            }
        }
        selected.insert(version.to_owned(), metadata);
    }
    object.insert(field.to_owned(), serde_json::Value::Object(selected));
    let bytes = serde_json::to_vec(&value).map_err(|_| DiscoveryStoreError::Decode)?;
    if bytes.len() > maximum {
        return Err(DiscoveryStoreError::Bounds);
    }
    Ok((bytes, presence))
}

fn request_timeout(remaining_budget: Duration) -> Option<Duration> {
    let timeout = REQUEST_TIMEOUT.min(remaining_budget);
    (!timeout.is_zero()).then_some(timeout)
}

fn fetch_object(
    url: &str,
    maximum: usize,
    cancelled: impl Fn() -> bool,
    remaining_budget: impl Fn() -> Duration,
    etag: Option<&str>,
) -> Result<ObjectResponse, DiscoveryStoreError> {
    fetch_object_with_encoding(url, maximum, cancelled, remaining_budget, etag, "identity")
}

fn fetch_nuget_document(
    url: &str,
    maximum: usize,
    cancelled: impl Fn() -> bool,
    remaining_budget: impl Fn() -> Duration,
) -> Result<ObjectResponse, DiscoveryStoreError> {
    fetch_object_with_encoding(url, maximum, cancelled, remaining_budget, None, "gzip")
}

fn fetch_object_with_encoding(
    url: &str,
    maximum: usize,
    cancelled: impl Fn() -> bool,
    remaining_budget: impl Fn() -> Duration,
    etag: Option<&str>,
    accept_encoding: &str,
) -> Result<ObjectResponse, DiscoveryStoreError> {
    if cancelled() {
        return Err(DiscoveryStoreError::Cancelled);
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut request = agent
        .get(url)
        .header("accept", "application/json")
        .header("accept-encoding", accept_encoding)
        .header("user-agent", "backend-index-compiler-discovery/1");
    if let Some(etag) = etag {
        request = request.header("if-none-match", etag);
    }
    // Apply the remaining absolute observation budget at request scope as
    // late as possible, so queue wait cannot leave a stale full 20s timeout.
    let timeout = request_timeout(remaining_budget()).ok_or(DiscoveryStoreError::Cancelled)?;
    let mut response = request
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()
        .map_err(|_| DiscoveryStoreError::Io(io::Error::other("metadata request failed")))?;
    let status = response.status().as_u16();
    if status == 304 {
        return if etag.is_some() {
            Ok(ObjectResponse::Unchanged)
        } else {
            Err(DiscoveryStoreError::Corrupt)
        };
    }
    let mut bytes = Vec::new();
    let mut reader = response.body_mut().as_reader();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        if cancelled() {
            return Err(DiscoveryStoreError::Cancelled);
        }
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        if bytes
            .len()
            .checked_add(count)
            .is_none_or(|length| length > maximum)
        {
            return Err(DiscoveryStoreError::Bounds);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if cancelled() {
        return Err(DiscoveryStoreError::Cancelled);
    }
    if status == 404 || status == 410 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.registry.package-metadata.http-absence.v1\0");
        hasher.update(&status.to_le_bytes());
        hasher.update(&bytes);
        return Ok(ObjectResponse::Missing {
            proof: *hasher.finalize().as_bytes(),
        });
    }
    if status != 200 {
        return Err(DiscoveryStoreError::Io(io::Error::other(format!(
            "metadata source returned HTTP {status}"
        ))));
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 1024)
        .map(str::to_owned);
    Ok(ObjectResponse::Modified { bytes, etag })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    const REQUESTS: &str = r#"{"info":{"name":"requests","version":"2.34.2"},"releases":{"2.34.2":[{"yanked":false,"upload_time_iso_8601":"2026-10-01T12:00:00Z"}]}}"#;
    const SAMPLEPROJECT: &str = include_str!("fixtures/pypi-sampleproject-project-20261006.json");

    #[test]
    fn metadata_get_timeout_is_capped_by_remaining_observation_budget() {
        assert_eq!(
            request_timeout(Duration::from_secs(90)),
            Some(REQUEST_TIMEOUT)
        );
        assert_eq!(
            request_timeout(Duration::from_secs(3)),
            Some(Duration::from_secs(3))
        );
        assert_eq!(request_timeout(Duration::ZERO), None);
    }

    #[test]
    fn nuget_point_metadata_uses_configured_registration_and_normalizes_versions() {
        let (endpoint, worker) = nuget_server(|address| {
            let registration = format!("http://{address}/private/v3/registration/");
            let root_url = format!("{registration}demo/index.json");
            vec![
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"resources":[{{"@id":"{registration}","@type":["RegistrationsBaseUrl/3.6.0"]}}]}}"#
                    ),
                ),
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"@id":"{root_url}","count":1,"items":[{{"catalogEntry":{{"id":"DEMO","version":"1.0.0.0+build42","listed":true,"published":"2026-10-20T00:00:00Z","downloads":12,"description":"exact package"}}}}]}}"#
                    ),
                ),
            ]
        });
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:nuget/demo@1.0")
            .expect("canonical pinned NuGet coordinate");
        assert!(owner.observe_package(&package).is_none());
        let fact = owner
            .store
            .facts()
            .map(|(_, fact)| fact.clone())
            .find(|fact| fact.coordinate == package)
            .expect("admitted exact version fact");
        assert_eq!(fact.standing, DiscoveryStanding::Published);
        assert_eq!(fact.metadata.downloads, DiscoveryFacet::Known(12));
        let requests = worker.join().expect("NuGet fixture worker");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /v3/index.json "));
        assert!(requests[1].starts_with("GET /private/v3/registration/demo/index.json "));
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("accept-encoding: gzip")
        );
    }

    #[test]
    fn nuget_private_source_404_is_distinct_from_incomplete_registration() {
        let (endpoint, worker) = nuget_server(|address| {
            let registration = format!("http://{address}/private/registration/");
            vec![
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"resources":[{{"@id":"{registration}","@type":"RegistrationsBaseUrl/3.6.0"}}]}}"#
                    ),
                ),
                ("404 Not Found".to_owned(), "not found".to_owned()),
            ]
        });
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:nuget/internal.widget@2.0.0")
            .expect("private package coordinate");
        assert!(matches!(
            owner.observe_package(&package),
            Some(RegistryPackageDiscoveryObservation::Missing { .. })
        ));
        let requests = worker.join().expect("NuGet fixture worker");
        assert_eq!(requests.len(), 2, "no public registry fallback request");
        assert!(requests[0].starts_with("GET /v3/index.json "));
        assert!(requests[1].starts_with("GET /private/registration/internal.widget/index.json "));
    }

    #[test]
    fn nuget_registration_page_mismatch_cannot_prove_absence() {
        let (endpoint, worker) = nuget_server(|address| {
            let registration = format!("http://{address}/registration/");
            let root_url = format!("{registration}demo/index.json");
            let page_url = format!("{registration}demo/page-0.json");
            vec![
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"resources":[{{"@id":"{registration}","@type":"RegistrationsBaseUrl/3.6.0"}}]}}"#
                    ),
                ),
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"@id":"{root_url}","count":1,"items":[{{"@id":"{page_url}","count":1,"lower":"1.0.0","upper":"1.0.0"}}]}}"#
                    ),
                ),
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"@id":"{page_url}","parent":"{root_url}","count":1,"items":[{{"catalogEntry":{{"id":"some-other-package","version":"1.0.0","listed":true}}}}]}}"#
                    ),
                ),
            ]
        });
        let mut owner = gateway(endpoint);
        let package =
            PackageCoordinate::parse("pkg:nuget/demo@2.0.0").expect("NuGet package coordinate");
        assert!(matches!(
            owner.observe_package(&package),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert_eq!(worker.join().expect("NuGet fixture worker").len(), 3);
    }

    #[test]
    fn nuget_matching_leaf_without_required_standing_is_incomplete_not_missing() {
        let (endpoint, worker) = nuget_server(|address| {
            let registration = format!("http://{address}/registration/");
            let root_url = format!("{registration}demo/index.json");
            vec![
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"resources":[{{"@id":"{registration}","@type":"RegistrationsBaseUrl/3.6.0"}}]}}"#
                    ),
                ),
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"@id":"{root_url}","count":1,"items":[{{"catalogEntry":{{"id":"Demo","version":"2.1.0"}}}}]}}"#
                    ),
                ),
            ]
        });
        let mut owner = gateway(endpoint);
        let package =
            PackageCoordinate::parse("pkg:nuget/demo@2.1.0").expect("NuGet package coordinate");
        assert!(matches!(
            owner.observe_package(&package),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert!(owner.store.facts().next().is_none());
        assert_eq!(worker.join().expect("NuGet fixture worker").len(), 2);
    }

    #[test]
    fn nuget_other_registered_version_is_exact_absence_not_version_aliasing() {
        let (endpoint, worker) = nuget_server(|address| {
            let registration = format!("http://{address}/registration/");
            let root_url = format!("{registration}demo/index.json");
            vec![
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"resources":[{{"@id":"{registration}","@type":"RegistrationsBaseUrl/3.6.0"}}]}}"#
                    ),
                ),
                (
                    "200 OK".to_owned(),
                    format!(
                        r#"{{"@id":"{root_url}","count":1,"items":[{{"catalogEntry":{{"id":"demo","version":"1.0.1","listed":true}}}}]}}"#
                    ),
                ),
            ]
        });
        let mut owner = gateway(endpoint);
        let package =
            PackageCoordinate::parse("pkg:nuget/demo@1.0.0").expect("NuGet package coordinate");
        assert!(matches!(
            owner.observe_package(&package),
            Some(RegistryPackageDiscoveryObservation::Missing { .. })
        ));
        assert!(owner.store.facts().next().is_none());
        assert_eq!(worker.join().expect("NuGet fixture worker").len(), 2);
    }

    #[test]
    fn nuget_registration_links_cannot_escape_the_configured_origin() {
        let (endpoint, worker) = nuget_server(|_address| {
            vec![(
                "200 OK".to_owned(),
                r#"{"resources":[{"@id":"https://api.nuget.org/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}]}"#.to_owned(),
            )]
        });
        let mut owner = gateway(endpoint);
        let package =
            PackageCoordinate::parse("pkg:nuget/demo@1.0.0").expect("NuGet package coordinate");
        assert!(matches!(
            owner.observe_package(&package),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert!(owner.store.facts().next().is_none());
        assert_eq!(worker.join().expect("NuGet fixture worker").len(), 1);
    }

    #[test]
    fn nuget_version_normalization_matches_documented_core_build_and_prerelease_rules() {
        assert_eq!(normalize_nuget_version("1.00"), Some("1.0.0".to_owned()));
        assert_eq!(
            normalize_nuget_version("1.0.0.0+githash"),
            Some("1.0.0".to_owned())
        );
        assert_eq!(
            normalize_nuget_version("1.0.0-Alpha.1"),
            Some("1.0.0-alpha.1".to_owned())
        );
        assert_eq!(normalize_nuget_version("1..0"), None);
        assert_eq!(normalize_nuget_version("1.0.0+"), None);
    }

    #[test]
    fn expired_metadata_budget_does_not_issue_a_get() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let url = format!(
            "http://{}/metadata",
            listener.local_addr().expect("loopback address")
        );
        let result = fetch_object(&url, 1024, || false, || Duration::ZERO, None);
        assert!(matches!(result, Err(DiscoveryStoreError::Cancelled)));
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn metadata_body_bound_is_enforced_for_http_response_bytes() {
        let (endpoint, server) = server(vec![("200 OK", "123456789")]);
        let url = format!("{}/bounded", endpoint.as_str());
        assert!(matches!(
            fetch_object(&url, 8, || false, || REQUEST_TIMEOUT, None),
            Err(DiscoveryStoreError::Bounds)
        ));
        assert_eq!(server.join().expect("bounded fixture").len(), 1);
    }

    fn server(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (RegistryEndpoint, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        listener.set_nonblocking(true).expect("bounded accept");
        let endpoint = RegistryEndpoint::new(
            RegistryEcosystem::Pypi,
            format!("http://{}", listener.local_addr().expect("address")),
        )
        .expect("endpoint");
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("bounded metadata fixture accept: {error}"),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .expect("blocking fixture stream");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read deadline");
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).expect("request");
                    request.push(byte[0]);
                    assert!(request.len() < 16 * 1024);
                }
                requests.push(String::from_utf8(request).expect("HTTP request"));
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nETag: \"requests-v1\"\r\nConnection: close\r\n\r\n{body}", body.len()).expect("response");
            }
            requests
        });
        (endpoint, worker)
    }

    fn nuget_server(
        responses: impl FnOnce(&str) -> Vec<(String, String)>,
    ) -> (RegistryEndpoint, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("NuGet listener");
        listener
            .set_nonblocking(true)
            .expect("bounded NuGet accept");
        let address = listener.local_addr().expect("NuGet address").to_string();
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Nuget, format!("http://{address}"))
            .expect("loopback NuGet source");
        let responses = responses(&address);
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("bounded NuGet fixture accept: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("NuGet fixture read deadline");
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).expect("NuGet request");
                    request.push(byte[0]);
                    assert!(request.len() < 16 * 1024);
                }
                requests.push(String::from_utf8(request).expect("HTTP request UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("NuGet response headers/body");
            }
            requests
        });
        (endpoint, worker)
    }

    fn gateway(endpoint: RegistryEndpoint) -> DiscoveryGateway {
        static NEXT_JOURNAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "backend-point-metadata-{}-{}-{}.journal",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
            NEXT_JOURNAL.fetch_add(1, Ordering::Relaxed)
        ));
        let (_sender, receiver) = mpsc::sync_channel(1);
        DiscoveryGateway {
            store: DiscoveryStore::open(path).expect("journal"),
            receiver,
            workers: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            metadata_sources: vec![endpoint],
            metadata_offline: false,
            package_metadata: PackageMetadataCache::default(),
        }
    }

    #[test]
    fn package_metadata_recorded_pypi_release_without_files_is_unavailable_not_missing() {
        // Official PyPI responses captured 2026-10-06: project JSON retains
        // releases["1.0"]=[] and the release-specific endpoint returns 200
        // with info.version="1.0". No files does not prove version absence.
        let release: serde_json::Value = serde_json::from_slice(include_bytes!(
            "fixtures/pypi-sampleproject-release-1.0-20261006.json"
        ))
        .expect("captured official release metadata");
        assert_eq!(release["info"]["version"], "1.0");
        assert_eq!(
            release["urls"].as_array().expect("distribution set").len(),
            0
        );
        let (endpoint, server) = server(vec![("200 OK", SAMPLEPROJECT), ("200 OK", SAMPLEPROJECT)]);
        let feed = discovery_source_identity(&endpoint);
        let mut owner = gateway(endpoint);
        let recorded =
            PackageCoordinate::parse("pkg:pypi/sampleproject@1.0").expect("recorded release");
        let unavailable = owner
            .observe_package(&recorded)
            .expect("no distributable release evidence");
        assert!(matches!(
            &unavailable,
            RegistryPackageDiscoveryObservation::Unavailable { reason, .. }
                if reason.as_str().contains("without distribution files")
        ));
        assert_eq!(
            owner.observe_package(&recorded),
            Some(unavailable),
            "the admitted unavailable observation is reused without another fetch"
        );
        assert!(
            owner
                .store
                .facts()
                .all(|(_, fact)| fact.coordinate != recorded)
        );
        assert_eq!(owner.store.progress_token(feed).sequence, 0);

        let absent =
            PackageCoordinate::parse("pkg:pypi/sampleproject@999.0.0").expect("absent release");
        assert!(matches!(
            owner.observe_package(&absent),
            Some(RegistryPackageDiscoveryObservation::Missing { .. })
        ));
        assert_eq!(owner.store.progress_token(feed).sequence, 0);
        assert_eq!(server.join().expect("bounded fixture server").len(), 2);
    }

    #[test]
    fn exact_metadata_cache_revalidates_without_advancing_namespace_cursor() {
        let (endpoint, server) = server(vec![("200 OK", REQUESTS), ("304 Not Modified", "")]);
        let feed = discovery_source_identity(&endpoint);
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("package");
        assert!(owner.observe_package(&package).is_none());
        assert!(
            owner.observe_package(&package).is_none(),
            "fresh validated object is reused"
        );
        let revision = owner.store.observation_revision();
        Arc::get_mut(
            owner
                .package_metadata
                .objects
                .values_mut()
                .next()
                .expect("object"),
        )
        .expect("unshared cached object")
        .observed = Instant::now() - Duration::from_secs(61);
        assert!(owner.observe_package(&package).is_none());
        assert!(owner.store.observation_revision() > revision);
        let requests = server.join().expect("server");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("if-none-match: \"requests-v1\"")
        );
        assert_eq!(owner.store.progress_token(feed).sequence, 0);
        assert!(owner.store.progress_token(feed).cursor.is_empty());
        assert!(owner.store.facts().all(|(source, _)| *source != feed));
        assert!(
            owner
                .store
                .facts()
                .any(|(_, fact)| fact.coordinate == package
                    && fact.metadata.yanked == DiscoveryFacet::Known(false))
        );
    }

    #[test]
    fn offline_metadata_reuses_only_fresh_current_admitted_cache_without_network() {
        let (endpoint, server) = server(vec![("200 OK", REQUESTS)]);
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("package");
        assert!(owner.observe_package(&package).is_none());
        assert_eq!(server.join().expect("one fetch").len(), 1);
        let revision = owner.store.observation_revision();
        owner.metadata_offline = true;
        assert!(matches!(
            owner.prepare_package(&package, [1; 16], 2),
            PackageMetadataPreparation::Cached(None)
        ));
        assert_eq!(
            owner.store.observation_revision(),
            revision,
            "offline reuse commits no new observation"
        );
        let absent = PackageCoordinate::parse("pkg:pypi/absent@1.0").expect("package");
        assert!(matches!(
            owner.prepare_package(&absent, [1; 16], 3),
            PackageMetadataPreparation::Cached(Some(
                RegistryPackageDiscoveryObservation::Unavailable { .. }
            ))
        ));
        Arc::get_mut(
            owner
                .package_metadata
                .objects
                .values_mut()
                .next()
                .expect("object"),
        )
        .expect("no queued witness")
        .observed = Instant::now() - DISCOVERY_REFRESH_INTERVAL;
        assert!(matches!(
            owner.prepare_package(&package, [1; 16], 4),
            PackageMetadataPreparation::Cached(Some(
                RegistryPackageDiscoveryObservation::Unavailable { .. }
            ))
        ));
        assert_eq!(owner.store.observation_revision(), revision);
    }

    #[test]
    fn invalid_metadata_is_not_cached_and_does_not_abort_another_package() {
        let (endpoint, server) = server(vec![
            ("200 OK", REQUESTS),
            ("200 OK", REQUESTS),
            ("200 OK", REQUESTS),
        ]);
        let mut owner = gateway(endpoint);
        let invalid = PackageCoordinate::parse("pkg:pypi/other@2.34.2").expect("package");
        assert!(matches!(
            owner.observe_package(&invalid),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert!(owner.package_metadata.objects.is_empty());
        assert!(matches!(
            owner.observe_package(&invalid),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        let valid = PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("package");
        assert!(owner.observe_package(&valid).is_none());
        assert_eq!(server.join().expect("server").len(), 3);
        assert_eq!(owner.package_metadata.objects.len(), 1);
    }

    #[test]
    fn absent_release_and_failed_request_have_different_closed_states() {
        let (endpoint, server) =
            server(vec![("200 OK", REQUESTS), ("503 Unavailable", "temporary")]);
        let mut owner = gateway(endpoint);
        let absent = PackageCoordinate::parse("pkg:pypi/requests@0.0.0").expect("package");
        assert!(matches!(
            owner.observe_package(&absent),
            Some(RegistryPackageDiscoveryObservation::Missing { .. })
        ));
        let another = PackageCoordinate::parse("pkg:pypi/requests@0.0.1").expect("package");
        assert!(matches!(
            owner.observe_package(&another),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert_eq!(server.join().expect("server").len(), 2);
    }

    #[test]
    fn go_info_matches_the_requested_version_and_preserves_unknown_security() {
        let package =
            PackageCoordinate::parse("pkg:golang/github.com/spf13/cobra@v1.10.1").expect("package");
        let (releases, complete) = parse_object(
            RegistryEcosystem::Golang,
            br#"{"Version":"v1.10.1","Time":"2025-09-01T12:00:00Z"}"#,
            "github.com/spf13/cobra",
            &package,
        )
        .expect("exact info");
        assert!(complete);
        assert_eq!(releases[0].metadata.advisories, DiscoveryFacet::Unknown);
        assert_eq!(releases[0].metadata.yanked, DiscoveryFacet::Unknown);
        assert!(
            parse_object(
                RegistryEcosystem::Golang,
                br#"{"Version":"v1.10.2","Time":"2025-09-01T12:00:00Z"}"#,
                "github.com/spf13/cobra",
                &package
            )
            .is_err()
        );
        assert_eq!(
            go_proxy_escape("example.com/Acme/Lib"),
            "example.com/!acme/!lib"
        );
    }

    #[test]
    fn absence_proof_binds_endpoint_parser_and_exact_version() {
        let key = ObjectKey {
            endpoint: "https://pypi.org/pypi/requests/json".to_owned(),
            parser: PARSER_IDENTITY,
            package: "pkg:pypi/requests@0.0.0".to_owned(),
        };
        let proof = missing_proof(&key, [9; 32]);
        let mut another = key.clone();
        another.package = "pkg:pypi/requests@0.0.1".to_owned();
        assert_ne!(missing_proof(&another, [9; 32]), proof);
        another = key.clone();
        another.endpoint = "https://example.org/pypi/requests/json".to_owned();
        assert_ne!(missing_proof(&another, [9; 32]), proof);
        another = key.clone();
        another.parser = "different-parser";
        assert_ne!(missing_proof(&another, [9; 32]), proof);
        assert_ne!(missing_proof(&key, [8; 32]), proof);
        assert!(PackageCoordinate::parse("pkg:pypi/requests").is_err());
        assert!(PackageCoordinate::parse("pkg:pypi/requests@").is_err());
    }

    #[test]
    fn cancellation_refuses_a_fresh_cached_object_without_a_new_commit() {
        let (endpoint, server) = server(vec![("200 OK", REQUESTS)]);
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("package");
        assert!(owner.observe_package(&package).is_none());
        assert_eq!(server.join().expect("server").len(), 1);
        let revision = owner.store.observation_revision();
        owner.cancelled.store(true, Ordering::Release);
        assert!(matches!(
            owner.observe_package(&package),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert_eq!(owner.store.observation_revision(), revision);
    }

    #[test]
    fn package_metadata_late_response_cannot_replace_a_newer_point_observation() {
        const YANKED: &str = r#"{"info":{"name":"requests","version":"2.34.2"},"releases":{"2.34.2":[{"yanked":true,"upload_time_iso_8601":"2026-10-01T12:00:00Z"}]}}"#;
        let (endpoint, server) = server(vec![("200 OK", REQUESTS), ("200 OK", YANKED)]);
        let feed = discovery_source_identity(&endpoint);
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("package");
        let PackageMetadataPreparation::Fetch(older) = owner.prepare_package(&package, [3; 16], 31)
        else {
            panic!("cold request");
        };
        let PackageMetadataPreparation::Fetch(newer) = owner.prepare_package(&package, [3; 16], 32)
        else {
            panic!("cold request");
        };
        let older = older.fetch(|| false);
        let newer = newer.fetch(|| false);
        assert!(owner.admit_package_completion(newer, [3; 16]).is_none());
        let revision = owner.store.observation_revision();
        assert!(matches!(
            owner.admit_package_completion(older, [3; 16]),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert_eq!(owner.store.observation_revision(), revision);
        assert!(
            owner
                .store
                .facts()
                .any(|(_, fact)| fact.coordinate == package
                    && fact.metadata.yanked == DiscoveryFacet::Known(true))
        );
        assert_eq!(owner.store.progress_token(feed).sequence, 0);
        assert!(owner.store.progress_token(feed).cursor.is_empty());
        assert_eq!(server.join().expect("server").len(), 2);
    }

    #[test]
    fn package_metadata_owner_epoch_change_discards_fetched_evidence() {
        let (endpoint, server) = server(vec![("200 OK", REQUESTS)]);
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("package");
        let PackageMetadataPreparation::Fetch(request) =
            owner.prepare_package(&package, [3; 16], 31)
        else {
            panic!("cold request");
        };
        assert!(matches!(
            owner.admit_package_completion(request.fetch(|| false), [4; 16]),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert!(owner.store.facts().next().is_none());
        assert!(owner.package_metadata.objects.is_empty());
        assert_eq!(server.join().expect("server").len(), 1);
    }

    #[test]
    fn package_metadata_304_without_an_admitted_cached_object_is_unavailable() {
        let (endpoint, server) = server(vec![("304 Not Modified", "")]);
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("package");
        assert!(matches!(
            owner.observe_package(&package),
            Some(RegistryPackageDiscoveryObservation::Unavailable { .. })
        ));
        assert!(owner.store.facts().next().is_none());
        assert!(owner.package_metadata.objects.is_empty());
        assert_eq!(server.join().expect("server").len(), 1);
    }

    #[test]
    fn package_metadata_cache_hit_requires_the_current_point_source_sequence() {
        let (endpoint, server) = server(vec![("200 OK", REQUESTS), ("200 OK", REQUESTS)]);
        let mut owner = gateway(endpoint);
        let absent = PackageCoordinate::parse("pkg:pypi/requests@0.0.0").expect("absent version");
        assert!(matches!(
            owner.observe_package(&absent),
            Some(RegistryPackageDiscoveryObservation::Missing { .. })
        ));
        assert!(matches!(
            owner.prepare_package(&absent, [1; 16], 2),
            PackageMetadataPreparation::Cached(Some(
                RegistryPackageDiscoveryObservation::Missing { .. }
            ))
        ));
        let present =
            PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("present version");
        assert!(owner.observe_package(&present).is_none());
        assert!(
            matches!(
                owner.prepare_package(&absent, [1; 16], 3),
                PackageMetadataPreparation::Fetch(_)
            ),
            "an older negative cache entry cannot claim the newer selected source observation"
        );
        assert_eq!(server.join().expect("server").len(), 2);
    }

    #[test]
    fn package_metadata_exact_npm_and_pypi_versions_survive_the_listing_window() {
        for (ecosystem, kind, root) in [
            (
                RegistryEcosystem::Npm,
                "versions",
                serde_json::json!({"name":"fixture"}),
            ),
            (
                RegistryEcosystem::Pypi,
                "releases",
                serde_json::json!({"info":{"name":"fixture","version":"99.0.0"}}),
            ),
        ] {
            let mut root = root;
            let mut versions = serde_json::Map::new();
            for index in 0..MAX_DISCOVERY_PAGE_ITEMS + 3 {
                let version = format!("1.0.{index}");
                let release = if ecosystem == RegistryEcosystem::Npm {
                    serde_json::json!({"version":version})
                } else {
                    serde_json::json!([{"yanked":false}])
                };
                versions.insert(version, release);
            }
            // Lexical ordering puts 10 before 2; the exact target is beyond
            // both and beyond the unrelated namespace-listing window.
            for version in ["10.0.0", "2.0.0", "99.0.0"] {
                versions.insert(
                    version.to_owned(),
                    if ecosystem == RegistryEcosystem::Npm {
                        serde_json::json!({"version":version})
                    } else {
                        serde_json::json!([{"yanked":true}])
                    },
                );
            }
            root[kind] = serde_json::Value::Object(versions);
            let bytes = serde_json::to_vec(&root).expect("full bounded document");
            let purl = if ecosystem == RegistryEcosystem::Npm {
                "pkg:npm/fixture@99.0.0"
            } else {
                "pkg:pypi/fixture@99.0.0"
            };
            let package = PackageCoordinate::parse(purl).expect("exact target");
            let (releases, complete) = parse_object(ecosystem, &bytes, "fixture", &package)
                .expect("exact schema admission");
            assert!(complete);
            assert_eq!(releases.len(), 1);
            assert_eq!(releases[0].coordinate, package);
            assert_eq!(releases[0].proof, *blake3::hash(&bytes).as_bytes());
            if ecosystem == RegistryEcosystem::Pypi {
                assert_eq!(releases[0].metadata.yanked, DiscoveryFacet::Known(true));
            }
            let absent = PackageCoordinate::parse(purl.replace("99.0.0", "98.0.0"))
                .expect("absent exact target");
            let (releases, complete) = parse_object(ecosystem, &bytes, "fixture", &absent)
                .expect("full document establishes target absence");
            assert!(complete);
            assert!(releases.is_empty());
        }
    }

    #[test]
    fn package_metadata_refresh_replaces_its_own_cache_witness_and_is_reused() {
        let (endpoint, server) = server(vec![("200 OK", REQUESTS), ("304 Not Modified", "")]);
        let mut owner = gateway(endpoint);
        let package = PackageCoordinate::parse("pkg:pypi/requests@2.34.2").expect("package");
        assert!(owner.observe_package(&package).is_none());
        Arc::get_mut(
            owner
                .package_metadata
                .objects
                .values_mut()
                .next()
                .expect("cached object"),
        )
        .expect("unshared object")
        .observed = Instant::now() - Duration::from_secs(61);
        assert!(owner.observe_package(&package).is_none());
        let cached = owner
            .package_metadata
            .objects
            .values()
            .next()
            .expect("revalidated object");
        let request = owner
            .prepare_request(&package, [1; 16], 2)
            .expect("selected authority");
        assert_eq!(cached.admitted_sequence, request.progress.sequence);
        assert!(cached.observed.elapsed() < Duration::from_secs(60));
        drop(request);
        assert!(
            matches!(
                owner.prepare_package(&package, [1; 16], 3),
                PackageMetadataPreparation::Cached(None)
            ),
            "revalidation must not refetch forever"
        );
        assert_eq!(server.join().expect("server").len(), 2);
    }

    #[test]
    fn package_metadata_exact_projection_only_withdraws_its_requested_version() {
        const BOTH: &str = r#"{"info":{"name":"requests","version":"2.0.0"},"releases":{"1.0.0":[{"yanked":false}],"2.0.0":[{"yanked":false}]}}"#;
        const SECOND: &str = r#"{"info":{"name":"requests","version":"2.0.0"},"releases":{"2.0.0":[{"yanked":false}]}}"#;
        let (endpoint, server) =
            server(vec![("200 OK", BOTH), ("200 OK", BOTH), ("200 OK", SECOND)]);
        let mut owner = gateway(endpoint);
        let first = PackageCoordinate::parse("pkg:pypi/requests@1.0.0").expect("first");
        let second = PackageCoordinate::parse("pkg:pypi/requests@2.0.0").expect("second");
        assert!(owner.observe_package(&first).is_none());
        assert!(owner.observe_package(&second).is_none());
        assert!(
            owner
                .store
                .facts()
                .filter(|(_, fact)| fact.standing == DiscoveryStanding::Published)
                .any(|(_, fact)| fact.coordinate == first)
        );
        assert!(matches!(
            owner.observe_package(&first),
            Some(RegistryPackageDiscoveryObservation::Missing { .. })
        ));
        assert!(
            owner.store.facts().any(|(_, fact)| fact.coordinate == first
                && fact.standing == DiscoveryStanding::Withdrawn)
        );
        assert!(
            owner
                .store
                .facts()
                .any(|(_, fact)| fact.coordinate == second
                    && fact.standing == DiscoveryStanding::Published)
        );
        assert_eq!(server.join().expect("server").len(), 3);
    }

    #[test]
    fn package_metadata_missing_evidence_survives_refused_cache_retention() {
        let (endpoint, server) = server(vec![("200 OK", REQUESTS); MAX_CACHE_OBJECTS + 1]);
        let mut owner = gateway(endpoint);
        for index in 0..MAX_CACHE_OBJECTS {
            let package = PackageCoordinate::parse(format!("pkg:pypi/requests@0.0.{index}"))
                .expect("absent version");
            assert!(matches!(
                owner.observe_package(&package),
                Some(RegistryPackageDiscoveryObservation::Missing { .. })
            ));
        }
        let pinned = owner
            .package_metadata
            .objects
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let package =
            PackageCoordinate::parse("pkg:pypi/requests@0.1.0").expect("new absent version");
        assert!(matches!(
            owner.observe_package(&package),
            Some(RegistryPackageDiscoveryObservation::Missing { .. })
        ));
        assert!(
            !owner
                .package_metadata
                .objects
                .keys()
                .any(|key| key.package == package.as_str())
        );
        assert_eq!(owner.package_metadata.objects.len(), MAX_CACHE_OBJECTS);
        drop(pinned);
        assert_eq!(server.join().expect("server").len(), MAX_CACHE_OBJECTS + 1);
    }
}
