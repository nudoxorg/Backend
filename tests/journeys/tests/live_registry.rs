//! Opt-in live registry journeys over the production locald, CLI, MCP, and
//! desktop transport seams.
//!
//! The cases are the same coordinates that the pinned Nix fleet corpus uses.
//! The test never replaces an upstream response with a fixture: it asks the
//! real registry's native metadata endpoint for one exact release, downloads
//! the verified archive, stages it through locald, and then checks every
//! client surface against the resulting durable view.  Because this is a
//! network and native-toolchain lane it is intentionally ignored in ordinary
//! offline CI.  Run it with `NUDOX_LIVE_REGISTRY=1` and `--ignored`.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use backend_client::Session;
use backend_desktop::Model as DesktopModel;
use backend_library::{
    Command, CommandDto, CommandReply, PackageReference, ProductText, ProjectName, RowId,
    SurfaceCommand, SurfaceReply,
};
use backend_mcp::{decode_reply, encode_request};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(180);
const STARTUP_DEADLINE: Duration = Duration::from_secs(45);
const AUTHORITY_SECRET: [u8; 32] = [0x5a; 32];
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// A real package whose archive is pinned by `.config/nix/corpus-pins.json`.
#[derive(Clone, Copy, Debug)]
struct LiveCase {
    lane: &'static str,
    coordinate: &'static str,
    endpoint: &'static str,
    package_name: &'static str,
    ecosystem: &'static str,
    /// Every lane is a release gate. A native source without a raw archive
    /// digest still passes through HTTPS authority admission and becomes
    /// content addressed by the durable owner; it is never downgraded to a
    /// coverage-only terminal.
    ingest: bool,
}

const CASES: &[LiveCase] = &[
    LiveCase {
        lane: "rust",
        coordinate: "cargo:hashbrown@0.14.5",
        endpoint: "https://index.crates.io",
        package_name: "hashbrown",
        ecosystem: "cargo",
        ingest: true,
    },
    LiveCase {
        lane: "typescript",
        coordinate: "npm:@babel/parser@7.26.8",
        endpoint: "https://registry.npmjs.org",
        package_name: "@babel/parser",
        ecosystem: "npm",
        ingest: true,
    },
    LiveCase {
        lane: "python",
        coordinate: "pypi:tomli@2.2.1",
        endpoint: "https://pypi.org",
        package_name: "tomli",
        ecosystem: "pypi",
        ingest: true,
    },
    LiveCase {
        lane: "java",
        coordinate: "maven:com.fasterxml.jackson.core:jackson-annotations@2.16.1",
        endpoint: "https://repo1.maven.org/maven2",
        package_name: "com.fasterxml.jackson.core:jackson-annotations",
        ecosystem: "maven",
        ingest: true,
    },
    LiveCase {
        lane: "csharp",
        coordinate: "nuget:nullable@1.3.1",
        // The adapter must discover RegistrationsBaseUrl from NuGet's service
        // index; a registration leaf is intentionally not accepted as the
        // configured source identity.
        endpoint: "https://api.nuget.org/v3/index.json",
        package_name: "nullable",
        ecosystem: "nuget",
        ingest: true,
    },
    LiveCase {
        lane: "go",
        coordinate: "golang:github.com/BurntSushi/toml@v1.4.0",
        endpoint: "https://proxy.golang.org",
        package_name: "github.com/BurntSushi/toml",
        ecosystem: "golang",
        ingest: true,
    },
    LiveCase {
        lane: "cpp",
        coordinate: "cpp:conan/zlib@1.3.1",
        endpoint: "https://center2.conan.io",
        package_name: "zlib",
        ecosystem: "cpp",
        ingest: true,
    },
];

#[derive(Debug)]
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn spawn(args: &[OsString]) -> Self {
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn locald");
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

impl ChildGuard {
    /// Stops locald without giving it a graceful shutdown path. The next
    /// process must recover the same workspace journal and root.
    fn crash(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        if child
            .try_wait()
            .expect("poll locald before crash")
            .is_none()
        {
            child.kill().expect("kill locald for crash recovery");
        }
        child.wait().expect("wait crashed locald");
    }
}

fn run_bounded(mut command: ProcessCommand, label: &str) -> Output {
    run_bounded_with_input(&mut command, &[], label)
}

fn run_bounded_with_input(command: &mut ProcessCommand, input: &[u8], label: &str) -> Output {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {label}: {error}"));
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).expect("write child input");
    }
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
    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
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

fn unique_root(case: LiveCase) -> PathBuf {
    if let Some(base) = std::env::var_os("NUDOX_LIVE_WORKSPACE_ROOT") {
        let path = PathBuf::from(base).join(case.lane);
        if path.exists() {
            if std::env::var_os("NUDOX_LIVE_RESET_WORKSPACE").is_some() {
                std::fs::remove_dir_all(&path).expect("reset live workspace");
            } else {
                panic!(
                    "live workspace {} already exists; set NUDOX_LIVE_RESET_WORKSPACE=1 to replace it",
                    path.display()
                );
            }
        }
        std::fs::create_dir_all(&path).expect("create reusable live journey root");
        return path;
    }
    let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "nudox-live-{}-{}-{}",
        case.lane,
        std::process::id(),
        serial
    ));
    std::fs::create_dir_all(&path).expect("create live journey root");
    path
}

fn keep_workspace() -> bool {
    std::env::var_os("NUDOX_LIVE_KEEP_WORKSPACE").is_some()
}

fn endpoint_for(workspace: &Path) -> PathBuf {
    backend_runtime::derive_endpoint(workspace)
}

fn authority_secret(root: &Path) -> PathBuf {
    let path = root.join("authority.secret");
    std::fs::write(&path, AUTHORITY_SECRET).expect("write authority secret");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("restrict authority secret");
    path
}

fn locald_args(
    endpoint: &Path,
    workspace: &Path,
    authority: &Path,
    case: LiveCase,
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
        OsString::from(case.endpoint),
        OsString::from("--registry-ecosystem"),
        OsString::from(case.ecosystem),
        OsString::from("--registry-native"),
        OsString::from("--max-frame"),
        OsString::from("1048576"),
        OsString::from("--timeout-ms"),
        OsString::from("15000"),
        OsString::from("--idle-timeout-ms"),
        OsString::from("0"),
    ];
    if offline {
        args.push(OsString::from("--registry-offline"));
    }
    args
}

fn unconfigured_locald_args(endpoint: &Path, workspace: &Path, authority: &Path) -> Vec<OsString> {
    let case = CASES[0];
    let configured = locald_args(endpoint, workspace, authority, case, false);
    let mut args = Vec::with_capacity(configured.len());
    let mut at = 0;
    while at < configured.len() {
        match configured[at].to_str() {
            Some("--registry-endpoint") | Some("--registry-ecosystem") => at += 2,
            Some("--registry-native") => at += 1,
            _ => {
                args.push(configured[at].clone());
                at += 1;
            }
        }
    }
    args
}

fn wait_for_socket(path: &Path, child: &mut ChildGuard) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    while Instant::now() < deadline {
        assert!(child.running(), "locald exited before readiness");
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && metadata.file_type().is_socket()
            && UnixStream::connect(path).is_ok()
        {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for locald socket {}", path.display());
}

fn purl_with_version(case: LiveCase) -> String {
    let coordinate = case
        .coordinate
        .split_once(':')
        .expect("Nix coordinate has ecosystem separator")
        .1;
    let (name, version) = coordinate
        .rsplit_once('@')
        .expect("Nix coordinate has version");
    let name = if case.ecosystem == "golang" {
        name.to_owned()
    } else if case.ecosystem == "maven" {
        name.replace(':', "/")
    } else {
        name.to_owned()
    };
    let package_type = if case.ecosystem == "cpp" {
        "generic"
    } else {
        case.ecosystem
    };
    format!("pkg:{package_type}/{name}@{version}")
}

fn assert_nix_pin(case: LiveCase) {
    let pins: serde_json::Value =
        serde_json::from_str(include_str!("../../../.config/nix/corpus-pins.json"))
            .expect("decode pinned corpus manifest");
    let found = pins
        .as_array()
        .expect("corpus pins array")
        .iter()
        .any(|row| {
            let coordinate_matches = if case.lane == "cpp" {
                row.get("lane").and_then(serde_json::Value::as_str) == Some("clang")
                    && row.get("coordinate").and_then(serde_json::Value::as_str) == Some("zlib")
            } else {
                row.get("lane").and_then(serde_json::Value::as_str) == Some(case.lane)
                    && row.get("coordinate").and_then(serde_json::Value::as_str)
                        == Some(case.coordinate)
            };
            coordinate_matches
                && row.get("url").and_then(serde_json::Value::as_str).is_some()
                && row
                    .get("hash")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
        });
    assert!(
        found,
        "live case {} is not pinned in the Nix corpus",
        case.coordinate
    );
}

fn curl_get(url: &str, label: &str) -> Vec<u8> {
    let mut command = ProcessCommand::new("curl");
    command
        .arg("--fail")
        .arg("--silent")
        .arg("--show-error")
        .arg("--location")
        .arg("--max-time")
        .arg("30")
        .arg(url);
    let output = run_bounded(command, label);
    assert_process_success(&output, label);
    output.stdout
}

/// Verifies the real native route shape before locald is involved. This
/// catches a deceptively common integration error where one configured URL is
/// sent to every ecosystem adapter. Maven group/artifact paths, NuGet's
/// service index, Go's uppercase escaping, Conan revisions, and PyPI wheel
/// only releases all have different protocol contracts.
fn assert_native_protocol_routes() {
    let maven = curl_get(
        "https://repo1.maven.org/maven2/com/fasterxml/jackson/core/jackson-annotations/maven-metadata.xml",
        "Maven group/artifact metadata",
    );
    let maven = String::from_utf8(maven).expect("Maven metadata UTF-8");
    assert!(maven.contains("<version>2.16.1</version>"));
    let sha1 = curl_get(
        "https://repo1.maven.org/maven2/com/fasterxml/jackson/core/jackson-annotations/2.16.1/jackson-annotations-2.16.1-sources.jar.sha1",
        "Maven archive sidecar",
    );
    assert_eq!(sha1.len(), 40, "Maven SHA-1 sidecar must be one digest");

    let nuget: serde_json::Value = serde_json::from_slice(&curl_get(
        "https://api.nuget.org/v3/index.json",
        "NuGet service index",
    ))
    .expect("NuGet service index JSON");
    let registration = nuget
        .get("resources")
        .and_then(serde_json::Value::as_array)
        .and_then(|resources| {
            resources.iter().find(|resource| {
                resource
                    .get("@type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| kind.starts_with("RegistrationsBaseUrl"))
            })
        })
        .and_then(|resource| resource.get("@id"))
        .and_then(serde_json::Value::as_str)
        .expect("NuGet service index registration resource");
    assert!(registration.starts_with("https://"));
    let registration_url = format!("{registration}nullable/index.json");
    let registration: serde_json::Value =
        serde_json::from_slice(&curl_get(&registration_url, "NuGet registration document"))
            .expect("NuGet registration JSON");
    assert!(registration.get("items").is_some());

    let go = String::from_utf8(curl_get(
        "https://proxy.golang.org/github.com/!burnt!sushi/toml/@v/list",
        "Go escaped module list",
    ))
    .expect("Go version list UTF-8");
    assert!(go.lines().any(|line| line.trim() == "v1.4.0"));

    let conan: serde_json::Value = serde_json::from_slice(&curl_get(
        "https://center2.conan.io/v2/conans/zlib/1.3.1/_/_/revisions",
        "Conan revisions",
    ))
    .expect("Conan revisions JSON");
    assert!(
        conan
            .get("revisions")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|revisions| !revisions.is_empty())
    );

    let wheels: serde_json::Value = serde_json::from_slice(&curl_get(
        "https://pypi.org/pypi/nvidia-cublas-cu12/json",
        "PyPI wheel-only release metadata",
    ))
    .expect("PyPI wheel-only JSON");
    let wheel_only = wheels
        .get("releases")
        .and_then(serde_json::Value::as_object)
        .and_then(|releases| {
            releases.values().find(|files| {
                files.as_array().is_some_and(|files| {
                    !files.is_empty()
                        && files.iter().all(|file| {
                            file.get("packagetype").and_then(serde_json::Value::as_str)
                                == Some("bdist_wheel")
                        })
                })
            })
        });
    assert!(wheel_only.is_some(), "PyPI wheel-only release disappeared");
}

fn cli_surface(endpoint: &Path, command: SurfaceCommand) -> Output {
    let body = serde_json::to_string(&command).expect("encode surface command");
    let mut process = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    process
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--format")
        .arg("json")
        .arg("surface")
        .arg(body);
    run_bounded(process, "live CLI surface")
}

fn cli_add(endpoint: &Path, coordinate: &str) -> Output {
    let mut process = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    process
        .arg("--endpoint")
        .arg(endpoint)
        .arg("add")
        .arg(coordinate);
    run_bounded(process, "live CLI add")
}

fn mcp_surface(endpoint: &Path, workspace: &Path, command: SurfaceCommand) -> Output {
    let request = CommandDto::new(1, Command::Surface(command));
    let encoded = encode_request(&request).expect("encode MCP request");
    let mut process = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
    process
        .arg("--framed")
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace);
    run_bounded_with_input(&mut process, &encoded, "live MCP surface")
}

fn assert_process_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_profile_json(output: &Output, purl: &str) {
    assert_process_success(output, "CLI profile");
    let text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("CLI profile was not typed JSON: {error}: {text}"));
    assert_eq!(
        value.get("answer").and_then(serde_json::Value::as_str),
        Some("product")
    );
    assert_eq!(
        value.get("heading").and_then(serde_json::Value::as_str),
        Some("package-profile")
    );
    assert!(
        text.contains(purl),
        "profile omitted exact coordinate: {text}"
    );
    assert!(
        text.contains("package-profile"),
        "profile reply shape changed: {text}"
    );
}

fn assert_surface_json(output: &Output, kind: &str, purl: &str) {
    assert_process_success(output, "CLI registry surface");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("CLI surface was not typed JSON: {error}"));
    assert_eq!(
        value.get("answer").and_then(serde_json::Value::as_str),
        Some("product")
    );
    assert_eq!(
        value.get("heading").and_then(serde_json::Value::as_str),
        Some(kind)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(purl),
        "CLI {kind} omitted exact coordinate: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn assert_mcp_profile(output: &Output, purl: &str) {
    assert_process_success(output, "MCP profile");
    let reply = decode_reply(&output.stdout).expect("decode MCP profile reply");
    let CommandReply::Surface(SurfaceReply::PackageProfile { latest, versions }) = reply.reply
    else {
        panic!("MCP profile reply changed shape: {:?}", reply.reply);
    };
    let latest = latest.expect("MCP profile omitted latest release");
    assert_eq!(latest.coordinate.as_str(), purl);
    assert!(versions >= 1, "MCP profile omitted recorded versions");
}

fn assert_registry_facts(endpoint: &Path, coordinate: &str, package_name: &str) -> u64 {
    let package = PackageReference::parse(coordinate).expect("registry package reference");
    let mut session = Session::connect(endpoint).expect("registry facts session connect");
    let record = match session
        .surface(SurfaceCommand::Package {
            package: package.clone(),
        })
        .expect("registry package details")
    {
        SurfaceReply::Package(records) => {
            assert_eq!(records.len(), 1, "package details returned duplicate rows");
            let record = records.first().expect("package details row");
            assert_eq!(record.coordinate.as_str(), coordinate);
            assert_eq!(record.name.as_str(), package_name);
            assert!(record.bytes > 0, "package details omitted archive bytes");
            assert_ne!(record.facts_version, [0; 32]);
            record.clone()
        }
        reply => panic!("package details reply changed shape: {reply:?}"),
    };

    let versions = session
        .surface(SurfaceCommand::PackageVersions {
            package: package.clone(),
        })
        .expect("registry package versions");
    let SurfaceReply::PackageVersions(versions) = versions else {
        panic!("package versions reply changed shape");
    };
    assert!(
        versions
            .iter()
            .any(|version| version.coordinate == record.coordinate)
    );

    let dependents = session
        .surface(SurfaceCommand::Dependents {
            package: package.clone(),
        })
        .expect("registry dependents");
    assert!(matches!(
        dependents,
        SurfaceReply::Dependents(backend_library::RegistryMetadata::Recorded(_))
            | SurfaceReply::Dependents(backend_library::RegistryMetadata::NotRecorded(_))
    ));
    let owner = session
        .surface(SurfaceCommand::Owner {
            owner: ProductText::new(package_name).expect("registry owner name"),
        })
        .expect("registry owner metadata");
    assert!(matches!(
        owner,
        SurfaceReply::Owner(backend_library::RegistryMetadata::Recorded(_))
            | SurfaceReply::Owner(backend_library::RegistryMetadata::NotRecorded(_))
    ));
    record.bytes
}

fn assert_desktop_root(endpoint: &Path) -> (backend_library::ViewRoot, String) {
    let mut session = Session::connect(endpoint).expect("desktop session connect");
    let reply = session.packages().expect("desktop package snapshot");
    let CommandReply::Packages(snapshot) = reply.reply else {
        panic!("desktop package reply changed shape");
    };
    let root = snapshot.root;
    let model = DesktopModel::try_new(root.clone(), root.basis().root)
        .expect("desktop reducer admitted live root");
    assert_eq!(model.root().root(), root.root());
    assert!(
        model
            .root()
            .rows()
            .iter()
            .any(|row| row.label.starts_with("pkg:"))
    );

    // Exercise the semantic-side desktop journey against the same root. A
    // package-only row is still a valid indexed package, while a symbol row
    // unlocks document/source/graph probes. Those probes are deliberately
    // asserted as typed replies so missing compiler coverage cannot be
    // mistaken for an empty result.
    let search = session.search("pkg:", 25).expect("desktop code search");
    let CommandReply::Search(search_snapshot) = search.reply else {
        panic!("desktop code search reply changed shape");
    };
    assert_eq!(
        search_snapshot.root.basis().root,
        root.basis().root,
        "desktop search must retain the package view's source basis"
    );
    let names = session.names("pkg:", 25).expect("desktop semantic names");
    let CommandReply::Names(names_snapshot) = names.reply else {
        panic!("desktop semantic names reply changed shape");
    };
    assert_eq!(
        names_snapshot.root.basis().root,
        root.basis().root,
        "desktop names must retain the package view's source basis"
    );
    if let Some(row) = search_snapshot
        .root
        .rows()
        .iter()
        .find(|row| matches!(row.id, RowId::Symbol(_)))
    {
        let RowId::Symbol(symbol) = row.id else {
            unreachable!("symbol row predicate admitted a non-symbol");
        };
        let document = session
            .document_symbol(symbol)
            .expect("desktop declaration document");
        let CommandReply::Document(document) = document.reply else {
            panic!("desktop declaration document reply changed shape");
        };
        assert_eq!(document.basis, root.basis().root);
        let source = session.source(&row.label).expect("desktop source request");
        assert!(matches!(source.reply, CommandReply::Document(_)));
        let graph = session.graph_symbol(symbol).expect("desktop graph request");
        assert!(matches!(graph.reply, CommandReply::Graph(_)));
    }
    let project_name = format!("live-{}", std::process::id());
    let created = session
        .surface(SurfaceCommand::ProjectCreate {
            name: ProjectName::new(project_name).expect("project name"),
            lockfile: None,
        })
        .expect("desktop settings project create");
    let SurfaceReply::ProjectCreated(project) = created else {
        panic!("desktop project create reply changed shape");
    };
    let selected = project.name.as_str().to_owned();
    (root, selected)
}

fn live_artifact_root() -> PathBuf {
    std::env::var_os("NUDOX_LIVE_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/live-artifacts")
        })
}

fn native_metadata_endpoint(case: LiveCase) -> String {
    match case.ecosystem {
        "cargo" => "https://index.crates.io/ha/sh/hashbrown".to_owned(),
        "npm" => "https://registry.npmjs.org/@babel%2Fparser/7.26.8".to_owned(),
        "pypi" => "https://pypi.org/pypi/tomli/2.2.1/json".to_owned(),
        "maven" => "https://repo1.maven.org/maven2/com/fasterxml/jackson/core/jackson-annotations/maven-metadata.xml".to_owned(),
        "nuget" => "https://api.nuget.org/v3/index.json".to_owned(),
        "golang" => "https://proxy.golang.org/github.com/!burnt!sushi/toml/@v/v1.4.0.info".to_owned(),
        "cpp" => "https://center2.conan.io/v2/conans/zlib/1.3.1/_/_/revisions".to_owned(),
        ecosystem => panic!("missing native metadata route for {ecosystem}"),
    }
}

fn write_live_artifacts(
    case: LiveCase,
    coordinate: &str,
    endpoint: &Path,
    workspace: &Path,
    action_names: &[&str],
    archive_bytes: u64,
    elapsed: Duration,
) {
    let directory = live_artifact_root().join(case.lane);
    std::fs::create_dir_all(&directory).expect("create live artifact directory");
    let action_tree = serde_json::json!({
        "schema": "nudox.live.action-tree.v1",
        "lane": case.lane,
        "coordinate": coordinate,
        "endpoint": case.endpoint,
        "actions": action_names.iter().enumerate().map(|(index, name)| serde_json::json!({
            "id": index + 1,
            "name": name,
            "surface": if name.starts_with("mcp") { "mcp" } else if name.starts_with("cli") { "cli" } else { "desktop" },
        })).collect::<Vec<_>>(),
    });
    std::fs::write(
        directory.join("action-tree.json"),
        serde_json::to_vec_pretty(&action_tree).expect("encode live action tree"),
    )
    .expect("write live action tree");
    let receipt = serde_json::json!({
        "schema": "nudox.live.registry-receipt.v1",
        "lane": case.lane,
        "coordinate": coordinate,
        "workspace": workspace.display().to_string(),
        "metadata_endpoint": native_metadata_endpoint(case),
        "local_endpoint": endpoint.display().to_string(),
        "archive_authority": case.endpoint,
        "protocol": "native-release -> verified-archive -> confined-extraction -> durable-index",
        "outcome": "passed",
        "integrity": "verified",
        "staging": "confined",
        "index": "durable",
        "archive_bytes": archive_bytes,
        "elapsed_ms": elapsed.as_millis(),
        "surfaces": ["cli", "mcp", "desktop-model"],
        "gui_prepopulate": true,
        "gui_capture": "handoff-to-real-gpui-harness",
        "restart": true,
        "warm_offline_read": true,
        "actions": action_names,
    });
    std::fs::write(
        directory.join("receipt.json"),
        serde_json::to_vec_pretty(&receipt).expect("encode live receipt"),
    )
    .expect("write live receipt");
    let handoff = serde_json::json!({
        "schema": "nudox.gui.live-handoff.v1",
        "lane": case.lane,
        "coordinate": coordinate,
        "workspace": workspace.display().to_string(),
        "scene": "package",
        "requires_real_gpui": true,
        "capture_command": "main gui journey --mode warm",
        "note": "The registry journey records model/CLI/MCP evidence only; the GPUI harness owns rendered PNG capture."
    });
    std::fs::write(
        directory.join("gui-handoff.json"),
        serde_json::to_vec_pretty(&handoff).expect("encode GUI handoff"),
    )
    .expect("write GUI handoff");
    let mut checksums = serde_json::Map::new();
    for artifact in ["receipt.json", "action-tree.json", "gui-handoff.json"] {
        let bytes = std::fs::read(directory.join(artifact)).expect("read live artifact");
        assert!(!bytes.is_empty(), "live artifact {artifact} is blank");
        checksums.insert(
            artifact.to_owned(),
            serde_json::json!({
                "bytes": bytes.len(),
                "blake3": blake3::hash(&bytes).to_hex().to_string(),
            }),
        );
    }
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "nudox.live.artifact-manifest.v1",
            "lane": case.lane,
            "coordinate": coordinate,
            "algorithm": "blake3",
            "artifacts": checksums,
        }))
        .expect("encode live artifact manifest"),
    )
    .expect("write live artifact manifest");
}

fn selected_case() -> Vec<LiveCase> {
    let Some(value) = std::env::var_os("NUDOX_LIVE_REGISTRY") else {
        panic!("live registry lane is opt-in; set NUDOX_LIVE_REGISTRY=1 and run this ignored test");
    };
    assert_eq!(value, "1", "NUDOX_LIVE_REGISTRY must be exactly 1");
    let filter = std::env::var("NUDOX_LIVE_ECOSYSTEMS").ok();
    let selected = CASES
        .iter()
        .copied()
        .filter(|case| {
            filter.as_deref().is_none_or(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .any(|lane| lane == case.lane)
            })
        })
        .collect::<Vec<_>>();
    assert!(
        !selected.is_empty(),
        "NUDOX_LIVE_ECOSYSTEMS selected no cases"
    );
    selected
}

#[test]
#[ignore = "requires NUDOX_LIVE_REGISTRY=1 and a real locald process"]
fn unconfigured_registry_is_an_explicit_typed_empty_state() {
    let _ = selected_case();
    let case = CASES[0];
    let root = unique_root(case);
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create unconfigured workspace");
    let endpoint = endpoint_for(&workspace);
    let authority = authority_secret(&root);
    let mut owner = ChildGuard::spawn(&unconfigured_locald_args(&endpoint, &workspace, &authority));
    wait_for_socket(&endpoint, &mut owner);
    let coordinate = purl_with_version(case);
    let added = cli_add(&endpoint, &coordinate);
    assert_process_success(&added, "unconfigured package add");
    let mut session = Session::connect(&endpoint).expect("unconfigured desktop session");
    let profile = session
        .surface(SurfaceCommand::PackageProfile {
            package: PackageReference::parse(&coordinate).expect("unconfigured package reference"),
        })
        .expect("unconfigured package profile");
    let SurfaceReply::PackageProfile { latest, versions } = profile else {
        panic!("unconfigured package profile reply changed shape");
    };
    assert!(
        latest.is_none(),
        "unconfigured registry fabricated package facts"
    );
    assert_eq!(versions, 0);
    owner.crash();
    if !keep_workspace() {
        std::fs::remove_dir_all(root).expect("remove unconfigured journey root");
    }
}

#[test]
#[ignore = "requires NUDOX_LIVE_REGISTRY=1 and real registry/network/toolchain access"]
fn pinned_native_registries_ingest_through_cli_mcp_and_desktop() {
    assert_native_protocol_routes();
    for case in selected_case() {
        assert_nix_pin(case);
        let root = unique_root(case);
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("create live workspace");
        let endpoint = endpoint_for(&workspace);
        let authority = authority_secret(&root);
        let purl = purl_with_version(case);
        let coordinate = purl.clone();
        let started = Instant::now();

        let mut owner =
            ChildGuard::spawn(&locald_args(&endpoint, &workspace, &authority, case, false));
        wait_for_socket(&endpoint, &mut owner);

        assert!(case.ingest, "all live ecosystem cases must exercise ingest");
        let added = cli_add(&endpoint, &coordinate);
        assert_process_success(&added, "live CLI add");
        let recorded_name = if case.lane == "cpp" {
            // Conan's namespace is part of the durable native identity even
            // though the human package query remains `zlib`.
            "conan:zlib"
        } else {
            case.package_name
        };
        let archive_bytes = assert_registry_facts(&endpoint, &coordinate, recorded_name);
        let profile = cli_surface(
            &endpoint,
            SurfaceCommand::PackageProfile {
                package: PackageReference::parse(&coordinate).expect("package reference"),
            },
        );
        assert_profile_json(&profile, &coordinate);
        let package = PackageReference::parse(&coordinate).expect("CLI package reference");
        assert_surface_json(
            &cli_surface(
                &endpoint,
                SurfaceCommand::Package {
                    package: package.clone(),
                },
            ),
            "package",
            &coordinate,
        );
        assert_surface_json(
            &cli_surface(&endpoint, SurfaceCommand::PackageVersions { package }),
            "package-versions",
            &coordinate,
        );
        let indexed = cli_surface(
            &endpoint,
            SurfaceCommand::IndexSearch {
                query: ProductText::new(if case.lane == "java" {
                    "jackson-annotations"
                } else {
                    case.package_name
                })
                .expect("index query"),
                limit: 200,
            },
        );
        assert_process_success(&indexed, "live CLI index search");
        assert!(
            String::from_utf8_lossy(&indexed.stdout).contains(&coordinate),
            "index search omitted exact coordinate: {}",
            String::from_utf8_lossy(&indexed.stdout)
        );

        let mcp = mcp_surface(
            &endpoint,
            &workspace,
            SurfaceCommand::PackageProfile {
                package: PackageReference::parse(&coordinate).expect("MCP package reference"),
            },
        );
        assert_mcp_profile(&mcp, &coordinate);

        let (_root, project_name) = assert_desktop_root(&endpoint);
        let mut desktop = Session::connect(&endpoint).expect("desktop settings reconnect");
        let package = PackageReference::parse(&coordinate).expect("desktop project package");
        let added_to_project = desktop
            .surface(SurfaceCommand::ProjectAdd {
                project: backend_library::ProjectSelector::parse(&project_name)
                    .expect("project selector"),
                package: package.clone(),
            })
            .expect("desktop project add");
        assert!(matches!(added_to_project, SurfaceReply::ProjectAdded(_)));
        let projects = desktop
            .surface(SurfaceCommand::Projects)
            .expect("desktop projects");
        let SurfaceReply::Projects(projects) = projects else {
            panic!("desktop projects reply changed shape");
        };
        assert!(
            projects
                .iter()
                .any(|project| project.name.as_str() == project_name)
        );
        let deleted = desktop
            .surface(SurfaceCommand::ProjectDelete {
                project: backend_library::ProjectSelector::parse(&project_name)
                    .expect("delete project selector"),
            })
            .expect("desktop project delete");
        assert!(matches!(deleted, SurfaceReply::ProjectDeleted(_)));

        owner.crash();
        let mut offline_owner =
            ChildGuard::spawn(&locald_args(&endpoint, &workspace, &authority, case, true));
        wait_for_socket(&endpoint, &mut offline_owner);
        let warm = cli_add(&endpoint, &coordinate);
        assert_process_success(&warm, "offline warm-cache CLI add");
        let warm_profile = cli_surface(
            &endpoint,
            SurfaceCommand::PackageVersions {
                package: PackageReference::parse(&coordinate).expect("warm package reference"),
            },
        );
        assert_process_success(&warm_profile, "offline warm-cache package versions");
        assert!(
            String::from_utf8_lossy(&warm_profile.stdout).contains(&coordinate),
            "offline warm cache lost package version: {}",
            String::from_utf8_lossy(&warm_profile.stdout)
        );

        write_live_artifacts(
            case,
            &coordinate,
            &endpoint,
            &workspace,
            &[
                "native-metadata",
                "verified-archive",
                "confined-extraction",
                "durable-index",
                "cli-add",
                "cli-package-profile",
                "cli-package-versions",
                "cli-index-search",
                "mcp-package-profile",
                "desktop-package-root",
                "desktop-project-create",
                "desktop-project-add",
                "desktop-project-delete",
                "restart",
                "warm-offline-read",
            ],
            archive_bytes,
            started.elapsed(),
        );
        drop(offline_owner);
        if !keep_workspace() {
            std::fs::remove_dir_all(root).expect("remove live journey root");
        }
    }
}
