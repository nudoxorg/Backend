//! One new-user conversation through advisory, semantic version selection, and
//! project folders on the real daemon/CLI path.
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
        "backend-new-user-projects-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-new-user-projects-{label}-{}-{serial}.sock",
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

fn shelf_app_entry(endpoint: &Path, workspace: &Path, project: &Path) -> Value {
    let packages = cli_json(endpoint, workspace, project, &["packages".to_owned()]);
    assert_eq!(packages["answer"], "shelf");
    packages["projects"]
        .as_array()
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry["identity"]["project"]
                    .as_str()
                    .is_some_and(|path| Path::new(path).is_dir())
            })
        })
        .cloned()
        .expect("packages did not list the indexed app project root")
}

fn indexed_app_identity(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    indexed_path: &str,
) -> (String, String) {
    let package = cli_json(
        endpoint,
        workspace,
        project,
        &["package".to_owned(), indexed_path.to_owned()],
    );
    assert_eq!(package["answer"], "product");
    let record = package["records"]
        .as_array()
        .and_then(|records| records.first())
        .expect("package did not return the indexed app record");
    let operand = record["operand"]
        .as_str()
        .expect("package did not return the indexed app operand")
        .to_owned();
    let manifest_name = record["title"]
        .as_str()
        .and_then(|title| title.split_once(' ').map(|(name, _)| name.to_owned()))
        .expect("package did not read the indexed manifest name from Cargo.toml");
    (operand, manifest_name)
}

#[test]
#[allow(clippy::too_many_lines, reason = "one conversation exercises projects surfaces")]
fn new_user_projects_covers_advisory_semantic_selection_and_project_folders() {
    let root = unique_root("projects");
    let project = fixture_app();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("projects");
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

    let shelf = shelf_app_entry(&endpoint, &workspace, &project);
    let indexed_path = shelf["identity"]["project"]
        .as_str()
        .expect("indexed app shelf entry omitted project path");
    let (indexed_package, indexed_manifest_name) =
        indexed_app_identity(&endpoint, &workspace, &project, indexed_path);

    let advisory = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["advisory".to_owned(), helper_v1.clone()],
    );
    assert_eq!(advisory["answer"], "product");
    let advisory_titles = product_titles(&advisory);
    assert!(
        advisory_titles.iter().any(|title| title == "security decision"),
        "advisory did not return a typed security decision: {advisory}"
    );
    assert!(
        advisory_titles.iter().any(|title| title == "no matching advisory object"),
        "advisory must name the explicit empty reason when no CVE matches: {advisory}"
    );

    let semantic = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["semantic-versions".to_owned(), helper_v1.clone()],
    );
    assert_eq!(semantic["answer"], "product");
    let semantic_tags = product_tags(&semantic);
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
    assert!(
        semantic_tags
            .iter()
            .any(|tags| tags.iter().any(|tag| tag == "selected")),
        "semantic-versions did not mark the indexed helper generation selected: {semantic}"
    );
    let semantic_record = semantic["records"]
        .as_array()
        .and_then(|records| records.first())
        .expect("semantic-versions returned no rows");
    let generation = semantic_record["operand"]
        .as_str()
        .expect("semantic-versions omitted generation operand");
    let coordinate = semantic_record["title"]
        .as_str()
        .expect("semantic-versions omitted coordinate title");

    let selected = cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "select-semantic-version".to_owned(),
            helper_v1.clone(),
            coordinate.to_owned(),
            "rust".to_owned(),
            generation.to_owned(),
        ],
    );
    assert_eq!(selected["answer"], "product");
    let selected_tags = product_tags(&selected);
    assert!(
        selected_tags
            .iter()
            .any(|tags| tags.iter().any(|tag| tag == "version 1.0.0")),
        "select-semantic-version did not report the indexed helper release 1.0.0: {selected}"
    );
    assert!(
        !selected_tags
            .iter()
            .any(|tags| tags.iter().any(|tag| tag == "version 2.0.0")),
        "select-semantic-version must not select the unindexed registry feed release 2.0.0: {selected}"
    );
    assert!(
        selected_tags.iter().any(|tags| tags.iter().any(|tag| tag == "selected")),
        "select-semantic-version did not mark the generation selected: {selected}"
    );

    let empty_projects = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["projects".to_owned()],
    );
    assert_eq!(empty_projects["answer"], "product");
    assert!(
        empty_projects["records"].as_array().is_none_or(|records| records.is_empty()),
        "projects should start empty before a folder is created: {empty_projects}"
    );

    let project_folder = "graph-project";
    cli_json(
        &endpoint,
        &workspace,
        &project,
        &["project-create".to_owned(), project_folder.to_owned()],
    );
    cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "project-add".to_owned(),
            project_folder.to_owned(),
            indexed_package.clone(),
        ],
    );

    let projects = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["projects".to_owned()],
    );
    assert_eq!(projects["answer"], "product");
    let project_tags = product_tags(&projects);
    assert!(
        project_tags
            .iter()
            .any(|tags| tags.iter().any(|tag| tag == "1 member(s)")),
        "projects did not report the indexed app package as a member: {projects}"
    );
    assert!(
        project_tags
            .iter()
            .any(|tags| {
                tags.iter()
                    .any(|tag| tag == &format!("member {indexed_manifest_name}"))
            }),
        "projects did not name the indexed manifest package {indexed_manifest_name} from Cargo.toml: {projects}"
    );

    eprintln!(
        "new-user-projects result=PASS advisory={advisory_titles:?} semantic={semantic_tags:?} selected={selected_tags:?} projects={project_tags:?} indexed_path={indexed_path:?} indexed_package={indexed_package:?} indexed_manifest_name={indexed_manifest_name:?}"
    );

    drop(daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
