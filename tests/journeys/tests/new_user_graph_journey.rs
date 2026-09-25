//! One new-user conversation through the real daemon, CLI, and MCP surfaces.
//!
//! The flow mirrors what an agent does on day one: index a project, inspect a
//! declaration with graph queries, judge in-package impact after a change,
//! search by concern rather than identifier, add another package, and diff two
//! indexed labels before an upgrade.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/fake_registry.rs"]
mod fake_registry;
#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use fake_registry::{FakeFile, FakePackage, FakeRegistry};
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
        "backend-new-user-graph-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-new-user-graph-{label}-{}-{serial}.sock",
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

fn mcp(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority: &Path,
    calls: &[(u64, &'static str, Value)],
) -> BTreeMap<u64, Value> {
    let mut lines = vec![
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "new-user-graph-journey", "version": "1" }
            }
        })
        .to_string(),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}).to_string(),
    ];
    lines.extend(calls.iter().map(|(id, method, params)| {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
    }));
    let mut input = lines.join("\n").into_bytes();
    input.push(b'\n');
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority);
    let output = surface_matrix::run_bounded(command, "MCP new-user-graph journey", Some(input));
    assert!(
        output.status.success(),
        "MCP failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("MCP stdout UTF-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| value["id"].as_u64().map(|id| (id, value)))
        .collect()
}

fn assert_mcp_ok(reply: &Value, label: &str) {
    assert!(
        reply.get("error").is_none(),
        "MCP {label} protocol error: {reply}"
    );
    assert_eq!(
        reply["result"]["isError"], false,
        "MCP {label} typed error: {reply}"
    );
}

#[derive(Debug)]
struct TokenDigest {
    label: &'static str,
    bytes: usize,
    tokens: usize,
}

fn digest_reply(label: &'static str, reply: &Value) -> TokenDigest {
    let bytes = serde_json::to_vec(reply).expect("serialize reply").len();
    let tokens = backend_present::estimate_tokens(bytes);
    eprintln!("new-user-graph-journey step={label} bytes={bytes} tokens={tokens}");
    TokenDigest { label, bytes, tokens }
}

fn assert_nonempty_reply(digest: &TokenDigest, structured: &Value) {
    assert!(digest.bytes > 0, "{digest:?} serialized to zero bytes");
    assert!(digest.tokens > 0, "{digest:?} estimated zero tokens");
    assert!(
        !structured.is_null(),
        "{} returned null structured content",
        digest.label
    );
    match structured["answer"].as_str() {
        Some("records") => {
            let records = structured["records"].as_array().expect("records array");
            assert!(
                !records.is_empty() || structured.get("empty_reason").is_some(),
                "{} returned an empty success without a reason: {structured}",
                digest.label
            );
        }
        Some("page") => {
            assert!(
                structured.get("identity").is_some() || structured.get("package").is_some(),
                "{} page omitted identity: {structured}",
                digest.label
            );
        }
        Some("product") => {
            let records = structured["records"]
                .as_array()
                .filter(|records| !records.is_empty());
            let fault = structured.get("fault");
            assert!(
                records.is_some() || fault.is_some(),
                "{} product answer was empty without a fault: {structured}",
                digest.label
            );
        }
        Some("status") => {
            assert!(
                structured.get("revision").is_some(),
                "{} status omitted revision: {structured}",
                digest.label
            );
        }
        other => panic!("{} returned unexpected answer kind {other:?}: {structured}", digest.label),
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

fn record_names(value: &Value) -> Vec<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["identity"]["name"].as_str().map(str::to_owned))
        .collect()
}

fn product_record_tags(value: &Value, title: &str) -> Vec<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|record| record["title"] == title)
        .and_then(|record| record["tags"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn relation_names_by_label(page: &Value, label: &str) -> Vec<String> {
    page["relations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|group| group["label"] == label)
        .and_then(|group| group["relations"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|relation| relation["name"].as_str().map(str::to_owned))
        .collect()
}

#[test]
#[allow(clippy::too_many_lines, reason = "one conversation exercises the full graph journey")]
fn new_user_graph_journey_covers_index_graph_impact_search_add_and_upgrade_diff() {
    let root = unique_root("journey");
    let project = fixture_app();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("journey");
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

    let daemon = launch(&endpoint, &workspace, &authority, registry.endpoint());

    // 1. Index the local project, then add the first dependency release.
    let index_app = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["add".to_owned(), project.to_string_lossy().into_owned()],
    );
    let index_app_digest = digest_reply("index-app", &index_app);
    assert_eq!(index_app["answer"], "product");
    assert_nonempty_reply(&index_app_digest, &index_app);
    wait_for_index(&endpoint, &workspace, &project);

    let add_helper_v1 = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["add".to_owned(), helper_v1.clone()],
    );
    let add_v1_digest = digest_reply("add-helper-v1", &add_helper_v1);
    assert_eq!(add_helper_v1["answer"], "product");
    assert_nonempty_reply(&add_v1_digest, &add_helper_v1);
    wait_for_registry_symbol(&endpoint, &workspace, &project, "helper_value");

    let parse_config = wait_for_search(&endpoint, &workspace, &project, "parse_config", "parse_config");
    let run_app = wait_for_search(&endpoint, &workspace, &project, "run_app", "run_app");
    let parse_coordinate = parse_config["identity"]["coordinate"]
        .as_str()
        .expect("parse_config coordinate")
        .to_owned();
    let run_coordinate = run_app["identity"]["coordinate"]
        .as_str()
        .expect("run_app coordinate")
        .to_owned();

    // 2. Graph and document queries show what the declaration is and where it lives.
    let show_parse = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["show".to_owned(), parse_coordinate.clone()],
    );
    let show_digest = digest_reply("show-parse-config", &show_parse);
    assert_eq!(show_parse["answer"], "page");
    assert_eq!(show_parse["identity"]["name"], "parse_config");
    assert_eq!(show_parse["identity"]["path"], "src/lib.rs");
    assert_nonempty_reply(&show_digest, &show_parse);
    let callers = relation_names_by_label(&show_parse, "called by");
    assert_eq!(
        callers,
        vec!["run_app".to_owned()],
        "show on parse_config did not carry exactly one Calls edge from run_app: {show_parse}"
    );
    assert!(
        !callers.iter().any(|name| name == "decoy_mention"),
        "comment/string mention of parse_config( must not become a Calls edge: {show_parse}"
    );

    let graph_run = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["graph".to_owned(), run_coordinate.clone()],
    );
    let graph_digest = digest_reply("graph-run-app", &graph_run);
    assert_nonempty_reply(&graph_digest, &graph_run);
    assert!(
        record_names(&graph_run).iter().any(|name| name == "parse_config"),
        "graph of run_app did not reach parse_config: {graph_run}"
    );

    // 3. In-package impact reaches the caller through a typed call edge.
    let related_parse = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["related".to_owned(), parse_coordinate.clone()],
    );
    let related_digest = digest_reply("related-parse-config", &related_parse);
    assert_nonempty_reply(&related_digest, &related_parse);
    assert!(
        record_names(&related_parse).iter().any(|name| name == "run_app"),
        "related on parse_config did not reach caller run_app: {related_parse}"
    );

    // 4. A concern-shaped where-is query finds the error-handling declaration.
    let error_search = cli_json(
        &endpoint,
        &workspace,
        &project,
        &[
            "search".to_owned(),
            "error handling".to_owned(),
            "--limit".to_owned(),
            "20".to_owned(),
        ],
    );
    let error_digest = digest_reply("search-error-handling", &error_search);
    assert_nonempty_reply(&error_digest, &error_search);
    assert!(
        record_names(&error_search).iter().any(|name| name == "parse_config"),
        "search `error handling` did not name parse_config: {error_search}"
    );

    // 5–6. Add the newer release, then diff the two indexed labels.
    let replies = mcp(
        &endpoint,
        &workspace,
        &project,
        &authority,
        &[
            (
                10,
                "tools/call",
                json!({"name":"backend.index","arguments":{"path": helper_v2}}),
            ),
            (
                11,
                "tools/call",
                json!({
                    "name":"backend.diff",
                    "arguments":{"from": helper_v1, "to": helper_v2}
                }),
            ),
        ],
    );
    let add_v2_reply = &replies[&10];
    assert_mcp_ok(add_v2_reply, "add-helper-v2");
    let add_v2_structured = surface_matrix::mcp_structured(add_v2_reply);
    let add_v2_digest = digest_reply("mcp-add-helper-v2", add_v2_reply);
    assert_nonempty_reply(&add_v2_digest, add_v2_structured);
    wait_for_registry_symbol(&endpoint, &workspace, &project, "helper_bonus");

    let diff_reply = &replies[&11];
    assert_mcp_ok(diff_reply, "diff-helper");
    let diff_structured = surface_matrix::mcp_structured(diff_reply);
    let diff_digest = digest_reply("mcp-diff-helper", diff_reply);
    assert_eq!(diff_structured["answer"], "product");
    assert_nonempty_reply(&diff_digest, diff_structured);
    assert!(
        product_record_tags(diff_structured, "helper_value").contains(&"changed".to_owned()),
        "upgrade diff did not mark helper_value changed between {helper_v1} and {helper_v2}: {diff_structured}"
    );
    assert!(
        product_record_tags(diff_structured, "helper_bonus").contains(&"added".to_owned()),
        "upgrade diff did not mark helper_bonus added between {helper_v1} and {helper_v2}: {diff_structured}"
    );

    let token_digests = [
        index_app_digest,
        add_v1_digest,
        show_digest,
        graph_digest,
        related_digest,
        error_digest,
        add_v2_digest,
        diff_digest,
    ];
    for digest in &token_digests {
        assert!(digest.tokens >= 1, "token digest missing for {}", digest.label);
    }
    let total_tokens = token_digests.iter().map(|digest| digest.tokens).sum::<usize>();
    assert!(
        total_tokens >= token_digests.len(),
        "conversation token total regressed: {total_tokens}"
    );

    eprintln!(
        "new-user-graph-journey result=PASS steps={} total_tokens={}",
        token_digests.len(),
        total_tokens
    );

    drop(daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
