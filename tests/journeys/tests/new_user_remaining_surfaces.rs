//! One new-user conversation through the declaration surfaces the graph journey
//! does not exercise: read, references, outline, source, document, and the
//! package dependency graph.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/fake_registry.rs"]
mod fake_registry;
#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use fake_registry::{FakeFile, FakePackage, FakeRegistry};
use serde_json::Value;
use std::collections::BTreeSet;
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
        "backend-new-user-remaining-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-new-user-remaining-{label}-{}-{serial}.sock",
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

fn wait_for_search(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    query: &str,
    name: &str,
) -> Value {
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let search = cli_json(
            endpoint,
            workspace,
            project,
            &[
                "search".to_owned(),
                query.to_owned(),
                "--limit".to_owned(),
                "20".to_owned(),
            ],
        );
        if let Some(record) = search["records"]
            .as_array()
            .and_then(|records| {
                records.iter().find(|record| record["identity"]["name"] == name)
            })
        {
            return record.clone();
        }
        assert!(
            Instant::now() < end,
            "search `{query}` never found `{name}`: {search}"
        );
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

fn outline_names(value: &Value) -> BTreeSet<String> {
    fn walk(node: &Value, names: &mut BTreeSet<String>) {
        if let Some(name) = node["name"].as_str() {
            names.insert(name.to_owned());
        }
        if let Some(children) = node["children"].as_array() {
            for child in children {
                walk(child, names);
            }
        }
    }
    let mut names = BTreeSet::new();
    for root in value["roots"].as_array().unwrap_or(&Vec::new()) {
        walk(root, &mut names);
    }
    names
}

#[test]
#[allow(clippy::too_many_lines, reason = "one conversation exercises the remaining surfaces")]
fn new_user_remaining_surfaces_cover_read_references_outline_source_document_and_dependencies() {
    let root = unique_root("remaining");
    let project = fixture_app();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("remaining");
    let registry = FakeRegistry::start(
        "cargo",
        vec![FakePackage {
            name: "journey-helper".to_owned(),
            version: "1.0.0".to_owned(),
            files: HELPER_V1_FILES.to_vec(),
        }],
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
    wait_for_registry_symbol(&endpoint, &workspace, &project, "helper_value");

    let parse_config = wait_for_search(&endpoint, &workspace, &project, "parse_config", "parse_config");
    let parse_coordinate = parse_config["identity"]["coordinate"]
        .as_str()
        .expect("parse_config coordinate")
        .to_owned();

    let read = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["read".to_owned(), parse_coordinate.clone()],
    );
    assert_eq!(read["answer"], "product");
    let read_text = read["records"]
        .as_array()
        .and_then(|records| records.first())
        .and_then(|record| record["title"].as_str())
        .expect("read returned no declaration text");
    assert!(
        read_text.contains("Ok(\"configured\".to_string())"),
        "read did not return the parse_config function body: {read}"
    );

    let references = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["references".to_owned(), parse_coordinate.clone()],
    );
    assert_eq!(references["answer"], "product");
    let reference_titles = product_titles(&references);
    assert_eq!(
        reference_titles.len(),
        1,
        "references on parse_config did not name exactly one Calls site: {references}"
    );
    assert!(
        reference_titles[0].ends_with("::run_app"),
        "references on parse_config did not name run_app as the caller: {references}"
    );
    let reference_tags = product_tags(&references);
    assert!(
        reference_tags.iter().any(|tag| tag == "calls"),
        "references must carry the Calls relation kind, not substring mention: {references}"
    );
    assert!(
        !reference_titles.iter().any(|name| name.ends_with("::decoy_mention")),
        "comment/string mention must not become a reference: {references}"
    );

    let outline = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["outline".to_owned(), project.to_string_lossy().into_owned()],
    );
    assert_eq!(outline["answer"], "outline");
    let outline_names = outline_names(&outline);
    assert!(
        outline_names.contains("parse_config"),
        "outline omitted parse_config: {outline}"
    );
    assert!(
        outline_names.contains("run_app"),
        "outline omitted run_app: {outline}"
    );

    let source = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["source".to_owned(), parse_coordinate.clone()],
    );
    assert_eq!(source["answer"], "page");
    assert_eq!(
        source["identity"]["path"].as_str(),
        Some("src/lib.rs"),
        "source did not name the file containing parse_config: {source}"
    );

    let document = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["show".to_owned(), parse_coordinate.clone()],
    );
    assert_eq!(document["answer"], "page");
    let prose = document["prose"]
        .as_array()
        .map(|paragraphs| {
            paragraphs
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    assert!(
        prose.contains("error handling"),
        "document did not return the parse_config doc comment: {document}"
    );

    let dependencies = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["dependencies".to_owned(), app_package.to_owned()],
    );
    assert_eq!(dependencies["answer"], "product");
    assert!(
        product_titles(&dependencies)
            .iter()
            .any(|title| title.contains("journey-helper")),
        "dependencies on the app did not name journey-helper: {dependencies}"
    );

    let dependents = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["dependents".to_owned(), helper_v1.clone()],
    );
    assert_eq!(dependents["answer"], "product");
    let dependent_titles = product_titles(&dependents);
    assert!(
        dependent_titles.iter().any(|title| title.contains("new-user-app")),
        "dependents on journey-helper did not name new-user-app: {dependents}"
    );

    eprintln!(
        "new-user-remaining-surfaces result=PASS read={read_text:?} references={reference_titles:?} dependents={dependent_titles:?}"
    );

    drop(daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
