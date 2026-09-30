//! Cold locald process journey through exact selected S3 segment residency.

#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![allow(clippy::too_many_lines)]

use backend_client::LocalSemanticIndexClient;
use backend_engine::package_key;
use backend_extension_turso::{
    AuthorityNamespace, SelectedGeneration, TursoAuthority, reopen_selected_compiler_metadata,
};
use backend_replication::{
    FileSemanticRangeStore, HydrationCredits, IrHydrationPoll, SemanticRangeClientProgress,
    SemanticTargetKey, TransportLimits,
};
use backend_semantic::ir::{
    SemanticIrPlane, SemanticManifestRoot, SemanticPlaneImageKey, SemanticPlaneKind,
    SemanticPlaneRoot,
};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
use backend_store::{FileStore, ObjectId, UntrustedObjectId};
use backend_store_s3::test_support::LoopbackS3;
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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
    let local_pin = local_store
        .pin_garbage_collection()
        .expect("pin selected members during admission checks");
    for member in &selected_members {
        assert!(
            local_store
                .contains_object(ObjectId::from_bytes(*member))
                .expect("inspect local selected member before cold restart"),
            "selected closure member was not local before publication"
        );
        let expected_id = ObjectId::from_bytes(*member);
        local_store
            .with_verified_object_claim_pinned(
                &local_pin,
                UntrustedObjectId::from_bytes(*member),
                |verified| {
                    assert_eq!(verified.id(), expected_id);
                    Ok(())
                },
            )
            .expect("admit local selected member before cold restart");
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
    let cold_pin = cold_store
        .pin_garbage_collection()
        .expect("pin selected members during cold admission checks");
    let mut locally_present_members = 0usize;
    let mut remotely_resident_members = 0usize;
    for member in &selected_members {
        let expected_id = ObjectId::from_bytes(*member);
        let present = cold_store
            .contains_object(expected_id)
            .expect("inspect selected closure member after restart");
        assert_eq!(
            present,
            s3.is_none(),
            "selected plane segment residency did not match the configured storage route"
        );
        if present {
            locally_present_members += 1;
            cold_store
                .with_verified_object_claim_pinned(
                    &cold_pin,
                    UntrustedObjectId::from_bytes(*member),
                    |verified| {
                        assert_eq!(verified.id(), expected_id);
                        Ok(())
                    },
                )
                .expect("admit selected closure member after restart");
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
        assert!(
            cold_store
                .contains_object(ObjectId::from_bytes(*member))
                .expect("inspect locally retained metadata/image after restart"),
            "selected envelope, compiler metadata, or semantic image was evicted"
        );
        let expected_id = ObjectId::from_bytes(*member);
        cold_store
            .with_verified_object_claim_pinned(
                &cold_pin,
                UntrustedObjectId::from_bytes(*member),
                |verified| {
                    assert_eq!(verified.id(), expected_id);
                    Ok(())
                },
            )
            .expect("admit locally retained metadata/image after restart");
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
        project_label,
        coordinate,
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
    std::fs::write(
        project.join("src/lib.rs"),
        "#[doc = include_str!(\"../Cargo.toml\")]\npub fn cold_s3_helper() -> u32 { 41 }\npub fn cold_s3_entry() -> u32 { cold_s3_helper() + 1 }\n",
    )
    .expect("write source file");
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
