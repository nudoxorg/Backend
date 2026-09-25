//! Shared production-path surface-matrix fixtures and assertions.
//!
//! The matrix intentionally describes languages as data.  Every case uses the
//! same ingest, CLI, MCP, and desktop journey; a new extension is one table
//! row and one source specimen rather than another branch of test logic.

#![cfg(unix)]
#![allow(unreachable_pub)]

use backend_client::Session;
use backend_desktop::{
    core::{LocalProjectId, VersionedRoot},
    navigation::RequestId,
    runtime::{CancellationToken, EngineClient, EngineDto, EngineRequest, LocalEngineClient},
};
use backend_library::{CommandReply, Cursor, RowId, ViewStateRoot, view_state_root};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Bounded wait used by every process boundary in the matrix.
pub const PROCESS_DEADLINE: Duration = Duration::from_secs(90);
/// Bounded wait used while the daemon composes and publishes its first root.
pub const READY_DEADLINE: Duration = Duration::from_secs(90);

/// One claimed source language and its production-path specimen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LanguageCase {
    /// Stable display label emitted by CLI, MCP, and desktop projections.
    pub language: &'static str,
    /// Source-relative path written to the fixture.
    pub path: &'static str,
    /// Search needle and declaration name expected from that path.
    pub name: &'static str,
    /// Source specimen that must be retained by the index.
    pub source: &'static str,
}

/// Every source language claimed by the built-in frontend set.
pub const LANGUAGE_CASES: [LanguageCase; 9] = [
    LanguageCase {
        language: "rust",
        path: "src/lib.rs",
        name: "RustBeaconEntry",
        source: "pub struct RustBeacon { pub intensity: u64 }\n\nimpl RustBeacon {\n    pub fn illuminate(&self) -> u64 { self.intensity }\n}\n\npub fn RustBeaconEntry() -> u64 { RustBeacon { intensity: 8 }.illuminate() }\n",
    },
    LanguageCase {
        language: "csharp",
        path: "src/CSharpBeacon.cs",
        name: "CSharpBeacon",
        source: "public sealed class CSharpBeacon {\n    public ulong Intensity { get; }\n    public CSharpBeacon(ulong intensity) => Intensity = intensity;\n    public ulong Illuminate() => Intensity;\n}\n",
    },
    LanguageCase {
        language: "java",
        path: "src/JavaBeacon.java",
        name: "JavaBeaconEntry",
        source: "public final class JavaBeacon {\n    private final long intensity;\n    public JavaBeacon(long intensity) { this.intensity = intensity; }\n    public long illuminate() { return intensity; }\n    public static long JavaBeaconEntry() { return new JavaBeacon(4).illuminate(); }\n}\n",
    },
    LanguageCase {
        language: "typescript",
        path: "src/beacon.js",
        name: "JavaScriptBeaconEntry",
        source: "export class JavaScriptBeacon {\n  constructor(intensity) { this.intensity = intensity; }\n  illuminate() { return this.intensity; }\n}\nexport function JavaScriptBeaconEntry() { return new JavaScriptBeacon(3).illuminate(); }\n",
    },
    LanguageCase {
        language: "typescript",
        path: "src/beacon.ts",
        name: "TypeScriptBeaconEntry",
        source: "export class TypeScriptBeacon {\n  constructor(readonly intensity: number) {}\n  illuminate(): number { return this.intensity; }\n}\nexport function TypeScriptBeaconEntry(): number { return new TypeScriptBeacon(5).illuminate(); }\n",
    },
    LanguageCase {
        language: "python",
        path: "src/beacon.py",
        name: "PythonBeaconEntry",
        source: "class PythonBeacon:\n    def __init__(self, intensity: int):\n        self.intensity = intensity\n    def illuminate(self) -> int:\n        return self.intensity\n\ndef PythonBeaconEntry() -> int:\n    return PythonBeacon(6).illuminate()\n",
    },
    LanguageCase {
        language: "go",
        path: "src/beacon.go",
        name: "GoBeaconEntry",
        source: "package beacon\n\ntype GoBeacon struct { Intensity uint64 }\n\nfunc (beacon GoBeacon) Illuminate() uint64 { return beacon.Intensity }\n\nfunc GoBeaconEntry() uint64 { return GoBeacon{Intensity: 7}.Illuminate() }\n",
    },
    LanguageCase {
        language: "c",
        path: "src/c_beacon.c",
        name: "c_beacon_entry",
        source: "typedef struct CBeacon { unsigned long intensity; } CBeacon;\n\nunsigned long c_beacon_entry(CBeacon beacon) { return beacon.intensity; }\n",
    },
    LanguageCase {
        language: "cpp",
        path: "src/cpp_beacon.cpp",
        name: "CppBeacon",
        source: "class CppBeacon {\npublic:\n    explicit CppBeacon(unsigned long intensity) : intensity_(intensity) {}\n    unsigned long illuminate() const { return intensity_; }\nprivate:\n    unsigned long intensity_;\n};\n\nunsigned long cpp_beacon_entry() { return CppBeacon(9).illuminate(); }\n",
    },
];

/// Writes one real multi-language project accepted by the production scanner.
pub fn write_polyglot(root: &Path) -> PathBuf {
    let project = root.join("polyglot");
    std::fs::create_dir_all(project.join("src")).expect("create polyglot source directory");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"surface-matrix\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[dependencies]\n",
    )
    .expect("write Cargo manifest");
    for case in LANGUAGE_CASES {
        std::fs::write(project.join(case.path), case.source).unwrap_or_else(|error| {
            panic!("write {}: {error}", case.path);
        });
    }
    project.canonicalize().expect("canonical polyglot project")
}

/// Writes the owner-only authority credential used by the real daemon.
pub fn write_authority(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("authority.secret");
    std::fs::write(&path, [0x5a_u8; 32]).expect("write authority secret");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat authority secret")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&path, permissions).expect("protect authority secret");
    path
}

/// Builds explicit locald arguments so every matrix lane uses one workspace
/// and one endpoint spelling.
pub fn locald_args(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    idle_timeout_ms: Option<u64>,
    offline: bool,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--endpoint"),
        endpoint.as_os_str().to_owned(),
        OsString::from("--workspace"),
        workspace.as_os_str().to_owned(),
        OsString::from("--profile"),
        OsString::from("builtin"),
        OsString::from("--authority-secret-file"),
        authority.as_os_str().to_owned(),
        OsString::from("--max-frame"),
        OsString::from("1048576"),
        OsString::from("--timeout-ms"),
        OsString::from("180000"),
    ];
    if let Some(idle_timeout_ms) = idle_timeout_ms {
        args.extend([
            OsString::from("--idle-timeout-ms"),
            OsString::from(idle_timeout_ms.to_string()),
        ]);
    }
    if offline {
        args.push(OsString::from("--registry-offline"));
    }
    args
}

/// Runs one bounded child process and captures both streams.
pub fn run_bounded(mut command: ProcessCommand, label: &str, input: Option<Vec<u8>>) -> Output {
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
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read child stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read child stderr");
        bytes
    });
    let end = Instant::now() + PROCESS_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < end => thread::sleep(Duration::from_millis(10)),
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
        stdout: stdout_reader.join().expect("join child stdout"),
        stderr: stderr_reader.join().expect("join child stderr"),
    }
}

/// One status identity projected by every product surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusIdentity {
    /// Exact immutable visible view root.
    pub revision: String,
    /// Exact source object/facet identity when a detailed status projection exposes it.
    pub source: Option<String>,
    /// Number of rows committed by the root.
    pub rows: u64,
    /// Owner sequence attached to the root when a detailed status projection exposes it.
    pub sequence: Option<u64>,
    /// Active project path, when one was selected.
    pub project: Option<String>,
}

/// Reads a status identity from the CLI JSON or MCP structured projection.
pub fn status_identity(value: &Value) -> StatusIdentity {
    StatusIdentity {
        revision: value["revision"]
            .as_str()
            .unwrap_or_else(|| panic!("status omitted revision: {value}"))
            .to_owned(),
        source: value["source"].as_str().map(ToOwned::to_owned),
        rows: value["rows"]
            .as_u64()
            .unwrap_or_else(|| panic!("status omitted rows: {value}")),
        sequence: value["sequence"].as_u64(),
        project: value["project"].as_str().map(ToOwned::to_owned),
    }
}

/// Reads the status structured body from an MCP JSON-RPC reply.
pub fn mcp_structured(reply: &Value) -> &Value {
    reply
        .get("result")
        .and_then(|result| result.get("structuredContent"))
        .unwrap_or_else(|| panic!("MCP reply omitted structuredContent: {reply}"))
}

/// Reads the text block from an MCP JSON-RPC reply.
pub fn mcp_text(reply: &Value) -> &str {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("MCP reply omitted text content: {reply}"))
}

/// Returns all exact row identities in a search projection.
pub fn search_identities(value: &Value) -> BTreeMap<String, RowIdentity> {
    value["records"]
        .as_array()
        .unwrap_or_else(|| panic!("search omitted records: {value}"))
        .iter()
        .filter_map(|record| {
            let identity = &record["identity"];
            // Search also returns semantic identities. Those intentionally
            // have no package-relative source path, so they are not rows in
            // this source-surface matrix.
            let path = identity["path"].as_str()?.to_owned();
            let coordinate = identity["coordinate"]
                .as_str()
                .unwrap_or_else(|| panic!("record omitted coordinate: {record}"))
                .to_owned();
            let name = identity["name"]
                .as_str()
                .unwrap_or_else(|| panic!("record omitted name: {record}"))
                .to_owned();
            let key = identity["key"]
                .as_str()
                .unwrap_or_else(|| panic!("record omitted stable key tag: {record}"))
                .to_owned();
            let language = record["language"]
                .as_str()
                .unwrap_or_else(|| panic!("record omitted language: {record}"))
                .to_owned();
            Some((
                coordinate.clone(),
                RowIdentity {
                    coordinate,
                    path,
                    name,
                    key,
                    language,
                },
            ))
        })
        .collect()
}

/// One exact identity projected by CLI, MCP, and desktop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowIdentity {
    /// Full coordinate accepted by follow-up calls.
    pub coordinate: String,
    /// Package-relative source path.
    pub path: String,
    /// Declaration name.
    pub name: String,
    /// Eight-hex presentation abbreviation of the stable key.
    pub key: String,
    /// Stable lowercase presentation language.
    pub language: String,
}

/// Finds the expected case row in one search result.
pub fn case_identity(value: &Value, case: LanguageCase) -> RowIdentity {
    let rows = search_identities(value);
    let found = rows
        .values()
        .find(|row| row.path == case.path && row.name == case.name)
        .unwrap_or_else(|| {
            panic!(
                "search `{}` omitted {}:{}; rows: {rows:?}",
                case.name, case.path, case.name
            )
        });
    assert_eq!(
        found.language, case.language,
        "truthful language label for {}",
        case.path
    );
    found.clone()
}

/// Collects all coordinates in an outline tree.
pub fn outline_coordinates(value: &Value) -> BTreeSet<String> {
    fn walk(node: &Value, found: &mut BTreeSet<String>) {
        if let Some(coordinate) = node["coordinate"].as_str() {
            found.insert(coordinate.to_owned());
        }
        if let Some(children) = node["children"].as_array() {
            for child in children {
                walk(child, found);
            }
        }
    }
    let mut found = BTreeSet::new();
    for root in value["roots"].as_array().unwrap_or(&Vec::new()) {
        walk(root, &mut found);
    }
    found
}

/// Indexes a fresh project and waits for a non-empty admitted root.
pub fn fresh_ingest(endpoint: &Path, project: &Path) -> backend_library::HealthReport {
    let mut session = Session::connect(endpoint).expect("connect fresh-ingest session");
    let before = session.health().expect("fresh health");
    assert_eq!(before.row_count(), 0, "fresh workspace already has rows");
    session
        .index(&project.to_string_lossy())
        .expect("submit fresh index");
    let end = Instant::now() + READY_DEADLINE;
    loop {
        let report = session.health().expect("poll fresh health");
        if report.row_count() > 1 {
            assert_ne!(report.revision().root(), view_state_root(&[]));
            return report;
        }
        assert!(Instant::now() < end, "fresh ingest never published rows");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Exercises desktop semantic probes against the same admitted root.
pub fn desktop_probe(
    endpoint: &Path,
    project: &Path,
    expected_root: ViewStateRoot,
    expected: &BTreeMap<String, RowIdentity>,
) {
    let mut session = Session::connect(endpoint).expect("connect desktop probe session");
    let packages = session.packages().expect("desktop packages");
    let CommandReply::Packages(packages) = packages.reply else {
        panic!("desktop packages reply changed shape");
    };
    assert_eq!(
        packages.root.basis().root,
        expected_root,
        "desktop package source basis drifted"
    );
    let basis = VersionedRoot::from_revision(1, Cursor::at(expected_root, 0), 0);
    let request = RequestId::new(1);
    let project_id = LocalProjectId::from_path(project).expect("desktop project identity");
    let mut desktop = LocalEngineClient::new(endpoint, project_id);
    let mapped = desktop
        .execute(&EngineRequest::Root {
            request,
            basis,
            cancel: CancellationToken::new(),
        })
        .expect("desktop runtime adapter admits production root");
    let EngineDto::Root {
        key,
        project: desktop_project,
        ..
    } = mapped
    else {
        panic!("desktop root request changed shape");
    };
    assert_eq!(key.root(), expected_root);
    assert_eq!(
        desktop_project.as_ref().map(|value| value.label.as_ref()),
        Some(project.to_string_lossy().as_ref()),
        "desktop runtime lost the selected project identity"
    );

    for (coordinate, identity) in expected {
        let search = session
            .search(&identity.name, 20)
            .unwrap_or_else(|error| panic!("desktop search {}: {error}", identity.name));
        let CommandReply::Search(search) = search.reply else {
            panic!("desktop search reply changed shape for {coordinate}");
        };
        assert_eq!(
            search.root.basis().root,
            expected_root,
            "desktop search source basis drifted"
        );
        let desktop_rows = search
            .root
            .rows()
            .iter()
            .filter(|row| matches!(row.id, RowId::Symbol(_)))
            .map(|row| (row.label.clone(), row.id.stable_key()))
            .collect::<BTreeMap<_, _>>();
        let stable = desktop_rows
            .get(coordinate)
            .unwrap_or_else(|| panic!("desktop search omitted exact row {coordinate}"));
        let tag = stable
            .split_once(':')
            .and_then(|(_, value)| value.get(..8))
            .unwrap_or_else(|| panic!("desktop stable key malformed: {stable}"));
        assert_eq!(
            tag, identity.key,
            "desktop stable key changed for {coordinate}"
        );
        let document = session
            .document(coordinate)
            .unwrap_or_else(|error| panic!("desktop document {coordinate}: {error}"));
        let CommandReply::Document(document) = document.reply else {
            panic!("desktop document reply changed shape for {coordinate}");
        };
        assert_eq!(
            document.basis, expected_root,
            "desktop document basis drifted"
        );
        assert!(
            document.location.captured().is_some(),
            "desktop document lost source site"
        );
        let source = session
            .source(coordinate)
            .unwrap_or_else(|error| panic!("desktop source {coordinate}: {error}"));
        let CommandReply::Document(source) = source.reply else {
            panic!("desktop source reply changed shape for {coordinate}");
        };
        assert_eq!(source.basis, expected_root, "desktop source basis drifted");
        let graph = session
            .graph(coordinate)
            .unwrap_or_else(|error| panic!("desktop graph {coordinate}: {error}"));
        let CommandReply::Graph(graph) = graph.reply else {
            panic!("desktop graph reply changed shape for {coordinate}");
        };
        assert_eq!(
            graph.root.basis().root,
            expected_root,
            "desktop graph source basis drifted"
        );
    }

    let outline = session
        .outline(&project.to_string_lossy())
        .expect("desktop outline route");
    let CommandReply::Outline(outline) = outline.reply else {
        panic!("desktop outline reply changed shape");
    };
    assert_eq!(
        outline.basis, expected_root,
        "desktop outline basis drifted"
    );
    for (coordinate, identity) in expected {
        let names = session
            .names(&identity.name, 20)
            .unwrap_or_else(|error| panic!("desktop names {}: {error}", identity.name));
        let CommandReply::Names(names) = names.reply else {
            panic!("desktop names reply changed shape for {coordinate}");
        };
        assert_eq!(
            names.root.basis().root,
            expected_root,
            "desktop names source basis drifted"
        );
        assert!(
            names.root.rows().iter().any(|row| {
                let stable = row.id.stable_key();
                row.label.contains(&identity.name)
                    && stable
                        .strip_prefix("symbol:")
                        .and_then(|value| value.get(..8))
                        == Some(identity.key.as_str())
            }),
            "desktop names omitted exact row {coordinate}"
        );
    }
}

/// Sends the owner’s public lifecycle shutdown frame and waits for acceptance.
pub fn graceful_shutdown(endpoint: &Path) {
    let mut stream = UnixStream::connect(endpoint).expect("connect graceful shutdown");
    let mut limits = backend_locald::FrameLimits {
        max_frame: 1_048_576,
        ..backend_locald::FrameLimits::default()
    };
    limits.transport.max_frame = limits.max_frame;
    limits.transport.max_chunk = limits.transport.max_chunk.min(limits.max_frame);
    let payload =
        backend_locald::encode_engine_request(1, &backend_locald::EngineRequest::Shutdown, limits)
            .expect("encode graceful shutdown");
    let frame = backend_locald::frame(&payload, limits).expect("frame graceful shutdown");
    stream
        .write_all(&frame)
        .and_then(|()| stream.flush())
        .expect("write graceful shutdown");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set graceful shutdown timeout");
    let response =
        backend_locald::read_frame(&mut stream, limits).expect("read graceful shutdown response");
    match backend_locald::decode_response(&response, limits).expect("decode graceful shutdown") {
        backend_locald::ResponseFrame::Engine { request_id, status }
            if request_id == 1 && matches!(status, backend_locald::EngineStatus::Accepted) => {}
        other => panic!("graceful shutdown was not accepted: {other:?}"),
    }
}

/// Builds one MCP JSON-RPC request batch for all matrix probes.
pub fn mcp_calls(
    project: &Path,
    dependency_package: &str,
    expected: &BTreeMap<String, RowIdentity>,
) -> Vec<(u64, &'static str, Value)> {
    let mut calls = vec![
        (2, "tools/list", json!({})),
        (
            3,
            "tools/call",
            json!({"name":"backend.status","arguments":{}}),
        ),
        (
            4,
            "tools/call",
            json!({"name":"backend.outline","arguments":{"path":project.to_string_lossy()}}),
        ),
        (
            5,
            "tools/call",
            json!({"name":"backend.dependencies","arguments":{"package":dependency_package}}),
        ),
    ];
    let mut id = 10_u64;
    let mut identities = expected.values().collect::<Vec<_>>();
    identities.sort_by_key(|identity| {
        LANGUAGE_CASES
            .iter()
            .position(|case| case.path == identity.path && case.name == identity.name)
            .unwrap_or(usize::MAX)
    });
    for identity in identities {
        calls.push((
            id,
            "tools/call",
            json!({"name":"backend.search","arguments":{"query":identity.name,"limit":20}}),
        ));
        id = id.saturating_add(1);
        calls.push((
            id,
            "tools/call",
            json!({"name":"backend.document","arguments":{"coordinate":identity.coordinate}}),
        ));
        id = id.saturating_add(1);
        calls.push((
            id,
            "tools/call",
            json!({"name":"backend.source","arguments":{"coordinate":identity.coordinate,"detail":"full"}}),
        ));
        id = id.saturating_add(1);
    }
    calls
}
