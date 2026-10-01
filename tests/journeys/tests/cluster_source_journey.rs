//! Source-only process journey through the production local owner, Iroh worker, and Turso head.
//!
//! The test provisions both peers with the shipped commands, compiles one Rust source file whose
//! docs read the otherwise hidden Cargo manifest, and verifies that a remote worker's real
//! result is selected by Turso. After local calibration, a Background Add changes the hidden
//! manifest and same-sized negative frontier. Journey-only fsync barriers cut after the worker's
//! `.pending` record before ResultReceipt, after recovered Pending status before grant pages, after
//! a live ResultReceipt before grant pages, before Turso selection, and at owner Stored intent and
//! worker retirement. Cold recovery, valid-checksum tamper rejection, and final Applied are checked
//! against both the v4 owner journal and Turso's independently selected head. The final remote
//! generation is then hydrated by a separate same-user client over the local range protocol,
//! including a dropped reply, cold restart, durable resume, and stale-stamp rejection.

#![cfg(any(target_os = "linux", target_os = "macos"))]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![allow(clippy::too_many_lines)]

use backend_client::LocalSemanticIndexClient;
use backend_engine::cluster_transport::{ClusterExecutionClass, ScopedClusterInvite};
use backend_engine::package_key;
use backend_extension_turso::{
    AuthorityNamespace, SelectedGeneration, SelectionOrigin, TursoAuthority,
};
use backend_replication::{
    AuthenticatedLocalPeer, ByteRange, FileSemanticRangeStore, HydrationCredits, IrHydrationPoll,
    LocalControlClient, LocalControlLimits, LocalControlRequest, LocalControlResponse, LocalStream,
    SelectedGenerationStamp, SemanticRangeClientCheckpoint, SemanticRangeClientProgress,
    SemanticRangeGet, SemanticTargetKey, TransportLimits,
};
use backend_semantic::ir::{SemanticIrPlane, SemanticPlaneKind};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
use backend_store::{ArtifactClosureClaim, FileStore};
use backend_store_s3::test_support::LoopbackS3;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::Write;
use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, UdpSocket};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(10);
const TRUSTED_RESULT_ACK: &str = "result owner ACK: Stored";
const PENDING_ACK_PATH: &str = "compiler-pending-stored-acks/pending-stored-acks.v4";
const PENDING_ACK_MAGIC: &[u8; 8] = b"BKPSACK4";
const PENDING_ACK_HEADER_BYTES: usize = 16;
const PENDING_ACK_CHECKSUM_BYTES: usize = 32;
const PENDING_ACK_STATE_AWAITING_SELECTION: u8 = 0;
const PENDING_ACK_STATE_STORED_PENDING: u8 = 1;
const PENDING_ACK_STATE_STORED_AWAITING_CONFIRM: u8 = 3;
const PENDING_ACK_RECOVERED_PENDING_BEFORE_GRANT_PAGES: u8 = 6;
const PENDING_ACK_LIVE_RECEIPT_BEFORE_GRANT_PAGES: u8 = 7;
const CARGO_MANIFEST_BEFORE: &str = "# REMOTE_CONFIG_BEFORE\n[package]\nname = \"cluster-source-project\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n";
const CARGO_MANIFEST_AFTER: &str = "# REMOTE_CONFIG_AFTER!\n[package]\nname = \"cluster-source-project\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n";
const CARGO_MANIFEST_LIVE: &str = "# REMOTE_CONFIG_LIVE!!\n[package]\nname = \"cluster-source-project\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n";
const INVENTORY_PATH_BEFORE: &str = "compiler.absent__";
const INVENTORY_PATH_AFTER: &str = "compiler.optional";
const INVENTORY_CONTENT: &str = "appeared after cold indexing\n";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct FixtureRoot(PathBuf);

impl FixtureRoot {
    fn new() -> Self {
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "backend-cluster-source-journey-{}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        create_private_directory(&path);
        Self(path.canonicalize().expect("canonical journey root"))
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

fn create_private_directory(path: &Path) {
    std::fs::create_dir_all(path)
        .unwrap_or_else(|error| panic!("create private directory {}: {error}", path.display()));
    let mut permissions = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("protect directory {}: {error}", path.display()));
}

fn write_authority_secret(path: &Path) {
    std::fs::write(path, [0x5a_u8; 32]).expect("write locald authority secret");
    let mut permissions = std::fs::metadata(path)
        .expect("stat locald authority secret")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).expect("protect locald authority secret");
}

fn available_rustc() -> PathBuf {
    if let Some(path) = std::env::var_os("NUDOX_RUSTC").map(PathBuf::from)
        && path.is_file()
    {
        return path.canonicalize().expect("canonical NUDOX_RUSTC");
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
        .unwrap_or_else(|| panic!("the real Rust compiler is required for this process journey"))
}

fn assert_cargo_process_headroom() {
    let output = Command::new("pgrep")
        .args(["-x", "cargo"])
        .output()
        .expect("inspect machine-wide Cargo process count before cluster journey");
    let active = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    assert!(
        output.status.success() || active == 0,
        "pgrep could not inspect machine-wide Cargo process count: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        active <= 1,
        "cluster compiler journey requires an exclusive Cargo slot: saw {active} active Cargo processes before starting"
    );
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

fn fixed_hex<const N: usize>(value: &str) -> [u8; N] {
    assert_eq!(value.len(), N * 2, "unexpected fixed-width hex length");
    std::array::from_fn(|index| {
        u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .expect("valid fixed-width hexadecimal byte")
    })
}

fn allocate_udp_loopback() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("allocate direct Iroh socket");
    let address = socket.local_addr().expect("read allocated Iroh socket");
    drop(socket);
    address
}

fn clean_product_environment(command: &mut Command) {
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

fn compile_ack_pause_interposer(directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("support")
        .join("pending_ack_pause.c");
    let library = directory.join(if cfg!(target_os = "macos") {
        "libbackend_journey_ack_pause.dylib"
    } else {
        "libbackend_journey_ack_pause.so"
    });
    let compiler = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let mut command = Command::new(compiler);
    clean_product_environment(&mut command);
    command.arg("-fPIC");
    if cfg!(target_os = "macos") {
        command.arg("-dynamiclib");
    } else {
        command.arg("-shared");
    }
    command.arg(&source).arg("-o").arg(&library);
    if cfg!(target_os = "linux") {
        command.args(["-ldl", "-pthread"]);
    }
    let output = run_bounded(command, "journey Stored-ACK barrier compilation", DEADLINE);
    assert_success(&output, "journey Stored-ACK barrier compilation");
    library
}

#[derive(Clone)]
struct CompilerProcessEnvironment {
    rustc: PathBuf,
    cargo: PathBuf,
    cargo_home: PathBuf,
    cargo_root: PathBuf,
    s3_endpoint: String,
    profile: String,
    pause_state1_marker: PathBuf,
    pause_state0_marker: PathBuf,
    pause_state3_marker: PathBuf,
    pause_offer_pending_marker: PathBuf,
    recovered_pending_marker: PathBuf,
    live_receipt_marker: PathBuf,
    pause_library: PathBuf,
}

impl CompilerProcessEnvironment {
    /// Applies the same compiler authority inputs and inert journey barriers to every process.
    fn apply(&self, command: &mut Command) {
        clean_product_environment(command);
        command
            .env("NUDOX_RUSTC", &self.rustc)
            .env("NUDOX_CARGO", &self.cargo)
            .env("NUDOX_CARGO_HOME", &self.cargo_home)
            .env("NUDOX_CARGO_ROOT", &self.cargo_root)
            .env("CARGO_BUILD_JOBS", "1")
            .env("BACKEND_JOURNEY_COMPILER_LANES", "1")
            .env(
                "BACKEND_JOURNEY_ACK_PAUSE_STATE0_MARKER",
                &self.pause_state0_marker,
            )
            .env(
                "BACKEND_JOURNEY_ACK_PAUSE_STATE1_MARKER",
                &self.pause_state1_marker,
            )
            .env(
                "BACKEND_JOURNEY_ACK_PAUSE_STATE3_MARKER",
                &self.pause_state3_marker,
            )
            .env(
                "BACKEND_JOURNEY_OFFER_PENDING_MARKER",
                &self.pause_offer_pending_marker,
            );
        command
            .env(
                "BACKEND_JOURNEY_RECOVERED_PENDING_MARKER",
                &self.recovered_pending_marker,
            )
            .env(
                "BACKEND_JOURNEY_LIVE_RECEIPT_MARKER",
                &self.live_receipt_marker,
            );
        if cfg!(target_os = "macos") {
            command.env("DYLD_INSERT_LIBRARIES", &self.pause_library);
        } else {
            command.env("LD_PRELOAD", &self.pause_library);
        }
    }

    /// Adds test storage credentials only to the owner process; compiler workers never receive
    /// index-side S3 secrets.
    fn apply_owner_storage(&self, command: &mut Command) {
        self.apply(command);
        command
            .env("BACKEND_S3_ENDPOINT", &self.s3_endpoint)
            .env("BACKEND_S3_BUCKET", "cluster-journey-bucket")
            .env("BACKEND_S3_REGION", "us-east-1")
            .env("BACKEND_S3_ACCESS_KEY_ID", "cluster-journey-access")
            .env("BACKEND_S3_SECRET_ACCESS_KEY", "cluster-journey-secret")
            .env("BACKEND_S3_PREFIX", "cluster-journey/");
    }
}

#[derive(Debug)]
struct Locald {
    child: Option<Child>,
    stderr: Arc<Mutex<Vec<String>>>,
    stderr_reader: Option<thread::JoinHandle<()>>,
    endpoint: PathBuf,
}

impl Locald {
    fn launch(
        endpoint: &Path,
        workspace: &Path,
        authority_secret: &Path,
        environment: &CompilerProcessEnvironment,
    ) -> Self {
        Self::launch_with_remote_segment_gc(
            endpoint,
            workspace,
            authority_secret,
            environment,
            false,
        )
    }

    fn launch_with_remote_segment_gc(
        endpoint: &Path,
        workspace: &Path,
        authority_secret: &Path,
        environment: &CompilerProcessEnvironment,
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
        environment.apply_owner_storage(&mut command);
        if collect_remote_segments {
            command.env("BACKEND_JOURNEY_REMOTE_SEGMENT_GC", "1");
        }
        let mut child = command.spawn().expect("spawn production locald");
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_reader = child.stderr.take().map(|stream| {
            let captured = Arc::clone(&stderr);
            thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(line);
                }
            })
        });
        let mut locald = Self {
            child: Some(child),
            stderr,
            stderr_reader,
            endpoint: endpoint.to_path_buf(),
        };
        wait_for_socket(endpoint, &mut locald);
        locald
    }

    fn rejected_gc_startup(
        endpoint: &Path,
        workspace: &Path,
        authority_secret: &Path,
        environment: &CompilerProcessEnvironment,
    ) -> String {
        assert!(
            !endpoint.exists(),
            "cannot start a rejected locald while its socket path exists"
        );
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
        environment.apply_owner_storage(&mut command);
        command.env("BACKEND_JOURNEY_REMOTE_SEGMENT_GC", "1");
        let mut child = command.spawn().expect("spawn GC rejection locald");
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll rejected GC startup") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("locald remained available with an absent pending closure claim");
            }
            thread::sleep(POLL);
        };
        let output = child
            .wait_with_output()
            .expect("collect rejected GC startup diagnostics");
        assert!(
            !status.success(),
            "GC startup accepted an absent closure claim"
        );
        assert_eq!(output.status, status);
        assert!(
            !endpoint.exists(),
            "locald published its socket after GC rejected pending roots"
        );
        let diagnostics = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            diagnostics.contains("collect semantic artifact CAS")
                && diagnostics.contains("Corrupt"),
            "locald did not fail at pending-root resolution before GC: {diagnostics}"
        );
        diagnostics
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll locald"))
            .is_none()
    }

    fn kill_now(&mut self) -> Option<ExitStatus> {
        let child = self.child.as_mut()?;
        if child
            .try_wait()
            .expect("poll locald before SIGKILL")
            .is_none()
        {
            child.kill().expect("SIGKILL locald process");
        }
        let status = child.wait().expect("wait for killed locald");
        let _ = std::fs::remove_file(&self.endpoint);
        Some(status)
    }

    fn release_ack_barrier(&mut self) {
        assert!(self.running(), "locald exited before ACK barrier release");
        let child = self.child.as_ref().expect("locald child process");
        let pid = child.id().to_string();
        let output = Command::new("kill")
            .args(["-USR1", pid.as_str()])
            .output()
            .expect("signal locald ACK barrier");
        assert_success(&output, "release state3 Stored-ACK barrier");
    }

    fn diagnostics(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .join("\n")
    }

    fn diagnostics_after_exit(&mut self) -> String {
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        self.diagnostics()
    }

    fn assert_no_local_fallback(&self) {
        let diagnostics = self.diagnostics();
        assert!(
            !diagnostics.contains("locald compiler route fallback"),
            "configured compiler work fell back to local execution: {diagnostics}"
        );
    }

    fn local_fallback_count(&self) -> usize {
        self.diagnostics()
            .lines()
            .filter(|line| line.contains("locald compiler route fallback"))
            .count()
    }

    fn wait_for_local_fallback_count(&self, required: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.local_fallback_count() >= required {
                return true;
            }
            thread::sleep(POLL);
        }
        self.local_fallback_count() >= required
    }
}

impl Drop for Locald {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child
                .try_wait()
                .expect("poll locald during cleanup")
                .is_none()
            {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

fn wait_for_socket(path: &Path, locald: &mut Locald) {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if !locald.running() {
            panic!(
                "locald exited during startup: {}",
                locald.diagnostics_after_exit()
            );
        }
        if std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.file_type().is_socket() && metadata.permissions().mode() & 0o777 == 0o600
        }) && UnixStream::connect(path).is_ok()
        {
            return;
        }
        thread::sleep(POLL);
    }
    panic!(
        "timed out waiting for locald {}: {}",
        path.display(),
        locald.diagnostics()
    );
}

fn persist_client_checkpoint(path: &Path, bytes: &[u8]) {
    let parent = path.parent().expect("client checkpoint parent");
    std::fs::create_dir_all(parent).expect("create client checkpoint directory");
    let temporary = path.with_extension("checkpoint.tmp");
    let mut file = std::fs::File::create(&temporary).expect("create checkpoint temp file");
    file.write_all(bytes)
        .expect("write encoded client checkpoint");
    file.sync_all().expect("fsync encoded client checkpoint");
    std::fs::rename(&temporary, path).expect("atomically replace client checkpoint");
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .expect("fsync client checkpoint directory");
}

/// Pulls the exact remote-compiler selection into a separate local CAS, cutting the real
/// authenticated local range stream after one durable page and restarting the owner daemon while
/// recreating the client and storage handles before the remaining ranges. The first range is also
/// replayed with an old frontier stamp to prove that sparse coverage only changes after
/// current-selection admission.
fn hydrate_remote_selected_core_with_restart(
    root: &FixtureRoot,
    endpoint: &Path,
    workspace: &Path,
    authority_secret: &Path,
    environment: &CompilerProcessEnvironment,
    project_label: &str,
    coordinate: &str,
    selected: &SelectedGeneration,
    daemon: &mut Locald,
    s3: &LoopbackS3,
) {
    // The owner was already running with remote-segment collection enabled. Reopen after the
    // final compiler selection so its local cache is cold for that exact generation; the local
    // range path must remain independent of whether the immutable segment is resident or in S3.
    eprintln!("journey client phase: cold owner restart begin");
    daemon.kill_now();
    *daemon = Locald::launch_with_remote_segment_gc(
        endpoint,
        workspace,
        authority_secret,
        environment,
        true,
    );
    eprintln!("journey client phase: cold owner restart complete");
    assert!(daemon.running(), "cold owner exited before local hydration");

    let target = SemanticTargetKey::new(
        project_label.to_owned(),
        coordinate.to_owned(),
        LanguageProfile::Rust(RustEdition::Rust2024),
    )
    .expect("canonical remote compiler semantic target");
    let client_store_path = root.path().join("local-client-semantic-cas");
    create_private_directory(&client_store_path);
    let checkpoint_path = root.path().join("local-client-core.checkpoint");
    let limits = TransportLimits {
        max_chunk: 16 * 1024,
        ..TransportLimits::default()
    };
    let core = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
    let s3_ranges_before = s3.stats().range_gets;
    let (
        image,
        manifest_root,
        full_image_bytes,
        full_image_pages,
        first_range_bytes,
        lost_response_bytes,
    ) = {
        let mut client = LocalSemanticIndexClient::connect(endpoint, target.clone())
            .expect("connect authenticated local semantic client");
        let catalog = client
            .fetch_selected_catalog()
            .expect("fetch selected remote-compiler catalog");
        let stamp = catalog.selected_stamp();
        assert_eq!(*stamp.namespace(), selected.namespace().namespace_id());
        assert_eq!(stamp.selection_revision(), selected.generation());
        assert_eq!(stamp.selected_root(), selected.target_root());
        assert_eq!(stamp.closure_id(), selected.closure_id());
        assert_eq!(
            stamp.catalog_root().as_bytes(),
            selected
                .semantic_catalog_root()
                .expect("remote compiler selection publishes its semantic catalog")
        );
        let (image, manifest) = catalog
            .catalog()
            .entries()
            .iter()
            .map(|entry| {
                let image = entry.image();
                let manifest = client
                    .fetch_selected_manifest(image)
                    .unwrap_or_else(|error| panic!("fetch selected image {image:?}: {error}"));
                (image, manifest)
            })
            .find(|(_, manifest)| {
                manifest
                    .plane(core)
                    .and_then(|plane| plane.segments().first())
                    .is_some_and(|segment| segment.byte_length() > 16 * 1024)
            })
            .expect("remote compiler core IR must contain a multi-range segment");
        assert_eq!(manifest.root(), image.manifest_root());
        assert_eq!(manifest.semantic_generation(), image.semantic_generation());
        let segment = manifest
            .plane(core)
            .and_then(|plane| plane.segments().first())
            .expect("selected core segment exceeds one 16 KiB range");
        assert!(segment.byte_length() > 16 * 1024);
        let mut store = FileSemanticRangeStore::open(
            FileStore::open(&client_store_path, 64 * 1024 * 1024)
                .expect("open independent local-client FileStore"),
            limits,
        )
        .expect("open independent local-client sparse range store");
        let full_image = client
            .fetch_selected_image(image, &store, 64 * 1024 * 1024, 4096)
            .expect("fetch and admit exact selected NXFI image into the local CAS");
        assert_eq!(full_image.image().generation(), image.semantic_generation());
        let full_image_bytes = full_image.transferred_bytes();
        let full_image_pages = full_image.page_requests();
        let have_ids = client
            .verified_local_segments(&manifest, image, core, &mut store)
            .expect("inspect exact selected image in the empty client CAS");
        assert!(
            have_ids.is_empty(),
            "client CAS must not alias owner storage"
        );
        let mut cursor = client
            .new_cursor(&manifest, image, core, &have_ids, limits)
            .expect("bind local range cursor to exact selected core plane");
        let request = match client
            .next_request(&mut cursor, None, HydrationCredits::new(1, 16 * 1024))
            .expect("plan the first bounded selected range")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected the first selected range, got {other:?}"),
        };
        let first_range_bytes = client
            .requested_bytes(&request)
            .expect("measure first selected range");
        assert_eq!(first_range_bytes, 16 * 1024);
        let checkpoint = match client
            .request_and_accept(&mut cursor, &request, &mut store, limits)
            .expect("receive and durably stage the first real range")
        {
            SemanticRangeClientProgress::Staged {
                coverage,
                checkpoint,
            } => {
                assert_eq!(coverage.bytes().covered_bytes(), first_range_bytes);
                assert_eq!(checkpoint.selected_stamp(), stamp);
                assert_eq!(checkpoint.image(), image);
                assert_eq!(
                    checkpoint.range_request().byte_length,
                    segment.byte_length()
                );
                checkpoint
            }
            SemanticRangeClientProgress::Complete(_) => {
                panic!("a 16 KiB page incorrectly completed a larger segment")
            }
        };
        persist_client_checkpoint(
            &checkpoint_path,
            &checkpoint
                .encode()
                .expect("encode durable sparse checkpoint"),
        );

        // A valid-shaped but stale stamp for the same object and byte interval must be rejected
        // by locald before serving any bytes. This is a real framed request over the same
        // authenticated local control transport as the production client.
        let wrong_stamp = SelectedGenerationStamp::checked(
            *stamp.namespace(),
            stamp.profile(),
            *stamp.source_coordinate(),
            stamp
                .selection_revision()
                .checked_sub(1)
                .expect("selected revisions are nonzero"),
            *stamp.selected_root(),
            *stamp.closure_id(),
            stamp.catalog_root(),
        )
        .expect("construct a valid-shaped older selected stamp");
        let stale_get = SemanticRangeGet {
            request_id: 91_004,
            target: target.clone(),
            selected_stamp: wrong_stamp,
            image,
            range_request: checkpoint.range_request(),
            byte_range: ByteRange::new(0, first_range_bytes).expect("first range bounds"),
        };
        let stream = LocalStream::connect(endpoint).expect("connect stale-range probe");
        let _peer = AuthenticatedLocalPeer::authenticate(&stream, endpoint)
            .expect("authenticate stale-range probe peer");
        let mut raw = LocalControlClient::new(stream, LocalControlLimits::default());
        let response = raw
            .request(&LocalControlRequest::SemanticRangeGet {
                request_id: stale_get.request_id,
                payload: stale_get
                    .encode()
                    .expect("encode old-stamp semantic range")
                    .into_boxed_slice(),
            })
            .expect("receive typed stale-selection rejection");
        assert_eq!(
            response,
            LocalControlResponse::SemanticStaleSelection {
                request_id: stale_get.request_id
            },
            "locald served a range claimed under an old selected stamp"
        );
        assert!(
            client
                .verified_local_segments(&manifest, image, core, &mut store)
                .expect("recheck segment coverage after stale request")
                .is_empty(),
            "a wrong selected stamp advanced verified local segment coverage"
        );
        let (_, retained, retained_poll) = client
            .resume_semantic_range(&checkpoint, &manifest, &[], limits, &mut store)
            .expect("reopen only the already durable current-generation bytes");
        assert_eq!(retained.bytes().covered_bytes(), first_range_bytes);
        assert!(matches!(retained_poll, IrHydrationPoll::Request(_)));
        let lost_response_bytes = checkpoint
            .range_request()
            .byte_length
            .saturating_sub(first_range_bytes)
            .min(16 * 1024);
        assert!(lost_response_bytes > 0);
        let retry_get = SemanticRangeGet {
            request_id: 91_005,
            target: target.clone(),
            selected_stamp: stamp,
            image,
            range_request: checkpoint.range_request(),
            byte_range: ByteRange::new(first_range_bytes, lost_response_bytes)
                .expect("next missing range bounds"),
        };
        let stream = LocalStream::connect(endpoint).expect("connect dropped-response probe");
        let _retry_peer = AuthenticatedLocalPeer::authenticate(&stream, endpoint)
            .expect("authenticate dropped-response probe peer");
        let mut raw = LocalControlClient::new(stream, LocalControlLimits::default());
        raw.send(&LocalControlRequest::SemanticRangeGet {
            request_id: retry_get.request_id,
            payload: retry_get
                .encode()
                .expect("encode next selected range")
                .into_boxed_slice(),
        })
        .expect("send one real range request before dropping its response");
        // The peer can finish a read, but this process never accepts or checkpoints that reply.
        // On restart the exact same interval must therefore remain in the missing set.
        drop(raw);
        assert!(
            daemon.running(),
            "owner exited during dropped range response"
        );
        (
            image,
            manifest.root(),
            full_image_bytes,
            full_image_pages,
            first_range_bytes,
            lost_response_bytes,
        )
    };

    // Recreate locald and the client/store handles. The encoded checkpoint and fsynced CAS
    // extents are the only state carried over this boundary.
    daemon.kill_now();
    *daemon = Locald::launch_with_remote_segment_gc(
        endpoint,
        workspace,
        authority_secret,
        environment,
        true,
    );
    let checkpoint = SemanticRangeClientCheckpoint::decode(
        &std::fs::read(&checkpoint_path).expect("read durable checkpoint after restart"),
    )
    .expect("decode exact-generation client checkpoint after restart");
    let mut client = LocalSemanticIndexClient::connect(endpoint, target)
        .expect("reconnect local semantic client after owner restart");
    let catalog = client
        .fetch_selected_catalog()
        .expect("re-read selected catalog after owner restart");
    assert_eq!(catalog.selected_stamp(), checkpoint.selected_stamp());
    let manifest = client
        .fetch_selected_manifest(image)
        .expect("reopen exact selected manifest after restart");
    assert_eq!(manifest.root(), manifest_root);
    let mut store = FileSemanticRangeStore::open(
        FileStore::open(&client_store_path, 64 * 1024 * 1024)
            .expect("cold-reopen independent local-client FileStore"),
        limits,
    )
    .expect("cold-reopen independent sparse range store");
    let have_ids = client
        .verified_local_segments(&manifest, image, core, &mut store)
        .expect("check which exact-generation segments survived the client restart");
    assert!(
        have_ids.is_empty(),
        "partial bytes cannot satisfy a segment ID"
    );
    let (mut cursor, durable_partial, mut poll) = client
        .resume_semantic_range(&checkpoint, &manifest, &have_ids, limits, &mut store)
        .expect("resume the same exact selection from its durable missing-object set");
    assert_eq!(durable_partial.bytes().covered_bytes(), first_range_bytes);
    let mut resumed_ranges = 0_usize;
    let mut resumed_bytes = 0_u64;
    let mut resumed_segments = 0_usize;
    loop {
        match poll {
            IrHydrationPoll::Request(request) => {
                let bytes = client
                    .requested_bytes(&request)
                    .expect("count each remaining selected byte range");
                if resumed_ranges == 0 {
                    assert_eq!(
                        bytes, lost_response_bytes,
                        "restart did not request again the range whose reply was dropped"
                    );
                }
                resumed_ranges = resumed_ranges.saturating_add(1);
                resumed_bytes = resumed_bytes.saturating_add(bytes);
                let next_partial = match client
                    .request_and_accept(&mut cursor, &request, &mut store, limits)
                    .expect("receive resumed bytes through authenticated locald")
                {
                    SemanticRangeClientProgress::Staged {
                        coverage,
                        checkpoint,
                    } => {
                        persist_client_checkpoint(
                            &checkpoint_path,
                            &checkpoint.encode().expect("encode resumed checkpoint"),
                        );
                        Some(coverage)
                    }
                    SemanticRangeClientProgress::Complete(_) => {
                        resumed_segments = resumed_segments.saturating_add(1);
                        let _ = std::fs::remove_file(&checkpoint_path);
                        None
                    }
                };
                poll = client
                    .next_request(
                        &mut cursor,
                        next_partial.as_ref(),
                        HydrationCredits::new(1, 16 * 1024),
                    )
                    .expect("plan next missing range from durable coverage");
            }
            IrHydrationPoll::VerifyLocal(request) => {
                client
                    .verify_local_segment(&mut cursor, &request, &mut store)
                    .expect("admit a sparse-complete segment from its durable CAS bytes");
                resumed_segments = resumed_segments.saturating_add(1);
                let _ = std::fs::remove_file(&checkpoint_path);
                poll = client
                    .next_request(&mut cursor, None, HydrationCredits::new(1, 16 * 1024))
                    .expect("plan next segment after local identity admission");
            }
            IrHydrationPoll::NoCredits => panic!("hydration credits are nonzero"),
            IrHydrationPoll::Exhausted => break,
        }
    }
    assert!(resumed_ranges > 0, "restart did not resume any byte ranges");
    assert!(
        resumed_bytes > 0,
        "restart did not transfer any missing bytes"
    );
    assert_eq!(
        resumed_bytes.saturating_add(first_range_bytes),
        manifest
            .plane(core)
            .expect("selected core plane")
            .segments()
            .iter()
            .map(|segment| segment.byte_length())
            .sum::<u64>(),
        "the restarted transfer must account for every verified core byte exactly once"
    );
    let verified = client
        .verified_local_segments(&manifest, image, core, &mut store)
        .expect("verify every selected core segment from the local CAS");
    let expected = manifest
        .plane(core)
        .expect("selected image contains core IR")
        .segments()
        .iter()
        .map(|segment| *segment.id_claim().as_bytes())
        .collect::<Vec<_>>();
    assert_eq!(
        verified
            .iter()
            .map(|segment| *segment.as_bytes())
            .collect::<Vec<_>>(),
        expected,
        "client only admits all bytes after they match this exact selected manifest"
    );
    let generation = client
        .commit_local_generation(image, &manifest, core, &mut store)
        .expect("commit exact selected generation after full core-plane verification");
    assert_eq!(generation.selected_stamp(), catalog.selected_stamp());
    assert_eq!(generation.image(), image);
    let s3_ranges_after = s3.stats().range_gets;
    assert!(
        s3_ranges_after > s3_ranges_before,
        "cold selected segment hydration did not fetch its immutable bytes from S3: before={s3_ranges_before}, after={s3_ranges_after}"
    );
    eprintln!(
        "remote compiler -> selected index -> local client: generation={} full_image_bytes={} full_image_pages={} first_range_bytes={} retained_after_wrong_stamp_bytes={} dropped_response_bytes={} resumed_ranges={} resumed_bytes={} resumed_segments={} verified_core_segments={}/{} s3_range_gets={}",
        selected.generation(),
        full_image_bytes,
        full_image_pages,
        first_range_bytes,
        first_range_bytes,
        lost_response_bytes,
        resumed_ranges,
        resumed_bytes,
        resumed_segments,
        verified.len(),
        expected.len(),
        s3_ranges_after.saturating_sub(s3_ranges_before),
    );
}

#[derive(Debug)]
struct Worker {
    child: Option<Child>,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
}

impl Worker {
    fn launch(config: &Path, data_dir: &Path, environment: &CompilerProcessEnvironment) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-worker"));
        command
            .args(["cluster", "run", "--config"])
            .arg(config)
            .args(["--data-dir"])
            .arg(data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        environment.apply(&mut command);
        let mut child = command.spawn().expect("spawn production Iroh worker");
        let (send, receive) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if send.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        let mut worker = Self {
            child: Some(child),
            lines: receive,
            seen: Vec::new(),
        };
        assert!(
            worker.wait_for_line("transport: direct-only", DEADLINE),
            "worker did not bind its direct-only Iroh endpoint: {:?}",
            worker.seen
        );
        worker
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll worker"))
            .is_none()
    }

    fn kill_now(&mut self) -> Option<ExitStatus> {
        let mut child = self.child.take()?;
        if child
            .try_wait()
            .expect("poll worker before SIGKILL")
            .is_none()
        {
            child.kill().expect("SIGKILL worker process");
        }
        let status = child.wait().expect("wait for killed worker");
        self.drain_lines();
        Some(status)
    }

    fn wait_for_ack_count(&mut self, required: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .seen
                .iter()
                .filter(|line| line.contains(TRUSTED_RESULT_ACK))
                .count()
                >= required
            {
                return true;
            }
            if !self.running() || Instant::now() >= deadline {
                self.drain_lines();
                return self
                    .seen
                    .iter()
                    .filter(|line| line.contains(TRUSTED_RESULT_ACK))
                    .count()
                    >= required;
            }
            match self.lines.recv_timeout(POLL) {
                Ok(line) => self.seen.push(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    fn wait_for_line(&mut self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.seen.iter().any(|line| line.contains(needle)) {
                return true;
            }
            if !self.running() || Instant::now() >= deadline {
                self.drain_lines();
                return self.seen.iter().any(|line| line.contains(needle));
            }
            match self.lines.recv_timeout(POLL) {
                Ok(line) => self.seen.push(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    fn drain_lines(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child
                .try_wait()
                .expect("poll worker during cleanup")
                .is_none()
            {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn run_bounded(mut command: Command, label: &str, timeout: Duration) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{label} exceeded {timeout:?}");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("wait for {label}: {error}");
            }
        }
    };
    Output {
        status,
        stdout: stdout_reader.join().expect("join command stdout"),
        stderr: stderr_reader.join().expect("join command stderr"),
    }
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed ({}): stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli_command(endpoint: &Path, workspace: &Path, project: &Path, words: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
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
    clean_product_environment(&mut command);
    command
}

fn cli_json(endpoint: &Path, workspace: &Path, project: &Path, words: &[&str]) -> Value {
    let output = run_bounded(
        cli_command(endpoint, workspace, project, words),
        "CLI",
        DEADLINE,
    );
    assert_success(&output, &format!("CLI {words:?}"));
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI {words:?} did not return JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn mcp(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority_secret: &Path,
    calls: &[(u64, Value)],
) -> std::collections::BTreeMap<u64, Value> {
    let mut lines = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "cluster-source-journey", "version": "1" }
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority_secret);
    clean_product_environment(&mut command);
    command.env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority_secret);
    command.stdin(Stdio::piped());
    let mut child = command.spawn().expect("spawn MCP process");
    child
        .stdin
        .take()
        .expect("MCP stdin")
        .write_all(&input)
        .expect("write MCP requests");
    let output = wait_for_output(child, "MCP process", DEADLINE);
    assert_success(&output, "MCP process");
    BufReader::new(output.stdout.as_slice())
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter_map(|value| value["id"].as_u64().map(|id| (id, value)))
        .collect()
}

fn wait_for_output(mut child: Child, label: &str, timeout: Duration) -> Output {
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
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{label} exceeded {timeout:?}");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("wait for {label}: {error}");
            }
        }
    };
    Output {
        status,
        stdout: stdout_reader.join().expect("join child stdout"),
        stderr: stderr_reader.join().expect("join child stderr"),
    }
}

fn mcp_content<'reply>(reply: &'reply Value, label: &str) -> &'reply Value {
    assert_eq!(
        reply["result"]["isError"], false,
        "MCP {label} failed: {reply}"
    );
    reply["result"]
        .get("structuredContent")
        .unwrap_or_else(|| panic!("MCP {label} omitted structured content: {reply}"))
}

fn direct_owner_cli(workspace: &Path, project: &Path, args: &[&str]) -> Output {
    direct_owner_cli_with_environment(workspace, project, args, None)
}

fn direct_owner_cli_with_environment(
    workspace: &Path,
    project: &Path,
    args: &[&str],
    environment: Option<&CompilerProcessEnvironment>,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .args(args);
    match environment {
        Some(environment) => environment.apply(&mut command),
        None => clean_product_environment(&mut command),
    }
    run_bounded(command, "owner cluster CLI", DEADLINE)
}

fn worker_cli(args: &[&str], environment: &CompilerProcessEnvironment) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-worker"));
    command.args(args);
    environment.apply(&mut command);
    run_bounded(command, "worker cluster CLI", DEADLINE)
}

fn compiler_scope_profile() -> LanguageProfile {
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
    let bytes = <[u8; 2]>::from(compiler_scope_profile());
    AuthorityNamespace::semantic_profile(
        project_label,
        coordinate,
        "locald",
        "locald-product-v1",
        format!("{:02x}{:02x}/lower-ir", bytes[0], bytes[1]),
    )
    .expect("semantic authority namespace")
}

fn authority_handle(runtime: &tokio::runtime::Runtime, path: &Path) -> TursoAuthority {
    runtime
        .block_on(TursoAuthority::open(path))
        .unwrap_or_else(|error| panic!("open Turso selected-head authority: {error}"))
}

fn selected_generation(
    runtime: &tokio::runtime::Runtime,
    authority: &TursoAuthority,
    namespace: &AuthorityNamespace,
) -> SelectedGeneration {
    let frontier = runtime
        .block_on(authority.selected_frontier(namespace))
        .expect("read Turso selected head")
        .unwrap_or_else(|| panic!("Turso has no selected head for {namespace:?}"));
    runtime
        .block_on(authority.selected_generation(namespace, frontier.generation()))
        .expect("read selected generation history")
        .unwrap_or_else(|| panic!("selected generation is missing from history"))
}

fn assert_same_selected_generation(expected: &SelectedGeneration, actual: &SelectedGeneration) {
    assert_eq!(
        expected.namespace().namespace_id(),
        actual.namespace().namespace_id()
    );
    assert_eq!(expected.generation(), actual.generation());
    assert_eq!(expected.candidate_id(), actual.candidate_id());
    assert_eq!(expected.target_root(), actual.target_root());
    assert_eq!(expected.closure_id(), actual.closure_id());
    assert_eq!(expected.input_digest(), actual.input_digest());
    assert_eq!(expected.attempt(), actual.attempt());
    assert_eq!(expected.scheduler_fence(), actual.scheduler_fence());
    assert_eq!(expected.pack_id(), actual.pack_id());
    assert_eq!(
        expected.semantic_manifest_root(),
        actual.semantic_manifest_root()
    );
    assert_eq!(expected.selection_origin(), actual.selection_origin());
}

fn wait_for_journey_marker(
    path: &Path,
    locald: &mut Locald,
    add_thread: Option<&thread::JoinHandle<Output>>,
    expected_state: u8,
    timeout: Duration,
) -> String {
    let fallback_count_before_wait = locald.local_fallback_count();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        assert_eq!(
            locald.local_fallback_count(),
            fallback_count_before_wait,
            "the configured remote Add fell back before the process durability barrier: {}",
            locald.diagnostics()
        );
        if let Some(add_thread) = add_thread {
            assert!(
                !add_thread.is_finished(),
                "public Add returned without reaching the expected remote process boundary"
            );
        }
        if path.is_file() {
            let marker = std::fs::read_to_string(path).expect("read process durability marker");
            let expected_marker = match expected_state {
                PENDING_ACK_STATE_AWAITING_SELECTION => "awaiting-selection",
                PENDING_ACK_STATE_STORED_PENDING => "stored-ack-pending",
                PENDING_ACK_STATE_STORED_AWAITING_CONFIRM => "stored-awaiting-retirement-confirm",
                PENDING_ACK_RECOVERED_PENDING_BEFORE_GRANT_PAGES => {
                    "pending-result-before-grant-pages"
                }
                PENDING_ACK_LIVE_RECEIPT_BEFORE_GRANT_PAGES => {
                    "live-result-receipt-before-grant-pages"
                }
                5 => "worker-pending-before-result-receipt",
                other => panic!("journey requested unsupported pause state {other}"),
            };
            assert_eq!(
                marker.trim(),
                expected_marker,
                "journey barrier reported an unexpected state"
            );
            assert!(
                locald.running(),
                "the process durability barrier appeared after locald exited"
            );
            return marker;
        }
        assert!(
            locald.running(),
            "locald exited before the durable ACK barrier: {}",
            locald.diagnostics()
        );
        thread::sleep(Duration::from_millis(1));
    }
    panic!(
        "journey-only durable process barrier did not block within {timeout:?}: {}",
        locald.diagnostics()
    );
}

fn assert_selected_head(
    generation: &SelectedGeneration,
    expected_generation: u64,
    expected_origin: SelectionOrigin,
) {
    assert_eq!(generation.generation(), expected_generation);
    assert_eq!(generation.selection_origin(), expected_origin);
    assert_ne!(*generation.target_root(), [0; 32]);
}

fn assert_ack_journal_state(path: &Path, expected_state: u8) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        panic!(
            "read durable Stored-ACK journal {}: {error}",
            path.display()
        )
    });
    assert!(
        bytes.len() > PENDING_ACK_HEADER_BYTES + PENDING_ACK_CHECKSUM_BYTES,
        "pending Stored-ACK journal has no row"
    );
    assert_eq!(
        &bytes[..8],
        PENDING_ACK_MAGIC,
        "unexpected ACK journal format"
    );
    assert_eq!(u16::from_be_bytes([bytes[8], bytes[9]]), 4);
    assert_eq!(
        usize::from(u16::from_be_bytes([bytes[10], bytes[11]])),
        1,
        "expected exactly one pending Stored-ACK row"
    );
    let body_length = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    assert_eq!(
        body_length,
        bytes.len() - PENDING_ACK_HEADER_BYTES - PENDING_ACK_CHECKSUM_BYTES,
        "pending Stored-ACK body length differs from its v4 header"
    );
    let record_length = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as usize;
    assert!(record_length > 0, "pending Stored-ACK record is empty");
    assert_eq!(
        record_length + 4,
        body_length,
        "pending Stored-ACK record does not fill its v4 body"
    );
    assert_eq!(bytes[20], 1, "expected a v4 selection entry");
    assert_eq!(
        bytes[21], expected_state,
        "unexpected owner ACK journal state"
    );
    let checksum_offset = bytes.len() - PENDING_ACK_CHECKSUM_BYTES;
    assert_eq!(
        blake3::hash(&bytes[..checksum_offset])
            .as_bytes()
            .as_slice(),
        &bytes[checksum_offset..],
        "pending Stored-ACK checksum is invalid"
    );
}

fn assert_ack_journal_empty(path: &Path) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        panic!(
            "read recovered Stored-ACK journal {}: {error}",
            path.display()
        )
    });
    assert_eq!(
        &bytes[..8],
        PENDING_ACK_MAGIC,
        "unexpected ACK journal format"
    );
    assert_eq!(u16::from_be_bytes([bytes[8], bytes[9]]), 4);
    assert_eq!(
        u16::from_be_bytes([bytes[10], bytes[11]]),
        0,
        "owner retained its ACK intent after retirement Applied"
    );
    assert_eq!(
        u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        0,
        "empty owner ACK journal retains a record body"
    );
    assert_eq!(
        bytes.len(),
        PENDING_ACK_HEADER_BYTES + PENDING_ACK_CHECKSUM_BYTES,
        "empty owner ACK journal has an unexpected v4 size"
    );
    let checksum_offset = bytes.len() - PENDING_ACK_CHECKSUM_BYTES;
    assert_eq!(
        blake3::hash(&bytes[..checksum_offset])
            .as_bytes()
            .as_slice(),
        &bytes[checksum_offset..],
        "empty owner ACK journal checksum is invalid"
    );
}

fn offer_reservation_entry(journal: &[u8]) -> &[u8] {
    assert!(journal.len() >= PENDING_ACK_HEADER_BYTES + PENDING_ACK_CHECKSUM_BYTES);
    assert_eq!(
        &journal[..8],
        PENDING_ACK_MAGIC,
        "unexpected ACK journal format"
    );
    assert_eq!(u16::from_be_bytes([journal[8], journal[9]]), 4);
    assert_eq!(u16::from_be_bytes([journal[10], journal[11]]), 1);
    let body_length =
        u32::from_be_bytes([journal[12], journal[13], journal[14], journal[15]]) as usize;
    assert_eq!(
        body_length,
        journal.len() - PENDING_ACK_HEADER_BYTES - PENDING_ACK_CHECKSUM_BYTES
    );
    let record_length =
        u32::from_be_bytes([journal[16], journal[17], journal[18], journal[19]]) as usize;
    assert_eq!(record_length + 4, body_length);
    assert_eq!(journal[20], 5, "expected OfferMayBeSent reservation entry");
    assert_eq!(journal[85], 2, "expected OfferMayBeSent reservation stage");
    let checksum_offset = journal.len() - PENDING_ACK_CHECKSUM_BYTES;
    assert_eq!(
        blake3::hash(&journal[..checksum_offset])
            .as_bytes()
            .as_slice(),
        &journal[checksum_offset..],
        "OfferMayBeSent journal checksum is invalid"
    );
    &journal[20..20 + record_length]
}

fn selection_journal_entry(journal: &[u8], expected_state: u8) -> &[u8] {
    assert!(journal.len() >= PENDING_ACK_HEADER_BYTES + PENDING_ACK_CHECKSUM_BYTES);
    assert_eq!(&journal[..8], PENDING_ACK_MAGIC);
    assert_eq!(u16::from_be_bytes([journal[8], journal[9]]), 4);
    assert_eq!(u16::from_be_bytes([journal[10], journal[11]]), 1);
    let body_length =
        u32::from_be_bytes([journal[12], journal[13], journal[14], journal[15]]) as usize;
    assert_eq!(
        body_length,
        journal.len() - PENDING_ACK_HEADER_BYTES - PENDING_ACK_CHECKSUM_BYTES
    );
    let record_length =
        u32::from_be_bytes([journal[16], journal[17], journal[18], journal[19]]) as usize;
    assert_eq!(record_length + 4, body_length);
    assert_eq!(journal[20], 1, "expected a v4 selection entry");
    assert_eq!(journal[21], expected_state, "unexpected selection state");
    let checksum_offset = journal.len() - PENDING_ACK_CHECKSUM_BYTES;
    assert_eq!(
        blake3::hash(&journal[..checksum_offset])
            .as_bytes()
            .as_slice(),
        &journal[checksum_offset..],
        "selection journal checksum is invalid"
    );
    &journal[20..20 + record_length]
}

fn mutate_selection_worker_closure(journal: &mut [u8]) -> ([u8; 32], [u8; 32]) {
    let entry_length = selection_journal_entry(journal, PENDING_ACK_STATE_AWAITING_SELECTION).len();
    let record_end = 20 + entry_length;
    let mut cursor = 22_usize; // entry tag, record state, then owner endpoint
    cursor += 32 + 32; // owner endpoint and worker peer
    let address_kind = *journal.get(cursor).expect("worker address family");
    cursor += match address_kind {
        4 => 1 + 4 + 2,
        6 => 1 + 16 + 2 + 4 + 4,
        other => panic!("unexpected worker address family {other}"),
    };
    cursor += 16; // namespace
    for _ in 0..3 {
        skip_journal_text(journal, &mut cursor);
    }
    cursor += 16 + 8 + 32; // work ID, assignment attempt, assignment fence
    let worker_closure_offset = cursor;
    let original_closure: [u8; 32] = journal
        .get(worker_closure_offset..worker_closure_offset + 32)
        .expect("worker closure identity")
        .try_into()
        .expect("fixed worker closure identity");
    cursor += 32 + 4 + 8 + 8; // closure, object count, payload bytes, verified bytes
    cursor += 16 + 16 + 8 + 32 + 32; // receipt scope and target root
    match *journal.get(cursor).expect("receipt pack option") {
        0 => cursor += 1,
        1 => cursor += 1 + 32,
        other => panic!("unexpected result receipt pack option {other}"),
    }
    let receipt_closure_offset = cursor;
    let receipt_closure: [u8; 32] = journal
        .get(receipt_closure_offset..receipt_closure_offset + 32)
        .expect("receipt closure identity")
        .try_into()
        .expect("fixed receipt closure identity");
    assert_eq!(original_closure, receipt_closure);
    assert_ne!(original_closure, [0; 32]);
    assert!(receipt_closure_offset + 32 <= record_end);

    let mut tampered_closure = original_closure;
    tampered_closure[0] ^= 0x40;
    assert_ne!(tampered_closure, [0; 32]);
    assert_ne!(tampered_closure, original_closure);
    journal[worker_closure_offset..worker_closure_offset + 32].copy_from_slice(&tampered_closure);
    journal[receipt_closure_offset..receipt_closure_offset + 32].copy_from_slice(&tampered_closure);

    let checksum_offset = journal.len() - PENDING_ACK_CHECKSUM_BYTES;
    let checksum = blake3::hash(&journal[..checksum_offset]);
    journal[checksum_offset..].copy_from_slice(checksum.as_bytes());
    let _ = selection_journal_entry(journal, PENDING_ACK_STATE_AWAITING_SELECTION);
    (original_closure, tampered_closure)
}

fn semantic_cas_inventory(workspace: &Path, directory: &str) -> Vec<(OsString, u64)> {
    let path = workspace.join("semantic-objects").join(directory);
    let mut files = std::fs::read_dir(&path)
        .unwrap_or_else(|error| panic!("read semantic CAS {}: {error}", path.display()))
        .map(|entry| {
            let entry = entry.unwrap_or_else(|error| {
                panic!("read semantic CAS entry in {}: {error}", path.display())
            });
            let metadata = entry
                .metadata()
                .unwrap_or_else(|error| panic!("stat semantic CAS entry: {error}"));
            assert!(metadata.is_file(), "unexpected nested semantic CAS entry");
            (entry.file_name(), metadata.len())
        })
        .collect::<Vec<_>>();
    files.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    files
}

fn assert_closure_objects_present(store: &FileStore, closure: [u8; 32], label: &str) {
    let manifest = store
        .open_closure_claim(ArtifactClosureClaim::from_bytes(closure))
        .unwrap_or_else(|error| panic!("reopen authentic {label} closure: {error:?}"));
    let mut after = None;
    loop {
        let page = manifest
            .page_ids(after, 128)
            .unwrap_or_else(|error| panic!("read authentic {label} closure members: {error:?}"));
        for object in page.object_ids() {
            assert!(
                store
                    .contains_object(*object)
                    .unwrap_or_else(|error| panic!("check authentic {label} object: {error:?}")),
                "GC removed an object from the authentic {label} closure"
            );
        }
        match page.next() {
            Some(next) => {
                assert_ne!(after, Some(next), "closure member cursor did not advance");
                after = Some(next);
            }
            None => break,
        }
    }
}

#[derive(Clone, Copy)]
enum SelectedClaimMutation {
    Generation,
    Candidate,
    SelectedClosure,
}

fn mutate_selected_claim(journal: &mut [u8], expected_state: u8, mutation: SelectedClaimMutation) {
    let entry_length = selection_journal_entry(journal, expected_state).len();
    let record_end = 20 + entry_length;
    let mut cursor = 22_usize; // entry tag and state
    cursor += 32 + 32; // owner endpoint and worker peer
    let address_kind = *journal.get(cursor).expect("worker address family");
    cursor += match address_kind {
        4 => 1 + 4 + 2,
        6 => 1 + 16 + 2 + 4 + 4,
        other => panic!("unexpected worker address family {other}"),
    };
    cursor += 16; // namespace
    for _ in 0..3 {
        skip_journal_text(journal, &mut cursor);
    }
    cursor += 16 + 8 + 32; // work ID, assignment attempt, assignment fence
    cursor += 32 + 4 + 8 + 8; // worker closure and receipt totals
    cursor += 16 + 16 + 8 + 32 + 32; // result scope and target root
    match *journal.get(cursor).expect("receipt pack option") {
        0 => cursor += 1,
        1 => cursor += 1 + 32,
        other => panic!("unexpected result receipt pack option {other}"),
    }
    cursor += 32 + 4 + 8 + 4; // result closure, object count, bytes, grant pages
    let generation_offset = cursor;
    cursor += 8 + 16 + 8 + 32 + 32; // Turso generation, attempt, epoch, fence, input digest
    let candidate_offset = cursor;
    let selected_closure_offset = candidate_offset + 32 + 32;
    assert!(
        selected_closure_offset + 32 <= record_end,
        "selected authority fields lie outside selected row"
    );
    let (offset, byte_index, mask) = match mutation {
        SelectedClaimMutation::Generation => (generation_offset, 7, 1),
        SelectedClaimMutation::Candidate => (candidate_offset, 0, 0x20),
        SelectedClaimMutation::SelectedClosure => (selected_closure_offset, 0, 0x40),
    };
    let field = journal
        .get_mut(
            offset
                ..offset
                    + if matches!(mutation, SelectedClaimMutation::Generation) {
                        8
                    } else {
                        32
                    },
        )
        .expect("selected authority field");
    assert!(field.iter().any(|byte| *byte != 0));
    field[byte_index] ^= mask;

    let checksum_offset = journal.len() - PENDING_ACK_CHECKSUM_BYTES;
    let checksum = blake3::hash(&journal[..checksum_offset]);
    journal[checksum_offset..].copy_from_slice(checksum.as_bytes());
    let _ = selection_journal_entry(journal, expected_state);
}

fn assert_offer_may_be_sent_journal(path: &Path) -> Vec<u8> {
    let journal = std::fs::read(path)
        .unwrap_or_else(|error| panic!("read OfferMayBeSent journal {}: {error}", path.display()));
    let _ = offer_reservation_entry(&journal);
    journal
}

fn skip_journal_text<'journal>(journal: &'journal [u8], cursor: &mut usize) -> &'journal [u8] {
    let length = u16::from_be_bytes([
        *journal.get(*cursor).expect("journal text length high byte"),
        *journal
            .get(*cursor + 1)
            .expect("journal text length low byte"),
    ]) as usize;
    let start = *cursor + 2;
    *cursor = start + length;
    assert!(
        *cursor <= journal.len(),
        "journal text runs past end of file"
    );
    &journal[start..*cursor]
}

fn mutate_capture_source_fence_digest(journal: &mut [u8], expected_source_root: &Path) {
    let entry_length = offer_reservation_entry(journal).len();
    let mut cursor = 21_usize; // typed entry tag followed by reservation body
    cursor += 32 + 32; // reservation ID and owner endpoint
    assert_eq!(journal[cursor], 2, "reservation is not OfferMayBeSent");
    cursor += 1;

    cursor += 32 + 32; // assignment identity owner endpoint and worker peer
    let address_kind = *journal.get(cursor).expect("worker address family");
    cursor += match address_kind {
        4 => 1 + 4 + 2,
        6 => 1 + 16 + 2 + 4 + 4,
        other => panic!("unexpected worker address family {other}"),
    };
    cursor += 16; // namespace
    for _ in 0..3 {
        skip_journal_text(journal, &mut cursor);
    }
    cursor += 16 + 8 + 32; // work ID, assignment attempt, assignment fence
    cursor += 32 + 2 + 1 + 32 + 32 + 32; // trust recipe/profile/stage/toolchain/environment/target
    let source_root_bytes = skip_journal_text(journal, &mut cursor); // v4 canonical source root
    assert_eq!(
        source_root_bytes,
        expected_source_root
            .to_str()
            .expect("UTF-8 fixture source root")
            .as_bytes(),
        "OfferMayBeSent did not persist the fixture's canonical source root"
    );
    cursor += 6 * 32 + 8 + 32 + 32 + 8 + 8; // capture through payload byte count
    assert!(
        cursor + 32 <= 20 + entry_length,
        "capture digest lies outside OfferMayBeSent row"
    );
    let digest = journal
        .get_mut(cursor..cursor + 32)
        .expect("capture source fence digest");
    assert!(
        digest.iter().any(|byte| *byte != 0),
        "capture source fence digest must be nonzero"
    );
    digest[0] ^= 0x80;

    let checksum_offset = journal.len() - PENDING_ACK_CHECKSUM_BYTES;
    let checksum = blake3::hash(&journal[..checksum_offset]);
    journal[checksum_offset..].copy_from_slice(checksum.as_bytes());
    let _ = offer_reservation_entry(journal);
}

fn rewrite_durable_file(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap_or_else(|error| {
            panic!("open journal for test mutation {}: {error}", path.display())
        });
    file.write_all(bytes)
        .unwrap_or_else(|error| panic!("write journal mutation {}: {error}", path.display()));
    file.sync_all()
        .unwrap_or_else(|error| panic!("sync journal mutation {}: {error}", path.display()));
    std::fs::File::open(path.parent().expect("journal parent"))
        .and_then(|directory| directory.sync_all())
        .unwrap_or_else(|error| panic!("sync journal directory {}: {error}", path.display()));
}

fn count_worker_result_records(data_dir: &Path, extension: &str) -> usize {
    let directory = data_dir.join("result-cas/worker-results");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(error) => panic!(
            "read worker result journal {}: {error}",
            directory.display()
        ),
    };
    entries
        .map(|entry| entry.expect("read worker result journal entry").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some(extension))
        .count()
}

fn run_pending_results(
    config: &Path,
    data_dir: &Path,
    environment: &CompilerProcessEnvironment,
) -> String {
    let output = worker_cli(
        &[
            "cluster",
            "pending",
            "--config",
            config.to_str().expect("UTF-8 worker config path"),
            "--data-dir",
            data_dir.to_str().expect("UTF-8 worker data path"),
        ],
        environment,
    );
    assert_success(&output, "worker pending-results command");
    String::from_utf8(output.stdout).expect("worker pending output UTF-8")
}

fn add_command(endpoint: &Path, workspace: &Path, project: &Path) -> Command {
    cli_command(
        endpoint,
        workspace,
        project,
        &["add", project.to_str().expect("UTF-8 project path")],
    )
}

fn background_add_command(endpoint: &Path, workspace: &Path, project: &Path) -> Command {
    let mut command = add_command(endpoint, workspace, project);
    command.args(["--execution-intent", "background"]);
    command
}

#[test]
fn public_add_selects_a_remote_compiler_head_and_replays_a_pending_stored_ack() {
    assert_cargo_process_headroom();
    let root = FixtureRoot::new();
    let workspace = root.path().join("owner-data");
    let project = root.path().join("one-source-project");
    let worker_data = root.path().join("worker-data");
    let probe_data = root.path().join("compiler-probe-data");
    create_private_directory(&workspace);
    create_private_directory(&project.join("src"));
    create_private_directory(&worker_data);
    create_private_directory(&probe_data);

    let cargo_manifest = project.join("Cargo.toml");
    assert_eq!(CARGO_MANIFEST_BEFORE.len(), CARGO_MANIFEST_AFTER.len());
    assert_eq!(CARGO_MANIFEST_AFTER.len(), CARGO_MANIFEST_LIVE.len());
    assert_eq!(INVENTORY_PATH_BEFORE.len(), INVENTORY_PATH_AFTER.len());
    std::fs::write(&cargo_manifest, CARGO_MANIFEST_BEFORE)
        .expect("write hidden compiler configuration");
    let mut compiler_source = String::from(
        "pub fn remote_journey_helper() -> &'static str { \"helper\" }\n#[doc = include_str!(\"../Cargo.toml\")]\npub fn remote_journey_entry() -> &'static str { remote_journey_helper() }\n",
    );
    // Keep the same one-file compiler fixture while making its canonical core plane larger than
    // one production 16 KiB range. That forces the real client path through durable partial
    // coverage and a resumable missing-object set instead of accidentally proving one-shot IO.
    let bulk_function_count = std::env::var("BACKEND_JOURNEY_BULK_FUNCTIONS")
        .map(|value| {
            value
                .parse::<usize>()
                .expect("BACKEND_JOURNEY_BULK_FUNCTIONS must be an integer")
        })
        .unwrap_or(1024);
    assert!(
        bulk_function_count > 0,
        "bulk fixture must exercise Rust IR"
    );
    eprintln!("remote compiler fixture bulk functions: {bulk_function_count}");
    for index in 0..bulk_function_count {
        compiler_source.push_str(&format!(
            "pub fn remote_journey_bulk_{index:04}() -> u64 {{ {index} }}\n"
        ));
    }
    std::fs::write(project.join("src/lib.rs"), compiler_source)
        .expect("write the only compiler source");
    assert_eq!(
        std::fs::read_dir(project.join("src"))
            .expect("read source directory")
            .count(),
        1,
        "the asymmetric fixture must contain exactly one Rust compiler source"
    );
    std::fs::write(project.join(INVENTORY_PATH_BEFORE), INVENTORY_CONTENT)
        .expect("write stable-size non-source inventory entry");
    assert!(
        !project.join("compiler.optional").exists(),
        "the later negative-frontier path must be absent during calibration"
    );
    let calibrated_manifest_size = std::fs::metadata(&cargo_manifest)
        .expect("stat calibrated Cargo manifest")
        .len();
    let calibrated_inventory_size = std::fs::metadata(project.join(INVENTORY_PATH_BEFORE))
        .expect("stat calibrated inventory entry")
        .len();
    let project = project.canonicalize().expect("canonical source project");
    let project_label = project.to_string_lossy().into_owned();
    let coordinate = semantic_coordinate(&project_label);
    let namespace = semantic_namespace(&project_label, &coordinate);
    let profile = <[u8; 2]>::from(compiler_scope_profile());
    let profile_hex = hex(&profile);
    let authority_path = workspace.join(backend_extension_turso::AUTHORITY_FILE_NAME);
    let endpoint = backend_runtime::derive_endpoint(&workspace);
    let authority_secret = root.path().join("authority.secret");
    write_authority_secret(&authority_secret);
    let s3 = LoopbackS3::start().expect("start real S3-compatible loopback HTTP server");

    let rustc = available_rustc();
    let cargo = rustc
        .parent()
        .expect("canonical rustc has a parent directory")
        .join("cargo");
    assert!(
        cargo.is_file(),
        "the production compiler host resolves Cargo beside NUDOX_RUSTC; missing {}",
        cargo.display()
    );
    let cargo_home = root.path().join("compiler-cargo-home");
    let cargo_root = cargo_home.join("registry").join("src");
    create_private_directory(&cargo_home);
    create_private_directory(&cargo_root);
    let pause_state0_marker = root.path().join("locald-paused-before-selection");
    let pause_state1_marker = root.path().join("locald-paused-after-selection");
    let pause_state3_marker = root.path().join("locald-paused-before-retirement-confirm");
    let pause_offer_pending_marker = root.path().join("worker-paused-before-result-receipt");
    let recovered_pending_marker = root.path().join("locald-paused-after-recovered-pending");
    let live_receipt_marker = root.path().join("locald-paused-after-live-result-receipt");
    // Keep this hook inert for the earlier process cuts. The live-receipt phase removes this
    // one-shot marker; journey-only controls do not participate in compiler capability identity.
    std::fs::write(&live_receipt_marker, "disabled until live-receipt phase\n")
        .expect("disable live-receipt pause during earlier journey phases");
    let pause_library = compile_ack_pause_interposer(root.path());
    let environment = CompilerProcessEnvironment {
        rustc,
        cargo,
        cargo_home,
        cargo_root,
        s3_endpoint: s3.origin().to_owned(),
        profile: profile_hex,
        pause_state0_marker: pause_state0_marker.clone(),
        pause_state1_marker: pause_state1_marker.clone(),
        pause_state3_marker: pause_state3_marker.clone(),
        pause_offer_pending_marker: pause_offer_pending_marker.clone(),
        recovered_pending_marker: recovered_pending_marker.clone(),
        live_receipt_marker: live_receipt_marker.clone(),
        pause_library,
    };

    // Ask the real compiler host for the exact portable grant. The resulting IDs are fed to
    // production owner and worker CLI commands, each of which independently checks the same
    // identity during invite creation and private trust import.
    let mut probe = Command::new(env!("CARGO_BIN_EXE_backend-journey-cluster-probe"));
    probe.arg(&probe_data);
    environment.apply(&mut probe);
    let probe_output = run_bounded(probe, "production compiler capability probe", DEADLINE);
    assert_success(&probe_output, "production compiler capability probe");
    let capability: Value =
        serde_json::from_slice(&probe_output.stdout).expect("parse compiler execution identity");
    assert_eq!(capability["profile"], environment.profile);

    // Exercise the same shipped scope-inspection command operators use for enrollment, with the
    // same typed toolchain inputs as locald and the worker. Use its report as the enrollment source
    // only after every field agrees with the production compiler-host probe.
    let scope_output = direct_owner_cli_with_environment(
        &workspace,
        &project,
        &[
            "--format",
            "json",
            "cluster",
            "scope",
            "show",
            "--package",
            &project_label,
            "--profile",
            "rust-2024",
        ],
        Some(&environment),
    );
    assert_success(&scope_output, "operator compiler scope inspection");
    let scope: Value =
        serde_json::from_slice(&scope_output.stdout).expect("parse operator compiler scope report");
    assert_eq!(scope["target_kind"], "local");
    assert_eq!(scope["coordinate"], coordinate);
    assert_eq!(scope["namespace"], hex(&namespace.namespace_id()));
    assert_eq!(scope["profile"], environment.profile);
    assert_eq!(scope["profile_name"], "rust-2024");
    assert_eq!(scope["stage"], "lower-ir");
    for field in ["recipe", "toolchain", "environment", "target_platform"] {
        assert_eq!(
            scope[field], capability[field],
            "scope {field} differs from the production compiler probe"
        );
    }
    let capability = scope;

    // Bind enrollment to a second real compiler-host probe at the worker's own runtime root,
    // under the same cleaned process environment used by trust import. The owner-signed invite is
    // host-trusted because this fixture invokes Cargo/rustc; compare the actual worker admission
    // tuple before importing it rather than manufacturing a pure-parser claim.
    let mut worker_probe = Command::new(env!("CARGO_BIN_EXE_backend-journey-cluster-probe"));
    worker_probe.arg(worker_data.join("compiler-runtime"));
    environment.apply(&mut worker_probe);
    let worker_probe_output =
        run_bounded(worker_probe, "worker compiler capability probe", DEADLINE);
    assert_success(&worker_probe_output, "worker compiler capability probe");
    let worker_capability: Value = serde_json::from_slice(&worker_probe_output.stdout)
        .expect("parse worker compiler execution identity");
    eprintln!(
        "worker compiler probe: profile={} toolchain={} environment={} target_platform={} local_authority_fingerprint={}",
        worker_capability["profile"],
        worker_capability["toolchain"],
        worker_capability["environment"],
        worker_capability["target_platform"],
        worker_capability["local_authority_fingerprint"],
    );
    for field in ["profile", "toolchain", "environment", "target_platform"] {
        assert_eq!(
            worker_capability[field], capability[field],
            "worker's independently probed {field} differs from the owner-signed execution scope"
        );
    }

    let owner_udp = allocate_udp_loopback();
    let worker_udp = allocate_udp_loopback();
    let owner_init = direct_owner_cli(
        &workspace,
        &project,
        &[
            "cluster",
            "owner",
            "init",
            "--bind",
            &owner_udp.to_string(),
            "--advertise",
            &owner_udp.to_string(),
        ],
    );
    assert_success(&owner_init, "private owner provisioning");

    let worker_config = worker_data.join("cluster-owner.worker");
    let namespace_hex = hex(&namespace.namespace_id());
    let recipe = capability["recipe"].as_str().expect("invocation recipe");
    let worker_init = worker_cli(
        &[
            "cluster",
            "init",
            "--config",
            worker_config.to_str().expect("UTF-8 worker config"),
            "--bind",
            &worker_udp.to_string(),
            "--namespace",
            &namespace_hex,
            "--recipe",
            recipe,
        ],
        &environment,
    );
    assert_success(&worker_init, "private worker identity provisioning");

    let identity = worker_cli(
        &[
            "cluster",
            "identity",
            "show",
            "--config",
            worker_config.to_str().expect("UTF-8 worker config"),
        ],
        &environment,
    );
    assert_success(&identity, "worker identity display");
    let identity_text = String::from_utf8(identity.stdout).expect("worker identity UTF-8");
    let worker_peer = identity_text
        .lines()
        .find_map(|line| line.strip_prefix("worker identity fingerprint: "))
        .expect("public worker endpoint identity");
    assert!(identity_text.contains(&format!("direct endpoint: {worker_udp}")));

    let owner_invite = direct_owner_cli(
        &workspace,
        &project,
        &[
            "cluster",
            "invite",
            "create",
            "--worker-peer",
            worker_peer,
            "--worker-address",
            &worker_udp.to_string(),
            "--namespace",
            &namespace_hex,
            "--recipe",
            recipe,
            "--profile",
            environment.profile.as_str(),
            "--stage",
            "lower-ir",
            "--toolchain",
            capability["toolchain"]
                .as_str()
                .expect("toolchain identity"),
            "--environment",
            capability["environment"]
                .as_str()
                .expect("execution environment"),
            "--target-platform",
            capability["target_platform"]
                .as_str()
                .expect("target platform"),
            "--ttl-seconds",
            "900",
        ],
    );
    assert_success(&owner_invite, "owner-signed, scoped worker invite");
    let invite_text = String::from_utf8(owner_invite.stdout).expect("invite output UTF-8");
    let invite = invite_text
        .lines()
        .find_map(|line| line.strip_prefix("Import this one-time token on the worker: "))
        .expect("one-time invite token");
    let decoded_invite = ScopedClusterInvite::decode_token(invite)
        .expect("decode the owner-issued private worker invite");
    eprintln!(
        "owner-issued invite execution class: {:?}",
        decoded_invite.execution_class()
    );
    assert_eq!(
        decoded_invite.execution_class(),
        ClusterExecutionClass::TrustedCoordinatorHostExecution,
        "the source fixture invokes Cargo/rustc and must receive the supported host-execution grant"
    );
    assert_eq!(
        decoded_invite.profile(),
        fixed_hex::<2>(
            worker_capability["profile"]
                .as_str()
                .expect("worker profile identity")
        ),
        "decoded invite profile differs from independently admitted worker profile"
    );
    assert_eq!(
        decoded_invite.toolchain(),
        fixed_hex::<32>(
            worker_capability["toolchain"]
                .as_str()
                .expect("worker toolchain identity")
        ),
        "decoded invite toolchain differs from independently admitted worker toolchain"
    );
    assert_eq!(
        decoded_invite.environment(),
        fixed_hex::<32>(
            worker_capability["environment"]
                .as_str()
                .expect("worker execution environment identity")
        ),
        "decoded invite environment differs from independently admitted worker environment"
    );
    assert_eq!(
        decoded_invite.target_platform(),
        fixed_hex::<32>(
            worker_capability["target_platform"]
                .as_str()
                .expect("worker target platform identity")
        ),
        "decoded invite target platform differs from independently admitted worker platform"
    );
    let fingerprint = invite_text
        .lines()
        .find_map(|line| line.strip_prefix("Fingerprint: "))
        .expect("invite fingerprint");
    let trust_import = worker_cli(
        &[
            "cluster",
            "trust",
            "import",
            "--config",
            worker_config.to_str().expect("UTF-8 worker config"),
            "--data-dir",
            worker_data.to_str().expect("UTF-8 worker data directory"),
            "--invite",
            invite,
            "--fingerprint",
            fingerprint,
        ],
        &environment,
    );
    assert_success(&trust_import, "worker's real private trust import");

    let mut worker = Worker::launch(&worker_config, &worker_data, &environment);
    let mut daemon = Locald::launch(&endpoint, &workspace, &authority_secret, &environment);
    let turso_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build selected-head observer runtime");
    let authority = authority_handle(&turso_runtime, &authority_path);

    // The first Add supplies the owner-local timing sample used by production placement. It must
    // be a local CompilerAttempt and must not manufacture a remote Stored ACK.
    let first_add = run_bounded(
        add_command(&endpoint, &workspace, &project),
        "first CLI Add",
        DEADLINE,
    );
    assert_success(&first_add, "first public Add");
    let first_reply: Value = serde_json::from_slice(&first_add.stdout).expect("first Add JSON");
    assert_eq!(
        first_reply["answer"], "product",
        "Add did not index the source project"
    );
    assert!(
        !worker.wait_for_ack_count(1, Duration::from_secs(1)),
        "the timing-calibration Add unexpectedly ACKed a remote worker result: {:?}",
        worker.seen
    );
    assert!(
        daemon.wait_for_local_fallback_count(1, Duration::from_secs(5)),
        "the first Add did not report its expected owner-local calibration route: {}",
        daemon.diagnostics()
    );
    let first_selected = selected_generation(&turso_runtime, &authority, &namespace);
    assert_selected_head(&first_selected, 1, SelectionOrigin::CompilerAttempt);
    assert_ne!(
        *first_selected.closure_id(),
        [0; 32],
        "Turso selected generation omitted its immutable closure"
    );
    assert!(
        s3.stats().puts > 0,
        "owner did not publish the selected compiler closure through real conditional S3 PUT"
    );
    assert!(
        s3.stored_object().is_some(),
        "loopback S3 did not retain the compiler pack"
    );

    let source_search = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["search", "remote_journey_entry", "--limit", "20"],
    );
    let entry = source_search["records"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|record| record["identity"]["name"] == "remote_journey_entry")
        .unwrap_or_else(|| panic!("CLI search omitted entry declaration: {source_search}"));
    let entry_coordinate = entry["identity"]["coordinate"]
        .as_str()
        .expect("entry declaration coordinate")
        .to_owned();
    let first_page = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["show", &entry_coordinate],
    );
    assert!(
        first_page.to_string().contains("REMOTE_CONFIG_BEFORE"),
        "documentation did not contain the hidden Cargo manifest read: {first_page}"
    );
    let first_graph = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["graph", &entry_coordinate],
    );
    assert!(
        first_graph.to_string().contains("remote_journey_helper"),
        "CLI graph omitted the single-source call edge: {first_graph}"
    );
    let cli_history = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["semantic-versions", &project_label],
    );
    assert!(
        cli_history.to_string().contains("selected"),
        "public semantic history did not expose the selected head: {cli_history}"
    );

    // Cold owner restart must preserve the Turso head and the same user-facing search, document,
    // and graph answers. The Iroh worker remains a separately managed process.
    let first_generation = first_selected.generation();
    let first_root = *first_selected.target_root();
    daemon.kill_now();
    let mut daemon = Locald::launch(&endpoint, &workspace, &authority_secret, &environment);
    let reopened = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(reopened.generation(), first_generation);
    assert_eq!(*reopened.target_root(), first_root);
    assert_same_selected_generation(&first_selected, &reopened);
    let reopened_search = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["search", "remote_journey_entry", "--limit", "20"],
    );
    let reopened_page = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["show", &entry_coordinate],
    );
    let reopened_graph = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["graph", &entry_coordinate],
    );
    assert!(reopened_search.to_string().contains("remote_journey_entry"));
    assert!(reopened_page.to_string().contains("REMOTE_CONFIG_BEFORE"));
    assert!(reopened_graph.to_string().contains("remote_journey_helper"));

    let replies = mcp(
        &endpoint,
        &workspace,
        &project,
        &authority_secret,
        &[
            (
                10,
                json!({"name":"backend.search","arguments":{"query":"remote_journey_entry","limit":20}}),
            ),
            (
                11,
                json!({"name":"backend.document","arguments":{"coordinate":entry_coordinate}}),
            ),
            (
                12,
                json!({"name":"backend.graph","arguments":{"coordinate":entry_coordinate}}),
            ),
        ],
    );
    let mcp_search = mcp_content(&replies[&10], "search");
    let mcp_document = mcp_content(&replies[&11], "document");
    let mcp_graph = mcp_content(&replies[&12], "graph");
    assert!(mcp_search.to_string().contains("remote_journey_entry"));
    assert_eq!(mcp_document, &reopened_page, "MCP and CLI documents differ");
    assert!(mcp_document.to_string().contains("REMOTE_CONFIG_BEFORE"));
    assert!(mcp_graph.to_string().contains("remote_journey_helper"));

    // Changing the manifest changes a hidden compiler input. Renaming a same-sized non-source
    // path makes a previously absent path appear while preserving total capture bytes, artifact
    // count, and compiler source count, so the exact-shape local timing sample remains usable.
    std::fs::write(&cargo_manifest, CARGO_MANIFEST_AFTER)
        .expect("change hidden compiler configuration");
    std::fs::rename(
        project.join(INVENTORY_PATH_BEFORE),
        project.join(INVENTORY_PATH_AFTER),
    )
    .expect("make the previously absent inventory path appear");
    assert_eq!(
        std::fs::metadata(&cargo_manifest)
            .expect("stat changed Cargo manifest")
            .len(),
        calibrated_manifest_size,
        "hidden input mutation changed captured payload byte count"
    );
    assert_eq!(
        std::fs::metadata(project.join(INVENTORY_PATH_AFTER))
            .expect("stat changed inventory entry")
            .len(),
        calibrated_inventory_size,
        "negative-frontier mutation changed captured payload byte count"
    );
    assert_eq!(
        std::fs::read_dir(project.join("src"))
            .expect("read source directory after hidden input edit")
            .count(),
        1,
        "hidden input edit introduced another compiler source"
    );

    // Pause the worker after its real durable `.pending` write and directory fsync, before it can
    // send ResultReceipt. At this cut the owner must have only its durable OfferMayBeSent row and
    // Turso must still expose the local calibration generation.
    let add_thread = {
        let endpoint = endpoint.clone();
        let workspace = workspace.clone();
        let project = project.clone();
        thread::spawn(move || {
            run_bounded(
                background_add_command(&endpoint, &workspace, &project),
                "second CLI Add interrupted before result receipt",
                DEADLINE,
            )
        })
    };
    let offer_marker = wait_for_journey_marker(
        &pause_offer_pending_marker,
        &mut daemon,
        Some(&add_thread),
        5,
        DEADLINE,
    );
    assert_eq!(offer_marker.trim(), "worker-pending-before-result-receipt");
    assert!(
        !add_thread.is_finished(),
        "Add returned before the preselection process barrier"
    );
    let ack_journal_path = workspace.join(PENDING_ACK_PATH);
    let original_offer_journal = assert_offer_may_be_sent_journal(&ack_journal_path);
    assert_eq!(
        count_worker_result_records(&worker_data, "pending"),
        1,
        "worker did not durably retain its result before ResultReceipt"
    );
    let preselection_head = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(preselection_head.generation(), first_selected.generation());
    assert_eq!(*preselection_head.target_root(), first_root);
    assert_eq!(
        *preselection_head.closure_id(),
        *first_selected.closure_id()
    );
    worker.drain_lines();
    assert_eq!(
        worker
            .seen
            .iter()
            .filter(|line| line.contains(TRUSTED_RESULT_ACK))
            .count(),
        0,
        "the worker received an ACK before it had sent its ResultReceipt"
    );
    let stopped_worker = worker.kill_now().expect("worker child process");
    assert!(
        !stopped_worker.success(),
        "worker was not terminated at the preselection barrier"
    );
    daemon.kill_now();
    let interrupted_add = add_thread
        .join()
        .unwrap_or_else(|_| panic!("interrupted public Add panicked"));
    assert!(
        !interrupted_add.status.success(),
        "locald returned success although the worker never sent ResultReceipt"
    );

    // Mutate a single schema-valid capture digest and recompute the journal checksum. Cold owner
    // open must leave the assignment unresolved rather than selecting a worker result from a
    // capture whose source fence no longer matches.
    let mut mutated_offer_journal = original_offer_journal.clone();
    mutate_capture_source_fence_digest(&mut mutated_offer_journal, &project);
    rewrite_durable_file(&ack_journal_path, &mutated_offer_journal);
    let mut daemon = Locald::launch(&endpoint, &workspace, &authority_secret, &environment);
    let mut worker = Worker::launch(&worker_config, &worker_data, &environment);
    thread::sleep(Duration::from_millis(2_300));
    assert_eq!(
        assert_offer_may_be_sent_journal(&ack_journal_path),
        mutated_offer_journal,
        "cold owner changed or resolved a tampered OfferMayBeSent capture"
    );
    let rejected_capture_head = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(
        rejected_capture_head.generation(),
        first_selected.generation()
    );
    assert_eq!(*rejected_capture_head.target_root(), first_root);
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 1);
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 0);
    assert!(
        !worker.wait_for_ack_count(1, Duration::from_millis(50)),
        "owner ACKed a result whose v4 capture digest was tampered"
    );
    let tamper_worker_exit = worker.kill_now().expect("tamper-check worker process");
    assert!(!tamper_worker_exit.success());
    daemon.kill_now();

    // Restore the authentic durable reservation and restart owner before worker. The cold
    // recovery query must recover the exact pending assignment, select its result, and persist the
    // ordinary Stored ACK intent before any ACK reaches the worker.
    rewrite_durable_file(&ack_journal_path, &original_offer_journal);
    let mut daemon = Locald::launch(&endpoint, &workspace, &authority_secret, &environment);
    assert_eq!(
        assert_offer_may_be_sent_journal(&ack_journal_path),
        original_offer_journal
    );
    let mut worker = Worker::launch(&worker_config, &worker_data, &environment);
    let pending_marker = wait_for_journey_marker(
        &recovered_pending_marker,
        &mut daemon,
        None,
        PENDING_ACK_RECOVERED_PENDING_BEFORE_GRANT_PAGES,
        DEADLINE,
    );
    assert_eq!(pending_marker.trim(), "pending-result-before-grant-pages");
    assert_eq!(
        assert_offer_may_be_sent_journal(&ack_journal_path),
        original_offer_journal,
        "owner advanced the offered reservation before retrieving result grant pages"
    );
    let pending_status_head = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(
        pending_status_head.generation(),
        first_selected.generation()
    );
    assert_eq!(*pending_status_head.target_root(), first_root);
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 1);
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 0);
    worker.drain_lines();
    assert_eq!(
        worker
            .seen
            .iter()
            .filter(|line| line.contains(TRUSTED_RESULT_ACK))
            .count(),
        0,
        "owner ACKed before recovered result grant pages were admitted"
    );
    assert!(
        worker.running(),
        "worker exited at recovered Pending barrier"
    );
    daemon.kill_now();

    // The result and its source-bound OfferMayBeSent claim remain durable across another owner
    // crash. Keep the same worker process online and retry the authenticated status/page exchange
    // from a fresh owner. The synced marker makes this barrier one-shot while keeping the
    // worker-granted compiler environment identity byte-for-byte unchanged across restarts.
    assert!(
        worker.running(),
        "worker did not survive owner process restart"
    );
    let mut daemon = Locald::launch(&endpoint, &workspace, &authority_secret, &environment);
    let marker = wait_for_journey_marker(
        &pause_state1_marker,
        &mut daemon,
        None,
        PENDING_ACK_STATE_STORED_PENDING,
        DEADLINE,
    );
    assert_eq!(marker.trim(), "stored-ack-pending");
    let paused_selected = selected_generation(&turso_runtime, &authority, &namespace);
    assert_selected_head(&paused_selected, 2, SelectionOrigin::CompilerAttempt);
    assert_ne!(
        *paused_selected.target_root(),
        first_root,
        "the recovered worker result reused the calibration result"
    );
    assert_ne!(
        *paused_selected.closure_id(),
        *first_selected.closure_id(),
        "Turso selected the calibration closure for the recovered remote result"
    );
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_STORED_PENDING);
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 1);
    worker.drain_lines();
    assert_eq!(
        worker
            .seen
            .iter()
            .filter(|line| line.contains(TRUSTED_RESULT_ACK))
            .count(),
        0,
        "the recovered result ACK preceded retirement intent"
    );
    let stopped_worker = worker.kill_now().expect("recovered worker child process");
    assert!(!stopped_worker.success());
    daemon.kill_now();

    // The exact selection is durable while the Stored ACK is still pending. Revoke the owner's
    // execution grant, then prove that a forged checksum-valid candidate claim cannot use the
    // cleanup-only path. The immutable Turso selected history must still match every journal
    // identity before the owner can send Stored to the original authenticated peer.
    let original_stored_journal = std::fs::read(&ack_journal_path)
        .expect("read exact StoredAckPending row before revocation");
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_STORED_PENDING);
    let revoked = direct_owner_cli(
        &workspace,
        &project,
        &["cluster", "trust", "revoke", "--peer", worker_peer],
    );
    assert_success(&revoked, "revoke owner execution grant before Stored retry");
    for (label, mutation) in [
        ("generation", SelectedClaimMutation::Generation),
        ("candidate", SelectedClaimMutation::Candidate),
        ("selected closure", SelectedClaimMutation::SelectedClosure),
    ] {
        let mut forged_selected_journal = original_stored_journal.clone();
        mutate_selected_claim(
            &mut forged_selected_journal,
            PENDING_ACK_STATE_STORED_PENDING,
            mutation,
        );
        rewrite_durable_file(&ack_journal_path, &forged_selected_journal);
        let mut daemon = Locald::launch(&endpoint, &workspace, &authority_secret, &environment);
        let mut worker = Worker::launch(&worker_config, &worker_data, &environment);
        thread::sleep(Duration::from_millis(2_300));
        assert_eq!(
            std::fs::read(&ack_journal_path).expect("read unresolved forged selected row"),
            forged_selected_journal,
            "a checksum-valid but non-selected {label} claim was rewritten or retired"
        );
        let forged_head = selected_generation(&turso_runtime, &authority, &namespace);
        assert_eq!(forged_head.generation(), paused_selected.generation());
        assert_eq!(*forged_head.closure_id(), *paused_selected.closure_id());
        assert_eq!(count_worker_result_records(&worker_data, "pending"), 1);
        assert_eq!(count_worker_result_records(&worker_data, "retired"), 0);
        assert!(
            !worker.wait_for_ack_count(1, Duration::from_millis(50)),
            "owner sent Stored for a journal {label} that differs from selected history"
        );
        worker.kill_now();
        daemon.kill_now();
    }

    // Restore the authentic proof-bound row. A cold owner now reopens the exact Turso selected
    // history, skips only the revoked execution-grant check for Stored retirement, and keeps the
    // original peer, address, scope, and closure pinned through the authenticated handshake.
    rewrite_durable_file(&ack_journal_path, &original_stored_journal);

    // Selection is independently committed in Turso. Cold owner startup retains the v4 state1
    // row while the worker remains offline, then its live retry completes the terminal handshake.
    let mut daemon = Locald::launch(&endpoint, &workspace, &authority_secret, &environment);
    let recovered_selected = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(
        recovered_selected.generation(),
        paused_selected.generation()
    );
    assert_eq!(
        *recovered_selected.target_root(),
        *paused_selected.target_root()
    );
    assert_eq!(
        recovered_selected.selection_origin(),
        SelectionOrigin::CompilerAttempt
    );
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_STORED_PENDING);

    // Restore the same worker identity and durable store. The journey-only barrier stops after the
    // worker has durably retired the closure but before the owner sends retirement confirmation.
    let mut worker = Worker::launch(&worker_config, &worker_data, &environment);
    let retirement_marker = wait_for_journey_marker(
        &pause_state3_marker,
        &mut daemon,
        None,
        PENDING_ACK_STATE_STORED_AWAITING_CONFIRM,
        DEADLINE,
    );
    assert_eq!(
        retirement_marker.trim(),
        "stored-awaiting-retirement-confirm"
    );
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_STORED_AWAITING_CONFIRM);
    assert_eq!(
        count_worker_result_records(&worker_data, "retired"),
        1,
        "worker did not durably retain its terminal result tombstone before owner confirmation"
    );
    assert!(
        !worker.wait_for_ack_count(1, Duration::from_millis(50)),
        "worker reported terminal ACK before the owner confirmation was applied"
    );
    daemon.release_ack_barrier();
    assert!(
        worker.wait_for_ack_count(1, DEADLINE),
        "selected-history cleanup after trust revocation did not complete the Stored ACK retirement handshake: {:?}",
        worker.seen
    );
    assert_ack_journal_empty(&ack_journal_path);
    assert_eq!(
        count_worker_result_records(&worker_data, "retired"),
        0,
        "worker retained a terminal tombstone after Applied"
    );
    assert_eq!(
        count_worker_result_records(&worker_data, "pending"),
        0,
        "worker retained result replay bytes after Applied"
    );
    assert_eq!(
        count_worker_result_records(&worker_data, "running"),
        0,
        "worker retained a running assignment after Applied"
    );
    assert!(
        worker.running(),
        "worker stopped instead of returning to its offer loop after Applied"
    );
    let pending = run_pending_results(&worker_config, &worker_data, &environment);
    assert!(
        pending.contains("no retained result closures are awaiting owner ACK"),
        "worker did not remove the retained result after owner recovery: {pending}"
    );
    let restored_trust = direct_owner_cli(
        &workspace,
        &project,
        &[
            "cluster",
            "trust",
            "add",
            "--peer",
            worker_peer,
            "--address",
            &worker_udp.to_string(),
            "--namespace",
            &namespace_hex,
            "--recipe",
            recipe,
            "--profile",
            &environment.profile,
            "--stage",
            "lower-ir",
            "--toolchain",
            capability["toolchain"]
                .as_str()
                .expect("toolchain identity"),
            "--environment",
            capability["environment"]
                .as_str()
                .expect("execution environment"),
            "--target-platform",
            capability["target_platform"]
                .as_str()
                .expect("target platform"),
        ],
    );
    assert_success(
        &restored_trust,
        "restore worker execution grant after cleanup test",
    );
    daemon.assert_no_local_fallback();

    let after_replay_page = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["show", &entry_coordinate],
    );
    assert!(
        after_replay_page
            .to_string()
            .contains("REMOTE_CONFIG_AFTER"),
        "cold recovery lost the compiler's hidden manifest input: {after_replay_page}"
    );
    let after_replay_search = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["search", "remote_journey_entry", "--limit", "20"],
    );
    let after_replay_graph = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["graph", &entry_coordinate],
    );
    assert!(
        after_replay_search
            .to_string()
            .contains("remote_journey_entry")
    );
    assert!(
        after_replay_graph
            .to_string()
            .contains("remote_journey_helper")
    );

    // Exercise the next fsync boundary: the owner has durably admitted the exact worker result,
    // but Turso's compare-and-select has not run. After cold restart the owner must revalidate
    // that v4 source-root-bound capture and select the pinned local CAS result without a new Add
    // or an online worker.
    std::fs::remove_file(&pause_state1_marker).expect("reset Stored-intent test barrier");
    std::fs::remove_file(&pause_state3_marker).expect("reset retirement-confirm test barrier");
    let selection_add_thread = {
        let endpoint = endpoint.clone();
        let workspace = workspace.clone();
        let project = project.clone();
        thread::spawn(move || {
            run_bounded(
                background_add_command(&endpoint, &workspace, &project),
                "third CLI Add interrupted before Turso selection",
                DEADLINE,
            )
        })
    };
    let selection_marker = wait_for_journey_marker(
        &pause_state0_marker,
        &mut daemon,
        Some(&selection_add_thread),
        PENDING_ACK_STATE_AWAITING_SELECTION,
        DEADLINE,
    );
    assert_eq!(selection_marker.trim(), "awaiting-selection");
    let generation_before_selection_recovery =
        selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(generation_before_selection_recovery.generation(), 2);
    assert_eq!(
        *generation_before_selection_recovery.closure_id(),
        *recovered_selected.closure_id(),
        "Turso advanced before the owner selection boundary"
    );
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 1);
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_AWAITING_SELECTION);
    assert!(
        s3.object_count() >= 2,
        "the selected history and pending worker result did not both reach the real S3 store"
    );
    let stopped_worker = worker
        .kill_now()
        .expect("worker at AwaitingSelection barrier");
    assert!(!stopped_worker.success());
    daemon.kill_now();
    let interrupted_selection_add = selection_add_thread
        .join()
        .unwrap_or_else(|_| panic!("interrupted selection Add panicked"));
    assert!(!interrupted_selection_add.status.success());

    // Keep the row schema-valid but alter both copies of the worker closure identity, then
    // recompute the v4 checksum. The resulting claim names no local closure root. With the
    // remote-segment GC restart enabled, the ordinary retention pass must fail before either
    // collection pass sweeps the authentic pending result or selected head.
    let original_selection_journal = std::fs::read(&ack_journal_path)
        .expect("read durable AwaitingSelection intent before mutation");
    selection_journal_entry(
        &original_selection_journal,
        PENDING_ACK_STATE_AWAITING_SELECTION,
    );
    let mut mutated_selection_journal = original_selection_journal.clone();
    let (authentic_worker_closure, absent_worker_closure) =
        mutate_selection_worker_closure(&mut mutated_selection_journal);
    let selected_before_gc = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(selected_before_gc.generation(), 2);
    let store = FileStore::open(workspace.join("semantic-objects"), 512 * 1024 * 1024)
        .expect("open semantic CAS before checksum-valid closure fault");
    assert_closure_objects_present(&store, authentic_worker_closure, "pending worker");
    assert_closure_objects_present(&store, *selected_before_gc.closure_id(), "selected head");
    assert!(
        store
            .open_closure_claim(ArtifactClosureClaim::from_bytes(absent_worker_closure))
            .is_err(),
        "mutated worker closure unexpectedly names an existing CAS root"
    );
    let closures_before_gc = semantic_cas_inventory(&workspace, "closures");
    let objects_before_gc = semantic_cas_inventory(&workspace, "objects");
    assert!(
        !closures_before_gc.is_empty(),
        "semantic CAS has no closure roots"
    );
    assert!(!objects_before_gc.is_empty(), "semantic CAS has no objects");
    drop(store);
    rewrite_durable_file(&ack_journal_path, &mutated_selection_journal);
    let gc_rejection =
        Locald::rejected_gc_startup(&endpoint, &workspace, &authority_secret, &environment);
    assert!(
        gc_rejection.contains("reconcile semantic authority"),
        "owner did not reject the missing worker root during GC reconciliation: {gc_rejection}"
    );
    assert_eq!(
        std::fs::read(&ack_journal_path).expect("read unresolved mutated selection intent"),
        mutated_selection_journal,
        "GC recovery advanced or rewrote an AwaitingSelection row with a missing worker closure claim"
    );
    assert_eq!(
        semantic_cas_inventory(&workspace, "closures"),
        closures_before_gc,
        "failed ordinary GC changed semantic closure roots before rejecting the pending claim"
    );
    assert_eq!(
        semantic_cas_inventory(&workspace, "objects"),
        objects_before_gc,
        "failed ordinary GC swept semantic objects before rejecting the pending claim"
    );
    let store = FileStore::open(workspace.join("semantic-objects"), 512 * 1024 * 1024)
        .expect("reopen semantic CAS after checksum-valid closure fault");
    assert_closure_objects_present(&store, authentic_worker_closure, "pending worker");
    assert_closure_objects_present(&store, *selected_before_gc.closure_id(), "selected head");
    drop(store);
    let rejected_selection_head = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(rejected_selection_head.generation(), 2);
    assert_eq!(
        *rejected_selection_head.closure_id(),
        *recovered_selected.closure_id(),
        "an absent worker closure claim advanced Turso's selected head"
    );
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 1);
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 0);
    assert!(
        !pause_state1_marker.is_file(),
        "owner reached Stored ACK intent for an absent worker closure claim"
    );

    // Restore the authentic AwaitingSelection bytes and restart owner, still without a worker.
    // Both GC passes and retry recovery must now proceed from the verified pinned CAS result.
    rewrite_durable_file(&ack_journal_path, &original_selection_journal);
    let mut daemon = Locald::launch_with_remote_segment_gc(
        &endpoint,
        &workspace,
        &authority_secret,
        &environment,
        true,
    );
    assert_eq!(
        std::fs::read(&ack_journal_path).expect("read restored selection intent"),
        original_selection_journal
    );
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_AWAITING_SELECTION);
    let selection_recovery_marker = wait_for_journey_marker(
        &pause_state1_marker,
        &mut daemon,
        None,
        PENDING_ACK_STATE_STORED_PENDING,
        DEADLINE,
    );
    assert_eq!(selection_recovery_marker.trim(), "stored-ack-pending");
    let selection_recovered_head = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(selection_recovered_head.generation(), 3);
    assert_eq!(
        selection_recovered_head.selection_origin(),
        SelectionOrigin::CompilerAttempt
    );
    assert_ne!(
        *selection_recovered_head.closure_id(),
        *recovered_selected.closure_id(),
        "AwaitingSelection recovery reused the preceding compiler result"
    );
    assert_ne!(
        *selection_recovered_head.candidate_id(),
        [0; 32],
        "recovered selection omitted its exact candidate identity"
    );
    assert_ne!(
        *selection_recovered_head.target_root(),
        [0; 32],
        "recovered selection omitted its exact logical root"
    );
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_STORED_PENDING);
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 1);
    daemon.kill_now();

    // Cold-restart once more with the selected head and Stored intent durable, then bring the
    // worker back. It must complete the same retirement-confirm/Applied protocol as the first
    // recovered result, removing its `.pending` and `.retired` records only at terminal ACK.
    let mut daemon = Locald::launch_with_remote_segment_gc(
        &endpoint,
        &workspace,
        &authority_secret,
        &environment,
        true,
    );
    let final_reopened_head = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(final_reopened_head.generation(), 3);
    assert_eq!(
        *final_reopened_head.closure_id(),
        *selection_recovered_head.closure_id()
    );
    assert_same_selected_generation(&selection_recovered_head, &final_reopened_head);
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_STORED_PENDING);
    let mut worker = Worker::launch(&worker_config, &worker_data, &environment);
    let final_retirement_marker = wait_for_journey_marker(
        &pause_state3_marker,
        &mut daemon,
        None,
        PENDING_ACK_STATE_STORED_AWAITING_CONFIRM,
        DEADLINE,
    );
    assert_eq!(
        final_retirement_marker.trim(),
        "stored-awaiting-retirement-confirm"
    );
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 1);
    assert!(
        !worker.wait_for_ack_count(1, Duration::from_millis(50)),
        "worker reported Applied before final owner confirmation"
    );
    daemon.release_ack_barrier();
    assert!(
        worker.wait_for_ack_count(1, DEADLINE),
        "AwaitingSelection recovery did not finish its Applied handshake: {:?}",
        worker.seen
    );
    assert_ack_journal_empty(&ack_journal_path);
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 0);
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 0);
    assert_eq!(count_worker_result_records(&worker_data, "running"), 0);

    // A live owner can fail after authenticating a receipt but before receiving any grant page.
    // The durable OfferMayBeSent row remains retryable; that transient local receive failure must
    // not be rewritten as a terminal rejection. The same worker stays online across owner restart.
    std::fs::remove_file(&pause_state1_marker).expect("reset live-result Stored-intent barrier");
    std::fs::remove_file(&pause_state3_marker).expect("reset live-result retirement barrier");
    std::fs::remove_file(&live_receipt_marker).expect("enable live-result receipt process barrier");
    std::fs::write(&cargo_manifest, CARGO_MANIFEST_LIVE)
        .expect("change hidden compiler input for live receipt retry");
    worker.drain_lines();
    let acknowledgements_before_live_fault = worker
        .seen
        .iter()
        .filter(|line| line.contains(TRUSTED_RESULT_ACK))
        .count();
    let live_add_thread = {
        let endpoint = endpoint.clone();
        let workspace = workspace.clone();
        let project = project.clone();
        thread::spawn(move || {
            run_bounded(
                background_add_command(&endpoint, &workspace, &project),
                "fourth CLI Add interrupted after authenticated result receipt",
                DEADLINE,
            )
        })
    };
    let live_receipt_pause = wait_for_journey_marker(
        &live_receipt_marker,
        &mut daemon,
        Some(&live_add_thread),
        PENDING_ACK_LIVE_RECEIPT_BEFORE_GRANT_PAGES,
        DEADLINE,
    );
    assert_eq!(
        live_receipt_pause.trim(),
        "live-result-receipt-before-grant-pages"
    );
    let live_offer_journal =
        std::fs::read(&ack_journal_path).expect("read durable live-result OfferMayBeSent row");
    let _ = offer_reservation_entry(&live_offer_journal);
    assert_eq!(
        count_worker_result_records(&worker_data, "pending"),
        1,
        "worker lost the output while the owner was receiving result grants"
    );
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 0);
    let before_live_selection = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(before_live_selection.generation(), 3);
    assert_eq!(
        *before_live_selection.closure_id(),
        *selection_recovered_head.closure_id(),
        "Turso selected before live result grant pages were received"
    );
    worker.drain_lines();
    assert_eq!(
        worker
            .seen
            .iter()
            .filter(|line| line.contains(TRUSTED_RESULT_ACK))
            .count(),
        acknowledgements_before_live_fault,
        "worker received a terminal result ACK before the live result was admitted"
    );
    let retained_before_kill = count_worker_result_records(&worker_data, "pending");
    daemon.kill_now();
    let interrupted_live_add = live_add_thread
        .join()
        .unwrap_or_else(|_| panic!("live receipt Add panicked"));
    assert!(
        !interrupted_live_add.status.success(),
        "live Add succeeded after its owner died before the first result grant page"
    );
    assert_eq!(
        std::fs::read(&ack_journal_path).expect("read row after live receive interruption"),
        live_offer_journal,
        "transient local receive interruption terminally rejected or rewrote the result"
    );
    assert_eq!(
        count_worker_result_records(&worker_data, "pending"),
        retained_before_kill
    );
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 0);
    let after_live_fault = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(after_live_fault.generation(), 3);
    assert_eq!(
        *after_live_fault.closure_id(),
        *selection_recovered_head.closure_id()
    );
    assert!(
        worker.running(),
        "worker exited with a retained pending result"
    );

    // The recovered Pending path requests the exact same durable result. The preexisting live
    // marker prevents a second block while preserving the compiler environment and trust grant.
    let mut daemon = Locald::launch(&endpoint, &workspace, &authority_secret, &environment);
    let live_stored_marker = wait_for_journey_marker(
        &pause_state1_marker,
        &mut daemon,
        None,
        PENDING_ACK_STATE_STORED_PENDING,
        DEADLINE,
    );
    assert_eq!(live_stored_marker.trim(), "stored-ack-pending");
    let live_selected = selected_generation(&turso_runtime, &authority, &namespace);
    assert_selected_head(&live_selected, 4, SelectionOrigin::CompilerAttempt);
    assert_ne!(
        *live_selected.closure_id(),
        *selection_recovered_head.closure_id(),
        "retry selected an earlier compiler result instead of the live receipt"
    );
    assert_ack_journal_state(&ack_journal_path, PENDING_ACK_STATE_STORED_PENDING);
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 1);
    worker.drain_lines();
    assert_eq!(
        worker
            .seen
            .iter()
            .filter(|line| line.contains(TRUSTED_RESULT_ACK))
            .count(),
        acknowledgements_before_live_fault,
        "worker ACK arrived before the durable Stored intent"
    );

    let live_retirement_marker = wait_for_journey_marker(
        &pause_state3_marker,
        &mut daemon,
        None,
        PENDING_ACK_STATE_STORED_AWAITING_CONFIRM,
        DEADLINE,
    );
    assert_eq!(
        live_retirement_marker.trim(),
        "stored-awaiting-retirement-confirm"
    );
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 1);
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 0);
    assert!(
        !worker.wait_for_ack_count(
            acknowledgements_before_live_fault + 1,
            Duration::from_millis(50)
        ),
        "worker reported Applied before the live result retirement confirmation"
    );
    daemon.release_ack_barrier();
    assert!(
        worker.wait_for_ack_count(acknowledgements_before_live_fault + 1, DEADLINE),
        "live receipt recovery did not complete the four-frame retirement: {:?}",
        worker.seen
    );
    assert_ack_journal_empty(&ack_journal_path);
    assert_eq!(count_worker_result_records(&worker_data, "pending"), 0);
    assert_eq!(count_worker_result_records(&worker_data, "retired"), 0);
    assert_eq!(count_worker_result_records(&worker_data, "running"), 0);
    let final_head = selected_generation(&turso_runtime, &authority, &namespace);
    assert_eq!(final_head.generation(), 4);
    assert_eq!(*final_head.closure_id(), *live_selected.closure_id());
    hydrate_remote_selected_core_with_restart(
        &root,
        &endpoint,
        &workspace,
        &authority_secret,
        &environment,
        &project_label,
        &coordinate,
        &final_head,
        &mut daemon,
        &s3,
    );
    let live_page = cli_json(
        &endpoint,
        &workspace,
        &project,
        &["show", &entry_coordinate],
    );
    assert!(
        live_page.to_string().contains("REMOTE_CONFIG_LIVE"),
        "CLI did not read the hidden manifest compiled after transient receive recovery: {live_page}"
    );
    let live_replies = mcp(
        &endpoint,
        &workspace,
        &project,
        &authority_secret,
        &[
            (
                20,
                json!({"name":"backend.search","arguments":{"query":"remote_journey_entry","limit":20}}),
            ),
            (
                21,
                json!({"name":"backend.document","arguments":{"coordinate":entry_coordinate}}),
            ),
            (
                22,
                json!({"name":"backend.graph","arguments":{"coordinate":entry_coordinate}}),
            ),
        ],
    );
    let live_mcp_search = mcp_content(&live_replies[&20], "post-recovery search");
    let live_mcp_document = mcp_content(&live_replies[&21], "post-recovery document");
    let live_mcp_graph = mcp_content(&live_replies[&22], "post-recovery graph");
    assert!(live_mcp_search.to_string().contains("remote_journey_entry"));
    assert_eq!(
        live_mcp_document, &live_page,
        "CLI and MCP documents differ after replay"
    );
    assert!(live_mcp_document.to_string().contains("REMOTE_CONFIG_LIVE"));
    assert!(live_mcp_graph.to_string().contains("remote_journey_helper"));
    daemon.assert_no_local_fallback();
}
