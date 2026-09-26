//! One new-user conversation through semantic versions, package profile,
//! releases, and registry index search on the real daemon/CLI path.
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
        contents: "/// Shared helper utilities (v2).\npub fn helper_value(level: u64) -> u64 { level }\n",
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
        "backend-new-user-versions-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-new-user-versions-{label}-{}-{serial}.sock",
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

fn product_tags(value: &Value) -> Vec<Vec<String>> {
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

fn record_by_title<'a>(value: &'a Value, title: &str) -> &'a Value {
    value["records"]
        .as_array()
        .and_then(|records| records.iter().find(|record| record["title"] == title))
        .unwrap_or_else(|| panic!("record with title {title:?} missing: {value}"))
}

#[test]
#[allow(clippy::too_many_lines, reason = "one conversation exercises version surfaces")]
fn new_user_versions_covers_semantic_versions_package_profile_releases_and_index_search() {
    let root = unique_root("versions");
    let project = fixture_app();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("versions");
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
    wait_for_index(&endpoint, &workspace, &project);

    let semantic = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["semantic-versions".to_owned(), helper_v1.clone()],
    );
    assert_eq!(semantic["answer"], "product");
    let semantic_tags = product_tags(&semantic);
    assert!(
        semantic_tags.iter().any(|tags| tags.iter().any(|tag| tag == "selected")),
        "semantic-versions did not mark the indexed helper generation selected: {semantic}"
    );
    assert!(
        semantic_tags
            .iter()
            .any(|tags| tags.iter().any(|tag| tag == "version 1.0.0")),
        "semantic-versions did not name the indexed helper release 1.0.0: {semantic}"
    );
    assert!(
        !semantic_tags
            .iter()
            .any(|tags| tags.iter().any(|tag| tag == "version 2.0.0")),
        "semantic-versions must not treat the unindexed registry feed release as indexed: {semantic}"
    );

    let helper_profile = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["package-profile".to_owned(), helper_v1.clone()],
    );
    assert_eq!(helper_profile["answer"], "product");
    assert_eq!(
        product_titles(&helper_profile),
        vec!["journey-helper 1.0.0".to_owned()],
        "package-profile did not read the helper name and version from its indexed manifest: {helper_profile}"
    );
    let helper_profile_tags = product_tags(&helper_profile);
    assert!(
        helper_profile_tags
            .iter()
            .any(|tags| tags.iter().any(|tag| tag == "1 version(s)")),
        "package-profile did not count indexed helper versions from the index: {helper_profile}"
    );
    assert_ne!(
        helper_profile["records"]
            .as_array()
            .and_then(|records| records.first())
            .and_then(|record| record["title"].as_str()),
        Some(helper_v1.as_str()),
        "package-profile must not echo the query purl as the only answer: {helper_profile}"
    );

    let app_profile = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["package-profile".to_owned(), app_package.to_owned()],
    );
    assert_eq!(app_profile["answer"], "product");
    assert_eq!(
        product_titles(&app_profile),
        vec!["new-user-app 0.1.0".to_owned()],
        "package-profile did not read the app name and version from its indexed manifest: {app_profile}"
    );

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &["subscribe".to_owned(), helper_v1.clone()],
    );
    let releases = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["releases".to_owned()],
    );
    assert_eq!(releases["answer"], "product");
    let release_titles = product_titles(&releases);
    assert!(
        release_titles
            .iter()
            .any(|title| title == "journey-helper 1.0.0"),
        "releases did not list the indexed helper release 1.0.0: {releases}"
    );
    assert!(
        !release_titles
            .iter()
            .any(|title| title.ends_with(" 2.0.0")),
        "releases must not list the unindexed registry feed release 2.0.0: {releases}"
    );

    let index_parse = cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "index-search".to_owned(),
            "parse_config".to_owned(),
            "--limit".to_owned(),
            "20".to_owned(),
        ],
    );
    assert_eq!(index_parse["answer"], "product");
    let parse_record = record_by_title(&index_parse, "parse_config src/lib.rs");
    assert!(
        parse_record["operand"]
            .as_str()
            .is_some_and(|operand| operand.contains("parse_config")),
        "index-search must return the indexed declaration coordinate as operand: {index_parse}"
    );
    let index_titles = product_titles(&index_parse);
    assert!(
        index_titles.iter().any(|title| title == "parse_config src/lib.rs"),
        "index-search did not return parse_config at src/lib.rs: {index_parse}"
    );
    assert!(
        index_titles.len() > 1
            || !index_titles
                .iter()
                .any(|title| title.starts_with("decoy_mention ")),
        "index-search must not return only the comment decoy for parse_config: {index_parse}"
    );

    let index_concern = cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "index-search".to_owned(),
            "error handling".to_owned(),
            "--limit".to_owned(),
            "20".to_owned(),
        ],
    );
    assert_eq!(index_concern["answer"], "product");
    let concern_titles = product_titles(&index_concern);
    assert!(
        concern_titles
            .iter()
            .any(|title| title == "parse_config src/lib.rs"),
        "index-search `error handling` did not return parse_config at src/lib.rs: {index_concern}"
    );
    assert!(
        concern_titles.len() > 1
            || !concern_titles
                .iter()
                .any(|title| title.starts_with("decoy_mention ")),
        "index-search `error handling` must not return only the comment decoy: {index_concern}"
    );

    eprintln!(
        "new-user-versions result=PASS semantic={semantic_tags:?} helper_profile={helper_profile_tags:?} releases={release_titles:?} index_parse={index_titles:?} index_concern={concern_titles:?}"
    );

    drop(daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
