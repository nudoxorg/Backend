//! Source-only discovery journeys exercise the real feed adapters, owner
//! journal, CLI and MCP projections without acquiring package archives.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use backend_library::{
    Command, CommandDto, CommandReply, ProductText, RegistryDiscoveryCompleteness,
    RegistryDiscoveryFreshness, RegistryDiscoveryStanding, RegistrySearchHit, SurfaceCommand,
    SurfaceReply,
};
use backend_mcp::{decode_reply, encode_request};
use serde_json::json;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const QUICK_QUERY_BOUND: Duration = Duration::from_secs(2);
const POLL_BOUND: Duration = Duration::from_secs(20);
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum FeedKind {
    Cargo,
    Nuget,
}

struct FeedFixture {
    kind: FeedKind,
    address: SocketAddr,
    mode: Arc<AtomicUsize>,
    entered: Arc<AtomicBool>,
    released: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl FeedFixture {
    fn start(kind: FeedKind, hold_first: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind registry fixture");
        listener
            .set_nonblocking(true)
            .expect("make registry fixture nonblocking");
        let address = listener.local_addr().expect("registry fixture address");
        let mode = Arc::new(AtomicUsize::new(0));
        let hold = Arc::new(AtomicBool::new(hold_first));
        let entered = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (
            thread_mode,
            thread_hold,
            thread_entered,
            thread_released,
            thread_stop,
            thread_requests,
        ) = (
            Arc::clone(&mode),
            Arc::clone(&hold),
            Arc::clone(&entered),
            Arc::clone(&released),
            Arc::clone(&stop),
            Arc::clone(&requests),
        );
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve_request(
                        stream,
                        kind,
                        address,
                        &thread_mode,
                        &thread_hold,
                        &thread_entered,
                        &thread_released,
                        &thread_stop,
                        &thread_requests,
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            kind,
            address,
            mode,
            entered,
            released,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn source_url(&self) -> String {
        match self.kind {
            FeedKind::Cargo => self.base_url(),
            FeedKind::Nuget => format!("{}/v3/catalog0/index.json", self.base_url()),
        }
    }

    fn wait_until_held(&self) {
        let end = Instant::now() + Duration::from_secs(5);
        while !self.entered.load(Ordering::Acquire) && Instant::now() < end {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            self.entered.load(Ordering::Acquire),
            "feed worker did not reach held response"
        );
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
    }

    fn advance(&self) {
        self.mode.store(1, Ordering::Release);
    }

    fn fail(&self) {
        self.mode.store(2, Ordering::Release);
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("request log lock").clone()
    }
}

impl Drop for FeedFixture {
    fn drop(&mut self) {
        self.released.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("stop registry fixture");
        }
    }
}

fn serve_request(
    mut stream: TcpStream,
    kind: FeedKind,
    address: SocketAddr,
    mode: &AtomicUsize,
    hold_first: &AtomicBool,
    entered: &AtomicBool,
    released: &AtomicBool,
    stop: &AtomicBool,
    requests: &Mutex<Vec<String>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => return,
            Ok(_) if header == "\r\n" || header == "\n" => break,
            Ok(_) => {}
        }
    }
    let target = request.split_whitespace().nth(1).unwrap_or("/");
    requests
        .lock()
        .expect("request log lock")
        .push(target.to_owned());
    if matches!(kind, FeedKind::Nuget)
        && target.starts_with("/v3/catalog0/index.json")
        && hold_first.swap(false, Ordering::AcqRel)
    {
        entered.store(true, Ordering::Release);
        let end = Instant::now() + Duration::from_secs(10);
        while !released.load(Ordering::Acquire)
            && !stop.load(Ordering::Acquire)
            && Instant::now() < end
        {
            thread::sleep(Duration::from_millis(5));
        }
    }
    let path = target.split('?').next().unwrap_or(target);
    if matches!(kind, FeedKind::Nuget)
        && mode.load(Ordering::Acquire) == 2
        && path == "/v3/catalog0/index.json"
    {
        write_response(&mut stream, 503, "Unavailable", b"offline");
        return;
    }
    let body = fixture_body(kind, address, mode.load(Ordering::Acquire), path);
    match body {
        Some(body) => write_response(&mut stream, 200, "OK", &body),
        None => write_response(&mut stream, 404, "Not Found", b"not found"),
    }
}

fn fixture_body(kind: FeedKind, address: SocketAddr, mode: usize, path: &str) -> Option<Vec<u8>> {
    match kind {
        FeedKind::Cargo => match path {
            "/api/v1/crates" => Some(
                json!({
                    "crates": [{
                        "name": "serde",
                        "newest_version": "1.2.3",
                        "max_stable_version": "1.2.3",
                        "updated_at": "2026-09-01T12:00:00Z"
                    }],
                    "meta": {"total": 1}
                })
                .to_string()
                .into_bytes(),
            ),
            "/index/se/rd/serde" => Some(
                b"{\"name\":\"serde\",\"vers\":\"1.0.0\",\"yanked\":false}\n{\"name\":\"serde\",\"vers\":\"1.2.3\",\"yanked\":true}\n"
                    .to_vec(),
            ),
            _ => None,
        },
        FeedKind::Nuget => {
            let base = format!("http://{address}/v3/catalog0");
            let t1 = "2026-09-01T00:00:00Z";
            let t2 = "2026-09-02T00:00:00Z";
            let pages = if mode == 0 {
                vec![("page0.json", t1, "event0.json")]
            } else {
                vec![
                    ("page0.json", t1, "event0.json"),
                    ("page1.json", t2, "event1.json"),
                ]
            };
            match path {
                "/v3/catalog0/index.json" => Some(
                    json!({
                        "commitId": if mode == 0 { "commit-1" } else { "commit-2" },
                        "commitTimeStamp": if mode == 0 { t1 } else { t2 },
                        "count": pages.len(),
                        "items": pages.iter().map(|(file, timestamp, _)| json!({
                            "@id": format!("{base}/{file}"),
                            "commitId": format!("page-{timestamp}"),
                            "commitTimeStamp": timestamp,
                            "count": 1
                        })).collect::<Vec<_>>()
                    })
                    .to_string()
                    .into_bytes(),
                ),
                "/v3/catalog0/page0.json" | "/v3/catalog0/page1.json" => {
                    let is_second = path.ends_with("page1.json");
                    let (timestamp, leaf) = if is_second {
                        (t2, "event1.json")
                    } else {
                        (t1, "event0.json")
                    };
                    Some(
                        json!({
                            "commitId": format!("page-{timestamp}"),
                            "commitTimeStamp": timestamp,
                            "count": 1,
                            "items": [{
                                "@id": format!("{base}/data/{leaf}"),
                                "commitTimeStamp": timestamp,
                                "commitId": format!("event-{timestamp}")
                            }]
                        })
                        .to_string()
                        .into_bytes(),
                    )
                }
                "/v3/catalog0/data/event0.json" => Some(
                    json!({
                        "@type": "nuget:PackageDetails",
                        "commitTimeStamp": t1,
                        "nuget:id": "Widget",
                        "nuget:version": "1.0.0",
                        "listed": true
                    })
                    .to_string()
                    .into_bytes(),
                ),
                "/v3/catalog0/data/event1.json" => Some(
                    json!({
                        "@type": "nuget:PackageDetails",
                        "commitTimeStamp": t2,
                        "nuget:id": "Widget",
                        "nuget:version": "1.0.0",
                        "listed": false
                    })
                    .to_string()
                    .into_bytes(),
                ),
                _ => None,
            }
        }
    }
}

fn write_response(stream: &mut TcpStream, status: u16, reason: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

#[derive(Debug)]
struct Locald {
    child: Option<Child>,
}

impl Locald {
    fn launch(
        endpoint: &Path,
        workspace: &Path,
        authority: &Path,
        source: &str,
        offline: bool,
    ) -> Self {
        let mut args = surface_matrix::locald_args(endpoint, workspace, authority, Some(0), false);
        args.extend([
            OsString::from("--registry-discovery-source"),
            OsString::from(source),
        ]);
        if offline {
            args.push(OsString::from("--registry-discovery-offline"));
        }
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .env_remove("NUDOX_RUSTC");
        let child = command.spawn().expect("spawn locald");
        let mut daemon = Self { child: Some(child) };
        wait_for_socket(endpoint, &mut daemon);
        daemon
    }

    fn kill_now(&mut self) {
        let child = self.child.as_mut().expect("locald child remains owned");
        if child.try_wait().expect("poll locald").is_none() {
            child.kill().expect("kill locald");
        }
        child.wait().expect("reap locald");
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll locald"))
            .is_none()
    }
}

impl Drop for Locald {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn wait_for_socket(endpoint: &Path, daemon: &mut Locald) {
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        assert!(daemon.running(), "locald exited before readiness");
        if let Ok(metadata) = std::fs::symlink_metadata(endpoint)
            && metadata.file_type().is_socket()
            && UnixStream::connect(endpoint).is_ok()
        {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for locald socket {}", endpoint.display());
}

fn unique_root(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-registry-discovery-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-registry-discovery-{label}-{}-{serial}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&endpoint);
    endpoint
}

fn authority(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("authority.secret");
    std::fs::write(&path, [0x5a_u8; 32]).expect("write authority key");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat authority key")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&path, permissions).expect("protect authority key");
    path
}

fn search_command(query: &str) -> SurfaceCommand {
    SurfaceCommand::IndexSearch {
        query: ProductText::new(query).expect("search query"),
        limit: 10,
        cursor: None,
    }
}

fn cli_command(endpoint: &Path) -> ProcessCommand {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--format")
        .arg("json")
        .arg("surface")
        .arg(serde_json::to_string(&search_command("Widget")).expect("encode search"));
    command
}

fn run_quick(
    mut command: ProcessCommand,
    label: &str,
    input: Option<Vec<u8>>,
) -> (Output, Duration) {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {label}: {error}"));
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("quick child stdin")
            .write_all(&input)
            .unwrap_or_else(|error| panic!("write {label} input: {error}"));
    }
    let mut stdout = child.stdout.take().expect("quick child stdout");
    let mut stderr = child.stderr.take().expect("quick child stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read quick stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read quick stderr");
        bytes
    });
    let deadline = start + QUICK_QUERY_BOUND;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{label} blocked on the catalog response for {QUICK_QUERY_BOUND:?}");
            }
            Err(error) => panic!("wait {label}: {error}"),
        }
    };
    let elapsed = start.elapsed();
    (
        Output {
            status,
            stdout: stdout_reader.join().expect("join quick stdout"),
            stderr: stderr_reader.join().expect("join quick stderr"),
        },
        elapsed,
    )
}

fn run_bounded(command: ProcessCommand, label: &str, input: Option<Vec<u8>>) -> Output {
    surface_matrix::run_bounded(command, label, input)
}

fn mcp_command(endpoint: &Path, workspace: &Path) -> ProcessCommand {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
    command
        .arg("--framed")
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace);
    command
}

fn mcp_request(query: &str) -> Vec<u8> {
    encode_request(&CommandDto::new(1, Command::Surface(search_command(query))))
        .expect("encode MCP query")
}

fn decode_surface(output: &Output, label: &str) -> SurfaceReply {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let reply = decode_reply(&output.stdout).expect("decode MCP reply");
    let CommandReply::Surface(surface) = reply.reply else {
        panic!("{label} returned a non-surface reply: {:?}", reply.reply);
    };
    surface
}

fn cli_search(endpoint: &Path, quick: bool) -> Output {
    let output = if quick {
        run_quick(cli_command(endpoint), "CLI index-search", None).0
    } else {
        run_bounded(cli_command(endpoint), "CLI index-search", None)
    };
    assert!(
        output.status.success(),
        "CLI index-search failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn mcp_search(endpoint: &Path, workspace: &Path, query: &str, quick: bool) -> SurfaceReply {
    let command = mcp_command(endpoint, workspace);
    let request = mcp_request(query);
    let output = if quick {
        run_quick(command, "MCP index-search", Some(request)).0
    } else {
        run_bounded(command, "MCP index-search", Some(request))
    };
    decode_surface(&output, "MCP index-search")
}

fn wait_for_cli_candidate(endpoint: &Path, query: &str, coordinate: &str) -> Output {
    wait_for_cli_text(endpoint, query, coordinate, None)
}

fn wait_for_cli_text(
    endpoint: &Path,
    query: &str,
    coordinate: &str,
    marker: Option<&str>,
) -> Output {
    let end = Instant::now() + POLL_BOUND;
    loop {
        let output = cli_search_with_query(endpoint, query, false);
        let text = String::from_utf8_lossy(&output.stdout);
        if text.contains(coordinate) && marker.is_none_or(|marker| text.contains(marker)) {
            return output;
        }
        assert!(
            Instant::now() < end,
            "catalog candidate did not arrive: {text}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn cli_search_with_query(endpoint: &Path, query: &str, quick: bool) -> Output {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--format")
        .arg("json")
        .arg("surface")
        .arg(serde_json::to_string(&search_command(query)).expect("encode search"));
    let output = if quick {
        run_quick(command, "CLI index-search", None).0
    } else {
        run_bounded(command, "CLI index-search", None)
    };
    assert!(
        output.status.success(),
        "CLI index-search failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn discovered(reply: SurfaceReply) -> Vec<backend_library::RegistryDiscoveryCandidate> {
    let SurfaceReply::IndexSearchWithDiscovery(hits) = reply else {
        panic!("index-search did not return typed acquired/discovered hits: {reply:?}");
    };
    hits.iter()
        .filter_map(|hit| match hit {
            RegistrySearchHit::Discovered(candidate) => Some(candidate.clone()),
            RegistrySearchHit::PackageGroup(group) => {
                group.releases.iter().find_map(|release| match release {
                    backend_library::RegistrySearchRelease::Discovered(candidate) => {
                        Some(candidate.clone())
                    }
                    backend_library::RegistrySearchRelease::Acquired(_)
                    | backend_library::RegistrySearchRelease::ForgeDiscovered(_) => None,
                })
            }
            RegistrySearchHit::Acquired(_)
            | RegistrySearchHit::ForgeDiscovered(_)
            | RegistrySearchHit::LocalDeclaration(_) => None,
        })
        .collect()
}

fn assert_metadata_only(requests: &[String]) {
    assert!(!requests.is_empty(), "fixture saw no metadata requests");
    assert!(
        requests.iter().all(|path| {
            !path.to_ascii_lowercase().contains(".nupkg")
                && !path.to_ascii_lowercase().contains("archive")
                && !path.to_ascii_lowercase().contains("compiler")
                && !path.to_ascii_lowercase().contains("download")
        }),
        "discovery fetched non-metadata content: {requests:?}"
    );
}

#[test]
fn nuget_discovery_is_nonblocking_durable_and_tracks_unlisting() {
    let root = unique_root("nuget");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = authority(&root);
    let endpoint = unique_endpoint("nuget");
    let feed = FeedFixture::start(FeedKind::Nuget, true);
    let source = format!("nuget={}", feed.source_url());

    let mut daemon = Locald::launch(&endpoint, &workspace, &authority, &source, false);
    feed.wait_until_held();
    let (cli, cli_elapsed) = run_quick(cli_command(&endpoint), "held CLI index-search", None);
    assert!(cli.status.success(), "held CLI search failed: {cli:?}");
    assert!(cli_elapsed < QUICK_QUERY_BOUND);
    let (mcp, mcp_elapsed) = run_quick(
        mcp_command(&endpoint, &workspace),
        "held MCP index-search",
        Some(mcp_request("Widget")),
    );
    assert!(mcp.status.success(), "held MCP search failed: {mcp:?}");
    assert!(mcp_elapsed < QUICK_QUERY_BOUND);
    assert!(discovered(decode_surface(&mcp, "held MCP search")).is_empty());

    feed.release();
    let published = wait_for_cli_candidate(&endpoint, "Widget", "pkg:nuget/Widget@1.0.0");
    let published_text = String::from_utf8_lossy(&published.stdout);
    assert!(
        published_text.contains("discovered · unacquired"),
        "{published_text}"
    );
    assert!(published_text.contains("published"), "{published_text}");
    assert!(
        published_text.contains("complete through cursor"),
        "{published_text}"
    );
    let mcp_hits = discovered(mcp_search(&endpoint, &workspace, "Widget", false));
    assert_eq!(mcp_hits.len(), 1);
    assert_eq!(mcp_hits[0].standing, RegistryDiscoveryStanding::Published);
    assert_eq!(
        mcp_hits[0].completeness,
        RegistryDiscoveryCompleteness::CompleteThroughCursor
    );
    assert!(matches!(
        mcp_hits[0].freshness,
        RegistryDiscoveryFreshness::Current { .. }
    ));
    daemon.kill_now(); // SIGKILL after the owner acknowledged the durable batch.

    let before_offline = feed.requests().len();
    let mut offline = Locald::launch(&endpoint, &workspace, &authority, &source, true);
    let offline_text = String::from_utf8_lossy(&cli_search(&endpoint, false).stdout).into_owned();
    assert!(
        offline_text.contains("pkg:nuget/Widget@1.0.0"),
        "{offline_text}"
    );
    assert!(
        offline_text.contains("historical observation"),
        "{offline_text}"
    );
    assert_eq!(
        feed.requests().len(),
        before_offline,
        "offline reopen polled the feed"
    );
    let offline_hits = discovered(mcp_search(&endpoint, &workspace, "Widget", false));
    assert_eq!(offline_hits.len(), 1);
    assert!(matches!(
        offline_hits[0].freshness,
        RegistryDiscoveryFreshness::Historical { .. }
    ));
    offline.kill_now();

    feed.advance();
    let mut updated = Locald::launch(&endpoint, &workspace, &authority, &source, false);
    let yanked = wait_for_cli_text(
        &endpoint,
        "Widget",
        "pkg:nuget/Widget@1.0.0",
        Some("yanked"),
    );
    let yanked_text = String::from_utf8_lossy(&yanked.stdout);
    assert!(
        yanked_text.contains("yanked"),
        "unlisting was not projected: {yanked_text}"
    );
    let updated_hits = discovered(mcp_search(&endpoint, &workspace, "Widget", false));
    assert_eq!(updated_hits.len(), 1);
    assert_eq!(updated_hits[0].standing, RegistryDiscoveryStanding::Yanked);
    updated.kill_now();

    let mut cold = Locald::launch(&endpoint, &workspace, &authority, &source, true);
    let cold_hits = discovered(mcp_search(&endpoint, &workspace, "Widget", false));
    assert_eq!(cold_hits.len(), 1);
    assert_eq!(cold_hits[0].standing, RegistryDiscoveryStanding::Yanked);
    assert!(matches!(
        cold_hits[0].freshness,
        RegistryDiscoveryFreshness::Historical { .. }
    ));
    cold.kill_now();

    feed.fail();
    let mut failed = Locald::launch(&endpoint, &workspace, &authority, &source, false);
    let unavailable_text = wait_for_cli_text(
        &endpoint,
        "Widget",
        "pkg:nuget/Widget@1.0.0",
        Some("refresh unavailable · historical"),
    );
    assert!(String::from_utf8_lossy(&unavailable_text.stdout).contains("yanked"));
    let failed_hits = discovered(mcp_search(&endpoint, &workspace, "Widget", false));
    assert_eq!(failed_hits.len(), 1);
    assert_eq!(failed_hits[0].standing, RegistryDiscoveryStanding::Yanked);
    assert!(matches!(
        failed_hits[0].freshness,
        RegistryDiscoveryFreshness::Unavailable {
            historical: true,
            ..
        }
    ));
    failed.kill_now();

    let requests = feed.requests();
    assert_metadata_only(&requests);
    assert!(
        requests
            .iter()
            .all(|path| path.starts_with("/v3/catalog0/")),
        "NuGet discovery requested data outside its catalog: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|path| path.contains("/v3/catalog0/index.json"))
    );
    assert!(requests.iter().any(|path| path.contains("event0.json")));
    assert!(requests.iter().any(|path| path.contains("event1.json")));
}

#[test]
fn crates_recent_and_sparse_discovery_is_explicitly_windowed() {
    let root = unique_root("cargo");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = authority(&root);
    let endpoint = unique_endpoint("cargo");
    let feed = FeedFixture::start(FeedKind::Cargo, false);
    let source = format!("cargo={}", feed.source_url());
    let mut daemon = Locald::launch(&endpoint, &workspace, &authority, &source, false);

    let result = wait_for_cli_candidate(&endpoint, "serde", "pkg:cargo/serde@1.2.3");
    let text = String::from_utf8_lossy(&result.stdout);
    assert!(text.contains("discovered · unacquired"), "{text}");
    assert!(
        text.contains("windowed coverage"),
        "crates.io is not an event-complete feed: {text}"
    );
    assert!(
        text.contains("yanked"),
        "sparse-index yank state was omitted: {text}"
    );
    let hits = discovered(mcp_search(&endpoint, &workspace, "serde", false));
    assert!(hits.iter().any(|candidate| {
        candidate.coordinate.as_str() == "pkg:cargo/serde@1.2.3"
            && candidate.standing == RegistryDiscoveryStanding::Yanked
            && candidate.completeness == RegistryDiscoveryCompleteness::Windowed
    }));
    daemon.kill_now();

    let requests = feed.requests();
    assert_metadata_only(&requests);
    assert!(
        requests
            .iter()
            .all(|path| { path.starts_with("/api/v1/crates?") || path == "/index/se/rd/serde" }),
        "Cargo discovery requested data outside recent/sparse metadata: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|path| path.starts_with("/api/v1/crates?"))
    );
    assert!(requests.iter().any(|path| path == "/index/se/rd/serde"));
}
