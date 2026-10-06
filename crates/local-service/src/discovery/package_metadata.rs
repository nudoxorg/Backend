//! Exact package metadata reads do not depend on namespace crawl position.
//! The cache retains only admitted source observations. A 304 is usable only
//! with the exact prior object, parser identity, endpoint and queried PURL.

use super::*;
use backend_engine::registry::{PackageCoordinate, admit_registry_coordinate};
use backend_library::{ProductText, RegistryPackageDiscoveryObservation};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CACHE_OBJECTS: usize = 32;
const MAX_CACHE_ENCODED_BYTES: usize = 64 * 1024 * 1024;
const PARSER_IDENTITY: &str = "exact-registry-package-metadata-v3";

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ObjectKey {
    endpoint: String,
    parser: &'static str,
    package: String,
}

struct CachedObject {
    etag: Option<String>,
    releases: Vec<DiscoveryReleaseObservation>,
    /// Establishes the requested version only; never a complete package history.
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
}

pub(crate) struct FetchedPackageMetadata {
    pub(crate) request: PreparedPackageMetadata,
    result: Result<CachedObject, DiscoveryStoreError>,
}

impl PreparedPackageMetadata {
    pub(crate) fn fetch(self, cancelled: impl Fn() -> bool) -> FetchedPackageMetadata {
        let result = (|| {
            if cancelled() || self.cancelled.load(Ordering::Acquire) {
                return Err(DiscoveryStoreError::Cancelled);
            }
            let etag = self
                .cached
                .as_ref()
                .and_then(|cached| cached.etag.as_deref());
            let response = fetch_object(
                &self.key.endpoint,
                self.maximum,
                || cancelled() || self.cancelled.load(Ordering::Acquire),
                etag,
            )?;
            let (releases, complete, proof, etag) = match response {
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
            };
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
        if self.metadata_offline {
            return Err(unavailable(
                Some(source.id()),
                "registry metadata requests are disabled in offline mode",
            ));
        }
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
                    "the bounded package document does not establish whether this release exists",
                ))
            };
            return PackageMetadataPreparation::Cached(observation);
        }
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
        if request.owner_epoch != owner_epoch
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
                "the bounded package document does not establish whether this release exists",
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
            let projected = exact_version_document(
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
            let projected = exact_version_document(
                bytes,
                "releases",
                version.version().as_str(),
                MAX_SOURCE_BODY_BYTES,
            )?;
            let mut parsed = parse_pypi_project_metadata(&projected, name, 1)?;
            for release in &mut parsed.releases {
                release.proof = *blake3::hash(bytes).as_bytes();
            }
            Ok((parsed.releases, true))
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
) -> Result<Vec<u8>, DiscoveryStoreError> {
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
    if let Some(metadata) = versions.remove(version) {
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
    Ok(bytes)
}

fn fetch_object(
    url: &str,
    maximum: usize,
    cancelled: impl Fn() -> bool,
    etag: Option<&str>,
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
        .header("accept-encoding", "identity")
        .header("user-agent", "backend-index-compiler-discovery/1");
    if let Some(etag) = etag {
        request = request.header("if-none-match", etag);
    }
    let mut response = request
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
