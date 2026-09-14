use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use super::*;
use crate::{
    capability::CapabilityArtifactId,
    fault::{Boundary, Faults},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha512};

static TEMPORARY: AtomicU64 = AtomicU64::new(0);

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
        ) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
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
        ) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
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
fn endpoint_policy_rejects_plaintext_remote_hosts_and_redirects() {
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
fn all_native_ecosystem_grammars_normalize_and_verify_archives() {
    let archive = b"native registry archive";
    let sha256 = digest_hex(Sha256::digest(archive).as_slice());
    let sha512 = STANDARD.encode(Sha512::digest(archive));
    let origin = "https://registry.example.test";
    let fixtures = [
        (
            RegistryEcosystem::Cargo,
            None,
            format!("{{\"name\":\"demo\",\"vers\":\"1.2.3\",\"cksum\":\"{sha256}\"}}"),
        ),
        (
            RegistryEcosystem::Npm,
            None,
            format!(
                "{{\"versions\":{{\"1.2.3\":{{\"dist\":{{\"integrity\":\"sha512-{sha512}\",\"tarball\":\"{origin}/demo/-/demo-1.2.3.tgz\"}}}}}}}}"
            ),
        ),
        (
            RegistryEcosystem::Pypi,
            None,
            format!(
                "{{\"releases\":{{\"1.2.3\":[{{\"packagetype\":\"sdist\",\"url\":\"{origin}/files/demo-1.2.3.tar.gz\",\"digests\":{{\"sha256\":\"{sha256}\"}}}}]}}}}"
            ),
        ),
        (
            RegistryEcosystem::Maven,
            Some("org.example"),
            format!(
                "<metadata><release version=\"1.2.3\" sha256=\"{sha256}\" url=\"{origin}/org/example/demo/1.2.3/demo-1.2.3.jar\"/></metadata>"
            ),
        ),
        (
            RegistryEcosystem::Nuget,
            None,
            format!(
                "{{\"items\":[{{\"catalogEntry\":{{\"version\":\"1.2.3\"}},\"packageContent\":\"{origin}/demo/1.2.3/demo.1.2.3.nupkg\",\"packageHash\":\"{sha512}\"}}]}}"
            ),
        ),
        (
            RegistryEcosystem::Golang,
            None,
            format!("v1.2.3 {sha256}\n"),
        ),
        (
            RegistryEcosystem::Cpp,
            Some("stable/example"),
            format!(
                "{{\"results\":[{{\"version\":\"1.2.3\",\"download_url\":\"{origin}/v2/conans/demo/stable/example/download\",\"sha256\":\"{sha256}\"}}]}}"
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
        ) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
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
        ) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
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

fn digest_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    output
}
