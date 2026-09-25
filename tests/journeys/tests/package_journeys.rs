//! Hermetic package-indexing journeys through the real product processes.
//!
//! Each lane publishes one small, real-shaped package from an in-process
//! registry (`src/fake_registry.rs`, which speaks the product's canonical feed
//! protocol) and drives the real `locald`, CLI, MCP, and desktop runtime
//! through the flow a user takes:
//!
//! `add pkg:…` → registry feed and archive download → staging → the
//! language's compiler authority → seal and publish → search → show →
//! related → daemon crash → restart with the registry gone → reopen.
//!
//! Every assertion reads what a person or an agent reads: declaration names,
//! kinds, package-relative paths, languages, signatures, documentation, and
//! the source lines themselves. Nothing here compares counts.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/fake_registry.rs"]
mod fake_registry;

use backend_client::Session;
use backend_library::{CommandReply, RowId};
use fake_registry::{FakeFile, FakePackage, FakeRegistry, RegistryMode};
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

/// Bound on one CLI or MCP process.
const PROCESS_DEADLINE: Duration = Duration::from_secs(120);
/// Bound on a package becoming searchable after `add` returns.
const INDEX_DEADLINE: Duration = Duration::from_secs(150);
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

/// What a reader must see for one declaration of a lane's package.
#[derive(Clone, Copy, Debug)]
struct Expected {
    /// Declaration name, as searched for and as displayed.
    name: &'static str,
    /// Declaration kind label (`function`, `method`, `struct`, ...).
    kind: &'static str,
    /// Package-relative source path.
    path: &'static str,
    /// One-based line of the declaration.
    line: u32,
    /// Text that must appear in the rendered signature.
    signature: &'static str,
    /// Text that must appear in the captured source lines.
    source: &'static str,
    /// Text that must appear in the documentation prose.
    prose: &'static str,
}

/// One ecosystem lane.
#[derive(Clone, Copy, Debug)]
struct Lane {
    /// Registry ecosystem passed to `--registry-ecosystem`.
    ecosystem: &'static str,
    /// Registry-native package name.
    name: &'static str,
    /// Release version.
    version: &'static str,
    /// Presentation language label.
    language: &'static str,
    /// Archive files under `package/`.
    files: &'static [FakeFile],
    /// The entry declaration: a documented function that calls `callee`.
    entry: Expected,
    /// Which lane answers: `semantic` when the language's compiler
    /// authority ran in the gate environment, `declaration` when the
    /// structural baseline is the honest answer.
    shape: &'static str,
    /// A second declaration the entry point calls, when the answering lane
    /// resolves calls (the structural baseline does not).
    callee: Option<&'static str>,
}

const RUST_FILES: &[FakeFile] = &[
    FakeFile {
        path: "Cargo.toml",
        contents: "[package]\nname = \"journey-beacon\"\nversion = \"1.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    },
    FakeFile {
        path: "src/lib.rs",
        contents: "/// A beacon that shines with a fixed intensity.\npub struct Beacon {\n    pub intensity: u64,\n}\n\nimpl Beacon {\n    /// Returns how brightly the beacon shines.\n    pub fn illuminate(&self) -> u64 {\n        self.intensity\n    }\n}\n\n/// Builds a beacon and reads its light.\npub fn beacon_entry(level: u64) -> u64 {\n    Beacon { intensity: level }.illuminate()\n}\n",
    },
];

const RUST: Lane = Lane {
    ecosystem: "cargo",
    name: "journey-beacon",
    version: "1.0.0",
    language: "rust",
    files: RUST_FILES,
    entry: Expected {
        name: "beacon_entry",
        kind: "function",
        path: "src/lib.rs",
        line: 14,
        signature: "u64",
        source: "pub fn beacon_entry(level: u64) -> u64 {",
        prose: "Builds a beacon and reads its light.",
    },
    shape: "semantic",
    callee: Some("illuminate"),
};

const PYTHON_FILES: &[FakeFile] = &[
    FakeFile {
        path: "pyproject.toml",
        contents: "[project]\nname = \"journey-beacon\"\nversion = \"1.0.0\"\n",
    },
    FakeFile {
        path: "journey_beacon/__init__.py",
        contents: "class Beacon:\n    \"\"\"A beacon that shines with a fixed intensity.\"\"\"\n\n    def __init__(self, intensity: int) -> None:\n        self.intensity = intensity\n\n    def illuminate(self) -> int:\n        \"\"\"Returns how brightly the beacon shines.\"\"\"\n        return self.intensity\n\n\ndef beacon_entry(level: int) -> int:\n    \"\"\"Builds a beacon and reads its light.\"\"\"\n    return Beacon(level).illuminate()\n",
    },
];

const PYTHON: Lane = Lane {
    ecosystem: "pypi",
    name: "journey-beacon",
    version: "1.0.0",
    language: "python",
    files: PYTHON_FILES,
    entry: Expected {
        name: "beacon_entry",
        kind: "function",
        path: "journey_beacon/__init__.py",
        line: 12,
        signature: "int",
        source: "def beacon_entry(level: int) -> int:",
        prose: "Builds a beacon and reads its light.",
    },
    shape: "semantic",
    callee: Some("illuminate"),
};

const TYPESCRIPT_FILES: &[FakeFile] = &[
    FakeFile {
        path: "package.json",
        contents: "{\n  \"name\": \"journey-beacon\",\n  \"version\": \"1.0.0\",\n  \"types\": \"index.ts\"\n}\n",
    },
    FakeFile {
        path: "index.ts",
        contents: "/** A beacon that shines with a fixed intensity. */\nexport class Beacon {\n  constructor(readonly intensity: number) {}\n\n  /** Returns how brightly the beacon shines. */\n  illuminate(): number {\n    return this.intensity;\n  }\n}\n\n/** Builds a beacon and reads its light. */\nexport function beaconEntry(level: number): number {\n  return new Beacon(level).illuminate();\n}\n",
    },
];

const TYPESCRIPT: Lane = Lane {
    ecosystem: "npm",
    name: "journey-beacon",
    version: "1.0.0",
    language: "typescript",
    files: TYPESCRIPT_FILES,
    entry: Expected {
        name: "beaconEntry",
        kind: "function",
        path: "index.ts",
        line: 12,
        signature: "number",
        source: "export function beaconEntry(level: number): number {",
        prose: "Builds a beacon and reads its light.",
    },
    shape: "semantic",
    callee: Some("illuminate"),
};

const GO_FILES: &[FakeFile] = &[
    FakeFile {
        path: "go.mod",
        contents: "module github.com/journey/beacon\n\ngo 1.22\n",
    },
    FakeFile {
        path: "beacon.go",
        contents: "// Package beacon shines.\npackage beacon\n\n// Beacon shines with a fixed intensity.\ntype Beacon struct {\n\tIntensity uint64\n}\n\n// Illuminate returns how brightly the beacon shines.\nfunc (b Beacon) Illuminate() uint64 {\n\treturn b.Intensity\n}\n\n// BeaconEntry builds a beacon and reads its light.\nfunc BeaconEntry(level uint64) uint64 {\n\treturn Beacon{Intensity: level}.Illuminate()\n}\n",
    },
];

const GO: Lane = Lane {
    ecosystem: "golang",
    name: "github.com/journey/beacon",
    version: "v1.0.0",
    language: "go",
    files: GO_FILES,
    entry: Expected {
        name: "BeaconEntry",
        kind: "function",
        path: "beacon.go",
        line: 15,
        signature: "uint64",
        source: "func BeaconEntry(level uint64) uint64 {",
        prose: "BeaconEntry builds a beacon and reads its light.",
    },
    shape: "semantic",
    callee: Some("Illuminate"),
};

const JAVA_FILES: &[FakeFile] = &[FakeFile {
    path: "com/journey/beacon/Beacon.java",
    contents: "package com.journey.beacon;\n\n/** A beacon that shines with a fixed intensity. */\npublic final class Beacon {\n    private final long intensity;\n\n    public Beacon(long intensity) {\n        this.intensity = intensity;\n    }\n\n    /** Returns how brightly the beacon shines. */\n    public long illuminate() {\n        return intensity;\n    }\n\n    /** Builds a beacon and reads its light. */\n    public static long beaconEntry(long level) {\n        return new Beacon(level).illuminate();\n    }\n}\n",
}];

/// The daemon compiles every Java file as `--release 25`, and the gate's JDK
/// is Zulu 21, which rejects that release, so the Java compiler authority
/// never answers here and the structural baseline does. When that is fixed
/// this lane fails on `shape` and must be promoted to the semantic answer.
const JAVA: Lane = Lane {
    ecosystem: "maven",
    name: "com.journey:beacon",
    version: "1.0.0",
    language: "java",
    files: JAVA_FILES,
    entry: Expected {
        name: "beaconEntry",
        kind: "method",
        path: "com/journey/beacon/Beacon.java",
        line: 17,
        signature: "long",
        source: "public static long beaconEntry(long level) {",
        prose: "Builds a beacon and reads its light.",
    },
    shape: "declaration",
    callee: None,
};

const CSHARP_FILES: &[FakeFile] = &[
    FakeFile {
        path: "Journey.Beacon.csproj",
        contents: "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n",
    },
    FakeFile {
        path: "Beacon.cs",
        contents: "namespace Journey;\n\n/// <summary>A beacon that shines with a fixed intensity.</summary>\npublic sealed class Beacon\n{\n    public Beacon(ulong intensity) => Intensity = intensity;\n\n    public ulong Intensity { get; }\n\n    /// <summary>Returns how brightly the beacon shines.</summary>\n    public ulong Illuminate() => Intensity;\n\n    /// <summary>Builds a beacon and reads its light.</summary>\n    public static ulong BeaconEntry(ulong level) => new Beacon(level).Illuminate();\n}\n",
    },
];

/// The gate provisions `dotnet` but no Roslyn helper (`NUDOX_ROSLYN_HELPER`),
/// so the C# compiler authority is unconfigured for the daemon and the
/// structural baseline answers. Promote this lane once the helper ships.
const CSHARP: Lane = Lane {
    ecosystem: "nuget",
    name: "journey.beacon",
    version: "1.0.0",
    language: "csharp",
    files: CSHARP_FILES,
    entry: Expected {
        name: "BeaconEntry",
        kind: "method",
        path: "Beacon.cs",
        line: 14,
        signature: "ulong",
        source: "public static ulong BeaconEntry(ulong level) => new Beacon(level).Illuminate();",
        prose: "Builds a beacon and reads its light.",
    },
    shape: "declaration",
    callee: None,
};

const CPP_FILES: &[FakeFile] = &[
    FakeFile {
        path: "conanfile.txt",
        contents: "[requires]\n",
    },
    FakeFile {
        path: "src/beacon.cpp",
        contents: "/// A beacon that shines with a fixed intensity.\nclass Beacon {\npublic:\n    explicit Beacon(unsigned long intensity) : intensity_(intensity) {}\n\n    /// Returns how brightly the beacon shines.\n    unsigned long illuminate() const { return intensity_; }\n\nprivate:\n    unsigned long intensity_;\n};\n\n/// Builds a beacon and reads its light.\nunsigned long beacon_entry(unsigned long level) {\n    return Beacon(level).illuminate();\n}\n",
    },
];

const CPP: Lane = Lane {
    ecosystem: "cpp",
    name: "conan:journey-beacon",
    version: "1.0.0",
    language: "cpp",
    files: CPP_FILES,
    entry: Expected {
        name: "beacon_entry",
        kind: "function",
        path: "src/beacon.cpp",
        line: 14,
        signature: "unsigned long",
        source: "unsigned long beacon_entry(unsigned long level) {",
        prose: "Builds a beacon and reads its light.",
    },
    shape: "semantic",
    callee: Some("illuminate"),
};

// ---------------------------------------------------------------------------
// Process plumbing
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Daemon {
    child: Option<Child>,
    endpoint: PathBuf,
}

impl Daemon {
    fn start(workspace: &Path, authority: &Path, registry: &str, ecosystem: &str) -> Self {
        let endpoint = backend_runtime::derive_endpoint(workspace);
        let args: Vec<OsString> = vec![
            "--endpoint".into(),
            endpoint.as_os_str().to_owned(),
            "--workspace".into(),
            workspace.as_os_str().to_owned(),
            "--profile".into(),
            "builtin".into(),
            "--authority-secret-file".into(),
            authority.as_os_str().to_owned(),
            "--registry-endpoint".into(),
            registry.into(),
            "--registry-ecosystem".into(),
            ecosystem.into(),
            "--max-frame".into(),
            "1048576".into(),
            "--timeout-ms".into(),
            "120000".into(),
            "--idle-timeout-ms".into(),
            "0".into(),
        ];
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"))
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn locald");
        let mut daemon = Self {
            child: Some(child),
            endpoint,
        };
        daemon.wait_ready();
        daemon
    }

    fn wait_ready(&mut self) {
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end {
            let exited = self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().expect("poll locald"));
            assert!(
                exited.is_none(),
                "locald exited before readiness: {exited:?}"
            );
            if std::fs::symlink_metadata(&self.endpoint)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
                && UnixStream::connect(&self.endpoint).is_ok()
            {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("locald never listened at {}", self.endpoint.display());
    }

    /// Kills the daemon without a graceful shutdown: the next process must
    /// recover everything from the durable workspace alone.
    fn crash(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            child.wait().expect("reap crashed locald");
        }
        let end = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&self.endpoint).is_ok() {
            assert!(Instant::now() < end, "crashed locald still answers");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn fresh_root(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-package-journey-{label}-{}-{serial}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("workspace")).expect("create journey workspace");
    root.canonicalize().expect("canonical journey root")
}

fn authority(root: &Path) -> PathBuf {
    let path = root.join("authority.secret");
    std::fs::write(&path, [0x5a_u8; 32]).expect("write authority secret");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("protect authority secret");
    path
}

fn run_bounded(mut command: ProcessCommand, label: &str, input: Option<Vec<u8>>) -> Output {
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
            .expect("child stdin")
            .write_all(&input)
            .unwrap_or_else(|error| panic!("write {label} stdin: {error}"));
    }
    let mut stdout = child.stdout.take().expect("child stdout");
    let mut stderr = child.stderr.take().expect("child stderr");
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let end = Instant::now() + PROCESS_DEADLINE;
    let status = loop {
        match child.try_wait().expect("poll child") {
            Some(status) => break status,
            None if Instant::now() < end => thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{label} exceeded {PROCESS_DEADLINE:?}");
            }
        }
    };
    Output {
        status,
        stdout: out.join().expect("join stdout"),
        stderr: err.join().expect("join stderr"),
    }
}

/// The typed fault a failed CLI command reports, from whichever stream the
/// selected output format writes it to.
fn fault(output: &Output) -> String {
    format!("{}{}", text(&output.stdout), text(&output.stderr))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Runs the real CLI against the daemon in one output format.
fn cli(endpoint: &Path, workspace: &Path, format: &str, words: &[&str]) -> Output {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--format")
        .arg(format)
        .args(words)
        .env("NO_COLOR", "1")
        .env("COLUMNS", "120");
    run_bounded(command, &format!("CLI {words:?}"), None)
}

fn cli_json(endpoint: &Path, workspace: &Path, words: &[&str]) -> Value {
    let output = cli(endpoint, workspace, "json", words);
    assert!(
        output.status.success(),
        "CLI {words:?} failed ({}): stdout={} stderr={}",
        output.status,
        text(&output.stdout),
        text(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI {words:?} printed no JSON ({error}): {}",
            text(&output.stdout)
        )
    })
}

/// Runs one MCP session with the given tool calls and returns replies by id.
fn mcp(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    calls: &[(u64, Value)],
) -> BTreeMap<u64, Value> {
    let mut lines = vec![
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "package-journey", "version": "1" }
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
    // The MCP entry point indexes its `--project` at startup; an empty
    // directory keeps that from touching anything but the package.
    let project = workspace.with_file_name("mcp-project");
    std::fs::create_dir_all(&project).expect("create empty MCP project");
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(&project)
        .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority);
    let output = run_bounded(command, "MCP session", Some(input));
    assert!(
        output.status.success(),
        "MCP failed: {}",
        text(&output.stderr)
    );
    text(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| value["id"].as_u64().map(|id| (id, value)))
        .collect()
}

fn mcp_structured<'a>(replies: &'a BTreeMap<u64, Value>, id: u64, label: &str) -> &'a Value {
    let reply = replies
        .get(&id)
        .unwrap_or_else(|| panic!("MCP {label} got no reply: {replies:?}"));
    assert_eq!(
        reply["result"]["isError"], false,
        "MCP {label} refused: {reply}"
    );
    &reply["result"]["structuredContent"]
}

/// Finds the search record for one declaration of the package.
fn record_named<'a>(search: &'a Value, expected: &Expected) -> Option<&'a Value> {
    search["records"].as_array()?.iter().find(|record| {
        record["identity"]["name"] == expected.name && record["identity"]["path"] == expected.path
    })
}

/// Polls CLI search until the package's entry declaration is published.
fn wait_for_record(endpoint: &Path, workspace: &Path, expected: &Expected) -> Value {
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let search = cli_json(endpoint, workspace, &["search", expected.name]);
        if let Some(record) = record_named(&search, expected) {
            return record.clone();
        }
        assert!(
            Instant::now() < end,
            "`{}` never became searchable at {}: {search}",
            expected.name,
            expected.path
        );
        thread::sleep(Duration::from_millis(200));
    }
}

fn assert_record(record: &Value, lane: &Lane) {
    let expected = &lane.entry;
    assert_eq!(
        record["identity"]["shape"], lane.shape,
        "the {} lane answered from an unexpected lane: {record}",
        lane.language
    );
    assert_eq!(record["kind"], expected.kind, "record kind: {record}");
    assert_eq!(
        record["language"], lane.language,
        "record language: {record}"
    );
    assert_eq!(
        record["identity"]["line"], expected.line,
        "record line: {record}"
    );
    assert_eq!(
        record["summary"], expected.prose,
        "record summary is not the declaration's documentation: {record}"
    );
}

/// Asserts the declaration page a reader sees for the entry point.
fn assert_page(page: &Value, lane: &Lane, coordinate: &str) {
    let expected = &lane.entry;
    assert_eq!(
        page["identity"]["coordinate"], coordinate,
        "page identity: {page}"
    );
    assert_eq!(page["identity"]["name"], expected.name, "page name: {page}");
    assert_eq!(page["identity"]["path"], expected.path, "page path: {page}");
    assert_eq!(page["kind"], expected.kind, "page kind: {page}");
    assert_eq!(page["language"], lane.language, "page language: {page}");
    let signature = page["signature"].as_str().unwrap_or_default();
    assert!(
        signature.contains(expected.signature),
        "page signature lost `{}`: {page}",
        expected.signature
    );
    let prose = page["prose"]
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
        prose.contains(expected.prose),
        "page documentation lost `{}`: {page}",
        expected.prose
    );
    let source = &page["source"];
    assert_eq!(source["path"], expected.path, "page source site: {page}");
    assert_eq!(source["line"], expected.line, "page source line: {page}");
    let lines = source["lines"]
        .as_array()
        .unwrap_or_else(|| panic!("page carries no source lines: {page}"));
    assert!(
        lines
            .iter()
            .filter_map(Value::as_str)
            .any(|line| line.contains(expected.source)),
        "page source lost `{}`: {page}",
        expected.source
    );
}

/// Adds the lane's package through the real CLI and returns the entry
/// declaration's exact coordinate once it is searchable.
fn add_and_index(endpoint: &Path, workspace: &Path, lane: &Lane, purl: &str) -> String {
    let added = cli(endpoint, workspace, "json", &["add", purl]);
    assert!(
        added.status.success(),
        "add {purl} failed: stdout={} stderr={}",
        text(&added.stdout),
        text(&added.stderr)
    );
    let record = wait_for_record(endpoint, workspace, &lane.entry);
    assert_record(&record, lane);
    record["identity"]["coordinate"]
        .as_str()
        .unwrap_or_else(|| panic!("record without coordinate: {record}"))
        .to_owned()
}

fn fake_package(lane: &Lane) -> FakePackage {
    FakePackage {
        name: lane.name.to_owned(),
        version: lane.version.to_owned(),
        files: lane.files.to_vec(),
    }
}

/// Drives one ecosystem lane from registry to reopen.
fn run_lane(lane: &Lane) {
    let root = fresh_root(lane.ecosystem);
    let workspace = root.join("workspace");
    let authority = authority(&root);
    let registry = FakeRegistry::start(lane.ecosystem, vec![fake_package(lane)]);
    let purl = fake_registry::canonical_purl(lane.ecosystem, lane.name, lane.version);
    let mut daemon = Daemon::start(&workspace, &authority, registry.endpoint(), lane.ecosystem);
    let endpoint = daemon.endpoint.clone();

    // add → download → compile → publish → searchable.
    let coordinate = add_and_index(&endpoint, &workspace, lane, &purl);
    let downloads = registry.requests();
    assert!(
        downloads.iter().any(|path| path.starts_with("/archive/")),
        "add never downloaded the archive: {downloads:?}"
    );

    // show: the page a reader opens.
    let page = cli_json(&endpoint, &workspace, &["show", &coordinate]);
    assert_page(&page, lane, &coordinate);

    // graph: the entry point's outgoing call reaches its callee.
    if let Some(callee) = lane.callee {
        let graph = cli_json(&endpoint, &workspace, &["graph", &coordinate]);
        let calls = graph["records"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|record| record["identity"]["name"] == callee);
        assert!(
            calls,
            "graph of {} lost its call to `{callee}`: {graph}",
            lane.entry.name
        );
    }

    // packages: the package is on the shelf under its canonical URL.
    let packages = cli_json(&endpoint, &workspace, &["packages"]);
    assert!(
        packages.to_string().contains(&purl),
        "shelf lost {purl}: {packages}"
    );

    // MCP answers the same revision with the same content.
    let replies = mcp(
        &endpoint,
        &workspace,
        &authority,
        &[
            (
                10,
                json!({"name":"backend.search","arguments":{"query":lane.entry.name,"limit":20}}),
            ),
            (
                11,
                json!({"name":"backend.document","arguments":{"coordinate":coordinate}}),
            ),
        ],
    );
    let mcp_search = mcp_structured(&replies, 10, "search");
    let mcp_record = record_named(mcp_search, &lane.entry)
        .unwrap_or_else(|| panic!("MCP search lost {}: {mcp_search}", lane.entry.name));
    assert_record(mcp_record, lane);
    assert_eq!(
        mcp_structured(&replies, 11, "document"),
        &page,
        "MCP and CLI render different pages for {coordinate}"
    );

    // The desktop runtime reads the same declaration through its session.
    assert_desktop(&endpoint, &coordinate, lane);

    // Crash the daemon, take the registry away, and reopen: everything the
    // user saw must come back from the durable workspace alone.
    daemon.crash();
    registry.set_mode(RegistryMode::Down);
    let before_restart = registry.requests().len();
    let mut reopened = Daemon::start(&workspace, &authority, registry.endpoint(), lane.ecosystem);
    let reopened_page = cli_json(&reopened.endpoint, &workspace, &["show", &coordinate]);
    assert_eq!(
        reopened_page, page,
        "the reopened workspace renders a different page"
    );
    let again = cli(&reopened.endpoint, &workspace, "json", &["add", &purl]);
    assert!(
        again.status.success(),
        "re-adding a durable package needed the registry: stdout={} stderr={}",
        text(&again.stdout),
        text(&again.stderr)
    );
    assert_eq!(
        registry.requests().len(),
        before_restart,
        "reopen or re-add reached the network: {:?}",
        registry.requests()
    );
    reopened.crash();
    drop(registry);
    let _ = std::fs::remove_dir_all(root);
}

/// The desktop runtime's session reads the same page facts.
fn assert_desktop(endpoint: &Path, coordinate: &str, lane: &Lane) {
    let mut session = Session::connect(endpoint).expect("desktop session");
    let search = session
        .search(lane.entry.name, 20)
        .unwrap_or_else(|error| panic!("desktop search: {error}"));
    let CommandReply::Search(search) = search.reply else {
        panic!("desktop search changed shape");
    };
    assert!(
        search
            .root
            .rows()
            .iter()
            .any(|row| row.label == coordinate && matches!(row.id, RowId::Symbol(_))),
        "desktop search lost {coordinate}"
    );
    let document = session
        .document(coordinate)
        .unwrap_or_else(|error| panic!("desktop document: {error}"));
    let CommandReply::Document(document) = document.reply else {
        panic!("desktop document changed shape");
    };
    let site = document
        .location
        .captured()
        .unwrap_or_else(|| panic!("desktop document lost its source site: {document:?}"));
    assert_eq!(site.path(), lane.entry.path);
    assert_eq!(site.start_line(), lane.entry.line);
    assert!(
        document
            .excerpt
            .text()
            .is_some_and(|excerpt| excerpt.contains(lane.entry.source)),
        "desktop document lost its source text: {:?}",
        document.excerpt
    );
}

#[test]
fn registry_package_journey_rust() {
    run_lane(&RUST);
}

#[test]
fn registry_package_journey_python() {
    run_lane(&PYTHON);
}

#[test]
fn registry_package_journey_typescript() {
    run_lane(&TYPESCRIPT);
}

#[test]
fn registry_package_journey_go() {
    run_lane(&GO);
}

#[test]
fn registry_package_journey_java() {
    run_lane(&JAVA);
}

#[test]
fn registry_package_journey_csharp() {
    run_lane(&CSHARP);
}

#[test]
fn registry_package_journey_cpp() {
    run_lane(&CPP);
}

/// A live subscription opened before a daemon crash resumes afterwards on the
/// durable root: the reopened owner answers the pre-crash cursor rather than
/// losing the reader's place, and the root it answers still holds the
/// package's declaration.
#[test]
fn a_live_subscription_resumes_across_a_daemon_crash() {
    let root = fresh_root("subscription");
    let workspace = root.join("workspace");
    let authority = authority(&root);
    let registry = FakeRegistry::start("pypi", vec![fake_package(&PYTHON)]);
    let mut daemon = Daemon::start(&workspace, &authority, registry.endpoint(), "pypi");
    let purl = fake_registry::canonical_purl("pypi", PYTHON.name, PYTHON.version);
    let coordinate = add_and_index(&daemon.endpoint, &workspace, &PYTHON, &purl);
    let mut live = backend_client::LocalSubscriptionTransport::connect(&daemon.endpoint)
        .expect("connect live subscription");
    let (before, cursor) = live.bootstrap_root().expect("bootstrap live subscription");
    assert!(
        before.rows().iter().any(|row| row.label == coordinate),
        "the bootstrapped root lost {coordinate}"
    );
    drop(live);

    daemon.crash();
    registry.set_mode(RegistryMode::Down);
    let reopened = Daemon::start(&workspace, &authority, registry.endpoint(), "pypi");
    let mut resumed = backend_client::LocalSubscriptionTransport::connect(&reopened.endpoint)
        .expect("reconnect live subscription");
    let read = resumed
        .subscribe_with_certificate(
            backend_client::SubscriptionRequest::new(cursor, 64).expect("subscription credit"),
            None,
        )
        .expect("resume the live subscription after the crash");
    match read {
        backend_library::CursorRead::Events { cursor: after, .. } => {
            assert_eq!(after.root(), before.root(), "events resumed on another root");
        }
        backend_library::CursorRead::Reset { cursor: after, root, .. } => {
            assert_eq!(after.root(), root.root());
            assert!(
                root.rows().iter().any(|row| row.label == coordinate),
                "the reset root lost {coordinate}"
            );
        }
    }
    drop(reopened);
    drop(registry);
    let _ = std::fs::remove_dir_all(root);
}

/// The failure paths a user meets while adding packages, each asserted as the
/// typed message the CLI prints, and each followed by proof that the failure
/// left nothing behind that a later success would trip over.
///
/// A canonical feed page is committed with every archive it lists, so the
/// first successful poll downloads them all; each fault is therefore staged
/// before the registry has served a single archive.
#[test]
fn registry_failures_are_typed_and_leave_no_residue() {
    let root = fresh_root("failures");
    let workspace = root.join("workspace");
    let authority = authority(&root);
    let registry = FakeRegistry::start("cargo", vec![fake_package(&RUST)]);
    let daemon = Daemon::start(&workspace, &authority, registry.endpoint(), "cargo");
    let endpoint = daemon.endpoint.clone();
    let purl = "pkg:cargo/journey-beacon@1.0.0";

    // The registry is down.
    registry.set_mode(RegistryMode::Down);
    let down = cli(&endpoint, &workspace, "json", &["add", purl]);
    assert!(
        !down.status.success(),
        "an add succeeded with the registry down"
    );
    let down_fault = fault(&down);
    assert!(
        down_fault.contains("unavailable") || down_fault.contains("retry"),
        "registry outage error is not typed: {down_fault}"
    );

    // The registry answers the feed, then drops every archive connection
    // halfway through its body.
    registry.set_mode(RegistryMode::TruncateArchives);
    let truncated = cli(&endpoint, &workspace, "json", &["add", purl]);
    assert!(
        !truncated.status.success(),
        "a truncated download was admitted: requests={:?} output={}",
        registry.requests(),
        fault(&truncated)
    );
    let truncated_fault = fault(&truncated);
    assert!(
        truncated_fault.contains(purl),
        "truncated download error does not name the package: {truncated_fault}"
    );
    assert!(
        registry
            .requests()
            .iter()
            .any(|path| path.starts_with("/archive/")),
        "the truncation was never exercised: {:?}",
        registry.requests()
    );
    let shelf = cli_json(&endpoint, &workspace, &["packages"]);
    assert!(
        !shelf.to_string().contains("journey-beacon"),
        "a failed download reached the shelf: {shelf}"
    );

    // Recovery: the same add now succeeds and indexes real content, so
    // neither failure poisoned the durable acquisition state.
    registry.set_mode(RegistryMode::Serve);
    let coordinate = add_and_index(&endpoint, &workspace, &RUST, purl);
    let page = cli_json(&endpoint, &workspace, &["show", &coordinate]);
    assert_page(&page, &RUST, &coordinate);

    // A release the registry does not publish is a typed not-found, not a
    // hang and not an empty success.
    let absent = cli(
        &endpoint,
        &workspace,
        "json",
        &["add", "pkg:cargo/journey-absent@9.9.9"],
    );
    assert!(
        !absent.status.success(),
        "adding an unpublished release succeeded"
    );
    let absent_fault = fault(&absent);
    assert!(
        absent_fault.contains("registry package was not found"),
        "unpublished release error is not typed: {absent_fault}"
    );
    drop(daemon);
    drop(registry);
    let _ = std::fs::remove_dir_all(root);
}
