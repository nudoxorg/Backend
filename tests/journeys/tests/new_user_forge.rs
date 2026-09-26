//! Forge add and reference reject unconfigured acquisition authority on the
//! real daemon/CLI path.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use serde_json::Value;
use std::ffi::OsString;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const CHILD_POLL: Duration = Duration::from_millis(10);
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

const FORGE_COORDINATE: &str = "https://github.com/example/widget@tag:v1.0.0";
const FORGE_AUTHORITY_UNCONFIGURED: &str =
    "forge acquisition authority is not configured in this owner";

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
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().expect("poll locald during cleanup").is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
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

fn fixture_app() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/new_user_graph/app")
        .canonicalize()
        .expect("canonical new-user-app fixture")
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

fn launch(endpoint: &Path, workspace: &Path, authority: &Path) -> ChildGuard {
    let args = surface_matrix::locald_args(endpoint, workspace, authority, Some(0), false);
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

fn product_titles(value: &Value) -> Vec<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["title"].as_str().map(str::to_owned))
        .collect()
}

fn assert_forge_fault(output: Output, label: &str) {
    assert!(
        !output.status.success(),
        "{label} unexpectedly succeeded: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} printed no JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(
        value["answer"],
        "fault",
        "{label} must fail before presenting a forge package: {value}"
    );
    let detail = value["detail"]
        .as_str()
        .expect("forge fault omitted detail");
    assert_eq!(
        detail,
        FORGE_AUTHORITY_UNCONFIGURED,
        "{label} must report unconfigured forge acquisition authority: {value}"
    );
    assert!(
        !product_titles(&value).contains(&"unknown / unknown".to_owned()),
        "{label} must not present a stub forge package row: {value}"
    );
}

#[test]
fn new_user_forge_rejects_unconfigured_acquisition_authority() {
    let root = unique_root("forge");
    let project = fixture_app();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("forge");

    let daemon = launch(&endpoint, &workspace, &authority);

    let forge_add = cli(
        &endpoint,
        &workspace,
        &project,
        &["forge-add".to_owned(), FORGE_COORDINATE.to_owned()],
    );
    assert_forge_fault(forge_add, "forge-add");

    let forge_reference = cli(
        &endpoint,
        &workspace,
        &project,
        &["forge-reference".to_owned(), FORGE_COORDINATE.to_owned()],
    );
    assert_forge_fault(forge_reference, "forge-reference");

    eprintln!(
        "new-user-forge result=PASS coordinate={FORGE_COORDINATE:?} detail={FORGE_AUTHORITY_UNCONFIGURED:?}"
    );

    drop(daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
