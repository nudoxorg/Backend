//! Live interface coverage for the typed product surface and Trustfall lane.
//!
//! These cases deliberately cross the same Unix owner through the canonical
//! CLI process, the MCP JSON-RPC process, and the MCP command transport.  The
//! in-process unit tests cover parsing and individual adapters; this file
//! checks that the process seams preserve the typed command/reply contract.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use backend_library::{
    COMMANDS, Command, CommandReply, CommandSpec, GraphQueryRequest, GraphValue, PackageCoordinate,
    PackageReference, PageTerminal, ProductText, ProjectName, ProjectSelector, QueryLimit,
    SemanticGenerationId, SemanticLanguageProfile, SurfaceCommand, SurfaceReply, TreeNodeId,
    TreeOpener, TreeSubject, ViewStateRoot, WireCertificate,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
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
const LOCALD_DEADLINE: Duration = Duration::from_secs(20);
const COMMAND_DEADLINE: Duration = Duration::from_secs(20);
const MCP_DEADLINE: Duration = Duration::from_mins(1);
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceExpectation {
    Result(&'static str),
    TypedInvalidQuery(&'static [&'static str]),
}

#[derive(Clone, Debug)]
struct SurfaceCase {
    command: SurfaceCommand,
    expectation: SurfaceExpectation,
}

#[derive(Debug)]
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn spawn(args: &[OsString], rustc: Option<&Path>) -> Self {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .env_remove("COMPILER_GO_COMPILER")
            .env_remove("NUDOX_GO_ORACLE_BIN")
            .env_remove("NUDOX_GO")
            .env_remove("NUDOX_GO_ORACLE");
        if let Some(rustc) = rustc {
            command.env("NUDOX_RUSTC", rustc);
        }
        let child = command.spawn().expect("spawn locald");
        Self { child: Some(child) }
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

fn run_bounded(mut command: ProcessCommand, label: &str, deadline: Duration) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("COMPILER_GO_COMPILER")
        .env_remove("NUDOX_GO_ORACLE_BIN")
        .env_remove("NUDOX_GO")
        .env_remove("NUDOX_GO_ORACLE");
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {label}: {error}"));
    let mut stdout = child.stdout.take().expect("command stdout");
    let mut stderr = child.stderr.take().expect("command stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read command stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read command stderr");
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
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("wait {label}: {error}");
            }
        }
    };
    Output {
        status,
        stdout: stdout_reader.join().expect("join command stdout"),
        stderr: stderr_reader.join().expect("join command stderr"),
    }
}

fn run_with_input(
    mut command: ProcessCommand,
    label: &str,
    input: &[u8],
    deadline: Duration,
) -> Output {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("COMPILER_GO_COMPILER")
        .env_remove("NUDOX_GO_ORACLE_BIN")
        .env_remove("NUDOX_GO")
        .env_remove("NUDOX_GO_ORACLE");
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {label}: {error}"));
    child
        .stdin
        .take()
        .expect("command stdin")
        .write_all(input)
        .unwrap_or_else(|error| panic!("write {label} stdin: {error}"));
    let mut stdout = child.stdout.take().expect("command stdout");
    let mut stderr = child.stderr.take().expect("command stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read command stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read command stderr");
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
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("wait {label}: {error}");
            }
        }
    };
    Output {
        status,
        stdout: stdout_reader.join().expect("join command stdout"),
        stderr: stderr_reader.join().expect("join command stderr"),
    }
}

fn unique_root(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-surface-interface-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create journey root");
    root
}

fn unique_endpoint(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let endpoint = PathBuf::from(format!(
        "/tmp/backend-surface-{label}-{}-{serial}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&endpoint);
    endpoint
}

fn authority_secret(root: &Path) -> PathBuf {
    let path = root.join("authority.secret");
    std::fs::write(&path, AUTHORITY_SECRET).expect("write authority secret");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat authority secret")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&path, permissions).expect("protect authority secret");
    path
}

fn locald_args(endpoint: &Path, workspace: &Path, authority: &Path) -> Vec<OsString> {
    vec![
        OsString::from("--endpoint"),
        endpoint.as_os_str().to_owned(),
        OsString::from("--workspace"),
        workspace.as_os_str().to_owned(),
        OsString::from("--profile"),
        OsString::from("builtin"),
        OsString::from("--authority-secret-file"),
        authority.as_os_str().to_owned(),
        OsString::from("--max-frame"),
        OsString::from("131072"),
        OsString::from("--timeout-ms"),
        OsString::from("60000"),
    ]
}

fn wait_for_socket(endpoint: &Path, child: &mut ChildGuard) {
    let end = Instant::now() + LOCALD_DEADLINE;
    while Instant::now() < end {
        assert!(child.running(), "locald exited before readiness");
        if let Ok(metadata) = std::fs::symlink_metadata(endpoint)
            && metadata.file_type().is_socket()
            && metadata.permissions().mode() & 0o777 == 0o600
            && UnixStream::connect(endpoint).is_ok()
        {
            return;
        }
        thread::yield_now();
    }
    panic!("timed out waiting for locald socket {}", endpoint.display());
}

fn package() -> PackageReference {
    PackageReference::parse("pkg:cargo/example@1.0.0").expect("admit package reference")
}

fn coordinate() -> PackageCoordinate {
    PackageCoordinate::parse("pkg:cargo/example@1.0.0").expect("admit package coordinate")
}

#[allow(
    clippy::too_many_lines,
    reason = "one literal per registry surface row is the point: a table, not a loop"
)]
fn surface_cases(root: &Path) -> Vec<SurfaceCase> {
    let package = package();
    let name = ProjectName::new("surface-project").expect("admit project name");
    let project = ProjectSelector::Name(name.clone());
    let title = ProductText::new("example").expect("admit tree title");
    let lockfile = ProductText::new(root.join("Cargo.lock").to_string_lossy().into_owned())
        .expect("admit lockfile path");
    let profile = SemanticLanguageProfile::new(LanguageProfile::Rust(RustEdition::Rust2024));
    let generation = SemanticGenerationId::new([7; 32]);
    let forge = ProductText::new("https://github.com/acme/example@tag:v1.0.0")
        .expect("admit forge coordinate");
    vec![
        SurfaceCase {
            command: SurfaceCommand::Advisory {
                package: package.clone(),
                override_evidence: None,
            },
            expectation: SurfaceExpectation::Result("advisory"),
        },
        SurfaceCase {
            command: SurfaceCommand::Read {
                locators: vec![ProductText::new("missing::symbol").expect("admit locator")]
                    .into_boxed_slice(),
            },
            expectation: SurfaceExpectation::Result("read"),
        },
        SurfaceCase {
            command: SurfaceCommand::Diff {
                from: package.clone(),
                to: PackageReference::parse("pkg:cargo/example@2.0.0")
                    .expect("admit newer package"),
            },
            expectation: SurfaceExpectation::TypedInvalidQuery(&[
                "publication authority",
                "package is not indexed",
            ]),
        },
        SurfaceCase {
            command: SurfaceCommand::Explore {
                query: None,
                limit: 1,
            },
            expectation: SurfaceExpectation::Result("explored"),
        },
        SurfaceCase {
            command: SurfaceCommand::Package {
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("package"),
        },
        SurfaceCase {
            command: SurfaceCommand::ForgeAdd {
                coordinate: forge.clone(),
            },
            expectation: SurfaceExpectation::TypedInvalidQuery(&[
                "forge acquisition authority is not configured in this owner",
            ]),
        },
        SurfaceCase {
            command: SurfaceCommand::ForgeReference { coordinate: forge },
            expectation: SurfaceExpectation::TypedInvalidQuery(&[
                "forge acquisition authority is not configured in this owner",
            ]),
        },
        SurfaceCase {
            command: SurfaceCommand::Dependents {
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("dependents"),
        },
        SurfaceCase {
            command: SurfaceCommand::Dependencies {
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("dependencies"),
        },
        SurfaceCase {
            command: SurfaceCommand::Owner {
                owner: ProductText::new("example-owner").expect("admit owner"),
            },
            expectation: SurfaceExpectation::Result("owner"),
        },
        SurfaceCase {
            command: SurfaceCommand::IndexSearch {
                query: ProductText::new("example").expect("admit search query"),
                limit: 1,
            },
            expectation: SurfaceExpectation::Result("index-search"),
        },
        SurfaceCase {
            command: SurfaceCommand::PackageVersions {
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("package-versions"),
        },
        SurfaceCase {
            command: SurfaceCommand::SemanticVersions {
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("semantic-versions"),
        },
        SurfaceCase {
            command: SurfaceCommand::SelectSemanticVersion {
                package: package.clone(),
                coordinate: coordinate(),
                profile,
                generation,
            },
            expectation: SurfaceExpectation::TypedInvalidQuery(&[
                "publication authority",
                "semantic generation",
            ]),
        },
        SurfaceCase {
            command: SurfaceCommand::PackageProfile {
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("package-profile"),
        },
        SurfaceCase {
            command: SurfaceCommand::Subscribe {
                package: package.clone(),
                project: None,
            },
            expectation: SurfaceExpectation::Result("subscribed"),
        },
        SurfaceCase {
            command: SurfaceCommand::Unsubscribe {
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("unsubscribed"),
        },
        SurfaceCase {
            command: SurfaceCommand::Subscriptions,
            expectation: SurfaceExpectation::Result("subscriptions"),
        },
        SurfaceCase {
            command: SurfaceCommand::Releases { mark_seen: true },
            expectation: SurfaceExpectation::Result("releases"),
        },
        SurfaceCase {
            command: SurfaceCommand::Projects,
            expectation: SurfaceExpectation::Result("projects"),
        },
        SurfaceCase {
            command: SurfaceCommand::ProjectCreate {
                name: name.clone(),
                lockfile: Some(lockfile),
            },
            expectation: SurfaceExpectation::Result("project-created"),
        },
        SurfaceCase {
            command: SurfaceCommand::ProjectAdd {
                project: project.clone(),
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("project-added"),
        },
        SurfaceCase {
            command: SurfaceCommand::ProjectRemove {
                project: project.clone(),
                package: package.clone(),
            },
            expectation: SurfaceExpectation::Result("project-removed"),
        },
        SurfaceCase {
            command: SurfaceCommand::ProjectSync {
                project: project.clone(),
            },
            expectation: SurfaceExpectation::Result("project-synced"),
        },
        SurfaceCase {
            command: SurfaceCommand::ProjectDelete { project },
            expectation: SurfaceExpectation::Result("project-deleted"),
        },
        SurfaceCase {
            command: SurfaceCommand::Tree,
            expectation: SurfaceExpectation::Result("tree"),
        },
        SurfaceCase {
            command: SurfaceCommand::TreeOpen {
                subject: TreeSubject::Package(package),
                parent: None,
                title: Some(title),
                opener: TreeOpener::Cli,
            },
            expectation: SurfaceExpectation::Result("tree-opened"),
        },
        SurfaceCase {
            command: SurfaceCommand::TreeClose {
                node: TreeNodeId::new(std::num::NonZeroU64::MIN),
                branch: true,
            },
            expectation: SurfaceExpectation::Result("tree-closed"),
        },
    ]
}

fn registry_surface_rows() -> Vec<CommandSpec> {
    COMMANDS
        .iter()
        .copied()
        .filter(|spec| spec.is_surface())
        .collect()
}

fn assert_surface_registry_matches_cases(cases: &[SurfaceCase]) {
    let rows = registry_surface_rows();
    assert_eq!(
        COMMANDS.len(),
        40,
        "the closed command registry changed size"
    );
    assert_eq!(
        rows.len(),
        28,
        "the registry surface projection changed size"
    );
    assert_eq!(
        cases.len(),
        28,
        "the live SurfaceCommand matrix is incomplete"
    );
    let registry_ids = rows.iter().map(|row| row.id).collect::<BTreeSet<_>>();
    let case_ids = cases
        .iter()
        .map(|case| case.command.id())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        registry_ids, case_ids,
        "live matrix must cover every registry row"
    );
    for case in cases {
        let encoded = serde_json::to_value(&case.command).expect("encode surface command");
        assert_eq!(
            encoded["operation"],
            rows.iter()
                .find(|row| row.id == case.command.id())
                .expect("surface registry row")
                .name,
            "canonical operation name drifted from COMMANDS"
        );
    }
}

/// Checks one CLI answer against the *presentation* contract.
///
/// `--format json` deliberately no longer emits the wire reply: a consumer that
/// bound to it would be binding to certificate claims, cursor internals, and
/// request correlation, none of which are product facts. What it emits is the
/// tagged presentation DTO, so that is what this asserts — the answer's own
/// discriminator for a success, and the shared fault's slug and operand for a
/// refusal. The typed `SurfaceReply` identity is still proven per case on the
/// surface that still carries it verbatim, by `assert_mcp_surface_reply`.
fn assert_cli_surface_reply(value: &Value, case: &SurfaceCase, label: &str, status: bool) {
    match case.expectation {
        SurfaceExpectation::Result(_) => {
            assert!(status, "{label} failed: {value}");
            assert_eq!(value["answer"], "product", "{label} answer kind");
            assert!(
                value["heading"].as_str().is_some_and(|heading| !heading.is_empty()),
                "{label} published no heading: {value}"
            );
            assert!(
                value.get("certificate").is_none(),
                "{label} leaked wire proof into the product JSON"
            );
            // A registry feed that does not publish a fact answers *with* that
            // fact: the reply is a success and the view carries a typed
            // lane-unavailable fault naming what was not recorded. That is the
            // whole point of the model, so it is asserted rather than excluded.
            if let Some(fault) = value.get("fault").filter(|fault| !fault.is_null()) {
                assert!(
                    fault["slug"].as_str().is_some_and(|slug| !slug.is_empty()),
                    "{label} carried an untyped fault: {value}"
                );
                assert!(
                    fault["operand"].as_str().is_some_and(|operand| !operand.is_empty()),
                    "{label} carried a fault with no operand: {value}"
                );
                assert!(
                    fault["detail"].as_str().is_some_and(|detail| !detail.is_empty()),
                    "{label} carried a fault with no sentence: {value}"
                );
            }
        }
        SurfaceExpectation::TypedInvalidQuery(allowed_details) => {
            assert!(!status, "{label} unexpectedly succeeded: {value}");
            assert_eq!(value["answer"], "fault", "{label} answer kind");
            assert_eq!(value["slug"], "invalid-query", "{label} failure class");
            let detail = value["detail"]
                .as_str()
                .unwrap_or_else(|| panic!("{label} omitted its cause sentence: {value}"));
            assert!(
                allowed_details.iter().any(|needle| detail.contains(needle)),
                "{label} failure changed its typed cause: {detail}"
            );
        }
    }
}

fn run_cli_surface_matrix(root: &Path, endpoint: &Path, cases: &[SurfaceCase]) {
    for (index, case) in cases.iter().enumerate() {
        let encoded = serde_json::to_string(&case.command).expect("encode CLI surface command");
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
        command
            .args(["--endpoint"])
            .arg(endpoint)
            .args(["--workspace"])
            .arg(root.join("workspace"))
            .args(["--json", "surface"])
            .arg(encoded);
        let label = format!("CLI surface variant {index}");
        let output = run_bounded(command, &label, COMMAND_DEADLINE);
        let value = serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "decode {label}: {error}; stdout={}; stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert_cli_surface_reply(&value, case, &label, output.status.success());
    }
}

fn mcp_surface_input(cases: &[SurfaceCase]) -> Vec<u8> {
    let mut lines = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "surface-journey", "version": "1" }
            }
        })
        .to_string(),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        })
        .to_string(),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })
        .to_string(),
    ];
    lines.extend(cases.iter().enumerate().map(|(index, case)| {
        json!({
            "jsonrpc": "2.0",
            "id": 100 + index,
            "method": "tools/call",
            "params": {
                "name": "backend.surface",
                "arguments": { "command": serde_json::to_value(&case.command).expect("encode MCP command") }
            }
        })
        .to_string()
    }));
    let mut input = lines.join("\n").into_bytes();
    input.push(b'\n');
    input
}

fn assert_mcp_surface_reply(value: &Value, case: &SurfaceCase, label: &str) {
    match case.expectation {
        SurfaceExpectation::Result(expected) => {
            assert_eq!(value["result"]["isError"], false, "{label} MCP error");
            let surface = value["result"]["structuredContent"]["surface"].clone();
            let typed = serde_json::from_value::<SurfaceReply>(surface.clone())
                .unwrap_or_else(|error| panic!("decode typed {label} reply: {error}; {value}"));
            assert_eq!(typed.id(), case.command.id(), "{label} typed identity");
            assert_eq!(surface["result"], expected, "{label} result tag");
        }
        SurfaceExpectation::TypedInvalidQuery(allowed_details) => {
            assert_eq!(value["error"]["code"], -32603, "{label} MCP failure code");
            let detail = value["error"]["data"]["detail"]
                .as_str()
                .unwrap_or_else(|| panic!("{label} omitted MCP failure detail: {value}"));
            assert!(
                allowed_details.iter().any(|needle| detail.contains(needle)),
                "{label} MCP failure changed its typed cause: {detail}"
            );
        }
    }
}

fn run_mcp_surface_matrix(
    root: &Path,
    endpoint: &Path,
    project: &Path,
    authority: &Path,
    cases: &[SurfaceCase],
) {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
    // The MCP process signs cursors with the owner's authority credential
    // (d10dd4d83), so it must read the same file the locald was given.
    command
        .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority)
        .args(["--endpoint"])
        .arg(endpoint)
        .args(["--workspace"])
        .arg(root.join("workspace"))
        .args(["--project"])
        .arg(project);
    let output = run_with_input(
        command,
        "MCP surface matrix",
        &mcp_surface_input(cases),
        MCP_DEADLINE,
    );
    assert!(
        output.status.success(),
        "MCP surface process failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let responses = String::from_utf8(output.stdout)
        .expect("MCP stdout UTF-8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("decode MCP response"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), cases.len() + 2, "MCP response count");
    assert_eq!(responses[0]["id"], 1, "MCP initialize request identity");
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");
    let tools = responses[1]["result"]["tools"]
        .as_array()
        .expect("MCP tools list");
    let surface = tools
        .iter()
        .find(|tool| tool["name"] == "backend.surface")
        .expect("backend.surface tool");
    let advertised =
        surface["inputSchema"]["properties"]["command"]["properties"]["operation"]["enum"]
            .as_array()
            .expect("backend.surface operation enum");
    let expected = registry_surface_rows()
        .iter()
        .map(|row| Value::String(row.name.to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(
        advertised, &expected,
        "MCP surface schema drifted from COMMANDS"
    );
    for (index, case) in cases.iter().enumerate() {
        let response = &responses[index + 2];
        assert_eq!(response["id"], 100 + index, "MCP surface request identity");
        assert_mcp_surface_reply(response, case, &format!("MCP surface variant {index}"));
    }
}

fn write_rust_project(root: &Path) -> PathBuf {
    let project = root.join("rust-project");
    std::fs::create_dir_all(project.join("src")).expect("create Rust project");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"example\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )
    .expect("write Rust manifest");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\npub fn gamma() -> u32 { 3 }\n",
    )
    .expect("write Rust source");
    project
}

fn explicit_rustc() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|directory| {
            let candidate = directory.join("rustc");
            candidate
                .is_file()
                .then(|| candidate.canonicalize().ok())
                .flatten()
        })
    })
}

fn wait_for_index(session: &mut backend_mcp::Session, project: &Path) {
    session
        .index(&project.to_string_lossy())
        .expect("submit live Rust index");
    let end = Instant::now() + LOCALD_DEADLINE;
    loop {
        let report = session.health().expect("read live owner health");
        if report.row_count() > 1 {
            return;
        }
        assert!(
            Instant::now() < end,
            "Rust index did not publish multiple rows"
        );
        thread::yield_now();
    }
}

fn revision_parts(reply: &backend_mcp::ReplyDto) -> (ViewStateRoot, WireCertificate) {
    let CommandReply::Revision(receipt) = &reply.reply else {
        panic!("revision reply changed shape: {:?}", reply.reply);
    };
    let certificate = reply
        .certificate()
        .cloned()
        .expect("revision certificate")
        .with_claim_once(backend_library::WireClaim::RootCommitment {
            schema: backend_library::WireSchema::ViewRelation,
            id: backend_library::encode_id(receipt.root().as_bytes()),
        });
    (receipt.root(), certificate)
}

#[test]
fn registry_is_authoritative_for_all_surface_variants() {
    let root = unique_root("registry");
    let cases = surface_cases(&root);
    assert_surface_registry_matches_cases(&cases);
}

#[test]
fn every_surface_variant_crosses_the_real_cli_and_mcp_processes() {
    let root = unique_root("processes");
    let lockfile = root.join("Cargo.lock");
    std::fs::write(
        &lockfile,
        "[[package]]\nname = \"example\"\nversion = \"1.0.0\"\n",
    )
    .expect("write surface lockfile");
    let cases = surface_cases(&root);
    assert_surface_registry_matches_cases(&cases);

    let cli_root = root.join("cli");
    let cli_project = write_rust_project(&cli_root);
    let cli_endpoint = unique_endpoint("cli");
    let cli_workspace = cli_root.join("workspace");
    std::fs::create_dir_all(&cli_workspace).expect("create CLI workspace");
    let cli_authority = authority_secret(&cli_root);
    let cli_args = locald_args(&cli_endpoint, &cli_workspace, &cli_authority);
    let mut cli_locald = ChildGuard::spawn(&cli_args, explicit_rustc().as_deref());
    wait_for_socket(&cli_endpoint, &mut cli_locald);
    let mut cli_indexing =
        backend_mcp::Session::connect(&cli_endpoint).expect("connect CLI matrix indexing session");
    wait_for_index(&mut cli_indexing, &cli_project);
    run_cli_surface_matrix(&cli_root, &cli_endpoint, &cases);
    drop(cli_locald);

    let mcp_root = root.join("mcp");
    let mcp_project = write_rust_project(&mcp_root);
    let mcp_endpoint = unique_endpoint("mcp");
    let mcp_workspace = mcp_root.join("workspace");
    std::fs::create_dir_all(&mcp_workspace).expect("create MCP workspace");
    let mcp_authority = authority_secret(&mcp_root);
    let mcp_args = locald_args(&mcp_endpoint, &mcp_workspace, &mcp_authority);
    let mut mcp_locald = ChildGuard::spawn(&mcp_args, explicit_rustc().as_deref());
    wait_for_socket(&mcp_endpoint, &mut mcp_locald);
    run_mcp_surface_matrix(
        &mcp_root,
        &mcp_endpoint,
        &mcp_project,
        &mcp_authority,
        &cases,
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one live daemon paged, resumed, and cancelled in one sequence"
)]
fn live_mcp_trustfall_pages_resume_cancel_and_preserve_request_identity() {
    let root = unique_root("trustfall");
    let project = write_rust_project(&root);
    let endpoint = unique_endpoint("trustfall");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create Trustfall workspace");
    let authority = authority_secret(&root);
    let args = locald_args(&endpoint, &workspace, &authority);
    let mut locald = ChildGuard::spawn(&args, explicit_rustc().as_deref());
    wait_for_socket(&endpoint, &mut locald);

    let mut indexing = backend_mcp::Session::connect(&endpoint).expect("connect indexing session");
    wait_for_index(&mut indexing, &project);

    let mut transport = backend_mcp::UnixCommandTransport::connect(&endpoint)
        .expect("connect raw MCP command transport");
    let revision_reply = backend_mcp::CommandTransport::request(
        &mut transport,
        backend_mcp::CommandDto::new(700, Command::Revision),
    )
    .expect("read live revision");
    assert_eq!(revision_reply.request_id, 700, "revision request identity");
    let (root_revision, revision_certificate) = revision_parts(&revision_reply);
    let query = "{ Declaration { coordinate @output } }";
    let limit = QueryLimit::new(1).expect("admit one-row query limit");
    let first_request = GraphQueryRequest::new(
        query,
        BTreeMap::<String, GraphValue>::new(),
        root_revision,
        limit,
    )
    .expect("admit first Trustfall query");
    let first_command =
        backend_mcp::CommandDto::new(702, Command::GraphQuery(first_request.clone()))
            .with_certificate(revision_certificate.clone());
    backend_mcp::encode_request(&first_command).expect("encode first live Trustfall page");
    let first_reply = backend_mcp::CommandTransport::request(&mut transport, first_command)
        .expect("execute first live Trustfall page");
    assert_eq!(first_reply.request_id, 702, "first page request identity");
    let first_page = match &first_reply.reply {
        CommandReply::GraphQueryPage(page) => page,
        other => panic!("first Trustfall reply changed shape: {other:?}"),
    };
    assert_eq!(first_page.revision, root_revision, "first page revision");
    assert_eq!(first_page.rows.len(), 1, "first page limit");
    let continuation = match first_page.terminal {
        PageTerminal::More(continuation) => continuation,
        terminal => panic!("Trustfall query did not publish a continuation: {terminal:?}"),
    };

    let second_request = first_request.with_continuation(continuation);
    let second_certificate = first_reply
        .certificate()
        .cloned()
        .expect("first page carries continuation certificate");
    let second_reply = backend_mcp::CommandTransport::request(
        &mut transport,
        backend_mcp::CommandDto::new(703, Command::GraphQuery(second_request))
            .with_certificate(second_certificate),
    )
    .expect("resume live Trustfall query");
    assert_eq!(second_reply.request_id, 703, "resume request identity");
    let second_page = match second_reply.reply {
        CommandReply::GraphQueryPage(page) => page,
        other => panic!("resumed Trustfall reply changed shape: {other:?}"),
    };
    assert_eq!(second_page.revision, root_revision, "resumed page revision");
    assert!(
        !second_page.rows.is_empty(),
        "resumed query emitted no rows"
    );
    assert!(
        matches!(
            second_page.terminal,
            PageTerminal::Complete | PageTerminal::More(_)
        ),
        "resumed query returned an invalid terminal"
    );

    let cancelled_request = GraphQueryRequest::new(
        query,
        BTreeMap::<String, GraphValue>::new(),
        root_revision,
        limit,
    )
    .expect("admit cancelled Trustfall query")
    .cancelled();
    let cancelled_reply = backend_mcp::CommandTransport::request(
        &mut transport,
        backend_mcp::CommandDto::new(704, Command::GraphQuery(cancelled_request))
            .with_certificate(revision_certificate),
    )
    .expect("execute cancelled Trustfall query");
    assert_eq!(cancelled_reply.request_id, 704, "cancel request identity");
    let cancelled_page = match cancelled_reply.reply {
        CommandReply::GraphQueryPage(page) => page,
        other => panic!("cancelled Trustfall reply changed shape: {other:?}"),
    };
    assert_eq!(cancelled_page.terminal, PageTerminal::Cancelled);
    assert!(
        cancelled_page.rows.is_empty(),
        "cancelled query emitted rows"
    );
}
