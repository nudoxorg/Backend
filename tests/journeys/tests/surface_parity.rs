//! Identity-equal answers across the CLI and MCP processes.
//!
//! The claim this file proves is narrow and total: *the same corpus and the
//! same basis produce the same content on every surface*. Not "similar", not
//! "both non-empty" — the CLI's `--format markdown` and the MCP text block are
//! compared byte for byte, and the CLI's `--format json` and the MCP's
//! `structuredContent` are compared as decoded values. A count is never
//! compared: a count survives every field inside a record being erased, and a
//! round trip compared by counts is a test that cannot fail for the reason it
//! exists.
//!
//! Both processes are the real ones, spawned against one live daemon over one
//! indexed project, so nothing here can pass because a fake agreed with itself.
//!
//! Three failure classes are covered, and two of them are forced live:
//!
//! * **unknown coordinate** — a coordinate no revision publishes;
//! * **malformed operand** — a page bound outside its closed range;
//! * **unconfigured lane** — the capability rollup and the `~lanes` line, which
//!   is the one place an agent can see that a thin answer is thin.
//!
//! A fourth, **wrong basis**, cannot be forced through a live surface without
//! racing the owner's publication, and a race is not a test. It is covered
//! instead by comparing the two surfaces' rendering functions directly on one
//! constructed fault — which is meaningful precisely because every live case
//! above proves that both processes route their real faults through those same
//! functions.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use backend_library::{CommandFailure, ViewRevision, view_state_root};
use backend_present::{Fault, FaultSlug, Operand, markdown};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const AUTHORITY_SECRET: [u8; 32] = [0x5a; 32];
const READY_DEADLINE: Duration = Duration::from_secs(40);
const COMMAND_DEADLINE: Duration = Duration::from_secs(30);
const MCP_DEADLINE: Duration = Duration::from_secs(90);
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// the live fixture
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn spawn(args: &[OsString]) -> Self {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for (name, value) in rust_toolchain() {
            command.env(name, value);
        }
        Self(Some(command.spawn().expect("spawn locald")))
    }

    fn running(&mut self) -> bool {
        self.0
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll locald"))
            .is_none()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.0.as_mut() else {
            return;
        };
        if child.try_wait().expect("poll locald").is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

fn rust_toolchain() -> Vec<(&'static str, OsString)> {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path).find_map(|directory| {
                let candidate = directory.join("rustc");
                candidate
                    .is_file()
                    .then(|| candidate.canonicalize().ok())
                    .flatten()
            })
        })
        .map(|rustc| vec![("NUDOX_RUSTC", rustc.into_os_string())])
        .unwrap_or_default()
}

fn quiet(command: &mut ProcessCommand) {
    command
        .env_remove("COMPILER_GO_COMPILER")
        .env_remove("NUDOX_GO_ORACLE_BIN")
        .env_remove("NUDOX_GO")
        .env_remove("NUDOX_GO_ORACLE");
}

fn bounded(mut command: ProcessCommand, label: &str, input: Option<Vec<u8>>) -> Output {
    let deadline = if input.is_some() {
        MCP_DEADLINE
    } else {
        COMMAND_DEADLINE
    };
    quiet(&mut command);
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {label}: {error}"));
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(&input)
            .unwrap_or_else(|error| panic!("write {label} stdin: {error}"));
    }
    let mut out = child.stdout.take().expect("stdout");
    let mut err = child.stderr.take().expect("stderr");
    let out_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        out.read_to_end(&mut bytes).expect("read stdout");
        bytes
    });
    let err_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        err.read_to_end(&mut bytes).expect("read stderr");
        bytes
    });
    let end = Instant::now() + deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < end => thread::yield_now(),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{label} exceeded its bounded deadline");
            }
            Err(error) => panic!("wait {label}: {error}"),
        }
    };
    Output {
        status,
        stdout: out_reader.join().expect("join stdout"),
        stderr: err_reader.join().expect("join stderr"),
    }
}

fn unique(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-parity-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root.canonicalize().expect("canonical journey root")
}

fn endpoint_for(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-parity-{label}-{}-{serial}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&endpoint);
    endpoint
}

/// Writes the polyglot fixture: one crate whose declarations have known trails.
fn write_project(root: &Path) -> PathBuf {
    let project = root.join("polyglot");
    std::fs::create_dir_all(project.join("src")).expect("create project");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"polyglot\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )
    .expect("write manifest");
    std::fs::write(
        project.join("src/lib.rs"),
        "/// Returns the Rust lane marker.\npub fn ferris() -> &'static str { \"rust\" }\n\
         /// Lights the beacon.\npub fn beacon(lane: &str) -> usize { lane.len() }\n",
    )
    .expect("write source");
    project.canonicalize().expect("canonical project")
}

fn authority(root: &Path) -> PathBuf {
    let path = root.join("authority.secret");
    std::fs::write(&path, AUTHORITY_SECRET).expect("write authority secret");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat authority secret")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&path, permissions).expect("protect authority secret");
    path
}

fn locald_args(endpoint: &Path, workspace: &Path, secret: &Path) -> Vec<OsString> {
    vec![
        OsString::from("--endpoint"),
        endpoint.as_os_str().to_owned(),
        OsString::from("--workspace"),
        workspace.as_os_str().to_owned(),
        OsString::from("--profile"),
        OsString::from("builtin"),
        OsString::from("--authority-secret-file"),
        secret.as_os_str().to_owned(),
        OsString::from("--max-frame"),
        OsString::from("131072"),
        OsString::from("--timeout-ms"),
        OsString::from("180000"),
    ]
}

fn wait_for_socket(endpoint: &Path, child: &mut ChildGuard) {
    let end = Instant::now() + READY_DEADLINE;
    while Instant::now() < end {
        assert!(child.running(), "locald exited before readiness");
        if let Ok(metadata) = std::fs::symlink_metadata(endpoint)
            && metadata.file_type().is_socket()
            && UnixStream::connect(endpoint).is_ok()
        {
            return;
        }
        thread::yield_now();
    }
    panic!("timed out waiting for locald socket {}", endpoint.display());
}

/// Indexes the fixture and waits until the shelf publishes its declarations.
fn index_and_wait(endpoint: &Path, project: &Path) {
    let mut session = backend_mcp::Session::connect(endpoint).expect("connect indexing session");
    session
        .index(&project.to_string_lossy())
        .expect("submit index");
    let end = Instant::now() + READY_DEADLINE;
    loop {
        let report = session.health().expect("read health");
        if report.row_count() > 1 {
            return;
        }
        assert!(Instant::now() < end, "the fixture never published rows");
        thread::yield_now();
    }
}

// ---------------------------------------------------------------------------
// driving the two surfaces
// ---------------------------------------------------------------------------

struct Surfaces {
    endpoint: PathBuf,
    workspace: PathBuf,
    project: PathBuf,
    authority_secret: PathBuf,
}

impl Surfaces {
    fn cli(&self, words: &[&str]) -> Output {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
        command
            .arg("--endpoint")
            .arg(&self.endpoint)
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--project")
            .arg(&self.project)
            .args(words)
            .env("NO_COLOR", "1")
            .env("COLUMNS", "100");
        bounded(command, &format!("cli {words:?}"), None)
    }

    fn cli_text(&self, format: &str, words: &[&str]) -> String {
        let mut all = vec!["--format", format];
        all.extend_from_slice(words);
        let output = self.cli(&all);
        String::from_utf8(output.stdout).expect("CLI stdout is UTF-8")
    }

    /// Runs an arbitrary JSON-RPC batch in one MCP session.
    fn rpc(&self, calls: &[(u64, &str, Value)]) -> BTreeMap<u64, Value> {
        let mut lines = vec![
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "parity-journey", "version": "1" }
                }
            })
            .to_string(),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} })
                .to_string(),
        ];
        lines.extend(calls.iter().map(|(id, method, params)| {
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
        }));
        let mut input = lines.join("\n").into_bytes();
        input.push(b'\n');
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
        command
            .arg("--endpoint")
            .arg(&self.endpoint)
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--project")
            .arg(&self.project)
            .env(
                "BACKEND_LOCALD_AUTHORITY_SECRET_FILE",
                &self.authority_secret,
            );
        let output = bounded(command, "mcp session", Some(input));
        assert!(
            output.status.success(),
            "MCP process failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("MCP stdout is UTF-8")
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|value| {
                value["id"]
                    .as_u64()
                    .filter(|id| *id != 1)
                    .map(|id| (id, value))
            })
            .collect()
    }
}

fn text_block(result: &Value) -> String {
    result["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text block in {result}"))
        .to_owned()
}

fn structured(result: &Value) -> Value {
    result["result"]["structuredContent"].clone()
}

/// Finds the first declaration coordinate the live search page published.
fn first_coordinate(structured: &Value) -> String {
    structured["records"]
        .as_array()
        .and_then(|records| {
            records
                .iter()
                .find(|record| record["identity"]["path"].is_string())
        })
        .and_then(|record| record["identity"]["coordinate"].as_str())
        .unwrap_or_else(|| panic!("search published no source-bound record: {structured}"))
        .to_owned()
}

fn coverage_line(text: &str) -> &str {
    text.lines()
        .find(|line| line.starts_with("~lanes"))
        .unwrap_or_else(|| panic!("no coverage line in:\n{text}"))
}

// ---------------------------------------------------------------------------
// the proof
// ---------------------------------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one live daemon serves every case; splitting it would index the fixture five times"
)]
fn every_surface_renders_identity_equal_content_for_the_same_revision() {
    let root = unique("content");
    let project = write_project(&root);
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let endpoint = endpoint_for("content");
    let secret = authority(&root);
    let mut locald = ChildGuard::spawn(&locald_args(&endpoint, &workspace, &secret));
    wait_for_socket(&endpoint, &mut locald);
    index_and_wait(&endpoint, &project);

    let surfaces = Surfaces {
        endpoint: endpoint.clone(),
        workspace: workspace.clone(),
        project: project.clone(),
        authority_secret: secret.clone(),
    };

    // Search the live index first and pass its exact published coordinate into
    // the page lookup. Semantic rows use compiler-owned identities, so this
    // proves the owner resolves a copied search identity on both surfaces.
    let search_json: Value =
        serde_json::from_str(&surfaces.cli_text("json", &["search", "ferris"]))
            .expect("CLI search JSON");
    let coordinate = first_coordinate(&search_json);
    let missing = format!("{}::src/lib.rs:999::nothing", project.display());

    // One session answers every call, including the resource read that carries
    // the query card. Each MCP process re-indexes the project on startup, so
    // asking for everything at once is both the faster proof and the quieter
    // one: three sessions competing for one daemon is a load test, not a
    // rendering test.
    let calls = [
        (10, "backend.search", json!({ "query": "ferris" })),
        (11, "backend.document", json!({ "coordinate": coordinate })),
        (12, "backend.outline", json!({})),
        (13, "backend.packages", json!({})),
        (14, "backend.status", json!({})),
        (15, "backend.document", json!({ "coordinate": missing })),
        (
            16,
            "backend.search",
            json!({ "query": "ferris", "limit": 900 }),
        ),
    ];
    let mut batch = calls
        .iter()
        .map(|(id, tool, arguments)| {
            (
                *id,
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
        })
        .collect::<Vec<_>>();
    batch.push((
        20,
        "resources/read",
        json!({ "uri": "backend://schema/query" }),
    ));
    let replies = surfaces.rpc(&batch);

    let successes: [(u64, Vec<&str>); 5] = [
        (10, vec!["search", "ferris"]),
        (11, vec!["show", coordinate.as_str()]),
        (12, vec!["outline"]),
        (13, vec!["packages"]),
        (14, vec!["health"]),
    ];
    for (id, words) in &successes {
        let reply = replies
            .get(id)
            .unwrap_or_else(|| panic!("no MCP reply for request {id}"));
        assert_eq!(
            reply["result"]["isError"], false,
            "MCP refused `{words:?}`: {reply}"
        );
        let mcp_text = text_block(reply);
        let cli_markdown = surfaces.cli_text("markdown", words);
        assert_eq!(
            cli_markdown, mcp_text,
            "`{words:?}` renders differently on the two surfaces"
        );
        let cli_json: Value = serde_json::from_str(&surfaces.cli_text("json", words))
            .unwrap_or_else(|error| panic!("`{words:?}` CLI JSON: {error}"));
        assert_eq!(
            cli_json,
            structured(reply),
            "`{words:?}` typed projection differs between the surfaces"
        );
        // The human rendering differs only by colour and width, so every
        // identity string in the shared answer has to survive into it.
        let human = surfaces.cli_text("human", words);
        assert!(
            !human.contains('\u{1b}'),
            "NO_COLOR must produce plain bytes: {human}"
        );
        let needles = human_needles(&cli_json);
        assert!(
            !needles.is_empty(),
            "`{words:?}` published nothing a reader could copy: {cli_json}"
        );
        for needle in needles {
            assert!(
                human.contains(&needle),
                "`{words:?}` human output dropped `{needle}`:\n{human}"
            );
        }
    }

    // The page: identity first, the exact coordinate, the signature, the source.
    let page = structured(&replies[&11]);
    let page_text = text_block(&replies[&11]);
    let trail = page["identity"]["trail"].as_str().expect("page trail");
    assert!(trail.starts_with("polyglot › "), "trail: {trail}");
    assert!(trail.ends_with("› ferris"), "trail: {trail}");
    assert_eq!(
        page["identity"]["coordinate"].as_str(),
        Some(coordinate.as_str())
    );
    assert_eq!(page["kind"], "function");
    assert_eq!(page["language"], "rust");
    assert_eq!(
        page_text.lines().next(),
        Some(format!("# {trail}").as_str())
    );
    assert!(
        page_text.contains(&format!("`{coordinate}`")),
        "the exact coordinate is never wrapped or clipped:\n{page_text}"
    );
    assert!(
        !page_text.contains('|'),
        "a Markdown table would corrupt a signature or a path:\n{page_text}"
    );
    if let Some(signature) = page["signature"].as_str() {
        assert!(
            page_text.contains(signature),
            "the signature specimen is missing:\n{page_text}"
        );
        assert!(
            surfaces
                .cli_text("human", &["show", coordinate.as_str()])
                .contains(signature)
        );
    }

    // Identity is the centre of gravity, so the one thing that must survive a
    // parse is the address itself: re-spell the coordinate through the shared
    // model and ask the live engine for the same page with those bytes.
    let parsed = backend_present::Identity::parse(&coordinate);
    assert_eq!(
        parsed.coordinate().as_str(),
        coordinate,
        "a parsed coordinate must re-spell to the exact bytes the engine accepts"
    );
    assert_eq!(parsed.name(), "ferris");
    assert!(matches!(
        parsed.shape(),
        backend_present::IdentityShape::Declaration | backend_present::IdentityShape::Semantic
    ));
    let respelled = surfaces.cli_text("markdown", &["show", parsed.coordinate().as_str()]);
    assert_eq!(
        respelled, page_text,
        "the engine answered a re-spelled coordinate differently"
    );

    // The coverage line is the one place a thin answer is visibly thin, and it
    // is the same bytes on both surfaces and in both CLI renderings.
    let search_text = text_block(&replies[&10]);
    let lanes = coverage_line(&search_text);
    assert!(lanes.contains("exact"), "{lanes}");
    assert_eq!(
        coverage_line(&surfaces.cli_text("human", &["search", "ferris"])),
        lanes
    );
    assert_eq!(
        coverage_line(&text_block(&replies[&14])),
        coverage_line(&surfaces.cli_text("human", &["health"]))
    );

    // The outline is names and kinds, never bare digests.
    let outline_text = text_block(&replies[&12]);
    assert!(outline_text.contains("ferris"), "{outline_text}");
    assert!(outline_text.contains("declaration(s)"), "{outline_text}");

    // An unconfigured capability is stated, not implied by silence.
    let status = structured(&replies[&14]);
    let summary = status["capabilities"]["summary"]
        .as_str()
        .expect("capability summary");
    assert!(summary.contains("embedding "), "{summary}");
    assert!(
        text_block(&replies[&14]).contains(summary),
        "the status text carries the rollup verbatim"
    );
    assert!(
        text_block(&replies[&14]).lines().count() <= 25,
        "status must stay a glance, not a dump:\n{}",
        text_block(&replies[&14])
    );

    // Fault one: a coordinate no revision publishes.
    let refused = &replies[&15];
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let refused_text = text_block(refused);
    assert!(refused_text.starts_with("✗ not-found"), "{refused_text}");
    assert!(
        refused_text.contains(&missing),
        "the operand is never elided"
    );
    assert_eq!(structured(refused)["slug"], "not-found");
    let cli_refusal = surfaces.cli(&["--format", "markdown", "show", &missing]);
    assert_eq!(
        String::from_utf8_lossy(&cli_refusal.stderr).trim_end(),
        refused_text,
        "a refusal reads the same on both surfaces"
    );
    assert_eq!(cli_refusal.status.code(), Some(2), "the engine refused");
    let cli_refusal_json: Value =
        serde_json::from_slice(&surfaces.cli(&["--json", "show", &missing]).stdout)
            .expect("CLI fault JSON");
    assert_eq!(cli_refusal_json, structured(refused));

    // Fault two: an operand outside the closed range the grammar declares.
    let malformed = &replies[&16];
    assert_eq!(malformed["result"]["isError"], true, "{malformed}");
    assert_eq!(structured(malformed)["slug"], "usage");
    assert_eq!(structured(malformed)["operand"], "limit");
    let cli_malformed =
        surfaces.cli(&["--format", "markdown", "search", "ferris", "--limit", "900"]);
    assert_eq!(
        String::from_utf8_lossy(&cli_malformed.stderr).trim_end(),
        text_block(malformed)
    );
    assert_eq!(
        cli_malformed.status.code(),
        Some(64),
        "the caller was wrong"
    );

    assert_the_query_card_runs(&surfaces, &replies[&20]);

    drop(locald);
}

/// Runs every worked query printed on the `backend.query` schema card.
///
/// A card that documents a query the engine refuses is worse than no card: an
/// agent spends a call learning that the documentation is wrong. So the card is
/// not prose about the schema — it is a list of queries, and this lifts each one
/// out of the rendered resource exactly as an agent would read it and runs it
/// against the live index.
fn assert_the_query_card_runs(surfaces: &Surfaces, card: &Value) {
    let text = card["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("the schema card is not readable: {card}"))
        .to_owned();
    let queries = fenced_queries(&text);
    assert!(
        queries.len() >= 4,
        "the card must work through examples, found {}",
        queries.len()
    );
    let calls = queries
        .iter()
        .enumerate()
        .map(|(index, query)| {
            (
                worked_query_id(index),
                "tools/call",
                json!({
                    "name": "backend.query",
                    "arguments": { "query": query, "limit": 20 }
                }),
            )
        })
        .collect::<Vec<_>>();
    let replies = surfaces.rpc(&calls);
    let mut rows = 0_usize;
    for (index, query) in queries.iter().enumerate() {
        let reply = &replies[&worked_query_id(index)];
        assert!(
            reply.get("error").is_none(),
            "the card documents a query the engine refuses:\n{query}\n{reply}"
        );
        assert_eq!(
            reply["result"]["isError"],
            false,
            "the card documents a query that fails:\n{query}\n{}",
            text_block(reply)
        );
        rows = rows.saturating_add(
            reply["result"]["structuredContent"]["rows"]
                .as_array()
                .map_or(0, Vec::len),
        );
    }
    assert!(
        rows > 0,
        "every worked query returned nothing; the card teaches an empty schema"
    );
}

/// The request id one worked query is asked under.
fn worked_query_id(index: usize) -> u64 {
    u64::try_from(index)
        .map(|index| index.saturating_add(30))
        .expect("a card carries far fewer than u64::MAX examples")
}

/// Lifts every fenced `graphql` block out of the rendered card.
fn fenced_queries(card: &str) -> Vec<String> {
    let mut queries = Vec::new();
    let mut rest = card;
    while let Some((_, after)) = rest.split_once("```graphql\n") {
        let Some((query, remainder)) = after.split_once("\n```") else {
            break;
        };
        queries.push(query.to_owned());
        rest = remainder;
    }
    queries
}

/// Every string the human rendering of one answer must reproduce verbatim.
///
/// This is a contract per answer, not a sweep for anything that looks like an
/// address, because the contracts genuinely differ. A result page and the shelf
/// print every exact coordinate on its own line, because an agent copies them.
/// A declaration page prints its own coordinate and each relation's, but its
/// member lines are trails and signatures: a column of absolute coordinates
/// under a heading called "members" is not a list anyone reads. An outline is a
/// shape, so it owes names and kinds. Status owes the capability rollup, which
/// is the one sentence that distinguishes "nothing matched" from "that lane was
/// never configured".
fn human_needles(value: &Value) -> Vec<String> {
    match value["answer"].as_str().unwrap_or_default() {
        "records" => each(value, "records", &["identity", "coordinate"]),
        "shelf" => each(value, "projects", &["identity", "coordinate"]),
        "page" => {
            let mut needles = string_at(value, &["identity", "coordinate"])
                .into_iter()
                .collect::<Vec<_>>();
            for group in value["relations"].as_array().unwrap_or(&Vec::new()) {
                needles.extend(each(group, "relations", &["coordinate"]));
            }
            needles
        }
        "outline" => {
            let mut names = string_at(value, &["package", "name"])
                .into_iter()
                .collect::<Vec<_>>();
            for root in value["roots"].as_array().unwrap_or(&Vec::new()) {
                collect_names(root, &mut names);
            }
            names
        }
        "status" => string_at(value, &["capabilities", "summary"])
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn each(value: &Value, list: &str, path: &[&str]) -> Vec<String> {
    value[list]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|item| string_at(item, path))
        .collect()
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut at = value;
    for step in path {
        at = at.get(step)?;
    }
    at.as_str().map(ToOwned::to_owned)
}

fn collect_names(node: &Value, found: &mut Vec<String>) {
    if let Some(name) = node["name"].as_str() {
        found.push(name.to_owned());
    }
    for child in node["children"].as_array().unwrap_or(&Vec::new()) {
        collect_names(child, found);
    }
}

/// The fourth failure class, proven against the renderers both surfaces call.
///
/// A wrong basis means the owner published a newer revision while a request was
/// in flight. Forcing that live means winning a race against the daemon, and a
/// test that has to win a race is a test that will one day lose one. The live
/// cases above already prove that both processes render their real faults with
/// these functions; what is left to prove is that this fault class renders the
/// same way through them, which is checkable without a daemon at all.
#[test]
fn a_wrong_basis_names_both_revisions_on_every_surface() {
    let fault = Fault::from_command_failure(
        &CommandFailure::WrongBasis {
            expected: ViewRevision::from(view_state_root(&[(
                "owner".to_owned(),
                "new".to_owned(),
            )])),
            observed: ViewRevision::from(view_state_root(&[])),
        },
        Operand::Whole,
    );
    assert_eq!(fault.slug(), FaultSlug::WrongBasis);

    let operand = fault.operand().render();
    assert!(
        operand.contains("(owner holds "),
        "a wrong basis names the revision the caller pinned and the one the owner holds: {operand}"
    );

    let agent = markdown::fault(&fault);
    assert_eq!(
        backend_cli::render::fault(
            &fault,
            &backend_cli::Options::plain(backend_cli::Format::Markdown)
        ),
        agent,
        "the CLI's Markdown fault is the agent-facing renderer, not a copy of it"
    );
    assert!(agent.starts_with("✗ wrong-basis "), "{agent}");
    assert!(
        agent.contains("published a newer revision"),
        "the cause is a sentence a reader can act on: {agent}"
    );

    let human = backend_cli::render::fault(
        &fault,
        &backend_cli::Options::plain(backend_cli::Format::Human),
    );
    assert!(human.starts_with("✗ wrong-basis "), "{human}");
    assert!(
        human.contains(&operand),
        "the human rendering keeps the exact operand: {human}"
    );
    assert!(
        human.contains("→ re-run the same command"),
        "a retryable fault offers the retry: {human}"
    );

    let typed = backend_present::fault_value(&fault);
    assert_eq!(typed["answer"], "fault");
    assert_eq!(typed["slug"], "wrong-basis");
    assert_eq!(typed["cause"], "moved");
    assert_eq!(typed["operand"], operand);
}
