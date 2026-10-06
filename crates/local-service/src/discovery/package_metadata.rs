//! Exact package metadata reads do not depend on namespace crawl position.
//! The cache retains only admitted source observations. A 304 is usable only
//! with the exact prior object, parser identity, endpoint and queried PURL.

use super::*;
use backend_engine::registry::{PackageCoordinate, admit_registry_coordinate};
use backend_library::{ProductText, RegistryPackageDiscoveryObservation};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CACHE_OBJECTS: usize = 32;
const MAX_CACHE_ENCODED_BYTES: usize = 64 * 1024 * 1024;
const PARSER_IDENTITY: &str = "exact-registry-package-metadata-v1";

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ObjectKey {
    endpoint: String,
    parser: &'static str,
    package: String,
}

struct CachedObject {
    etag: Option<String>,
    releases: Vec<DiscoveryReleaseObservation>,
    complete: bool,
    proof: [u8; 32],
    observed: Instant,
    observed_at_millis: u64,
    encoded_bytes: usize,
}

#[derive(Default)]
pub(super) struct PackageMetadataCache {
    objects: BTreeMap<ObjectKey, CachedObject>,
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

impl DiscoveryGateway {
    /// Commits exact package metadata under a distinct endpoint identity.
    /// Namespace feed cursors and their CAS sequences are never advanced.
    /// Positive replies read the admitted journal; negative outcomes remain
    /// explicit and cannot masquerade as an empty acquired-package catalog.
    pub(crate) fn observe_package(
        &mut self,
        package: &PackageCoordinate,
    ) -> Option<RegistryPackageDiscoveryObservation> {
        if self.cancelled.load(Ordering::Acquire) {
            return Some(unavailable(None, "registry metadata request was cancelled"));
        }
        let coordinate = match admit_registry_coordinate(package) {
            Ok(coordinate) => coordinate,
            Err(_) => {
                return Some(unavailable(
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
            return Some(unavailable(
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
                let root = if endpoint.as_str() == "https://index.golang.org" {
                    "https://proxy.golang.org"
                } else {
                    endpoint.as_str().trim_end_matches('/')
                };
                (
                    format!(
                        "{}/{}/@v/{}.info",
                        root,
                        go_proxy_escape(name),
                        go_proxy_escape(coordinate.version().as_str())
                    ),
                    1024 * 1024,
                )
            }
            _ => {
                return Some(unavailable(
                    None,
                    "exact metadata lookup is not supported by this source protocol",
                ));
            }
        };
        let point_endpoint = match RegistryEndpoint::new(coordinate.ecosystem(), url.clone()) {
            Ok(endpoint) => endpoint,
            Err(_) => {
                return Some(unavailable(
                    None,
                    "package metadata endpoint failed admission",
                ));
            }
        };
        let source = discovery_source_identity(&point_endpoint);
        if self.metadata_offline {
            return Some(unavailable(
                Some(source.id()),
                "registry metadata requests are disabled in offline mode",
            ));
        }
        let key = ObjectKey {
            endpoint: url.clone(),
            parser: PARSER_IDENTITY,
            package: package.as_str().to_owned(),
        };
        if let Some(cached) = self.package_metadata.objects.get(&key)
            && cached.observed.elapsed() < DISCOVERY_REFRESH_INTERVAL
        {
            return if cached
                .releases
                .iter()
                .any(|release| &release.coordinate == package)
            {
                None
            } else if cached.complete {
                Some(RegistryPackageDiscoveryObservation::Missing {
                    source: source.id(),
                    proof: missing_proof(&key, cached.proof),
                    observed_at_millis: cached.observed_at_millis,
                })
            } else {
                Some(unavailable(
                    Some(source.id()),
                    "the bounded package document does not establish whether this release exists",
                ))
            };
        }
        let etag = self
            .package_metadata
            .objects
            .get(&key)
            .and_then(|cached| cached.etag.as_deref());
        let response = fetch_object(&url, maximum, &self.cancelled, etag);
        let (releases, complete, proof, etag) = match response {
            Ok(ObjectResponse::Modified { bytes, etag }) => {
                let proof = *blake3::hash(&bytes).as_bytes();
                let parsed = parse_object(coordinate.ecosystem(), &bytes, name, package);
                match parsed {
                    Ok((releases, complete)) => (releases, complete, proof, etag),
                    Err(error) => {
                        self.store.mark_failed(source, true);
                        return Some(unavailable(
                            Some(source.id()),
                            &format!("metadata document failed admission: {error:?}"),
                        ));
                    }
                }
            }
            Ok(ObjectResponse::Unchanged) => {
                let Some(cached) = self.package_metadata.objects.get(&key) else {
                    return Some(unavailable(
                        Some(source.id()),
                        "HTTP 304 has no admitted prior metadata object",
                    ));
                };
                (
                    cached.releases.clone(),
                    cached.complete,
                    cached.proof,
                    cached.etag.clone(),
                )
            }
            Ok(ObjectResponse::Missing { proof }) => (Vec::new(), true, proof, None),
            Err(error) => {
                self.store.mark_failed(source, true);
                return Some(unavailable(
                    Some(source.id()),
                    &format!("metadata request failed: {error:?}"),
                ));
            }
        };
        let observed_at = DiscoveryObservedAt::from_unix_millis(discovery_now());
        if self.cancelled.load(Ordering::Acquire) {
            return Some(unavailable(
                Some(source.id()),
                "registry metadata request was cancelled before admission",
            ));
        }
        let progress = self.store.progress_token(source);
        let mut facts = releases
            .iter()
            .cloned()
            .map(|mut release| {
                // A point snapshot supplies no append-feed ordering evidence.
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
        if complete {
            // A complete new object may retract earlier claims. Keep the
            // retraction as evidence rather than silently preserving Published.
            for (_, old) in self
                .store
                .facts()
                .filter(|(old_source, _)| **old_source == source)
            {
                if !facts.iter().any(|fact| fact.coordinate == old.coordinate) {
                    facts.push(DiscoveryFact {
                        source,
                        coordinate: old.coordinate.clone(),
                        standing: DiscoveryStanding::Withdrawn,
                        observed_at,
                        source_event: DiscoverySourceEvent::Snapshot,
                        source_event_time: None,
                        proof,
                        metadata: DiscoveryMetadata::default(),
                    });
                }
            }
        }
        let encoded_bytes = facts
            .iter()
            .try_fold(0_usize, |total, fact| {
                serde_json::to_vec(fact)
                    .ok()
                    .and_then(|bytes| total.checked_add(bytes.len()))
            })
            .unwrap_or(usize::MAX);
        if encoded_bytes > MAX_CACHE_ENCODED_BYTES {
            return Some(unavailable(
                Some(source.id()),
                "admitted package metadata exceeds the bounded object cache",
            ));
        }
        let draft = DiscoveryBatchDraft {
            source,
            previous_cursor: progress.cursor.clone(),
            next_cursor: progress.cursor.clone(),
            source_high_watermark: progress.cursor,
            caught_up: false,
            observed_at,
            completeness: if complete {
                DiscoveryCompleteness::Windowed
            } else {
                DiscoveryCompleteness::Incomplete
            },
            facts,
            package_retractions: Vec::new(),
        };
        let committed = draft
            .admit_for_sequence(progress.sequence)
            .map_err(DiscoveryStoreError::from)
            .and_then(|batch| {
                if self.cancelled.load(Ordering::Acquire) {
                    return Err(DiscoveryStoreError::Cancelled);
                }
                self.store.commit(batch)
            });
        if let Err(error) = committed {
            self.store.mark_failed(source, true);
            return Some(unavailable(
                Some(source.id()),
                &format!("metadata observation was not committed: {error:?}"),
            ));
        }
        self.store.mark_failed(source, false);
        if self.cancelled.load(Ordering::Acquire) {
            return Some(unavailable(
                Some(source.id()),
                "registry metadata request was cancelled before cache publication",
            ));
        }
        let found = releases
            .iter()
            .any(|release| &release.coordinate == package);
        let negative_proof = missing_proof(&key, proof);
        self.package_metadata.insert(
            key,
            CachedObject {
                etag,
                releases,
                complete,
                proof,
                observed: Instant::now(),
                observed_at_millis: observed_at.as_unix_millis(),
                encoded_bytes,
            },
        );
        if found {
            None
        } else if complete {
            Some(RegistryPackageDiscoveryObservation::Missing {
                source: source.id(),
                proof: negative_proof,
                observed_at_millis: observed_at.as_unix_millis(),
            })
        } else {
            Some(unavailable(
                Some(source.id()),
                "the bounded package document does not establish whether this release exists",
            ))
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
                .min_by_key(|(_, value)| value.observed)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest
                && let Some(old) = self.objects.remove(&oldest)
            {
                self.encoded_bytes = self.encoded_bytes.saturating_sub(old.encoded_bytes);
            }
        }
        self.encoded_bytes = self.encoded_bytes.saturating_add(object.encoded_bytes);
        self.objects.insert(key, object);
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
            let parsed = parse_npm_packument_document(bytes, name, 0, MAX_DISCOVERY_PAGE_ITEMS)?;
            Ok((parsed.releases, !parsed.is_truncated))
        }
        RegistryEcosystem::Pypi => {
            let parsed = parse_pypi_project_metadata(bytes, name, MAX_DISCOVERY_PAGE_ITEMS)?;
            Ok((parsed.releases, !parsed.is_truncated))
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

fn fetch_object(
    url: &str,
    maximum: usize,
    cancelled: &AtomicBool,
    etag: Option<&str>,
) -> Result<ObjectResponse, DiscoveryStoreError> {
    if cancelled.load(Ordering::Acquire) {
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
        let path = std::env::temp_dir().join(format!(
            "backend-point-metadata-{}-{}.journal",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
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
        owner
            .package_metadata
            .objects
            .values_mut()
            .next()
            .expect("object")
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
}
