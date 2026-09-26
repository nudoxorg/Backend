//! One new-user conversation through resolve, explore, package, and tree
//! navigation on the real daemon/CLI path.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/fake_registry.rs"]
mod fake_registry;
#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use fake_registry::{FakeFile, FakePackage, FakeRegistry};
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
const INDEX_DEADLINE: Duration = Duration::from_secs(150);
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

const HELPER_V1_FILES: &[FakeFile] = &[
    FakeFile {
        path: "Cargo.toml",
        contents: "[package]\nname = \"journey-helper\"\nversion = \"1.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    },
    FakeFile {
        path: "src/lib.rs",
        contents: "/// Shared helper utilities.\npub fn helper_value() -> u64 { 1 }\n",
    },
];

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
        "backend-new-user-resolve-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-new-user-resolve-{label}-{}-{serial}.sock",
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

fn launch(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    registry_endpoint: &str,
) -> ChildGuard {
    let mut args =
        surface_matrix::locald_args(endpoint, workspace, authority, Some(0), false);
    args.extend([
        OsString::from("--registry-endpoint"),
        OsString::from(registry_endpoint),
        OsString::from("--registry-ecosystem"),
        OsString::from("cargo"),
    ]);
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

fn cli_json(endpoint: &Path, workspace: &Path, project: &Path, words: &[String]) -> Value {
    let output = cli(endpoint, workspace, project, words);
    assert!(
        output.status.success(),
        "CLI {words:?} failed ({}): stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI {words:?} printed no JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn wait_for_index(endpoint: &Path, workspace: &Path, project: &Path) {
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let health = cli_json(
            endpoint,
            workspace,
            project,
            &["health".to_owned()],
        );
        if health["rows"].as_u64().unwrap_or(0) > 1 {
            return;
        }
        assert!(Instant::now() < end, "index never published rows: {health}");
        thread::sleep(Duration::from_millis(200));
    }
}

fn product_titles(value: &Value) -> Vec<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["title"].as_str().map(str::to_owned))
        .collect()
}

fn product_tags(value: &Value) -> Vec<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|record| {
            record["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn record_tag_sets(value: &Value) -> Vec<Vec<String>> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|record| {
            record["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
#[allow(clippy::too_many_lines, reason = "one conversation exercises resolve/explore/tree")]
fn new_user_resolve_explore_covers_resolve_explore_package_and_tree_navigation() {
    let root = unique_root("resolve-explore");
    let project = fixture_app();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("resolve-explore");
    let registry = FakeRegistry::start(
        "cargo",
        vec![FakePackage {
            name: "journey-helper".to_owned(),
            version: "1.0.0".to_owned(),
            files: HELPER_V1_FILES.to_vec(),
        }],
    );
    let app_package = "pkg:cargo/new-user-app@0.1.0";

    let daemon = launch(&endpoint, &workspace, &authority, registry.endpoint());

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &["add".to_owned(), project.to_string_lossy().into_owned()],
    );
    wait_for_index(&endpoint, &workspace, &project);

    let resolve = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["resolve".to_owned(), "parse_config".to_owned()],
    );
    assert_eq!(resolve["answer"], "records");
    let parse_resolve = resolve["records"]
        .as_array()
        .and_then(|records| {
            records.iter().find(|record| {
                record["identity"]["name"] == "parse_config"
                    && record["identity"]["path"] == "src/lib.rs"
            })
        })
        .expect("resolve did not return one parse_config record at src/lib.rs");
    let parse_coordinate = parse_resolve["identity"]["coordinate"]
        .as_str()
        .expect("parse_config coordinate")
        .to_owned();

    let explore = cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "explore".to_owned(),
            project.to_string_lossy().into_owned(),
            "--limit".to_owned(),
            "20".to_owned(),
        ],
    );
    assert_eq!(explore["answer"], "product");
    let explore_titles = product_titles(&explore);
    assert!(
        explore_titles
            .iter()
            .any(|title| title.starts_with("parse_config ")),
        "explore did not name parse_config from the indexed project: {explore}"
    );

    let package = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["package".to_owned(), app_package.to_owned()],
    );
    assert_eq!(package["answer"], "product");
    assert_eq!(
        product_titles(&package),
        vec!["new-user-app 0.1.0".to_owned()],
        "package did not read name and version from the indexed manifest: {package}"
    );
    assert_eq!(
        package["records"]
            .as_array()
            .and_then(|records| records.first())
            .and_then(|record| record["operand"].as_str()),
        Some(app_package),
        "package operand must be the requested purl: {package}"
    );

    let tree_open = cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "tree-open".to_owned(),
            "declaration".to_owned(),
            parse_coordinate.clone(),
        ],
    );
    assert_eq!(tree_open["answer"], "product");
    let tree_open_tags = product_tags(&tree_open);
    assert!(
        tree_open_tags.contains(&"name parse_config".to_owned()),
        "tree-open did not look up the declaration name from the index: {tree_open}"
    );
    assert!(
        tree_open_tags.contains(&"path src/lib.rs".to_owned()),
        "tree-open did not look up the declaration path from the index: {tree_open}"
    );

    let tree = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["tree".to_owned()],
    );
    assert_eq!(tree["answer"], "product");
    let tree_tag_sets = record_tag_sets(&tree);
    assert!(
        tree_tag_sets.iter().any(|tags| {
            tags.contains(&"name parse_config".to_owned())
                && tags.contains(&"path src/lib.rs".to_owned())
        }),
        "tree did not retain the indexed declaration lookup: {tree}"
    );

    eprintln!(
        "new-user-resolve-explore result=PASS resolve={parse_coordinate:?} explore={explore_titles:?} package={app_package:?} tree-open={tree_open_tags:?}"
    );

    drop(daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
