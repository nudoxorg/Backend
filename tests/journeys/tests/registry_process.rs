//! Process journey for a canonical registry package add.
//!
//! The feed and archive are served by a loopback HTTP endpoint. The test then
//! restarts locald without the endpoint and proves that the durable owner can
//! satisfy the same add from its published object while the normal source
//! ingestion and query path remains usable.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use backend_engine::capability::CapabilityArtifactId;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(12);
const AUTHORITY_SECRET: [u8; 32] = [0x5a; 32];
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn spawn(args: &[OsString]) -> Self {
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn locald");
        Self { child: Some(child) }
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll locald"))
            .is_none()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        if child
            .try_wait()
            .expect("poll locald during cleanup")
            .is_none()
        {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

fn run_bounded(mut command: ProcessCommand, label: &str) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {label}: {error}"));
    let mut stdout = child.stdout.take().expect("command stdout");
    let mut stderr = child.stderr.take().expect("command stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read command stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read command stderr");
        bytes
    });
    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::yield_now(),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{label} exceeded its bounded deadline");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("wait {label}: {error}");
            }
        }
    };
    Output {
        status,
        stdout: stdout_reader.join().expect("join command stdout"),
        stderr: stderr_reader.join().expect("join command stderr"),
    }
}

fn unique_root() -> PathBuf {
    let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "backend-registry-process-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create process journey root");
    path
}

fn authority_secret(root: &Path) -> PathBuf {
    let path = root.join("authority.secret");
    std::fs::write(&path, AUTHORITY_SECRET).expect("write authority secret");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat authority secret")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&path, permissions).expect("protect authority secret");
    path
}

fn wait_for_socket(path: &Path, child: &mut ChildGuard) {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        assert!(child.running(), "locald exited before readiness");
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && metadata.file_type().is_socket()
            && metadata.permissions().mode() & 0o777 == 0o600
            && UnixStream::connect(path).is_ok()
        {
            return;
        }
        thread::yield_now();
    }
    panic!("timed out waiting for locald socket {}", path.display());
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

fn tar_archive(path: &str, content: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    let mut header = [0_u8; 512];
    header[..path.len()].copy_from_slice(path.as_bytes());
    let size = format!("{:011o}\0", content.len());
    header[124..136].copy_from_slice(size.as_bytes());
    header[156] = b'0';
    header[148..156].fill(b' ');
    let checksum = header.iter().map(|byte| u64::from(*byte)).sum::<u64>();
    let checksum = format!("{:06o}\0 ", checksum);
    header[148..156].copy_from_slice(checksum.as_bytes());
    archive.extend_from_slice(&header);
    archive.extend_from_slice(content);
    archive.resize(archive.len().div_ceil(512) * 512, 0);
    archive.extend_from_slice(&[0; 1024]);
    archive
}

fn loopback_registry(archive: Vec<u8>) -> (String, thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback registry");
    let address = listener.local_addr().expect("read registry address");
    let endpoint = format!("http://{address}");
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let feed = format!(
        concat!(
            "{{\"schema\":1,\"next\":\"{}\",\"items\":[{{",
            "\"name\":\"demo\",\"version\":\"1.2.3\",",
            "\"archive\":\"{}/archive\",\"blake3\":\"{}\",",
            "\"provenance\":\"{}\"}}]}}"
        ),
        hex(&[7; 32]),
        endpoint,
        hex(&digest),
        hex(&[9; 32]),
    )
    .into_bytes();
    let responses = [feed, archive];
    let server = thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().expect("accept registry request");
            let mut request = [0_u8; 8192];
            let length = stream.read(&mut request).expect("read registry request");
            let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
            assert!(
                request.contains("authorization: bearer journey-token"),
                "registry request did not carry configured authorization"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write registry headers");
            stream.write_all(&body).expect("write registry response");
        }
    });
    (endpoint, server)
}

fn locald_args(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    registry: &str,
) -> Vec<OsString> {
    vec![
        OsString::from("--endpoint"),
        endpoint.as_os_str().to_owned(),
        OsString::from("--workspace"),
        workspace.as_os_str().to_owned(),
        OsString::from("--profile"),
        OsString::from("builtin"),
        OsString::from("--authority-secret-file"),
        authority.as_os_str().to_owned(),
        OsString::from("--registry-endpoint"),
        OsString::from(registry),
        OsString::from("--registry-ecosystem"),
        OsString::from("cargo"),
        OsString::from("--registry-auth"),
        OsString::from("Bearer journey-token"),
        OsString::from("--max-frame"),
        OsString::from("131072"),
        OsString::from("--timeout-ms"),
        OsString::from("3000"),
    ]
}

fn cli_add(endpoint: &Path) -> Output {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("add")
        .arg("pkg:cargo/demo@1.2.3");
    run_bounded(command, "backend-cli add")
}

fn cli_name(endpoint: &Path) -> Output {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("name")
        .arg("from_registry");
    run_bounded(command, "backend-cli name")
}

#[test]
fn remote_add_materializes_searchable_rows_and_reuses_cursor_after_restart() {
    let root = unique_root();
    let endpoint = root.join("locald.sock");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create daemon workspace");
    let authority = authority_secret(&root);
    let archive = tar_archive("package/src/lib.rs", b"pub fn from_registry() {}\n");
    let (registry, server) = loopback_registry(archive);
    let args = locald_args(&endpoint, &workspace, &authority, &registry);

    let mut first = ChildGuard::spawn(&args);
    wait_for_socket(&endpoint, &mut first);
    let added = cli_add(&endpoint);
    assert!(
        added.status.success(),
        "remote add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&added.stdout),
        String::from_utf8_lossy(&added.stderr)
    );
    let indexed = cli_name(&endpoint);
    assert!(
        indexed.status.success()
            && String::from_utf8_lossy(&indexed.stdout).contains("from_registry"),
        "remote archive content was not searchable: stdout={} stderr={}",
        String::from_utf8_lossy(&indexed.stdout),
        String::from_utf8_lossy(&indexed.stderr)
    );
    server.join().expect("loopback registry server");
    let journal = workspace.join("registry/registry.journal");
    let journal_len = std::fs::metadata(&journal)
        .expect("registry journal after first add")
        .len();

    drop(first);
    let mut second = ChildGuard::spawn(&args);
    wait_for_socket(&endpoint, &mut second);
    let reused = cli_add(&endpoint);
    assert!(
        reused.status.success(),
        "restart add did not reuse durable archive: stdout={} stderr={}",
        String::from_utf8_lossy(&reused.stdout),
        String::from_utf8_lossy(&reused.stderr)
    );
    let still_indexed = cli_name(&endpoint);
    assert!(
        still_indexed.status.success()
            && String::from_utf8_lossy(&still_indexed.stdout).contains("from_registry"),
        "restart lost searchable remote rows: stdout={} stderr={}",
        String::from_utf8_lossy(&still_indexed.stdout),
        String::from_utf8_lossy(&still_indexed.stderr)
    );
    assert_eq!(
        std::fs::metadata(&journal)
            .expect("registry journal after restart add")
            .len(),
        journal_len,
        "restart add unexpectedly advanced the registry cursor"
    );

    drop(second);
    std::fs::remove_dir_all(root).expect("remove registry process fixture");
}
