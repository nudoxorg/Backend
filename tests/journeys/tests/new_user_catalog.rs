//! One new-user conversation through catalog surfaces: packages, status, owner,
//! package versions, and remove on the real daemon/CLI path.
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

const HELPER_V2_FILES: &[FakeFile] = &[
    FakeFile {
        path: "Cargo.toml",
        contents: "[package]\nname = \"journey-helper\"\nversion = \"2.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    },
    FakeFile {
        path: "src/lib.rs",
        contents: "/// Shared helper utilities (v2).\npub fn helper_value(level: u64) -> u64 { level }\n\n/// Extra helper surface added in 2.0.0.\npub fn helper_bonus() -> u64 { 99 }\n",
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
        "backend-new-user-catalog-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-new-user-catalog-{label}-{}-{serial}.sock",
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

fn wait_for_registry_symbol(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    name: &str,
) -> String {
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let search = cli_json(
            endpoint,
            workspace,
            project,
            &[
                "search".to_owned(),
                name.to_owned(),
                "--limit".to_owned(),
                "20".to_owned(),
            ],
        );
        if let Some(coordinate) = search["records"]
            .as_array()
            .and_then(|records| {
                records
                    .iter()
                    .find(|record| record["identity"]["name"] == name)
                    .and_then(|record| record["identity"]["coordinate"].as_str())
            })
        {
            return coordinate.to_owned();
        }
        assert!(
            Instant::now() < end,
            "registry package never published `{name}`: {search}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

fn shelf_project_names(value: &Value) -> Vec<String> {
    value["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["identity"]["name"].as_str().map(str::to_owned))
        .collect()
}

fn product_titles(value: &Value) -> Vec<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["title"].as_str().map(str::to_owned))
        .collect()
}

fn product_versions(value: &Value) -> Vec<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| {
            record["title"]
                .as_str()
                .and_then(|title| title.rsplit_once(' ').map(|(_, version)| version))
                .map(str::to_owned)
        })
        .collect()
}

fn package_title_parts(title: &str) -> (&str, &str) {
    title
        .rsplit_once(' ')
        .expect("package title must separate manifest name and version")
}

fn wait_for_packages(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    expected: &[&str],
) -> Value {
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let packages = cli_json(
            endpoint,
            workspace,
            project,
            &["packages".to_owned()],
        );
        let names = shelf_project_names(&packages);
        if expected.iter().all(|name| names.iter().any(|listed| listed == name)) {
            return packages;
        }
        assert!(
            Instant::now() < end,
            "packages never listed every expected project {expected:?}: {packages}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[allow(clippy::too_many_lines, reason = "one conversation exercises catalog surfaces")]
fn new_user_catalog_covers_packages_status_owner_versions_and_remove() {
    let root = unique_root("catalog");
    let project = fixture_app();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("catalog");
    let registry = FakeRegistry::start(
        "cargo",
        vec![
            FakePackage {
                name: "journey-helper".to_owned(),
                version: "1.0.0".to_owned(),
                files: HELPER_V1_FILES.to_vec(),
            },
            FakePackage {
                name: "journey-helper".to_owned(),
                version: "2.0.0".to_owned(),
                files: HELPER_V2_FILES.to_vec(),
            },
        ],
    );
    let helper_v1 = fake_registry::canonical_purl("cargo", "journey-helper", "1.0.0");
    let helper_v2 = fake_registry::canonical_purl("cargo", "journey-helper", "2.0.0");
    let app_package = "pkg:cargo/new-user-app@0.1.0";

    let daemon = launch(&endpoint, &workspace, &authority, registry.endpoint());

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &["add".to_owned(), project.to_string_lossy().into_owned()],
    );
    wait_for_index(&endpoint, &workspace, &project);

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &["add".to_owned(), helper_v1.clone()],
    );
    wait_for_registry_symbol(&endpoint, &workspace, &project, "helper_value");

    let packages = wait_for_packages(
        &endpoint,
        &workspace,
        &project,
        &["new-user-app", "journey-helper"],
    );
    assert_eq!(packages["answer"], "shelf");
    let package_names = shelf_project_names(&packages);
    assert!(
        package_names.iter().any(|name| name == "new-user-app"),
        "packages did not name new-user-app from the indexed manifest: {packages}"
    );
    assert!(
        package_names.iter().any(|name| name == "journey-helper"),
        "packages did not name journey-helper from the indexed manifest: {packages}"
    );

    let status = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["health".to_owned()],
    );
    assert_eq!(status["answer"], "status");
    let rows = status["rows"]
        .as_u64()
        .expect("status must publish a row count");
    assert!(
        rows > 1,
        "status reported an empty index after fixtures were indexed: {status}"
    );
    let ready_project = packages["projects"]
        .as_array()
        .and_then(|projects| {
            projects.iter().find(|entry| {
                entry["identity"]["name"] == "new-user-app"
                    && entry["readiness"] == "ready"
                    && entry["identity"]["project"]
                        .as_str()
                        .is_some_and(|path| path == project.to_string_lossy())
            })
        })
        .expect("packages must carry a ready row for the indexed fixture project");
    assert_eq!(
        ready_project["identity"]["project"].as_str(),
        Some(project.to_str().expect("fixture path UTF-8")),
        "status shelf row must match the indexed fixture path: {ready_project}"
    );

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
    let indexed_path = parse_resolve["identity"]["path"]
        .as_str()
        .expect("parse_config path from index");
    let indexed_package = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["package".to_owned(), app_package.to_owned()],
    );
    let indexed_package_record = indexed_package["records"]
        .as_array()
        .and_then(|records| records.first())
        .expect("package must read manifest identity from the index");
    let indexed_package_title = indexed_package_record["title"]
        .as_str()
        .expect("indexed package title");
    let (expected_package_name, expected_package_version) =
        package_title_parts(indexed_package_title);

    let owner = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["owner".to_owned(), "parse_config".to_owned()],
    );
    assert_eq!(owner["answer"], "product");
    let owner_records = owner["records"]
        .as_array()
        .expect("owner must return records");
    let package_owner = owner_records
        .iter()
        .find(|record| {
            record["title"].as_str() == Some(indexed_package_title)
                && record["operand"].as_str() == indexed_package_record["operand"].as_str()
        })
        .expect("owner did not return the indexed package record for parse_config");
    assert_eq!(
        package_title_parts(
            package_owner["title"]
                .as_str()
                .expect("package owner title"),
        ),
        (expected_package_name, expected_package_version),
        "owner package name and version must come from the indexed manifest: {owner}"
    );
    let file_owner = owner_records
        .iter()
        .find(|record| record["title"].as_str() == Some(indexed_path))
        .expect("owner did not return the indexed file path for parse_config");
    assert_eq!(
        file_owner["title"].as_str(),
        Some(indexed_path),
        "file owner must be the indexed path without a package version: {owner}"
    );

    let versions_v1 = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["package-versions".to_owned(), helper_v1.clone()],
    );
    assert_eq!(versions_v1["answer"], "product");
    let listed_versions_v1 = product_versions(&versions_v1);
    assert!(
        listed_versions_v1.iter().any(|version| version == "1.0.0"),
        "package-versions did not list indexed 1.0.0 for journey-helper: {versions_v1}"
    );
    assert!(
        !listed_versions_v1.iter().any(|version| version == "2.0.0"),
        "package-versions listed catalog-only 2.0.0 before it was indexed: {versions_v1}"
    );

    let remove = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["remove".to_owned(), helper_v1.clone()],
    );
    assert_eq!(remove["answer"], "product");

    let after_remove = wait_for_packages(&endpoint, &workspace, &project, &["new-user-app"]);
    let remaining = shelf_project_names(&after_remove);
    assert!(
        remaining.iter().any(|name| name == "new-user-app"),
        "remove dropped the remaining indexed package new-user-app: {after_remove}"
    );
    assert!(
        !remaining.iter().any(|name| name == "journey-helper"),
        "remove left journey-helper on the shelf: {after_remove}"
    );

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &["add".to_owned(), helper_v1.clone()],
    );
    wait_for_registry_symbol(&endpoint, &workspace, &project, "helper_value");
    let restored = wait_for_packages(
        &endpoint,
        &workspace,
        &project,
        &["new-user-app", "journey-helper"],
    );
    assert!(
        shelf_project_names(&restored).iter().any(|name| name == "journey-helper"),
        "re-add did not restore journey-helper on the shelf: {restored}"
    );

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &["add".to_owned(), helper_v2.clone()],
    );
    wait_for_registry_symbol(&endpoint, &workspace, &project, "helper_bonus");
    let versions_both = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["package-versions".to_owned(), helper_v1.clone()],
    );
    assert_eq!(versions_both["answer"], "product");
    let listed_versions = product_versions(&versions_both);
    assert!(
        listed_versions.iter().any(|version| version == "1.0.0"),
        "package-versions did not list indexed 1.0.0 after re-add: {versions_both}"
    );
    assert!(
        listed_versions.iter().any(|version| version == "2.0.0"),
        "package-versions did not list indexed 2.0.0 after both were indexed: {versions_both}"
    );

    eprintln!(
        "new-user-catalog result=PASS packages={package_names:?} rows={rows} owner_package={expected_package_name}@{expected_package_version} owner_file={indexed_path} versions={listed_versions:?}"
    );

    drop(daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
