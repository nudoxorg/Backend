use super::*;
use backend_library::{RegistryForgeAssociationState, RegistryForgeBlobKind};

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

fn sha(value: &str) -> ForgeObjectId {
    ForgeObjectId::parse(value).expect("hash")
}

fn coordinate() -> ForgeCoordinate {
    ForgeCoordinate::new(
        "https://github.com/acme/mono.git",
        ForgeRevision::Tag(ForgeRefName::new("v1.2.3").expect("tag")),
        Some("crates/widget"),
    )
    .expect("coordinate")
}

struct Fixture {
    calls: AtomicUsize,
    archive: Vec<u8>,
}

impl ForgeTransport for Fixture {
    fn resolve(
        &mut self,
        coordinate: &ForgeCoordinate,
    ) -> Result<ForgeResolution, ForgeTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        ForgeResolution::for_coordinate(
            coordinate,
            sha("0123456789012345678901234567890123456789"),
            Some(sha("abcdefabcdefabcdefabcdefabcdefabcdefabcd")),
            "0123456789012345678901234567890123456789",
        )
        .map_err(|_| ForgeTransportError::Integrity)
    }
    fn fetch_archive(
        &mut self,
        _: &ForgeCoordinate,
        _: &ForgeResolution,
    ) -> Result<ForgeArchive, ForgeTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(ForgeArchive::tar(self.archive.clone(), None::<String>))
    }
    fn fetch_metadata(
        &mut self,
        coordinate: &ForgeCoordinate,
        _: &ForgeResolution,
    ) -> Result<ForgeRepositoryMetadata, ForgeTransportError> {
        Ok(ForgeRepositoryMetadata::unavailable(
            coordinate.owner(),
            ForgeUnavailableReason::AuthorityOmitted,
        ))
    }
}

fn tar_one(path: &str, bytes: &[u8]) -> Vec<u8> {
    let mut header = [0_u8; 512];
    header[..path.len()].copy_from_slice(path.as_bytes());
    header[100..108].copy_from_slice(b"0000644\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", bytes.len()).as_bytes());
    header[156] = b'0';
    header[148..156].fill(b' ');
    let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    header[148..156].copy_from_slice(format!("{:06o}\0 ", checksum).as_bytes());
    let mut result = header.to_vec();
    result.extend_from_slice(bytes);
    result.resize(result.len() + ((512 - (bytes.len() % 512)) % 512) + 1024, 0);
    result
}

fn serve_loopback_request(mut stream: TcpStream, root: &Path) {
    // On macOS and the BSDs an accepted socket inherits the listener's
    // non-blocking mode, so a first read can fail with `WouldBlock` before the
    // request arrives. Serve each connection in blocking mode under a bound
    // and read through the end of the request headers.
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .is_err()
    {
        return;
    }
    let mut request = [0_u8; 4096];
    let mut bytes = 0;
    while !request[..bytes]
        .windows(4)
        .any(|window| window == b"\r\n\r\n")
    {
        match stream.read(&mut request[bytes..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => bytes += read,
        }
        if bytes == request.len() {
            break;
        }
    }
    if bytes == 0 {
        return;
    }
    let request = String::from_utf8_lossy(&request[..bytes]);
    let Some(path) = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
    else {
        return;
    };
    let path = path.split('?').next().unwrap_or_default();
    let path = path.trim_start_matches('/');
    if path.is_empty() || path.split('/').any(|part| part == ".." || part.is_empty()) {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let file = root.join(path);
    let Ok(body) = fs::read(file) else {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}

fn start_loopback_http(
    root: &Path,
) -> (
    std::net::SocketAddr,
    Arc<AtomicBool>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    listener.set_nonblocking(true).expect("nonblocking");
    let address = listener.local_addr().expect("address");
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_for_thread = Arc::clone(&stopped);
    let root = Arc::new(root.to_path_buf());
    let thread = thread::spawn(move || {
        while !stopped_for_thread.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let root = Arc::clone(&root);
                    thread::spawn(move || serve_loopback_request(stream, &root));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::yield_now();
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                    thread::yield_now();
                }
                Err(_) => thread::yield_now(),
            }
        }
    });
    (address, stopped, thread)
}

#[test]
fn coordinate_is_canonical_and_credential_free() {
    let coordinate =
        ForgeCoordinate::parse("https://GitHub.com/acme/mono.git@tag:v1#crates/widget")
            .expect("coordinate");
    assert_eq!(coordinate.repository_url(), "https://github.com/acme/mono");
    assert_eq!(coordinate.provider(), ForgeProvider::Github);
    assert_eq!(coordinate.subdir(), Some("crates/widget"));
    assert!(!coordinate.canonical().contains("token"));
    assert!(ForgeCoordinate::parse("https://u:p@github.com/acme/mono@tag:v1").is_err());
    assert!(ForgeCoordinate::parse("https://github.com/acme/mono@v1").is_err());
    assert!(ForgeCoordinate::parse("https://github.com/acme/mono@branch:main?x=1").is_err());
}

#[test]
fn every_supported_provider_has_one_canonical_coordinate_shape() {
    for (url, provider) in [
        ("https://github.com/acme/mono.git", ForgeProvider::Github),
        ("https://gitlab.com/acme/mono.git", ForgeProvider::Gitlab),
        (
            "https://codeberg.org/acme/mono.git",
            ForgeProvider::Codeberg,
        ),
        (
            "https://git.example.test/acme/mono.git",
            ForgeProvider::GenericHttpsGit,
        ),
    ] {
        let coordinate = ForgeCoordinate::new(
            url,
            ForgeRevision::Branch(ForgeRefName::new("main").expect("ref")),
            None::<String>,
        )
        .expect("coordinate");
        assert_eq!(coordinate.provider(), provider);
        assert_eq!(coordinate.repository_url(), url.trim_end_matches(".git"));
        assert!(coordinate.canonical().contains("@branch:main"));
    }
}

#[test]
fn immutable_commit_requests_cannot_be_rebound() {
    let coordinate = ForgeCoordinate::new(
        "https://github.com/acme/mono",
        ForgeRevision::Commit(sha("0123456789012345678901234567890123456789")),
        None::<String>,
    )
    .expect("coordinate");
    let error = ForgeResolution::new(
        coordinate.revision().clone(),
        sha("abcdefabcdefabcdefabcdefabcdefabcdefabcd"),
        None,
    )
    .expect_err("mismatch");
    assert_eq!(error, ForgeProtocolError::RevisionMismatch);
}

#[test]
fn mutable_resolution_requires_authority_validator_binding() {
    struct Unbound;
    impl ForgeTransport for Unbound {
        fn resolve(
            &mut self,
            coordinate: &ForgeCoordinate,
        ) -> Result<ForgeResolution, ForgeTransportError> {
            ForgeResolution::new(
                coordinate.revision().clone(),
                sha("0123456789012345678901234567890123456789"),
                None,
            )
            .map_err(|_| ForgeTransportError::Protocol)
        }

        fn fetch_archive(
            &mut self,
            _coordinate: &ForgeCoordinate,
            _: &ForgeResolution,
        ) -> Result<ForgeArchive, ForgeTransportError> {
            Ok(ForgeArchive::tar(Vec::new(), None::<String>))
        }
    }

    let root = std::env::temp_dir().join(format!("nudox-forge-binding-{}", now_millis()));
    let _ = fs::remove_dir_all(&root);
    let service = ForgeAcquisitionService::open(
        &root,
        ForgeAcquisitionPolicy::Online,
        ForgeAcquisitionLimits::default(),
    )
    .expect("service");
    assert_eq!(
        service.acquire(&coordinate(), &mut Unbound),
        ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::RevisionMismatch)
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn fixture_acquisition_reuses_content_and_restarts_offline() {
    let root = std::env::temp_dir().join(format!("nudox-forge-test-{}", now_millis()));
    let _ = fs::remove_dir_all(&root);
    let archive = tar_one(
        "crates/widget/Cargo.toml",
        b"[package]\nname=\"widget\"\nversion=\"1.0.0\"\n",
    );
    let mut fixture = Fixture {
        calls: AtomicUsize::new(0),
        archive,
    };
    let service = ForgeAcquisitionService::open(
        &root,
        ForgeAcquisitionPolicy::Online,
        ForgeAcquisitionLimits::default(),
    )
    .expect("service");
    let first = service.acquire(&coordinate(), &mut fixture);
    let ForgeAcquisitionOutcome::Hit(first) = first else {
        panic!("first acquisition")
    };
    assert_eq!(first.manifests.len(), 1);
    assert_eq!(first.manifests[0].path.as_ref(), "Cargo.toml");
    let product = first.product_record().expect("product projection");
    assert_eq!(product.manifests[0].path.as_str(), "Cargo.toml");
    assert_eq!(first.snapshot.id().to_bytes(), first.receipt.snapshot);
    assert_eq!(first.delta.id().to_bytes(), first.receipt.delta);
    let lineage = first
        .registry_association(
            PackageReference::parse("pkg:cargo/widget@1.0.0").expect("registry coordinate"),
        )
        .expect("registry lineage");
    assert!(lineage.is_resolved());
    assert_eq!(lineage.candidates.len(), 1);
    assert_eq!(lineage.candidates[0].source.coordinate, first.coordinate);
    let RegistryForgeAssociationState::Resolved { blobs, .. } = &lineage.state else {
        panic!("receipt lineage must resolve");
    };
    assert!(blobs.iter().any(|blob| {
        blob.kind == RegistryForgeBlobKind::Archive && blob.content_id == first.archive.to_bytes()
    }));
    assert!(blobs.iter().any(|blob| {
        blob.kind == RegistryForgeBlobKind::TreeManifest
            && blob.content_id == first.tree.id().to_bytes()
    }));
    assert!(blobs.iter().any(|blob| {
        blob.kind == RegistryForgeBlobKind::SourceSnapshot
            && blob.content_id == first.snapshot.id().to_bytes()
    }));
    assert!(blobs.iter().any(|blob| {
        blob.kind == RegistryForgeBlobKind::SourceDelta
            && blob.content_id == first.delta.id().to_bytes()
    }));
    let calls = fixture.calls.load(Ordering::Relaxed);
    let second = service.acquire(&coordinate(), &mut fixture);
    assert!(matches!(second, ForgeAcquisitionOutcome::Hit(_)));
    assert_eq!(fixture.calls.load(Ordering::Relaxed), calls);
    drop(service);
    let offline = ForgeAcquisitionService::open(
        &root,
        ForgeAcquisitionPolicy::Offline,
        ForgeAcquisitionLimits::default(),
    )
    .expect("reopen");
    let recovered = offline
        .reference(&coordinate())
        .expect("reference")
        .expect("recovered source");
    assert_eq!(recovered.snapshot.id(), first.snapshot.id());
    assert_eq!(recovered.delta.id(), first.delta.id());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn not_found_tombstone_survives_restart_without_becoming_identity() {
    struct Missing;
    impl ForgeTransport for Missing {
        fn resolve(&mut self, _: &ForgeCoordinate) -> Result<ForgeResolution, ForgeTransportError> {
            Err(ForgeTransportError::NotFound)
        }

        fn fetch_archive(
            &mut self,
            _: &ForgeCoordinate,
            _: &ForgeResolution,
        ) -> Result<ForgeArchive, ForgeTransportError> {
            Err(ForgeTransportError::NotFound)
        }
    }

    let root = std::env::temp_dir().join(format!("nudox-forge-tombstone-{}", now_millis()));
    let _ = fs::remove_dir_all(&root);
    let coordinate = coordinate();
    let mut missing = Missing;
    let service = ForgeAcquisitionService::open(
        &root,
        ForgeAcquisitionPolicy::Online,
        ForgeAcquisitionLimits::default(),
    )
    .expect("service");
    assert_eq!(
        service.acquire(&coordinate, &mut missing),
        ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::RevisionMismatch)
    );
    assert!(
        fs::metadata(root.join("forge.journal"))
            .expect("journal")
            .len()
            > 0
    );
    drop(service);
    let offline = ForgeAcquisitionService::open(
        &root,
        ForgeAcquisitionPolicy::Offline,
        ForgeAcquisitionLimits::default(),
    )
    .expect("restart");
    assert!(offline.reference(&coordinate).expect("reference").is_none());
    assert_eq!(
        offline.acquire(&coordinate, &mut missing),
        ForgeAcquisitionOutcome::Offline
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn process_transport_streams_exact_commit_from_loopback_http() {
    let root = std::env::temp_dir().join(format!(
        "nudox-forge-process-{}-{}",
        std::process::id(),
        now_millis()
    ));
    let _ = fs::remove_dir_all(&root);
    let work = root.join("work");
    let bare = root.join("server/acme/mono");
    fs::create_dir_all(&work).expect("work directory");
    let git = |directory: &Path, args: &[&str]| {
        let output = Command::new("git")
            .current_dir(directory)
            .args(args)
            .output()
            .expect("git process");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    git(&work, &["init", "--quiet"]);
    git(&work, &["config", "user.email", "forge@example.test"]);
    git(&work, &["config", "user.name", "Forge Fixture"]);
    fs::write(
        work.join("Cargo.toml"),
        b"[package]\nname=\"loopback\"\nversion=\"1.0.0\"\n",
    )
    .expect("manifest");
    git(&work, &["add", "Cargo.toml"]);
    git(&work, &["commit", "--quiet", "-m", "fixture"]);
    git(&work, &["branch", "-M", "main"]);
    fs::create_dir_all(bare.parent().expect("bare parent")).expect("server directory");
    let clone = Command::new("git")
        .args([
            "clone",
            "--bare",
            work.to_str().expect("work path"),
            bare.to_str().expect("bare path"),
        ])
        .output()
        .expect("bare clone");
    assert!(clone.status.success(), "bare clone failed");
    git(&bare, &["update-server-info"]);
    let commit = String::from_utf8(git(&work, &["rev-parse", "HEAD"]))
        .expect("commit text")
        .trim()
        .to_owned();
    let (address, stopped, server) = start_loopback_http(&root.join("server"));
    let coordinate = ForgeCoordinate::new(
        format!("http://{address}/acme/mono.git"),
        ForgeRevision::Branch(ForgeRefName::new("main").expect("branch")),
        None::<String>,
    )
    .expect("coordinate");
    let service = ForgeAcquisitionService::open(
        root.join("service"),
        ForgeAcquisitionPolicy::Online,
        ForgeAcquisitionLimits::default(),
    )
    .expect("service");
    let mut transport = GitCommandTransport::new(root.join("transport")).expect("transport");
    let outcome = service.acquire(&coordinate, &mut transport);
    let ForgeAcquisitionOutcome::Hit(result) = outcome else {
        panic!("loopback process acquisition failed: {outcome:?}");
    };
    assert_eq!(result.resolution.commit.as_hex(), commit);
    assert!(
        result
            .tree
            .entries()
            .iter()
            .any(|entry| entry.path.as_ref() == "Cargo.toml")
    );
    stopped.store(true, Ordering::Relaxed);
    let _ = TcpStream::connect(address);
    server.join().expect("server");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn delegated_exact_commit_is_verified_before_archive_admission() {
    let coordinate = ForgeCoordinate::new(
        "https://github.com/acme/mono",
        ForgeRevision::Commit(sha("0123456789012345678901234567890123456789")),
        None::<String>,
    )
    .expect("coordinate");
    let request = ForgeDelegationRequest::new(coordinate).expect("request");
    let response = ForgeDelegatedObject {
        commit: sha("abcdefabcdefabcdefabcdefabcdefabcdefabcd"),
        archive: ForgeArchive::tar(vec![0; 1024], None::<String>),
    };
    assert!(matches!(
        verify_delegated_object(&request, response),
        Err(ForgeProtocolError::DelegatedCommitMismatch)
    ));
}

#[test]
#[ignore = "real forge network smoke; set NUDOX_FORGE_REAL_SMOKE=1 and run --ignored"]
fn gated_real_github_smoke_uses_exact_resolved_commit() {
    if std::env::var("NUDOX_FORGE_REAL_SMOKE").as_deref() != Ok("1") {
        return;
    }
    let coordinate = ForgeCoordinate::new(
        "https://github.com/octocat/Hello-World",
        ForgeRevision::Branch(ForgeRefName::new("master").expect("ref")),
        None::<String>,
    )
    .expect("coordinate");
    let root = std::env::temp_dir().join(format!("nudox-forge-smoke-{}", now_millis()));
    let service = ForgeAcquisitionService::open(
        &root,
        ForgeAcquisitionPolicy::Online,
        ForgeAcquisitionLimits::default(),
    )
    .expect("service");
    let mut transport =
        HttpForgeTransport::new(ForgeAcquisitionLimits::default(), None).expect("transport");
    let result = service.acquire(&coordinate, &mut transport);
    assert!(matches!(result, ForgeAcquisitionOutcome::Hit(_)));
    let _ = fs::remove_dir_all(root);
}

fn directory_bytes(path: &Path) -> u64 {
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                pending.push(child);
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

fn git_quiet(directory: Option<&Path>, args: &[&str]) {
    let mut command = Command::new("git");
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    let output = command.args(args).output().expect("git process");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn serve_smart_git(
    mut stream: TcpStream,
    project_root: &Path,
    backend: &Path,
    response_bytes: &AtomicUsize,
) {
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
    let mut header = Vec::new();
    let mut buffer = [0_u8; 8192];
    while !header.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => header.extend_from_slice(&buffer[..read]),
        }
        if header.len() > 1024 * 1024 {
            return;
        }
    }
    let Some(split) = header.windows(4).position(|window| window == b"\r\n\r\n") else {
        return;
    };
    let head = String::from_utf8_lossy(&header[..split]).into_owned();
    let mut body = header[split + 4..].to_vec();
    let mut lines = head.lines();
    let Some(request) = lines.next() else {
        return;
    };
    let mut parts = request.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return;
    };
    let mut content_length = 0_usize;
    let mut content_type = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type = value.trim().to_owned();
        }
    }
    while body.len() < content_length {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => body.extend_from_slice(&buffer[..read]),
        }
        if body.len() > 2 * 1024 * 1024 {
            return;
        }
    }
    body.truncate(content_length);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let Ok(mut child) = Command::new(backend)
        .env("GIT_PROJECT_ROOT", project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", method)
        .env("QUERY_STRING", query)
        .env("PATH_INFO", path)
        .env("CONTENT_TYPE", &content_type)
        .env("CONTENT_LENGTH", content_length.to_string())
        .env("REMOTE_ADDR", "127.0.0.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&body);
    }
    let Ok(output) = child.wait_with_output() else {
        return;
    };
    let bytes = output.stdout;
    let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return;
    };
    let raw_headers = String::from_utf8_lossy(&bytes[..split]);
    let payload = &bytes[split + 4..];
    let mut status = "200 OK".to_owned();
    let mut headers = String::new();
    for line in raw_headers.lines() {
        if let Some(value) = line.strip_prefix("Status:") {
            status = value.trim().to_owned();
        } else if !line.is_empty() && !line.to_ascii_lowercase().starts_with("content-length:") {
            headers.push_str(line);
            headers.push_str("\r\n");
        }
    }
    let response = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    if stream.write_all(response.as_bytes()).is_ok() {
        // Count before sending the body: Git may finish as soon as it reads
        // Content-Length, before this handler observes connection shutdown.
        response_bytes.fetch_add(payload.len(), Ordering::Relaxed);
        let _ = stream.write_all(payload);
    }
}

fn start_smart_git_http(
    project_root: &Path,
) -> (
    std::net::SocketAddr,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
    thread::JoinHandle<()>,
) {
    let exec_path = Command::new("git")
        .args(["--exec-path"])
        .output()
        .expect("git exec-path");
    let backend = PathBuf::from(
        String::from_utf8(exec_path.stdout)
            .expect("exec-path")
            .trim(),
    )
    .join("git-http-backend");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    listener.set_nonblocking(true).expect("nonblocking");
    let address = listener.local_addr().expect("address");
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_for_thread = Arc::clone(&stopped);
    let response_bytes = Arc::new(AtomicUsize::new(0));
    let response_bytes_for_thread = Arc::clone(&response_bytes);
    let project_root = project_root.to_path_buf();
    let thread = thread::spawn(move || {
        while !stopped_for_thread.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let project_root = project_root.clone();
                    let backend = backend.clone();
                    let response_bytes = Arc::clone(&response_bytes_for_thread);
                    thread::spawn(move || {
                        serve_smart_git(stream, &project_root, &backend, &response_bytes);
                    });
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::Interrupted =>
                {
                    thread::yield_now();
                }
                Err(_) => thread::yield_now(),
            }
        }
    });
    (address, stopped, response_bytes, thread)
}

#[test]
fn partial_clone_skips_historical_blobs() {
    let root = std::env::temp_dir().join(format!(
        "nudox-forge-partial-{}-{}",
        std::process::id(),
        now_millis()
    ));
    let _ = fs::remove_dir_all(&root);
    let work = root.join("work");
    fs::create_dir_all(&work).expect("work directory");
    git_quiet(None, &["init", "--quiet", work.to_str().expect("work")]);
    git_quiet(Some(&work), &["config", "user.email", "forge@example.test"]);
    git_quiet(Some(&work), &["config", "user.name", "Forge Fixture"]);
    let mut bytes = vec![0_u8; 256 * 1024];
    for generation in 1_u64..=12 {
        let mut state = generation;
        for byte in &mut bytes {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 33) as u8;
        }
        fs::write(work.join("blob.bin"), &bytes).expect("blob");
        git_quiet(Some(&work), &["add", "blob.bin"]);
        git_quiet(Some(&work), &["commit", "--quiet", "-m", "generation"]);
    }
    git_quiet(Some(&work), &["branch", "-M", "main"]);
    let bare = root.join("server/acme/mono.git");
    fs::create_dir_all(bare.parent().expect("server parent")).expect("server");
    git_quiet(
        None,
        &[
            "clone",
            "--bare",
            "--quiet",
            work.to_str().expect("work"),
            bare.to_str().expect("bare"),
        ],
    );
    git_quiet(Some(&bare), &["config", "uploadpack.allowFilter", "true"]);
    git_quiet(
        Some(&bare),
        &["config", "uploadpack.allowReachableSHA1InWant", "true"],
    );
    let expected = String::from_utf8(
        Command::new("git")
            .args(["-C", work.to_str().expect("work"), "rev-parse", "HEAD"])
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("commit text");
    let expected = expected.trim();
    let (address, stopped, response_bytes, server) =
        start_smart_git_http(&root.join("server"));
    let url = format!("http://{address}/acme/mono.git");
    let full = root.join("full");
    git_quiet(None, &["init", "--quiet", full.to_str().expect("full")]);
    git_quiet(
        None,
        &[
            "-C",
            full.to_str().expect("full"),
            "fetch",
            "--quiet",
            "--no-tags",
            &url,
            "main",
        ],
    );
    // Elapsed time is host-sensitive, and the old comparison measured only
    // the full `fetch` against partial `resolve` plus repository setup and
    // commit/tree lookup. Compare response payloads from the same server.
    let full_transfer_bytes = response_bytes.swap(0, Ordering::Relaxed);
    let full_bytes = directory_bytes(&full.join(".git"));
    let coordinate = ForgeCoordinate::new(
        url.clone(),
        ForgeRevision::Branch(ForgeRefName::new("main").expect("branch")),
        None::<String>,
    )
    .expect("coordinate");
    let mut transport = GitCommandTransport::new(root.join("transport"))
        .expect("transport")
        .with_token(ForgeAuthToken::new("super-secret-token").expect("token"));
    let resolution = transport.resolve(&coordinate).expect("partial resolve");
    let partial_transfer_bytes = response_bytes.load(Ordering::Relaxed);
    let partial_bytes = directory_bytes(&root.join("transport/repo/.git"));
    assert_eq!(resolution.commit.as_hex(), expected);
    let config = fs::read_to_string(root.join("transport/repo/.git/config")).expect("git config");
    assert!(
        !config.contains("super-secret-token"),
        "bearer token was written into git config"
    );
    eprintln!(
        "forge_partial_clone full_disk_bytes={full_bytes} partial_disk_bytes={partial_bytes} \
         full_transfer_bytes={full_transfer_bytes} \
         partial_transfer_bytes={partial_transfer_bytes}"
    );
    assert!(
        full_transfer_bytes > 0 && partial_transfer_bytes > 0,
        "expected non-empty HTTP response payloads for both fetches"
    );
    assert!(
        partial_bytes.saturating_mul(8) < full_bytes,
        "partial clone kept {partial_bytes} bytes against a full fetch of {full_bytes}"
    );
    assert!(
        partial_transfer_bytes.saturating_mul(8) < full_transfer_bytes,
        "partial clone transferred {partial_transfer_bytes} HTTP response bytes against a full fetch of {full_transfer_bytes} bytes"
    );
    let archive = transport
        .fetch_archive(&coordinate, &resolution)
        .expect("archive");
    let mut tar = Vec::new();
    archive
        .into_reader()
        .read_to_end(&mut tar)
        .expect("read archive");
    assert!(
        tar.windows(8).any(|window| window == b"blob.bin"),
        "partial archive omitted the tip blob"
    );
    let missing = ForgeCoordinate::new(
        url,
        ForgeRevision::Branch(ForgeRefName::new("no-such-ref").expect("branch")),
        None::<String>,
    )
    .expect("missing coordinate");
    assert_eq!(
        transport.resolve(&missing).expect_err("missing ref"),
        ForgeTransportError::NotFound
    );
    stopped.store(true, Ordering::Relaxed);
    let _ = TcpStream::connect(address);
    server.join().expect("server");
    let _ = fs::remove_dir_all(root);
}
