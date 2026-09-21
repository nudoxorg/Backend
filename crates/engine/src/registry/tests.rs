use std::{
    env, fs,
    io::{Read, Seek, SeekFrom, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use super::*;
use crate::{
    capability::CapabilityArtifactId,
    fault::{Boundary, Faults},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha512};

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

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
        facts: ReleaseFacts::default(),
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
    let request = crate::acquisition::AcquisitionRequest::new(
        service.source_id(),
        package.coordinate.to_string(),
        crate::acquisition::RawArchiveObjectId::from_bytes(&[]),
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
    let request = crate::acquisition::AcquisitionRequest::new(
        service.source_id(),
        coordinate.to_string(),
        crate::acquisition::RawArchiveObjectId::from_bytes(&[]),
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
    let request = crate::acquisition::AcquisitionRequest::new(
        circuit.source_id(),
        coordinate.to_string(),
        crate::acquisition::RawArchiveObjectId::from_bytes(&[]),
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
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).expect("read request");
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture registry");
    let address = listener.local_addr().expect("fixture address");
    let headers = extra_headers.to_owned();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept registry request");
        let mut request = [0_u8; 8192];
        let _ = stream.read(&mut request).expect("read request");
        write!(
            stream,
            "HTTP/1.1 {status} TEST\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
            body.len()
        )
        .expect("write headers");
        stream.write_all(&body).expect("write body");
    });
    (format!("http://{address}"), handle)
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
        facts: ReleaseFacts::default(),
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
                    facts: ReleaseFacts::new(
                        standing,
                        DownloadCount::NotReported(DownloadCountGap::Unsupported),
                        SecurityStanding::Unassessed,
                    ),
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
        RegistryOwner::open(&root, endpoint, AcquisitionPolicy::Online, limits()).expect("owner");
    let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
    let mut transport = PolicyDelta {
        archive: b"one immutable archive".to_vec(),
        page: 0,
        archive_fetches: 0,
    };
    let _ = owner.poll(&mut transport).expect("initial release");
    let first = owner.published(&coordinate).expect("published").artifact;
    let _ = owner.poll(&mut transport).expect("yank delta");
    let current = owner.published(&coordinate).expect("updated");
    assert_eq!(transport.archive_fetches, 1);
    assert_eq!(current.artifact, first);
    assert_eq!(current.facts.standing(), ReleaseStanding::Yanked);
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
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).expect("read native request");
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
        assert!(layout.join("registry-objects").is_dir());
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
    assert!(cargo_root.join("registry-objects").is_dir());
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
    let (endpoint_text, server) = live_fixture(&archive, [0_u8; 32]);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("endpoint");
    let root = temporary("archive-overrun");
    let mut constrained = limits();
    constrained.max_archive_bytes = 256;
    constrained.max_page_archive_bytes = 256;
    let (mut owner, _) = RegistryOwner::open(
        &root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        constrained,
    )
    .expect("owner");
    let mut transport = HttpRegistryTransport::new(endpoint, None, constrained).expect("transport");
    match owner.poll(&mut transport) {
        Err(AcquisitionError::Transport(TransportFailure::Overrun { measured, limit })) => {
            assert_eq!(limit, 256);
            assert!(measured > limit, "measured extent must exceed the cap");
        }
        other => panic!("expected measured typed overrun, got {other:?}"),
    }
    assert_eq!(owner.cursor().sequence(), 0, "no cursor advance on overrun");
    server.join().expect("server");
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
        for body in [page, archive_a.to_vec(), archive_b.to_vec()] {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).expect("read");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("headers");
            stream.write_all(&body).expect("body");
        }
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
            let mut request = [0_u8; 8192];
            let size = stream.read(&mut request).expect("read NuGet request");
            assert!(size > 0);
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
            let mut request = [0_u8; 8192];
            assert!(stream.read(&mut request).expect("read Conan request") > 0);
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
    let archive = b"bridged archive bytes";
    let digest = *CapabilityArtifactId::from_value(archive.as_slice()).as_bytes();
    let (endpoint_text, server) = live_fixture(archive, digest);
    let endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_text).expect("endpoint");
    let root = temporary("bridge");
    let (mut owner, _) =
        RegistryOwner::open(&root, endpoint.clone(), AcquisitionPolicy::Online, limits())
            .expect("owner");
    let mut transport =
        HttpRegistryTransport::new(endpoint.clone(), None, limits()).expect("transport");
    let AcquisitionOutcome::Published(receipt) = owner.poll(&mut transport).expect("poll") else {
        panic!("expected publication")
    };
    // The owner catalog is the materialization bridge: unknown coordinates
    // resolve to a typed None, never a fabricated object.
    let unknown = PackageCoordinate::parse("pkg:cargo/demo@9.9.9").expect("coordinate");
    assert!(owner.published(&unknown).is_none());
    assert!(owner.read_artifact(&unknown).expect("read").is_none());
    assert_eq!(owner.published_packages().len(), 1);
    server.join().expect("server");
    // Tampering with the content-addressed object is corruption, not a new version.
    let object_path = storage_root(&root, &endpoint)
        .join("registry-objects")
        .join(transport::hex(&digest));
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

fn digest_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    output
}
