//! One production-path matrix for every built-in source language.
//!
//! The fixture, request shapes, and assertions are shared across languages.
//! Each case is a real declaration retained by locald, and every surface is
//! compared by its immutable root and exact row identity.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use backend_client::Session;
use backend_library::{HealthReport, ViewStateRoot, encode_id};
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
            .env_remove("COMPILER_GO_COMPILER")
            .env_remove("NUDOX_GO_COMPILER")
            .env_remove("NUDOX_GO_ORACLE_BIN")
            .env_remove("NUDOX_GO")
            .env_remove("NUDOX_GO_ORACLE");
        // Keep the matrix independent of a host-specific Rust probe.  The
        // production built-in owner still publishes its typed structural
        // lanes and reports native semantic availability in health; injecting
        // a discovered compiler here can block the Add request while the
        // optional probe is being admitted.
        command.env_remove("NUDOX_RUSTC");
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

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    fn graceful_wait(&mut self, deadline: Duration) {
        let end = Instant::now() + deadline;
        while Instant::now() < end {
            if !self.running() {
                return;
            }
            thread::sleep(CHILD_POLL);
        }
        panic!("locald did not exit within {:?}", deadline);
    }

    fn kill_now(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        if child
            .try_wait()
            .expect("poll locald before SIGKILL")
            .is_none()
        {
            child.kill().expect("SIGKILL locald");
        }
        child.wait().expect("wait SIGKILL locald");
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

fn unique_root(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-surface-matrix-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-surface-matrix-{label}-{}-{serial}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&endpoint);
    endpoint
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

fn wait_for_socket_dead(endpoint: &Path, child: &mut ChildGuard) {
    let end = Instant::now() + surface_matrix::PROCESS_DEADLINE;
    while Instant::now() < end {
        if !child.running() && UnixStream::connect(endpoint).is_err() {
            return;
        }
        thread::sleep(CHILD_POLL);
    }
    panic!(
        "stale locald endpoint survived restart: {}",
        endpoint.display()
    );
}

fn rss_kib(child: &ChildGuard) -> Option<u64> {
    let pid = child.pid()?;
    let output = ProcessCommand::new("ps")
        .args(["-o", "rss=", "-p"])
        .arg(pid.to_string())
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn json_output(output: Output, label: &str) -> Value {
    assert!(
        output.status.success(),
        "{label} failed ({}):\n{}",
        output.status,
        format!(
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} did not produce JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
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
    json_output(
        cli(endpoint, workspace, project, words),
        &format!("CLI {words:?}"),
    )
}

fn cli_markdown(endpoint: &Path, workspace: &Path, project: &Path, words: &[String]) -> String {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .arg("--format")
        .arg("markdown")
        .args(words)
        .env("NO_COLOR", "1")
        .env("COLUMNS", "100");
    let output = surface_matrix::run_bounded(command, &format!("CLI markdown {words:?}"), None);
    assert!(
        output.status.success(),
        "CLI markdown {words:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("CLI markdown UTF-8")
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
                "clientInfo": { "name": "surface-matrix", "version": "1" }
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
    let output = surface_matrix::run_bounded(command, "MCP surface matrix", Some(input));
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

fn restart_health(endpoint: &Path, expected_root: ViewStateRoot) -> HealthReport {
    let mut session = Session::connect(endpoint).expect("connect restart health");
    let report = session.health().expect("read restart health");
    assert_eq!(
        report.revision().root(),
        expected_root,
        "restart root changed"
    );
    session.reconnect().expect("reconnect restart health");
    let reconnected = session.health().expect("read reconnected health");
    assert_eq!(
        reconnected.revision().root(),
        expected_root,
        "reconnect root changed"
    );
    report
}

fn launch(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    idle_timeout_ms: Option<u64>,
    offline: bool,
) -> ChildGuard {
    let args =
        surface_matrix::locald_args(endpoint, workspace, authority, idle_timeout_ms, offline);
    let mut child = ChildGuard::spawn(&args);
    wait_for_socket(endpoint, &mut child);
    child
}

fn assert_capability_truth(status: &Value, surface: &str) -> bool {
    let capabilities = &status["capabilities"];
    let ready = capabilities["frontends_ready"]
        .as_u64()
        .unwrap_or_else(|| panic!("status omitted frontend readiness: {status}"));
    let total = capabilities["frontends_total"]
        .as_u64()
        .unwrap_or_else(|| panic!("status omitted frontend total: {status}"));
    assert_eq!(
        total, 9,
        "built-in language profile set regressed: {status}"
    );
    assert_eq!(
        ready, total,
        "status claimed an incomplete frontend set: {status}"
    );
    let coverage = status["coverage"]
        .as_array()
        .unwrap_or_else(|| panic!("status omitted coverage: {status}"));
    assert!(
        !coverage.is_empty(),
        "status omitted lane coverage: {status}"
    );
    assert!(
        coverage
            .iter()
            .all(|lane| lane["lane"].as_str().is_some() && lane["state"].as_str().is_some()),
        "status coverage rows are not typed: {status}"
    );
    assert!(
        coverage.iter().any(|lane| lane["state"] == "complete"),
        "status omitted a complete coverage lane: {status}"
    );
    let unavailable = coverage
        .iter()
        .filter(|lane| lane["state"] != "complete")
        .map(|lane| {
            (
                lane["lane"].as_str().unwrap_or("unknown"),
                lane["reason"].as_str().unwrap_or("unspecified"),
            )
        })
        .collect::<Vec<_>>();
    for (lane, reason) in &unavailable {
        eprintln!(
            "surface-matrix surface={} capability={} pass=false state=unavailable reason={}",
            surface, lane, reason
        );
    }
    unavailable.is_empty()
}

fn assert_product_lane(value: &Value, lane: &str, surface: &str) -> bool {
    assert_eq!(value["answer"], "product", "{lane} changed answer kind");
    let records = value["records"]
        .as_array()
        .unwrap_or_else(|| panic!("{lane} omitted typed records: {value}"));
    if !records.is_empty() {
        return true;
    }
    let fault = value["fault"]
        .as_object()
        .unwrap_or_else(|| panic!("{lane} returned empty records without a typed fault: {value}"));
    let slug = fault["slug"]
        .as_str()
        .unwrap_or_else(|| panic!("{lane} fault omitted slug: {value}"));
    let detail = fault["detail"]
        .as_str()
        .unwrap_or_else(|| panic!("{lane} fault omitted detail: {value}"));
    assert!(!slug.is_empty(), "{lane} fault slug was empty: {value}");
    assert!(!detail.is_empty(), "{lane} fault detail was empty: {value}");
    eprintln!(
        "surface-matrix surface={} capability={} pass=false state=unavailable slug={} detail={}",
        surface, lane, slug, detail
    );
    false
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one daemon serves the complete surface matrix"
)]
fn production_surface_matrix_is_identity_equal_across_languages_and_restarts() {
    let started = Instant::now();
    let root = unique_root("journey");
    let project = surface_matrix::write_polyglot(&root);
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = unique_endpoint("journey");

    let daemon = launch(&endpoint, &workspace, &authority, Some(0), false);
    let fresh = surface_matrix::fresh_ingest(&endpoint, &project);
    let expected_root = fresh.revision().root();
    assert!(
        fresh.row_count() >= 10,
        "polyglot fixture retained too few rows: {fresh:?}"
    );

    let status_words = vec!["health".to_owned()];
    let cli_status = cli_json(&endpoint, &workspace, &project, &status_words);
    assert_eq!(cli_status["answer"], "status");
    let cli_capabilities_ready = assert_capability_truth(&cli_status, "cli");
    assert_eq!(
        surface_matrix::status_identity(&cli_status).revision,
        encode_id(expected_root.as_bytes())
    );

    // The exact row identity map is produced from live CLI search answers.
    let mut expected = BTreeMap::new();
    let mut cli_documents = BTreeMap::new();
    let mut cli_sources = BTreeMap::new();
    for case in surface_matrix::LANGUAGE_CASES {
        let search = cli_json(
            &endpoint,
            &workspace,
            &project,
            &[
                "search".to_owned(),
                case.name.to_owned(),
                "--limit".to_owned(),
                "20".to_owned(),
            ],
        );
        let identity = surface_matrix::case_identity(&search, case);
        assert_eq!(identity.path, case.path);
        assert_eq!(identity.name, case.name);
        assert_eq!(identity.language, case.language);
        assert!(
            expected
                .insert(identity.coordinate.clone(), identity.clone())
                .is_none()
        );

        let coordinate = identity.coordinate.clone();
        let show = vec!["show".to_owned(), coordinate.clone()];
        let source = vec![
            "source".to_owned(),
            coordinate.clone(),
            "--detail".to_owned(),
            "full".to_owned(),
        ];
        let document = cli_json(&endpoint, &workspace, &project, &show);
        let source_value = cli_json(&endpoint, &workspace, &project, &source);
        assert_eq!(document["answer"], "page");
        assert_eq!(document["language"], case.language);
        assert_eq!(source_value["answer"], "page");
        assert_eq!(source_value["language"], case.language);
        assert!(
            source_value["source"]["lines"]
                .as_array()
                .is_some_and(|lines| lines
                    .iter()
                    .any(|line| { line.as_str().is_some_and(|text| text.contains(case.name)) })),
            "source projection lost {}",
            case.name
        );
        cli_documents.insert(coordinate.clone(), document);
        cli_sources.insert(coordinate, source_value);
    }

    let cli_outline = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["outline".to_owned(), project.to_string_lossy().into_owned()],
    );
    let package_coordinate = expected
        .keys()
        .next()
        .and_then(|coordinate| coordinate.split_once("::"))
        .map(|(package, _)| package.to_owned())
        .expect("indexed rows have a package coordinate");
    let cli_dependencies = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["dependencies".to_owned(), package_coordinate.clone()],
    );
    assert_eq!(cli_outline["answer"], "outline");
    assert_eq!(cli_outline["package"]["coordinate"], package_coordinate);
    let cli_dependencies_ready = assert_product_lane(&cli_dependencies, "dependencies", "cli");
    let outline_ids = surface_matrix::outline_coordinates(&cli_outline);
    for coordinate in expected.keys() {
        assert!(
            outline_ids.contains(coordinate),
            "outline omitted exact indexed row {coordinate}: {cli_outline}"
        );
    }

    let calls = surface_matrix::mcp_calls(&project, &package_coordinate, &expected);
    let replies = mcp(&endpoint, &workspace, &project, &authority, &calls);
    for (id, response) in &replies {
        let bytes = serde_json::to_vec(response).expect("MCP response serializes").len();
        assert!(
            bytes <= backend_present::DEFAULT_RESPONSE_BUDGET_BYTES,
            "local daemon MCP response {id} is {bytes} bytes, above the {} byte budget: {response}",
            backend_present::DEFAULT_RESPONSE_BUDGET_BYTES
        );
    }
    for id in std::iter::once(1_u64).chain(calls.iter().map(|(id, _, _)| *id)) {
        assert!(
            replies.contains_key(&id),
            "MCP omitted response id {id}: {replies:?}"
        );
    }
    assert!(replies[&1]["result"]["protocolVersion"].is_string());
    let tools = &replies[&2]["result"]["tools"];
    for name in [
        "backend.status",
        "backend.outline",
        "backend.dependencies",
        "backend.search",
        "backend.document",
        "backend.source",
    ] {
        assert!(
            tools
                .as_array()
                .is_some_and(|rows| rows.iter().any(|tool| tool["name"] == name)),
            "MCP tools/list omitted {name}: {tools}"
        );
    }

    assert_mcp_ok(&replies[&3], "status");
    let mcp_status = surface_matrix::mcp_structured(&replies[&3]);
    assert_eq!(mcp_status, &cli_status);
    let mcp_capabilities_ready = assert_capability_truth(mcp_status, "mcp");
    assert_eq!(mcp_capabilities_ready, cli_capabilities_ready);
    assert_eq!(
        surface_matrix::status_identity(mcp_status),
        surface_matrix::status_identity(&cli_status)
    );

    assert_mcp_ok(&replies[&4], "outline");
    assert_eq!(surface_matrix::mcp_structured(&replies[&4]), &cli_outline);
    assert_mcp_ok(&replies[&5], "dependencies");
    let mcp_dependencies_ready = assert_product_lane(
        surface_matrix::mcp_structured(&replies[&5]),
        "dependencies",
        "mcp",
    );
    assert_eq!(mcp_dependencies_ready, cli_dependencies_ready);
    assert_eq!(
        surface_matrix::mcp_structured(&replies[&5]),
        &cli_dependencies
    );

    let mut id = 10_u64;
    for case in surface_matrix::LANGUAGE_CASES {
        let identity = expected
            .values()
            .find(|identity| identity.path == case.path && identity.name == case.name)
            .expect("case identity retained");
        let search_reply = &replies[&id];
        assert_mcp_ok(search_reply, "search");
        let mcp_identity =
            surface_matrix::case_identity(surface_matrix::mcp_structured(search_reply), case);
        assert_eq!(
            &mcp_identity, identity,
            "MCP search identity drifted for {}",
            case.path
        );
        assert_eq!(mcp_identity.language, case.language);
        id = id.saturating_add(1);

        let document_reply = &replies[&id];
        assert_mcp_ok(document_reply, "document");
        assert_eq!(
            surface_matrix::mcp_structured(document_reply),
            &cli_documents[&identity.coordinate]
        );
        assert_eq!(
            surface_matrix::mcp_text(document_reply),
            cli_markdown(
                &endpoint,
                &workspace,
                &project,
                &["show".to_owned(), identity.coordinate.clone()]
            )
        );
        id = id.saturating_add(1);

        let source_reply = &replies[&id];
        assert_mcp_ok(source_reply, "source");
        assert_eq!(
            surface_matrix::mcp_structured(source_reply),
            &cli_sources[&identity.coordinate]
        );
        assert_eq!(
            surface_matrix::mcp_text(source_reply),
            cli_markdown(
                &endpoint,
                &workspace,
                &project,
                &[
                    "source".to_owned(),
                    identity.coordinate.clone(),
                    "--detail".to_owned(),
                    "full".to_owned(),
                ]
            )
        );
        eprintln!(
            "surface-matrix case={} path={} language={} coordinate={} key={} cli=true mcp=true desktop=true pass=true",
            case.name, case.path, case.language, identity.coordinate, identity.key
        );
        id = id.saturating_add(1);
    }

    // The desktop reducer and semantic routes must consume the same admitted
    // root and expose every row identity found by the process surfaces.
    surface_matrix::desktop_probe(&endpoint, &project, expected_root, &expected);

    let mut max_rss = rss_kib(&daemon);
    eprintln!(
        "surface-matrix phase=fresh rows={} root={:?} rss_kib={:?}",
        fresh.row_count(),
        expected_root,
        max_rss
    );

    // Idle retirement is a real owner exit.  Reconnect to the same durable
    // workspace and require the exact root to be recovered.
    drop(daemon);
    let idle_started = Instant::now();
    let mut idle_daemon = launch(&endpoint, &workspace, &authority, Some(300), false);
    let _ = restart_health(&endpoint, expected_root);
    max_rss = max_rss.max(rss_kib(&idle_daemon));
    idle_daemon.graceful_wait(surface_matrix::PROCESS_DEADLINE);
    wait_for_socket_dead(&endpoint, &mut idle_daemon);
    eprintln!(
        "surface-matrix phase=idle-reconnect pass=true runtime_ms={} rss_kib={:?}",
        idle_started.elapsed().as_millis(),
        rss_kib(&idle_daemon)
    );

    let graceful_started = Instant::now();
    let mut graceful_daemon = launch(&endpoint, &workspace, &authority, Some(0), false);
    let _ = restart_health(&endpoint, expected_root);
    max_rss = max_rss.max(rss_kib(&graceful_daemon));
    surface_matrix::graceful_shutdown(&endpoint);
    graceful_daemon.graceful_wait(surface_matrix::PROCESS_DEADLINE);
    wait_for_socket_dead(&endpoint, &mut graceful_daemon);
    eprintln!(
        "surface-matrix phase=graceful-restart pass=true runtime_ms={} rss_kib={:?}",
        graceful_started.elapsed().as_millis(),
        rss_kib(&graceful_daemon)
    );

    let sigkill_started = Instant::now();
    let mut sigkill_daemon = launch(&endpoint, &workspace, &authority, Some(0), false);
    let _ = restart_health(&endpoint, expected_root);
    max_rss = max_rss.max(rss_kib(&sigkill_daemon));
    sigkill_daemon.kill_now();
    wait_for_socket_dead(&endpoint, &mut sigkill_daemon);
    let mut recovered_daemon = launch(&endpoint, &workspace, &authority, Some(0), false);
    let _ = restart_health(&endpoint, expected_root);
    max_rss = max_rss.max(rss_kib(&recovered_daemon));
    eprintln!(
        "surface-matrix phase=sigkill-restart pass=true runtime_ms={} rss_kib={:?}",
        sigkill_started.elapsed().as_millis(),
        rss_kib(&recovered_daemon)
    );

    let offline_started = Instant::now();
    recovered_daemon.kill_now();
    wait_for_socket_dead(&endpoint, &mut recovered_daemon);
    let offline_daemon = launch(&endpoint, &workspace, &authority, Some(0), true);
    let offline_report = restart_health(&endpoint, expected_root);
    max_rss = max_rss.max(rss_kib(&offline_daemon));
    assert_eq!(offline_report.revision().root(), expected_root);
    let offline_status = cli_json(&endpoint, &workspace, &project, &status_words);
    assert_eq!(
        surface_matrix::status_identity(&offline_status),
        surface_matrix::status_identity(&cli_status)
    );
    eprintln!(
        "surface-matrix phase=offline-restart pass=true runtime_ms={} rss_kib={:?}",
        offline_started.elapsed().as_millis(),
        rss_kib(&offline_daemon)
    );

    eprintln!(
        "surface-matrix result={} languages={} rows={} runtime_ms={} max_rss_kib={:?}",
        if cli_capabilities_ready && cli_dependencies_ready {
            "PASS"
        } else {
            "PASS_WITH_UNAVAILABLE"
        },
        surface_matrix::LANGUAGE_CASES.len(),
        expected.len(),
        started.elapsed().as_millis(),
        max_rss
    );
    drop(offline_daemon);
    let _ = std::fs::remove_file(&endpoint);
    let _ = std::fs::remove_dir_all(&root);
}
