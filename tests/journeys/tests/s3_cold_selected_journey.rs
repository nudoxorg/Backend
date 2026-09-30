//! Cold locald process journey through exact selected S3 segment residency.

#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![allow(clippy::too_many_lines)]

use backend_client::LocalSemanticIndexClient;
use backend_engine::cluster_transport::{
    EndpointAddr, EndpointId, RemoteIndexCapability, RemoteIndexChannel, RemoteIndexOutcome,
    RemoteIndexRequest, RemoteIndexSessionHello, SecretKey, bind_direct, connect_remote_index,
};
use backend_engine::package_key;
use backend_extension_turso::{
    AuthorityNamespace, SelectedGeneration, TursoAuthority, reopen_selected_compiler_metadata,
};
use backend_replication::{
    ByteRange, FileSemanticRangeStore, HydrationCredits, IrHydrationPoll, LocalControlLimits,
    LocalControlRequest, SemanticRangeClientCheckpoint, SemanticRangeClientProgress,
    SemanticRangeGet, SemanticRangeRequest, SemanticTargetKey, TransportLimits, encode_request,
};
use backend_semantic::ir::{
    GenerationId, SemanticIrPlane, SemanticManifestRoot, SemanticPlaneImageKey, SemanticPlaneKind,
    SemanticPlaneRoot,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
use backend_store::{ArtifactBudget, FileStore, UntrustedObjectId};
use backend_store_s3::test_support::LoopbackS3;
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(180);
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-s3-cold-selected-{}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        private_directory(&root);
        Self(root.canonicalize().expect("canonical fixture root"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path)
        .unwrap_or_else(|error| panic!("create fixture directory {}: {error}", path.display()));
    let mut permissions = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("stat fixture directory {}: {error}", path.display()))
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("protect fixture directory {}: {error}", path.display()));
}

fn write_authority_secret(path: &Path) {
    std::fs::write(path, [0x71_u8; 32]).expect("write authority secret");
    let mut permissions = std::fs::metadata(path)
        .expect("stat authority secret")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).expect("protect authority secret");
}

fn clear_product_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("BACKEND_")
            || name.starts_with("NUDOX_")
            || matches!(name.as_ref(), "LD_PRELOAD" | "DYLD_INSERT_LIBRARIES")
        {
            command.env_remove(key);
        }
    }
}

fn rustc_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NUDOX_RUSTC").map(PathBuf::from)
        && path.is_file()
    {
        return path.canonicalize().expect("canonical configured rustc");
    }
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
        .unwrap_or_else(|| panic!("the real Rust compiler is required for this journey"))
}

fn cargo_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NUDOX_CARGO").map(PathBuf::from)
        && path.is_file()
    {
        return path.canonicalize().expect("canonical configured cargo");
    }
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path).find_map(|directory| {
                let candidate = directory.join("cargo");
                candidate
                    .is_file()
                    .then(|| candidate.canonicalize().ok())
                    .flatten()
            })
        })
        .unwrap_or_else(|| panic!("the real Cargo executable is required for this journey"))
}

fn cargo_home_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NUDOX_CARGO_HOME").map(PathBuf::from)
        && path.is_dir()
    {
        return path
            .canonicalize()
            .expect("canonical configured Cargo home");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".cargo"))
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| panic!("an existing Cargo home is required for this journey"))
}

struct Locald {
    child: Option<Child>,
    endpoint: PathBuf,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Locald {
    fn launch(
        endpoint: &Path,
        workspace: &Path,
        authority_secret: &Path,
        s3: Option<&LoopbackS3>,
        collect_remote_segments: bool,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-locald"));
        command
            .args([
                OsString::from("--endpoint"),
                endpoint.as_os_str().to_owned(),
                OsString::from("--workspace"),
                workspace.as_os_str().to_owned(),
                OsString::from("--profile"),
                OsString::from("builtin"),
                OsString::from("--authority-secret-file"),
                authority_secret.as_os_str().to_owned(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        clear_product_environment(&mut command);
        command.env("NUDOX_RUSTC", rustc_path());
        command
            .env("NUDOX_CARGO", cargo_path())
            .env("NUDOX_CARGO_HOME", cargo_home_path());
        if let Some(s3) = s3 {
            command
                .env("BACKEND_S3_ENDPOINT", s3.origin())
                .env("BACKEND_S3_BUCKET", "journey-bucket")
                .env("BACKEND_S3_REGION", "us-east-1")
                .env("BACKEND_S3_ACCESS_KEY_ID", "journey-access")
                .env("BACKEND_S3_SECRET_ACCESS_KEY", "journey-secret")
                .env("BACKEND_S3_PREFIX", "cold-journey/");
        }
        if collect_remote_segments {
            assert!(s3.is_some(), "remote segment collection requires S3 mode");
            command.env("BACKEND_JOURNEY_REMOTE_SEGMENT_GC", "1");
        }
        let mut child = command.spawn().expect("spawn production locald");
        let stderr = Arc::new(Mutex::new(Vec::new()));
        if let Some(stream) = child.stderr.take() {
            let captured = Arc::clone(&stderr);
            thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(line);
                }
            });
        }
        let mut locald = Self {
            child: Some(child),
            endpoint: endpoint.to_path_buf(),
            stderr,
        };
        locald.wait_ready();
        locald
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if self.endpoint.exists() {
                return;
            }
            if let Some(status) = self
                .child
                .as_mut()
                .and_then(|child| child.try_wait().expect("poll locald"))
            {
                panic!(
                    "locald exited before readiness ({status}): {}",
                    self.diagnostics()
                );
            }
            assert!(
                Instant::now() < deadline,
                "locald startup timed out: {}",
                self.diagnostics()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn diagnostics(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .join("\n")
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll locald"))
            .is_none()
    }

    fn kill_now(&mut self) -> Option<ExitStatus> {
        let child = self.child.as_mut()?;
        if child.try_wait().expect("poll locald before kill").is_none() {
            child.kill().expect("kill locald process");
        }
        let status = child.wait().expect("wait for killed locald");
        let _ = std::fs::remove_file(&self.endpoint);
        Some(status)
    }
}

impl Drop for Locald {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut()
            && child.try_wait().ok().flatten().is_none()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

struct ProcessRssSampler {
    stop: Arc<AtomicBool>,
    owner_pid: Arc<AtomicU32>,
    samples: Arc<AtomicUsize>,
    peak_kb: Arc<AtomicUsize>,
    join: Option<thread::JoinHandle<()>>,
}

impl ProcessRssSampler {
    fn start(owner_pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let current_owner = Arc::new(AtomicU32::new(owner_pid));
        let samples = Arc::new(AtomicUsize::new(0));
        let peak_kb = Arc::new(AtomicUsize::new(0));
        let thread_stop = Arc::clone(&stop);
        let thread_owner = Arc::clone(&current_owner);
        let thread_samples = Arc::clone(&samples);
        let thread_peak = Arc::clone(&peak_kb);
        let client_pid = std::process::id();
        let join = thread::Builder::new()
            .name("backend-remote-index-rss".to_owned())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    let owner_pid = thread_owner.load(Ordering::Acquire);
                    if let (Some(client), Some(owner)) =
                        (process_rss_kb(client_pid), process_rss_kb(owner_pid))
                    {
                        thread_peak.fetch_max(client.saturating_add(owner), Ordering::Relaxed);
                        thread_samples.fetch_add(1, Ordering::Relaxed);
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            })
            .expect("start remote owner/client RSS sampler");
        Self {
            stop,
            owner_pid: current_owner,
            samples,
            peak_kb,
            join: Some(join),
        }
    }

    fn set_owner_pid(&self, owner_pid: u32) {
        self.owner_pid.store(owner_pid, Ordering::Release);
    }

    fn finish(mut self) -> (usize, usize) {
        self.stop.store(true, Ordering::Release);
        self.join
            .take()
            .expect("RSS sampler thread exists")
            .join()
            .expect("join remote owner/client RSS sampler");
        (
            self.samples.load(Ordering::Relaxed),
            self.peak_kb.load(Ordering::Relaxed),
        )
    }
}

impl Drop for ProcessRssSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn process_rss_kb(pid: u32) -> Option<usize> {
    let pid = pid.to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn run(command: &mut Command, label: &str) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {label}: {error}"));
    let stdout = child.stdout.take().expect("capture child stdout");
    let stderr = child.stderr.take().expect("capture child stderr");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stdout = stdout;
        stdout.read_to_end(&mut bytes).expect("read child stdout");
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stderr = stderr;
        stderr.read_to_end(&mut bytes).expect("read child stderr");
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
                panic!("{label} exceeded {DEADLINE:?}");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("wait for {label}: {error}");
            }
        }
    };
    let output = Output {
        status,
        stdout: stdout_reader.join().expect("join child stdout reader"),
        stderr: stderr_reader.join().expect("join child stderr reader"),
    };
    assert!(
        output.status.success(),
        "{label} failed ({}): stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn cli_json(endpoint: &Path, workspace: &Path, project: &Path, arguments: &[&str]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .args(["--format", "json"])
        .args(arguments)
        .env("NO_COLOR", "1")
        .env("COLUMNS", "120");
    clear_product_environment(&mut command);
    let output = run(&mut command, &format!("CLI {arguments:?}"));
    serde_json::from_slice(&output.stdout).expect("parse CLI JSON")
}

fn init_remote_owner(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
) -> (EndpointId, std::net::SocketAddr) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("reserve owner Iroh port");
    let address = socket.local_addr().expect("read owner Iroh port");
    drop(socket);
    let address_text = address.to_string();
    let mut init = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    init.arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .args([
            "cluster",
            "owner",
            "init",
            "--bind",
            &address_text,
            "--advertise",
            &address_text,
        ]);
    clear_product_environment(&mut init);
    run(&mut init, "create direct Iroh index owner");

    let owner = cli_json(endpoint, workspace, project, &["cluster", "owner", "show"]);
    let peer_bytes = decode_hex_32(owner["endpoint"].as_str().expect("owner endpoint peer ID"));
    let peer = EndpointId::from_bytes(&peer_bytes).expect("decode owner Iroh peer ID");
    let address = owner["advertisedAddress"]
        .as_str()
        .expect("owner advertised direct address")
        .parse()
        .expect("parse owner advertised direct address");
    (peer, address)
}

fn issue_semantic_capability(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    client_peer: EndpointId,
    capability_path: &Path,
    coordinate: &str,
    request_budget: u32,
    byte_budget: u64,
) -> RemoteIndexCapability {
    let client_peer = hex(client_peer.as_bytes());
    let request_budget = request_budget.to_string();
    let byte_budget = byte_budget.to_string();
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .args([
            "cluster",
            "owner",
            "grant",
            "semantic",
            "create",
            "--client-peer",
        ])
        .arg(client_peer)
        .arg("--capability-file")
        .arg(capability_path)
        .arg("--package")
        .arg(project)
        .arg("--coordinate")
        .arg(coordinate)
        .args(["--profile", "rust-2024", "--request-budget"])
        .arg(request_budget)
        .arg("--byte-budget")
        .arg(byte_budget);
    clear_product_environment(&mut command);
    run(&mut command, "issue exact selected semantic read grant");
    let bytes = std::fs::read(capability_path).expect("read issued semantic capability");
    let payload = bytes
        .strip_prefix(b"BKRICP01")
        .expect("capability has the installed CLI file header");
    RemoteIndexCapability::decode(payload).expect("decode owner-signed semantic capability")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("write hex digit");
    }
    output
}

fn decode_hex_32(value: &str) -> [u8; 32] {
    assert_eq!(value.len(), 64, "peer ID is exactly 32 bytes of hex");
    let mut output = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] =
            u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII peer ID hex"), 16)
                .expect("valid peer ID hex");
    }
    output
}

fn semantic_profile() -> LanguageProfile {
    LanguageProfile::Rust(RustEdition::Rust2024)
}

fn semantic_coordinate(project_label: &str) -> String {
    let key = package_key(project_label);
    format!(
        "pkg:cargo/local-{}@0.0.0-local",
        backend_engine::encode_id(key.as_bytes())
    )
}

fn semantic_namespace(project_label: &str, coordinate: &str) -> AuthorityNamespace {
    let bytes = <[u8; 2]>::from(semantic_profile());
    AuthorityNamespace::semantic_profile(
        project_label,
        coordinate,
        "locald",
        "locald-product-v1",
        format!("{:02x}{:02x}/lower-ir", bytes[0], bytes[1]),
    )
    .expect("semantic authority namespace")
}

fn selected_generation(
    runtime: &tokio::runtime::Runtime,
    authority: &TursoAuthority,
    namespace: &AuthorityNamespace,
) -> SelectedGeneration {
    let frontier = runtime
        .block_on(authority.selected_frontier(namespace))
        .expect("read selected Turso frontier")
        .expect("selected Turso frontier exists");
    runtime
        .block_on(authority.selected_generation(namespace, frontier.generation()))
        .expect("read selected generation history")
        .expect("selected generation history row exists")
}

#[derive(Debug, Eq, PartialEq)]
struct LogicalSelectionIdentity {
    namespace: [u8; 16],
    generation: u64,
    closure: [u8; 32],
    target_root: [u8; 32],
    input_digest: [u8; 32],
    semantic_catalog_root: Option<[u8; 32]>,
}

#[derive(Debug, Eq, PartialEq)]
struct PlaneIdentity {
    kind: SemanticPlaneKind,
    root: SemanticPlaneRoot,
    segments: Vec<[u8; 32]>,
}

#[derive(Debug, Eq, PartialEq)]
struct ImageIdentity {
    image: SemanticPlaneImageKey,
    manifest_root: SemanticManifestRoot,
    semantic_generation: backend_semantic::ir::GenerationId,
    planes: Vec<PlaneIdentity>,
}

#[derive(Debug, PartialEq)]
struct JourneyObservation {
    selection: LogicalSelectionIdentity,
    catalog_root: backend_semantic::ir::SemanticPlaneCatalogRoot,
    images: Vec<ImageIdentity>,
    search: Value,
    document: Value,
    graph: Value,
}

fn logical_selection(selected: &SelectedGeneration) -> LogicalSelectionIdentity {
    LogicalSelectionIdentity {
        namespace: selected.namespace().namespace_id(),
        generation: selected.generation(),
        closure: *selected.closure_id(),
        target_root: *selected.target_root(),
        input_digest: *selected.input_digest(),
        semantic_catalog_root: selected.semantic_catalog_root().copied(),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn exercise_remote_s3_range_interrupt(
    locald: &mut Locald,
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority_secret: &Path,
    runtime: &tokio::runtime::Runtime,
    authority: &TursoAuthority,
    namespace: &AuthorityNamespace,
    selected: &SelectedGeneration,
    s3: &LoopbackS3,
    owner_peer: EndpointId,
    owner_address: std::net::SocketAddr,
    target: SemanticTargetKey,
) {
    assert!(
        locald.kill_now().is_some(),
        "owner did not stop before remote S3 range setup"
    );
    *locald = Locald::launch(endpoint, workspace, authority_secret, Some(s3), true);
    assert_eq!(
        selected_generation(runtime, authority, namespace),
        *selected,
        "owner restart for remote S3 range serving changed the selected generation"
    );
    let owner_pid = locald
        .child
        .as_ref()
        .expect("remote owner child is running")
        .id();
    let rss_sampler = ProcessRssSampler::start(owner_pid);

    let client_secret = SecretKey::generate();
    let client_secret_bytes = client_secret.to_bytes();
    let low_budget_capability = issue_semantic_capability(
        endpoint,
        workspace,
        project,
        client_secret.public(),
        &workspace.join("remote-semantic-low-budget.cap"),
        target.coordinate(),
        1,
        1024 * 1024,
    );
    let low_budget_grant_id = hex(&low_budget_capability.grant_id());
    let capability = issue_semantic_capability(
        endpoint,
        workspace,
        project,
        client_secret.public(),
        &workspace.join("remote-semantic-read.cap"),
        target.coordinate(),
        10_000,
        64 * 1024 * 1024,
    );
    let grant_id = hex(&capability.grant_id());
    let scope = capability
        .claims
        .semantic
        .as_ref()
        .expect("owner-issued capability has semantic-only scope");
    assert_eq!(scope.package, target.package());
    assert_eq!(scope.coordinate, target.coordinate());
    assert_eq!(scope.selected_root, *selected.target_root());
    assert_eq!(scope.closure_id, *selected.closure_id());
    assert_eq!(
        Some(&scope.catalog_root),
        selected.semantic_catalog_root(),
        "grant scope does not bind the selected semantic catalog root"
    );

    let mut quota_client = LocalSemanticIndexClient::connect_remote(
        SecretKey::from_bytes(&client_secret_bytes),
        owner_peer,
        owner_address,
        low_budget_capability.clone(),
        target.clone(),
    )
    .expect("connect remote client with a bounded quota grant");
    let quota_failure = match quota_client.fetch_selected_catalog() {
        Err(error) => error,
        Ok(snapshot) => {
            let image = snapshot
                .catalog()
                .entries()
                .first()
                .expect("selected remote catalog has one image")
                .image();
            quota_client
                .fetch_selected_manifest(image)
                .expect_err("one-request grant cannot fetch both catalog and manifest")
        }
    };
    assert!(
        format!("{quota_failure:?}").contains("ReplayOrBudget"),
        "owner did not return a typed durable request-budget refusal: {quota_failure:?}"
    );
    drop(quota_client);
    assert_eq!(
        selected_generation(runtime, authority, namespace),
        *selected,
        "request-budget refusal changed the selected native generation"
    );

    let mut client = LocalSemanticIndexClient::connect_remote(
        SecretKey::from_bytes(&client_secret_bytes),
        owner_peer,
        owner_address,
        capability.clone(),
        target.clone(),
    )
    .expect("connect remote selected semantic client");
    let snapshot = client
        .fetch_selected_catalog()
        .expect("admit remote selected semantic catalog");
    assert_eq!(
        snapshot.selected_stamp().selected_root(),
        selected.target_root(),
        "remote catalog stamp is not bound to the exact selected native root"
    );
    let image = snapshot
        .catalog()
        .entries()
        .first()
        .expect("selected remote catalog has one image")
        .image();
    let manifest = client
        .fetch_selected_manifest(image)
        .expect("admit remote selected semantic manifest");
    let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
    let plane = manifest.plane(kind).expect("selected core IR plane");
    let segment = plane
        .segments()
        .first()
        .expect("selected core IR has one segment");
    assert!(
        segment.byte_length() > 16 * 1024,
        "remote interruption fixture needs a segment larger than one bounded range; got {} bytes",
        segment.byte_length()
    );

    let stale = request_unselected_generation(
        &client_secret_bytes,
        owner_peer,
        owner_address,
        &capability,
        &target,
        snapshot.selected_stamp(),
        image,
        &manifest,
        kind,
        segment,
    );
    assert_eq!(
        stale,
        RemoteIndexOutcome::StaleSemanticSelection,
        "owner did not refuse a nonselected generation with its typed stale outcome"
    );
    assert_eq!(
        selected_generation(runtime, authority, namespace),
        *selected,
        "stale generation request changed the selected native generation"
    );

    let client_store_path = workspace.join("remote-client-range-cas");
    let client_workspace = workspace.join("remote-client-state");
    private_directory(&client_workspace);
    let client_store = FileStore::open(client_store_path, 512 * 1024 * 1024)
        .expect("open independent remote client semantic CAS");
    let limits = TransportLimits {
        max_chunk: 16 * 1024,
        ..TransportLimits::default()
    };
    let mut range_store =
        FileSemanticRangeStore::open(client_store, limits).expect("open remote client range CAS");
    let have = client
        .verified_local_segments(&manifest, image, kind, &mut range_store)
        .expect("check prior remote client segment coverage");
    assert!(
        have.is_empty(),
        "new remote client unexpectedly has selected bytes"
    );
    let mut cursor = client
        .new_cursor(&manifest, image, kind, &have, limits)
        .expect("bind remote cursor to selected semantic generation");
    let request = match client
        .next_request(&mut cursor, None, HydrationCredits::new(1, 16 * 1024))
        .expect("plan first bounded remote range")
    {
        IrHydrationPoll::Request(request) => request,
        other => panic!("remote selected segment did not request bytes: {other:?}"),
    };
    let range_gets_before = s3.stats().range_gets;
    let first_range_bytes = client
        .requested_bytes(&request)
        .expect("account exact first remote range length");
    assert!(first_range_bytes > 0 && first_range_bytes <= 16 * 1024);
    let first_progress = client
        .request_and_accept(&mut cursor, &request, &mut range_store, limits)
        .expect("fetch and verify first remote S3-backed range");
    let mut verified_range_bytes = first_range_bytes;
    let checkpoint = match first_progress {
        SemanticRangeClientProgress::Staged {
            coverage,
            checkpoint,
        } => {
            assert!(coverage.bytes().covered_bytes() > 0);
            assert!(coverage.bytes().covered_bytes() < segment.byte_length());
            checkpoint
        }
        SemanticRangeClientProgress::Complete(_) => {
            panic!("one bounded range unexpectedly completed a multi-range segment")
        }
    };
    assert_eq!(checkpoint.selected_stamp(), snapshot.selected_stamp());
    assert_eq!(checkpoint.image(), image);
    let checkpoint_path = client_workspace.join("semantic-range.checkpoint");
    std::fs::write(
        &checkpoint_path,
        checkpoint
            .encode()
            .expect("encode selected range checkpoint"),
    )
    .expect("persist interrupted range checkpoint");
    assert!(
        s3.stats().range_gets > range_gets_before,
        "the external client range did not reach the real S3 object store"
    );
    drop(client);

    assert!(
        locald.kill_now().is_some(),
        "owner did not stop before range reconnect"
    );
    *locald = Locald::launch(endpoint, workspace, authority_secret, Some(s3), true);
    rss_sampler.set_owner_pid(
        locald
            .child
            .as_ref()
            .expect("restarted owner child is running")
            .id(),
    );
    assert_eq!(
        selected_generation(runtime, authority, namespace),
        *selected,
        "cold owner restart changed the selected native generation during transfer"
    );

    let mut restarted_quota_client = LocalSemanticIndexClient::connect_remote(
        SecretKey::from_bytes(&client_secret_bytes),
        owner_peer,
        owner_address,
        low_budget_capability,
        target.clone(),
    )
    .expect("reconnect exhausted grant after owner restart");
    let restarted_quota_failure = restarted_quota_client
        .fetch_selected_catalog()
        .expect_err("cold owner must retain the exhausted request budget");
    assert!(
        format!("{restarted_quota_failure:?}").contains("ReplayOrBudget"),
        "cold owner forgot the exhausted request budget: {restarted_quota_failure:?}"
    );
    drop(restarted_quota_client);

    let mut resumed_client = LocalSemanticIndexClient::connect_remote(
        SecretKey::from_bytes(&client_secret_bytes),
        owner_peer,
        owner_address,
        capability,
        target,
    )
    .expect("reconnect external client after owner restart");
    let resumed_snapshot = resumed_client
        .fetch_selected_catalog()
        .expect("re-admit selected catalog after owner restart");
    assert_eq!(
        resumed_snapshot.selected_stamp(),
        checkpoint.selected_stamp(),
        "owner restart no longer serves the exact checkpoint selection"
    );
    let resumed_image = resumed_snapshot
        .catalog()
        .entries()
        .first()
        .expect("selected remote catalog still has its image")
        .image();
    assert_eq!(resumed_image, checkpoint.image());
    let resumed_manifest = resumed_client
        .fetch_selected_manifest(resumed_image)
        .expect("re-admit exact manifest after owner restart");
    let resumed_have = resumed_client
        .verified_local_segments(&resumed_manifest, resumed_image, kind, &mut range_store)
        .expect("verify client CAS after owner restart");
    assert!(
        resumed_have.is_empty(),
        "partial bytes were incorrectly admitted as a complete segment"
    );
    let checkpoint_bytes = std::fs::read(&checkpoint_path)
        .expect("read durable remote range checkpoint after owner restart");
    let checkpoint = SemanticRangeClientCheckpoint::decode(&checkpoint_bytes)
        .expect("decode exact selected range checkpoint after owner restart");
    let (mut resumed_cursor, coverage, mut poll) = resumed_client
        .resume_semantic_range(
            &checkpoint,
            &resumed_manifest,
            &resumed_have,
            limits,
            &mut range_store,
        )
        .expect("resume verified sparse range under the unchanged selected stamp");
    let mut partial = Some(coverage);
    let mut resumed_ranges = 0usize;
    let verified = loop {
        match poll {
            IrHydrationPoll::Request(request) => {
                resumed_ranges = resumed_ranges.saturating_add(1);
                let requested_bytes = resumed_client
                    .requested_bytes(&request)
                    .expect("account exact resumed range length");
                assert!(requested_bytes > 0 && requested_bytes <= 16 * 1024);
                verified_range_bytes = verified_range_bytes
                    .checked_add(requested_bytes)
                    .expect("remote range byte total fits its typed budget");
                match resumed_client
                    .request_and_accept(&mut resumed_cursor, &request, &mut range_store, limits)
                    .expect("fetch remaining bounded selected ranges after reconnect")
                {
                    SemanticRangeClientProgress::Staged {
                        coverage,
                        checkpoint,
                    } => {
                        partial = Some(coverage);
                        std::fs::write(
                            &checkpoint_path,
                            checkpoint
                                .encode()
                                .expect("encode resumed range checkpoint"),
                        )
                        .expect("persist resumed range checkpoint");
                        poll = resumed_client
                            .next_request(
                                &mut resumed_cursor,
                                partial.as_ref(),
                                HydrationCredits::new(1, 16 * 1024),
                            )
                            .expect("plan next bounded resumed range");
                    }
                    SemanticRangeClientProgress::Complete(segment) => {
                        let _ = std::fs::remove_file(&checkpoint_path);
                        break segment;
                    }
                }
            }
            IrHydrationPoll::VerifyLocal(request) => {
                break resumed_client
                    .verify_local_segment(&mut resumed_cursor, &request, &mut range_store)
                    .expect("admit completed local sparse range after restart");
            }
            IrHydrationPoll::NoCredits => {
                panic!("resumed selected range unexpectedly exhausted its bounded credits")
            }
            IrHydrationPoll::Exhausted => {
                panic!("resumed range cursor exhausted before admitting its segment")
            }
        }
    };
    assert!(
        resumed_ranges > 0,
        "reconnected client did not fetch remaining ranges"
    );
    assert!(
        verified_range_bytes <= 64 * 1024 * 1024,
        "verified remote range payload exceeds the high grant's response-byte budget"
    );
    assert_eq!(
        verified.id().as_bytes(),
        checkpoint.range_request().segment_id.as_bytes()
    );
    assert_eq!(verified.selection().stamp(), checkpoint.selected_stamp());
    assert_eq!(
        selected_generation(runtime, authority, namespace),
        *selected,
        "range resume or admission advanced the owner's selected native generation"
    );
    assert!(
        s3.stats().range_gets >= range_gets_before.saturating_add(2),
        "interrupted remote hydration did not issue actual S3 range requests before and after restart: {:?}",
        s3.stats()
    );
    let grants = cli_json(
        endpoint,
        workspace,
        project,
        &["cluster", "owner", "grant", "list"],
    );
    let grants = grants.as_array().expect("grant list is a JSON array");
    let high_usage = grants
        .iter()
        .find(|grant| grant["grantId"].as_str() == Some(grant_id.as_str()))
        .expect("high-budget semantic grant remains listed");
    let high_requests = high_usage["requests"]
        .as_u64()
        .expect("metered request count");
    let high_request_budget = high_usage["requestBudget"]
        .as_u64()
        .expect("signed request budget");
    let high_response_bytes = high_usage["responseBytes"]
        .as_u64()
        .expect("persisted response byte total");
    let high_byte_budget = high_usage["byteBudget"]
        .as_u64()
        .expect("signed byte budget");
    assert!(high_requests > 0 && high_requests <= high_request_budget);
    assert!(high_response_bytes > 0 && high_response_bytes <= high_byte_budget);
    let low_usage = grants
        .iter()
        .find(|grant| grant["grantId"].as_str() == Some(low_budget_grant_id.as_str()))
        .expect("exhausted low-budget grant remains listed");
    assert_eq!(low_usage["requests"].as_u64(), Some(1));
    let low_response_bytes = low_usage["responseBytes"]
        .as_u64()
        .expect("persisted quota-refusal response bytes");
    let low_byte_budget = low_usage["byteBudget"]
        .as_u64()
        .expect("signed low response-byte budget");
    assert!(
        low_response_bytes > 0 && low_response_bytes <= low_byte_budget,
        "quota refusal response was not durably byte-metered within its grant"
    );
    let (rss_samples, peak_combined_rss_kb) = rss_sampler.finish();
    assert!(
        rss_samples > 0,
        "could not sample owner/client RSS during transfer"
    );
    let rss_limit_kb = std::env::var("REMOTE_INDEX_RSS_LIMIT_KB")
        .map(|value| value.parse::<usize>().expect("valid RSS ceiling"))
        .unwrap_or(2 * 1024 * 1024);
    assert!(
        peak_combined_rss_kb <= rss_limit_kb,
        "combined owner/client RSS {peak_combined_rss_kb} KiB exceeded ceiling {rss_limit_kb} KiB"
    );
    eprintln!(
        "remote S3 range journey sampled combined owner/client RSS: {peak_combined_rss_kb} KiB (ceiling {rss_limit_kb} KiB)"
    );
    assert!(
        locald.running(),
        "owner exited during remote semantic range resume: {}",
        locald.diagnostics()
    );
}

#[allow(clippy::too_many_arguments)]
fn request_unselected_generation(
    client_secret: &[u8; 32],
    owner_peer: EndpointId,
    owner_address: std::net::SocketAddr,
    capability: &RemoteIndexCapability,
    target: &SemanticTargetKey,
    selected_stamp: backend_replication::SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    manifest: &backend_semantic::ir::SemanticPlaneManifest,
    kind: SemanticPlaneKind,
    segment: &backend_semantic::ir::SemanticPlaneSegment,
) -> RemoteIndexOutcome {
    let bad_image = SemanticPlaneImageKey::new(
        image.artifact_ordinal(),
        GenerationId::from_raw([0xe1; 32]),
        image.manifest_root(),
    );
    let get = SemanticRangeGet {
        request_id: 1,
        target: target.clone(),
        selected_stamp,
        image: bad_image,
        range_request: SemanticRangeRequest {
            manifest_root: manifest.root(),
            plane: kind,
            segment_id: segment.id_claim(),
            first_key: *segment.first_key(),
            last_key: *segment.last_key(),
            byte_length: segment.byte_length(),
        },
        byte_range: ByteRange::new(0, 16 * 1024).expect("bounded stale-generation range"),
    };
    let payload = get
        .encode()
        .expect("encode canonical stale-generation request");
    let local_request = LocalControlRequest::SemanticRangeGet {
        request_id: 1,
        payload: payload.into_boxed_slice(),
    };
    let body = encode_request(&local_request, LocalControlLimits::default())
        .expect("encode canonical semantic owner request");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build remote stale-generation runtime");
    let endpoint = runtime
        .block_on(bind_direct(
            SecretKey::from_bytes(client_secret),
            "0.0.0.0:0".parse().expect("IPv4 client bind address"),
        ))
        .expect("bind authenticated direct remote client endpoint");
    let hello =
        RemoteIndexSessionHello::new(capability.clone(), RemoteIndexChannel::SemanticHydration)
            .expect("create signed semantic-hydration session hello");
    let mut session = runtime
        .block_on(connect_remote_index(
            &endpoint,
            EndpointAddr::new(owner_peer).with_ip_addr(owner_address),
            hello,
        ))
        .expect("connect authenticated Iroh semantic channel");
    let request = RemoteIndexRequest {
        request_id: 1,
        body: body.into_boxed_slice(),
    };
    let response = runtime
        .block_on(async {
            session.send_request(&request).await?;
            session.receive_response(request.request_id).await
        })
        .expect("receive correlated stale-generation owner result");
    drop(session);
    runtime.block_on(endpoint.close());
    response.outcome
}

fn run_storage_case(
    root: &Path,
    case_name: &str,
    project: &Path,
    authority_secret: &Path,
    s3: Option<&LoopbackS3>,
) -> JourneyObservation {
    let workspace = root.join(case_name);
    private_directory(&workspace);
    let project_label = project.to_string_lossy().into_owned();
    let coordinate = semantic_coordinate(&project_label);
    let namespace = semantic_namespace(&project_label, &coordinate);
    let endpoint = backend_runtime::derive_endpoint(&workspace);
    let authority_path = workspace.join(backend_extension_turso::AUTHORITY_FILE_NAME);
    let remote_owner = s3.map(|_| init_remote_owner(&endpoint, &workspace, project));

    let mut locald = Locald::launch(&endpoint, &workspace, authority_secret, s3, false);
    let mut add = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    add.args([
        OsStr::new("--endpoint"),
        endpoint.as_os_str(),
        OsStr::new("--workspace"),
        workspace.as_os_str(),
        OsStr::new("--project"),
        project.as_os_str(),
        OsStr::new("--format"),
        OsStr::new("json"),
        OsStr::new("add"),
        project.as_os_str(),
    ]);
    clear_product_environment(&mut add);
    add.env("NO_COLOR", "1");
    let add_output = run(
        &mut add,
        if s3.is_some() {
            "S3-backed local compiler Add"
        } else {
            "FileStore-only local compiler Add"
        },
    );
    let add_reply: Value = serde_json::from_slice(&add_output.stdout).expect("parse Add reply");
    assert_eq!(add_reply["answer"], "product");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build selected-head observer runtime");
    let authority = runtime
        .block_on(TursoAuthority::open(&authority_path))
        .expect("open selected Turso authority");
    let selected = selected_generation(&runtime, &authority, &namespace);
    let selected_before_restart = logical_selection(&selected);
    assert_ne!(selected_before_restart.closure, [0; 32]);

    let local_store = FileStore::open(workspace.join("semantic-objects"), 512 * 1024 * 1024)
        .expect("open selected FileStore");
    let reopened = reopen_selected_compiler_metadata(&local_store, &selected)
        .expect("reopen selected compiler metadata before cold restart");
    let plane_metadata = reopened
        .metadata()
        .versioned_planes()
        .expect("selected compiler metadata includes versioned planes");
    let mut selected_members = plane_metadata
        .artifacts()
        .iter()
        .flat_map(|artifact| artifact.members())
        .map(|member| *member.object_id())
        .collect::<Vec<_>>();
    selected_members.sort_unstable();
    selected_members.dedup();
    assert!(
        !selected_members.is_empty(),
        "selected generation has no plane segments"
    );
    let inspection_budget = ArtifactBudget::new(1, 1, 512 * 1024 * 1024, 16 * 1024, 1);
    let local_sink = local_store.artifact_sink(inspection_budget);
    let local_pin = local_store
        .pin_garbage_collection()
        .expect("pin selected members during admission checks");
    for member in &selected_members {
        let reader = local_sink
            .open_object(UntrustedObjectId::from_bytes(*member))
            .expect("verify local selected member before cold restart")
            .expect("selected closure member was not local before publication");
        assert_eq!(
            reader.id().as_bytes(),
            member,
            "selected member admission changed its exact identity"
        );
    }
    drop(local_pin);

    if let Some(s3) = s3 {
        let receipt_root = workspace.join("remote-s3-receipts");
        assert!(
            std::fs::read_dir(&receipt_root)
                .expect("read owner durable receipt directory")
                .filter_map(Result::ok)
                .any(|entry| entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "receipt")),
            "owner did not persist an exact closure receipt for the selected Turso generation"
        );
        assert!(
            s3.stats().puts > 0,
            "owner did not issue a conditional S3 PUT"
        );
        assert!(
            s3.stored_object().is_some(),
            "loopback S3 has no uploaded pack"
        );
    } else {
        assert!(
            !workspace.join("remote-s3-receipts").exists(),
            "FileStore-only publication unexpectedly created S3 receipts"
        );
    }

    let mut locally_retained = Vec::with_capacity(2 + reopened.metadata().images().len());
    locally_retained.push(*selected.candidate_id());
    locally_retained.push(*reopened.envelope().metadata_id());
    locally_retained.extend(
        reopened
            .metadata()
            .images()
            .iter()
            .map(|image| *image.object_id()),
    );

    locald.kill_now();
    let mut cold_locald = Locald::launch(&endpoint, &workspace, authority_secret, s3, s3.is_some());
    assert!(
        cold_locald.running(),
        "cold locald exited while reconciling selected storage: {}",
        cold_locald.diagnostics()
    );
    let selected_after_restart = selected_generation(&runtime, &authority, &namespace);
    assert_eq!(
        selected_after_restart, selected,
        "cold restart changed the exact selected Turso history record"
    );

    let cold_store = FileStore::open(workspace.join("semantic-objects"), 512 * 1024 * 1024)
        .expect("reopen FileStore after cold restart");
    let cold_sink = cold_store.artifact_sink(inspection_budget);
    let cold_pin = cold_store
        .pin_garbage_collection()
        .expect("pin selected members during cold admission checks");
    let mut locally_present_members = 0usize;
    let mut remotely_resident_members = 0usize;
    for member in &selected_members {
        let reader = cold_sink
            .open_object(UntrustedObjectId::from_bytes(*member))
            .expect("inspect and verify selected closure member after restart");
        let present = reader.is_some();
        assert_eq!(
            present,
            s3.is_none(),
            "selected plane segment residency did not match the configured storage route"
        );
        if let Some(reader) = reader {
            assert_eq!(
                reader.id().as_bytes(),
                member,
                "selected member admission changed its exact identity"
            );
            locally_present_members += 1;
        } else {
            remotely_resident_members += 1;
        }
    }
    assert_eq!(
        locally_present_members,
        if s3.is_none() {
            selected_members.len()
        } else {
            0
        },
        "cold restart retained the wrong number of selected plane members locally"
    );
    assert_eq!(
        remotely_resident_members,
        if s3.is_some() {
            selected_members.len()
        } else {
            0
        },
        "cold restart left the wrong number of selected plane members remote"
    );
    for member in &locally_retained {
        let reader = cold_sink
            .open_object(UntrustedObjectId::from_bytes(*member))
            .expect("verify locally retained metadata/image after restart")
            .expect("selected envelope, compiler metadata, or semantic image was evicted");
        assert_eq!(
            reader.id().as_bytes(),
            member,
            "retained object admission changed its exact identity"
        );
    }
    drop(cold_pin);

    // These ordinary product queries run through the cold process before any range hydration.
    let search = cli_json(
        &endpoint,
        &workspace,
        project,
        &["search", "cold_s3_entry", "--limit", "20"],
    );
    let entry = search["records"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|record| record["identity"]["name"] == "cold_s3_entry")
        .unwrap_or_else(|| panic!("cold selected query omitted package declaration: {search}"));
    let entry_coordinate = entry["identity"]["coordinate"]
        .as_str()
        .expect("selected entry coordinate")
        .to_owned();
    let document = cli_json(&endpoint, &workspace, project, &["show", &entry_coordinate]);
    let graph = cli_json(
        &endpoint,
        &workspace,
        project,
        &["graph", &entry_coordinate],
    );
    assert!(document.to_string().contains("cold-s3-project"));
    assert!(graph.to_string().contains("cold_s3_helper"));

    let target = SemanticTargetKey::new(
        project_label.clone(),
        coordinate.clone(),
        LanguageProfile::Rust(RustEdition::Rust2024),
    )
    .expect("canonical selected target");
    let mut client = LocalSemanticIndexClient::connect(&endpoint, target)
        .expect("connect normal semantic index client after restart");
    let catalog = client
        .fetch_selected_catalog()
        .expect("read selected semantic catalog after restart");
    assert!(
        !catalog.catalog().entries().is_empty(),
        "selected catalog is empty"
    );
    assert_eq!(
        selected_after_restart.semantic_catalog_root(),
        Some(catalog.catalog().root().as_bytes()),
        "selected Turso root differs from the client-admitted catalog root"
    );
    let catalog_root = catalog.catalog().root();
    let mut images = Vec::new();
    let mut manifests = Vec::new();
    for entry in catalog.catalog().entries() {
        let image = entry.image();
        let manifest = client
            .fetch_selected_manifest(image)
            .unwrap_or_else(|error| panic!("fetch selected image {image:?}: {error}"));
        assert_eq!(manifest.root(), image.manifest_root());
        let planes = manifest
            .planes()
            .iter()
            .map(|plane| PlaneIdentity {
                kind: plane.kind(),
                root: plane.root(),
                segments: plane
                    .segments()
                    .iter()
                    .map(|segment| *segment.id_claim().as_bytes())
                    .collect(),
            })
            .collect::<Vec<_>>();
        images.push(ImageIdentity {
            image,
            manifest_root: manifest.root(),
            semantic_generation: manifest.semantic_generation(),
            planes,
        });
        manifests.push((image, manifest));
    }
    assert!(
        images.iter().any(|image| image.planes.iter().any(|plane| {
            plane.kind == SemanticPlaneKind::Ir(SemanticIrPlane::Core) && !plane.segments.is_empty()
        })),
        "selected images have no core IR segment"
    );

    let limits = TransportLimits {
        max_chunk: 16 * 1024,
        ..TransportLimits::default()
    };
    let mut range_store =
        FileSemanticRangeStore::open(cold_store, limits).expect("open durable range receiver");
    let range_gets_before = s3.map_or(0, |s3| s3.stats().range_gets);
    let mut core_was_verified = false;
    for (image, manifest) in &manifests {
        for plane in manifest.planes() {
            if !plane.coverage().state().is_complete() || plane.segments().is_empty() {
                continue;
            }
            let have = client
                .verified_local_segments(manifest, *image, plane.kind(), &mut range_store)
                .expect("read and verify local selected segments");
            let is_core = plane.kind() == SemanticPlaneKind::Ir(SemanticIrPlane::Core);
            core_was_verified |= is_core;
            if s3.is_some() {
                assert!(
                    have.is_empty(),
                    "S3-selected segment was still local after cold collection"
                );
                let hydrated = hydrate_one_segment(
                    &mut client,
                    manifest,
                    *image,
                    plane.kind(),
                    &mut range_store,
                    limits,
                );
                assert_eq!(hydrated.selection().kind(), plane.kind());
            } else {
                let expected = plane
                    .segments()
                    .iter()
                    .map(|segment| *segment.id_claim().as_bytes())
                    .collect::<Vec<_>>();
                let verified = have
                    .iter()
                    .map(|segment| *segment.as_bytes())
                    .collect::<Vec<_>>();
                assert_eq!(
                    verified, expected,
                    "FileStore-only selected plane did not verify its complete local closure"
                );
            }
        }
    }
    assert!(core_was_verified, "selected core IR plane was not verified");
    if let Some(s3) = s3 {
        assert!(
            s3.stats().range_gets >= range_gets_before.saturating_add(3),
            "selected cold plane hydration did not use real S3 range GETs: {:?}",
            s3.stats()
        );
        let (owner_peer, owner_address) =
            remote_owner.expect("S3-backed remote journey initialized the Iroh owner");
        exercise_remote_s3_range_interrupt(
            &mut cold_locald,
            &endpoint,
            &workspace,
            project,
            authority_secret,
            &runtime,
            &authority,
            &namespace,
            &selected_after_restart,
            s3,
            owner_peer,
            owner_address,
            target,
        );
    }

    drop(cold_locald);
    JourneyObservation {
        selection: logical_selection(&selected_after_restart),
        catalog_root,
        images,
        search,
        document,
        graph,
    }
}

#[test]
fn cold_selected_closure_matches_between_filestore_and_real_s3_processes() {
    let root = Fixture::new();
    let project = root.path().join("project");
    private_directory(&project.join("src"));
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"cold-s3-project\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .expect("write source package manifest");
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"cold-s3-project\"\nversion = \"1.0.0\"\n",
    )
    .expect("write fixed source lockfile");
    let mut source = String::from(
        "#[doc = include_str!(\"../Cargo.toml\")]\npub fn cold_s3_helper() -> u32 { 41 }\npub fn cold_s3_entry() -> u32 { cold_s3_helper() + 1 }\n",
    );
    for index in 0..512_u32 {
        source.push_str(&format!(
            "pub fn cold_s3_filler_{index:04}() -> u64 {{ {index} }}\n"
        ));
    }
    std::fs::write(project.join("src/lib.rs"), source).expect("write source file");
    let project = project.canonicalize().expect("canonical source project");
    let input_paths = ["Cargo.toml", "Cargo.lock", "src/lib.rs"];
    let source_inputs = input_paths
        .map(|relative| std::fs::read(project.join(relative)).expect("read fixed project input"));
    let authority_secret = root.path().join("authority.secret");
    write_authority_secret(&authority_secret);
    let s3 = LoopbackS3::start().expect("start real loopback S3 HTTP server");

    let file_store = run_storage_case(
        root.path(),
        "owner-filestore-only",
        &project,
        &authority_secret,
        None,
    );
    assert_eq!(
        s3.stats().requests,
        0,
        "FileStore-only process unexpectedly contacted loopback S3"
    );
    for (index, (relative, expected)) in input_paths.iter().zip(&source_inputs).enumerate() {
        assert_eq!(
            std::fs::read(project.join(relative))
                .expect("re-read fixed project input")
                .as_slice(),
            expected.as_slice(),
            "FileStore-only run modified shared input {index} ({relative})"
        );
    }

    let s3_store = run_storage_case(
        root.path(),
        "owner-s3",
        &project,
        &authority_secret,
        Some(&s3),
    );
    for (index, (relative, expected)) in input_paths.iter().zip(&source_inputs).enumerate() {
        assert_eq!(
            std::fs::read(project.join(relative))
                .expect("re-read fixed project input")
                .as_slice(),
            expected.as_slice(),
            "S3 run modified shared input {index} ({relative})"
        );
    }

    assert_eq!(
        file_store.selection, s3_store.selection,
        "FileStore-only and S3 selected different logical closure roots"
    );
    assert_eq!(
        file_store.catalog_root, s3_store.catalog_root,
        "FileStore-only and S3 selected different semantic catalog roots"
    );
    assert_eq!(
        file_store.images, s3_store.images,
        "FileStore-only and S3 exposed different selected IR/embedding plane identities"
    );
    assert_eq!(
        file_store.search, s3_store.search,
        "cold CLI search response changed with the selected storage route"
    );
    assert_eq!(
        file_store.document, s3_store.document,
        "cold CLI document response changed with the selected storage route"
    );
    assert_eq!(
        file_store.graph, s3_store.graph,
        "cold CLI graph response changed with the selected storage route"
    );
    assert!(
        s3.stats().puts > 0,
        "paired S3 route never uploaded a real pack"
    );
    assert!(
        s3.stats().range_gets >= 3,
        "paired S3 route never fetched selected closure bytes with range GETs: {:?}",
        s3.stats()
    );
}

fn hydrate_one_segment(
    client: &mut LocalSemanticIndexClient,
    manifest: &backend_semantic::ir::SemanticPlaneManifest,
    image: backend_semantic::ir::SemanticPlaneImageKey,
    kind: SemanticPlaneKind,
    store: &mut FileSemanticRangeStore,
    limits: TransportLimits,
) -> backend_replication::VerifiedSemanticSegment {
    let have = client
        .verified_local_segments(manifest, image, kind, store)
        .expect("read and verify local selected segments");
    assert!(
        have.is_empty(),
        "selected remote segment remained in local CAS"
    );
    let mut cursor = client
        .new_cursor(manifest, image, kind, &have, limits)
        .expect("bind range cursor to current selected plane");
    let mut partial = None;
    loop {
        let request = match client
            .next_request(
                &mut cursor,
                partial.as_ref(),
                HydrationCredits::new(1, 16 * 1024),
            )
            .expect("plan exact selected range")
        {
            IrHydrationPoll::Request(request) => request,
            IrHydrationPoll::VerifyLocal(_) => {
                panic!("remote segment unexpectedly has complete local sparse coverage")
            }
            other => panic!("selected segment did not produce a remote request: {other:?}"),
        };
        match client
            .request_and_accept(&mut cursor, &request, store, limits)
            .expect("fetch and admit exact selected S3 range")
        {
            SemanticRangeClientProgress::Staged { coverage, .. } => partial = Some(coverage),
            SemanticRangeClientProgress::Complete(segment) => return segment,
        }
    }
}
