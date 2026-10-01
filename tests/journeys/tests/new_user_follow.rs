//! One new-user conversation through subscriptions, project folders, lockfile
//! sync, and session-tree close on the real daemon/CLI path.
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
        "backend-new-user-follow-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-new-user-follow-{label}-{}-{serial}.sock",
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

fn product_operands(value: &Value) -> Vec<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["operand"].as_str().map(str::to_owned))
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

fn all_product_tags(value: &Value) -> Vec<String> {
    product_tags(value).into_iter().flatten().collect()
}

fn scalar_note(value: &Value) -> &str {
    value["note"]
        .as_str()
        .expect("scalar product answer omitted note")
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

fn indexed_app_operand(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    indexed_path: &str,
) -> String {
    let package = cli_json(
        endpoint,
        workspace,
        project,
        &["package".to_owned(), indexed_path.to_owned()],
    );
    assert_eq!(package["answer"], "product");
    assert_eq!(
        product_titles(&package),
        vec!["new-user-app 0.1.0".to_owned()],
        "package did not read the app name and version from its indexed manifest: {package}"
    );
    package["records"]
        .as_array()
        .and_then(|records| records.first())
        .and_then(|record| record["operand"].as_str())
        .expect("package did not return the indexed app operand")
        .to_owned()
}

fn subscription_names_package(value: &Value, purl: &str, manifest_name: &str) -> bool {
    let titles = product_titles(value);
    let operands = product_operands(value);
    titles.iter().any(|title| title == manifest_name || title == purl)
        || operands.iter().any(|operand| operand == purl)
}

fn project_record_by_name(value: &Value, name: &str) -> Value {
    value["records"]
        .as_array()
        .and_then(|records| {
            records
                .iter()
                .find(|record| record["title"].as_str() == Some(name))
                .cloned()
        })
        .unwrap_or_else(|| panic!("projects did not list {name:?}: {value}"))
}

fn write_lockfile(path: &Path, name: &str, version: &str) {
    std::fs::write(
        path,
        format!("[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n"),
    )
    .expect("write lockfile");
}

#[test]
#[allow(clippy::too_many_lines, reason = "one conversation exercises follow/close surfaces")]
fn new_user_follow_covers_subscriptions_project_folders_sync_and_tree_close() {
    let root = unique_root("follow");
    let project = fixture_app();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("follow");
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
    let helper_manifest_name = "journey-helper";

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
    let indexed_package = indexed_app_operand(&endpoint, &workspace, &project, indexed_path);

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &["subscribe".to_owned(), helper_v1.clone()],
    );
    let subscriptions = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["subscriptions".to_owned()],
    );
    assert_eq!(subscriptions["answer"], "product");
    assert!(
        subscription_names_package(&subscriptions, &helper_v1, helper_manifest_name),
        "subscriptions did not name the indexed helper package: {subscriptions}"
    );

    let first_unsubscribe = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["unsubscribe".to_owned(), helper_v1.clone()],
    );
    assert_eq!(first_unsubscribe["answer"], "product");
    assert_eq!(
        scalar_note(&first_unsubscribe),
        "the subscription was removed",
        "first unsubscribe did not report removal: {first_unsubscribe}"
    );

    let after_first_unsubscribe = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["subscriptions".to_owned()],
    );
    assert!(
        !subscription_names_package(
            &after_first_unsubscribe,
            &helper_v1,
            helper_manifest_name,
        ),
        "subscriptions still listed the helper after unsubscribe: {after_first_unsubscribe}"
    );

    let second_unsubscribe = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["unsubscribe".to_owned(), helper_v1.clone()],
    );
    assert_eq!(second_unsubscribe["answer"], "product");
    assert_eq!(
        scalar_note(&second_unsubscribe),
        "no subscription existed for that package",
        "second unsubscribe did not report absence: {second_unsubscribe}"
    );

    let project_folder = "graph-project";
    let created = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["project-create".to_owned(), project_folder.to_owned()],
    );
    assert_eq!(created["answer"], "product");
    let created_tags = all_product_tags(&created);
    let project_id = created_tags
        .iter()
        .find_map(|tag| tag.strip_prefix("id "))
        .expect("project-create did not tag the folder id");

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

    let after_add = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["projects".to_owned()],
    );
    assert_eq!(after_add["answer"], "product");
    let add_tags = all_product_tags(&after_add);
    assert!(
        add_tags.iter().any(|tag| tag == "member new-user-app"),
        "projects did not tag member new-user-app from the indexed app manifest: {after_add}"
    );

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "project-remove".to_owned(),
            project_folder.to_owned(),
            indexed_package.clone(),
        ],
    );

    let after_remove = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["projects".to_owned()],
    );
    assert_eq!(after_remove["answer"], "product");
    let remove_tags = all_product_tags(&after_remove);
    let removed_record = project_record_by_name(&after_remove, project_folder);
    assert_eq!(removed_record["title"], project_folder);
    assert!(
        !remove_tags.iter().any(|tag| tag == "member new-user-app"),
        "project-remove still tagged member new-user-app: {after_remove}"
    );
    assert!(
        remove_tags.iter().any(|tag| tag == "0 member(s)"),
        "project-remove did not report zero members: {after_remove}"
    );

    cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "subscribe".to_owned(),
            helper_v1.clone(),
            "--project".to_owned(),
            project_folder.to_owned(),
        ],
    );

    let deleted = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["project-delete".to_owned(), project_folder.to_owned()],
    );
    assert_eq!(deleted["answer"], "product");
    assert_eq!(
        scalar_note(&deleted),
        format!("project {project_id} was deleted"),
        "project-delete did not name the deleted folder id: {deleted}"
    );

    let projects_after_delete = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["projects".to_owned()],
    );
    assert_eq!(projects_after_delete["answer"], "product");
    assert!(
        projects_after_delete["records"]
            .as_array()
            .is_none_or(|records| {
                records
                    .iter()
                    .all(|record| record["title"].as_str() != Some(project_folder))
            }),
        "projects still listed graph-project after delete: {projects_after_delete}"
    );

    let subscriptions_after_delete = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["subscriptions".to_owned()],
    );
    assert_eq!(subscriptions_after_delete["answer"], "product");
    let subscription_tags = all_product_tags(&subscriptions_after_delete);
    assert!(
        subscription_names_package(
            &subscriptions_after_delete,
            &helper_v1,
            helper_manifest_name,
        ),
        "subscriptions did not keep the helper after project delete: {subscriptions_after_delete}"
    );
    assert!(
        !subscription_tags
            .iter()
            .any(|tag| tag == &format!("project {project_id}")),
        "subscriptions still tagged deleted project {project_id}: {subscriptions_after_delete}"
    );

    let resolve = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["resolve".to_owned(), "parse_config".to_owned()],
    );
    assert_eq!(resolve["answer"], "records");
    let parse_coordinate = resolve["records"]
        .as_array()
        .and_then(|records| {
            records.iter().find(|record| {
                record["identity"]["name"] == "parse_config"
                    && record["identity"]["path"] == "src/lib.rs"
            })
        })
        .and_then(|record| record["identity"]["coordinate"].as_str())
        .expect("resolve did not return parse_config at src/lib.rs")
        .to_owned();

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
    let tree_open_tags = all_product_tags(&tree_open);
    let node_id = tree_open_tags
        .iter()
        .find_map(|tag| tag.strip_prefix("node "))
        .expect("tree-open did not tag the opened node id");

    let tree_close = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["tree-close".to_owned(), node_id.to_owned()],
    );
    assert_eq!(tree_close["answer"], "product");
    assert_eq!(
        scalar_note(&tree_close),
        "1 node(s) closed",
        "tree-close did not report one closed node: {tree_close}"
    );

    let tree_after_close = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["tree".to_owned()],
    );
    assert_eq!(tree_after_close["answer"], "product");
    assert!(
        !all_product_tags(&tree_after_close)
            .iter()
            .any(|tag| tag == &format!("node {node_id}")),
        "tree still listed node {node_id} after tree-close: {tree_after_close}"
    );

    let missing_close = cli(
        &endpoint,
        &workspace,
        &project,
        &["tree-close".to_owned(), node_id.to_owned()],
    );
    assert!(
        !missing_close.status.success(),
        "tree-close of a missing node unexpectedly succeeded: stdout={} stderr={}",
        String::from_utf8_lossy(&missing_close.stdout),
        String::from_utf8_lossy(&missing_close.stderr)
    );

    let synced_lock_dir = root.join("synced-lock");
    std::fs::create_dir_all(&synced_lock_dir).expect("create synced lock directory");
    let helper_lock = synced_lock_dir.join("Cargo.lock");
    write_lockfile(&helper_lock, "journey-helper", "1.0.0");
    cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "project-create".to_owned(),
            "synced".to_owned(),
            "--lockfile".to_owned(),
            helper_lock.to_string_lossy().into_owned(),
        ],
    );
    let synced = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["project-sync".to_owned(), "synced".to_owned()],
    );
    assert_eq!(synced["answer"], "product");
    let helper_profile = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["package-profile".to_owned(), helper_v1.clone()],
    );
    assert_eq!(
        product_titles(&helper_profile),
        vec!["journey-helper 1.0.0".to_owned()],
        "package-profile did not read the helper manifest title: {helper_profile}"
    );
    let synced_tags = all_product_tags(&synced);
    assert!(
        synced_tags
            .iter()
            .any(|tag| tag == &format!("member {helper_manifest_name}")),
        "project-sync did not tag member {helper_manifest_name} from the indexed helper manifest: {synced}"
    );

    let missing_lock_dir = root.join("missing-lock");
    std::fs::create_dir_all(&missing_lock_dir).expect("create missing lock directory");
    let missing_lock = missing_lock_dir.join("Cargo.lock");
    write_lockfile(&missing_lock, "not-indexed", "9.9.9");
    cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "project-create".to_owned(),
            "missing-sync".to_owned(),
            "--lockfile".to_owned(),
            missing_lock.to_string_lossy().into_owned(),
        ],
    );
    let missing_sync_output = cli(
        &endpoint,
        &workspace,
        &project,
        &["project-sync".to_owned(), "missing-sync".to_owned()],
    );
    assert!(
        !missing_sync_output.status.success(),
        "project-sync unexpectedly succeeded for an unindexed lockfile package: stdout={} stderr={}",
        String::from_utf8_lossy(&missing_sync_output.stdout),
        String::from_utf8_lossy(&missing_sync_output.stderr)
    );
    let missing_sync: Value =
        serde_json::from_slice(&missing_sync_output.stdout).unwrap_or_else(|error| {
        panic!(
            "project-sync failure printed no JSON ({error}): {}",
            String::from_utf8_lossy(&missing_sync_output.stdout)
        )
    });
    assert_eq!(
        missing_sync["answer"],
        "fault",
        "project-sync must fail for an unindexed lockfile package: {missing_sync}"
    );
    let not_indexed_purl = "pkg:cargo/not-indexed@9.9.9";
    let not_indexed_sync_error =
        format!("project member {not_indexed_purl} is not indexed");
    let detail = missing_sync["detail"]
        .as_str()
        .expect("project-sync fault omitted detail");
    assert_eq!(
        detail,
        not_indexed_sync_error,
        "project-sync must report the indexed-manifest lookup miss, not another fault: {missing_sync}"
    );
    assert!(
        !missing_sync
            .get("records")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|record| {
                record["tags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .any(|tag| tag.starts_with("member "))
            }),
        "project-sync must not tag a member for an unindexed lockfile package: {missing_sync}"
    );

    eprintln!(
        "new-user-follow result=PASS helper={helper_v1:?} project_id={project_id:?} node_id={node_id:?} synced_tags={synced_tags:?}"
    );

    drop(daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
