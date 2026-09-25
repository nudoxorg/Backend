use std::{
    env, fs,
    io::{self, Read, Seek, SeekFrom, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use super::ecosystem::{NativeArtifactKind, resolve_archive_url};
use super::identity::RegistryCredentialPolicy;
use super::*;

fn test_native_metadata() -> backend_library::RegistryNativeMetadata {
    backend_library::RegistryNativeMetadata::unavailable(RegistryEcosystem::Cargo, "test fixture")
}

fn test_facts() -> ReleaseFacts {
    let metadata = test_native_metadata();
    ReleaseFacts::default().with_native_metadata(metadata.identity().expect("test metadata"))
}

fn test_facts_for(standing: ReleaseStanding) -> ReleaseFacts {
    let metadata = test_native_metadata();
    ReleaseFacts::new(
        standing,
        DownloadCount::NotReported(DownloadCountGap::Unsupported),
        SecurityStanding::Unassessed,
    )
    .with_native_metadata(metadata.identity().expect("test metadata"))
}
use crate::{
    acquisition::{TransferResetReason, TransferValidator},
    capability::CapabilityArtifactId,
    fault::{Boundary, Faults},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha512};

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

fn unavailable_dependency_facts()
-> backend_library::DependencyFacts<Box<[backend_library::PackageDependencyRecord]>> {
    backend_library::DependencyFacts::Unavailable(
        backend_library::ProductText::new("test dependency metadata unavailable")
            .expect("bounded test dependency reason"),
    )
}

#[test]
fn acquisition_service_coalesces_concurrent_registry_effects() {
    struct CountingTransport {
        package: RemotePackage,
        archive: Vec<u8>,
        pages: Arc<AtomicU64>,
        archives: Arc<AtomicU64>,
    }
    impl RegistryTransport for CountingTransport {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            self.pages.fetch_add(1, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(20));
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [7; 32],
                packages: vec![self.package.clone()],
            }))
        }

        fn fetch_archive(
            &mut self,
            _package: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            self.archives.fetch_add(1, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(20));
            Ok(TransportResult::Available(ArchiveArtifact::from_bytes(
                self.archive.clone(),
            )))
        }
    }

    let endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Cargo,
        "http://127.0.0.1:9/acquisition-service",
    )
    .expect("admit endpoint");
    let archive = b"service archive".to_vec();
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/service@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([9; 32]),
        facts: test_facts(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from("http://127.0.0.1:9/acquisition-service/archive"),
    };
    let pages = Arc::new(AtomicU64::new(0));
    let archives = Arc::new(AtomicU64::new(0));
    let root = temporary("service-coalesce");
    let (owner, _) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("open owner");
    let service = Arc::new(
        crate::acquisition::AcquisitionService::from_owner(
            owner,
            root.join("service-coordination"),
        )
        .expect("open service"),
    );
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        package.coordinate.to_string(),
        1,
        0,
    )
    .expect("request");
    let barrier = Arc::new(Barrier::new(32));
    let (outcomes, receiver) = mpsc::channel();
    let mut threads = Vec::new();
    for _ in 0..32 {
        let service = Arc::clone(&service);
        let package = package.clone();
        let archive = archive.clone();
        let pages = Arc::clone(&pages);
        let archives = Arc::clone(&archives);
        let barrier = Arc::clone(&barrier);
        let request = request.clone();
        let outcomes = outcomes.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            let mut transport = CountingTransport {
                package,
                archive,
                pages,
                archives,
            };
            outcomes
                .send(service.acquire(&request, &mut transport))
                .expect("acquisition result receiver");
        }));
    }
    drop(outcomes);
    let mut receipt: Option<Arc<crate::acquisition::AcquisitionReceipt>> = None;
    for _ in 0..32 {
        let outcome = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("all acquisition callers complete within the bounded singleflight timeout");
        let crate::acquisition::AcquisitionOutcome::Hit(result) = outcome else {
            panic!("all callers share the published receipt: {outcome:?}");
        };
        if let Some(expected) = &receipt {
            assert_eq!(expected.id, result.receipt.id);
            assert_eq!(expected.delta, result.receipt.delta);
            assert_eq!(expected.target, result.receipt.target);
        } else {
            receipt = Some(result.receipt.clone());
        }
    }
    for thread in threads {
        thread.join().expect("acquisition caller");
    }
    assert_eq!(pages.load(Ordering::Relaxed), 1);
    assert_eq!(archives.load(Ordering::Relaxed), 1);
    assert_eq!(service.telemetry().leaders, 1);
    assert_eq!(service.telemetry().followers, 31);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn acquisition_service_reuses_a_cached_release_after_not_modified() {
    struct RevalidatingTransport {
        package: RemotePackage,
        archive: Vec<u8>,
        revalidate: bool,
        pages: usize,
        archives: usize,
    }

    impl RegistryTransport for RevalidatingTransport {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            self.pages += 1;
            if self.revalidate {
                return Ok(TransportResult::NotModified);
            }
            self.revalidate = true;
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [7; 32],
                packages: vec![self.package.clone()],
            }))
        }

        fn fetch_archive(
            &mut self,
            _package: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            self.archives += 1;
            Ok(TransportResult::Available(ArchiveArtifact::from_bytes(
                self.archive.clone(),
            )))
        }
    }

    let endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Cargo,
        "http://127.0.0.1:9/etag-revalidation",
    )
    .expect("admit endpoint");
    let archive = b"etag cached archive".to_vec();
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/etag@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(
            *CapabilityArtifactId::from_value(&archive).as_bytes(),
        ),
        provenance: ProvenanceDigest::from_authenticated_feed([13; 32]),
        facts: test_facts(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from("http://127.0.0.1:9/etag-revalidation/archive"),
    };
    let root = temporary("etag-revalidation");
    let (owner, _) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("open owner");
    let service =
        crate::acquisition::AcquisitionService::from_owner(owner, root.join("coordination"))
            .expect("open acquisition service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        package.coordinate.to_string(),
        1,
        0,
    )
    .expect("request");
    let refresh_request = request
        .clone()
        .with_fact_freshness(crate::acquisition::FactFreshness::always());
    let mut transport = RevalidatingTransport {
        package,
        archive,
        revalidate: false,
        pages: 0,
        archives: 0,
    };

    assert!(matches!(
        service.acquire(
            &request
                .with_fact_freshness(crate::acquisition::FactFreshness::max_age_millis(u64::MAX)),
            &mut transport,
        ),
        crate::acquisition::AcquisitionOutcome::Hit(_)
    ));
    let second = service.acquire(&refresh_request, &mut transport);
    assert!(matches!(
        second,
        crate::acquisition::AcquisitionOutcome::Hit(_)
    ));
    assert_eq!(transport.pages, 2, "refresh must exercise the 304 branch");
    assert_eq!(transport.archives, 1, "304 must reuse the durable archive");

    let before_lineage_epoch = service.policy_epoch();
    let published = service
        .published_packages()
        .into_iter()
        .next()
        .expect("cached publication");
    let association = backend_library::RegistryForgeAssociation::new(
        backend_library::PackageReference::Purl(published.coordinate.clone()),
        Box::new([]),
        backend_library::RegistryForgeAssociationState::Unavailable {
            reason: backend_library::ForgeUnavailableReason::Offline,
            blobs: Box::new([]),
        },
        backend_library::RegistryForgeProvenance::RegistryMetadata,
    )
    .expect("typed forge lineage");
    service
        .link_forge_associations(
            &published.coordinate,
            vec![association.clone()].into_boxed_slice(),
        )
        .expect("link forge lineage");
    assert_ne!(service.policy_epoch(), before_lineage_epoch);
    let linked = service
        .published_packages()
        .into_iter()
        .next()
        .expect("linked publication");
    let joined = service
        .forge_sources_for(&linked)
        .expect("joined forge lineage");
    assert_eq!(joined.as_ref(), &[association.clone()]);
    let linked_facts_root = service.facts_frontier();

    drop(service);
    let (recovered_owner, _) = RegistryOwner::open(
        &root,
        RegistryEndpoint::new(
            RegistryEcosystem::Cargo,
            "http://127.0.0.1:9/etag-revalidation",
        )
        .expect("same endpoint"),
        AcquisitionPolicy::Online,
        limits(),
    )
    .expect("recover linked lineage");
    let recovered = recovered_owner
        .published_packages()
        .next()
        .expect("recovered publication");
    assert_eq!(recovered_owner.facts_frontier(), linked_facts_root);
    assert_eq!(
        recovered_owner
            .forge_sources_for(recovered)
            .expect("recovered forge lineage")
            .as_ref(),
        &[association]
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn not_modified_without_cached_coordinate_stays_unavailable() {
    struct ValidatorOnlyTransport;

    impl RegistryTransport for ValidatorOnlyTransport {
        fn fetch_page(
            &mut self,
            _request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            Ok(TransportResult::NotModified)
        }

        fn fetch_archive(
            &mut self,
            _package: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            panic!("a 304 without a cached coordinate must not fetch an archive")
        }
    }

    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, "http://127.0.0.1:9/etag-missing")
            .expect("admit endpoint");
    let root = temporary("etag-missing");
    let (owner, _) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("open owner");
    let service =
        crate::acquisition::AcquisitionService::from_owner(owner, root.join("coordination"))
            .expect("open acquisition service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        "pkg:cargo/etag-missing@1.0.0",
        1,
        0,
    )
    .expect("request");
    assert!(matches!(
        service.acquire(&request, &mut ValidatorOnlyTransport),
        crate::acquisition::AcquisitionOutcome::Unavailable(_)
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn acquisition_service_keeps_negative_facts_distinct_from_unavailable_and_circuit_open() {
    struct EmptyTransport {
        pages: Arc<AtomicU64>,
    }
    impl RegistryTransport for EmptyTransport {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            self.pages.fetch_add(1, Ordering::Relaxed);
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: request.cursor.token(),
                packages: Vec::new(),
            }))
        }

        fn fetch_archive(
            &mut self,
            _package: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            Err(TransportFailure::Protocol)
        }
    }

    struct UnavailableTransport;
    impl RegistryTransport for UnavailableTransport {
        fn fetch_page(
            &mut self,
            _request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            Ok(TransportResult::Unavailable)
        }

        fn fetch_archive(
            &mut self,
            _package: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            Ok(TransportResult::Unavailable)
        }
    }

    let endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Cargo,
        "http://127.0.0.1:9/acquisition-outcomes",
    )
    .expect("admit endpoint");
    let coordinate = PackageCoordinate::parse("pkg:cargo/missing@1.0.0").expect("coordinate");
    let root = temporary("service-outcomes");
    let (owner, _) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("open owner");
    let service = crate::acquisition::AcquisitionService::from_owner(
        owner,
        root.join("service-coordination"),
    )
    .expect("open service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        coordinate.to_string(),
        1,
        0,
    )
    .expect("request");
    let pages = Arc::new(AtomicU64::new(0));
    let mut empty = EmptyTransport {
        pages: Arc::clone(&pages),
    };
    assert!(matches!(
        service.acquire(&request, &mut empty),
        crate::acquisition::AcquisitionOutcome::NegativeFact(crate::acquisition::NegativeFact {
            kind: crate::acquisition::NegativeFactKind::NotFound,
            ..
        })
    ));
    let mut cached = EmptyTransport { pages };
    assert!(matches!(
        service.acquire(&request, &mut cached),
        crate::acquisition::AcquisitionOutcome::NegativeFact(_)
    ));
    assert_eq!(cached.pages.load(Ordering::Relaxed), 1);

    let circuit_root = temporary("service-circuit");
    let endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Cargo,
        "http://127.0.0.1:9/acquisition-circuit",
    )
    .expect("admit circuit endpoint");
    let (owner, _) =
        RegistryOwner::open(&circuit_root, endpoint, AcquisitionPolicy::Online, limits())
            .expect("open circuit owner");
    let circuit = crate::acquisition::AcquisitionService::from_owner(
        owner,
        circuit_root.join("service-coordination"),
    )
    .expect("open circuit service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        circuit.source_id(),
        coordinate.to_string(),
        1,
        0,
    )
    .expect("circuit request");
    for _ in 0..3 {
        assert!(matches!(
            circuit.acquire(&request, &mut UnavailableTransport),
            crate::acquisition::AcquisitionOutcome::Unavailable(_)
        ));
    }
    assert!(matches!(
        circuit.acquire(&request, &mut UnavailableTransport),
        crate::acquisition::AcquisitionOutcome::CircuitOpen(_)
    ));
    fs::remove_dir_all(root).expect("cleanup negative");
    fs::remove_dir_all(circuit_root).expect("cleanup circuit");
}

fn temporary(label: &str) -> PathBuf {
    let value = TEMPORARY.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "backend-registry-{label}-{}-{value}",
        std::process::id()
    ))
}

const MAX_REQUEST_HEAD_BYTES: usize = 16 * 1024;

fn read_request_head(stream: &mut std::net::TcpStream, context: &str) -> io::Result<Vec<u8>> {
    let mut request = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    loop {
        let remaining = MAX_REQUEST_HEAD_BYTES.saturating_sub(request.len());
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{context}: request headers exceeded {MAX_REQUEST_HEAD_BYTES} bytes"),
            ));
        }
        let chunk_len = remaining.min(buffer.len());
        let read = stream.read(&mut buffer[..chunk_len]).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("{context}: reading request headers: {error}"),
            )
        })?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "{context}: EOF before CRLFCRLF after {} bytes",
                    request.len()
                ),
            ));
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(request);
        }
    }
}

fn feed(endpoint: &str, archive: &[u8], digest: [u8; 32]) -> Vec<u8> {
    let _ = archive;
    format!(
        concat!(
            "{{\"schema\":1,\"next\":\"{}\",\"items\":[{{",
            "\"name\":\"demo\",\"version\":\"1.2.3\",",
            "\"archive\":\"{}/archive\",\"blake3\":\"{}\",",
            "\"provenance\":\"{}\"}}]}}"
        ),
        transport::hex(&[7; 32]),
        endpoint,
        transport::hex(&digest),
        transport::hex(&[9; 32])
    )
    .into_bytes()
}

fn live_fixture(archive: &[u8], digest: [u8; 32]) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture registry");
    let address = listener.local_addr().expect("fixture address");
    let endpoint = format!("http://{address}");
    let responses = [feed(&endpoint, archive, digest), archive.to_vec()];
    let handle = thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().expect("accept registry request");
            let _ = read_request_head(&mut stream, "registry fixture request")
                .expect("read registry request headers");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write headers");
            stream.write_all(&body).expect("write body");
        }
    });
    (endpoint, handle)
}

fn one_response(
    status: u16,
    extra_headers: &str,
    body: Vec<u8>,
) -> (String, thread::JoinHandle<()>) {
    response_with_length(status, extra_headers, body, None)
}

fn response_with_length(
    status: u16,
    extra_headers: &str,
    body: Vec<u8>,
    declared_length: Option<usize>,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture registry");
    let address = listener.local_addr().expect("fixture address");
    let headers = extra_headers.to_owned();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept registry request");
        let _ = read_request_head(&mut stream, "single-response fixture request")
            .expect("read single-response request headers");
        let content_length = declared_length.unwrap_or(body.len());
        write!(
            stream,
            "HTTP/1.1 {status} TEST\r\nContent-Length: {content_length}\r\n{headers}Connection: close\r\n\r\n",
        )
        .expect("write headers");
        stream.write_all(&body).expect("write body");
    });
    (format!("http://{address}"), handle)
}

#[test]
fn registry_credentials_are_scoped_to_the_configured_authority() {
    let endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Cargo,
        "https://registry.example.test/index",
    )
    .expect("endpoint");
    let token = AuthenticationToken::new("Bearer test-secret").expect("token");
    let mut policy = RegistryCredentialPolicy::new(&endpoint, Some(token));
    assert_eq!(
        policy.authorization_for("https://registry.example.test/archive"),
        Some("Bearer test-secret")
    );
    assert_eq!(
        policy.authorization_for("HTTPS://REGISTRY.EXAMPLE.TEST/archive"),
        Some("Bearer test-secret")
    );
    assert_eq!(
        policy.authorization_for("https://archive.example.test/archive"),
        None
    );
    assert!(policy.permit_authority("https://archive.example.test"));
    assert_eq!(
        policy.authorization_for("https://archive.example.test/archive"),
        Some("Bearer test-secret")
    );

    let debug = format!("{policy:?}");
    assert!(!debug.contains("test-secret"));
    let endpoint_debug = format!("{endpoint:?}");
    assert!(!endpoint_debug.contains("test-secret"));
}

#[test]
fn authenticated_metadata_request_uses_configured_authorization() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth fixture");
    let address = listener.local_addr().expect("auth fixture address");
    let endpoint_text = format!("http://{address}");
    let request = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&request);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept auth request");
        let bytes = read_request_head(&mut stream, "auth fixture request")
            .expect("read auth request headers");
        captured
            .lock()
            .expect("capture auth request")
            .extend_from_slice(&bytes);
        let body = b"{\"schema\":1,\"next\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"items\":[]}";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("write auth response headers");
        stream.write_all(body).expect("write auth response body");
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("loopback endpoint");
    let cursor = FeedCursor::genesis(endpoint.id());
    let mut transport = HttpRegistryTransport::new(
        endpoint,
        Some(AuthenticationToken::new("Bearer test-secret").expect("token")),
        limits(),
    )
    .expect("transport");
    assert!(matches!(
        transport.fetch_page(FeedRequest {
            cursor,
            max_items: 1,
        }),
        Ok(TransportResult::Available(FeedPage { packages, .. })) if packages.is_empty()
    ));
    server.join().expect("auth fixture server");
    let request = String::from_utf8(request.lock().expect("read captured auth request").clone())
        .expect("auth request UTF-8");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-secret")
    );
}

#[test]
fn missing_or_wrong_registry_credentials_stay_typed_rejections() {
    for authentication in [
        None,
        Some(AuthenticationToken::new("Bearer wrong-secret").expect("token")),
    ] {
        let (endpoint_text, server) = one_response(401, "", Vec::new());
        let endpoint =
            RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("endpoint");
        let cursor = FeedCursor::genesis(endpoint.id());
        let mut transport =
            HttpRegistryTransport::new(endpoint, authentication, limits()).expect("transport");
        assert!(matches!(
            transport.fetch_page(FeedRequest {
                cursor,
                max_items: 1,
            }),
            Err(TransportFailure::Rejected(401))
        ));
        server.join().expect("credential rejection fixture");
    }
}

fn limits() -> AcquisitionLimits {
    AcquisitionLimits {
        max_items: 4,
        max_feed_bytes: 4096,
        max_archive_bytes: 4096,
        max_page_archive_bytes: 8192,
        max_catalog_items: 16,
        ..AcquisitionLimits::default()
    }
}

fn large_archive_limits() -> AcquisitionLimits {
    AcquisitionLimits {
        max_items: 4,
        max_feed_bytes: 4096,
        max_archive_bytes: 4 * 1024 * 1024,
        max_page_archive_bytes: 8 * 1024 * 1024,
        max_catalog_items: 16,
        ..AcquisitionLimits::default()
    }
}

#[test]
fn http_publication_and_restart_advance_one_atomic_cursor() {
    let archive = b"verified package archive";
    let digest = *CapabilityArtifactId::from_value(archive.as_slice()).as_bytes();
    let (endpoint_text, server) = live_fixture(archive, digest);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("admit endpoint");
    let root = temporary("restart");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("open owner");
    let mut transport =
        HttpRegistryTransport::new(endpoint.clone(), None, limits()).expect("transport");
    let AcquisitionOutcome::Published(receipt) = owner.poll(&mut transport).expect("poll") else {
        panic!("expected publication")
    };
    assert_eq!(receipt.base.sequence(), 0);
    assert_eq!(receipt.target.sequence(), 1);
    assert_eq!(receipt.packages.len(), 1);
    let object = owner
        .read_artifact(&receipt.packages[0].coordinate)
        .expect("read artifact")
        .expect("published artifact");
    assert_eq!(object.bytes(), archive);
    server.join().expect("server");
    drop(owner);
    let (_, recovery) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("recover owner");
    assert_eq!(recovery.cursor.sequence(), 1);
    assert!(recovery.pending.is_none());
    assert_eq!(recovery.last_receipt, Some(receipt));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn large_archive_streams_once_through_a_bounded_handoff() {
    let archive = vec![0x5a; 2 * 1024 * 1024];
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let (endpoint_text, server) = live_fixture(&archive, digest);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("admit endpoint");
    let root = temporary("large-stream");
    let (mut owner, _) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        large_archive_limits(),
    )
    .expect("open owner");
    let mut transport =
        HttpRegistryTransport::new(endpoint, None, large_archive_limits()).expect("transport");
    let AcquisitionOutcome::Published(receipt) = owner.poll(&mut transport).expect("poll") else {
        panic!("expected large archive publication")
    };
    let object = owner
        .read_artifact(&receipt.packages[0].coordinate)
        .expect("read artifact")
        .expect("published artifact");
    assert_eq!(object.bytes().len(), archive.len());
    // `live_fixture` has exactly one archive response. A second download or
    // retained handoff would leave the server blocked or fail its join.
    server.join().expect("single archive response");
    drop(owner);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn archive_stream_hashing_never_requests_a_buffer_over_64kib() {
    struct TrackingReader {
        bytes: Vec<u8>,
        offset: usize,
        largest_request: usize,
    }
    impl Read for TrackingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.largest_request = self.largest_request.max(buffer.len());
            let remaining = self.bytes.len().saturating_sub(self.offset);
            let count = remaining.min(buffer.len());
            if count == 0 {
                return Ok(0);
            }
            buffer[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        }
    }
    impl Seek for TrackingReader {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            let target = match position {
                SeekFrom::Start(offset) => offset,
                SeekFrom::Current(offset) => (self.offset as i64)
                    .checked_add(offset)
                    .ok_or_else(|| std::io::Error::other("seek overflow"))?
                    as u64,
                SeekFrom::End(offset) => (self.bytes.len() as i64)
                    .checked_add(offset)
                    .ok_or_else(|| std::io::Error::other("seek overflow"))?
                    as u64,
            };
            self.offset =
                usize::try_from(target).map_err(|_| std::io::Error::other("seek overflow"))?;
            Ok(target)
        }
    }

    let bytes = vec![0xa5; 2 * 1024 * 1024];
    let digest = *CapabilityArtifactId::from_value(&bytes).as_bytes();
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/large@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([7; 32]),
        facts: test_facts(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: std::sync::Arc::from("https://registry.example.test/large.crate"),
    };
    let mut reader = TrackingReader {
        bytes,
        offset: 0,
        largest_request: 0,
    };
    let mut sink = std::io::sink();
    let artifact = package
        .stream_to(&mut reader, &mut sink, 2 * 1024 * 1024)
        .expect("stream and verify archive");
    assert_eq!(artifact.as_bytes(), &digest);
    assert_eq!(reader.offset, 2 * 1024 * 1024);
    assert!(reader.largest_request <= 64 * 1024);
}

#[test]
fn integrity_failure_never_advances_the_cursor() {
    let archive = b"wrong bytes";
    let (endpoint_text, server) = live_fixture(archive, [3; 32]);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("admit endpoint");
    let root = temporary("integrity");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let mut transport = HttpRegistryTransport::new(endpoint, None, limits()).expect("transport");
    let result = owner.poll(&mut transport);
    assert!(matches!(
        result,
        Err(AcquisitionError::Transport(TransportFailure::Integrity))
    ));
    assert_eq!(owner.cursor().sequence(), 0);
    server.join().expect("server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn crash_after_object_write_recovers_the_same_pending_intent() {
    let archive = b"retry-safe archive";
    let digest = *CapabilityArtifactId::from_value(archive.as_slice()).as_bytes();
    let (endpoint_text, server) = live_fixture(archive, digest);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("admit endpoint");
    let root = temporary("fault");
    let faults = std::sync::Arc::new(Faults::default());
    faults.arm(Boundary::EffectConfirmed);
    let (owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let mut owner = owner.with_faults(faults);
    let mut transport =
        HttpRegistryTransport::new(endpoint.clone(), None, limits()).expect("transport");
    assert!(matches!(
        owner.poll(&mut transport),
        Err(AcquisitionError::Injected(_))
    ));
    server.join().expect("server");
    drop(owner);
    let (_, recovery) =
        RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits()).expect("recover");
    assert_eq!(recovery.cursor.sequence(), 0);
    assert!(recovery.pending.is_some());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn registry_archive_stage_restarts_from_its_durable_transfer_checkpoint() {
    let archive = b"checkpointed registry archive".to_vec();
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/checkpoint@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([4; 32]),
        facts: ReleaseFacts::default(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from("https://registry.example.test/checkpoint.crate"),
    };
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("archive-stage-resume");
    let faults = Arc::new(Faults::default());
    faults.arm(Boundary::ObjectWrite);
    let (owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let owner = owner.with_faults(Arc::clone(&faults));
    let stage = owner.archive_stage();
    let error = stage
        .verify_and_store(
            &package,
            ArchiveArtifact::from_bytes(archive.clone()),
            archive.len(),
            backend_advisory::AdvisoryPackageDto::unknown(),
        )
        .expect_err("fault before publication");
    assert!(matches!(error, AcquisitionError::Injected(_)));
    let transfer_root = storage_root(&root, &endpoint)
        .join("registry-content")
        .join("transfers");
    assert!(
        fs::read_dir(&transfer_root)
            .expect("transfer directory")
            .any(|entry| entry.ok().is_some_and(|entry| entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                == Some("part")))
    );
    drop(owner);

    let (reopened, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("reopen owner");
    let publication = reopened
        .archive_stage()
        .verify_and_store(
            &package,
            ArchiveArtifact::from_bytes(archive.clone()),
            archive.len(),
            backend_advisory::AdvisoryPackageDto::unknown(),
        )
        .expect("resume and publish");
    assert_eq!(
        publication.raw_object,
        crate::acquisition::RawArchiveObjectId::from_bytes(&archive)
    );
    assert_eq!(publication.bytes, archive.len() as u64);
    assert!(
        fs::read_dir(&transfer_root)
            .expect("transfer directory")
            .all(|entry| entry.ok().is_none_or(|entry| entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                != Some("part")))
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn http_archive_resume_uses_authenticated_range_and_reopens_at_multiple_offsets() {
    let archive = (0..(192 * 1024))
        .map(|value| (value % 251) as u8)
        .collect::<Vec<_>>();
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind range fixture");
    let address = listener.local_addr().expect("range fixture address");
    let endpoint_text = format!("http://{address}");
    let server_archive = archive.clone();
    let server = thread::spawn(move || {
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept range request");
            let request = read_request_head(&mut stream, "range fixture request")
                .expect("read range request headers");
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            if attempt == 0 {
                assert!(!request.contains("range:"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
                    server_archive.len()
                )
                .expect("write initial headers");
                stream
                    .write_all(&server_archive)
                    .expect("write initial body");
                continue;
            }
            let range = request
                .lines()
                .find_map(|line| line.strip_prefix("range: bytes="))
                .and_then(|value| value.strip_suffix('-'))
                .and_then(|value| value.parse::<usize>().ok())
                .expect("range start");
            assert!(request.contains("if-range: \"v1\""));
            let end = server_archive.len() - 1;
            let suffix = &server_archive[range..];
            write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {range}-{end}/{}\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
                server_archive.len(),
                suffix.len()
            )
            .expect("write range headers");
            stream.write_all(suffix).expect("write range body");
        }
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text.clone()).expect("endpoint");
    let mut range_limits = limits();
    range_limits.max_archive_bytes = archive.len() + 1024;
    range_limits.max_page_archive_bytes = archive.len() + 1024;
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/range@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([6; 32]),
        facts: ReleaseFacts::default(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from(format!("{endpoint_text}/archive")),
    };
    let root = temporary("http-range-resume");
    let (owner, _) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        range_limits,
    )
    .expect("owner");
    let stage = owner.archive_stage();
    let mut transport =
        HttpRegistryTransport::new(endpoint.clone(), None, range_limits).expect("transport");
    let mut transfer = stage.open_transfer(&package).expect("transfer");
    let first = match transport
        .fetch_archive_resumable(&package, transfer.checkpoint())
        .expect("initial request")
    {
        TransportResult::Available(artifact) => artifact,
        other => panic!("unexpected initial response: {other:?}"),
    };
    let total = first.length();
    let validator = first.validator().cloned();
    transfer
        .set_validator(validator)
        .expect("persist validator");
    transfer
        .bind_expected_length(total)
        .expect("persist length");
    let prefix = 65_537_u64;
    let mut reader = first
        .into_reader(range_limits.max_archive_bytes)
        .expect("initial body");
    transfer
        .append((&mut reader).take(prefix), total)
        .expect("persist interrupted prefix");
    drop(transfer);

    let transfer = stage.open_transfer(&package).expect("reopen transfer");
    assert_eq!(transfer.resume_offset(), prefix);
    assert_eq!(
        transfer.validator().and_then(TransferValidator::etag),
        Some("\"v1\"")
    );
    let checkpoint = transfer.checkpoint();
    let suffix = match transport
        .fetch_archive_resumable(&package, checkpoint)
        .expect("range request")
    {
        TransportResult::Available(artifact) => artifact,
        other => panic!("unexpected range response: {other:?}"),
    };
    assert_eq!(suffix.start_offset(), prefix);
    assert_eq!(suffix.length(), archive.len() as u64);
    assert_eq!(suffix.telemetry().resumed_bytes, prefix);
    assert_eq!(
        suffix.telemetry().downloaded_bytes,
        archive.len() as u64 - prefix
    );
    let publication = stage
        .verify_and_store_with_transfer(
            &package,
            suffix,
            archive.len(),
            backend_advisory::AdvisoryPackageDto::unknown(),
            transfer,
        )
        .expect("publish resumed archive");
    assert_eq!(
        publication.raw_object,
        crate::acquisition::RawArchiveObjectId::from_bytes(&archive)
    );
    server.join().expect("range server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn http_archive_resume_reconciles_a_completed_prefix_from_416() {
    let archive = b"already-complete-on-disk".repeat(4_096);
    let archive_len = archive.len();
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 416 fixture");
    let address = listener.local_addr().expect("416 fixture address");
    let endpoint_text = format!("http://{address}");
    let total = archive.len();
    let server_archive = archive.clone();
    let server = thread::spawn(move || {
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept 416 request");
            let request = read_request_head(&mut stream, "416 fixture request")
                .expect("read 416 request headers");
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            if attempt == 0 {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"complete-v1\"\r\nConnection: close\r\n\r\n"
                )
                .expect("write 416 initial headers");
                stream
                    .write_all(&server_archive)
                    .expect("write 416 initial body");
            } else {
                assert!(request.contains(&format!("range: bytes={total}-")));
                write!(
                    stream,
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nETag: \"complete-v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .expect("write 416 response");
            }
        }
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text.clone()).expect("endpoint");
    let mut range_limits = limits();
    range_limits.max_archive_bytes = archive_len + 1024;
    range_limits.max_page_archive_bytes = archive_len + 1024;
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/range-416@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([8; 32]),
        facts: ReleaseFacts::default(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from(format!("{endpoint_text}/archive")),
    };
    let root = temporary("http-range-416");
    let (owner, _) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        range_limits,
    )
    .expect("owner");
    let stage = owner.archive_stage();
    let mut transport =
        HttpRegistryTransport::new(endpoint.clone(), None, range_limits).expect("transport");
    let mut transfer = stage.open_transfer(&package).expect("transfer");
    let first = match transport
        .fetch_archive_resumable(&package, transfer.checkpoint())
        .expect("initial request")
    {
        TransportResult::Available(artifact) => artifact,
        other => panic!("unexpected initial response: {other:?}"),
    };
    let validator = first.validator().cloned();
    let mut reader = first
        .into_reader(range_limits.max_archive_bytes)
        .expect("initial body");
    transfer
        .set_validator(validator)
        .expect("persist validator");
    transfer
        .bind_expected_length(total as u64)
        .expect("persist extent");
    transfer
        .append(&mut reader, total as u64)
        .expect("persist complete prefix");
    let checkpoint = transfer.checkpoint();
    let complete = match transport
        .fetch_archive_resumable(&package, checkpoint)
        .expect("416 request")
    {
        TransportResult::Available(artifact) => artifact,
        other => panic!("unexpected 416 response: {other:?}"),
    };
    assert_eq!(complete.start_offset(), total as u64);
    assert_eq!(complete.length(), total as u64);
    assert_eq!(complete.body_length(), 0);
    let publication = stage
        .verify_and_store_with_transfer(
            &package,
            complete,
            total,
            backend_advisory::AdvisoryPackageDto::unknown(),
            transfer,
        )
        .expect("reconcile complete prefix");
    assert_eq!(
        publication.raw_object,
        crate::acquisition::RawArchiveObjectId::from_bytes(&archive)
    );
    server.join().expect("416 server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn http_archive_resume_restarts_when_origin_ignores_range() {
    let archive = b"origin-ignored-range".repeat(8_192);
    let archive_len = archive.len();
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fallback fixture");
    let address = listener.local_addr().expect("fallback fixture address");
    let endpoint_text = format!("http://{address}");
    let server_archive = archive.clone();
    let server = thread::spawn(move || {
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept fallback request");
            let request = read_request_head(&mut stream, "fallback fixture request")
                .expect("read fallback request headers");
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            if attempt == 0 {
                assert!(!request.contains("range:"));
            } else {
                assert!(request.contains("range: bytes="));
                assert!(request.contains("if-range: \"fallback-v1\""));
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {archive_len}\r\nETag: \"fallback-v1\"\r\nConnection: close\r\n\r\n"
            )
            .expect("write fallback headers");
            stream
                .write_all(&server_archive)
                .expect("write fallback body");
        }
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text.clone()).expect("endpoint");
    let mut range_limits = limits();
    range_limits.max_archive_bytes = archive_len + 1024;
    range_limits.max_page_archive_bytes = archive_len + 1024;
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/range-fallback@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([10; 32]),
        facts: ReleaseFacts::default(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from(format!("{endpoint_text}/archive")),
    };
    let root = temporary("http-range-fallback");
    let (owner, _) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        range_limits,
    )
    .expect("owner");
    let stage = owner.archive_stage();
    let mut transport =
        HttpRegistryTransport::new(endpoint.clone(), None, range_limits).expect("transport");
    let mut transfer = stage.open_transfer(&package).expect("transfer");
    let first = match transport
        .fetch_archive_resumable(&package, transfer.checkpoint())
        .expect("initial request")
    {
        TransportResult::Available(artifact) => artifact,
        other => panic!("unexpected initial response: {other:?}"),
    };
    let mut reader = first
        .into_reader(range_limits.max_archive_bytes)
        .expect("initial body");
    let prefix = 12_345_u64;
    transfer
        .set_validator(TransferValidator::new(Some("\"fallback-v1\""), None))
        .expect("persist validator");
    transfer
        .bind_expected_length(archive_len as u64)
        .expect("persist length");
    transfer
        .append((&mut reader).take(prefix), archive_len as u64)
        .expect("persist interrupted prefix");
    drop(transfer);

    let transfer = stage.open_transfer(&package).expect("reopen transfer");
    let fallback = match transport
        .fetch_archive_resumable(&package, transfer.checkpoint())
        .expect("fallback request")
    {
        TransportResult::Available(artifact) => artifact,
        other => panic!("unexpected fallback response: {other:?}"),
    };
    assert_eq!(fallback.start_offset(), 0);
    assert_eq!(fallback.length(), archive_len as u64);
    assert_eq!(
        fallback.reset_reason(),
        Some(TransferResetReason::RangeIgnored)
    );
    assert_eq!(fallback.telemetry().resumed_bytes, 0);
    assert_eq!(fallback.telemetry().downloaded_bytes, archive_len as u64);
    let publication = stage
        .verify_and_store_with_transfer(
            &package,
            fallback,
            archive_len,
            backend_advisory::AdvisoryPackageDto::unknown(),
            transfer,
        )
        .expect("publish fallback archive");
    assert_eq!(
        publication.raw_object,
        crate::acquisition::RawArchiveObjectId::from_bytes(&archive)
    );
    let quarantine = storage_root(&root, &endpoint).join("registry-content/quarantine");
    assert!(
        fs::read_dir(quarantine)
            .expect("quarantine directory")
            .all(|entry| entry.ok().is_none_or(|entry| !entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "prefix")))
    );
    server.join().expect("fallback server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn http_archive_resume_quarantines_prefix_when_validator_changes() {
    let old_archive = b"old representation".repeat(4_096);
    let new_archive = b"new representation".repeat(4_096);
    assert_eq!(old_archive.len(), new_archive.len());
    let archive_len = new_archive.len();
    let digest = *CapabilityArtifactId::from_value(&new_archive).as_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind validator fixture");
    let address = listener.local_addr().expect("validator fixture address");
    let endpoint_text = format!("http://{address}");
    let server_old = old_archive.clone();
    let server_new = new_archive.clone();
    let server = thread::spawn(move || {
        for attempt in 0..3 {
            let (mut stream, _) = listener.accept().expect("accept validator request");
            let request = read_request_head(&mut stream, "validator fixture request")
                .expect("read validator request headers");
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            match attempt {
                0 => {
                    assert!(!request.contains("range:"));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {archive_len}\r\nETag: \"validator-v1\"\r\nConnection: close\r\n\r\n"
                    )
                    .expect("write validator initial headers");
                    stream
                        .write_all(&server_old)
                        .expect("write validator initial body");
                }
                1 => {
                    let range = request
                        .lines()
                        .find_map(|line| line.strip_prefix("range: bytes="))
                        .and_then(|value| value.strip_suffix('-'))
                        .and_then(|value| value.parse::<usize>().ok())
                        .expect("validator range start");
                    assert!(request.contains("if-range: \"validator-v1\""));
                    let end = server_old.len() - 1;
                    let suffix = &server_old[range..];
                    write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {range}-{end}/{archive_len}\r\nContent-Length: {}\r\nETag: \"validator-v2\"\r\nConnection: close\r\n\r\n",
                        suffix.len()
                    )
                    .expect("write validator changed range headers");
                    stream
                        .write_all(suffix)
                        .expect("write validator changed range body");
                }
                _ => {
                    assert!(!request.contains("range:"));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {archive_len}\r\nETag: \"validator-v2\"\r\nConnection: close\r\n\r\n"
                    )
                    .expect("write validator reset headers");
                    stream
                        .write_all(&server_new)
                        .expect("write validator reset body");
                }
            }
        }
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text.clone()).expect("endpoint");
    let mut range_limits = limits();
    range_limits.max_archive_bytes = archive_len + 1024;
    range_limits.max_page_archive_bytes = archive_len + 1024;
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/range-validator@1.0.0")
            .expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([11; 32]),
        facts: ReleaseFacts::default(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from(format!("{endpoint_text}/archive")),
    };
    let root = temporary("http-range-validator");
    let (owner, _) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        range_limits,
    )
    .expect("owner");
    let stage = owner.archive_stage();
    let mut transport =
        HttpRegistryTransport::new(endpoint.clone(), None, range_limits).expect("transport");
    let mut transfer = stage.open_transfer(&package).expect("transfer");
    let first = match transport
        .fetch_archive_resumable(&package, transfer.checkpoint())
        .expect("initial request")
    {
        TransportResult::Available(artifact) => artifact,
        other => panic!("unexpected initial response: {other:?}"),
    };
    let mut reader = first
        .into_reader(range_limits.max_archive_bytes)
        .expect("initial body");
    let prefix = 12_345_u64;
    transfer
        .set_validator(TransferValidator::new(Some("\"validator-v1\""), None))
        .expect("persist validator");
    transfer
        .bind_expected_length(archive_len as u64)
        .expect("persist length");
    transfer
        .append((&mut reader).take(prefix), archive_len as u64)
        .expect("persist interrupted prefix");
    drop(transfer);

    let transfer = stage.open_transfer(&package).expect("reopen transfer");
    let reset = match transport
        .fetch_archive_resumable(&package, transfer.checkpoint())
        .expect("validator-change request")
    {
        TransportResult::Available(artifact) => artifact,
        other => panic!("unexpected validator response: {other:?}"),
    };
    assert_eq!(reset.start_offset(), 0);
    assert_eq!(
        reset.reset_reason(),
        Some(TransferResetReason::ValidatorChanged)
    );
    assert_eq!(
        reset.validator().and_then(TransferValidator::etag),
        Some("\"validator-v2\"")
    );
    let publication = stage
        .verify_and_store_with_transfer(
            &package,
            reset,
            archive_len,
            backend_advisory::AdvisoryPackageDto::unknown(),
            transfer,
        )
        .expect("publish changed representation");
    assert_eq!(
        publication.raw_object,
        crate::acquisition::RawArchiveObjectId::from_bytes(&new_archive)
    );
    let quarantine = storage_root(&root, &endpoint).join("registry-content/quarantine");
    assert!(
        fs::read_dir(quarantine)
            .expect("quarantine directory")
            .any(|entry| entry.ok().is_some_and(|entry| entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "prefix")))
    );
    server.join().expect("validator server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn http_archive_resume_rejects_sparse_or_malformed_206() {
    let archive = vec![0x41; 64];
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind malformed range fixture");
    let address = listener
        .local_addr()
        .expect("malformed range fixture address");
    let endpoint_text = format!("http://{address}");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept malformed range request");
        let request = read_request_head(&mut stream, "malformed range fixture request")
            .expect("read malformed range request headers");
        let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(request.contains("range: bytes=5-"));
        assert!(request.contains("if-range: \"malformed-v1\""));
        write!(
            stream,
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-63/64\r\nContent-Length: 61\r\nETag: \"malformed-v1\"\r\nConnection: close\r\n\r\n"
        )
        .expect("write malformed range headers");
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text.clone()).expect("endpoint");
    let mut range_limits = limits();
    range_limits.max_archive_bytes = archive.len() + 32;
    range_limits.max_page_archive_bytes = archive.len() + 32;
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/range-malformed@1.0.0")
            .expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([12; 32]),
        facts: ReleaseFacts::default(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from(format!("{endpoint_text}/archive")),
    };
    let root = temporary("http-range-malformed");
    let (owner, _) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        range_limits,
    )
    .expect("owner");
    let stage = owner.archive_stage();
    let mut transfer = stage.open_transfer(&package).expect("transfer");
    transfer
        .set_validator(TransferValidator::new(Some("\"malformed-v1\""), None))
        .expect("validator");
    transfer
        .bind_expected_length(archive.len() as u64)
        .expect("extent");
    transfer
        .append(&archive[..5], archive.len() as u64)
        .expect("prefix");
    let mut transport =
        HttpRegistryTransport::new(endpoint, None, range_limits).expect("transport");
    assert!(matches!(
        transport.fetch_archive_resumable(&package, transfer.checkpoint()),
        Err(TransportFailure::Protocol)
    ));
    drop(transfer);
    server.join().expect("malformed range server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn offline_policy_never_calls_transport_and_secrets_are_redacted() {
    struct PanicTransport;
    impl RegistryTransport for PanicTransport {
        fn fetch_page(
            &mut self,
            _: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            panic!("network called")
        }
        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            panic!("network called")
        }
    }
    let token = AuthenticationToken::new("Bearer very-secret").expect("token");
    assert_eq!(format!("{token:?}"), "AuthenticationToken([REDACTED])");
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("offline");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Offline, limits()).expect("owner");
    assert!(matches!(
        owner.poll(&mut PanicTransport),
        Ok(AcquisitionOutcome::Offline { .. })
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn unavailable_feed_retains_one_durable_retry_intent() {
    struct UnavailableTransport;
    impl RegistryTransport for UnavailableTransport {
        fn fetch_page(
            &mut self,
            _: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            Ok(TransportResult::Unavailable)
        }
        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            panic!("archive called without a page")
        }
    }
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("unavailable");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    assert!(matches!(
        owner.poll(&mut UnavailableTransport),
        Ok(AcquisitionOutcome::Unavailable { .. })
    ));
    drop(owner);
    let (_, recovery) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("recovery");
    assert_eq!(recovery.cursor.sequence(), 0);
    assert!(recovery.pending.is_some());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn metadata_only_delta_advances_and_recovers_its_checkpoint() {
    struct MetadataDelta {
        next: [u8; 32],
    }
    impl RegistryTransport for MetadataDelta {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: self.next,
                packages: Vec::new(),
            }))
        }
        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            panic!("metadata-only delta has no archive")
        }
    }
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("metadata-only-delta");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let next = [19; 32];
    let outcome = owner
        .poll(&mut MetadataDelta { next })
        .expect("metadata checkpoint");
    let AcquisitionOutcome::Published(receipt) = outcome else {
        panic!("metadata delta must commit")
    };
    assert!(receipt.packages.is_empty());
    assert_eq!(receipt.target.token(), next);
    drop(owner);
    let (_, recovery) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("recovery");
    assert_eq!(recovery.cursor.sequence(), 1);
    assert_eq!(recovery.cursor.token(), next);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn advisory_gate_denies_unknown_version_before_archive_staging() {
    struct UnknownAdvisory {
        archive_fetches: usize,
    }

    impl RegistryTransport for UnknownAdvisory {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            let archive = b"must never be staged";
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [23; 32],
                packages: vec![RemotePackage {
                    coordinate: PackageCoordinate::parse("pkg:cargo/demo@1.0.0")
                        .expect("coordinate"),
                    integrity: transport::ArchiveIntegrity::Canonical(
                        *CapabilityArtifactId::from_value(archive).as_bytes(),
                    ),
                    provenance: ProvenanceDigest::from_authenticated_feed([23; 32]),
                    facts: test_facts(),
                    native_metadata: test_native_metadata(),
                    advisory: Some(backend_advisory::AdvisoryObservation {
                        advisories: Box::new([]),
                        coverage: backend_advisory::AdvisoryCoverage::Unknown,
                        freshness: backend_advisory::FreshnessState::Unknown,
                        offline: true,
                        yanked: false,
                        unlisted: false,
                        malware: backend_advisory::MalwareCoverage::NotCovered,
                    }),
                    dependency_facts: unavailable_dependency_facts(),
                    archive_url: std::sync::Arc::from("https://registry.example.test/demo.crate"),
                }],
            }))
        }

        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            self.archive_fetches += 1;
            Ok(TransportResult::Available(ArchiveArtifact::from_bytes(
                b"must never be staged".to_vec(),
            )))
        }
    }

    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("advisory-gate");
    let (owner, _) =
        RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits()).expect("owner");
    let mut owner = owner.with_advisory_gate(backend_advisory::AcquisitionGate {
        offline: backend_advisory::OfflinePolicy::FailClosed,
    });
    let mut transport = UnknownAdvisory { archive_fetches: 0 };
    assert!(matches!(
        owner.poll(&mut transport),
        Err(AcquisitionError::AdvisoryDenied(
            backend_advisory::AcquisitionDecision::Deny(_),
        ))
    ));
    assert_eq!(transport.archive_fetches, 0);
    assert_eq!(owner.cursor().sequence(), 0);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn policy_delta_reuses_the_exact_archive_without_a_second_download() {
    struct PolicyDelta {
        archive: Vec<u8>,
        page: u8,
        archive_fetches: usize,
    }
    impl RegistryTransport for PolicyDelta {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            self.page = self.page.saturating_add(1);
            let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
            let standing = if self.page == 1 {
                ReleaseStanding::Available
            } else {
                ReleaseStanding::Yanked
            };
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [self.page; 32],
                packages: vec![RemotePackage {
                    coordinate,
                    integrity: transport::ArchiveIntegrity::Canonical(
                        *CapabilityArtifactId::from_value(&self.archive).as_bytes(),
                    ),
                    provenance: ProvenanceDigest::from_authenticated_feed([self.page; 32]),
                    facts: test_facts_for(standing),
                    native_metadata: test_native_metadata(),
                    advisory: None,
                    dependency_facts: unavailable_dependency_facts(),
                    archive_url: std::sync::Arc::from("https://registry.example.test/demo.crate"),
                }],
            }))
        }
        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            self.archive_fetches += 1;
            Ok(TransportResult::Available(ArchiveArtifact::from_bytes(
                self.archive.clone(),
            )))
        }
    }
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("policy-delta-reuse");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
    let mut transport = PolicyDelta {
        archive: b"one immutable archive".to_vec(),
        page: 0,
        archive_fetches: 0,
    };
    let _ = owner.poll(&mut transport).expect("initial release");
    let first_epoch = owner.policy_epoch();
    assert_ne!(first_epoch, 0);
    let first = owner.published(&coordinate).expect("published").artifact;
    let _ = owner.poll(&mut transport).expect("yank delta");
    let second_epoch = owner.policy_epoch();
    assert_ne!(second_epoch, first_epoch);
    let current = owner.published(&coordinate).expect("updated");
    assert_eq!(transport.archive_fetches, 1);
    assert_eq!(current.artifact, first);
    assert_eq!(current.facts.standing(), ReleaseStanding::Yanked);
    drop(owner);
    let (owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("restart owner");
    assert_eq!(owner.policy_epoch(), second_epoch);
    let fail_closed = owner.with_advisory_gate(backend_advisory::AcquisitionGate {
        offline: backend_advisory::OfflinePolicy::FailClosed,
    });
    assert_ne!(fail_closed.policy_epoch(), second_epoch);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn service_revalidates_cached_facts_and_reuses_archive_bytes() {
    struct MutableFacts {
        archive: Vec<u8>,
        page: u8,
        pages: usize,
        archives: usize,
    }

    impl RegistryTransport for MutableFacts {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            self.pages += 1;
            self.page = self.page.saturating_add(1);
            let coordinate = PackageCoordinate::parse("pkg:cargo/demo@2.0.0").expect("coordinate");
            let standing = if self.page == 1 {
                ReleaseStanding::Available
            } else {
                ReleaseStanding::Yanked
            };
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [self.page; 32],
                packages: vec![RemotePackage {
                    coordinate,
                    integrity: transport::ArchiveIntegrity::Canonical(
                        *CapabilityArtifactId::from_value(&self.archive).as_bytes(),
                    ),
                    provenance: ProvenanceDigest::from_authenticated_feed([self.page; 32]),
                    facts: test_facts_for(standing),
                    native_metadata: test_native_metadata(),
                    advisory: None,
                    dependency_facts: unavailable_dependency_facts(),
                    archive_url: Arc::from("https://registry.example.test/demo.crate"),
                }],
            }))
        }

        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            self.archives += 1;
            Ok(TransportResult::Available(ArchiveArtifact::from_bytes(
                self.archive.clone(),
            )))
        }
    }

    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("service-fact-refresh");
    let (owner, _) =
        RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits()).expect("owner");
    let service =
        crate::acquisition::AcquisitionService::from_owner(owner, root.join("coordination"))
            .expect("service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        "pkg:cargo/demo@2.0.0",
        1,
        0,
    )
    .expect("request");
    let mut transport = MutableFacts {
        archive: b"one archive, two fact frontiers".to_vec(),
        page: 0,
        pages: 0,
        archives: 0,
    };
    let first = service.acquire(&request, &mut transport);
    let crate::acquisition::AcquisitionOutcome::Hit(first) = first else {
        panic!("initial acquisition must publish")
    };
    let epoch = first.receipt.policy_epoch;
    assert_ne!(epoch, 0);
    let wrong_hash = crate::acquisition::AcquisitionRequest::new(
        service.source_id(),
        "pkg:cargo/demo@2.0.0",
        crate::acquisition::RawArchiveObjectId::from_bytes(b"wrong archive"),
        1,
        0,
    )
    .expect("wrong-hash request");
    assert!(matches!(
        service.ensure(&wrong_hash),
        crate::acquisition::AcquisitionOutcome::Corrupt(
            crate::acquisition::CorruptReason::Integrity
        )
    ));
    let second = service.acquire(&request, &mut transport);
    assert!(matches!(
        second,
        crate::acquisition::AcquisitionOutcome::NegativeFact(crate::acquisition::NegativeFact {
            kind: crate::acquisition::NegativeFactKind::Yanked,
            ..
        })
    ));
    assert_ne!(service.policy_epoch(), epoch);
    assert_eq!(transport.pages, 2);
    assert_eq!(transport.archives, 1);
    drop(service);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn acquisition_service_preserves_offline_and_rejects_wrong_owner_requests() {
    struct NeverTransport;
    impl RegistryTransport for NeverTransport {
        fn fetch_page(
            &mut self,
            _: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            panic!("offline or malformed requests must not reach transport")
        }

        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            panic!("offline or malformed requests must not reach transport")
        }
    }

    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("service-offline-contract");
    let (owner, _) =
        RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Offline, limits()).expect("owner");
    let service =
        crate::acquisition::AcquisitionService::from_owner(owner, root.join("coordination"))
            .expect("service");
    let mut transport = NeverTransport;
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        "pkg:cargo/offline@1.0.0",
        1,
        0,
    )
    .expect("request");
    assert!(matches!(
        service.acquire(&request, &mut transport),
        crate::acquisition::AcquisitionOutcome::Offline(_)
    ));
    let mut wrong_source = request.clone();
    wrong_source.source = [8; 32];
    assert!(matches!(
        service.acquire(&wrong_source, &mut transport),
        crate::acquisition::AcquisitionOutcome::Rejected(
            crate::acquisition::RejectReason::Protocol
        )
    ));
    let mut wrong_schema = request;
    wrong_schema.schema = 99;
    assert!(matches!(
        service.acquire(&wrong_schema, &mut transport),
        crate::acquisition::AcquisitionOutcome::Rejected(
            crate::acquisition::RejectReason::Protocol
        )
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn acquisition_service_reuses_cached_archive_when_restarted_offline() {
    struct FixtureTransport {
        package: RemotePackage,
        archive: Vec<u8>,
    }
    impl RegistryTransport for FixtureTransport {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [18; 32],
                packages: vec![self.package.clone()],
            }))
        }

        fn fetch_archive(
            &mut self,
            _package: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            Ok(TransportResult::Available(ArchiveArtifact::from_bytes(
                self.archive.clone(),
            )))
        }
    }

    struct NoNetwork;
    impl RegistryTransport for NoNetwork {
        fn fetch_page(
            &mut self,
            _request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            panic!("offline cache reuse must not fetch metadata")
        }

        fn fetch_archive(
            &mut self,
            _package: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            panic!("offline cache reuse must not fetch archives")
        }
    }

    let endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Cargo,
        "http://127.0.0.1:9/acquisition-offline-reuse",
    )
    .expect("endpoint");
    let archive = b"offline cached archive".to_vec();
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/offline-reuse@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(
            *CapabilityArtifactId::from_value(&archive).as_bytes(),
        ),
        provenance: ProvenanceDigest::from_authenticated_feed([18; 32]),
        facts: test_facts(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from("http://127.0.0.1:9/acquisition-offline-reuse/archive"),
    };
    let root = temporary("service-offline-reuse");
    let (owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("open online owner");
    let service = crate::acquisition::AcquisitionService::from_owner(
        owner,
        root.join("service-coordination"),
    )
    .expect("open online service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        package.coordinate.to_string(),
        1,
        0,
    )
    .expect("request");
    let first = service.acquire(
        &request,
        &mut FixtureTransport {
            package,
            archive: archive.clone(),
        },
    );
    let crate::acquisition::AcquisitionOutcome::Hit(first) = first else {
        panic!("initial acquisition must publish: {first:?}");
    };
    assert_eq!(first.artifact.bytes(), archive);
    drop(service);

    let (owner, _) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Offline, limits())
        .expect("open offline owner");
    let service = crate::acquisition::AcquisitionService::from_owner(
        owner,
        root.join("service-coordination"),
    )
    .expect("open offline service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        "pkg:cargo/offline-reuse@1.0.0",
        1,
        0,
    )
    .expect("offline request");
    let reused = service.acquire(&request, &mut NoNetwork);
    let crate::acquisition::AcquisitionOutcome::Hit(reused) = reused else {
        panic!("offline acquisition must reuse the durable archive: {reused:?}");
    };
    assert_eq!(reused.artifact.bytes(), archive);
    // The restarted owner has a different online/offline policy epoch, so
    // the immutable snapshot id changes. The durable feed cursor and source
    // identity must still be exactly the same, proving this was a warm reuse
    // rather than a new feed reservation.
    assert_eq!(reused.snapshot.source(), first.snapshot.source());
    assert_eq!(reused.snapshot.cursor(), first.snapshot.cursor());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn cached_advisory_warning_is_rechecked_after_fail_closed_restart() {
    struct AdvisorySource;
    impl RegistryTransport for AdvisorySource {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            let archive = b"advisory-cached";
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [44; 32],
                packages: vec![RemotePackage {
                    coordinate: PackageCoordinate::parse("pkg:cargo/advisory@1.0.0")
                        .expect("coordinate"),
                    integrity: transport::ArchiveIntegrity::Canonical(
                        *CapabilityArtifactId::from_value(archive).as_bytes(),
                    ),
                    provenance: ProvenanceDigest::from_authenticated_feed([44; 32]),
                    facts: test_facts(),
                    native_metadata: test_native_metadata(),
                    advisory: Some(backend_advisory::AdvisoryObservation {
                        advisories: Box::new([]),
                        coverage: backend_advisory::AdvisoryCoverage::Unknown,
                        freshness: backend_advisory::FreshnessState::Unknown,
                        offline: true,
                        yanked: false,
                        unlisted: false,
                        malware: backend_advisory::MalwareCoverage::NotCovered,
                    }),
                    dependency_facts: unavailable_dependency_facts(),
                    archive_url: Arc::from("https://registry.example.test/advisory.crate"),
                }],
            }))
        }

        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            Ok(TransportResult::Available(ArchiveArtifact::from_bytes(
                b"advisory-cached".to_vec(),
            )))
        }
    }

    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let root = temporary("cached-advisory-recheck");
    let (owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let mut owner = owner.with_advisory_gate(backend_advisory::AcquisitionGate {
        offline: backend_advisory::OfflinePolicy::Warn,
    });
    owner
        .poll(&mut AdvisorySource)
        .expect("warn policy admits cache");
    drop(owner);
    let (owner, _) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("restart owner");
    let owner = owner.with_advisory_gate(backend_advisory::AcquisitionGate {
        offline: backend_advisory::OfflinePolicy::FailClosed,
    });
    let service =
        crate::acquisition::AcquisitionService::from_owner(owner, root.join("coordination"))
            .expect("service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        "pkg:cargo/advisory@1.0.0",
        1,
        0,
    )
    .expect("request");
    let cached_outcome = service.ensure(&request);
    assert!(matches!(
        cached_outcome,
        crate::acquisition::AcquisitionOutcome::NegativeFact(crate::acquisition::NegativeFact {
            kind: crate::acquisition::NegativeFactKind::AdvisoryBlocked,
            ..
        })
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn endpoint_policy_rejects_plaintext_remote_hosts_and_redirects() {
    // HTTPS mirrors and forge-backed package origins are valid configured
    // sources; only their explicitly advertised follow-up authorities are
    // trusted by the transport.
    assert!(
        RegistryEndpoint::new(
            RegistryEcosystem::Cargo,
            "https://packages.example.org/mirror"
        )
        .is_ok()
    );
    assert!(
        RegistryEndpoint::new(
            RegistryEcosystem::Npm,
            "https://github.com/example/registry"
        )
        .is_ok()
    );
    assert!(matches!(
        RegistryEndpoint::new(RegistryEcosystem::Cargo, "http://example.test"),
        Err(AcquisitionError::InvalidConfiguration)
    ));
    let (endpoint_text, server) =
        one_response(302, "Location: http://127.0.0.1:9/stolen\r\n", Vec::new());
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("loopback endpoint");
    let cursor = FeedCursor::genesis(endpoint.id());
    let mut transport = HttpRegistryTransport::new(
        endpoint,
        Some(AuthenticationToken::new("Bearer secret").expect("token")),
        limits(),
    )
    .expect("transport");
    assert!(matches!(
        transport.fetch_page(FeedRequest {
            cursor,
            max_items: 1
        }),
        Err(TransportFailure::Rejected(302))
    ));
    server.join().expect("server");
}

#[test]
fn oversized_feed_is_rejected_before_unbounded_read() {
    let (endpoint_text, server) = one_response(200, "", vec![b'x'; 65]);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("loopback endpoint");
    let cursor = FeedCursor::genesis(endpoint.id());
    let mut tiny = limits();
    tiny.max_feed_bytes = 64;
    let mut transport = HttpRegistryTransport::new(endpoint, None, tiny).expect("transport");
    assert!(matches!(
        transport.fetch_page(FeedRequest {
            cursor,
            max_items: 1
        }),
        Err(TransportFailure::Overrun {
            measured: 65,
            limit: 64
        })
    ));
    server.join().expect("server");
}

#[test]
fn rate_limit_preserves_and_clamps_the_server_retry_delay() {
    for (header, expected) in [
        ("Retry-After: 37\r\n", 37),
        ("Retry-After: 999999\r\n", 3600),
    ] {
        let (endpoint_text, server) = one_response(429, header, Vec::new());
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text)
            .expect("loopback endpoint");
        let cursor = FeedCursor::genesis(endpoint.id());
        let mut transport =
            HttpRegistryTransport::new(endpoint, None, limits()).expect("transport");
        assert!(matches!(
            transport.fetch_page(FeedRequest { cursor, max_items: 1 }),
            Ok(TransportResult::RetryAfter(delay)) if delay == std::time::Duration::from_secs(expected)
        ));
        server.join().expect("server");
    }
}

#[test]
fn server_failures_and_partial_bodies_remain_typed() {
    let (endpoint_text, server) = one_response(500, "", Vec::new());
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("loopback endpoint");
    let cursor = FeedCursor::genesis(endpoint.id());
    let mut transport = HttpRegistryTransport::new(endpoint, None, limits()).expect("transport");
    assert!(matches!(
        transport.fetch_page(FeedRequest {
            cursor,
            max_items: 1
        }),
        Err(TransportFailure::Rejected(500))
    ));
    server.join().expect("500 server");

    let (endpoint_text, server) = one_response(503, "Retry-After: 17\r\n", Vec::new());
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("loopback endpoint");
    let cursor = FeedCursor::genesis(endpoint.id());
    let mut transport = HttpRegistryTransport::new(endpoint, None, limits()).expect("transport");
    assert!(matches!(
        transport.fetch_page(FeedRequest { cursor, max_items: 1 }),
        Ok(TransportResult::RetryAfter(delay)) if delay == std::time::Duration::from_secs(17)
    ));
    server.join().expect("503 server");

    let body = b"{\"schema\":1}".to_vec();
    let (endpoint_text, server) =
        response_with_length(200, "", body.clone(), Some(body.len().saturating_add(1)));
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("loopback endpoint");
    let cursor = FeedCursor::genesis(endpoint.id());
    let mut transport = HttpRegistryTransport::new(endpoint, None, limits()).expect("transport");
    assert!(matches!(
        transport.fetch_page(FeedRequest {
            cursor,
            max_items: 1
        }),
        Err(TransportFailure::Protocol)
    ));
    server.join().expect("partial server");
}

#[test]
fn metadata_404_becomes_a_durable_not_found_fact() {
    let (endpoint_text, server) = one_response(404, "", Vec::new());
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("loopback endpoint");
    let root = temporary("metadata-404");
    let (owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("open owner");
    let service =
        crate::acquisition::AcquisitionService::from_owner(owner, root.join("coordination"))
            .expect("open acquisition service");
    let request = crate::acquisition::AcquisitionRequest::for_coordinate(
        service.source_id(),
        "pkg:cargo/missing@1.0.0",
        1,
        0,
    )
    .expect("request");
    let mut transport = HttpRegistryTransport::new(endpoint, None, limits()).expect("transport");
    assert!(matches!(
        service.acquire(&request, &mut transport),
        crate::acquisition::AcquisitionOutcome::NegativeFact(crate::acquisition::NegativeFact {
            kind: crate::acquisition::NegativeFactKind::NotFound,
            ..
        })
    ));
    server.join().expect("404 server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn all_native_ecosystem_grammars_normalize_and_verify_archives() {
    let archive = b"native registry archive";
    let sha256 = digest_hex(Sha256::digest(archive).as_slice());
    let sha512 = STANDARD.encode(Sha512::digest(archive));
    let origin = "https://registry.example.test";
    let fixtures: Vec<(RegistryEcosystem, Option<&str>, String)> = vec![
        (
            RegistryEcosystem::Cargo,
            None,
            format!("{{\"name\":\"demo\",\"vers\":\"1.2.3\",\"cksum\":\"{sha256}\"}}"),
        ),
        (
            RegistryEcosystem::Npm,
            None,
            format!(
                "{{\"name\":\"demo\",\"versions\":{{\"1.2.3\":{{\"dist\":{{\"integrity\":\"sha512-{sha512}\",\"tarball\":\"{origin}/demo/-/demo-1.2.3.tgz\"}}}}}}}}"
            ),
        ),
        (
            RegistryEcosystem::Pypi,
            None,
            format!(
                "{{\"meta\":{{\"api-version\":\"1.0\"}},\"files\":[{{\"filename\":\"demo-1.2.3.tar.gz\",\"url\":\"{origin}/files/demo-1.2.3.tar.gz\",\"hashes\":{{\"sha256\":\"{sha256}\"}},\"yanked\":false}}]}}"
            ),
        ),
        (
            RegistryEcosystem::Nuget,
            None,
            format!(
                "{{\"items\":[{{\"catalogEntry\":{{\"version\":\"1.2.3\",\"listed\":true}},\"packageContent\":\"{origin}/demo/1.2.3/demo.1.2.3.nupkg\",\"packageHash\":\"{sha512}\"}}]}}"
            ),
        ),
    ];
    for (ecosystem, namespace, fixture) in fixtures {
        let endpoint = RegistryEndpoint::new(ecosystem, origin).expect("endpoint");
        let adapter = EcosystemAdapter::new(
            endpoint,
            PackageName::new("demo").expect("package"),
            namespace
                .map(PackageName::new)
                .transpose()
                .expect("namespace"),
        )
        .expect("adapter");
        assert!(!adapter.metadata_url().is_empty());
        let releases = adapter.decode(fixture.as_bytes()).expect("decode fixture");
        assert_eq!(releases.len(), 1);
        assert_eq!(
            releases[0].coordinate.package_type().registry(),
            Some(ecosystem)
        );
        assert!(releases[0].checksum.verifies(archive));
    }
}

#[test]
fn pypi_simple_api_accepts_current_minor_version() {
    let archive = b"pypi simple api archive";
    let sha256 = digest_hex(Sha256::digest(archive).as_slice());
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Pypi, "https://pypi.org").expect("pypi endpoint");
    let adapter = EcosystemAdapter::new(endpoint, PackageName::new("demo").expect("package"), None)
        .expect("adapter");
    let metadata = format!(
        "{{\"meta\":{{\"api-version\":\"1.4\"}},\"files\":[{{\"filename\":\"demo-1.2.3.tar.gz\",\"url\":\"https://files.pythonhosted.org/demo-1.2.3.tar.gz\",\"hashes\":{{\"sha256\":\"{sha256}\"}},\"yanked\":false}}]}}"
    );
    let releases = adapter
        .decode(metadata.as_bytes())
        .expect("decode current api");
    assert_eq!(releases.len(), 1);
    assert!(releases[0].checksum.verifies(archive));
}

#[test]
fn pypi_json_api_admits_runtime_edges_and_marks_extras_optional() {
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Pypi, "https://pypi.org").expect("pypi endpoint");
    let adapter = EcosystemAdapter::new(endpoint, PackageName::new("demo").expect("package"), None)
        .expect("adapter");
    assert_eq!(
        adapter.pypi_json_url("1.0.0+local"),
        "https://pypi.org/pypi/demo/1.0.0%2Blocal/json"
    );
    let source = adapter.coordinate_for_version("1.2.3").expect("coordinate");
    let body = br#"{
        "info": {
            "name": "Demo",
            "version": "1.2.3",
            "requires_dist": [
                "charset-normalizer<4,>=2",
                "urllib3>=1.21.1; python_version >= \"3.7\"",
                "PySocks!=1.5.7,>=1.5.6; extra == \"socks\"",
                "demo-tool @ https://example.test/demo-tool.whl",
                "platform-dep>=1; sys_platform == \"extra\""
            ]
        }
    }"#;
    let facts = adapter
        .pypi_requires_dist(body, &source, body)
        .expect("requires_dist");
    let backend_library::DependencyFacts::Known(rows) = facts else {
        panic!("expected known requires_dist");
    };
    assert_eq!(rows.len(), 5);
    let by_name = |name: &str| {
        rows.iter()
            .find(|row| row.target.name.as_str() == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let charset = by_name("charset-normalizer");
    assert_eq!(charset.scope, backend_library::DependencyScope::Runtime);
    assert!(!charset.optional);
    assert_eq!(
        charset.target.requirement.as_str(),
        "charset-normalizer<4,>=2"
    );
    let urllib = by_name("urllib3");
    assert_eq!(urllib.scope, backend_library::DependencyScope::Runtime);
    assert!(!urllib.optional);
    let socks = by_name("PySocks");
    assert_eq!(socks.scope, backend_library::DependencyScope::Optional);
    assert!(socks.optional);
    assert!(
        socks
            .target
            .requirement
            .as_str()
            .contains("extra == \"socks\"")
    );
    let direct = by_name("demo-tool");
    assert_eq!(direct.scope, backend_library::DependencyScope::Runtime);
    assert!(direct.target.requirement.as_str().contains('@'));
    let platform = by_name("platform-dep");
    assert_eq!(platform.scope, backend_library::DependencyScope::Runtime);
    assert!(!platform.optional);
    assert!(matches!(
        adapter.pypi_requires_dist(
            br#"{"info":{"name":"demo","version":"1.2.3","requires_dist":null}}"#,
            &source,
            b"null",
        ),
        Ok(backend_library::DependencyFacts::Unknown(_))
    ));
    assert!(matches!(
        adapter.pypi_requires_dist(
            br#"{"info":{"name":"demo","version":"1.2.3","requires_dist":[]}}"#,
            &source,
            b"empty",
        ),
        Ok(backend_library::DependencyFacts::Known(rows)) if rows.is_empty()
    ));
    assert!(
        adapter
            .pypi_requires_dist(
                br#"{"info":{"name":"other","version":"1.2.3","requires_dist":[]}}"#,
                &source,
                b"other",
            )
            .is_err()
    );
    assert!(adapter
        .pypi_requires_dist(
            br#"{"info":{"name":"demo","version":"1.2.3","requires_dist":["charset-normalizer","charset-normalizer"]}}"#,
            &source,
            b"duplicate",
        )
        .is_err());
}

#[test]
fn version_pinned_native_adapter_admits_one_release_and_names_its_snapshot() {
    let archive = b"npm target archive";
    let integrity = STANDARD.encode(Sha512::digest(archive));
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Npm, "https://registry.npmjs.org")
        .expect("npm endpoint");
    let target = EcosystemAdapter::new_with_version(
        endpoint.clone(),
        PackageName::new("parser").expect("package"),
        Some(PackageName::new("@babel").expect("namespace")),
        PackageVersion::new("7.26.8").expect("version"),
    )
    .expect("target adapter");
    let metadata = format!(
        r#"{{"name":"@babel/parser","versions":{{"7.26.7":{{"dist":{{"integrity":"sha512-{integrity}","tarball":"https://registry.npmjs.org/parser-7.26.7.tgz"}}}},"7.26.8":{{"dist":{{"integrity":"sha512-{integrity}","tarball":"https://registry.npmjs.org/parser-7.26.8.tgz"}}}}}}}}"#
    );
    let releases = target.decode(metadata.as_bytes()).expect("target decode");
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].coordinate.version(), "7.26.8");

    let broad = EcosystemAdapter::new(
        endpoint,
        PackageName::new("parser").expect("package"),
        Some(PackageName::new("@babel").expect("namespace")),
    )
    .expect("broad adapter");
    assert_ne!(
        target.page_identity(metadata.as_bytes()),
        broad.page_identity(metadata.as_bytes())
    );
}

#[test]
fn protocol_phase_adapters_keep_maven_go_and_conan_typed() {
    let maven = EcosystemAdapter::new(
        RegistryEndpoint::new(RegistryEcosystem::Maven, "https://repo1.maven.org")
            .expect("maven endpoint"),
        PackageName::new("commons-lang3").expect("artifact"),
        Some(PackageName::new("org.apache.commons").expect("group")),
    )
    .expect("maven adapter");
    let metadata = br#"<metadata><groupId>org.apache.commons</groupId><artifactId>commons-lang3</artifactId><versioning><latest>3.18.0</latest><versions><version>3.17.0</version><version>3.18.0</version></versions></versioning></metadata>"#;
    assert_eq!(
        maven
            .maven_versions(metadata)
            .expect("maven metadata")
            .len(),
        2
    );

    let go = EcosystemAdapter::new(
        RegistryEndpoint::new(RegistryEcosystem::Golang, "https://proxy.golang.org")
            .expect("go endpoint"),
        PackageName::new("errors").expect("name"),
        Some(PackageName::new("github.com/pkg").expect("module namespace")),
    )
    .expect("go adapter");
    assert_eq!(
        go.go_versions(b"v0.9.1\nv0.8.0\n")
            .expect("go listing")
            .len(),
        2
    );

    let conan = EcosystemAdapter::new(
        RegistryEndpoint::new(RegistryEcosystem::Cpp, "https://center2.conan.io")
            .expect("conan endpoint"),
        PackageName::new("zlib").expect("name"),
        Some(PackageName::new("1.3.1").expect("version")),
    )
    .expect("conan adapter");
    assert!(matches!(
        conan.decode(br#"{}"#),
        Err(TransportFailure::Protocol)
    ));
}

#[test]
fn go_proxy_lists_pseudo_versions_and_rejects_ambiguous_rows() {
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Golang, "https://proxy.golang.org")
        .expect("go endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint,
        PackageName::new("errors").expect("name"),
        Some(PackageName::new("github.com/pkg").expect("namespace")),
    )
    .expect("adapter");
    let versions = adapter
        .go_versions(b"v1.10.0\nv1.9.0\nv1.10.0-0.20240101120000-deadbeefdead\n")
        .expect("pseudo version list");
    assert_eq!(versions.len(), 3);
    assert!(versions.contains(&"v1.10.0-0.20240101120000-deadbeefdead".to_owned()));
    assert!(adapter.go_versions(b"v1.0.0\nv1.0.0\n").is_err());
    assert!(adapter.go_versions(b" v1.0.0\n").is_err());
    assert!(adapter.go_versions(b"v1.0\n").is_err());
}

#[test]
fn go_info_mod_retractions_and_dependencies_are_retained_as_typed_facts() {
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Golang, "https://proxy.golang.org")
        .expect("go endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint,
        PackageName::new("mod").expect("name"),
        Some(PackageName::new("example.com/acme").expect("namespace")),
    )
    .expect("adapter");
    let info = adapter
        .go_info(
            br#"{"Version":"v1.2.3","Time":"2024-01-02T03:04:05Z","Future":true}"#,
            "v1.2.3",
        )
        .expect("info");
    assert_eq!(info.version, "v1.2.3");
    assert!(
        adapter
            .go_info(br#"{"Version":"v1.2.4"}"#, "v1.2.3")
            .is_err()
    );

    let module = adapter
        .go_mod(
            br#"module example.com/acme/mod

go 1.22

require (
    example.com/dep v1.4.0
    example.com/indirect v0.2.0 // indirect
)

retract [v1.2.0, v1.2.3]
"#,
            "v1.2.3",
        )
        .expect("go.mod");
    assert_eq!(module.requires.len(), 2);
    assert_eq!(module.requires[1].module, "example.com/indirect");
    assert!(module.retracts[0].contains("v1.2.3"));
    assert!(!module.retracts[0].contains("v1.2.4"));
    let coordinate =
        PackageCoordinate::parse("pkg:golang/example.com/acme/mod@v1.2.3").expect("coordinate");
    let dependencies = adapter
        .go_dependencies(&coordinate, &module, b"module frontier")
        .expect("dependencies");
    let backend_library::DependencyFacts::Known(rows) = dependencies else {
        panic!("expected known Go dependencies");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].target.ecosystem, RegistryEcosystem::Golang);
    let dep = rows
        .iter()
        .find(|row| row.target.name.as_str() == "example.com/dep")
        .expect("direct require");
    assert_eq!(dep.scope, backend_library::DependencyScope::Runtime);
    assert!(!dep.optional);
    let indirect = rows
        .iter()
        .find(|row| row.target.name.as_str() == "example.com/indirect")
        .expect("indirect require");
    assert_eq!(indirect.scope, backend_library::DependencyScope::Development);
    assert!(!indirect.optional);
}

#[test]
fn go_direct_and_indirect_same_module_collapses_to_runtime() {
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Golang, "https://proxy.golang.org")
        .expect("go endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint,
        PackageName::new("mod").expect("name"),
        Some(PackageName::new("example.com/acme").expect("namespace")),
    )
    .expect("adapter");
    let module = adapter
        .go_mod(
            br#"module example.com/acme/mod

require (
    example.com/shared v1.0.0
    example.com/shared v1.0.0 // indirect
)
"#,
            "v1.0.0",
        )
        .expect("go.mod");
    let coordinate =
        PackageCoordinate::parse("pkg:golang/example.com/acme/mod@v1.0.0").expect("coordinate");
    let dependencies = adapter
        .go_dependencies(&coordinate, &module, b"module frontier")
        .expect("dependencies");
    let backend_library::DependencyFacts::Known(rows) = dependencies else {
        panic!("expected known Go dependencies");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].target.name.as_str(), "example.com/shared");
    assert_eq!(rows[0].scope, backend_library::DependencyScope::Runtime);
    assert!(!rows[0].optional);
}

#[test]
fn conan_v2_file_manifest_is_bounded_and_keeps_source_availability_typed() {
    let adapter = EcosystemAdapter::new(
        RegistryEndpoint::new(RegistryEcosystem::Cpp, "https://center2.conan.io")
            .expect("Conan endpoint"),
        PackageName::new("zlib").expect("package"),
        Some(PackageName::new("1.3.1").expect("recipe version")),
    )
    .expect("adapter");
    let source = b"source archive";
    let digest = digest_hex(Sha256::digest(source).as_slice());
    let manifest = adapter
        .conan_file_manifest(
            format!(
                r#"{{"files":{{"conan_export.tgz":{{}},"conan_sources.tgz":{{"sha256":"{digest}","size":14}}}}}}"#
            )
            .as_bytes(),
        )
        .expect("manifest");
    assert_eq!(
        manifest.preferred_archive_name(),
        Ok("conan_sources.tgz".to_owned())
    );
    assert_eq!(
        manifest
            .entry("conan_sources.tgz")
            .and_then(|entry| entry.size),
        Some(14)
    );

    let oversized = format!(
        r#"{{"files":{{{}}}}}"#,
        (0..=4096)
            .map(|index| format!(r#""file-{index}":{{}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(matches!(
        adapter.conan_file_manifest(oversized.as_bytes()),
        Err(TransportFailure::Overrun {
            measured: 4097,
            limit: 4096
        })
    ));
    assert!(matches!(
        adapter.conan_file_manifest(br#"{"files":{"../escape.tgz":{}}}"#),
        Err(TransportFailure::Protocol)
    ));
}

#[test]
fn maven_metadata_resolves_snapshot_sidecars_and_rejects_xml_expansion() {
    let adapter = EcosystemAdapter::new(
        RegistryEndpoint::new(RegistryEcosystem::Maven, "https://repo.maven.apache.org")
            .expect("maven endpoint"),
        PackageName::new("demo").expect("artifact"),
        Some(PackageName::new("com.example").expect("group")),
    )
    .expect("maven adapter");
    let metadata = br#"
        <?xml version="1.0" encoding="UTF-8"?>
        <metadata>
          <groupId>com.example</groupId><artifactId>demo</artifactId>
          <versioning><latest>1.1.0</latest><release>1.1.0</release>
            <versions><version>1.0-SNAPSHOT</version><version>1.1.0</version></versions>
            <snapshotVersions>
              <snapshotVersion><extension>jar</extension><value>1.0-20260921.120000-4</value></snapshotVersion>
              <snapshotVersion><extension>jar</extension><classifier>sources</classifier><value>1.0-20260921.120000-4</value></snapshotVersion>
              <snapshotVersion><extension>pom</extension><value>1.0-20260921.120000-4</value></snapshotVersion>
            </snapshotVersions>
          </versioning>
        </metadata>
    "#;
    let parsed = adapter.maven_metadata(metadata).expect("metadata");
    assert_eq!(parsed.group, "com.example");
    assert_eq!(parsed.artifact, "demo");
    assert_eq!(parsed.latest.as_deref(), Some("1.1.0"));
    assert_eq!(parsed.release.as_deref(), Some("1.1.0"));
    assert_eq!(parsed.versions.len(), 2);
    assert_eq!(parsed.versions[0].version, "1.0-SNAPSHOT");
    assert_eq!(
        parsed.versions[0].timestamped_sources.as_deref(),
        Some("1.0-20260921.120000-4")
    );
    assert!(
        adapter
            .maven_source_archive_url_for(
                "1.0-SNAPSHOT",
                parsed.versions[0].timestamped_sources.as_deref()
            )
            .ends_with("/com/example/demo/1.0-SNAPSHOT/demo-1.0-20260921.120000-4-sources.jar")
    );

    let xxe = br#"<!DOCTYPE metadata [ <!ENTITY xxe SYSTEM "file:///etc/passwd"> ]>
        <metadata><groupId>com.example</groupId><artifactId>demo</artifactId>
        <versioning><versions><version>&xxe;</version></versions></versioning></metadata>"#;
    assert!(matches!(
        adapter.maven_metadata(xxe),
        Err(TransportFailure::Protocol)
    ));

    let oversized = vec![b'x'; 8 * 1024 * 1024 + 1];
    assert!(matches!(
        adapter.maven_metadata(&oversized),
        Err(TransportFailure::Overrun { .. })
    ));
}

#[test]
fn maven_transport_fetches_checksum_signature_and_pom_dependencies() {
    let source_archive = b"maven source archive";
    let checksum = digest_hex(Sha256::digest(source_archive).as_slice());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind maven fixture");
    let address = listener.local_addr().expect("maven fixture address");
    let endpoint_text = format!("http://{address}");
    let metadata = br#"<?xml version="1.0" encoding="UTF-8"?>
      <metadata><groupId>com.example</groupId><artifactId>demo</artifactId>
      <versioning><latest>1.2.3</latest><release>1.2.3</release>
      <versions><version>1.2.3</version></versions></versioning></metadata>"#
        .to_vec();
    let pom = br#"<?xml version="1.0" encoding="UTF-8"?>
      <project><modelVersion>4.0.0</modelVersion>
      <groupId>com.example</groupId><artifactId>demo</artifactId><version>1.2.3</version>
      <dependencyManagement><dependencies><dependency><groupId>ignored</groupId>
      <artifactId>management-only</artifactId><version>9.9.9</version></dependency></dependencies>
      </dependencyManagement><dependencies>
      <dependency><groupId>org.example</groupId><artifactId>runtime</artifactId>
      <version>2.0.0</version></dependency>
      <dependency><groupId>org.example</groupId><artifactId>tests</artifactId>
      <version>3.0.0</version><scope>test</scope></dependency>
      </dependencies></project>"#
        .to_vec();
    let server = thread::spawn(move || {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().expect("accept maven request");
            let request = read_request_head(&mut stream, "Maven fixture request")
                .expect("read Maven request headers");
            let request = String::from_utf8_lossy(&request);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("request path");
            let body = if path.ends_with("maven-metadata.xml") {
                metadata.clone()
            } else if path.ends_with("-sources.jar.sha256") {
                checksum.as_bytes().to_vec()
            } else if path.ends_with("-sources.jar.asc") {
                b"signed-by-fixture".to_vec()
            } else if path.ends_with(".pom") {
                pom.clone()
            } else {
                panic!("unexpected Maven fixture path: {path}");
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("maven headers");
            stream.write_all(&body).expect("maven body");
        }
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Maven, endpoint_text).expect("maven endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint.clone(),
        PackageName::new("demo").expect("artifact"),
        Some(PackageName::new("com.example").expect("group")),
    )
    .expect("maven adapter");
    let mut transport =
        HttpRegistryTransport::for_native(adapter, None, limits()).expect("maven transport");
    let page = match transport
        .fetch_page(FeedRequest {
            cursor: FeedCursor::genesis(endpoint.id()),
            max_items: 4,
        })
        .expect("maven page")
    {
        TransportResult::Available(page) => page,
        other => panic!("expected available Maven page, got {other:?}"),
    };
    assert_eq!(page.packages.len(), 1);
    let package = &page.packages[0];
    assert!(
        package
            .archive_url
            .ends_with("/com/example/demo/1.2.3/demo-1.2.3-sources.jar")
    );
    assert!(matches!(
        &package.dependency_facts,
        backend_library::DependencyFacts::Known(rows) if rows.len() == 2
    ));
    assert!(matches!(
        &package.native_metadata.details,
        backend_library::RegistryNativeDetails::Maven(metadata)
            if matches!(&metadata.checksum, backend_library::RegistryNativeObservation::Recorded(_))
                && matches!(&metadata.signature, backend_library::RegistryNativeObservation::Recorded(_))
                && matches!(&metadata.pom, backend_library::RegistryNativeObservation::Recorded(_))
                && matches!(&metadata.dependencies, backend_library::RegistryNativeObservation::Recorded(_))
                && metadata.artifacts[0].yanked.is_none()
    ));
    assert_eq!(
        package.facts.native_metadata_version(),
        package
            .native_metadata
            .identity()
            .expect("Maven metadata identity")
    );
    server.join().expect("maven server");
}

#[test]
fn nuget_registration_retains_standing_security_downloads_and_dependencies() {
    let archive = b"nuget registration archive";
    let hash = STANDARD.encode(Sha512::digest(archive));
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Nuget, "https://api.nuget.org")
        .expect("nuget endpoint");
    let adapter = EcosystemAdapter::new(endpoint, PackageName::new("demo").expect("package"), None)
        .expect("nuget adapter");
    let metadata = format!(
        r#"{{"items":[
          {{"catalogEntry":{{"version":"1.0.0","listed":false,"downloads":7,
            "vulnerabilities":[{{"severity":"3","advisoryUrl":"https://example.test/a"}}],
            "dependencyGroups":[{{"targetFramework":"net8.0","dependencies":[{{"id":"dep","range":"[2.0.0]"}}]}},{{"targetFramework":"netstandard2.0"}}]}},
           "packageContent":"https://api.nuget.org/v3-flatcontainer/demo/1.0.0/demo.1.0.0.nupkg","packageHash":"{hash}"}},
          {{"catalogEntry":{{"version":"1.1.0","listed":true,"deprecation":{{"message":"use 2.x"}}}},
           "packageContent":"https://api.nuget.org/v3-flatcontainer/demo/1.1.0/demo.1.1.0.nupkg","packageHash":"{hash}"}}
        ]}}"#
    );
    let releases = adapter.decode(metadata.as_bytes()).expect("registration");
    assert_eq!(releases.len(), 2);
    assert_eq!(releases[0].facts.standing(), ReleaseStanding::Unlisted);
    assert_eq!(releases[0].facts.downloads(), DownloadCount::Exact(7));
    assert_eq!(
        releases[0].facts.security(),
        SecurityStanding::Affected {
            advisories: 1,
            maximum_severity: 3
        }
    );
    assert!(matches!(
        &releases[0].dependency_facts,
        backend_library::DependencyFacts::Known(rows) if rows.len() == 1
    ));
    assert!(matches!(
        &releases[0].native_metadata.details,
        backend_library::RegistryNativeDetails::Nuget(metadata)
            if matches!(&metadata.vulnerabilities, backend_library::RegistryNativeObservation::Recorded(rows) if rows.len() == 1)
                && matches!(&metadata.dependencies, backend_library::RegistryNativeObservation::Recorded(backend_library::DependencyFacts::Known(rows)) if rows.len() == 1)
                && metadata.artifacts[0].yanked.is_none()
    ));
    assert_eq!(releases[1].facts.standing(), ReleaseStanding::Deprecated);
    assert!(matches!(
        &releases[1].native_metadata.details,
        backend_library::RegistryNativeDetails::Nuget(metadata) if metadata.deprecation.as_deref() == Some("use 2.x")
    ));
}

#[test]
fn nuget_v3_registration_pages_are_traversed_deterministically() {
    let archive = b"nuget paged archive";
    let hash = STANDARD.encode(Sha512::digest(archive));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind nuget fixture");
    let address = listener.local_addr().expect("nuget fixture address");
    let endpoint_text = format!("http://{address}");
    let registration = format!("{endpoint_text}/v3/registration5-semver1");
    let root_url = format!("{registration}/demo/index.json");
    let page_url = format!("{registration}/demo/page0.json");
    let service_index = format!(
        r#"{{"resources":[
          {{"@id":"{endpoint_text}/v3/registration3","@type":["RegistrationsBaseUrl/3.0.0"]}},
          {{"@id":"{registration}/","@type":["RegistrationsBaseUrl","RegistrationsBaseUrl/3.6.0","RegistrationsBaseUrl/3.0.0"]}},
          {{"@id":"{endpoint_text}/v3-flatcontainer","@type":"PackageBaseAddress/3.0.0"}}
        ]}}"#
    )
    .into_bytes();
    let registration_root = format!(
        r#"{{"@id":"{root_url}","count":2,"items":[
          {{"@id":"{page_url}","count":1,"lower":"1.0.0","upper":"1.0.0"}},
          {{"catalogEntry":{{"version":"2.0.0","listed":true}},"packageContent":"{endpoint_text}/v3-flatcontainer/demo/2.0.0/demo.2.0.0.nupkg","packageHash":"{hash}"}}
        ]}}"#
    )
    .into_bytes();
    let registration_page = format!(
        r#"{{"@id":"{page_url}","count":1,"items":[
          {{"catalogEntry":{{"version":"1.0.0","listed":true}},"packageContent":"{endpoint_text}/v3-flatcontainer/demo/1.0.0/demo.1.0.0.nupkg","packageHash":"{hash}"}}
        ]}}"#
    )
    .into_bytes();
    let responses = [service_index, registration_root, registration_page];
    let server = thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().expect("accept nuget request");
            let request = read_request_head(&mut stream, "NuGet registration request")
                .expect("read NuGet request headers");
            assert!(!request.is_empty());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("nuget headers");
            stream.write_all(&body).expect("nuget body");
        }
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Nuget, endpoint_text).expect("nuget endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint.clone(),
        PackageName::new("demo").expect("package"),
        None,
    )
    .expect("nuget adapter");
    let mut transport =
        HttpRegistryTransport::for_native(adapter, None, limits()).expect("nuget transport");
    let cursor = FeedCursor::genesis(endpoint.id());
    let page = match transport
        .fetch_page(FeedRequest {
            cursor,
            max_items: 4,
        })
        .expect("nuget page")
    {
        TransportResult::Available(page) => page,
        other => panic!("expected available nuget page, got {other:?}"),
    };
    assert_eq!(page.packages.len(), 2);
    assert_eq!(page.packages[0].coordinate.version(), "1.0.0");
    assert_eq!(page.packages[1].coordinate.version(), "2.0.0");
    server.join().expect("nuget server");
}

#[test]
fn native_cargo_adapter_runs_through_shared_owner_and_cursor() {
    let archive = b"cargo archive through native adapter";
    let checksum = digest_hex(Sha256::digest(archive).as_slice());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind native registry");
    let address = listener.local_addr().expect("native address");
    let endpoint_text = format!("http://{address}");
    let metadata =
        format!("{{\"name\":\"demo\",\"vers\":\"1.2.3\",\"cksum\":\"{checksum}\"}}\n").into_bytes();
    let responses = [metadata, archive.to_vec()];
    let server = thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().expect("accept native request");
            let _ = read_request_head(&mut stream, "native registry request")
                .expect("read native request headers");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("native headers");
            stream.write_all(&body).expect("native body");
        }
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("native endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint.clone(),
        PackageName::new("demo").expect("package"),
        None,
    )
    .expect("native adapter");
    let mut transport =
        HttpRegistryTransport::for_native(adapter, None, limits()).expect("native transport");
    let root = temporary("native-owner");
    let (mut owner, _) = RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
        .expect("native owner");
    let AcquisitionOutcome::Published(receipt) = owner.poll(&mut transport).expect("native poll")
    else {
        panic!("expected native publication")
    };
    assert_eq!(receipt.target.sequence(), 1);
    assert_eq!(receipt.packages.len(), 1);
    assert_eq!(
        owner
            .read_artifact(&receipt.packages[0].coordinate)
            .expect("native artifact")
            .expect("published native artifact")
            .bytes(),
        archive
    );
    server.join().expect("native server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn package_coordinates_are_closed_and_render_canonically() {
    let cases = [
        ("pkg:cargo/serde@1.0.0", RegistryEcosystem::Cargo),
        ("pkg:npm/@types/node@20.0.0", RegistryEcosystem::Npm),
        ("pkg:pypi/typing_extensions@4.0.0", RegistryEcosystem::Pypi),
        (
            "pkg:maven/com.google.guava/guava@33.0.0-jre",
            RegistryEcosystem::Maven,
        ),
        ("pkg:nuget/AutoMapper@13.0.0", RegistryEcosystem::Nuget),
        (
            "pkg:golang/github.com/pkg/errors@v0.9.1",
            RegistryEcosystem::Golang,
        ),
        (
            "pkg:generic/stable/example/zlib@1.2.13",
            RegistryEcosystem::Cpp,
        ),
    ];
    for (text, ecosystem) in cases {
        let coordinate = PackageCoordinate::parse(text).expect("valid package URL");
        assert_eq!(coordinate.package_type().registry(), Some(ecosystem));
        assert_eq!(coordinate.as_str(), text);
    }
    for text in [
        "serde",
        "pkg:cargo/serde",
        "pkg:cargo/serde@",
        "pkg:unknown/serde@1.0.0",
    ] {
        assert!(PackageCoordinate::parse(text).is_err());
    }
    let qualified = PackageCoordinate::parse("pkg:cargo/serde@1.0.0?source=other")
        .expect("qualifiers remain part of general package identity");
    assert!(matches!(
        admit_registry_coordinate(&qualified),
        Err(AcquisitionError::InvalidCoordinate)
    ));
    let incomplete_maven = PackageCoordinate::parse("pkg:maven/guava@33.0.0")
        .expect("valid package identity independent of registry policy");
    assert!(matches!(
        admit_registry_coordinate(&incomplete_maven),
        Err(AcquisitionError::InvalidCoordinate)
    ));
}

#[test]
fn cold_cache_resolution_is_typed_for_every_ecosystem() {
    struct PanicTransport;
    impl RegistryTransport for PanicTransport {
        fn fetch_page(
            &mut self,
            _: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            panic!("offline resolution must not touch the network")
        }
        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            panic!("offline resolution must not touch the network")
        }
    }
    let cases = [
        (RegistryEcosystem::Cargo, "pkg:cargo/serde@1.0.0"),
        (RegistryEcosystem::Npm, "pkg:npm/@types/node@20.0.0"),
        (RegistryEcosystem::Pypi, "pkg:pypi/typing_extensions@4.0.0"),
        (
            RegistryEcosystem::Maven,
            "pkg:maven/com.google.guava/guava@33.0.0-jre",
        ),
        (RegistryEcosystem::Nuget, "pkg:nuget/AutoMapper@13.0.0"),
        (
            RegistryEcosystem::Golang,
            "pkg:golang/github.com/pkg/errors@v0.9.1",
        ),
        (
            RegistryEcosystem::Cpp,
            "pkg:generic/stable/example/zlib@1.2.13",
        ),
    ];
    for (ecosystem, text) in cases {
        let coordinate = PackageCoordinate::parse(text).expect("representative coordinate");
        assert_eq!(coordinate.package_type().registry(), Some(ecosystem));
        let endpoint =
            RegistryEndpoint::new(ecosystem, "https://registry.example.test").expect("endpoint");
        let root = temporary("cold-cache");
        let (mut owner, recovery) = RegistryOwner::open(
            &root,
            endpoint.clone(),
            AcquisitionPolicy::Offline,
            limits(),
        )
        .expect("cold-cache owner");
        assert_eq!(recovery.cursor.sequence(), 0);
        assert!(
            matches!(
                owner.poll(&mut PanicTransport).expect("typed offline"),
                AcquisitionOutcome::Offline { .. }
            ),
            "{ecosystem:?} cold cache must resolve to a typed offline terminal"
        );
        let layout = storage_root(&root, &endpoint);
        assert!(
            layout.join("registry.journal").is_file(),
            "{ecosystem:?} cache root is not deterministic"
        );
        assert!(layout.join("registry-content").is_dir());
        fs::remove_dir_all(root).expect("cleanup");
    }
}

#[test]
fn acquisition_cache_layout_is_deterministic_and_ecosystem_scoped() {
    let cargo = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("cargo endpoint");
    let npm = RegistryEndpoint::new(RegistryEcosystem::Npm, "https://registry.example.test")
        .expect("npm endpoint");
    let root = temporary("layout");
    let cargo_root = storage_root(&root, &cargo);
    assert_eq!(
        cargo_root,
        root.join("cargo")
            .join(transport::hex(&cargo.id().as_bytes()))
    );
    assert_eq!(cargo_root, storage_root(&root, &cargo));
    assert_ne!(cargo_root, storage_root(&root, &npm));
    let (owner, _) =
        RegistryOwner::open(&root, cargo.clone(), AcquisitionPolicy::Offline, limits())
            .expect("owner");
    assert!(cargo_root.join("registry.journal").is_file());
    assert!(cargo_root.join("registry-content").is_dir());
    drop(owner);
    let (_, recovery) =
        RegistryOwner::open(&root, cargo, AcquisitionPolicy::Offline, limits()).expect("reopen");
    assert_eq!(recovery.cursor.sequence(), 0);
    assert!(recovery.pending.is_none());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn unavailable_source_is_a_typed_terminal_for_every_ecosystem() {
    struct UnavailableTransport;
    impl RegistryTransport for UnavailableTransport {
        fn fetch_page(
            &mut self,
            _: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            Ok(TransportResult::Unavailable)
        }
        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            panic!("archive called without an admitted page")
        }
    }
    for ecosystem in [
        RegistryEcosystem::Cargo,
        RegistryEcosystem::Npm,
        RegistryEcosystem::Pypi,
        RegistryEcosystem::Maven,
        RegistryEcosystem::Nuget,
        RegistryEcosystem::Golang,
        RegistryEcosystem::Cpp,
    ] {
        let endpoint =
            RegistryEndpoint::new(ecosystem, "https://registry.example.test").expect("endpoint");
        let root = temporary("unavailable-eco");
        let (mut owner, _) =
            RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
                .expect("owner");
        assert!(matches!(
            owner.poll(&mut UnavailableTransport).expect("typed"),
            AcquisitionOutcome::Unavailable { .. }
        ));
        assert_eq!(owner.cursor().sequence(), 0);
        drop(owner);
        let (_, recovery) =
            RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits())
                .expect("recovery");
        assert!(recovery.pending.is_some(), "durable retry intent retained");
        assert_eq!(recovery.cursor.sequence(), 0);
        fs::remove_dir_all(root).expect("cleanup");
    }
}

#[test]
fn archive_extent_over_the_cap_is_a_measured_typed_overrun() {
    let archive = vec![0x5a_u8; 4096];
    let (endpoint_text, server) = one_response(200, "", archive);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text.clone()).expect("endpoint");
    let mut constrained = limits();
    constrained.max_archive_bytes = 256;
    constrained.max_page_archive_bytes = 256;
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/oversized@1.0.0").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical([0; 32]),
        provenance: ProvenanceDigest::from_authenticated_feed([9; 32]),
        facts: test_facts(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from(endpoint_text.as_str()),
    };
    let mut http = HttpRegistryTransport::new(endpoint, None, constrained).expect("transport");
    match http.fetch_archive(&package) {
        Err(TransportFailure::Overrun { measured, limit }) => {
            assert_eq!(limit, 256);
            assert!(measured > limit, "measured extent must exceed the cap");
        }
        other => panic!("expected measured typed overrun, got {other:?}"),
    }
    server.join().expect("server");

    struct OverrunTransport {
        package: RemotePackage,
    }
    impl RegistryTransport for OverrunTransport {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [7; 32],
                packages: vec![self.package.clone()],
            }))
        }

        fn fetch_archive(
            &mut self,
            _: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            Err(TransportFailure::Overrun {
                measured: 257,
                limit: 256,
            })
        }
    }

    let durable_endpoint = RegistryEndpoint::new(
        RegistryEcosystem::Cargo,
        "https://registry.example.test/archive-overrun",
    )
    .expect("durable endpoint");
    let root = temporary("archive-overrun");
    let (mut owner, _) = RegistryOwner::open(
        &root,
        durable_endpoint,
        AcquisitionPolicy::Online,
        constrained,
    )
    .expect("owner");
    let mut overrun = OverrunTransport { package };
    assert!(matches!(
        owner.poll(&mut overrun),
        Err(AcquisitionError::Transport(TransportFailure::Overrun {
            measured: 257,
            limit: 256
        }))
    ));
    assert_eq!(owner.cursor().sequence(), 0, "no cursor advance on overrun");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn default_limits_admit_large_sdists_and_more_than_two_hundred_packages() {
    let limits = AcquisitionLimits::default();
    assert!(limits.max_items >= 200);
    assert!(limits.max_archive_bytes > 64 * 1024 * 1024);
    assert!(limits.max_page_archive_bytes >= limits.max_archive_bytes);
    assert!(limits.max_catalog_items >= 200);
    assert!(limits.validate().is_ok());
}

#[test]
fn native_metadata_admits_more_than_two_hundred_versions() {
    let origin = "https://registry.example.test";
    let checksum = "0".repeat(64);
    let rows = (0..250)
        .map(|index| {
            format!("{{\"name\":\"demo\",\"vers\":\"1.0.{index}\",\"cksum\":\"{checksum}\"}}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, origin).expect("endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint.clone(),
        PackageName::new("demo").expect("package"),
        None,
    )
    .expect("adapter");
    let releases = adapter
        .decode(rows.as_bytes())
        .expect("decode 250 versions");
    assert_eq!(releases.len(), 250);
    assert!(matches!(
        &releases[0].metadata().details,
        backend_library::RegistryNativeDetails::Cargo(_)
    ));
    assert_eq!(
        releases[0].facts.native_metadata_version(),
        releases[0]
            .metadata()
            .identity()
            .expect("metadata identity")
    );
    let page = adapter
        .admit_page(
            rows.as_bytes(),
            FeedRequest {
                cursor: FeedCursor::genesis(endpoint.id()),
                max_items: 250,
            },
        )
        .expect("admit 250 versions");
    assert_eq!(page.packages.len(), 250);
    assert_eq!(
        page.packages[0].native_metadata,
        releases[0].native_metadata
    );
}

#[test]
fn sparse_cargo_chunks_resume_and_changed_snapshot_restarts() {
    let origin = "https://registry.example.test";
    let checksum = "0".repeat(64);
    let rows = (0..5)
        .map(|index| {
            format!("{{\"name\":\"demo\",\"vers\":\"1.0.{index}\",\"cksum\":\"{checksum}\"}}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, origin).expect("endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint.clone(),
        PackageName::new("demo").expect("package"),
        None,
    )
    .expect("adapter");
    let first = adapter
        .admit_page(
            rows.as_bytes(),
            FeedRequest {
                cursor: FeedCursor::genesis(endpoint.id()),
                max_items: 2,
            },
        )
        .expect("first chunk");
    assert_eq!(first.packages.len(), 2);
    assert_eq!(first.base.sequence(), 0);
    let resumed_cursor = first.base.advance(first.next_token).expect("advance");
    let second = adapter
        .admit_page(
            rows.as_bytes(),
            FeedRequest {
                cursor: resumed_cursor,
                max_items: 2,
            },
        )
        .expect("resume chunk");
    assert_eq!(second.packages.len(), 2);
    assert_eq!(second.base, resumed_cursor);
    assert!(second.packages[0].coordinate.as_str().contains("1.0.2"));
    let third_cursor = second.base.advance(second.next_token).expect("advance");
    let third = adapter
        .admit_page(
            rows.as_bytes(),
            FeedRequest {
                cursor: third_cursor,
                max_items: 2,
            },
        )
        .expect("final chunk");
    assert_eq!(third.packages.len(), 1);
    // A changed metadata body carries a new snapshot prefix, so the same
    // cursor restarts at zero instead of resuming mid-listing.
    let changed = rows.replacen("1.0.0", "2.0.0", 1);
    let restarted = adapter
        .admit_page(
            changed.as_bytes(),
            FeedRequest {
                cursor: resumed_cursor,
                max_items: 2,
            },
        )
        .expect("changed snapshot");
    assert!(restarted.packages[0].coordinate.as_str().contains("1.0.1"));
    // An offset past the listing is a typed protocol rejection.
    let mut forged = [0_u8; 32];
    forged[..24].copy_from_slice(&first.next_token[..24]);
    forged[24..].copy_from_slice(&u64::from(99u8).to_be_bytes());
    let forged_cursor = FeedCursor::from_parts(endpoint.id(), resumed_cursor.sequence(), forged);
    assert!(matches!(
        adapter.admit_page(
            rows.as_bytes(),
            FeedRequest {
                cursor: forged_cursor,
                max_items: 2,
            },
        ),
        Err(TransportFailure::Protocol)
    ));
}

#[test]
fn sparse_cargo_duplicate_and_checksum_grammar_are_typed() {
    let origin = "https://registry.example.test";
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, origin).expect("endpoint");
    let adapter = EcosystemAdapter::new(endpoint, PackageName::new("demo").expect("package"), None)
        .expect("adapter");
    let digest = "00".repeat(32);
    let duplicate = format!(
        "{{\"name\":\"demo\",\"vers\":\"1.0.0\",\"cksum\":\"{digest}\"}}\n{{\"name\":\"demo\",\"vers\":\"1.0.0\",\"cksum\":\"{digest}\"}}\n"
    );
    assert!(matches!(
        adapter.decode(duplicate.as_bytes()),
        Err(TransportFailure::Protocol)
    ));
    // Shuffled rows admit the same canonical chunk.
    let ordered = ["1.0.0", "1.0.1", "2.0.0"]
        .iter()
        .map(|version| {
            format!("{{\"name\":\"demo\",\"vers\":\"{version}\",\"cksum\":\"{digest}\"}}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let shuffled = ["2.0.0", "1.0.0", "1.0.1"]
        .iter()
        .map(|version| {
            format!("{{\"name\":\"demo\",\"vers\":\"{version}\",\"cksum\":\"{digest}\"}}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let left = adapter.decode(ordered.as_bytes()).expect("ordered");
    let right = adapter.decode(shuffled.as_bytes()).expect("shuffled");
    assert_eq!(left, right);
    // Checksum grammar rejects whitespace exactly like the removed sparse parser.
    assert!(matches!(
        adapter.decode(
            format!("{{\"name\":\"demo\",\"vers\":\"1.0.0\",\"cksum\":\" {digest}\"}}").as_bytes()
        ),
        Err(TransportFailure::Protocol)
    ));
    assert!(matches!(
        adapter.decode(
            format!("{{\"name\":\"demo\",\"vers\":\"1.0.0\",\"cksum\":\"{digest} \"}}").as_bytes()
        ),
        Err(TransportFailure::Protocol)
    ));
    assert!(matches!(
        adapter.decode(b"{truncated"),
        Err(TransportFailure::Protocol)
    ));
    // Uppercase hex remains accepted and verifies the same bytes.
    let archive = b"case-insensitive checksum archive";
    let lower = digest_hex(Sha256::digest(archive).as_slice());
    let upper = lower.to_ascii_uppercase();
    let row = format!("{{\"name\":\"demo\",\"vers\":\"1.0.0\",\"cksum\":\"{upper}\"}}");
    let releases = adapter.decode(row.as_bytes()).expect("uppercase");
    assert_eq!(releases.len(), 1);
    assert!(releases[0].checksum.verifies(archive));
}

#[test]
fn sparse_cargo_yanked_rows_remain_visible_as_versioned_policy_facts() {
    let origin = "https://registry.example.test";
    let digest = "00".repeat(32);
    let body = format!(
        "{{\"name\":\"demo\",\"vers\":\"1.0.0\",\"cksum\":\"{digest}\",\"yanked\":false}}\n{{\"name\":\"demo\",\"vers\":\"1.0.1\",\"cksum\":\"{digest}\",\"yanked\":true}}\n"
    );
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, origin).expect("endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint.clone(),
        PackageName::new("demo").expect("package"),
        None,
    )
    .expect("adapter");
    let releases = adapter.decode(body.as_bytes()).expect("decode");
    assert_eq!(releases.len(), 2);
    assert!(releases[0].coordinate.as_str().contains("1.0.0"));
    assert_eq!(releases[0].facts.standing(), ReleaseStanding::Available);
    assert_eq!(releases[1].facts.standing(), ReleaseStanding::Yanked);
    let page = adapter
        .admit_page(
            body.as_bytes(),
            FeedRequest {
                cursor: FeedCursor::genesis(endpoint.id()),
                max_items: 4,
            },
        )
        .expect("page");
    assert_eq!(page.packages.len(), 2);
    assert_eq!(page.packages[1].facts.standing(), ReleaseStanding::Yanked);
}

#[test]
fn native_checksum_mismatch_never_advances_the_cursor() {
    // Hermetic mismatch through the shared owner:
    // advertise the wrong digest and prove the cursor never advances.
    let wrong_archive = b"wrong native bytes";
    let claimed = *CapabilityArtifactId::from_value(b"other bytes").as_bytes();
    let (endpoint_text, server) = live_fixture(wrong_archive, claimed);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("endpoint");
    let root = temporary("native-integrity");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let mut transport = HttpRegistryTransport::new(endpoint, None, limits()).expect("transport");
    assert!(matches!(
        owner.poll(&mut transport),
        Err(AcquisitionError::Transport(TransportFailure::Integrity))
    ));
    assert_eq!(owner.cursor().sequence(), 0);
    server.join().expect("server");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn receipt_catalog_overrun_is_measured_and_durable() {
    let archive_a = b"catalog overrun archive a";
    let archive_b = b"catalog overrun archive b";
    let digest_a = *CapabilityArtifactId::from_value(archive_a.as_slice()).as_bytes();
    let digest_b = *CapabilityArtifactId::from_value(archive_b.as_slice()).as_bytes();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    let endpoint_text = format!("http://{address}");
    let next = transport::hex(&[7; 32]);
    let provenance = transport::hex(&[9; 32]);
    let item_a = format!(
        "{{\"name\":\"demo\",\"version\":\"1.0.0\",\"archive\":\"{endpoint_text}/a\",\"blake3\":\"{}\",\"provenance\":\"{provenance}\"}}",
        transport::hex(&digest_a)
    );
    let item_b = format!(
        "{{\"name\":\"demo\",\"version\":\"2.0.0\",\"archive\":\"{endpoint_text}/b\",\"blake3\":\"{}\",\"provenance\":\"{provenance}\"}}",
        transport::hex(&digest_b)
    );
    let page =
        format!("{{\"schema\":1,\"next\":\"{next}\",\"items\":[{item_a},{item_b}]}}").into_bytes();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let _ = read_request_head(&mut stream, "catalog overrun request")
            .expect("read catalog overrun request headers");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            page.len()
        )
        .expect("headers");
        stream.write_all(&page).expect("body");
    });
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("endpoint");
    let root = temporary("catalog-overrun");
    let mut constrained = limits();
    constrained.max_catalog_items = 1;
    let (mut owner, _) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        constrained,
    )
    .expect("owner");
    let mut transport =
        HttpRegistryTransport::new(endpoint.clone(), None, constrained).expect("transport");
    match owner.poll(&mut transport) {
        Err(AcquisitionError::Overrun { measured, limit }) => {
            assert_eq!((measured, limit), (2, 1));
        }
        other => panic!("expected measured catalog overrun, got {other:?}"),
    }
    assert_eq!(owner.cursor().sequence(), 0);
    server.join().expect("server");
    drop(owner);
    drop(transport);
    let (_, recovery) =
        RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, constrained)
            .expect("recovery");
    assert_eq!(recovery.cursor.sequence(), 0);
    assert!(recovery.pending.is_some());
    assert!(recovery.last_receipt.is_none());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn conditional_304_is_a_typed_metadata_cache_hit() {
    // The single-stack transport never sends If-None-Match validators, so a
    // 304 has no conditional context and is a typed rejection. ETag resume is
    // intentionally unsupported rather than silently treated as no-change.
    let (endpoint_text, server) = one_response(304, "", Vec::new());
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("endpoint");
    let cursor = FeedCursor::genesis(endpoint.id());
    let mut transport = HttpRegistryTransport::new(endpoint, None, limits()).expect("transport");
    assert!(matches!(
        transport.fetch_page(FeedRequest {
            cursor,
            max_items: 1
        }),
        Ok(TransportResult::NotModified)
    ));
    server.join().expect("server");
}

#[test]
fn cross_authority_archive_is_a_configuration_rejection() {
    // Split-origin index/download authorities are intentionally unsupported:
    // one endpoint owns both metadata and archives, enforced by typed error.
    let origin = "https://registry.example.test";
    let sha512 = STANDARD.encode(Sha512::digest(b"archive"));
    let body = format!(
        "{{\"name\":\"demo\",\"versions\":{{\"1.2.3\":{{\"dist\":{{\"integrity\":\"sha512-{sha512}\",\"tarball\":\"https://other.example.test/demo.tgz\"}}}}}}}}"
    );
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Npm, origin).expect("endpoint");
    let adapter = EcosystemAdapter::new(endpoint, PackageName::new("demo").expect("package"), None)
        .expect("adapter");
    assert!(matches!(
        adapter.decode(body.as_bytes()),
        Err(TransportFailure::Configuration)
    ));
}

#[test]
fn crates_io_sparse_index_resolves_the_official_static_archive_origin() {
    let archive = b"crate";
    let digest = digest_hex(Sha256::digest(archive).as_slice());
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://index.crates.io")
        .expect("official sparse endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint,
        PackageName::new("serde").expect("crate name"),
        None,
    )
    .expect("cargo adapter");
    let row = format!(
        "{{\"name\":\"serde\",\"vers\":\"1.0.0\",\"cksum\":\"{digest}\",\"yanked\":false}}\n"
    );
    let releases = adapter.decode(row.as_bytes()).expect("official row");
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].archive_url,
        "https://static.crates.io/crates/serde/serde-1.0.0.crate"
    );
}

#[test]
fn namespaced_npm_and_go_adapters_preserve_the_native_identity() {
    let npm_endpoint = RegistryEndpoint::new(RegistryEcosystem::Npm, "https://registry.npmjs.org")
        .expect("npm endpoint");
    let npm = EcosystemAdapter::new(
        npm_endpoint,
        PackageName::new("node").expect("name"),
        Some(PackageName::new("@types").expect("scope")),
    )
    .expect("scoped npm adapter");
    assert_eq!(
        npm.metadata_url(),
        "https://registry.npmjs.org/@types%2Fnode"
    );

    let go_endpoint = RegistryEndpoint::new(RegistryEcosystem::Golang, "https://proxy.golang.org")
        .expect("go endpoint");
    let go = EcosystemAdapter::new(
        go_endpoint,
        PackageName::new("errors").expect("name"),
        Some(PackageName::new("github.com/pkg").expect("module namespace")),
    )
    .expect("go adapter");
    assert_eq!(
        go.metadata_url(),
        "https://proxy.golang.org/github.com/pkg/errors/@v/list"
    );
    assert_eq!(go.go_versions(b"v0.9.1\n").expect("go listing"), ["v0.9.1"]);

    let uppercase = EcosystemAdapter::new(
        RegistryEndpoint::new(RegistryEcosystem::Golang, "https://proxy.golang.org")
            .expect("endpoint"),
        PackageName::new("toml").expect("name"),
        Some(PackageName::new("github.com/BurntSushi").expect("namespace")),
    )
    .expect("uppercase module");
    assert_eq!(
        uppercase.metadata_url(),
        "https://proxy.golang.org/github.com/!burnt!sushi/toml/@v/list"
    );
}

#[test]
fn official_live_protocol_smoke_is_opt_in() {
    if env::var_os("NUDOX_REGISTRY_LIVE").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return;
    }

    let cases = [
        (
            RegistryEcosystem::Cargo,
            "https://index.crates.io",
            "serde",
            None,
        ),
        (
            RegistryEcosystem::Npm,
            "https://registry.npmjs.org",
            "lodash",
            None,
        ),
        (
            RegistryEcosystem::Pypi,
            "https://pypi.org",
            "requests",
            None,
        ),
        (
            RegistryEcosystem::Maven,
            "https://repo1.maven.org",
            "commons-lang3",
            Some("org.apache.commons"),
        ),
        (
            RegistryEcosystem::Nuget,
            "https://api.nuget.org",
            "newtonsoft.json",
            None,
        ),
        (
            RegistryEcosystem::Golang,
            "https://proxy.golang.org",
            "errors",
            Some("github.com/pkg"),
        ),
        (
            RegistryEcosystem::Cpp,
            "https://center2.conan.io",
            "zlib",
            Some("1.3.1"),
        ),
    ];
    for (ecosystem, endpoint, package, namespace) in cases {
        let endpoint = RegistryEndpoint::new(ecosystem, endpoint).expect("official endpoint");
        let adapter = EcosystemAdapter::new(
            endpoint.clone(),
            PackageName::new(package).expect("package"),
            namespace
                .map(PackageName::new)
                .transpose()
                .expect("namespace"),
        )
        .expect("adapter");
        let mut transport =
            HttpRegistryTransport::for_native(adapter, None, AcquisitionLimits::default())
                .expect("native transport");
        let request = FeedRequest {
            cursor: FeedCursor::genesis(endpoint.id()),
            max_items: 1,
        };
        let result = transport.fetch_page(request);
        if !matches!(
            result,
            Ok(TransportResult::Available(FeedPage { ref packages, .. })) if !packages.is_empty()
        ) {
            panic!("live protocol failed for {ecosystem:?} {package}: {result:?}");
        }
    }
}

#[test]
fn nuget_metadata_and_archive_resolution_coalesce_one_content_fetch() {
    let archive = b"captured nupkg bytes".to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind NuGet fixture");
    let address = listener.local_addr().expect("NuGet fixture address");
    let endpoint_text = format!("http://{address}");
    let registration_url = format!("{endpoint_text}/registration/demo/index.json");
    let package_base = format!("{endpoint_text}/v3-flatcontainer/");
    let archive_url = format!("{package_base}demo/1.2.3/demo.1.2.3.nupkg");
    let service_index = format!(
        "{{\"resources\":[{{\"@id\":\"{registration_url}\",\"@type\":\"RegistrationsBaseUrl/3.6.0\"}},{{\"@id\":\"{package_base}\",\"@type\":\"PackageBaseAddress/3.0.0\"}}]}}"
    )
    .into_bytes();
    let registration = format!(
        "{{\"items\":[{{\"catalogEntry\":{{\"version\":\"1.2.3\",\"listed\":true}},\"packageContent\":\"{archive_url}\",\"packageHash\":null}}]}}"
    )
    .into_bytes();
    let expected_bodies = [service_index, registration, archive.clone()];
    let server = thread::spawn(move || {
        for body in expected_bodies {
            let (mut stream, _) = listener.accept().expect("accept NuGet request");
            let request = read_request_head(&mut stream, "NuGet archive fixture request")
                .expect("read NuGet archive request headers");
            assert!(!request.is_empty());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write NuGet headers");
            stream.write_all(&body).expect("write NuGet body");
        }
    });
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Nuget, endpoint_text)
        .expect("loopback NuGet endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint.clone(),
        PackageName::new("demo").expect("package"),
        None,
    )
    .expect("NuGet adapter");
    let mut transport =
        HttpRegistryTransport::for_native(adapter, None, limits()).expect("NuGet transport");
    let page = match transport
        .fetch_page(FeedRequest {
            cursor: FeedCursor::genesis(endpoint.id()),
            max_items: 1,
        })
        .expect("NuGet page")
    {
        TransportResult::Available(page) => page,
        other => panic!("unexpected NuGet page result: {other:?}"),
    };
    let package = page.packages.first().expect("NuGet release").clone();
    let fetched = match transport.fetch_archive(&package).expect("NuGet archive") {
        TransportResult::Available(artifact) => artifact.into_bytes(limits().max_archive_bytes),
        other => panic!("unexpected NuGet archive result: {other:?}"),
    }
    .expect("read NuGet artifact");
    assert_eq!(fetched, archive);
    server.join().expect("NuGet fixture server");
}

#[test]
fn conan_v2_recipe_revision_and_export_archive_are_admitted() {
    let archive = b"captured conan export archive".to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind Conan fixture");
    let address = listener.local_addr().expect("Conan fixture address");
    let endpoint_text = format!("http://{address}");
    let revision = br#"{"reference":"zlib/1.3.1@_/_","revisions":[{"revision":"abc123"}]}"#;
    let files = br#"{"files":{"conan_export.tgz":{}}}"#;
    let expected_bodies = [revision.to_vec(), files.to_vec(), archive.clone()];
    let server = thread::spawn(move || {
        for body in expected_bodies {
            let (mut stream, _) = listener.accept().expect("accept Conan request");
            let request = read_request_head(&mut stream, "Conan fixture request")
                .expect("read Conan request headers");
            assert!(!request.is_empty());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write Conan headers");
            stream.write_all(&body).expect("write Conan body");
        }
    });
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cpp, endpoint_text)
        .expect("loopback Conan endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint.clone(),
        PackageName::new("zlib").expect("package"),
        Some(PackageName::new("1.3.1").expect("version")),
    )
    .expect("Conan adapter");
    let mut transport =
        HttpRegistryTransport::for_native(adapter, None, limits()).expect("Conan transport");
    let page = match transport
        .fetch_page(FeedRequest {
            cursor: FeedCursor::genesis(endpoint.id()),
            max_items: 1,
        })
        .expect("Conan page")
    {
        TransportResult::Available(page) => page,
        other => panic!("unexpected Conan page result: {other:?}"),
    };
    let package = page.packages.first().expect("Conan release").clone();
    let fetched = match transport.fetch_archive(&package).expect("Conan archive") {
        TransportResult::Available(artifact) => artifact.into_bytes(limits().max_archive_bytes),
        other => panic!("unexpected Conan archive result: {other:?}"),
    }
    .expect("read Conan artifact");
    assert_eq!(fetched, archive);
    server.join().expect("Conan fixture server");
}

#[test]
fn read_artifact_unknown_is_none_and_tamper_is_corruption() {
    struct BridgeTransport {
        package: RemotePackage,
        archive: Vec<u8>,
    }

    impl RegistryTransport for BridgeTransport {
        fn fetch_page(
            &mut self,
            request: FeedRequest,
        ) -> Result<TransportResult<FeedPage>, TransportFailure> {
            Ok(TransportResult::Available(FeedPage {
                base: request.cursor,
                next_token: [7; 32],
                packages: vec![self.package.clone()],
            }))
        }

        fn fetch_archive(
            &mut self,
            _package: &RemotePackage,
        ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
            Ok(TransportResult::Available(ArchiveArtifact::from_bytes(
                self.archive.clone(),
            )))
        }
    }

    let archive = b"bridged archive bytes";
    let digest = *CapabilityArtifactId::from_value(archive.as_slice()).as_bytes();
    let endpoint_text = "http://127.0.0.1:9/bridge";
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("endpoint");
    let root = temporary("bridge");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let package = RemotePackage {
        coordinate: PackageCoordinate::parse("pkg:cargo/demo@1.2.3").expect("coordinate"),
        integrity: transport::ArchiveIntegrity::Canonical(digest),
        provenance: ProvenanceDigest::from_authenticated_feed([9; 32]),
        facts: test_facts(),
        native_metadata: test_native_metadata(),
        advisory: None,
        dependency_facts: unavailable_dependency_facts(),
        archive_url: Arc::from("http://127.0.0.1:9/bridge/archive"),
    };
    let mut transport = BridgeTransport {
        package,
        archive: archive.to_vec(),
    };
    let AcquisitionOutcome::Published(receipt) = owner.poll(&mut transport).expect("poll") else {
        panic!("expected publication")
    };
    // The owner catalog is the materialization bridge: unknown coordinates
    // resolve to a typed None, never a fabricated object.
    let unknown = PackageCoordinate::parse("pkg:cargo/demo@9.9.9").expect("coordinate");
    assert!(owner.published(&unknown).is_none());
    assert!(owner.read_artifact(&unknown).expect("read").is_none());
    assert_eq!(owner.published_packages().len(), 1);
    // Tampering with the content-addressed object is corruption, not a new version.
    let raw = receipt.packages[0].raw_object.to_bytes();
    let encoded = transport::hex(&raw);
    let object_path = storage_root(&root, &endpoint)
        .join("registry-content")
        .join("objects")
        .join(&encoded[..2])
        .join(&encoded[2..]);
    let mut tampered = archive.to_vec();
    tampered[0] ^= 0xff;
    fs::write(&object_path, &tampered).expect("tamper");
    assert!(matches!(
        owner.read_artifact(&receipt.packages[0].coordinate),
        Err(AcquisitionError::CorruptJournal)
    ));
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn legacy_unscoped_journal_path_is_never_aliased() {
    // Cache-layout compatibility: the pre-scoped `root/registry.journal`
    // path is ignored; each endpoint owns `root/<eco>/<id>/`.
    let root = temporary("legacy-layout");
    fs::create_dir_all(&root).expect("root");
    fs::write(root.join("registry.journal"), b"legacy").expect("legacy file");
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://registry.example.test")
        .expect("endpoint");
    let (owner, recovery) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Offline,
        limits(),
    )
    .expect("owner");
    assert_eq!(recovery.cursor.sequence(), 0);
    assert!(recovery.pending.is_none());
    assert!(
        storage_root(&root, &endpoint)
            .join("registry.journal")
            .is_file()
    );
    assert_eq!(owner.cursor().sequence(), 0);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn cargo_sparse_rows_retain_features_and_reject_typed_policy_shapes() {
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://index.crates.io")
        .expect("Cargo endpoint");
    let adapter = EcosystemAdapter::new(endpoint, PackageName::new("demo").expect("package"), None)
        .expect("adapter");
    let row = br#"{"name":"demo","vers":"1.2.3","deps":[{"name":"serde","req":"^1","kind":"normal","optional":true,"features":["derive"]}],"cksum":"0000000000000000000000000000000000000000000000000000000000000000","features":{"default":["std","dep:serde"],"std":[]},"yanked":true,"links":"demo-sys"}
"#;
    let releases = adapter.decode(row).expect("Cargo sparse row");
    assert_eq!(releases.len(), 1);
    let release = &releases[0];
    assert_eq!(release.facts.standing(), ReleaseStanding::Yanked);
    assert_eq!(release.features().len(), 2);
    assert_eq!(release.features()[0].name(), "default");
    assert_eq!(
        release.features()[0].members(),
        [Arc::from("dep:serde"), Arc::from("std")]
    );
    assert_eq!(release.artifacts().len(), 1);
    assert_eq!(
        release.artifacts()[0].kind(),
        NativeArtifactKind::CargoCrate
    );
    let malformed = br#"{"name":"demo","vers":"1.2.3","cksum":"0000000000000000000000000000000000000000000000000000000000000000","yanked":"true"}"#;
    assert!(matches!(
        adapter.decode(malformed),
        Err(TransportFailure::Protocol)
    ));
}

#[test]
fn npm_packument_retains_scoped_tags_deprecation_and_relative_integrity() {
    let archive = b"npm package archive";
    let sha512 = STANDARD.encode(Sha512::digest(archive));
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Npm, "https://registry.example.test")
        .expect("npm endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint,
        PackageName::new("parser").expect("name"),
        Some(PackageName::new("@babel").expect("scope")),
    )
    .expect("adapter");
    let body = format!(
        r#"{{"name":"@babel/parser","dist-tags":{{"latest":"1.2.3","next":"1.3.0"}},"versions":{{"1.2.3":{{"name":"@babel/parser","version":"1.2.3","deprecated":"use 1.3.0","dist":{{"integrity":"sha512-{sha512}","tarball":"/-/parser-1.2.3.tgz","size":18}},"dependencies":{{"acorn":"^8"}}}},"1.3.0":{{"name":"@babel/parser","version":"1.3.0","dist":{{"shasum":"{}","tarball":"/-/parser-1.3.0.tgz"}}}}}}}}"#,
        digest_hex(sha1::Sha1::digest(archive).as_slice())
    );
    let releases = adapter.decode(body.as_bytes()).expect("npm packument");
    assert_eq!(releases.len(), 2);
    assert_eq!(releases[0].dist_tags().len(), 2);
    assert_eq!(releases[0].dist_tags()[0].name(), "latest");
    let deprecated = &releases[0];
    assert_eq!(deprecated.facts.standing(), ReleaseStanding::Deprecated);
    assert_eq!(deprecated.standing_reason(), Some("use 1.3.0"));
    assert_eq!(deprecated.artifacts().len(), 1);
    assert!(matches!(
        &deprecated.native_metadata.details,
        backend_library::RegistryNativeDetails::Npm(metadata)
            if metadata.dist_tags.len() == 2
                && metadata.deprecation.as_deref() == Some("use 1.3.0")
                && metadata.artifacts[0].yanked.is_none()
    ));
    assert_eq!(
        deprecated.artifacts()[0].url(),
        "https://registry.example.test/-/parser-1.2.3.tgz"
    );
    assert!(deprecated.artifacts()[0].checksum().verifies(archive));
    assert_eq!(
        releases[1].artifacts()[0].checksum().algorithm(),
        ChecksumAlgorithm::Sha1
    );
}

#[test]
fn pypi_simple_retains_every_file_kind_requires_python_and_yank_reason() {
    let sdist = b"python source distribution";
    let wheel = b"python wheel distribution";
    let signature = b"python signature";
    let sdist_hash = digest_hex(Sha256::digest(sdist).as_slice());
    let wheel_hash = digest_hex(Sha256::digest(wheel).as_slice());
    let signature_hash = digest_hex(Sha256::digest(signature).as_slice());
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Pypi, "https://mirror.example.test")
        .expect("PyPI endpoint");
    let adapter = EcosystemAdapter::new(
        endpoint,
        PackageName::new("Demo_Pkg").expect("package"),
        None,
    )
    .expect("adapter");
    let body = format!(
        r#"{{"meta":{{"api-version":"1.4"}},"name":"demo-pkg","files":[{{"filename":"demo_pkg-1.2.3.tar.gz","url":"../../packages/demo_pkg-1.2.3.tar.gz","hashes":{{"sha256":"{sdist_hash}"}},"requires-python":">=3.8","size":24,"yanked":false}},{{"filename":"demo_pkg-1.2.3-py3-none-any.whl","url":"../../packages/demo_pkg-1.2.3-py3-none-any.whl","hashes":{{"sha256":"{wheel_hash}"}},"requires-python":">=3.9","size":25,"yanked":"superseded"}},{{"filename":"demo_pkg-1.2.3.tar.gz.asc","url":"../../packages/demo_pkg-1.2.3.tar.gz.asc","hashes":{{"sha256":"{signature_hash}"}}}}]}}"#
    );
    let releases = adapter
        .decode(body.as_bytes())
        .expect("PyPI simple response");
    assert_eq!(releases.len(), 1);
    let release = &releases[0];
    assert_eq!(release.artifacts().len(), 3);
    assert_eq!(
        release.artifacts()[0].kind(),
        NativeArtifactKind::PythonWheel
    );
    assert_eq!(
        release.artifacts()[1].kind(),
        NativeArtifactKind::PythonSdist
    );
    assert_eq!(
        release.artifacts()[2].kind(),
        NativeArtifactKind::PythonSignature
    );
    assert_eq!(release.artifacts()[0].requires_python(), Some(">=3.9"));
    assert_eq!(release.requires_python(), Some(">=3.8"));
    assert_eq!(release.facts.standing(), ReleaseStanding::Available);
    assert_eq!(release.artifacts()[0].yanked_reason(), Some("superseded"));
    assert!(matches!(
        &release.native_metadata.details,
        backend_library::RegistryNativeDetails::Pypi(metadata)
            if metadata.artifacts.len() == 3
                && metadata.requires_python.as_deref() == Some(">=3.8")
                && metadata.artifacts[0].yanked_reason.as_deref() == Some("superseded")
    ));
    assert!(
        release.artifacts()[1]
            .url()
            .ends_with("/packages/demo_pkg-1.2.3.tar.gz")
    );
    assert!(release.artifacts()[1].checksum().verifies(sdist));

    let all_yanked = body.replace("\"yanked\":false", "\"yanked\":\"withdrawn\"");
    let releases = adapter
        .decode(all_yanked.as_bytes())
        .expect("withdrawn release");
    assert_eq!(releases[0].facts.standing(), ReleaseStanding::Yanked);
    assert_eq!(releases[0].standing_reason(), Some("withdrawn"));
}

#[test]
fn native_adapters_keep_empty_deltas_and_reject_malformed_or_unsafe_links() {
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Pypi, "https://pypi.org").expect("PyPI endpoint");
    let adapter = EcosystemAdapter::new(endpoint, PackageName::new("demo").expect("package"), None)
        .expect("adapter");
    assert!(
        adapter
            .decode(br#"{"meta":{"api-version":"1.4"},"files":[]}"#)
            .expect("empty withdrawal delta")
            .is_empty()
    );
    assert!(matches!(
        adapter.decode(br#"{"meta":{"api-version":"1.4"},"files":[{"filename":"demo-1.0.0.tar.gz","url":"https://pypi.org/demo.tar.gz","hashes":{}}]}"#),
        Err(TransportFailure::Protocol)
    ));
    assert!(matches!(
        resolve_archive_url(
            "https://pypi.org/simple/demo/",
            "https://user:pass@evil.test/demo.tgz"
        ),
        Err(TransportFailure::Protocol)
    ));
    assert_eq!(
        resolve_archive_url("https://pypi.org/simple/demo/", "../../packages/demo.tgz")
            .expect("relative URL"),
        "https://pypi.org/packages/demo.tgz"
    );
}

fn digest_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    output
}
