//! A hermetic, in-process package registry for process journeys.
//!
//! The server speaks the product's canonical feed protocol (`schema=1`), the
//! same grammar `crates/engine/src/registry/feed.rs` admits for a configured
//! `--registry-endpoint` that is not in native mode. It serves:
//!
//! * `GET /feed?schema=1&cursor=<hex>&limit=<n>`: every configured package on
//!   the genesis cursor, then an empty page whose `next` equals the cursor, so
//!   the owner's durable journal reaches a real fixed point;
//! * `GET /archive/<index>`: the package's tar archive, digest-bound in the
//!   feed row.
//!
//! Faults are explicit and switchable at runtime, so a journey can take the
//! registry down mid-session and assert the typed failure a user sees. Every
//! request path is logged so a journey can prove whether an operation reached
//! the network at all.
//!
//! The listener binds loopback TCP only, which is portable across Unix and
//! Windows; nothing here depends on a Unix socket.

#![allow(dead_code, unreachable_pub)]

use backend_engine::capability::CapabilityArtifactId;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// One file inside a package archive.
#[derive(Clone, Debug)]
pub struct FakeFile {
    /// Path relative to the archive's `package/` root.
    pub path: &'static str,
    /// Exact file bytes.
    pub contents: &'static str,
}

/// One package release served by the fake registry.
#[derive(Clone, Debug)]
pub struct FakePackage {
    /// Registry-native package name, e.g. `demo`, `group:artifact`.
    pub name: String,
    /// Exact release version.
    pub version: String,
    /// Files placed under `package/` in the archive.
    pub files: Vec<FakeFile>,
}

/// How the registry answers the next requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryMode {
    /// Serve the feed and archives faithfully.
    Serve,
    /// Serve the feed, then drop each archive connection after sending the
    /// headers and half of the body: an outage in the middle of a download.
    TruncateArchives,
    /// Answer every request `503 Service Unavailable`.
    Down,
}

#[derive(Debug)]
struct Shared {
    mode: RegistryMode,
    requests: Vec<String>,
}

/// A running loopback registry. Dropping it stops the listener.
#[derive(Debug)]
pub struct FakeRegistry {
    endpoint: String,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

const END_CURSOR: [u8; 32] = [0x7e; 32];

impl FakeRegistry {
    /// Starts serving `packages` for `ecosystem` (`cargo`, `pypi`, `npm`,
    /// `golang`, `maven`, `nuget`, or `cpp`). Rows are ordered by their
    /// canonical package URL, as the feed grammar requires.
    #[must_use]
    pub fn start(ecosystem: &str, packages: Vec<FakePackage>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake registry");
        listener
            .set_nonblocking(true)
            .expect("make fake registry listener non-blocking");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("fake registry address")
        );
        let archives = packages
            .iter()
            .map(|package| tar_archive(&package.files))
            .collect::<Vec<_>>();
        let mut rows = packages
            .iter()
            .enumerate()
            .map(|(index, package)| {
                let digest = *CapabilityArtifactId::from_value(&archives[index]).as_bytes();
                (
                    canonical_purl(ecosystem, &package.name, &package.version),
                    serde_json::json!({
                        "name": package.name,
                        "version": package.version,
                        "archive": format!("/archive/{index}"),
                        "blake3": hex(&digest),
                        "provenance": hex(blake3::hash(package.name.as_bytes()).as_bytes()),
                    }),
                )
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        let first_page = serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "next": hex(&END_CURSOR),
            "items": rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>(),
        }))
        .expect("encode fake feed page");
        let last_page = serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "next": hex(&END_CURSOR),
            "items": [],
        }))
        .expect("encode fake feed tail");
        let shared = Arc::new(Mutex::new(Shared {
            mode: RegistryMode::Serve,
            requests: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            serve(stream, &shared, &first_page, &last_page, &archives);
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("fake registry accept: {error}"),
                    }
                }
            })
        };
        Self {
            endpoint,
            shared,
            stop,
            server: Some(server),
        }
    }

    /// Base URL to pass as `--registry-endpoint`.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Switches how subsequent requests are answered.
    pub fn set_mode(&self, mode: RegistryMode) {
        self.shared.lock().expect("fake registry state").mode = mode;
    }

    /// Every request path received so far, in arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.shared
            .lock()
            .expect("fake registry state")
            .requests
            .clone()
    }
}

impl Drop for FakeRegistry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

/// The canonical package URL the product derives from one feed row.
#[must_use]
pub fn canonical_purl(ecosystem: &str, name: &str, version: &str) -> String {
    let (package_type, path) = match ecosystem {
        "maven" => ("maven", name.replacen(':', "/", 1)),
        "cpp" => ("generic", name.replacen(':', "/", 1)),
        other => (other, name.to_owned()),
    };
    format!("pkg:{package_type}/{path}@{version}")
}

fn serve(
    mut stream: TcpStream,
    shared: &Mutex<Shared>,
    first_page: &[u8],
    last_page: &[u8],
    archives: &[Vec<u8>],
) {
    stream
        .set_nonblocking(false)
        .expect("make accepted registry stream blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("bound registry request read");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
            Err(_) => return,
        }
    }
    let head = String::from_utf8_lossy(&request);
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_owned();
    let mode = {
        let mut state = shared.lock().expect("fake registry state");
        state.requests.push(path.clone());
        state.mode
    };
    if mode == RegistryMode::Down {
        respond(&mut stream, "503 Service Unavailable", b"", None);
        return;
    }
    if let Some(query) = path.strip_prefix("/feed?") {
        let cursor = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("cursor="))
            .unwrap_or("");
        let body = if cursor == hex(&END_CURSOR) {
            last_page
        } else {
            first_page
        };
        respond(&mut stream, "200 OK", body, None);
        return;
    }
    if let Some(index) = path
        .strip_prefix("/archive/")
        .and_then(|index| index.parse::<usize>().ok())
        && let Some(archive) = archives.get(index)
    {
        let truncate = (mode == RegistryMode::TruncateArchives).then_some(archive.len() / 2);
        respond(&mut stream, "200 OK", archive, truncate);
        return;
    }
    respond(&mut stream, "404 Not Found", b"", None);
}

fn respond(stream: &mut TcpStream, status: &str, body: &[u8], truncate: Option<usize>) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let sent = truncate.unwrap_or(body.len());
    let _ = stream.write_all(&body[..sent]);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

/// Builds a POSIX ustar archive with every file under `package/`.
fn tar_archive(files: &[FakeFile]) -> Vec<u8> {
    let mut archive = Vec::new();
    for file in files {
        let path = format!("package/{}", file.path);
        assert!(path.len() < 100, "fake archive path too long: {path}");
        let content = file.contents.as_bytes();
        let mut header = [0_u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        let size = format!("{:011o}\0", content.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].fill(b' ');
        let checksum = header.iter().map(|byte| u64::from(*byte)).sum::<u64>();
        let checksum = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
        archive.extend_from_slice(&header);
        archive.extend_from_slice(content);
        archive.resize(archive.len().div_ceil(512) * 512, 0);
    }
    archive.extend_from_slice(&[0; 1024]);
    archive
}
