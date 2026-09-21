//! Production process journey for cold launch, durable roots, and recovery.
//!
//! Every boundary in this file is a real executable or a real persistent
//! workspace.  The assertions intentionally compare immutable roots and
//! stable row identities rather than trusting process exit status or a
//! successful socket connect.  This keeps a crash that leaves a partial view
//! visible as a test failure.

#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use backend_client::Session;
use backend_desktop::{SubscriptionRequest, UnixSubscriptionTransport};
use backend_engine::capability::CapabilityArtifactId;
use backend_engine::registry::{RegistryEcosystem, RegistryEndpoint, storage_root};
use backend_library::{
    CursorRead, GraphValue, PackageReference, ProjectName, SurfaceCommand, SurfaceReply,
    ViewRevision, encode_id,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as ProcessCommand, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const READY_DEADLINE: Duration = Duration::from_secs(90);
const PROCESS_DEADLINE: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(10);
const AUTHORITY_BYTES: [u8; 32] = [0x5a; 32];
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

const PRODUCT_ENV: &[&str] = &[
    "BACKEND_PROJECT",
    "BACKEND_LOCALD_WORKSPACE",
    "BACKEND_LOCALD_ENDPOINT",
    "BACKEND_LOCALD_BIN",
    "BACKEND_LOCALD_AUTHORITY_SECRET_FILE",
    "BACKEND_PROFILE",
    "NUDOX_DATA_ROOT",
    "NUDOX_RUSTC",
    "NUDOX_RUST_SYSROOT",
    "NUDOX_CLANG",
    "NUDOX_PYTHON",
    "NUDOX_TSC",
    "NUDOX_GO",
    "NUDOX_JAVAC",
    "NUDOX_DOTNET",
    "NUDOX_TYPESCRIPT_NODE",
    "NUDOX_TYPESCRIPT_MODULE_ROOT",
    "NUDOX_TYPESCRIPT_REPORT_PROGRAM",
    "NUDOX_PYREFLY",
    "NUDOX_GO_ORACLE",
    "NUDOX_JDK",
    "NUDOX_ROSLYN_HELPER",
    "NUDOX_CARGO_ROOT",
    "NUDOX_NPM_ROOT",
    "NUDOX_PYPI_ROOT",
    "NUDOX_GO_ROOT",
    "NUDOX_MAVEN_ROOT",
    "NUDOX_NUGET_ROOT",
    "NUDOX_GENERIC_ROOT",
    "NUDOX_REGISTRY_ENDPOINT",
    "NUDOX_REGISTRY_ECOSYSTEM",
    "NUDOX_REGISTRY_AUTH",
    "NUDOX_REGISTRY_AUTH_FILE",
    "NUDOX_REGISTRY_OFFLINE",
];

#[derive(Debug, serde::Serialize)]
struct Observation {
    phase: String,
    elapsed_ms: u128,
    rss_kib: Option<u64>,
}

#[derive(Debug)]
struct ProcessGuard {
    child: Option<Child>,
}

impl ProcessGuard {
    fn locald(args: &[OsString]) -> Self {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        scrub(&mut command);
        Self {
            child: Some(command.spawn().expect("spawn locald")),
        }
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll child"))
            .is_none()
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    fn kill_now(&mut self) -> Option<ExitStatus> {
        let child = self.child.as_mut()?;
        if child.try_wait().expect("poll before SIGKILL").is_none() {
            child.kill().expect("SIGKILL child");
        }
        Some(child.wait().expect("wait SIGKILL child"))
    }

    fn wait_exit(&mut self, deadline: Duration) -> ExitStatus {
        let end = Instant::now() + deadline;
        loop {
            if let Some(child) = self.child.as_mut() {
                if let Some(status) = child.try_wait().expect("poll exit") {
                    return status;
                }
            } else {
                panic!("child was already reaped");
            }
            assert!(
                Instant::now() < end,
                "child did not exit within {deadline:?}"
            );
            thread::sleep(POLL);
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        if child
            .try_wait()
            .expect("poll child during cleanup")
            .is_none()
        {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

fn scrub(command: &mut ProcessCommand) {
    for variable in PRODUCT_ENV {
        command.env_remove(variable);
    }
    // Keep the GUI and child-process paths independent of newly introduced
    // product selectors too. Explicit endpoint/workspace arguments are the
    // only configuration this journey is allowed to require.
    for (variable, _) in std::env::vars_os() {
        let name = variable.to_string_lossy();
        if name.starts_with("BACKEND_") || name.starts_with("NUDOX_") {
            command.env_remove(variable);
        }
    }
}

fn unique_root(label: &str) -> PathBuf {
    let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "backend-cold-restart-{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create fixture root");
    root.canonicalize().expect("canonical fixture root")
}

fn authority_secret(workspace: &Path) -> PathBuf {
    let path = workspace.join("authority.secret");
    std::fs::write(&path, AUTHORITY_BYTES).expect("write authority secret");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat authority secret")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&path, permissions).expect("protect authority secret");
    path
}

/// Writes the small persisted project used by the restart journey. Keeping
/// this corpus to the two historical identity cases makes a graph admission
/// failure point at the restart contract instead of at an unrelated language
/// specimen; the boundary project below still supplies a large real ingest.
fn restart_project(root: &Path) -> PathBuf {
    let project = root.join("restart-project");
    std::fs::create_dir_all(project.join("src")).expect("create restart source");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"restart-project\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )
    .expect("write restart manifest");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn RustBeaconEntry() -> u64 { 1 }\n",
    )
    .expect("write restart Rust source");
    std::fs::write(
        project.join("src/CSharpBeacon.cs"),
        "public sealed class CSharpBeacon { }\n",
    )
    .expect("write restart C# source");
    project.canonicalize().expect("canonical restart project")
}

fn large_project(root: &Path) -> PathBuf {
    let project = root.join("restart-boundary");
    std::fs::create_dir_all(project.join("src")).expect("create boundary source");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"restart-boundary\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )
    .expect("write boundary manifest");
    let mut lib = String::new();
    for index in 0..128_u32 {
        lib.push_str(&format!("mod part_{index};\n"));
        let body = format!(
            "pub fn RestartBoundaryBeacon{index}() -> u64 {{ {index} }}\n{}",
            "// durable-boundary-payload\n".repeat(32)
        );
        std::fs::write(project.join("src").join(format!("part_{index}.rs")), body)
            .expect("write boundary module");
    }
    lib.push_str("pub fn RestartBoundaryBeacon() -> u64 { 128 }\n");
    std::fs::write(project.join("src/lib.rs"), lib).expect("write boundary lib");
    project.canonicalize().expect("canonical boundary project")
}

/// Adds the two source shapes that previously crossed the product identity
/// boundary: Rust methods selected by both the function and method tags, and
/// a one-word block C# namespace selected by both namespace patterns.
fn write_identity_regressions(project: &Path) {
    let rust_path = project.join("src/lib.rs");
    let mut rust_source = std::fs::read_to_string(&rust_path).expect("read Rust fixture");
    rust_source.push_str(
        r#"
pub trait RestartIdentityTrait {
    fn trait_method_marker(&self) -> u64;
}

pub struct RestartIdentityType;

impl RestartIdentityTrait for RestartIdentityType {
    fn trait_method_marker(&self) -> u64 { 1 }
}

impl RestartIdentityType {
    pub fn impl_method_marker(&self) -> u64 { 2 }
}
"#,
    );
    std::fs::write(&rust_path, rust_source).expect("write Rust identity regression fixture");

    let csharp_path = project.join("src/CSharpBeacon.cs");
    let csharp_source = std::fs::read_to_string(&csharp_path).expect("read C# fixture");
    std::fs::write(
        &csharp_path,
        format!("namespace RestartIdentityNamespace\n{{\n{csharp_source}\n}}\n"),
    )
    .expect("write C# identity regression fixture");
}

/// Adds the generated trees that occur in real JavaScript workspaces.  The
/// oversized file is deliberately placed under an ignored build directory:
/// discovery must prune it before the bounded source reader sees it, so one
/// stale bundle cannot abort admission of the rest of the project.
fn write_ignored_javascript_outputs(project: &Path) {
    std::fs::write(
        project.join(".gitignore"),
        "src/ignored-by-gitignore.js\nnode_modules/\ndist/\n.next/\nbuild/\ncoverage/\n",
    )
    .expect("write JavaScript ignore policy");
    for (directory, name) in [
        ("node_modules/transitive", "IgnoredNodeModulesBeacon"),
        ("dist", "IgnoredDistBeacon"),
        (".next/server", "IgnoredNextBeacon"),
        ("build", "IgnoredBuildBeacon"),
        ("coverage", "IgnoredCoverageBeacon"),
    ] {
        let directory = project.join(directory);
        std::fs::create_dir_all(&directory).expect("create ignored JavaScript output");
        std::fs::write(
            directory.join("bundle.js"),
            format!("export function {name}() {{ return 1; }}\n"),
        )
        .expect("write ignored JavaScript output");
    }
    std::fs::write(
        project.join("src/ignored-by-gitignore.js"),
        "export function IgnoredByGitignoreBeacon() { return 2; }\n",
    )
    .expect("write explicitly ignored JavaScript source");
    std::fs::write(
        project.join("dist/oversized-build.js"),
        vec![b'x'; 512 * 1024 + 1],
    )
    .expect("write oversized ignored JavaScript build output");
}

fn locald_args(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    offline: bool,
    timeout_ms: u64,
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
        OsString::from(timeout_ms.to_string()),
        OsString::from("--idle-timeout-ms"),
        OsString::from("0"),
    ];
    if offline {
        args.push(OsString::from("--registry-offline"));
    }
    args
}

fn wait_for_socket(endpoint: &Path, child: &mut ProcessGuard) {
    let end = Instant::now() + READY_DEADLINE;
    while Instant::now() < end {
        assert!(child.running(), "locald exited before readiness");
        if let Ok(metadata) = std::fs::symlink_metadata(endpoint)
            && metadata.file_type().is_socket()
            && UnixStream::connect(endpoint).is_ok()
        {
            return;
        }
        thread::sleep(POLL);
    }
    panic!("timed out waiting for locald socket {}", endpoint.display());
}

fn wait_for_socket_dead(endpoint: &Path) {
    let end = Instant::now() + PROCESS_DEADLINE;
    while Instant::now() < end {
        if UnixStream::connect(endpoint).is_err() {
            return;
        }
        thread::sleep(POLL);
    }
    panic!(
        "endpoint remained live after daemon kill: {}",
        endpoint.display()
    );
}

fn launch(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    offline: bool,
    timeout_ms: u64,
) -> ProcessGuard {
    let args = locald_args(endpoint, workspace, authority, offline, timeout_ms);
    let mut child = ProcessGuard::locald(&args);
    wait_for_socket(endpoint, &mut child);
    child
}

fn rss_kib(child: &ProcessGuard) -> Option<u64> {
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

fn note(
    observations: &mut Vec<Observation>,
    phase: &str,
    started: Instant,
    child: Option<&ProcessGuard>,
) {
    let observation = Observation {
        phase: phase.to_owned(),
        elapsed_ms: started.elapsed().as_millis(),
        rss_kib: child.and_then(rss_kib),
    };
    eprintln!(
        "cold-restart phase={} elapsed_ms={} rss_kib={:?}",
        observation.phase, observation.elapsed_ms, observation.rss_kib
    );
    observations.push(observation);
}

fn bounded(mut command: ProcessCommand, label: &str, input: Option<&[u8]>) -> Output {
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
            .write_all(input)
            .unwrap_or_else(|error| panic!("write {label}: {error}"));
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
            Ok(None) if Instant::now() < end => thread::sleep(POLL),
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

fn cli_json(endpoint: &Path, workspace: &Path, project: &Path, words: &[&str]) -> Value {
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
    scrub(&mut command);
    let output = bounded(command, &format!("CLI {words:?}"), None);
    assert!(
        output.status.success(),
        "CLI {words:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI {words:?} returned invalid JSON: {error}; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn search_identity(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
) -> surface_matrix::RowIdentity {
    let value = search_json(endpoint, workspace, project, "RustBeaconEntry");
    surface_matrix::case_identity(
        &value,
        surface_matrix::LanguageCase {
            language: "rust",
            path: "src/lib.rs",
            name: "RustBeaconEntry",
            source: "",
        },
    )
}

fn search_json(endpoint: &Path, workspace: &Path, project: &Path, text: &str) -> Value {
    cli_json(
        endpoint,
        workspace,
        project,
        &["search", text, "--limit", "40"],
    )
}

fn names_json(endpoint: &Path, workspace: &Path, project: &Path, text: &str) -> Value {
    cli_json(
        endpoint,
        workspace,
        project,
        &["name", text, "--limit", "40"],
    )
}

fn records_for<'a>(value: &'a Value, path: &str, name: &str) -> Vec<&'a Value> {
    value["records"]
        .as_array()
        .unwrap_or_else(|| panic!("search omitted records: {value}"))
        .iter()
        .filter(|record| record["identity"]["path"] == path && record["identity"]["name"] == name)
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RegressionSnapshot(Vec<(String, String, String, String, String)>);

fn regression_snapshot(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
) -> Option<RegressionSnapshot> {
    let cases = [
        // The trait declaration and its impl are two valid declarations with
        // one name.  The former regression tagged each as both a function and
        // a method, so the public rows must be exactly these two methods.
        ("src/lib.rs", "trait_method_marker", "method", 2),
        ("src/lib.rs", "impl_method_marker", "method", 1),
        (
            "src/CSharpBeacon.cs",
            "RestartIdentityNamespace",
            "module",
            1,
        ),
    ];
    let mut rows = Vec::new();
    for (path, name, expected_kind, expected_count) in cases {
        let search_value = search_json(endpoint, workspace, project, name);
        let search_records = records_for(&search_value, path, name);
        let names_value = names_json(endpoint, workspace, project, name);
        let names_records = records_for(&names_value, path, name);
        if search_records.len() != expected_count || names_records.len() != expected_count {
            return None;
        }
        let mut search_rows = Vec::with_capacity(search_records.len());
        for record in search_records {
            if record["kind"].as_str() != Some(expected_kind) {
                return None;
            }
            let identity = &record["identity"];
            search_rows.push((
                identity["coordinate"].as_str()?.to_owned(),
                identity["path"].as_str()?.to_owned(),
                identity["name"].as_str()?.to_owned(),
                identity["key"].as_str()?.to_owned(),
                record["kind"].as_str()?.to_owned(),
            ));
        }
        let mut names_rows = Vec::with_capacity(names_records.len());
        for record in names_records {
            if record["kind"].as_str() != Some(expected_kind) {
                return None;
            }
            let identity = &record["identity"];
            names_rows.push((
                identity["coordinate"].as_str()?.to_owned(),
                identity["path"].as_str()?.to_owned(),
                identity["name"].as_str()?.to_owned(),
                identity["key"].as_str()?.to_owned(),
                record["kind"].as_str()?.to_owned(),
            ));
        }
        search_rows.sort();
        names_rows.sort();
        if search_rows != names_rows {
            return None;
        }
        rows.extend(search_rows);
    }
    rows.sort();
    Some(RegressionSnapshot(rows))
}

fn wait_regression_snapshot(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
) -> RegressionSnapshot {
    let end = Instant::now() + READY_DEADLINE;
    loop {
        if let Some(snapshot) = regression_snapshot(endpoint, workspace, project) {
            return snapshot;
        }
        assert!(
            Instant::now() < end,
            "identity regression rows were missing, duplicated, or misclassified"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SurfaceEvidence {
    status: surface_matrix::StatusIdentity,
    search: surface_matrix::RowIdentity,
    names: surface_matrix::RowIdentity,
    outline: BTreeSet<String>,
    query_revision: ViewRevision,
    query_coordinates: BTreeSet<String>,
    regressions: RegressionSnapshot,
}

fn graph_query_evidence(session: &mut Session) -> (ViewRevision, BTreeSet<String>) {
    let page = session
        .graph_query(
            "{ Declaration { coordinate @output } }".to_owned(),
            BTreeMap::<String, GraphValue>::new(),
            40,
            None,
            false,
        )
        .expect("execute graph query after restart");
    assert!(
        !page.rows.is_empty(),
        "graph query returned no declarations"
    );
    let coordinates = page
        .rows
        .iter()
        .filter_map(|row| {
            row.fields()
                .iter()
                .find_map(|(name, value)| match (name.as_str(), value) {
                    ("coordinate", GraphValue::String(coordinate)) => Some(coordinate.clone()),
                    _ => None,
                })
        })
        .collect();
    (page.revision, coordinates)
}

fn surface_evidence(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    expected_root: backend_library::ViewStateRoot,
) -> SurfaceEvidence {
    let status_value = cli_json(endpoint, workspace, project, &["health"]);
    assert_eq!(status_value["answer"], "status");
    let status = surface_matrix::status_identity(&status_value);
    assert_eq!(
        status.revision,
        encode_id(expected_root.as_bytes()),
        "status observed a root outside the admitted restart set"
    );

    let search = search_identity(endpoint, workspace, project);
    let names_value = cli_json(
        endpoint,
        workspace,
        project,
        // The public registry calls this projection `resolve` and keeps
        // `name` as its short alias.  There is no `names` verb; the reply is
        // still the names lane that the replacement shell consumes.
        &["name", "RustBeaconEntry", "--limit", "40"],
    );
    assert_eq!(names_value["answer"], "records");
    let names = surface_matrix::case_identity(
        &names_value,
        surface_matrix::LanguageCase {
            language: "rust",
            path: "src/lib.rs",
            name: "RustBeaconEntry",
            source: "",
        },
    );
    let outline_value = cli_json(
        endpoint,
        workspace,
        project,
        &["outline", project.to_string_lossy().as_ref()],
    );
    assert_eq!(outline_value["answer"], "outline");
    let outline = surface_matrix::outline_coordinates(&outline_value);
    assert!(
        outline.contains(&search.coordinate),
        "outline omitted the stable Rust row {}",
        search.coordinate
    );

    let mut query_session = Session::connect(endpoint).expect("connect query evidence session");
    let (query_revision, query_coordinates) = graph_query_evidence(&mut query_session);
    assert_eq!(query_revision, ViewRevision::from(expected_root));

    let regressions = regression_snapshot(endpoint, workspace, project)
        .expect("identity regression rows changed kind or multiplicity");
    for (coordinate, _, _, _, _) in &regressions.0 {
        assert!(
            outline.contains(coordinate),
            "outline omitted identity regression {coordinate}"
        );
        assert!(
            query_coordinates.contains(coordinate),
            "graph query omitted identity regression {coordinate}"
        );
    }
    SurfaceEvidence {
        status,
        search,
        names,
        outline,
        query_revision,
        query_coordinates,
        regressions,
    }
}

fn wait_ingest(session: &mut Session, minimum_rows: u64) -> backend_library::HealthReport {
    let end = Instant::now() + READY_DEADLINE;
    loop {
        let report = session.health().expect("poll ingest health");
        if report.row_count() >= minimum_rows {
            return report;
        }
        assert!(
            Instant::now() < end,
            "ingest did not publish {minimum_rows} rows"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn projects(session: &mut Session) -> Vec<(u64, String)> {
    let SurfaceReply::Projects(rows) = session
        .surface(SurfaceCommand::Projects)
        .expect("read project shelf")
    else {
        panic!("project shelf reply changed shape");
    };
    rows.iter()
        .map(|project| (project.id.get(), project.name.as_str().to_owned()))
        .collect()
}

fn run_gui(root: &Path, journey: &str) -> Value {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-gui"));
    command
        .arg(journey)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("COLUMNS", "100");
    scrub(&mut command);
    let output = bounded(command, &format!("GUI {journey}"), None);
    assert!(
        output.status.success(),
        "GUI {journey} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "GUI {journey} returned invalid JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl McpProcess {
    fn spawn(endpoint: &Path, workspace: &Path, project: &Path, locald: &Path) -> Self {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
        command
            .arg("--endpoint")
            .arg(endpoint)
            .arg("--workspace")
            .arg(workspace)
            .arg("--project")
            .arg(project)
            .env("BACKEND_LOCALD_BIN", locald)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        scrub(&mut command);
        command.env("BACKEND_LOCALD_BIN", locald);
        let mut child = command.spawn().expect("spawn MCP");
        let stdin = child.stdin.take().expect("MCP stdin");
        let stdout = child.stdout.take().expect("MCP stdout");
        let (sender, replies) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            replies,
            reader: Some(reader),
        }
    }

    fn send(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().expect("MCP stdin is open");
        writeln!(stdin, "{value}").expect("write MCP request");
        stdin.flush().expect("flush MCP request");
    }

    fn reply(&self, id: u64) -> Value {
        let end = Instant::now() + PROCESS_DEADLINE;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "MCP reply {id} exceeded deadline");
            match self
                .replies
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                Ok(line) => {
                    let value: Value = serde_json::from_str(&line).unwrap_or_else(|error| {
                        panic!("MCP emitted invalid JSON: {error}: {line}")
                    });
                    if value["id"].as_u64() == Some(id) {
                        return value;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => panic!("MCP exited before reply {id}"),
            }
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self
            .child
            .try_wait()
            .expect("poll MCP during cleanup")
            .is_none()
        {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn mcp_status(mcp: &mut McpProcess, id: u64) -> Value {
    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": "backend.status", "arguments": {}}
    }));
    let reply = mcp.reply(id);
    assert!(
        reply.get("error").is_none(),
        "MCP status protocol error: {reply}"
    );
    assert_eq!(
        reply["result"]["isError"], false,
        "MCP status tool error: {reply}"
    );
    reply
}

fn mcp_rss(pid: u32) -> Option<u64> {
    let pid_text = pid.to_string();
    let output = ProcessCommand::new("ps")
        .args(["-o", "rss=", "-p", pid_text.as_str()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn tar_archive(path: &str, content: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    let mut header = [0_u8; 512];
    header[..path.len()].copy_from_slice(path.as_bytes());
    let size = format!("{:011o}\0", content.len());
    header[124..136].copy_from_slice(size.as_bytes());
    header[156] = b'0';
    header[148..156].fill(b' ');
    let checksum = header.iter().map(|byte| u64::from(*byte)).sum::<u64>();
    let checksum = format!("{:06o}\0 ", checksum);
    header[148..156].copy_from_slice(checksum.as_bytes());
    archive.extend_from_slice(&header);
    archive.extend_from_slice(content);
    archive.resize(archive.len().div_ceil(512) * 512, 0);
    archive.extend_from_slice(&[0; 1024]);
    archive
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            use std::fmt::Write as _;
            let _ = write!(text, "{byte:02x}");
            text
        })
}

fn registry_server(archive: Vec<u8>) -> (String, JoinHandle<()>, std::sync::Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind registry");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("registry address")
    );
    let digest = *CapabilityArtifactId::from_value(&archive).as_bytes();
    let feed = format!(
        concat!(
            "{{\"schema\":1,\"next\":\"{}\",\"items\":[{{",
            "\"name\":\"demo\",\"version\":\"1.2.3\",",
            "\"archive\":\"{}/archive\",\"blake3\":\"{}\",",
            "\"provenance\":\"{}\"}}]}}"
        ),
        hex(&[7; 32]),
        endpoint,
        hex(&digest),
        hex(&[9; 32]),
    )
    .into_bytes();
    let requests = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let server = thread::spawn(move || {
        for body in [feed, archive] {
            let (mut stream, _) = listener.accept().expect("accept registry request");
            observed.fetch_add(1, Ordering::Relaxed);
            let mut request = [0_u8; 8192];
            let length = stream.read(&mut request).expect("read registry request");
            let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
            assert!(
                request.contains("authorization: bearer journey-token"),
                "registry request omitted configured authorization"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write registry headers");
            stream.write_all(&body).expect("write registry body");
        }
    });
    (endpoint, server, requests)
}

fn registry_args(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    registry: &str,
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
        OsString::from("--registry-endpoint"),
        OsString::from(registry),
        OsString::from("--registry-ecosystem"),
        OsString::from("cargo"),
        OsString::from("--registry-auth"),
        OsString::from("Bearer journey-token"),
        OsString::from("--max-frame"),
        OsString::from("1048576"),
        OsString::from("--timeout-ms"),
        OsString::from("3000"),
    ];
    if offline {
        args.push(OsString::from("--registry-offline"));
    }
    args
}

fn cli_add_registry(endpoint: &Path, workspace: &Path) -> Output {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("add")
        .arg("pkg:cargo/demo@1.2.3");
    scrub(&mut command);
    bounded(command, "CLI registry add", None)
}

#[test]
#[allow(clippy::too_many_lines)]
fn cold_restart_preserves_atomic_roots_live_subscriptions_and_gui_shelf() {
    let total = Instant::now();
    let fixture = unique_root("journey");
    let project = restart_project(&fixture);
    write_identity_regressions(&project);
    write_ignored_javascript_outputs(&project);
    let boundary_project = large_project(&fixture);
    let workspace = fixture.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let authority = authority_secret(&workspace);
    let endpoint = backend_runtime::derive_endpoint(&workspace);
    let mut observations = Vec::new();

    let gui_started = Instant::now();
    let first_launch = run_gui(&fixture, "first-launch");
    assert_eq!(first_launch["journey"], "first-launch");
    assert_eq!(
        first_launch["onboarding"], true,
        "cold GUI launch skipped native onboarding"
    );
    note(&mut observations, "gui-first-launch", gui_started, None);

    let mut daemon = launch(&endpoint, &workspace, &authority, false, 180_000);
    let launch_started = Instant::now();
    let mut session = Session::connect(&endpoint).expect("connect empty session");
    let empty = session.health().expect("read empty first-launch health");
    assert_eq!(empty.row_count(), 0, "first local root was not empty");
    note(
        &mut observations,
        "daemon-empty-launch",
        launch_started,
        Some(&daemon),
    );

    let ingest_started = Instant::now();
    session
        .index(&project.to_string_lossy())
        .expect("submit first real project");
    let first = wait_ingest(&mut session, 2);
    for ignored in [
        "IgnoredNodeModulesBeacon",
        "IgnoredDistBeacon",
        "IgnoredNextBeacon",
        "IgnoredBuildBeacon",
        "IgnoredCoverageBeacon",
        "IgnoredByGitignoreBeacon",
    ] {
        let value = names_json(&endpoint, &workspace, &project, ignored);
        assert_eq!(
            records_for(&value, "src/ignored-by-gitignore.js", ignored).len(),
            0,
            "ignored JavaScript build/source output {ignored} leaked into the index: {value}"
        );
        assert_eq!(
            value["records"].as_array().map_or(0, Vec::len),
            0,
            "ignored JavaScript output {ignored} produced an index row: {value}"
        );
    }
    let old_root = first.revision().root();
    let first_identity = search_identity(&endpoint, &workspace, &project);
    note(
        &mut observations,
        "ingest-first-project",
        ingest_started,
        Some(&daemon),
    );

    for name in ["shelf-alpha", "shelf-beta"] {
        let created = session
            .surface(SurfaceCommand::ProjectCreate {
                name: ProjectName::new(name).expect("admit shelf name"),
                lockfile: None,
            })
            .expect("create shelf project");
        assert!(matches!(created, SurfaceReply::ProjectCreated(_)));
    }
    let shelf_before = projects(&mut session);
    assert_eq!(
        shelf_before.len(),
        2,
        "multiple shelf projects were not durable"
    );

    let identity_after_shelf = search_identity(&endpoint, &workspace, &project);
    assert_eq!(
        identity_after_shelf, first_identity,
        "shelf mutation changed row identity"
    );
    let expected_regressions = wait_regression_snapshot(&endpoint, &workspace, &project);
    let before_graceful = surface_evidence(&endpoint, &workspace, &project, old_root);
    assert_eq!(
        before_graceful.search, first_identity,
        "pre-restart search identity drifted"
    );
    assert_eq!(before_graceful.regressions, expected_regressions);

    let gui_project_started = Instant::now();
    let gui_project = run_gui(&fixture, "choose-project");
    assert_eq!(gui_project["journey"], "choose-project");
    assert!(gui_project["shelf_count"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(gui_project["onboarding"], false);
    note(
        &mut observations,
        "gui-native-project-choice",
        gui_project_started,
        None,
    );

    let graceful_started = Instant::now();
    surface_matrix::graceful_shutdown(&endpoint);
    let graceful_status = daemon.wait_exit(PROCESS_DEADLINE);
    assert!(
        graceful_status.success(),
        "graceful daemon shutdown failed: {graceful_status}"
    );
    wait_for_socket_dead(&endpoint);
    daemon = launch(&endpoint, &workspace, &authority, false, 180_000);
    let mut graceful_session = Session::connect(&endpoint).expect("connect graceful restart");
    let graceful_health = graceful_session
        .health()
        .expect("read graceful restart health");
    assert_eq!(
        graceful_health.revision().root(),
        old_root,
        "graceful restart admitted a non-atomic root"
    );
    assert_eq!(
        projects(&mut graceful_session),
        shelf_before,
        "graceful restart changed shelf project identities"
    );
    let after_graceful = surface_evidence(&endpoint, &workspace, &project, old_root);
    assert_eq!(
        after_graceful, before_graceful,
        "status/search/names/outline/query identity changed after graceful restart"
    );
    note(
        &mut observations,
        "graceful-daemon-restart",
        graceful_started,
        Some(&daemon),
    );

    let mut subscription = UnixSubscriptionTransport::connect(&endpoint)
        .expect("connect live subscription before SIGKILL");
    let (subscription_root, subscription_cursor) = subscription
        .bootstrap_root()
        .expect("bootstrap live subscription");
    assert_eq!(subscription_root.root(), old_root);

    let journal = workspace.join("view.journal");
    let before_journal = std::fs::metadata(&journal)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let index_started = Instant::now();
    let mut inflight = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    inflight
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("--workspace")
        .arg(&workspace)
        .arg("--project")
        .arg(&boundary_project)
        .arg("index")
        .arg(&boundary_project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    scrub(&mut inflight);
    let mut inflight = inflight.spawn().expect("spawn in-flight index");
    let boundary_end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < boundary_end {
        if std::fs::metadata(&journal)
            .map(|metadata| metadata.len() > before_journal)
            .unwrap_or(false)
        {
            break;
        }
        if inflight.try_wait().expect("poll in-flight index").is_some() {
            break;
        }
        thread::sleep(POLL);
    }
    let _ = daemon.kill_now();
    wait_for_socket_dead(&endpoint);
    if inflight
        .try_wait()
        .expect("poll in-flight index before kill")
        .is_none()
    {
        inflight.kill().expect("SIGKILL in-flight index");
    }
    let _ = inflight.wait();
    note(
        &mut observations,
        "sigkill-safe-boundary",
        index_started,
        None,
    );

    let restart_started = Instant::now();
    daemon = launch(&endpoint, &workspace, &authority, false, 180_000);
    let recovered = Session::connect(&endpoint)
        .expect("connect recovered session")
        .health()
        .expect("read recovered health");
    let recovered_root = recovered.revision().root();
    let mut recovered_session = Session::connect(&endpoint).expect("connect post-restart index");
    let after_sigkill = surface_evidence(&endpoint, &workspace, &project, recovered_root);
    assert_eq!(
        after_sigkill.search, before_graceful.search,
        "search row identity drifted after SIGKILL"
    );
    assert_eq!(
        after_sigkill.names, before_graceful.names,
        "names row identity drifted after SIGKILL"
    );
    assert!(
        after_sigkill
            .outline
            .contains(&before_graceful.search.coordinate),
        "outline lost the stable row after SIGKILL"
    );
    for (coordinate, _, _, _, _) in &before_graceful.regressions.0 {
        assert!(
            after_sigkill.outline.contains(coordinate),
            "outline lost identity regression {coordinate} after SIGKILL"
        );
        assert!(
            after_sigkill.query_coordinates.contains(coordinate),
            "graph query lost identity regression {coordinate} after SIGKILL"
        );
    }
    assert!(
        after_sigkill
            .query_coordinates
            .is_superset(&before_graceful.query_coordinates),
        "graph query lost rows after SIGKILL"
    );
    assert_eq!(
        after_sigkill.regressions, before_graceful.regressions,
        "identity regression rows changed after SIGKILL"
    );
    assert_eq!(
        after_sigkill.query_revision,
        ViewRevision::from(recovered_root),
        "graph query basis changed after SIGKILL"
    );
    recovered_session
        .index(&boundary_project.to_string_lossy())
        .expect("finish boundary project after restart");
    let final_report = wait_ingest(&mut recovered_session, first.row_count().saturating_add(2));
    let new_root = final_report.revision().root();
    assert_ne!(
        new_root, old_root,
        "boundary project did not publish a new root"
    );
    assert!(
        recovered_root == old_root || recovered_root == new_root,
        "recovered root was a partial state: recovered={recovered_root:?} old={old_root:?} new={new_root:?}"
    );
    assert!(
        after_sigkill.status.revision == before_graceful.status.revision
            || after_sigkill.status.revision == encode_id(new_root.as_bytes()),
        "status admitted a root outside old-or-new atomic roots"
    );
    let identity_after_restart = search_identity(&endpoint, &workspace, &project);
    assert_eq!(
        identity_after_restart, first_identity,
        "stable row identity drifted after SIGKILL"
    );
    let shelf_after = projects(&mut recovered_session);
    assert_eq!(
        shelf_after, shelf_before,
        "shelf project identity changed after restart"
    );
    note(
        &mut observations,
        "daemon-restart-recovery",
        restart_started,
        Some(&daemon),
    );

    let reconnect_started = Instant::now();
    let mut reconnected_subscription = UnixSubscriptionTransport::connect(&endpoint)
        .expect("connect subscription after daemon restart");
    let resumed = reconnected_subscription
        .subscribe_with_certificate(
            SubscriptionRequest::new(subscription_cursor, 64).expect("subscription credit"),
            None,
        )
        .expect("resume live subscription after restart");
    match resumed {
        CursorRead::Events { cursor, .. } => {
            assert_eq!(
                cursor.root(),
                recovered_root,
                "subscription event cursor root drifted"
            );
        }
        CursorRead::Reset { cursor, root, .. } => {
            assert!(cursor.root() == root.root());
            assert!(root.root() == recovered_root || root.root() == new_root);
        }
    }
    drop(subscription);
    note(
        &mut observations,
        "subscription-reconnect",
        reconnect_started,
        Some(&daemon),
    );

    let compiler_dir = workspace.join("compiler");
    assert!(
        compiler_dir.is_dir(),
        "semantic publication compiler root was not created"
    );
    assert!(
        workspace.join("product-state.json").is_file(),
        "durable product state was not published"
    );
    let package = PackageReference::parse(project.to_string_lossy().into_owned())
        .expect("admit local semantic package");
    let semantic_before = recovered_session
        .semantic_versions(package.clone())
        .expect("read semantic publication history");
    let semantic_root = final_report.revision().root();

    let offline_started = Instant::now();
    let _ = daemon.kill_now();
    wait_for_socket_dead(&endpoint);
    daemon = launch(&endpoint, &workspace, &authority, true, 180_000);
    let mut offline_session = Session::connect(&endpoint).expect("connect offline warm reopen");
    let offline = offline_session.health().expect("read offline warm health");
    assert_eq!(
        offline.revision().root(),
        semantic_root,
        "offline warm reopen changed root"
    );
    assert_eq!(
        search_identity(&endpoint, &workspace, &project),
        first_identity
    );
    let semantic_after = offline_session
        .semantic_versions(package)
        .expect("read semantic publication after offline reopen");
    assert_eq!(
        semantic_after, semantic_before,
        "semantic publication changed after restart"
    );
    note(
        &mut observations,
        "offline-warm-reopen",
        offline_started,
        Some(&daemon),
    );

    let mcp_started = Instant::now();
    let _ = daemon.kill_now();
    wait_for_socket_dead(&endpoint);
    daemon = launch(&endpoint, &workspace, &authority, true, 75_000);
    let journey_locald = PathBuf::from(env!("CARGO_BIN_EXE_backend-journey-locald"));
    let mut mcp = McpProcess::spawn(&endpoint, &workspace, &project, &journey_locald);
    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "cold-restart", "version": "1"}
        }
    }));
    let initialized = mcp.reply(1);
    assert!(initialized["result"]["protocolVersion"].is_string());
    mcp.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    }));
    let first_status = mcp_status(&mut mcp, 2);
    let first_mcp_root = first_status["result"]["structuredContent"]["revision"]
        .as_str()
        .expect("MCP status revision")
        .to_owned();
    thread::sleep(Duration::from_millis(71_000));
    let _ = daemon.kill_now();
    wait_for_socket_dead(&endpoint);
    daemon = launch(&endpoint, &workspace, &authority, true, 75_000);
    let second_status = mcp_status(&mut mcp, 3);
    let second_mcp_root = second_status["result"]["structuredContent"]["revision"]
        .as_str()
        .expect("MCP repeated status revision")
        .to_owned();
    assert_eq!(
        second_mcp_root, first_mcp_root,
        "MCP reconnect changed the admitted root"
    );
    assert_eq!(second_status["result"]["isError"], false);
    let mcp_observation = Observation {
        phase: "mcp-idle-reconnect".to_owned(),
        elapsed_ms: mcp_started.elapsed().as_millis(),
        rss_kib: mcp_rss(mcp.pid()),
    };
    eprintln!(
        "cold-restart phase={} elapsed_ms={} rss_kib={:?}",
        mcp_observation.phase, mcp_observation.elapsed_ms, mcp_observation.rss_kib
    );
    observations.push(mcp_observation);
    drop(mcp);

    let corruption_started = Instant::now();
    let _ = daemon.kill_now();
    wait_for_socket_dead(&endpoint);
    let journal_bytes = std::fs::read(&journal).expect("read journal for corruption test");
    assert!(
        journal_bytes.len() > 50,
        "view journal did not contain a complete frame"
    );
    let payload_length =
        u64::from_be_bytes(journal_bytes[10..18].try_into().expect("journal length")) as usize;
    let frame_length = 50_usize.saturating_add(payload_length);
    assert!(
        frame_length <= journal_bytes.len(),
        "journal frame was truncated before corruption test"
    );
    let mut corrupt = journal_bytes.clone();
    corrupt[50] ^= 0x5a;
    // Keep a complete frame after the damaged one so recovery cannot classify
    // the checksum failure as an allowed torn final frame.
    corrupt.extend_from_slice(&journal_bytes[..frame_length]);
    std::fs::write(&journal, &corrupt).expect("write deliberately corrupt journal");
    let bad_args = locald_args(&endpoint, &workspace, &authority, true, 180_000);
    let mut bad = ProcessGuard::locald(&bad_args);
    let bad_status = bad.wait_exit(Duration::from_secs(20));
    assert!(
        !bad_status.success(),
        "corrupt journal unexpectedly opened locald"
    );
    assert!(
        UnixStream::connect(&endpoint).is_err(),
        "corrupt journal exposed a listener"
    );
    std::fs::write(&journal, &journal_bytes).expect("restore journal after fail-closed check");
    daemon = launch(&endpoint, &workspace, &authority, true, 180_000);
    let restored = Session::connect(&endpoint)
        .expect("connect restored journal")
        .health()
        .expect("read restored journal health");
    assert_eq!(restored.revision().root(), semantic_root);
    note(
        &mut observations,
        "corruption-fail-closed",
        corruption_started,
        Some(&daemon),
    );

    println!(
        "{}",
        serde_json::to_string(&json!({
            "schema": "nudox.cold-restart.v3",
            "old_root": format!("{old_root:?}"),
            "recovered_root": format!("{recovered_root:?}"),
            "new_root": format!("{new_root:?}"),
            "observations": observations,
            "total_elapsed_ms": total.elapsed().as_millis(),
        }))
        .expect("encode cold restart report")
    );
}

#[test]
fn registry_archive_cache_reuses_published_object_after_process_restart() {
    let root = unique_root("registry");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create registry workspace");
    let authority = authority_secret(&workspace);
    let endpoint = backend_runtime::derive_endpoint(&workspace);
    let archive = tar_archive("package/src/lib.rs", b"pub fn from_registry_restart() {}\n");
    let (registry, server, requests) = registry_server(archive);
    let first_args = registry_args(&endpoint, &workspace, &authority, &registry, false);
    let mut first = ProcessGuard::locald(&first_args);
    wait_for_socket(&endpoint, &mut first);
    let added = cli_add_registry(&endpoint, &workspace);
    assert!(
        added.status.success(),
        "registry add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&added.stdout),
        String::from_utf8_lossy(&added.stderr)
    );
    server.join().expect("registry server");
    assert_eq!(
        requests.load(Ordering::Relaxed),
        2,
        "registry did not receive feed and archive"
    );
    let registry_url = registry.clone();
    let registry_endpoint =
        RegistryEndpoint::new(RegistryEcosystem::Cargo, registry).expect("admit registry endpoint");
    let journal =
        storage_root(&workspace.join("registry"), &registry_endpoint).join("registry.journal");
    let journal_len = std::fs::metadata(&journal)
        .expect("registry journal after first add")
        .len();
    let staging = workspace.join("registry").join("registry-staging");
    assert!(
        staging.exists(),
        "registry archive staging root was not created"
    );
    let _ = first.kill_now();
    wait_for_socket_dead(&endpoint);

    let second_args = registry_args(&endpoint, &workspace, &authority, &registry_url, true);
    let mut second = ProcessGuard::locald(&second_args);
    wait_for_socket(&endpoint, &mut second);
    let reused = cli_add_registry(&endpoint, &workspace);
    assert!(
        reused.status.success(),
        "offline restart add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&reused.stdout),
        String::from_utf8_lossy(&reused.stderr)
    );
    assert_eq!(
        std::fs::metadata(&journal)
            .expect("registry journal after offline reuse")
            .len(),
        journal_len,
        "offline restart add advanced the registry feed cursor"
    );
}

#[test]
fn derived_endpoint_is_short_for_windows_and_unix_portable_launchers() {
    let root = unique_root("endpoint");
    let long = root
        .join("nix-shell-simulated-long-temporary-prefix")
        .join("a-very-long-workspace-name-that-must-never-be-copied-into-a-native-socket-path");
    std::fs::create_dir_all(&long).expect("create long endpoint workspace");
    let endpoint = backend_runtime::derive_endpoint(&long);
    let endpoint_text = endpoint.to_string_lossy();
    assert!(
        endpoint_text.len() <= 100,
        "derived endpoint exceeded portable bound: {endpoint_text}"
    );
    assert!(
        !endpoint.starts_with(std::env::temp_dir()),
        "endpoint leaked TMPDIR into socket path"
    );
}
