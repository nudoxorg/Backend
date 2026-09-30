//! Tentpole package-index journey through real locald, CLI, and MCP processes.
//!
//! The fixture is its own oracle: the expected dependency edges are read from
//! the exact manifest strings written here, package coordinates come from the
//! declared release versions, and the advisory identity comes from the
//! repository's real RustSec fixture. A same-source dependency edit must alter
//! the graph answer; a hidden compiler input edit must alter the compiler-owned
//! documentation that users can search; and a cold reopen must preserve those
//! answers while exposing persisted freshness as stale.
//!
//! The canonical journey registry does not publish native yank facts, and the
//! public CLI/MCP path has no control for interrupting or tampering with a
//! particular remote closure frame. Those cases are left to a native registry
//! protocol journey and the worker transport process journey respectively.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![allow(
    clippy::too_many_lines,
    reason = "one user journey crosses each durable boundary"
)]

#[path = "../src/fake_registry.rs"]
mod fake_registry;
#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use backend_client::{ClientError, LocalSemanticIndexClient};
use backend_replication::{HydrationCredits, IrHydrationPoll, SemanticTargetKey, TransportLimits};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
use fake_registry::{FakeFile, FakePackage, FakeRegistry, RegistryMode};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const INDEX_DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(20);
const DIAGNOSTIC_SAMPLES: usize = 7;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

const LINEAGE_V1: &[FakeFile] = &[
    FakeFile {
        path: "Cargo.toml",
        contents: "[package]\nname = \"tentpole-lineage\"\nversion = \"1.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    },
    FakeFile {
        path: "src/lib.rs",
        contents: "/// First independently authored release fixture.\npub fn TentpoleLineageV1() -> u32 { 1 }\n",
    },
];

const LINEAGE_V2: &[FakeFile] = &[
    FakeFile {
        path: "Cargo.toml",
        contents: "[package]\nname = \"tentpole-lineage\"\nversion = \"2.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    },
    FakeFile {
        path: "src/lib.rs",
        contents: "/// Second independently authored release fixture.\npub fn TentpoleLineageV2() -> u32 { 2 }\n",
    },
];

const BINCODE_FIXTURE: &[FakeFile] = &[
    FakeFile {
        path: "Cargo.toml",
        contents: "[package]\nname = \"bincode\"\nversion = \"1.3.3\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    },
    FakeFile {
        path: "src/lib.rs",
        contents: "/// A small source specimen paired with the real bincode advisory fixture.\npub fn BincodeTentpoleFixture() -> u32 { 133 }\n",
    },
];

#[derive(Debug)]
struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "backend-index-tentpole-{}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create tentpole fixture root");
        Self(
            path.canonicalize()
                .expect("canonical tentpole fixture root"),
        )
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct Locald {
    child: Option<Child>,
    endpoint: PathBuf,
}

impl Locald {
    fn launch(
        endpoint: &Path,
        workspace: &Path,
        authority: &Path,
        registry: &str,
        rustsec: &Path,
    ) -> Self {
        let mut args = surface_matrix::locald_args(endpoint, workspace, authority, Some(0), false);
        args.extend([
            OsString::from("--registry-endpoint"),
            OsString::from(registry),
            OsString::from("--registry-ecosystem"),
            OsString::from("cargo"),
            OsString::from("--advisory-rustsec"),
            rustsec.as_os_str().to_owned(),
            OsString::from("--advisory-max-age-secs"),
            OsString::from("86400"),
        ]);
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("BACKEND_") {
                command.env_remove(key);
            }
        }
        command.env("BACKEND_ADVISORY_POLICY", "warn");
        let child = command.spawn().expect("spawn locald");
        let mut daemon = Self {
            child: Some(child),
            endpoint: endpoint.to_path_buf(),
        };
        wait_for_socket(endpoint, &mut daemon);
        daemon
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

    fn kill_now(&mut self) {
        if let Some(mut child) = self.child.take() {
            if child
                .try_wait()
                .expect("poll locald before SIGKILL")
                .is_none()
            {
                child.kill().expect("kill locald");
            }
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

impl Drop for Locald {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if child
                .try_wait()
                .expect("poll locald during cleanup")
                .is_none()
            {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

#[derive(Default)]
struct Diagnostics {
    warm_query_ms: Vec<u128>,
    reopen_query_ms: Vec<u128>,
    peak_rss_kib: Option<u64>,
}

fn endpoint() -> PathBuf {
    let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!(
        "/tmp/backend-index-tentpole-{}-{serial}.sock",
        std::process::id()
    ))
}

fn wait_for_socket(path: &Path, daemon: &mut Locald) {
    let end = Instant::now() + surface_matrix::READY_DEADLINE;
    while Instant::now() < end {
        assert!(daemon.running(), "locald exited before readiness");
        if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
            && UnixStream::connect(path).is_ok()
        {
            return;
        }
        thread::sleep(POLL);
    }
    panic!("timed out waiting for locald socket {}", path.display());
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
        .env("COLUMNS", "120");
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

fn words(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn mcp(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority: &Path,
    calls: &[(u64, Value)],
) -> BTreeMap<u64, Value> {
    let mut lines = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "index-tentpole", "version": "1" }
            }
        })
        .to_string(),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}).to_string(),
    ];
    lines.extend(calls.iter().map(|(id, params)| {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":params}).to_string()
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
        .arg(project);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("BACKEND_") || name.starts_with("NUDOX_") {
            command.env_remove(key);
        }
    }
    command.env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority);
    let output = surface_matrix::run_bounded(command, "MCP tentpole process", Some(input));
    assert!(
        output.status.success(),
        "MCP process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    BufReader::new(output.stdout.as_slice())
        .lines()
        .filter_map(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter_map(|value| value["id"].as_u64().map(|id| (id, value)))
        .collect()
}

fn mcp_structured<'a>(reply: &'a Value, label: &str) -> &'a Value {
    assert_eq!(
        reply["result"]["isError"], false,
        "MCP {label} returned an error: {reply}"
    );
    reply["result"]
        .get("structuredContent")
        .unwrap_or_else(|| panic!("MCP {label} omitted structured content: {reply}"))
}

fn wait_for_cli_title(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    query: &str,
    title: &str,
) -> Value {
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let result = cli_json(
            endpoint,
            workspace,
            project,
            &[
                "index-search".to_owned(),
                query.to_owned(),
                "--limit".to_owned(),
                "40".to_owned(),
            ],
        );
        if result["records"].as_array().is_some_and(|records| {
            records
                .iter()
                .any(|record| record["title"].as_str() == Some(title))
        }) {
            return result;
        }
        assert!(
            Instant::now() < end,
            "index-search `{query}` never returned `{title}`: {result}"
        );
        thread::sleep(Duration::from_millis(150));
    }
}

fn titles(value: &Value) -> BTreeSet<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["title"].as_str().map(str::to_owned))
        .collect()
}

fn registry_coordinates(value: &Value, package_name: &str) -> BTreeSet<String> {
    value["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|record| {
            record["title"]
                .as_str()
                .is_some_and(|title| title.starts_with(&format!("{package_name} ")))
        })
        .filter_map(|record| record["operand"].as_str().map(str::to_owned))
        .collect()
}

fn dependency_titles(value: &Value) -> BTreeSet<String> {
    titles(value)
}

fn create_javascript_project(root: &Path) -> PathBuf {
    let project = root.join("tentpole-web");
    std::fs::create_dir_all(project.join("src")).expect("create web source directory");
    std::fs::write(
        project.join("package.json"),
        "{\n  \"name\": \"tentpole-app\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"dep-before\": \"^1.0.0\" }\n}\n",
    )
    .expect("write web manifest");
    std::fs::write(
        project.join(".gitignore"),
        "node_modules/\ndist/\n.next/\nbuild/\ncoverage/\n",
    )
    .expect("write web ignore rules");
    std::fs::write(
        project.join("src/index.js"),
        "export function TentpoleWebEntry() { return 'indexed source'; }\n",
    )
    .expect("write web source");

    for directory in [
        "node_modules/cache",
        "dist",
        ".next/server",
        "build",
        "coverage",
    ] {
        std::fs::create_dir_all(project.join(directory)).expect("create ignored build directory");
    }
    let padding = "/* ignored generated JavaScript payload */\n".repeat(4096);
    for index in 0..64_u32 {
        let directory = match index % 5 {
            0 => "node_modules/cache",
            1 => "dist",
            2 => ".next/server",
            3 => "build",
            _ => "coverage",
        };
        let source = format!(
            "export function IgnoredTentpoleBuildDecoy{index}() {{ return 1; }}\n{padding}"
        );
        std::fs::write(
            project.join(directory).join(format!("bundle-{index}.js")),
            source,
        )
        .expect("write ignored generated JavaScript");
    }
    project.canonicalize().expect("canonical web fixture")
}

fn create_dynamic_rust_project(root: &Path) -> PathBuf {
    let project = root.join("tentpole-build-input");
    std::fs::create_dir_all(project.join("src")).expect("create Rust fixture source");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"tentpole-build-input\"\nversion = \"1.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .expect("write Rust fixture manifest");
    std::fs::write(
        project.join("src/lib.rs"),
        "#[doc = include_str!(\"../compiler.recipe\")]\npub fn dynamic_compiler_input() -> u32 { 1 }\n",
    )
    .expect("write Rust fixture source with a non-source compiler read");
    std::fs::write(
        project.join("compiler.recipe"),
        "Opaque compiler input before refresh: TentpoleDynamicBeforeBeacon.\n",
    )
    .expect("write unlisted compiler input");
    project.canonicalize().expect("canonical Rust fixture")
}

fn wait_for_title_absent(endpoint: &Path, workspace: &Path, project: &Path, query: &str) -> Value {
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let result = cli_json(
            endpoint,
            workspace,
            project,
            &[
                "index-search".to_owned(),
                query.to_owned(),
                "--limit".to_owned(),
                "40".to_owned(),
            ],
        );
        if result["records"].as_array().is_some_and(Vec::is_empty) {
            return result;
        }
        assert!(
            Instant::now() < end,
            "index-search `{query}` retained a removed compiler fact: {result}"
        );
        thread::sleep(Duration::from_millis(150));
    }
}

fn sampled_query(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    daemon: &Locald,
    samples: &mut Vec<u128>,
    peak_rss_kib: &mut Option<u64>,
) -> Value {
    let started = Instant::now();
    let value = cli_json(
        endpoint,
        workspace,
        project,
        &words(&["index-search", "tentpole-lineage", "--limit", "20"]),
    );
    samples.push(started.elapsed().as_millis());
    if let Some(pid) = daemon.pid() {
        if let Some(sample) = rss_kib(pid) {
            *peak_rss_kib = Some((*peak_rss_kib).map_or(sample, |previous| previous.max(sample)));
        }
    }
    value
}

fn rss_kib(pid: u32) -> Option<u64> {
    let output = ProcessCommand::new("ps")
        .args(["-o", "rss=", "-p"])
        .arg(pid.to_string())
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn tree_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            let file_type = entry.file_type().ok();
            if file_type.as_ref().is_some_and(|kind| kind.is_dir()) {
                tree_bytes(&entry.path())
            } else if file_type.as_ref().is_some_and(|kind| kind.is_file()) {
                entry.metadata().map_or(0, |metadata| metadata.len())
            } else {
                0
            }
        })
        .sum()
}

fn percentile(samples: &[u128], percentile: usize) -> Option<u128> {
    if samples.is_empty() {
        return None;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let rank = ordered
        .len()
        .saturating_sub(1)
        .saturating_mul(percentile)
        .div_ceil(100);
    ordered.get(rank).copied()
}

fn assert_mcp_parity(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority: &Path,
    app_coordinate: &str,
    bincode_coordinate: &str,
    exact_lineage_coordinate: &str,
    lineage_name: &str,
) {
    let mcp_project = project.with_file_name("mcp-empty-project");
    std::fs::create_dir_all(&mcp_project).expect("create empty MCP project");
    let calls = [
        (
            10,
            json!({
                "name":"backend.dependencies",
                "arguments":{"package":app_coordinate}
            }),
        ),
        (
            11,
            json!({
                "name":"backend.advisory",
                "arguments":{"package":bincode_coordinate}
            }),
        ),
        (
            12,
            json!({
                "name":"backend.index_search",
                "arguments":{"query":exact_lineage_coordinate,"limit":20}
            }),
        ),
        (
            13,
            json!({
                "name":"backend.index_search",
                "arguments":{"query":lineage_name,"limit":20}
            }),
        ),
    ];
    let replies = mcp(endpoint, workspace, &mcp_project, authority, &calls);
    for (id, label, words) in [
        (10, "dependencies", vec!["dependencies", app_coordinate]),
        (11, "advisory", vec!["advisory", bincode_coordinate]),
        (
            12,
            "exact index search",
            vec!["index-search", exact_lineage_coordinate, "--limit", "20"],
        ),
        (
            13,
            "lineage index search",
            vec!["index-search", lineage_name, "--limit", "20"],
        ),
    ] {
        let expected = cli_json(
            endpoint,
            workspace,
            project,
            &words.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>(),
        );
        assert_eq!(
            mcp_structured(&replies[&id], label),
            &expected,
            "CLI and MCP disagree on {label}"
        );
    }
}

#[test]
fn index_tentpole_reconciles_live_facts_and_reopens_from_a_cold_process() {
    let fixture = FixtureRoot::new();
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = surface_matrix::write_authority(fixture.path());
    let socket = endpoint();
    let web = create_javascript_project(fixture.path());
    let rust_project = create_dynamic_rust_project(fixture.path());
    let rust_package = rust_project.to_string_lossy().into_owned();
    let rustsec = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/advisory/fixtures/rustsec")
        .canonicalize()
        .expect("repository RustSec fixture");
    let registry = FakeRegistry::start(
        "cargo",
        vec![
            FakePackage {
                name: "tentpole-lineage".to_owned(),
                version: "1.0.0".to_owned(),
                files: LINEAGE_V1.to_vec(),
            },
            FakePackage {
                name: "tentpole-lineage".to_owned(),
                version: "2.0.0".to_owned(),
                files: LINEAGE_V2.to_vec(),
            },
            FakePackage {
                name: "bincode".to_owned(),
                version: "1.3.3".to_owned(),
                files: BINCODE_FIXTURE.to_vec(),
            },
        ],
    );
    let bincode = "pkg:cargo/bincode@1.3.3";
    let lineage_v1 = "pkg:cargo/tentpole-lineage@1.0.0";
    let lineage_v2 = "pkg:cargo/tentpole-lineage@2.0.0";
    let lineage_versions = BTreeSet::from([lineage_v1.to_owned(), lineage_v2.to_owned()]);
    let app_coordinate = "pkg:npm/tentpole-app@1.0.0";
    let rust_coordinate = "pkg:cargo/tentpole-build-input@1.0.0";
    let mut diagnostics = Diagnostics::default();

    let mut daemon = Locald::launch(
        &socket,
        &workspace,
        &authority,
        registry.endpoint(),
        &rustsec,
    );
    let empty = cli_json(&socket, &workspace, &web, &words(&["health"]));
    assert_eq!(empty["rows"].as_u64(), Some(0), "fresh start was not empty");

    // Real package archives cross the registry, staging, compiler, durable
    // publication, catalog search, and release-lineage surfaces.
    for coordinate in [lineage_v1, lineage_v2, bincode] {
        let add = cli_json(
            &socket,
            &workspace,
            &web,
            &["add".to_owned(), coordinate.to_owned()],
        );
        assert_eq!(add["answer"], "product", "registry add failed: {add}");
    }
    assert!(
        registry
            .requests()
            .iter()
            .any(|request| request.starts_with("/archive/")),
        "registry package fixtures did not cross the archive process path"
    );

    let exact = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["index-search", lineage_v1, "--limit", "20"]),
    );
    assert_eq!(
        registry_coordinates(&exact, "tentpole-lineage"),
        BTreeSet::from([lineage_v1.to_owned()]),
        "exact package search returned a sibling release or lost the requested coordinate: {exact}"
    );
    let lineage = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["index-search", "tentpole-lineage", "--limit", "20"]),
    );
    assert_eq!(
        registry_coordinates(&lineage, "tentpole-lineage"),
        lineage_versions,
        "lineage search did not return exactly the two fixture releases: {lineage}"
    );

    // The large generated tree contains valid JS declarations with unique
    // names. An empty independent search proves none reached the index.
    let add_web = cli_json(
        &socket,
        &workspace,
        &web,
        &["add".to_owned(), web.to_string_lossy().into_owned()],
    );
    assert_eq!(
        add_web["answer"], "product",
        "local web add failed: {add_web}"
    );
    let web_entry = wait_for_cli_title(
        &socket,
        &workspace,
        &web,
        "TentpoleWebEntry",
        "TentpoleWebEntry src/index.js",
    );
    assert!(
        !titles(&web_entry)
            .iter()
            .any(|title| title.starts_with("IgnoredTentpoleBuildDecoy")),
        "expected source fixture search was polluted by generated output: {web_entry}"
    );
    let ignored = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["index-search", "IgnoredTentpoleBuildDecoy", "--limit", "40"]),
    );
    assert_eq!(
        ignored["records"].as_array().map_or(0, Vec::len),
        0,
        "declarations from the multi-megabyte ignored JS tree leaked into search: {ignored}"
    );

    let before_dependencies = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["dependencies", app_coordinate]),
    );
    assert_eq!(
        dependency_titles(&before_dependencies),
        BTreeSet::from(["dep-before ^1.0.0".to_owned()]),
        "the first manifest's independent dependency oracle disagrees: {before_dependencies}"
    );
    std::fs::write(
        web.join("package.json"),
        "{\n  \"name\": \"tentpole-app\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"dep-after\": \"~2.0.0\" },\n  \"devDependencies\": { \"test-fixture\": \"^4.0.0\" }\n}\n",
    )
    .expect("change only manifest dependency facts");
    let refreshed_web = cli_json(
        &socket,
        &workspace,
        &web,
        &["add".to_owned(), web.to_string_lossy().into_owned()],
    );
    assert_eq!(refreshed_web["answer"], "product");
    let expected_dependencies = BTreeSet::from([
        "dep-after ~2.0.0".to_owned(),
        "test-fixture ^4.0.0".to_owned(),
    ]);
    let end = Instant::now() + INDEX_DEADLINE;
    let after_dependencies = loop {
        let current = cli_json(
            &socket,
            &workspace,
            &web,
            &words(&["dependencies", app_coordinate]),
        );
        if dependency_titles(&current) == expected_dependencies {
            break current;
        }
        assert!(
            Instant::now() < end,
            "same package source still answered from old dependency facts: {current}"
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        dependency_titles(&after_dependencies),
        expected_dependencies,
        "updated dependency facts do not match the changed manifest: {after_dependencies}"
    );
    let dependents = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["dependents", "pkg:npm/dep-after@2.0.0"]),
    );
    assert!(
        titles(&dependents)
            .iter()
            .any(|title| title.contains("tentpole-app")),
        "reverse dependency lookup missed the manifest's new package edge: {dependents}"
    );

    // The compiler reads compiler.recipe through include_str! and incorporates
    // it into the function documentation. The file is deliberately outside
    // the source scanner's Rust-file set. Until a language authority can prove
    // a complete present-and-negative read set, the fail-closed scheduler must
    // compile this unchanged Rust source again and replace the stale answer.
    let add_rust = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &[
            "add".to_owned(),
            rust_project.to_string_lossy().into_owned(),
        ],
    );
    assert_eq!(
        add_rust["answer"], "product",
        "Rust fixture add failed: {add_rust}"
    );
    let before_dynamic = wait_for_cli_title(
        &socket,
        &workspace,
        &rust_project,
        "TentpoleDynamicBeforeBeacon",
        "dynamic_compiler_input src/lib.rs",
    );
    let history_before = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&["semantic-versions", &rust_package]),
    );
    let generations_before = history_before["records"].as_array().map_or(0, Vec::len);
    assert!(
        generations_before > 0,
        "Rust fixture published no compiler generation: {history_before}"
    );
    let semantic_store = fixture.path().join("selected-semantic-cache");
    let semantic_store_arg = semantic_store.to_string_lossy().into_owned();
    let semantic_hydration = words(&[
        "semantic-hydrate",
        "--package",
        &rust_package,
        "--coordinate",
        rust_coordinate,
        "--profile",
        "rust-2021",
        "--image-ordinal",
        "0",
        "--plane",
        "core",
        "--store",
        &semantic_store_arg,
    ]);
    let hydrated_before = cli_json(&socket, &workspace, &rust_project, &semantic_hydration);
    assert!(
        hydrated_before["rangeRequests"].as_u64().unwrap_or(0) > 0
            && hydrated_before["transferredBytes"].as_u64().unwrap_or(0) > 0
            && hydrated_before["totalSegments"].as_u64().unwrap_or(0) > 0,
        "the spawned CLI did not hydrate a selected semantic segment through locald: {hydrated_before}"
    );
    std::fs::write(
        rust_project.join("compiler.recipe"),
        "Opaque compiler input after refresh: TentpoleDynamicAfterBeacon.\n",
    )
    .expect("change only the unlisted compiler input");
    let reindex_rust = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &[
            "add".to_owned(),
            rust_project.to_string_lossy().into_owned(),
        ],
    );
    assert_eq!(reindex_rust["answer"], "product");
    let after_dynamic = wait_for_cli_title(
        &socket,
        &workspace,
        &rust_project,
        "TentpoleDynamicAfterBeacon",
        "dynamic_compiler_input src/lib.rs",
    );
    assert_eq!(
        after_dynamic["records"][0]["title"], "dynamic_compiler_input src/lib.rs",
        "new compiler input did not resolve to the expected declaration: {after_dynamic}"
    );
    wait_for_title_absent(
        &socket,
        &workspace,
        &rust_project,
        "TentpoleDynamicBeforeBeacon",
    );
    let history_after = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&["semantic-versions", &rust_package]),
    );
    let generations_after = history_after["records"].as_array().map_or(0, Vec::len);
    assert!(
        generations_after > generations_before,
        "changing the unlisted compiler input did not retain a new immutable generation; before={history_before}, after={history_after}"
    );

    // Hold a cursor for the exact selected generation, then deliberately swap
    // the authority pointer below. The old request must fail as typed stale
    // selection before any range is transferred or acknowledged.
    let semantic_target = SemanticTargetKey::new(
        rust_package.clone(),
        rust_coordinate,
        LanguageProfile::Rust(RustEdition::Rust2021),
    )
    .expect("canonical semantic target");
    let mut stale_client = LocalSemanticIndexClient::connect(&socket, semantic_target)
        .expect("connect selected semantic client");
    let selected_catalog = stale_client
        .fetch_selected_catalog()
        .expect("read selected semantic catalog");
    assert_ne!(
        hydrated_before["selectedRoot"].as_str(),
        Some(backend_engine::encode_id(selected_catalog.selected_root()).as_str()),
        "the selected logical root did not change with the published generation"
    );
    let selected_image = selected_catalog
        .catalog()
        .entries()
        .first()
        .expect("selected image catalog entry")
        .image();
    let selected_manifest = stale_client
        .fetch_selected_manifest(selected_image)
        .expect("read selected semantic manifest");
    let have_ids = Vec::new();
    let mut selected_cursor = stale_client
        .new_cursor(
            &selected_manifest,
            selected_image,
            backend_semantic::ir::SemanticPlaneKind::Ir(
                backend_semantic::ir::SemanticIrPlane::Core,
            ),
            &have_ids,
            TransportLimits {
                max_chunk: 16 * 1024,
                ..TransportLimits::default()
            },
        )
        .expect("create generation-bound cursor");
    let stale_request = match stale_client
        .next_request(
            &mut selected_cursor,
            None,
            HydrationCredits::new(1, 16 * 1024),
        )
        .expect("plan current selected range")
    {
        IrHydrationPoll::Request(request) => request,
        other => panic!("fixture has no missing core segment to swap: {other:?}"),
    };

    let prior_generation = history_after["records"]
        .as_array()
        .and_then(|records| {
            records.iter().find(|record| {
                record["tags"]
                    .as_array()
                    .is_some_and(|tags| !tags.iter().any(|tag| tag.as_str() == Some("selected")))
            })
        })
        .and_then(|record| record["operand"].as_str())
        .expect("history retained the previously selected generation")
        .to_owned();
    let rollback = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&[
            "select-semantic-version",
            &rust_package,
            rust_coordinate,
            "rust",
            &prior_generation,
        ]),
    );
    assert_eq!(rollback["answer"], "product");
    assert!(matches!(
        stale_client.requested_bytes(&stale_request),
        Err(ClientError::StaleSelection)
    ));
    assert!(
        rollback["records"].as_array().is_some_and(|records| {
            records.iter().any(|record| {
                record["tags"].as_array().is_some_and(|tags| {
                    tags.iter().any(|tag| tag.as_str() == Some("selected"))
                        && tags.iter().any(|tag| {
                            tag.as_str()
                                .is_some_and(|tag| tag.starts_with("historical source input "))
                        })
                })
            })
        }),
        "rollback reply omitted its historical source status: {rollback}"
    );
    let rolled_back_history = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&["semantic-versions", &rust_package]),
    );
    let rolled_back_record = rolled_back_history["records"]
        .as_array()
        .and_then(|records| {
            records
                .iter()
                .find(|record| record["operand"].as_str() == Some(prior_generation.as_str()))
        })
        .expect("rollback target remains in immutable history");
    assert!(
        rolled_back_record["tags"].as_array().is_some_and(|tags| {
            tags.iter().any(|tag| tag.as_str() == Some("selected"))
                && tags.iter().any(|tag| {
                    tag.as_str()
                        .is_some_and(|tag| tag.starts_with("historical source input "))
                })
        }),
        "rollback hid that the selected generation uses older source input: {rolled_back_history}"
    );
    let rolled_back_mcp = mcp(
        &socket,
        &workspace,
        &rust_project,
        &authority,
        &[(
            31,
            json!({
                "name":"backend.semantic_versions",
                "arguments":{"package":rust_package}
            }),
        )],
    );
    assert_eq!(
        mcp_structured(&rolled_back_mcp[&31], "rolled-back semantic history"),
        &rolled_back_history,
        "CLI and MCP disagree about historical semantic freshness"
    );

    let recompile_latest = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &[
            "add".to_owned(),
            rust_project.to_string_lossy().into_owned(),
        ],
    );
    assert_eq!(recompile_latest["answer"], "product");
    let current_history = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&["semantic-versions", &rust_package]),
    );
    assert!(
        current_history["records"]
            .as_array()
            .is_some_and(|records| {
                records.iter().any(|record| {
                    record["tags"].as_array().is_some_and(|tags| {
                        tags.iter().any(|tag| tag.as_str() == Some("selected"))
                            && tags
                                .iter()
                                .any(|tag| tag.as_str() == Some("current source input"))
                    })
                })
            }),
        "recompiling the latest source did not restore current freshness: {current_history}"
    );
    let current_history_mcp = mcp(
        &socket,
        &workspace,
        &rust_project,
        &authority,
        &[(
            32,
            json!({
                "name":"backend.semantic_versions",
                "arguments":{"package":rust_package}
            }),
        )],
    );
    assert_eq!(
        mcp_structured(&current_history_mcp[&32], "current semantic history"),
        &current_history,
        "CLI and MCP disagree after the latest source was recompiled"
    );

    let advisory_refresh = cli_json(&socket, &workspace, &web, &words(&["advisory-refresh"]));
    assert_eq!(advisory_refresh["answer"], "product");
    let advisory_before_restart =
        cli_json(&socket, &workspace, &web, &words(&["advisory", bincode]));
    let advisory_records = advisory_before_restart["records"]
        .as_array()
        .unwrap_or_else(|| panic!("advisory omitted records: {advisory_before_restart}"));
    let rustsec_record = advisory_records
        .iter()
        .find(|record| record["title"] == "RUSTSEC-2025-0141")
        .unwrap_or_else(|| panic!("real bincode advisory missing: {advisory_before_restart}"));
    assert!(
        rustsec_record["tags"].as_array().is_some_and(|tags| tags
            .iter()
            .any(|tag| tag.as_str() == Some("statuses: [Unmaintained]"))),
        "bincode's real unmaintained status was lost: {rustsec_record}"
    );
    let decision_record = advisory_records
        .iter()
        .find(|record| record["title"] == "security decision")
        .unwrap_or_else(|| panic!("advisory omitted its decision row: {advisory_before_restart}"));
    assert!(
        decision_record["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| {
                tag.as_str()
                    .is_some_and(|tag| tag.starts_with("freshness: "))
            })),
        "advisory decision did not expose freshness: {decision_record}"
    );

    for _ in 0..DIAGNOSTIC_SAMPLES {
        let value = sampled_query(
            &socket,
            &workspace,
            &web,
            &daemon,
            &mut diagnostics.warm_query_ms,
            &mut diagnostics.peak_rss_kib,
        );
        assert_eq!(
            registry_coordinates(&value, "tentpole-lineage"),
            lineage_versions,
            "warm catalog projection disagreed with the fixture: {value}"
        );
    }

    let before_restart = cli_json(&socket, &workspace, &web, &words(&["health"]));
    let saved_root = before_restart["revision"].clone();
    // Take the comparison snapshots at one settled root. Earlier answers
    // were checked against independent fixture facts, but their revision
    // fields predate later package and advisory publications.
    let saved_dependencies = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["dependencies", app_coordinate]),
    );
    let saved_dependents = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["dependents", "pkg:npm/dep-after@2.0.0"]),
    );
    let saved_exact = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["index-search", lineage_v1, "--limit", "20"]),
    );
    let saved_lineage = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["index-search", "tentpole-lineage", "--limit", "20"]),
    );
    let saved_dynamic = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&[
            "index-search",
            "TentpoleDynamicAfterBeacon",
            "--limit",
            "40",
        ]),
    );
    let saved_semantic_history = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&["semantic-versions", &rust_package]),
    );
    let saved_generation_count = saved_semantic_history["records"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(
        saved_generation_count,
        current_history["records"].as_array().map_or(0, Vec::len)
    );
    let request_count = registry.requests().len();
    daemon.kill_now();
    drop(daemon);
    registry.set_mode(RegistryMode::Down);

    // A separate owner process has no in-memory projection or compiler cache.
    let reopened = Locald::launch(
        &socket,
        &workspace,
        &authority,
        registry.endpoint(),
        &rustsec,
    );
    let after_restart_health = cli_json(&socket, &workspace, &web, &words(&["health"]));
    assert_eq!(
        after_restart_health["revision"], saved_root,
        "cold reopen selected a different source root"
    );
    let reopened_dependencies = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["dependencies", app_coordinate]),
    );
    assert_eq!(
        reopened_dependencies, saved_dependencies,
        "dependency answer changed after cold restart"
    );
    let reopened_dependents = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["dependents", "pkg:npm/dep-after@2.0.0"]),
    );
    assert_eq!(
        reopened_dependents, saved_dependents,
        "reverse dependency answer changed after cold restart"
    );
    let reopened_exact = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["index-search", lineage_v1, "--limit", "20"]),
    );
    assert_eq!(
        registry_coordinates(&reopened_exact, "tentpole-lineage"),
        registry_coordinates(&saved_exact, "tentpole-lineage"),
        "exact search lost or changed the release selected before restart"
    );
    let reopened_lineage = cli_json(
        &socket,
        &workspace,
        &web,
        &words(&["index-search", "tentpole-lineage", "--limit", "20"]),
    );
    assert_eq!(
        registry_coordinates(&reopened_lineage, "tentpole-lineage"),
        registry_coordinates(&saved_lineage, "tentpole-lineage"),
        "lineage search lost or changed releases after cold restart"
    );
    let reopened_dynamic = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&[
            "index-search",
            "TentpoleDynamicAfterBeacon",
            "--limit",
            "40",
        ]),
    );
    assert_eq!(
        reopened_dynamic, saved_dynamic,
        "compiler-owned input answer changed after restart"
    );
    let reopened_history = cli_json(
        &socket,
        &workspace,
        &rust_project,
        &words(&["semantic-versions", &rust_package]),
    );
    assert_eq!(
        reopened_history["records"].as_array().map_or(0, Vec::len),
        saved_generation_count,
        "immutable compiler history changed after cold restart: {reopened_history}"
    );
    assert_eq!(
        reopened_history, saved_semantic_history,
        "cold reopen changed the selected source freshness or history: {reopened_history}"
    );
    let hydrated_after_restart = cli_json(&socket, &workspace, &rust_project, &semantic_hydration);
    assert!(
        hydrated_after_restart["totalSegments"]
            .as_u64()
            .unwrap_or(0)
            > 0
            && hydrated_after_restart["verifiedSegments"]
                .as_u64()
                .is_some_and(|verified| {
                    Some(verified) == hydrated_after_restart["totalSegments"].as_u64()
                })
            && hydrated_after_restart["transferredBytes"]
                .as_u64()
                .unwrap_or(0)
                > 0,
        "cold locald did not re-read the selected semantic metadata and hydrate a missing range: {hydrated_after_restart}"
    );
    let reopened_history_mcp = mcp(
        &socket,
        &workspace,
        &rust_project,
        &authority,
        &[(
            33,
            json!({
                "name":"backend.semantic_versions",
                "arguments":{"package":rust_package}
            }),
        )],
    );
    assert_eq!(
        mcp_structured(&reopened_history_mcp[&33], "reopened semantic history"),
        &reopened_history,
        "cold-reopened CLI and MCP semantic projections disagree"
    );
    let reopened_advisory = cli_json(&socket, &workspace, &web, &words(&["advisory", bincode]));
    let stale_decision = reopened_advisory["records"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|record| record["title"] == "security decision")
        .unwrap_or_else(|| panic!("reopened advisory omitted its decision: {reopened_advisory}"));
    assert!(
        stale_decision["tags"].as_array().is_some_and(|tags| tags
            .iter()
            .any(|tag| tag.as_str() == Some("freshness: Stale"))),
        "cold reopen did not mark its persisted advisory observation stale: {reopened_advisory}"
    );
    assert!(
        reopened_advisory["records"]
            .as_array()
            .is_some_and(|records| records.iter().any(|record| {
                record["title"] == "RUSTSEC-2025-0141"
                    && record["tags"].as_array().is_some_and(|tags| {
                        tags.iter()
                            .any(|tag| tag.as_str().is_some_and(|tag| tag.contains("Unmaintained")))
                    })
            })),
        "cold reopen lost the real advisory claim while marking it stale: {reopened_advisory}"
    );
    assert_eq!(
        registry.requests().len(),
        request_count,
        "cold query unexpectedly reached the unavailable registry"
    );

    assert_mcp_parity(
        &socket,
        &workspace,
        &web,
        &authority,
        app_coordinate,
        bincode,
        lineage_v1,
        "tentpole-lineage",
    );
    for _ in 0..DIAGNOSTIC_SAMPLES {
        let value = sampled_query(
            &socket,
            &workspace,
            &web,
            &reopened,
            &mut diagnostics.reopen_query_ms,
            &mut diagnostics.peak_rss_kib,
        );
        assert_eq!(
            registry_coordinates(&value, "tentpole-lineage"),
            lineage_versions,
            "reopened catalog projection disagreed with the fixture: {value}"
        );
    }
    let fixture_bytes = tree_bytes(fixture.path());
    let workspace_bytes = tree_bytes(&workspace);
    eprintln!(
        "index-tentpole diagnostics warm_ms_p50={:?} warm_ms_p95={:?} reopen_ms_p50={:?} reopen_ms_p95={:?} fixture_bytes={fixture_bytes} workspace_bytes={workspace_bytes} daemon_peak_rss_kib={:?}",
        percentile(&diagnostics.warm_query_ms, 50),
        percentile(&diagnostics.warm_query_ms, 95),
        percentile(&diagnostics.reopen_query_ms, 50),
        percentile(&diagnostics.reopen_query_ms, 95),
        diagnostics.peak_rss_kib,
    );
    drop(reopened);
    let _ = std::fs::remove_file(socket);
}
