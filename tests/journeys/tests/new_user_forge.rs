//! Forge add and cache-only reference cross real locald, CLI, and MCP processes.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CHILD_POLL: Duration = Duration::from_millis(10);
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn spawn(args: &[OsString]) -> Self {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .env_remove("NUDOX_RUSTC");
        Self {
            child: Some(command.spawn().expect("spawn locald")),
        }
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll locald"))
            .is_none()
    }

    fn kill_now(&mut self) {
        let child = self.child.as_mut().expect("locald child remains owned");
        if child
            .try_wait()
            .expect("poll locald before SIGKILL")
            .is_none()
        {
            child.kill().expect("SIGKILL locald");
        }
        child.wait().expect("reap killed locald");
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
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
}

struct HttpFixture {
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    address: std::net::SocketAddr,
}

impl HttpFixture {
    fn start(root: PathBuf) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback git HTTP fixture");
        listener
            .set_nonblocking(true)
            .expect("make loopback fixture nonblocking");
        let address = listener.local_addr().expect("loopback fixture address");
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_stop = Arc::clone(&stop);
        let thread_root = root.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        thread_requests.fetch_add(1, Ordering::Relaxed);
                        serve_git_http(stream, &thread_root);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            requests,
            stop,
            thread: Some(thread),
            address,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}/acme/widget", self.address)
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("stop loopback fixture");
        }
    }
}

fn serve_git_http(mut stream: TcpStream, root: &Path) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
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
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .trim_start_matches('/');
    let relative = Path::new(path);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        write_http_error(&mut stream, 400, "Bad Request");
        return;
    }
    let file = root.join(relative);
    let body = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(_) => {
            write_http_error(&mut stream, 404, "Not Found");
            return;
        }
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}

fn write_http_error(stream: &mut TcpStream, status: u16, reason: &str) {
    let body = b"not found";
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

fn unique_root(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-new-user-forge-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-new-user-forge-{label}-{}-{serial}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&endpoint);
    endpoint
}

fn run_git(args: &[&str]) {
    let output = ProcessCommand::new("git")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn create_bare_git_fixture(root: &Path) -> PathBuf {
    let work = root.join("work");
    std::fs::create_dir_all(&work).expect("create source fixture");
    let work_text = work.to_string_lossy().into_owned();
    run_git(&["init", "--quiet", "--initial-branch=main", &work_text]);
    run_git(&["-C", &work_text, "config", "user.name", "Forge Journey"]);
    run_git(&[
        "-C",
        &work_text,
        "config",
        "user.email",
        "forge-journey@example.test",
    ]);
    std::fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"forge-widget\"\nversion = \"1.2.3\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1\"\n",
    )
    .expect("write valid manifest");
    std::fs::create_dir_all(work.join("src")).expect("create source directory");
    std::fs::write(
        work.join("src/lib.rs"),
        "pub fn forge_value() -> u32 { 7 }\n",
    )
    .expect("write source file");
    std::fs::write(work.join("README.md"), "Forge fixture README.\n").expect("write README");
    run_git(&["-C", &work_text, "add", "."]);
    run_git(&["-C", &work_text, "commit", "--quiet", "-m", "valid source"]);

    run_git(&["-C", &work_text, "checkout", "--quiet", "-b", "malformed"]);
    std::fs::write(work.join("Cargo.toml"), "not = [valid TOML\n")
        .expect("write malformed manifest");
    run_git(&["-C", &work_text, "add", "Cargo.toml"]);
    run_git(&[
        "-C",
        &work_text,
        "commit",
        "--quiet",
        "-m",
        "malformed manifest",
    ]);

    run_git(&["-C", &work_text, "checkout", "--quiet", "main"]);
    run_git(&["-C", &work_text, "checkout", "--quiet", "-b", "oversized"]);
    std::fs::write(work.join("large.bin"), vec![0x7f; 16 * 1024])
        .expect("write oversized archive fixture");
    run_git(&["-C", &work_text, "add", "large.bin"]);
    run_git(&[
        "-C",
        &work_text,
        "commit",
        "--quiet",
        "-m",
        "oversized source",
    ]);

    let web_root = root.join("www");
    let bare = web_root.join("acme/widget");
    std::fs::create_dir_all(bare.parent().expect("bare parent")).expect("create bare parent");
    let bare_text = bare.to_string_lossy().into_owned();
    run_git(&["clone", "--quiet", "--bare", &work_text, &bare_text]);
    run_git(&["--git-dir", &bare_text, "update-server-info"]);
    web_root.canonicalize().expect("canonical git HTTP root")
}

fn wait_for_socket(endpoint: &Path, child: &mut ChildGuard) {
    let end = Instant::now() + surface_matrix::READY_DEADLINE;
    while Instant::now() < end {
        assert!(child.running(), "locald exited before readiness");
        if let Ok(metadata) = std::fs::symlink_metadata(endpoint)
            && metadata.file_type().is_socket()
            && UnixStream::connect(endpoint).is_ok()
        {
            return;
        }
        thread::sleep(CHILD_POLL);
    }
    panic!("timed out waiting for locald socket {}", endpoint.display());
}

fn locald_args(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    offline: bool,
    low_limit: bool,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--endpoint"),
        endpoint.as_os_str().to_owned(),
        OsString::from("--workspace"),
        workspace.as_os_str().to_owned(),
        OsString::from("--profile"),
        OsString::from("builtin"),
        OsString::from("--authority-secret-file"),
        authority.as_os_str().to_owned(),
        OsString::from("--idle-timeout-ms"),
        OsString::from("0"),
    ];
    if offline {
        args.push(OsString::from("--forge-offline"));
    }
    if low_limit {
        args.extend([
            OsString::from("--forge-max-archive-bytes"),
            OsString::from("1024"),
        ]);
    }
    args
}

fn launch(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    offline: bool,
    low_limit: bool,
) -> ChildGuard {
    let args = locald_args(endpoint, workspace, authority, offline, low_limit);
    let mut child = ChildGuard::spawn(&args);
    wait_for_socket(endpoint, &mut child);
    child
}

fn cli(endpoint: &Path, workspace: &Path, project: &Path, words: &[String]) -> Output {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .arg("--format")
        .arg("json")
        .args(words)
        .env("NO_COLOR", "1")
        .env("COLUMNS", "100");
    surface_matrix::run_bounded(command, &format!("CLI {words:?}"), None)
}

fn mcp_reference(endpoint: &Path, workspace: &Path, authority: &Path, coordinate: &str) -> Value {
    let project = workspace.with_file_name("mcp-empty-project");
    std::fs::create_dir_all(&project).expect("create empty MCP project");
    let input = [
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"forge-journey","version":"1"}}
        })
        .to_string(),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})
            .to_string(),
        json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{"name":"backend.surface","arguments":{
                "detail":"full",
                "command":{"operation":"forge-reference","coordinate":coordinate}
            }}
        })
        .to_string(),
    ]
    .join("\n")
        + "\n";
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority);
    let output =
        surface_matrix::run_bounded(command, "MCP forge-reference", Some(input.into_bytes()));
    assert!(
        output.status.success(),
        "MCP forge-reference failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value["id"] == 2)
        .unwrap_or_else(|| panic!("MCP omitted forge-reference result: {output:?}"))
}

fn cli_record(output: &Output, label: &str) -> Value {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} printed no JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["answer"], "product", "{label} answer: {value}");
    let record = value["records"][0]["forge"].clone();
    assert!(
        record.is_object(),
        "{label} omitted full forge facts: {value}"
    );
    record
}

fn assert_rejected(output: Output, label: &str, expected: &str) {
    assert!(!output.status.success(), "{label} unexpectedly succeeded");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} printed no JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(
        value["answer"], "fault",
        "{label} was not a typed fault: {value}"
    );
    assert!(
        value["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(expected)),
        "{label} omitted typed rejection {expected}: {value}"
    );
}

#[test]
fn forge_add_survives_sigkill_and_offline_cli_mcp_reference() {
    let root = unique_root("restart");
    let web_root = create_bare_git_fixture(&root);
    let mut fixture = HttpFixture::start(web_root);
    let coordinate = format!("{}@branch:main", fixture.base_url());
    let malformed = format!("{}@branch:malformed", fixture.base_url());
    let oversized = format!("{}@branch:oversized", fixture.base_url());
    let project = root.join("empty-project");
    std::fs::create_dir_all(&project).expect("create empty CLI project");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("restart");

    let mut daemon = launch(&endpoint, &workspace, &authority, false, false);
    let added = cli(
        &endpoint,
        &workspace,
        &project,
        &["forge-add".to_owned(), coordinate.clone()],
    );
    let added_record = cli_record(&added, "forge-add");
    assert!(
        fixture.request_count() > 0,
        "forge-add made no loopback HTTP requests"
    );
    assert_eq!(added_record["provider"], "generic-https-git");
    assert!(added_record["commit"]["value"].as_str().is_some());
    assert!(
        added_record["manifests"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );

    let malformed_output = cli(
        &endpoint,
        &workspace,
        &project,
        &["forge-add".to_owned(), malformed],
    );
    assert_rejected(malformed_output, "malformed manifest", "Manifest");
    let after_malformed = cli(
        &endpoint,
        &workspace,
        &project,
        &["forge-reference".to_owned(), coordinate.clone()],
    );
    assert_eq!(
        cli_record(&after_malformed, "reference after malformed source"),
        added_record
    );

    daemon.kill_now();
    let mut daemon = launch(&endpoint, &workspace, &authority, false, true);
    let oversized_output = cli(
        &endpoint,
        &workspace,
        &project,
        &["forge-add".to_owned(), oversized],
    );
    assert_rejected(oversized_output, "oversized archive", "Bounds");
    let after_oversized = cli(
        &endpoint,
        &workspace,
        &project,
        &["forge-reference".to_owned(), coordinate.clone()],
    );
    assert_eq!(
        cli_record(&after_oversized, "reference after oversized source"),
        added_record
    );

    daemon.kill_now();
    let requests_before_offline = fixture.request_count();
    let mut daemon = launch(&endpoint, &workspace, &authority, true, false);
    let cli_reference = cli(
        &endpoint,
        &workspace,
        &project,
        &["forge-reference".to_owned(), coordinate.clone()],
    );
    let cli_record = cli_record(&cli_reference, "offline CLI forge-reference");
    let mcp = mcp_reference(&endpoint, &workspace, &authority, &coordinate);
    assert_eq!(
        mcp["result"]["isError"], false,
        "MCP forge-reference failed: {mcp}"
    );
    let mcp_record = &mcp["result"]["structuredContent"]["surface"]["data"];
    assert_eq!(
        cli_record, *mcp_record,
        "CLI and MCP forge reference facts drifted"
    );
    assert_eq!(
        cli_record, added_record,
        "cold reopen changed the acquired forge record"
    );
    assert_eq!(
        fixture.request_count(),
        requests_before_offline,
        "offline reference made a network request"
    );

    daemon.kill_now();
    drop(fixture);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
