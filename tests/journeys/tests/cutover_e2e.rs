//! Process-level cutover journeys.
//!
//! These tests deliberately cross the Unix process boundary. They launch the
//! compiled adapters, negotiate the canonical framed protocol, and use bounded
//! deadlines instead of sleeps. The cancellation journey uses one explicitly
//! named fixture worker whose pure recipe injects a deterministic cooperative
//! wait; the Product journeys launch the compiled production worker and locald
//! profiles.
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[cfg(unix)]
mod unix_journeys {
    use backend_compile::{
        Authority, Coverage, FlowSchema, InputContentSchema, InputKind, ProfileSchema,
        SemanticBasisSchema, SessionKey, SourceExcerptExtent, SyntaxError, SyntaxFrontend,
        typed_of,
    };
    use backend_desktop::Model as DesktopModel;
    use backend_engine::{
        AttemptId, AuthorityEpoch, AuthorityVersion, CancelAttempt, CancellationId,
        CapabilityManifest, ExecutionScopeId, Fence, ResourceEnvelope, TransportLimits,
        TransportMessage, WireAuthorityPolicy, WireIdentity, WireRecipeRequest, WireRecipeResult,
    };
    use backend_execution::{
        AuthorityVersionSchema, ReadManifestId, ReadManifestSchema, RecipeId, RecipeSchema,
        VersionedWorkIdentity, WorkKeySchema,
    };
    use backend_library::{
        CapabilityFamily, CapabilityLifecycle, Command, CommandDto, CommandReply,
        LanguageOracleTask, PageTerminal, ReplyDto, ViewRoot, WireCertificate, WireClaim,
        WireSchema,
    };
    use backend_replication::{RecipeCapability, SchemaDescriptor, VersionRange};
    use backend_version::{
        AuthorityScopeClaim, CoverageWitness, ObjectClosure, ProducerObservationClaims,
        ProducerObservationVerifier, Relation, RelationBinding, RelationState,
        UntrustedProducerObservation, WorkspaceManifest, admit_complete_scope,
        admit_producer_observation,
    };
    use backend_worker::{WorkerLimits, read_message, write_message};
    use backend_semantic::vocabulary::LanguageProfile;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::num::NonZeroU64;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command as ProcessCommand, ExitStatus, Output, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    const DEADLINE: Duration = Duration::from_secs(12);
    const LEGACY_SCOPE_ONE: NonZeroU64 = NonZeroU64::MIN;
    const RECIPE_BYTES: &[u8] = b"backend.worker.builtin.echo.v1";
    const READ_BYTES: &[u8] = b"backend.worker.builtin.echo.reads.v1";
    const AUTHORITY_BYTES: &[u8] = b"backend.worker.builtin.echo.authority.v1";
    const EQUIVALENCE_BYTES: &[u8] = b"backend.worker.builtin.echo.equivalence.v1";
    const OUTPUT_BYTES: &[u8] = b"backend.worker.builtin.echo.output.v1";
    const CANCEL_RECIPE_BYTES: &[u8] = b"backend.journey.cancel.recipe.v1";
    const CANCEL_READ_BYTES: &[u8] = b"backend.journey.cancel.reads.v1";
    const CANCEL_AUTHORITY_BYTES: &[u8] = b"backend.journey.cancel.authority.v1";
    const CANCEL_EQUIVALENCE_BYTES: &[u8] = b"backend.journey.cancel.equivalence.v1";
    const CANCEL_START_ENV: &str = "BACKEND_JOURNEY_STARTED";
    const CANCELLED_ENV: &str = "BACKEND_JOURNEY_CANCELLED";
    const PRODUCT_AUTHORITY_SECRET: [u8; 32] = [0x5a; 32];

    fn assert_syntax_baseline(
        frontend: SyntaxFrontend,
        path: &Path,
        source: &[u8],
        declaration: &str,
    ) {
        assert!(
            frontend.supports_path(path),
            "{} syntax frontend did not claim {}",
            frontend.language().name(),
            path.display()
        );
        let analysis = frontend
            .analyze(path, source)
            .unwrap_or_else(|error| panic!("analyze {}: {error}", path.display()));
        assert_eq!(analysis.language(), frontend.language());
        assert_eq!(analysis.content(), typed_of::<InputContentSchema>(source));
        assert_eq!(analysis.producer(), frontend.producer());
        let declaration_fact = analysis.declarations().iter().find(|fact| {
            fact.name() == declaration && fact.location().path() == path.to_string_lossy().as_ref()
        });
        assert!(
            declaration_fact.is_some(),
            "{} syntax facts omitted {declaration}: {:?}",
            frontend.language().name(),
            analysis.declarations()
        );
        let excerpt = declaration_fact
            .expect("checked declaration")
            .source_excerpt();
        assert!(
            excerpt
                .text()
                .is_some_and(|text| text.contains(declaration)),
            "{} did not retain real declaration source",
            frontend.language().name()
        );
        assert_eq!(excerpt.extent(), Some(SourceExcerptExtent::Complete));
    }

    fn assert_malformed_source_is_terminal() {
        let frontend =
            backend_frontend_typescript::syntax_frontend().expect("TypeScript syntax frontend");
        assert!(matches!(
            frontend.analyze(Path::new("broken.ts"), b"export function {"),
            Err(SyntaxError::MalformedSource(
                backend_compile::SourceLanguage::TypeScript
            ))
        ));
    }

    fn assert_manifest_only_authority<A: Authority>(
        authority: &A,
        source_name: &str,
        source: &[u8],
        expected: Coverage,
    ) {
        let snapshot = authority
            .discover()
            .unwrap_or_else(|error| panic!("discover {source_name}: {error}"));
        let rediscovered = authority
            .discover()
            .unwrap_or_else(|error| panic!("rediscover {source_name}: {error}"));
        assert_eq!(
            snapshot.manifest().digest(),
            rediscovered.manifest().digest(),
            "unchanged discovery was not content stable for {source_name}"
        );
        let source_input = snapshot
            .manifest()
            .get(InputKind::Source, source_name)
            .unwrap_or_else(|| panic!("{source_name} missing from authority manifest"));
        assert!(source_input.is_present());
        assert_eq!(
            source_input.digest(),
            typed_of::<InputContentSchema>(source),
            "manifest did not bind exact fixture bytes for {source_name}"
        );
        assert!(
            snapshot
                .manifest()
                .get(InputKind::Configuration, "semantic-fact-schema")
                .is_some_and(backend_compile::Input::is_present),
            "{source_name} omitted its semantic schema dependency"
        );
        let key = SessionKey::new(
            authority.identity(),
            snapshot.manifest(),
            typed_of::<ProfileSchema>(b"polyglot-cutover-profile"),
            typed_of::<FlowSchema>(b"polyglot-cutover-flow"),
            typed_of::<SemanticBasisSchema>(b"polyglot-cutover-basis"),
        );
        let extraction = authority
            .extract(&snapshot, key)
            .unwrap_or_else(|error| panic!("extract {source_name}: {error}"));
        assert_eq!(
            extraction.coverage().state(),
            expected,
            "frontend reported the wrong native semantic availability for {source_name}"
        );
        assert!(extraction.records().is_empty() && extraction.facts().is_empty());
    }

    struct ProcessCoverageVerifier(backend_version::ScopeRoot);

    impl ProducerObservationVerifier for ProcessCoverageVerifier {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            let scope = self.0;
            let expected_identity = *scope.as_bytes();
            let expected_evidence = scope.as_bytes();
            if observation.producer_identity() != expected_identity
                || observation.context() != expected_identity
                || observation.scope_root() != scope
                || observation.evidence() != expected_evidence
            {
                return Err("invalid process journey producer observation");
            }
            Ok(ProducerObservationClaims::new(
                expected_identity,
                scope,
                expected_identity,
                *blake3::hash(expected_evidence).as_bytes(),
            ))
        }
    }

    fn complete_scope(authority: AuthorityVersion) -> CoverageWitness {
        let declared = AuthorityScopeClaim::from_object_version(authority);
        let scope = declared.scope_root();
        let producer = admit_producer_observation(
            UntrustedProducerObservation::new(
                *scope.as_bytes(),
                scope,
                *scope.as_bytes(),
                scope.as_bytes().to_vec(),
            ),
            &ProcessCoverageVerifier(scope),
        )
        .unwrap_or_else(|error| panic!("admit process producer observation: {error}"));
        CoverageWitness::Complete(
            admit_complete_scope(declared, producer)
                .unwrap_or_else(|error| panic!("admit process scope: {error}")),
        )
    }

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);
    static PROCESS_JOURNEY_LEASE: Mutex<()> = Mutex::new(());

    fn process_journey_lease() -> std::sync::MutexGuard<'static, ()> {
        PROCESS_JOURNEY_LEASE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct ChildGuard {
        child: Option<Child>,
        stderr: Arc<Mutex<Vec<u8>>>,
        stderr_done: Arc<AtomicBool>,
    }

    impl ChildGuard {
        fn spawn(name: &str, args: &[OsString], env: &[(&str, &Path)]) -> Self {
            let mut command = ProcessCommand::new(binary(name));
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                // Retain child diagnostics so a process-boundary failure
                // reports the exact startup terminal alongside its status.
                .stderr(Stdio::piped());
            for (key, value) in env {
                command.env(key, value);
            }
            let child = command
                .spawn()
                .unwrap_or_else(|error| panic!("spawn {name}: {error}"));
            let mut child = child;
            let stderr = Arc::new(Mutex::new(Vec::new()));
            let stderr_done = Arc::new(AtomicBool::new(false));
            if let Some(mut stream) = child.stderr.take() {
                let captured = Arc::clone(&stderr);
                let done = Arc::clone(&stderr_done);
                thread::spawn(move || {
                    let mut bytes = [0_u8; 4096];
                    loop {
                        match stream.read(&mut bytes) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => {
                                if let Ok(mut retained) = captured.lock() {
                                    retained.extend_from_slice(&bytes[..read]);
                                }
                            }
                        }
                    }
                    done.store(true, Ordering::Release);
                });
            }
            Self {
                child: Some(child),
                stderr,
                stderr_done,
            }
        }

        fn try_wait(&mut self) -> Option<ExitStatus> {
            self.child
                .as_mut()
                .and_then(|child| child.try_wait().unwrap_or(None))
        }

        fn is_running(&mut self) -> bool {
            self.try_wait().is_none()
        }

        fn exit_diagnostic(&mut self) -> Option<String> {
            let status = self.try_wait()?;
            // `try_wait` can observe the child exit before the stderr reader
            // gets scheduled. Give it a small bounded window to drain the
            // startup terminal, while still tolerating descendants that keep
            // the pipe open.
            let deadline = Instant::now() + Duration::from_millis(100);
            while !self.stderr_done.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            let stderr = self
                .stderr
                .lock()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|_| "<stderr lock poisoned>".to_owned());
            Some(format!("status={status}, stderr={stderr:?}"))
        }

        fn stderr_snapshot(&self) -> String {
            self.stderr
                .lock()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|_| "<stderr lock poisoned>".to_owned())
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let Some(child) = self.child.as_mut() else {
                return;
            };
            if child.try_wait().unwrap_or(None).is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }

    fn binary(name: &str) -> PathBuf {
        match name {
            "backend-locald" => PathBuf::from(env!("CARGO_BIN_EXE_backend-journey-locald")),
            "backend-worker" => PathBuf::from(env!("CARGO_BIN_EXE_backend-journey-worker")),
            "backend-cli" => PathBuf::from(env!("CARGO_BIN_EXE_backend-journey-cli")),
            "backend-mcp" => PathBuf::from(env!("CARGO_BIN_EXE_backend-journey-mcp")),
            "backend-journey-cancel-worker" => {
                PathBuf::from(env!("CARGO_BIN_EXE_backend-journey-cancel-worker"))
            }
            other => panic!("journey does not own a process wrapper for {other}"),
        }
    }

    fn bounded_command(command: ProcessCommand, label: &str, input: Option<&[u8]>) -> Output {
        bounded_command_for(command, label, input, DEADLINE)
    }

    fn bounded_command_for(
        mut command: ProcessCommand,
        label: &str,
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Output {
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
                .unwrap_or_else(|| panic!("{label} stdin"))
                .write_all(input)
                .unwrap_or_else(|error| panic!("write {label} stdin: {error}"));
        }
        let mut stdout = child
            .stdout
            .take()
            .unwrap_or_else(|| panic!("{label} stdout"));
        let mut stderr = child
            .stderr
            .take()
            .unwrap_or_else(|| panic!("{label} stderr"));
        let stdout_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .read_to_end(&mut bytes)
                .unwrap_or_else(|error| panic!("read command stdout: {error}"));
            bytes
        });
        let stderr_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr
                .read_to_end(&mut bytes)
                .unwrap_or_else(|error| panic!("read command stderr: {error}"));
            bytes
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::yield_now(),
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
        let stdout = stdout_reader
            .join()
            .unwrap_or_else(|_| panic!("{label} stdout reader"));
        let stderr = stderr_reader
            .join()
            .unwrap_or_else(|_| panic!("{label} stderr reader"));
        Output {
            status,
            stdout,
            stderr,
        }
    }

    fn unique_directory(label: &str) -> PathBuf {
        let serial = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let leaf = format!("backend-v2-{label}-{}-{serial}", std::process::id());
        let candidate = std::env::temp_dir().join(&leaf);
        let path =
            if backend_engine::UnixEndpointPath::new(candidate.join("worker-proxy.sock")).is_ok() {
                candidate
            } else {
                Path::new("/tmp").join(leaf)
            };
        backend_engine::UnixEndpointPath::new(path.join("worker-proxy.sock"))
            .unwrap_or_else(|error| panic!("allocate bounded endpoint directory: {error}"));
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|error| panic!("create temporary directory: {error}"));
        path
    }

    fn authority_secret_file(root: &Path) -> PathBuf {
        let path = root.join("authority.secret");
        std::fs::write(&path, PRODUCT_AUTHORITY_SECRET)
            .unwrap_or_else(|error| panic!("write authority credential: {error}"));
        let mut permissions = std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("stat authority credential: {error}"))
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&path, permissions)
            .unwrap_or_else(|error| panic!("protect authority credential: {error}"));
        path
    }

    struct HangingGoProbe {
        root: PathBuf,
        executable: PathBuf,
        started: PathBuf,
    }

    impl HangingGoProbe {
        fn new() -> Self {
            let root = unique_directory("go-probe");
            let executable = root.join("go");
            let started = PathBuf::from(format!("{}.started", executable.display()));
            // This fixture is intentionally parent-aware.  It keeps the
            // version probe alive while locald is alive, then exits as soon
            // as the bounded process guard tears locald down.  No ambient Go
            // executable is consulted and the fixture cannot outlive the
            // journey after its owner is reaped.
            std::fs::write(
                &executable,
                "#!/bin/sh\n: > \"$0.started\"\nparent=\"$PPID\"\nwhile kill -0 \"$parent\" 2>/dev/null; do sleep 0.02; done\nexit 1\n",
            )
            .unwrap_or_else(|error| panic!("write hanging Go probe: {error}"));
            let mut permissions = std::fs::metadata(&executable)
                .unwrap_or_else(|error| panic!("stat hanging Go probe: {error}"))
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&executable, permissions)
                .unwrap_or_else(|error| panic!("protect hanging Go probe: {error}"));
            Self {
                root,
                executable,
                started,
            }
        }
    }

    impl Drop for HangingGoProbe {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn wait_for_socket(path: &Path, child: &mut ChildGuard) {
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline {
            if let Some(diagnostic) = child.exit_diagnostic() {
                panic!(
                    "endpoint owner exited before readiness: {} ({diagnostic})",
                    path.display()
                );
            }
            if let Ok(metadata) = std::fs::symlink_metadata(path)
                && metadata.file_type().is_socket()
                && metadata.permissions().mode() & 0o777 == 0o600
                // A restart can leave the prior private socket inode in
                // place until the new owner removes and rebinds it. Require
                // an accepted connection so a stale inode is never mistaken
                // for readiness.
                && UnixStream::connect(path).is_ok()
            {
                return;
            }
            thread::yield_now();
        }
        let status = child.try_wait().map_or_else(
            || "still running".to_owned(),
            |status| format!("status={status}"),
        );
        panic!(
            "timed out waiting for {} ({status}, stderr={:?})",
            path.display(),
            child.stderr_snapshot()
        );
    }

    fn worker_limits() -> WorkerLimits {
        let mut limits = WorkerLimits::default();
        // A canonical node proof can consume 64 KiB by itself; the response
        // also carries a bounded decoded page. Exercise the smallest practical
        // envelope that can represent both without weakening either bound.
        limits.transport.max_frame = 128 * 1024;
        limits.io_timeout = Duration::from_millis(100);
        limits
    }

    fn builtin_manifest(limits: TransportLimits) -> CapabilityManifest {
        let recipe = RecipeId::from_value(RECIPE_BYTES);
        let mut schemas = vec![
            SchemaDescriptor::of::<RecipeSchema>(),
            SchemaDescriptor::of::<ReadManifestSchema>(),
            SchemaDescriptor::of::<AuthorityVersionSchema>(),
            SchemaDescriptor::of::<WorkKeySchema>(),
        ];
        schemas.sort_by_key(|schema| (schema.domain, schema.type_id));
        let max_object = u64::try_from(OUTPUT_BYTES.len()).unwrap_or(u64::MAX);
        CapabilityManifest {
            protocol: VersionRange { min: 1, max: 1 },
            schemas,
            recipes: vec![RecipeCapability {
                recipe: WireIdentity::from_typed(&recipe),
                versions: VersionRange { min: 1, max: 1 },
            }],
            max_object,
            max_chunk: u32::try_from(max_object)
                .unwrap_or(u32::MAX)
                .min(u32::try_from(limits.max_frame).unwrap_or(u32::MAX)),
            max_frame: u32::try_from(limits.max_frame).unwrap_or(u32::MAX),
            max_ranges: 4,
            max_resources: ResourceEnvelope::UNBOUNDED,
        }
    }

    fn builtin_request(_limits: TransportLimits) -> WireRecipeRequest {
        let recipe = RecipeId::from_value(RECIPE_BYTES);
        let read_manifest = ReadManifestId::from_value(READ_BYTES);
        let authority = AuthorityVersion::from_value(AUTHORITY_BYTES);
        let equivalence = backend_execution::OutputEquivalence::from_value(EQUIVALENCE_BYTES);
        let relation =
            backend_engine::semantic_publication_fixture_with_authority(false, authority)
                .unwrap_or_else(|error| panic!("builtin relation: {error}"));
        let manifest = backend_engine::semantic_execution_input_manifest(&relation, authority)
            .unwrap_or_else(|error| panic!("builtin input manifest: {error}"));
        let identity = VersionedWorkIdentity::new(
            recipe,
            relation.root(),
            read_manifest,
            authority,
            equivalence,
        );
        let resources = ResourceEnvelope {
            cpu_millis: 100,
            memory_bytes: 1024,
            network_bytes: u64::try_from(OUTPUT_BYTES.len()).unwrap_or(u64::MAX),
            storage_bytes: u64::try_from(OUTPUT_BYTES.len()).unwrap_or(u64::MAX),
            output_bytes: u64::try_from(OUTPUT_BYTES.len()).unwrap_or(u64::MAX),
            processes: 1,
            wall_millis: 1_000,
        };
        WireRecipeRequest {
            attempt: AttemptId::new(1).unwrap_or_else(|error| panic!("attempt: {error}")),
            recipe: WireIdentity::from_typed(&recipe),
            work_key: WireIdentity::from_typed(&identity.work_key()),
            inputs: Vec::new(),
            read_manifest: WireIdentity::from_typed(&read_manifest),
            scope: ExecutionScopeId::from_legacy_ordinal(LEGACY_SCOPE_ONE),
            authority: WireAuthorityPolicy {
                id: WireIdentity::from_typed(&authority),
                minimum_epoch: AuthorityEpoch(1),
                revocation_version: backend_engine::RevocationVersion(1),
            },
            resources,
            fence: Fence::from_u64(1).unwrap_or_else(|error| panic!("fence: {error}")),
            cancellation: CancellationId::new([0x41; 32])
                .unwrap_or_else(|error| panic!("cancellation: {error}")),
            input_basis: backend_engine::WorkspaceRootClaim::from_bytes(manifest.root().to_bytes()),
        }
    }

    fn product_manifest(limits: TransportLimits) -> CapabilityManifest {
        let profile = backend_engine::profile_descriptor(backend_engine::Profile::Product)
            .unwrap_or_else(|error| panic!("product profile: {error}"));
        backend_engine::execution_manifest(limits, &profile)
            .unwrap_or_else(|error| panic!("product manifest: {error}"))
    }

    fn product_request(entries: &[ProductEntry]) -> WireRecipeRequest {
        let ids = backend_engine::profile_ids(&backend_engine::Profile::Product)
            .unwrap_or_else(|error| panic!("product profile ids: {error}"));
        let relation = semantic_relation(entries, ids.authority);
        let input = backend_engine::SemanticPublicationInput::from_checked_relation(&relation);
        let input_basis = backend_engine::semantic_execution_input_basis(&relation, ids.authority)
            .unwrap_or_else(|error| panic!("product input basis: {error}"));
        let identity = VersionedWorkIdentity::new(
            ids.recipe,
            relation.root(),
            ids.read_manifest,
            ids.authority,
            ids.equivalence,
        );
        let resources = backend_engine::execution_resources(ids);
        let input_version = backend_engine::semantic_input_version(input);
        WireRecipeRequest {
            attempt: AttemptId::new(1).unwrap_or_else(|error| panic!("product attempt: {error}")),
            recipe: WireIdentity::from_typed(&ids.recipe),
            work_key: WireIdentity::from_typed(&identity.work_key()),
            inputs: vec![WireIdentity::from_typed(&input_version)],
            read_manifest: WireIdentity::from_typed(&ids.read_manifest),
            scope: ExecutionScopeId::from_legacy_ordinal(LEGACY_SCOPE_ONE),
            authority: WireAuthorityPolicy {
                id: WireIdentity::from_typed(&ids.authority),
                minimum_epoch: AuthorityEpoch(1),
                revocation_version: backend_engine::RevocationVersion(1),
            },
            resources,
            fence: Fence::from_u64(1).unwrap_or_else(|error| panic!("product fence: {error}")),
            cancellation: CancellationId::new([0x41; 32])
                .unwrap_or_else(|error| panic!("product cancellation: {error}")),
            input_basis: backend_engine::WorkspaceRootClaim::from_bytes(input_basis.to_bytes()),
        }
    }

    fn semantic_relation(
        entries: &[ProductEntry],
        authority: AuthorityVersion,
    ) -> RelationState<backend_engine::ProductSemanticPublicationRelation> {
        let mut semantic_entries = entries
            .iter()
            .map(|(expected_package, record)| {
                let coordinate = backend_semantic::vocabulary::PackageUrl::parse(record.label().to_owned())
                    .unwrap_or_else(|error| panic!("semantic fixture coordinate: {error}"));
                let package = backend_engine::PackageReference::parse(record.label().to_owned())
                    .unwrap_or_else(|error| {
                        panic!("semantic fixture package reference: {error:?}")
                    });
                assert_eq!(
                    backend_engine::package_key(package.as_str()).to_bytes(),
                    *expected_package,
                    "source and semantic fixture package identities diverged"
                );
                let profile = match coordinate.package_type() {
                    backend_semantic::vocabulary::PackageType::Cargo => {
                        LanguageProfile::Rust(backend_semantic::vocabulary::RustEdition::Rust2024)
                    }
                    backend_semantic::vocabulary::PackageType::Npm => LanguageProfile::TypeScript(
                        backend_semantic::vocabulary::TypeScriptSource::TypeScript,
                    ),
                    backend_semantic::vocabulary::PackageType::Pypi => {
                        LanguageProfile::Python(backend_semantic::vocabulary::PythonVersion::Python314)
                    }
                    backend_semantic::vocabulary::PackageType::Golang => {
                        LanguageProfile::Go(backend_semantic::vocabulary::GoVersion::Go125)
                    }
                    backend_semantic::vocabulary::PackageType::Maven => {
                        LanguageProfile::Java(backend_semantic::vocabulary::JavaRelease::Java25)
                    }
                    backend_semantic::vocabulary::PackageType::Nuget => {
                        LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp14)
                    }
                    backend_semantic::vocabulary::PackageType::Generic => {
                        panic!("generic semantic fixture needs an explicit language profile")
                    }
                };
                let key = backend_engine::ProductSemanticPublicationKey::new(
                    package, coordinate, profile,
                )
                .unwrap_or_else(|error| panic!("semantic fixture key: {error}"));
                (
                    key,
                    backend_engine::ProductSemanticPublicationRecord::Unavailable(
                        backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority,
                    ),
                )
            })
            .collect::<Vec<_>>();
        semantic_entries.sort_by(|left, right| left.0.cmp(&right.0));
        RelationState::from_entries(semantic_entries, complete_scope(authority))
            .unwrap_or_else(|error| panic!("semantic relation: {error}"))
    }

    fn expected_product_output(
        entries: &[([u8; 32], backend_engine::ProductSourceRecord)],
    ) -> Vec<u8> {
        let ids = backend_engine::profile_ids(&backend_engine::Profile::Product)
            .unwrap_or_else(|error| panic!("product profile ids: {error}"));
        let relation = semantic_relation(entries, ids.authority);
        let basis = backend_engine::semantic_execution_input_basis(&relation, ids.authority)
            .unwrap_or_else(|error| panic!("product input basis: {error}"));
        let mut projection = backend_engine::SemanticPublicationProjectionBuilder::new();
        for (key, value) in relation.iter() {
            projection
                .push(key, value)
                .unwrap_or_else(|error| panic!("project expected product row: {error}"));
        }
        backend_engine::semantic_publication_output_bytes(
            ids,
            basis,
            projection.finish(relation.root()),
        )
    }

    fn connect_worker(path: &Path) -> UnixStream {
        let deadline = Instant::now() + DEADLINE;
        loop {
            match UnixStream::connect(path) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(100)))
                        .unwrap_or_else(|error| panic!("worker read timeout: {error}"));
                    stream
                        .set_write_timeout(Some(Duration::from_millis(100)))
                        .unwrap_or_else(|error| panic!("worker write timeout: {error}"));
                    return stream;
                }
                Err(error) if Instant::now() < deadline => {
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        ),
                        "unexpected worker connect error: {error}"
                    );
                    thread::yield_now();
                }
                Err(error) => panic!("connect worker endpoint: {error}"),
            }
        }
    }

    fn read_until_message(stream: &mut UnixStream) -> TransportMessage {
        let deadline = Instant::now() + DEADLINE;
        loop {
            match read_message(stream, worker_limits()) {
                Ok(message) => return message,
                Err(backend_worker::WorkerProtocolError::Timeout) if Instant::now() < deadline => {
                    thread::yield_now();
                }
                Err(error) => panic!("read worker message: {error}"),
            }
        }
    }

    fn locald_limits() -> backend_locald::FrameLimits {
        let mut limits = backend_locald::FrameLimits {
            max_frame: 64 * 1024,
            ..backend_locald::FrameLimits::default()
        };
        limits.transport.max_frame = limits.max_frame;
        limits.transport.max_chunk = limits.transport.max_chunk.min(limits.max_frame);
        limits
    }

    fn connect_locald(path: &Path) -> UnixStream {
        let deadline = Instant::now() + DEADLINE;
        loop {
            match UnixStream::connect(path) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(100)))
                        .unwrap_or_else(|error| panic!("locald read timeout: {error}"));
                    stream
                        .set_write_timeout(Some(Duration::from_millis(100)))
                        .unwrap_or_else(|error| panic!("locald write timeout: {error}"));
                    return stream;
                }
                Err(error) if Instant::now() < deadline => {
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        ),
                        "unexpected locald connect error: {error}"
                    );
                    thread::yield_now();
                }
                Err(error) => panic!("connect locald endpoint: {error}"),
            }
        }
    }

    fn locald_engine_request(
        stream: &mut UnixStream,
        request_id: u64,
        request: &backend_locald::EngineRequest,
    ) -> backend_locald::ResponseFrame {
        let limits = locald_limits();
        let payload = backend_locald::encode_engine_request(request_id, request, limits)
            .unwrap_or_else(|error| panic!("encode locald engine request: {error}"));
        let framed = backend_locald::frame(&payload, limits)
            .unwrap_or_else(|error| panic!("frame locald engine request: {error}"));
        stream
            .write_all(&framed)
            .and_then(|()| stream.flush())
            .unwrap_or_else(|error| panic!("write locald engine request: {error}"));
        let deadline = Instant::now() + DEADLINE;
        let response = loop {
            match backend_locald::read_frame(stream, limits) {
                Ok(response) => break response,
                Err(backend_locald::ProtocolError::Timeout) if Instant::now() < deadline => {
                    thread::yield_now();
                }
                Err(error) => panic!("read locald engine response: {error}"),
            }
        };
        backend_locald::decode_response(&response, limits)
            .unwrap_or_else(|error| panic!("decode locald engine response: {error}"))
    }

    #[derive(Clone, Debug, Default)]
    struct WorkerProxySnapshot {
        negotiations: usize,
        requests: usize,
        results: usize,
        controls: usize,
        injected_wrong: usize,
        late_results: usize,
        last_request: Option<WireRecipeRequest>,
        result: Option<WireRecipeResult>,
        result_bytes: Vec<u8>,
        locald_frames: Vec<String>,
        worker_frames: Vec<String>,
    }

    #[derive(Debug)]
    struct WorkerProxyGuard {
        stop: Arc<AtomicBool>,
        endpoint: PathBuf,
        state: Arc<Mutex<WorkerProxySnapshot>>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl WorkerProxyGuard {
        fn snapshot(&self) -> WorkerProxySnapshot {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl Drop for WorkerProxyGuard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            // The proxy listener is nonblocking, but a self-connect also
            // wakes an accept call immediately on platforms that coalesce
            // readiness notifications.
            let _ = UnixStream::connect(&self.endpoint);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
            let _ = std::fs::remove_file(&self.endpoint);
        }
    }

    fn record_proxy_message(state: &Mutex<WorkerProxySnapshot>, message: &TransportMessage) {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.locald_frames.push(proxy_message_kind(message));
        match message {
            TransportMessage::WireRecipeRequest(request) => {
                state.requests = state.requests.saturating_add(1);
                state.last_request = Some(request.clone());
            }
            TransportMessage::CancelAttempt(_) => {
                state.controls = state.controls.saturating_add(1);
            }
            _ => {}
        }
    }

    fn record_proxy_response(state: &Mutex<WorkerProxySnapshot>, response: &TransportMessage) {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.worker_frames.push(proxy_message_kind(response));
        match response {
            TransportMessage::Capabilities(_) => {
                state.negotiations = state.negotiations.saturating_add(1);
            }
            TransportMessage::WireRecipeResult(result) => {
                state.results = state.results.saturating_add(1);
                state.result = Some((**result).clone());
                state.result_bytes.clone_from(result.output_bytes.as_ref());
            }
            _ => {}
        }
    }

    fn proxy_message_kind(message: &TransportMessage) -> String {
        match message {
            TransportMessage::ClosurePageRequest(request) => format!(
                "closure-page-request(root={}, level-node={:02x?})",
                request.request.node == request.request.root.digest(),
                &request.request.node.as_bytes()[..4]
            ),
            TransportMessage::ClosurePageResponse(response) => format!(
                "closure-page-response(root={}, level={}, body={}, proof={})",
                response.page.node == response.page.root.digest(),
                response.page.level,
                match &response.page.body {
                    backend_engine::MerklePageBody::Branch(children) =>
                        format!("branch:{}", children.len()),
                    backend_engine::MerklePageBody::Leaf(entries) =>
                        format!("leaf:{}", entries.len()),
                },
                response.proof.len()
            ),
            TransportMessage::WirePack(_) => "wire-pack".to_owned(),
            TransportMessage::Capabilities(_) => "capabilities".to_owned(),
            TransportMessage::RootSummary(_) => "root-summary".to_owned(),
            TransportMessage::WireRootSummary(_) => "wire-root-summary".to_owned(),
            TransportMessage::ClosureRootOffer(_) => "closure-root-offer".to_owned(),
            TransportMessage::ClosureRootAck(ack) => format!(
                "closure-root-ack(warm={}, missing={:?})",
                ack.warm,
                ack.missing
                    .iter()
                    .map(|claim| format!("{:02x?}", &claim.as_bytes()[..4]))
                    .collect::<Vec<_>>()
            ),
            TransportMessage::ClosureNeedRequest(_) => "closure-need-request".to_owned(),
            TransportMessage::NodeRequest(_) => "node-request".to_owned(),
            TransportMessage::WireNodeRequest(_) => "wire-node-request".to_owned(),
            TransportMessage::RangeRequest(_) => "range-request".to_owned(),
            TransportMessage::WireRangeRequest(_) => "wire-range-request".to_owned(),
            TransportMessage::Chunk(frame) => {
                format!("chunk(version={:02x?})", &frame.version.as_bytes()[..4])
            }
            TransportMessage::Resume(_) => "resume".to_owned(),
            TransportMessage::WireResumeRequest(_) => "wire-resume-request".to_owned(),
            TransportMessage::CancelAttempt(_) => "cancel-attempt".to_owned(),
            TransportMessage::WireRecipeRequest(_) => "recipe-request".to_owned(),
            TransportMessage::WireRecipeResult(_) => "recipe-result".to_owned(),
        }
    }

    fn wait_for_proxy(
        proxy: &WorkerProxyGuard,
        locald: &mut ChildGuard,
        worker: &mut ChildGuard,
        admitted: impl Fn(&WorkerProxySnapshot) -> bool,
        label: &str,
    ) -> WorkerProxySnapshot {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let snapshot = proxy.snapshot();
            if admitted(&snapshot) {
                return snapshot;
            }
            assert!(locald.is_running(), "locald exited while awaiting {label}");
            assert!(worker.is_running(), "worker exited while awaiting {label}");
            assert!(
                Instant::now() < deadline,
                "timed out awaiting {label}: {snapshot:?}"
            );
            thread::yield_now();
        }
    }

    fn forward_locald_messages(
        locald_reader: &mut UnixStream,
        worker_writer: &Mutex<UnixStream>,
        locald_writer: &Mutex<UnixStream>,
        delayed_result: &Mutex<Option<WireRecipeResult>>,
        connection_stop: &AtomicBool,
        thread_stop: &AtomicBool,
        thread_state: &Mutex<WorkerProxySnapshot>,
    ) {
        while !thread_stop.load(Ordering::Acquire) && !connection_stop.load(Ordering::Acquire) {
            let message = match read_message(locald_reader, worker_limits()) {
                Ok(message) => message,
                Err(backend_worker::WorkerProtocolError::Timeout) => continue,
                Err(_) => break,
            };
            record_proxy_message(thread_state, &message);
            let cancelled = matches!(message, TransportMessage::CancelAttempt(_));
            let forwarded = {
                let mut writer = worker_writer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                write_message(&mut *writer, &message, worker_limits()).is_ok()
            };
            if cancelled {
                let result = delayed_result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some(result) = result {
                    let delivered = {
                        let mut writer = locald_writer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        write_message(
                            &mut *writer,
                            &TransportMessage::WireRecipeResult(Box::new(result)),
                            worker_limits(),
                        )
                        .is_ok()
                    };
                    if delivered {
                        let mut state = thread_state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.late_results = state.late_results.saturating_add(1);
                    }
                }
            }
            if !forwarded {
                break;
            }
        }
        connection_stop.store(true, Ordering::Release);
    }

    fn forward_worker_messages(
        worker_reader: &mut UnixStream,
        locald_writer: &Mutex<UnixStream>,
        delayed_result: &Mutex<Option<WireRecipeResult>>,
        connection_stop: &AtomicBool,
        thread_stop: &AtomicBool,
        thread_state: &Mutex<WorkerProxySnapshot>,
        inject_out_of_order_result: Option<usize>,
    ) {
        while !thread_stop.load(Ordering::Acquire) && !connection_stop.load(Ordering::Acquire) {
            let response = match read_message(worker_reader, worker_limits()) {
                Ok(response) => response,
                Err(backend_worker::WorkerProtocolError::Timeout) => continue,
                Err(_) => break,
            };
            record_proxy_response(thread_state, &response);
            let claimed_injection = matches!(response, TransportMessage::WireRecipeResult(_)) && {
                let mut state = thread_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if inject_out_of_order_result == Some(state.results) && state.injected_wrong == 0 {
                    state.injected_wrong = 1;
                    true
                } else {
                    false
                }
            };
            let forwarded = if claimed_injection {
                let TransportMessage::WireRecipeResult(result) = response else {
                    unreachable!("result injection was claimed for a non-result frame");
                };
                let mut wrong = (*result).clone();
                wrong.attempt = AttemptId::new(wrong.attempt.get().saturating_add(1))
                    .unwrap_or_else(|error| panic!("proxy wrong attempt: {error}"));
                *delayed_result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(*result);
                let mut writer = locald_writer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                write_message(
                    &mut *writer,
                    &TransportMessage::WireRecipeResult(Box::new(wrong)),
                    worker_limits(),
                )
                .is_ok()
            } else {
                let mut writer = locald_writer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                write_message(&mut *writer, &response, worker_limits()).is_ok()
            };
            if !forwarded {
                break;
            }
        }
        connection_stop.store(true, Ordering::Release);
    }

    fn proxy_connection(
        locald: &UnixStream,
        worker_endpoint: &Path,
        thread_stop: &AtomicBool,
        thread_state: &Mutex<WorkerProxySnapshot>,
        inject_out_of_order_result: Option<usize>,
    ) {
        if locald
            .set_nonblocking(false)
            .and_then(|()| locald.set_read_timeout(Some(Duration::from_millis(100))))
            .and_then(|()| locald.set_write_timeout(Some(Duration::from_millis(100))))
            .is_err()
        {
            // A reconnect can be retired between accept and configuration.
            // Treat that as an already-closed generation, not a proxy panic.
            return;
        }

        let worker = connect_worker(worker_endpoint);
        let mut locald_reader = locald
            .try_clone()
            .unwrap_or_else(|error| panic!("clone proxy locald reader: {error}"));
        let locald_writer = Mutex::new(
            locald
                .try_clone()
                .unwrap_or_else(|error| panic!("clone proxy locald writer: {error}")),
        );
        let mut worker_reader = worker
            .try_clone()
            .unwrap_or_else(|error| panic!("clone proxy worker reader: {error}"));
        let worker_writer = Mutex::new(
            worker
                .try_clone()
                .unwrap_or_else(|error| panic!("clone proxy worker writer: {error}")),
        );
        let connection_stop = AtomicBool::new(false);
        let delayed_result = Mutex::new(None::<WireRecipeResult>);

        // Production transport is independently duplex: closure pages and
        // chunks can flow in either direction while a recipe is active.  The
        // failure-injection proxy must preserve that concurrency or it can
        // manufacture a deadlock that does not exist in the actual transport.
        thread::scope(|scope| {
            scope.spawn(|| {
                forward_locald_messages(
                    &mut locald_reader,
                    &worker_writer,
                    &locald_writer,
                    &delayed_result,
                    &connection_stop,
                    thread_stop,
                    thread_state,
                );
            });

            scope.spawn(|| {
                forward_worker_messages(
                    &mut worker_reader,
                    &locald_writer,
                    &delayed_result,
                    &connection_stop,
                    thread_stop,
                    thread_state,
                    inject_out_of_order_result,
                );
            });
        });
        let _ = locald.shutdown(Shutdown::Both);
        let _ = worker.shutdown(Shutdown::Both);
    }

    fn spawn_worker_proxy(
        endpoint: &Path,
        worker_endpoint: &Path,
        inject_out_of_order_result: Option<usize>,
    ) -> WorkerProxyGuard {
        let listener = UnixListener::bind(endpoint)
            .unwrap_or_else(|error| panic!("bind worker proxy: {error}"));
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|error| panic!("configure worker proxy: {error}"));
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(WorkerProxySnapshot::default()));
        let thread_stop = Arc::clone(&stop);
        let thread_state = Arc::clone(&state);
        let proxy_endpoint = endpoint.to_owned();
        let worker_endpoint = worker_endpoint.to_owned();
        let join = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                let (locald, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::yield_now();
                        continue;
                    }
                    Err(_) => break,
                };
                proxy_connection(
                    &locald,
                    &worker_endpoint,
                    &thread_stop,
                    &thread_state,
                    inject_out_of_order_result,
                );
            }
            let _ = std::fs::remove_file(proxy_endpoint);
        });
        WorkerProxyGuard {
            stop,
            endpoint: endpoint.to_owned(),
            state,
            join: Some(join),
        }
    }

    #[test]
    fn echo_fixture_worker_negotiates_same_uid_and_returns_an_attested_result() {
        let _process_lease = process_journey_lease();
        let root = unique_directory("worker");
        let endpoint = root.join("worker.sock");
        let args = vec![
            OsString::from("--endpoint"),
            endpoint.clone().into_os_string(),
            OsString::from("--profile"),
            OsString::from("builtin-echo"),
            OsString::from("--max-frame"),
            OsString::from((4 * 1024 * 1024).to_string()),
            OsString::from("--timeout-ms"),
            OsString::from("100"),
        ];
        let mut worker = ChildGuard::spawn("backend-worker", &args, &[]);
        wait_for_socket(&endpoint, &mut worker);
        let mode = std::fs::metadata(&endpoint)
            .unwrap_or_else(|error| panic!("worker endpoint metadata: {error}"))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "worker endpoint must be private");

        let mut stream = connect_worker(&endpoint);
        assert!(
            backend_engine::peer_is_same_effective_uid(&stream)
                .unwrap_or_else(|error| panic!("peer credentials: {error}")),
            "worker accepted a same-UID peer"
        );
        let limits = worker_limits();
        let manifest = builtin_manifest(limits.transport);
        write_message(
            &mut stream,
            &TransportMessage::Capabilities(manifest.clone()),
            limits,
        )
        .unwrap_or_else(|error| panic!("worker capability negotiation request: {error}"));
        let negotiated = match read_until_message(&mut stream) {
            TransportMessage::Capabilities(peer) => manifest
                .negotiate(&peer, limits.transport)
                .unwrap_or_else(|error| panic!("negotiate worker capabilities: {error}")),
            other => panic!("unexpected worker negotiation response: {other:?}"),
        };
        assert_eq!(negotiated.protocol, 1);
        assert_eq!(negotiated.recipes.len(), 1);

        let request = builtin_request(limits.transport);
        write_message(
            &mut stream,
            &TransportMessage::WireRecipeRequest(request.clone()),
            limits,
        )
        .unwrap_or_else(|error| panic!("worker request: {error}"));
        let result = match read_until_message(&mut stream) {
            TransportMessage::WireRecipeResult(result) => *result,
            other => panic!("unexpected worker result: {other:?}"),
        };
        assert_eq!(result.attempt, request.attempt);
        assert_eq!(result.recipe, request.recipe);
        assert_eq!(result.work_key, request.work_key);
        assert_eq!(
            result.input_basis.as_bytes(),
            request.input_basis.as_bytes()
        );
        assert_eq!(result.output_bytes.as_ref(), OUTPUT_BYTES);
        assert!(result.byte_coverage.is_complete(OUTPUT_BYTES.len() as u64));
        assert!(
            result.attestation.is_some(),
            "remote worker must attest output"
        );
        assert!(worker.is_running(), "worker exited after one request");
        drop(stream);
        drop(worker);
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("remove worker fixture: {error}"));
    }

    fn cancel_manifest(limits: TransportLimits) -> CapabilityManifest {
        let recipe = RecipeId::from_value(CANCEL_RECIPE_BYTES);
        let mut schemas = vec![
            SchemaDescriptor::of::<RecipeSchema>(),
            SchemaDescriptor::of::<ReadManifestSchema>(),
            SchemaDescriptor::of::<AuthorityVersionSchema>(),
            SchemaDescriptor::of::<WorkKeySchema>(),
        ];
        schemas.sort_by_key(|schema| (schema.domain, schema.type_id));
        CapabilityManifest {
            protocol: VersionRange { min: 1, max: 1 },
            schemas,
            recipes: vec![RecipeCapability {
                recipe: WireIdentity::from_typed(&recipe),
                versions: VersionRange { min: 1, max: 1 },
            }],
            max_object: limits.max_object,
            max_chunk: u32::try_from(limits.max_chunk).unwrap_or(u32::MAX),
            max_frame: u32::try_from(limits.max_frame).unwrap_or(u32::MAX),
            max_ranges: u32::try_from(limits.max_ranges).unwrap_or(u32::MAX),
            max_resources: ResourceEnvelope::UNBOUNDED,
        }
    }

    #[derive(Debug)]
    struct CancelRelation;

    impl Relation for CancelRelation {
        const DOMAIN: u8 = 0x97;
        const TYPE: u16 = 2;
        type Key = u64;
        type Value = u64;

        fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
            output.extend_from_slice(&value.to_be_bytes());
        }

        fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(&value.to_be_bytes());
        }
    }

    fn cancel_request() -> WireRecipeRequest {
        let recipe = RecipeId::from_value(CANCEL_RECIPE_BYTES);
        let read_manifest = ReadManifestId::from_value(CANCEL_READ_BYTES);
        let authority = AuthorityVersion::from_value(CANCEL_AUTHORITY_BYTES);
        let equivalence =
            backend_execution::OutputEquivalence::from_value(CANCEL_EQUIVALENCE_BYTES);
        let relation = RelationState::<CancelRelation>::from_entries(
            [(1_u64, 1_u64)],
            complete_scope(authority),
        )
        .unwrap_or_else(|error| panic!("cancel relation: {error}"));
        let manifest = WorkspaceManifest::new_checked(
            1,
            vec![RelationBinding::from_state(&relation)],
            Vec::new(),
            ObjectClosure::from_version(authority),
            relation.coverage(),
        )
        .unwrap_or_else(|error| panic!("cancel input manifest: {error}"));
        let identity = VersionedWorkIdentity::new(
            recipe,
            relation.root(),
            read_manifest,
            authority,
            equivalence,
        );
        WireRecipeRequest {
            attempt: AttemptId::new(1).unwrap_or_else(|error| panic!("cancel attempt: {error}")),
            recipe: WireIdentity::from_typed(&recipe),
            work_key: WireIdentity::from_typed(&identity.work_key()),
            inputs: Vec::new(),
            read_manifest: WireIdentity::from_typed(&read_manifest),
            scope: ExecutionScopeId::from_legacy_ordinal(LEGACY_SCOPE_ONE),
            authority: WireAuthorityPolicy {
                id: WireIdentity::from_typed(&authority),
                minimum_epoch: AuthorityEpoch(1),
                revocation_version: backend_engine::RevocationVersion(1),
            },
            resources: ResourceEnvelope {
                cpu_millis: 100,
                memory_bytes: 1024,
                network_bytes: 1024,
                storage_bytes: 1024,
                output_bytes: 1024,
                processes: 1,
                wall_millis: 10_000,
            },
            fence: Fence::from_u64(1).unwrap_or_else(|error| panic!("cancel fence: {error}")),
            cancellation: CancellationId::new([0x42; 32])
                .unwrap_or_else(|error| panic!("cancel id: {error}")),
            input_basis: backend_engine::WorkspaceRootClaim::from_bytes(manifest.root().to_bytes()),
        }
    }

    #[test]
    fn cancellation_fixture_is_admitted_mid_execution_and_worker_accepts_followup_control() {
        let _process_lease = process_journey_lease();
        let root = unique_directory("cancel-worker");
        let endpoint = root.join("worker.sock");
        let started = root.join("started");
        let cancelled = root.join("cancelled");
        let args = vec![
            OsString::from("--endpoint"),
            endpoint.clone().into_os_string(),
            OsString::from("--max-frame"),
            OsString::from("131072"),
            OsString::from("--timeout-ms"),
            OsString::from("100"),
        ];
        let mut worker = ChildGuard::spawn(
            "backend-journey-cancel-worker",
            &args,
            &[(CANCEL_START_ENV, &started), (CANCELLED_ENV, &cancelled)],
        );
        wait_for_socket(&endpoint, &mut worker);
        let mut stream = connect_worker(&endpoint);
        let limits = worker_limits();
        let manifest = cancel_manifest(limits.transport);
        write_message(
            &mut stream,
            &TransportMessage::Capabilities(manifest.clone()),
            limits,
        )
        .unwrap_or_else(|error| panic!("cancel worker negotiation: {error}"));
        assert!(matches!(
            read_until_message(&mut stream),
            TransportMessage::Capabilities(_)
        ));
        let request = cancel_request();
        write_message(
            &mut stream,
            &TransportMessage::WireRecipeRequest(request.clone()),
            limits,
        )
        .unwrap_or_else(|error| panic!("slow worker request: {error}"));
        let deadline = Instant::now() + DEADLINE;
        while !started.exists() && Instant::now() < deadline {
            assert!(worker.is_running(), "slow worker exited before execution");
            thread::yield_now();
        }
        assert!(started.exists(), "slow recipe never entered execution");

        let cancel = CancelAttempt {
            attempt: request.attempt,
            work_key: request.work_key,
            cancellation: request.cancellation,
            fence: request.fence,
        };
        write_message(
            &mut stream,
            &TransportMessage::CancelAttempt(cancel),
            limits,
        )
        .unwrap_or_else(|error| panic!("authenticated cancellation: {error}"));
        let cancellation_deadline = Instant::now() + DEADLINE;
        while !cancelled.exists() && Instant::now() < cancellation_deadline {
            assert!(worker.is_running(), "worker exited before observing cancel");
            thread::yield_now();
        }
        assert!(
            cancelled.exists(),
            "slow recipe did not observe cancellation"
        );
        // Cancellation has no result frame by design.  A capabilities frame
        // after the command proves that serve_stream returned to its control
        // loop instead of killing or wedging the worker process.
        write_message(
            &mut stream,
            &TransportMessage::Capabilities(manifest.clone()),
            limits,
        )
        .unwrap_or_else(|error| panic!("follow-up capability control: {error}"));
        assert!(matches!(
            read_until_message(&mut stream),
            TransportMessage::Capabilities(_)
        ));
        assert!(worker.is_running(), "worker exited after cancellation");
        drop(stream);

        // The listener is synchronous per stream; once the canceled stream is
        // closed it must still accept a fresh negotiated connection.
        let mut reconnected = connect_worker(&endpoint);
        write_message(
            &mut reconnected,
            &TransportMessage::Capabilities(manifest),
            limits,
        )
        .unwrap_or_else(|error| panic!("reconnect capability negotiation: {error}"));
        assert!(matches!(
            read_until_message(&mut reconnected),
            TransportMessage::Capabilities(_)
        ));
        assert!(worker.is_running(), "worker did not recover its listener");
        drop(reconnected);
        drop(worker);
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("remove cancellation fixture: {error}"));
    }

    #[test]
    fn real_locald_restart_reopens_checked_workspace_and_rejects_unadmitted_completion() {
        let _process_lease = process_journey_lease();
        let root = unique_directory("locald-restart");
        let endpoint = root.join("locald.sock");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace)
            .unwrap_or_else(|error| panic!("create locald restart workspace: {error}"));
        let authority_secret = authority_secret_file(&root);
        let args = locald_args(&endpoint, &workspace, &authority_secret, None, 3_000);
        let mut first = ChildGuard::spawn("backend-locald", &args, &[]);
        wait_for_socket(&endpoint, &mut first);

        let mode = std::fs::metadata(&endpoint)
            .unwrap_or_else(|error| panic!("locald endpoint metadata: {error}"))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "locald endpoint must be private");
        let limits = locald_limits();
        let peer = product_manifest(limits.transport);
        let mut capabilities = connect_locald(&endpoint);
        assert!(
            backend_engine::peer_is_same_effective_uid(&capabilities)
                .unwrap_or_else(|error| panic!("locald peer credentials: {error}")),
            "locald accepted a same-UID peer"
        );
        let capability_reply = locald_engine_request(
            &mut capabilities,
            41,
            &backend_locald::EngineRequest::Replicate(Box::new(TransportMessage::Capabilities(
                peer,
            ))),
        );
        assert!(matches!(
            capability_reply,
            backend_locald::ResponseFrame::Engine {
                request_id: 41,
                status: backend_locald::EngineStatus::Accepted,
            }
        ));
        drop(capabilities);

        // A raw completion claim has no authority at this boundary. The
        // compiled builtin profile deliberately has no retained scheduler
        // ticket, so it must return a correlated rejection and close only
        // this client stream.
        let mut completion = connect_locald(&endpoint);
        let rejected = locald_engine_request(
            &mut completion,
            42,
            &backend_locald::EngineRequest::Complete(backend_locald::CompletionClaim {
                work_key: [0; 32],
                output: [0; 32],
                ordinal: 0,
                fence: [0; 32],
            }),
        );
        match rejected {
            backend_locald::ResponseFrame::Engine {
                request_id: 42,
                status: backend_locald::EngineStatus::Rejected(message),
            } => assert!(
                message.contains("completion admission")
                    || message.contains("invalid local control"),
                "unexpected completion rejection: {message}"
            ),
            other => panic!("raw completion was not rejected: {other:?}"),
        }
        drop(completion);

        let first_json = cli_health(&endpoint, "first restart health");
        let first_root = first_json["reply"]["data"]["root"].clone();

        // ChildGuard performs a bounded kill/wait; the next profile startup
        // must recover the same durable checked genesis through the stale
        // socket path without a test sleep or a second owner.
        drop(first);
        let mut second = ChildGuard::spawn("backend-locald", &args, &[]);
        wait_for_socket(&endpoint, &mut second);
        let second_json = cli_health(&endpoint, "second restart health");
        assert_eq!(
            second_json["reply"]["data"]["root"], first_root,
            "restart changed the checked workspace root"
        );
        let mut recovered = connect_locald(&endpoint);
        let recovered_reply = locald_engine_request(
            &mut recovered,
            43,
            &backend_locald::EngineRequest::Replicate(Box::new(TransportMessage::Capabilities(
                product_manifest(limits.transport),
            ))),
        );
        assert!(matches!(
            recovered_reply,
            backend_locald::ResponseFrame::Engine {
                request_id: 43,
                status: backend_locald::EngineStatus::Accepted,
            }
        ));
        drop(recovered);
        drop(second);
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("remove locald restart fixture: {error}"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn semantic_version_selection_is_exact_and_durable_across_restart() {
        let _process_lease = process_journey_lease();
        let root = unique_directory("semantic-version-selection");
        let endpoint = root.join("locald.sock");
        let workspace = root.join("workspace");
        let project = root.join("rust-project");
        std::fs::create_dir_all(project.join("src"))
            .unwrap_or_else(|error| panic!("create semantic project: {error}"));
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"semantic-selection\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap_or_else(|error| panic!("write semantic manifest: {error}"));
        let source = project.join("src/lib.rs");
        std::fs::write(
            &source,
            "/// First immutable marker.\npub fn first_marker() -> &'static str { \"first\" }\n",
        )
        .unwrap_or_else(|error| panic!("write first semantic source: {error}"));
        std::fs::create_dir_all(&workspace)
            .unwrap_or_else(|error| panic!("create semantic selection workspace: {error}"));
        let authority_secret = authority_secret_file(&root);
        let args = vec![
            OsString::from("--endpoint"),
            endpoint.clone().into_os_string(),
            OsString::from("--workspace"),
            workspace.clone().into_os_string(),
            OsString::from("--profile"),
            OsString::from("builtin"),
            OsString::from("--authority-secret-file"),
            authority_secret.clone().into_os_string(),
            OsString::from("--max-frame"),
            OsString::from("65536"),
            OsString::from("--timeout-ms"),
            OsString::from("60000"),
        ];
        let rustc = explicit_path_executable("rustc")
            .unwrap_or_else(|| panic!("bounded semantic history requires an explicit rustc"));
        let mut locald = ChildGuard::spawn("backend-locald", &args, &[("NUDOX_RUSTC", &rustc)]);
        wait_for_socket(&endpoint, &mut locald);

        index_project(&endpoint, &project);
        let package = backend_library::PackageReference::parse(
            project
                .canonicalize()
                .unwrap_or_else(|error| panic!("canonicalize semantic package: {error}"))
                .to_string_lossy()
                .into_owned(),
        )
        .unwrap_or_else(|error| panic!("admit semantic package reference: {error:?}"));
        let first_records = client_semantic_versions(&endpoint, package.clone());
        assert_eq!(
            first_records.len(),
            1,
            "first semantic publication did not produce one retained generation: {first_records:?}"
        );
        let first = first_records
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("first semantic generation"));
        assert!(
            first.selected,
            "first generation was not initially selected"
        );

        std::fs::write(
            &source,
            "/// Second immutable marker.\npub fn second_marker() -> &'static str { \"second\" }\n",
        )
        .unwrap_or_else(|error| panic!("write second semantic source: {error}"));
        index_project(&endpoint, &project);
        let second_records = client_semantic_versions(&endpoint, package.clone());
        assert_eq!(
            second_records.len(),
            2,
            "same target did not retain both immutable semantic generations: {second_records:?}"
        );
        let second = second_records
            .iter()
            .find(|record| record.selected)
            .cloned()
            .unwrap_or_else(|| panic!("selected second semantic generation"));
        let retained_first = second_records
            .iter()
            .find(|record| record.generation == first.generation)
            .cloned()
            .unwrap_or_else(|| panic!("retained first semantic generation"));
        assert!(!retained_first.selected);
        assert_ne!(
            first.generation, second.generation,
            "source mutation reused an immutable binding identity"
        );
        assert_eq!(first.coordinate, second.coordinate);
        assert_eq!(first.profile, second.profile);

        let unknown = surface_reply(
            &endpoint,
            53,
            backend_library::SurfaceCommand::SelectSemanticVersion {
                package: package.clone(),
                coordinate: first.coordinate.clone(),
                profile: first.profile,
                generation: backend_library::SemanticGenerationId::new([0xa5; 32]),
            },
        );
        match unknown.reply {
            CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(message)) => {
                assert!(
                    message.contains("not retained") || message.contains("malformed"),
                    "unknown generation rejection lost its typed reason: {message}"
                );
            }
            other => panic!("unknown generation was not rejected: {other:?}"),
        }

        let foreign_package = backend_library::PackageReference::parse(
            root.join("other-project").to_string_lossy().into_owned(),
        )
        .unwrap_or_else(|error| panic!("admit foreign package reference: {error:?}"));
        let foreign = surface_reply(
            &endpoint,
            54,
            backend_library::SurfaceCommand::SelectSemanticVersion {
                package: foreign_package,
                coordinate: first.coordinate.clone(),
                profile: first.profile,
                generation: first.generation,
            },
        );
        match foreign.reply {
            CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(message)) => {
                assert!(
                    message.contains("not retained"),
                    "cross-package generation rejection lost its typed reason: {message}"
                );
            }
            other => panic!("cross-package generation was not rejected: {other:?}"),
        }

        let mut client = backend_mcp::Session::connect(&endpoint)
            .unwrap_or_else(|error| panic!("connect semantic selection client: {error}"));
        let selected_first = client
            .select_semantic_version(
                package.clone(),
                first.coordinate.clone(),
                first.profile,
                first.generation,
            )
            .unwrap_or_else(|error| panic!("valid rollback selection failed: {error}"));
        assert!(selected_first.selected);
        assert_eq!(selected_first.generation, first.generation);
        let rolled_back = client_semantic_versions(&endpoint, package.clone());
        assert_eq!(
            rolled_back
                .iter()
                .filter(|record| record.selected)
                .map(|record| record.generation)
                .collect::<Vec<_>>(),
            vec![first.generation],
            "rollback did not move the one selected pointer"
        );

        // The selected history is the only semantic input to the derived
        // product view. A fresh root and the old declaration prove that the
        // rollback crossed publication, search, and graph admission rather
        // than changing a client-side label.
        let mut session = backend_mcp::Session::connect(&endpoint)
            .unwrap_or_else(|error| panic!("connect rollback query session: {error}"));
        let rollback_revision = session
            .revision()
            .unwrap_or_else(|error| panic!("read rollback revision: {error}"));
        let names = session
            .names("first_marker", 8)
            .unwrap_or_else(|error| panic!("search rolled-back semantic declaration: {error}"));
        let first_symbol = match names.reply {
            CommandReply::Names(snapshot) => snapshot
                .root
                .rows()
                .iter()
                .find(|row| row.label.contains("first_marker"))
                .and_then(|row| match row.id {
                    backend_library::RowId::Symbol(symbol) => Some(symbol),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("rollback search omitted first_marker")),
            other => panic!("rollback name reply changed shape: {other:?}"),
        };
        let graph = session
            .graph_symbol(first_symbol)
            .unwrap_or_else(|error| panic!("read rolled-back semantic graph: {error}"));
        assert!(matches!(graph.reply, CommandReply::Graph(_)));
        let document = session
            .document_symbol(first_symbol)
            .unwrap_or_else(|error| panic!("read rolled-back semantic document: {error}"));
        assert!(matches!(document.reply, CommandReply::Document(_)));
        let diff = session
            .diff(package.clone(), package.clone())
            .unwrap_or_else(|error| panic!("read rolled-back semantic diff: {error}"));
        assert!(
            diff.is_empty(),
            "a package compared with itself must have no selected-generation diff: {diff:?}"
        );
        let post_query_revision = session
            .revision()
            .unwrap_or_else(|error| panic!("read post-query rollback revision: {error}"));
        assert_eq!(
            rollback_revision.root, post_query_revision.root,
            "search/graph observed different roots after rollback"
        );

        drop(locald);
        let mut restarted = ChildGuard::spawn("backend-locald", &args, &[]);
        wait_for_socket(&endpoint, &mut restarted);
        let reopened = client_semantic_versions(&endpoint, package.clone());
        assert_eq!(reopened.len(), 2);
        assert_eq!(
            reopened
                .iter()
                .filter(|record| record.selected)
                .map(|record| record.generation)
                .collect::<Vec<_>>(),
            vec![first.generation],
            "restart lost the durable rollback selection"
        );
        assert!(
            reopened
                .iter()
                .any(|record| record.generation == second.generation),
            "restart dropped the newer immutable history generation"
        );
        let mut desktop_transport = backend_desktop::UnixSubscriptionTransport::connect(&endpoint)
            .unwrap_or_else(|error| panic!("connect desktop history transport: {error}"));
        let (desktop_root, desktop_cursor) =
            desktop_transport.bootstrap_root().unwrap_or_else(|error| {
                panic!("hydrate selected semantic root after restart: {error}")
            });
        let desktop_basis = desktop_root.basis().root;
        let desktop = DesktopModel::try_new_at(desktop_root, desktop_cursor, desktop_basis)
            .unwrap_or_else(|error| panic!("admit selected semantic root in desktop: {error}"));
        assert!(
            desktop
                .root()
                .rows()
                .iter()
                .any(|row| row.label.contains("first_marker")),
            "desktop restart hydration omitted the selected first generation"
        );
        drop(restarted);
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("remove semantic selection fixture: {error}"));
    }

    #[test]
    fn locald_readiness_does_not_wait_for_an_ambient_go_probe() {
        let _process_lease = process_journey_lease();
        let root = unique_directory("locald-go-probe-readiness");
        let endpoint = root.join("locald.sock");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace)
            .unwrap_or_else(|error| panic!("create locald Go-probe workspace: {error}"));
        let authority_secret = authority_secret_file(&root);
        let probe = HangingGoProbe::new();
        let args = locald_args(&endpoint, &workspace, &authority_secret, None, 3_000);
        let mut locald = ChildGuard::spawn(
            "backend-locald",
            &args,
            &[("NUDOX_GO", probe.executable.as_path())],
        );

        let probe_deadline = Instant::now() + DEADLINE;
        while !probe.started.exists() && Instant::now() < probe_deadline {
            assert!(
                locald.is_running(),
                "locald exited before the Go probe started"
            );
            thread::yield_now();
        }
        assert!(
            probe.started.exists(),
            "the child did not exercise the explicitly supplied Go authority fixture"
        );

        // The listener is the process readiness boundary.  It must become
        // usable while the Go capability is still in its typed probing state;
        // a toolchain probe cannot hold the owner before this assertion.
        wait_for_socket(&endpoint, &mut locald);
        assert!(
            locald.is_running(),
            "locald exited after publishing readiness during the Go probe"
        );
        drop(locald);
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("remove Go-probe readiness fixture: {error}"));
    }

    fn add_probe_package(
        client: &mut backend_mcp::UnixCommandTransport,
        package: &str,
        index: usize,
    ) -> ([u8; 32], backend_engine::ProductSourceRecord) {
        let key = backend_engine::package_key(package);
        let certificate = WireCertificate::new().with_claim(WireClaim::Key {
            schema: WireSchema::Package,
            id: backend_engine::encode_id(key.as_bytes()),
            value: package.to_owned(),
        });
        let reply = backend_mcp::CommandTransport::request(
            client,
            CommandDto::new(200 + index as u64, Command::Add { package: key })
                .with_certificate(certificate),
        )
        .unwrap_or_else(|error| panic!("product mutation {index}: {error}"));
        assert!(
            matches!(&reply.reply, CommandReply::Added(_)),
            "product mutation {index} was rejected: {:?}",
            reply.reply
        );
        let record = backend_engine::ProductSourceRecord::new(package)
            .unwrap_or_else(|error| panic!("product source record: {error}"));
        (key.to_bytes(), record)
    }

    fn probe_package_after(
        entries: &[([u8; 32], backend_engine::ProductSourceRecord)],
        label: &str,
    ) -> String {
        let floor = entries.iter().map(|(key, _)| *key).max().unwrap_or([0; 32]);
        (0..10_000_u64)
            .map(|nonce| {
                format!(
                    "pkg:cargo/backend-remote-probe-{label}-{nonce:04}-{}@1.0.0",
                    "y".repeat(1_700)
                )
            })
            .find(|candidate| backend_engine::package_key(candidate).to_bytes() > floor)
            .unwrap_or_else(|| panic!("find a deterministic package key after the warm frontier"))
    }

    fn seed_local_product_route(
        locald_endpoint: &Path,
        entries: &[([u8; 32], backend_engine::ProductSourceRecord)],
    ) {
        let mut stream = connect_locald(locald_endpoint);
        let seeded = locald_engine_request(
            &mut stream,
            100,
            &backend_locald::EngineRequest::Replicate(Box::new(
                TransportMessage::WireRecipeRequest(product_request(entries)),
            )),
        );
        assert!(
            matches!(
                &seeded,
                backend_locald::ResponseFrame::Engine {
                    request_id: 100,
                    status: backend_locald::EngineStatus::Accepted,
                }
            ),
            "local route-model seed response: {seeded:?}"
        );
    }

    type ProductEntry = ([u8; 32], backend_engine::ProductSourceRecord);

    struct ProductProbe {
        entries: Vec<ProductEntry>,
        request: WireRecipeRequest,
        expected_output: Vec<u8>,
    }

    fn queue_remote_probe(
        locald_endpoint: &Path,
        locald: &mut ChildGuard,
        worker: &mut ChildGuard,
    ) -> ProductProbe {
        // This command gives the sole owner loop a deterministic turn to
        // install the already-negotiated asynchronous transport.
        let _health = cli_health(locald_endpoint, "remote worker readiness health");
        // Values are intentionally large enough to force a multilevel
        // canonical relation with only a handful of mutations. This crosses
        // descendant proof admission and the worker's depth-first warm-CAS
        // walk without making the process journey depend on thousands of
        // tiny setup requests.
        let packages = (0..20)
            .map(|index| {
                format!(
                    "pkg:cargo/backend-remote-probe-{index:02}-{}@1.0.0",
                    "x".repeat(1_700)
                )
            })
            .collect::<Vec<_>>();
        let mut client = backend_mcp::UnixCommandTransport::connect(locald_endpoint)
            .unwrap_or_else(|error| panic!("connect bulk mutation client: {error}"));
        let mut entries = Vec::with_capacity(packages.len());
        for (index, package) in packages
            .iter()
            .enumerate()
            .take(packages.len().saturating_sub(1))
        {
            entries.push(add_probe_package(&mut client, package, index));
        }
        entries.sort_by_key(|(key, _)| *key);

        // Observe the already split root locally. The following mutation is
        // then an exact adjacent delta, so remote placement is learned from
        // authenticated version history rather than a test-only route hint.
        seed_local_product_route(locald_endpoint, &entries);

        let index = packages.len().saturating_sub(1);
        let package = packages
            .get(index)
            .unwrap_or_else(|| panic!("remote probe package"));
        entries.push(add_probe_package(&mut client, package, index));
        entries.sort_by_key(|(key, _)| *key);

        let request = product_request(&entries);
        let expected = expected_product_output(&entries);
        // Bulk durable publication can outlive one connection's I/O lease.
        // Reconnect to prove that route state and the selected workspace root
        // belong to the daemon owner rather than to a client transport.
        let mut stream = connect_locald(locald_endpoint);
        let first = locald_engine_request(
            &mut stream,
            101,
            &backend_locald::EngineRequest::Replicate(Box::new(
                TransportMessage::WireRecipeRequest(request.clone()),
            )),
        );
        assert!(
            matches!(
                &first,
                backend_locald::ResponseFrame::Engine {
                    request_id: 101,
                    status: backend_locald::EngineStatus::Queued { .. },
                }
            ),
            "first remote dispatch response: {first:?}"
        );
        assert!(locald.is_running(), "locald exited during remote fallback");
        assert!(worker.is_running(), "worker exited during remote fallback");
        drop(stream);
        ProductProbe {
            entries,
            request,
            expected_output: expected,
        }
    }

    fn queue_product_remote_request(
        locald_endpoint: &Path,
        request_id: u64,
        request: &WireRecipeRequest,
    ) {
        let mut stream = connect_locald(locald_endpoint);
        let response = locald_engine_request(
            &mut stream,
            request_id,
            &backend_locald::EngineRequest::Replicate(Box::new(
                TransportMessage::WireRecipeRequest(request.clone()),
            )),
        );
        assert!(
            matches!(
                response,
                backend_locald::ResponseFrame::Engine {
                    request_id: returned,
                    status: backend_locald::EngineStatus::Queued { .. }
                        | backend_locald::EngineStatus::Accepted,
                } if returned == request_id
            ),
            "remote Product delta was not admitted: {response:?}"
        );
    }

    fn proxy_count(snapshot: &WorkerProxySnapshot, prefix: &str) -> usize {
        snapshot
            .locald_frames
            .iter()
            .filter(|kind| kind.starts_with(prefix))
            .count()
    }

    fn assert_product_reused(
        locald_endpoint: &Path,
        request_id: u64,
        request: &WireRecipeRequest,
        proxy: &WorkerProxyGuard,
        dispatched: usize,
    ) {
        let mut stream = connect_locald(locald_endpoint);
        let response = locald_engine_request(
            &mut stream,
            request_id,
            &backend_locald::EngineRequest::Replicate(Box::new(
                TransportMessage::WireRecipeRequest(request.clone()),
            )),
        );
        assert!(matches!(
            response,
            backend_locald::ResponseFrame::Engine {
                request_id: returned,
                status: backend_locald::EngineStatus::Accepted,
            } if returned == request_id
        ));
        assert_eq!(
            proxy.snapshot().requests,
            dispatched,
            "completed Product result was not reused"
        );
    }

    fn reject_late_result_and_assert_reuse(
        locald_endpoint: &Path,
        locald: &mut ChildGuard,
        proxy: &WorkerProxyGuard,
        snapshot: &WorkerProxySnapshot,
        request: WireRecipeRequest,
    ) {
        // The valid frame was deliberately delivered after the daemon had
        // selected fallback.  Injecting that late frame back through the
        // locald socket must be rejected by the owner-retained ticket check,
        // while the daemon remains available for a fresh client connection.
        let late_result = snapshot
            .result
            .clone()
            .unwrap_or_else(|| panic!("proxy did not retain the late result"));
        let mut late_stream = connect_locald(locald_endpoint);
        let late = locald_engine_request(
            &mut late_stream,
            103,
            &backend_locald::EngineRequest::Replicate(Box::new(
                TransportMessage::WireRecipeResult(Box::new(late_result)),
            )),
        );
        match late {
            backend_locald::ResponseFrame::Engine {
                request_id: 103,
                status: backend_locald::EngineStatus::Rejected(message),
            } => assert!(
                message.contains("owner-retained dispatch ticket"),
                "late result rejection lost its admission reason: {message}"
            ),
            other => panic!("late worker result was not rejected: {other:?}"),
        }
        assert!(
            locald.is_running(),
            "locald exited after rejecting a late worker result"
        );
        drop(late_stream);

        // The local fallback selected the worker's exact canonical bytes. A
        // retry of the same semantic request over a new connection must reuse
        // the retained output and therefore must not dispatch a second worker
        // attempt.
        let mut reuse_stream = connect_locald(locald_endpoint);
        let second = locald_engine_request(
            &mut reuse_stream,
            102,
            &backend_locald::EngineRequest::Replicate(Box::new(
                TransportMessage::WireRecipeRequest(request),
            )),
        );
        assert!(matches!(
            second,
            backend_locald::ResponseFrame::Engine {
                request_id: 102,
                status: backend_locald::EngineStatus::Accepted,
            }
        ));
        let reused = proxy.snapshot();
        assert_eq!(
            reused.requests, snapshot.requests,
            "retained output was not reused"
        );
        assert_eq!(reused.result_bytes, snapshot.result_bytes);
        drop(reuse_stream);
    }

    #[derive(Clone, Copy)]
    struct ClosureTraffic {
        pages: usize,
        chunks: usize,
    }

    fn observe_cold_product(
        locald_endpoint: &Path,
        locald: &mut ChildGuard,
        worker: &mut ChildGuard,
        proxy: &WorkerProxyGuard,
        probe: &ProductProbe,
    ) -> ClosureTraffic {
        let cold = wait_for_proxy(
            proxy,
            locald,
            worker,
            |snapshot| snapshot.requests == 1 && snapshot.results == 1,
            "cold remote Product result",
        );
        assert_eq!(
            cold.result_bytes, probe.expected_output,
            "cold remote execution diverged from the local projection oracle"
        );
        assert_eq!(
            cold.last_request
                .as_ref()
                .unwrap_or_else(|| panic!("proxy omitted cold worker request"))
                .work_key,
            probe.request.work_key,
        );
        let traffic = ClosureTraffic {
            pages: proxy_count(&cold, "closure-page-response"),
            chunks: proxy_count(&cold, "chunk"),
        };
        assert!(
            traffic.pages >= 3,
            "cold closure did not cross a multilevel tree"
        );
        assert!(
            traffic.chunks >= 1,
            "cold closure omitted its Product input"
        );
        assert_product_reused(locald_endpoint, 106, &probe.request, proxy, 1);
        traffic
    }

    fn advance_and_observe_warm_product(
        locald_endpoint: &Path,
        locald: &mut ChildGuard,
        worker: &mut ChildGuard,
        proxy: &WorkerProxyGuard,
        entries: &mut Vec<ProductEntry>,
        cold: ClosureTraffic,
    ) {
        let mut mutation = backend_mcp::UnixCommandTransport::connect(locald_endpoint)
            .unwrap_or_else(|error| panic!("connect warm delta client: {error}"));
        let index = entries.len();
        let package = probe_package_after(entries, "warm");
        entries.push(add_probe_package(&mut mutation, &package, index));
        entries.sort_by_key(|(key, _)| *key);
        let request = product_request(entries);
        let expected = expected_product_output(entries);
        queue_product_remote_request(locald_endpoint, 104, &request);
        let warm = wait_for_proxy(
            proxy,
            locald,
            worker,
            |snapshot| snapshot.requests == 2 && snapshot.results == 2,
            "warm remote Product delta",
        );
        assert_eq!(
            warm.result_bytes, expected,
            "warm remote execution diverged from the local projection oracle"
        );
        let warm_pages = proxy_count(&warm, "closure-page-response").saturating_sub(cold.pages);
        let warm_chunks = proxy_count(&warm, "chunk").saturating_sub(cold.chunks);
        assert!(
            warm_pages < cold.pages,
            "warm delta sent {warm_pages} node proofs after cold sent {}: locald={:?}, worker={:?}",
            cold.pages,
            warm.locald_frames,
            warm.worker_frames,
        );
        assert_eq!(
            warm_chunks, 1,
            "warm delta should transfer only its fixed Product input on the object plane"
        );
        assert_product_reused(locald_endpoint, 107, &request, proxy, 2);
    }

    fn advance_and_observe_fallback(
        locald_endpoint: &Path,
        locald: &mut ChildGuard,
        worker: &mut ChildGuard,
        proxy: &WorkerProxyGuard,
        entries: &mut Vec<ProductEntry>,
    ) {
        let mut mutation = backend_mcp::UnixCommandTransport::connect(locald_endpoint)
            .unwrap_or_else(|error| panic!("connect fallback delta client: {error}"));
        let index = entries.len();
        let package = format!(
            "pkg:cargo/backend-remote-probe-fallback-{}@1.0.0",
            "z".repeat(1_700)
        );
        entries.push(add_probe_package(&mut mutation, &package, index));
        entries.sort_by_key(|(key, _)| *key);
        let request = product_request(entries);
        let expected = expected_product_output(entries);
        queue_product_remote_request(locald_endpoint, 105, &request);
        let snapshot = wait_for_proxy(
            proxy,
            locald,
            worker,
            |snapshot| {
                snapshot.requests == 3
                    && snapshot.results == 3
                    && snapshot.injected_wrong == 1
                    && snapshot.late_results == 1
                    && snapshot.controls == 1
            },
            "remote result, cancellation, and late-result fence",
        );
        assert_proxy_snapshot(&snapshot, &request, &expected);
        reject_late_result_and_assert_reuse(locald_endpoint, locald, proxy, &snapshot, request);
    }

    #[test]
    fn real_locald_socket_dispatches_to_worker_then_reuses_fallback_bytes() {
        let _process_lease = process_journey_lease();
        let root = unique_directory("locald-remote");
        let locald_endpoint = root.join("locald.sock");
        let worker_endpoint = root.join("worker.sock");
        let proxy_endpoint = root.join("worker-proxy.sock");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace)
            .unwrap_or_else(|error| panic!("create locald remote workspace: {error}"));
        let authority_secret = authority_secret_file(&root);

        let worker_args = worker_args(&worker_endpoint, &authority_secret);
        let mut worker = ChildGuard::spawn("backend-worker", &worker_args, &[]);
        wait_for_socket(&worker_endpoint, &mut worker);

        // The proxy forwards canonical frames over both Unix boundaries. The
        // first two Product versions complete normally so the second can
        // measure warm Merkle-frontier reuse. It injects a wrong attempt for
        // the third result and holds the valid result until exact cancellation.
        let proxy = spawn_worker_proxy(&proxy_endpoint, &worker_endpoint, Some(3));
        let locald_args = locald_args(
            &locald_endpoint,
            &workspace,
            &authority_secret,
            Some(&proxy_endpoint),
            3_000,
        );
        let mut locald = ChildGuard::spawn("backend-locald", &locald_args, &[]);
        wait_for_socket(&locald_endpoint, &mut locald);
        let negotiated = wait_for_proxy(
            &proxy,
            &mut locald,
            &mut worker,
            |snapshot| snapshot.negotiations >= 1,
            "worker capability negotiation",
        );
        assert!(negotiated.negotiations >= 1);

        let mut probe = queue_remote_probe(&locald_endpoint, &mut locald, &mut worker);
        let cold = observe_cold_product(&locald_endpoint, &mut locald, &mut worker, &proxy, &probe);
        advance_and_observe_warm_product(
            &locald_endpoint,
            &mut locald,
            &mut worker,
            &proxy,
            &mut probe.entries,
            cold,
        );
        advance_and_observe_fallback(
            &locald_endpoint,
            &mut locald,
            &mut worker,
            &proxy,
            &mut probe.entries,
        );

        drop(locald);
        drop(proxy);
        drop(worker);
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("remove locald remote fixture: {error}"));
    }

    fn spawn_mcp(endpoint: &Path, input: &[u8]) -> (ExitStatus, Vec<u8>) {
        let mut command = ProcessCommand::new(binary("backend-mcp"));
        command.arg("--endpoint").arg(endpoint).arg("--framed");
        let output = bounded_command(command, "backend-mcp", Some(input));
        (output.status, output.stdout)
    }

    fn spawn_json_mcp(endpoint: &Path, project: &Path, input: &[u8]) -> Output {
        let mut command = ProcessCommand::new(binary("backend-mcp"));
        command
            .arg("--endpoint")
            .arg(endpoint)
            .arg("--project")
            .arg(project);
        bounded_command(command, "backend-mcp JSON-RPC", Some(input))
    }

    fn worker_args(endpoint: &Path, authority_secret: &Path) -> Vec<OsString> {
        vec![
            OsString::from("--endpoint"),
            endpoint.as_os_str().to_owned(),
            OsString::from("--profile"),
            OsString::from("builtin"),
            OsString::from("--authority-secret-file"),
            authority_secret.as_os_str().to_owned(),
            OsString::from("--max-frame"),
            OsString::from("131072"),
            OsString::from("--timeout-ms"),
            // Keep the authenticated session alive while the test publishes
            // enough bounded source records to split the canonical tree, and
            // beyond locald's three-second fallback budget. The process guard
            // still owns the global twelve-second test deadline.
            OsString::from("60000"),
        ]
    }

    fn locald_args(
        endpoint: &Path,
        workspace: &Path,
        authority_secret: &Path,
        worker_endpoint: Option<&Path>,
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
            authority_secret.as_os_str().to_owned(),
        ];
        if let Some(worker_endpoint) = worker_endpoint {
            args.extend([
                OsString::from("--worker-endpoint"),
                worker_endpoint.as_os_str().to_owned(),
            ]);
        }
        args.extend([
            OsString::from("--max-frame"),
            OsString::from("131072"),
            OsString::from("--timeout-ms"),
            // This is also the end-to-end remote fallback budget. Leave room
            // for a cold debug worker to admit the paged closure and start
            // execution before the proxy deliberately withholds its result.
            OsString::from(timeout_ms.to_string()),
        ]);
        args
    }

    fn cli_health(endpoint: &Path, label: &str) -> serde_json::Value {
        let mut command = ProcessCommand::new(binary("backend-cli"));
        command
            .arg("--endpoint")
            .arg(endpoint)
            .arg("--json")
            .arg("health");
        let output = bounded_command(command, label, None);
        assert!(
            output.status.success(),
            "{label} failed with {}: stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("decode {label}: {error}"))
    }

    fn assert_proxy_snapshot(
        snapshot: &WorkerProxySnapshot,
        request: &WireRecipeRequest,
        expected_output: &[u8],
    ) {
        assert!(
            snapshot
                .result_bytes
                .starts_with(backend_engine::PRODUCT_OUTPUT_BYTES),
            "worker returned bytes outside the product output contract"
        );
        assert_eq!(
            snapshot.result_bytes, expected_output,
            "remote execution and local canonical projection diverged"
        );
        let worker_request = snapshot
            .last_request
            .as_ref()
            .unwrap_or_else(|| panic!("proxy did not retain the worker request"));
        assert_eq!(worker_request.recipe, request.recipe);
        assert_eq!(worker_request.work_key, request.work_key);
        assert_ne!(
            worker_request.cancellation, request.cancellation,
            "worker request must carry the daemon-owned cancellation identity"
        );
        assert_eq!(
            snapshot.injected_wrong, 1,
            "proxy did not inject one stale attempt"
        );
        assert_eq!(
            snapshot.late_results, 1,
            "proxy did not deliver one late result"
        );
        assert_eq!(
            snapshot.controls, 1,
            "wrong-attempt result must trigger one authenticated cancellation"
        );
    }

    fn wait_for_indexed_symbol(endpoint: &Path, symbol: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let mut query = ProcessCommand::new(binary("backend-cli"));
            query
                .arg("--endpoint")
                .arg(endpoint)
                .arg("name")
                .arg(symbol);
            let found = bounded_command(query, "backend-cli publication readiness", None);
            if found.status.success() && String::from_utf8_lossy(&found.stdout).contains(symbol) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "index intent never published {symbol}: stdout={} stderr={}",
                String::from_utf8_lossy(&found.stdout),
                String::from_utf8_lossy(&found.stderr)
            );
            thread::yield_now();
        }
    }

    fn surface_reply(
        endpoint: &Path,
        request_id: u64,
        surface: backend_library::SurfaceCommand,
    ) -> ReplyDto {
        let mut transport = backend_mcp::UnixCommandTransport::connect(endpoint)
            .unwrap_or_else(|error| panic!("connect surface transport: {error}"));
        backend_mcp::CommandTransport::request(
            &mut transport,
            CommandDto::new(request_id, Command::Surface(surface)),
        )
        .unwrap_or_else(|error| panic!("surface request {request_id}: {error}"))
    }

    fn index_project(endpoint: &Path, project: &Path) {
        let mut index = ProcessCommand::new(binary("backend-cli"));
        index
            .arg("--endpoint")
            .arg(endpoint)
            .arg("index")
            .arg(project);
        let indexed = bounded_command_for(
            index,
            "backend-cli semantic history index",
            None,
            Duration::from_secs(60),
        );
        assert!(
            indexed.status.success(),
            "CLI semantic history index failed: stdout={} stderr={}",
            String::from_utf8_lossy(&indexed.stdout),
            String::from_utf8_lossy(&indexed.stderr)
        );
    }

    fn explicit_path_executable(name: &str) -> Option<PathBuf> {
        std::env::var_os("PATH")?.to_str().and_then(|path| {
            std::env::split_paths(path).find_map(|directory| {
                let candidate = directory.join(name);
                candidate
                    .is_file()
                    .then(|| candidate.canonicalize().ok())
                    .flatten()
            })
        })
    }

    fn client_semantic_versions(
        endpoint: &Path,
        package: backend_library::PackageReference,
    ) -> Box<[backend_library::SemanticVersionRecord]> {
        let mut session = backend_mcp::Session::connect(endpoint)
            .unwrap_or_else(|error| panic!("connect semantic history client: {error}"));
        session
            .semantic_versions(package)
            .unwrap_or_else(|error| panic!("read semantic history through client: {error}"))
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn cli_mcp_and_desktop_share_one_process_root_and_proof_bearing_reply() {
        let _process_lease = process_journey_lease();
        let root = unique_directory("product-clients");
        let endpoint = root.join("locald.sock");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace)
            .unwrap_or_else(|error| panic!("create locald workspace: {error}"));
        let authority_secret = authority_secret_file(&root);
        let args = vec![
            OsString::from("--endpoint"),
            endpoint.clone().into_os_string(),
            OsString::from("--workspace"),
            workspace.clone().into_os_string(),
            OsString::from("--profile"),
            OsString::from("builtin"),
            OsString::from("--authority-secret-file"),
            authority_secret.into_os_string(),
            OsString::from("--max-frame"),
            OsString::from("65536"),
            OsString::from("--timeout-ms"),
            // Cold seven-language indexing can cross one second after the
            // remote process journey has exercised fsync-heavy paths. Keep
            // the request bounded by the outer twelve-second journey while
            // avoiding a machine-load-dependent false timeout.
            OsString::from("5000"),
        ];
        let mut locald = ChildGuard::spawn("backend-locald", &args, &[]);
        wait_for_socket(&endpoint, &mut locald);
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/polyglot");
        let rust_path = project.join("src/lib.rs");
        let python_path = project.join("src/main.py");
        let typescript_path = project.join("src/main.ts");
        let go_path = project.join("src/main.go");
        let java_path = project.join("src/Main.java");
        let csharp_path = project.join("src/Main.cs");
        let clang_path = project.join("src/main.cpp");
        let rust_source = std::fs::read(&rust_path).expect("read Rust fixture");
        let python_source = std::fs::read(&python_path).expect("read Python fixture");
        let typescript_source = std::fs::read(&typescript_path).expect("read TypeScript fixture");
        let go_source = std::fs::read(&go_path).expect("read Go fixture");
        let java_source = std::fs::read(&java_path).expect("read Java fixture");
        let csharp_source = std::fs::read(&csharp_path).expect("read C# fixture");
        let clang_source = std::fs::read(&clang_path).expect("read C++ fixture");
        assert_malformed_source_is_terminal();

        // The syntax lane is an explicitly labeled structural baseline. It
        // proves local parsing, coordinates, and source capture without
        // claiming native type, binding, diagnostic, or dependency authority.
        assert_syntax_baseline(
            backend_frontend_rust::syntax_frontend().expect("Rust syntax frontend"),
            &rust_path,
            &rust_source,
            "ferris",
        );
        assert_syntax_baseline(
            backend_frontend_python::syntax_frontend().expect("Python syntax frontend"),
            &python_path,
            &python_source,
            "monty",
        );
        assert_syntax_baseline(
            backend_frontend_typescript::syntax_frontend().expect("TypeScript syntax frontend"),
            &typescript_path,
            &typescript_source,
            "turing",
        );
        assert_syntax_baseline(
            backend_frontend_go::syntax_frontend().expect("Go syntax frontend"),
            &go_path,
            &go_source,
            "Gopher",
        );
        assert_syntax_baseline(
            backend_frontend_java::syntax_frontend().expect("Java syntax frontend"),
            &java_path,
            &java_source,
            "Duke",
        );
        assert_syntax_baseline(
            backend_frontend_csharp::syntax_frontend().expect("C# syntax frontend"),
            &csharp_path,
            &csharp_source,
            "Anders",
        );
        assert_syntax_baseline(
            backend_frontend_clang::syntax_frontend().expect("C++ syntax frontend"),
            &clang_path,
            &clang_source,
            "Bjarne",
        );

        // A native semantic claim requires a configured authority-protocol
        // helper. The manifest-only constructors still discover all inputs,
        // but must return a typed Unsupported witness rather than manufacture
        // facts from ordinary compiler stdout.
        let executable = "/bin/sh";
        let rust_authority = backend_frontend_rust::RustFrontend::new(
            rust_source.clone(),
            executable,
            b"[package]\nname='polyglot'\nversion='0.0.0'\n".to_vec(),
            "",
        )
        .expect("Rust manifest authority");
        assert_manifest_only_authority(
            &rust_authority,
            "src/lib.rs",
            &rust_source,
            Coverage::Unsupported,
        );
        let python_authority = backend_frontend_python::PythonFrontend::new(
            python_source.clone(),
            executable,
            executable,
            "3.13",
            b"{}".to_vec(),
        )
        .expect("Python manifest authority");
        assert_manifest_only_authority(
            &python_authority,
            "module.py",
            &python_source,
            Coverage::Unsupported,
        );
        let typescript_authority = backend_frontend_typescript::TypeScriptFrontend::new(
            typescript_source.clone(),
            executable,
            executable,
            "ts",
            b"{}".to_vec(),
        )
        .expect("TypeScript manifest authority");
        assert_manifest_only_authority(
            &typescript_authority,
            "index.ts",
            &typescript_source,
            Coverage::Unsupported,
        );
        let go_authority = backend_frontend_go::GoFrontend::new(
            go_source.clone(),
            executable,
            b"module polyglot\n\ngo 1.22\n".to_vec(),
            "",
        )
        .expect("Go manifest authority");
        assert_manifest_only_authority(
            &go_authority,
            "module/source.go",
            &go_source,
            Coverage::Unavailable,
        );
        let java_authority = backend_frontend_java::JavaFrontend::new(
            java_source.clone(),
            executable,
            Vec::new(),
            "21",
        )
        .expect("Java manifest authority");
        assert_manifest_only_authority(
            &java_authority,
            "src/Main.java",
            &java_source,
            Coverage::Unsupported,
        );
        let csharp_authority = backend_frontend_csharp::CSharpFrontend::new(
            csharp_source.clone(),
            executable,
            executable,
            "Release",
        )
        .expect("C# manifest authority");
        assert_manifest_only_authority(
            &csharp_authority,
            "source.cs",
            &csharp_source,
            Coverage::Unsupported,
        );
        let clang_authority = backend_frontend_clang::ClangFrontend::new(
            clang_source.clone(),
            executable,
            "clang++ -std=c++20",
        )
        .expect("Clang manifest authority");
        assert_manifest_only_authority(
            &clang_authority,
            "translation-unit",
            &clang_source,
            Coverage::Unsupported,
        );

        // Unavailable is deliberately distinct from Unsupported: this
        // authority was selected, but its required local executable vanished.
        let absent = root.join("missing-python-authority");
        let unavailable_python = backend_frontend_python::PythonFrontend::new(
            python_source.clone(),
            absent.to_string_lossy(),
            executable,
            "3.13",
            b"{}".to_vec(),
        )
        .expect("unavailable Python authority configuration");
        let unavailable_snapshot = unavailable_python
            .discover()
            .expect("discover unavailable Python authority");
        let unavailable_key = SessionKey::new(
            unavailable_python.identity(),
            unavailable_snapshot.manifest(),
            typed_of::<ProfileSchema>(b"polyglot-cutover-profile"),
            typed_of::<FlowSchema>(b"polyglot-cutover-flow"),
            typed_of::<SemanticBasisSchema>(b"polyglot-cutover-basis"),
        );
        let unavailable = unavailable_python
            .extract(&unavailable_snapshot, unavailable_key)
            .expect("unavailable authority is an explicit result");
        assert_eq!(unavailable.coverage().state(), Coverage::Unavailable);

        let mut index = ProcessCommand::new(binary("backend-cli"));
        index
            .arg("--endpoint")
            .arg(&endpoint)
            .arg("index")
            .arg(&project);
        let indexed = bounded_command(index, "backend-cli index", None);
        assert!(
            indexed.status.success(),
            "CLI index failed: stdout={} stderr={}",
            String::from_utf8_lossy(&indexed.stdout),
            String::from_utf8_lossy(&indexed.stderr)
        );
        wait_for_indexed_symbol(&endpoint, "ferris");
        // Seeing one row establishes that the new generation is visible. All
        // seven lanes must now be readable; observing a mixed generation is
        // an atomic-publication failure.
        for (symbol, source_path) in [
            ("ferris", "src/lib.rs"),
            ("monty", "src/main.py"),
            ("turing", "src/main.ts"),
            ("Gopher", "src/main.go"),
            ("Duke", "src/Main.java"),
            ("Anders", "src/Main.cs"),
            ("Bjarne", "src/main.cpp"),
        ] {
            let mut query = ProcessCommand::new(binary("backend-cli"));
            query
                .arg("--endpoint")
                .arg(&endpoint)
                .arg("name")
                .arg(symbol);
            let found = bounded_command(query, "backend-cli name", None);
            let output = String::from_utf8_lossy(&found.stdout);
            assert!(
                found.status.success() && output.contains(symbol) && output.contains(source_path),
                "{symbol} did not cross the indexed process path: stdout={} stderr={}",
                output,
                String::from_utf8_lossy(&found.stderr)
            );
        }
        let mut cli_command = ProcessCommand::new(binary("backend-cli"));
        cli_command
            .arg("--endpoint")
            .arg(&endpoint)
            .arg("--json")
            .arg("health");
        let cli_output = bounded_command(cli_command, "backend-cli", None);
        assert!(
            cli_output.status.success(),
            "CLI health failed: {}",
            String::from_utf8_lossy(&cli_output.stderr)
        );
        let cli_json: serde_json::Value = serde_json::from_slice(&cli_output.stdout)
            .unwrap_or_else(|error| panic!("decode CLI JSON: {error}"));
        assert_eq!(cli_json["request_id"], serde_json::json!(1));
        assert_eq!(cli_json["reply"]["kind"], serde_json::json!("revision"));
        let cli_reply_root = cli_json["reply"]["data"]["root"].clone();
        let cli_has_certificate = cli_json
            .get("certificate")
            .is_some_and(|certificate| !certificate.is_null());

        let request = CommandDto::new(1, Command::Health);
        let input = backend_mcp::encode_request(&request)
            .unwrap_or_else(|error| panic!("encode MCP request: {error}"));
        let (mcp_status, mcp_output) = spawn_mcp(&endpoint, &input);
        assert!(
            mcp_status.success(),
            "MCP health failed with output {}",
            String::from_utf8_lossy(&mcp_output)
        );
        let mcp_json: serde_json::Value = serde_json::from_slice(&mcp_output[4..])
            .unwrap_or_else(|error| panic!("decode MCP reply JSON: {error}"));
        assert_eq!(mcp_json["reply"]["data"]["root"], cli_reply_root);
        let mcp_has_certificate = mcp_json
            .get("certificate")
            .is_some_and(|certificate| !certificate.is_null());

        let ferris_coordinate = format!("{}::src/lib.rs:2::ferris", project.display());
        let mcp_input = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "journey", "version": "1" }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "backend.search",
                    // Tantivy intersects token-prefix clauses independent of
                    // source order. The compatibility substring projection
                    // cannot match this reversed phrase, so this query proves
                    // the daemon returns the live lexical lane's selection.
                    "arguments": { "query": "turing function", "limit": 8 }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "backend.query",
                    "arguments": {
                        "query": "{ Declaration { name @filter(op: \"=\", value: [\"$selected\"]) coordinate @output } }",
                        "variables": { "selected": "turing" },
                        "limit": 8
                    }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "backend.document",
                    "arguments": { "coordinate": ferris_coordinate.clone() }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "backend.graph",
                    "arguments": { "coordinate": ferris_coordinate.clone() }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {
                    "name": "backend.outline",
                    "arguments": { "path": project.to_string_lossy() }
                }
            }),
        ]
        .into_iter()
        .map(|request| request.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        let json_mcp = spawn_json_mcp(&endpoint, &project, mcp_input.as_bytes());
        assert!(
            json_mcp.status.success(),
            "JSON-RPC MCP failed: stdout={} stderr={}",
            String::from_utf8_lossy(&json_mcp.stdout),
            String::from_utf8_lossy(&json_mcp.stderr)
        );
        let json_replies = String::from_utf8(json_mcp.stdout)
            .unwrap_or_else(|error| panic!("MCP JSON-RPC UTF-8: {error}"))
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .unwrap_or_else(|error| panic!("MCP JSON-RPC response: {error}: {line}"))
            })
            .collect::<Vec<_>>();
        assert_eq!(json_replies.len(), 6);
        assert_eq!(
            json_replies[0]["result"]["protocolVersion"],
            serde_json::json!("2025-11-25")
        );
        assert!(
            json_replies[1]["result"]["structuredContent"]["rows"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| {
                    row["coordinate"]
                        .as_str()
                        .is_some_and(|coordinate| coordinate.contains("turing"))
                })),
            "MCP tool search did not return the TypeScript declaration: {:?}",
            json_replies[1]
        );
        assert!(
            json_replies[1]["result"]["structuredContent"]["rows"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| {
                    row["coordinate"]
                        .as_str()
                        .is_some_and(|coordinate| coordinate.contains("turing"))
                        && row["source"]["state"] == serde_json::json!("captured")
                        && row["source"]["path"]
                            .as_str()
                            .is_some_and(|path| path == "src/main.ts")
                        && row["excerpt"]["state"] == serde_json::json!("captured")
                        && row["excerpt"]["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("turing"))
                        && row["excerpt"]["extent"] == serde_json::json!("complete")
                })),
            "MCP search lost exact source evidence: {:?}",
            json_replies[1]
        );
        let trustfall_returned_live_typescript =
            json_replies[2]["result"]["structuredContent"]["rows"]
                .as_array()
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["coordinate"]
                            .as_str()
                            .is_some_and(|coordinate| coordinate.contains("turing"))
                    })
                });
        for (reply, tool) in
            json_replies[3..]
                .iter()
                .zip(["backend.document", "backend.graph", "backend.outline"])
        {
            assert_ne!(
                reply["result"]["isError"],
                serde_json::json!(true),
                "{tool} failed against the live indexed view: {reply:?}"
            );
        }
        assert!(
            json_replies[3]["result"]["structuredContent"]["signature"]
                .as_str()
                .is_some_and(|signature| signature.contains("ferris")),
            "document retrieval lost the indexed Rust declaration: {:?}",
            json_replies[3]
        );
        assert_eq!(
            json_replies[3]["result"]["structuredContent"]["excerpt"]["state"],
            serde_json::json!("captured")
        );
        assert_eq!(
            json_replies[3]["result"]["structuredContent"]["excerpt"]["extent"],
            serde_json::json!("complete")
        );
        assert!(
            json_replies[3]["result"]["structuredContent"]["excerpt"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("ferris")),
            "document retrieval did not return bounded Rust source: {:?}",
            json_replies[3]
        );
        assert_eq!(
            json_replies[5]["result"]["structuredContent"]["extent"],
            serde_json::json!("complete"),
            "polyglot outline was silently truncated"
        );
        assert!(
            json_replies[5]["result"]["structuredContent"]["roots"]
                .as_array()
                .is_some_and(|roots| roots.len() >= 7),
            "outline did not retain the seven top-level language lanes: {:?}",
            json_replies[5]
        );

        // Raw JSON cannot mint a complete-view capability. Re-admit the same
        // MCP command through a live owner-authenticated endpoint before the
        // typed view is allowed to seed desktop state.
        let mut typed_mcp = backend_mcp::UnixCommandTransport::connect(&endpoint)
            .unwrap_or_else(|error| panic!("connect typed MCP transport: {error}"));
        let mcp_reply: ReplyDto = backend_mcp::CommandTransport::request(
            &mut typed_mcp,
            CommandDto::new(1, Command::Health),
        )
        .unwrap_or_else(|error| panic!("admit typed MCP reply: {error}"));
        assert_eq!(mcp_reply.request_id, 1);
        assert!(matches!(&mcp_reply.reply, CommandReply::Readiness(_)));
        let readiness = match &mcp_reply.reply {
            CommandReply::Readiness(report) => report,
            other => panic!("MCP health reply changed shape: {other:?}"),
        };
        assert_eq!(
            backend_library::encode_id(readiness.revision().root().as_bytes()),
            cli_reply_root.as_str().expect("CLI revision root")
        );
        assert!(
            readiness.row_count() >= 15,
            "MCP health did not observe the polyglot indexed view"
        );
        assert!(
            readiness
                .coverage()
                .iter()
                .any(|coverage| coverage.is_complete()),
            "structural browsing lost its complete owner coverage"
        );
        assert!(
            readiness.coverage().iter().any(|coverage| matches!(
                coverage,
                backend_library::Coverage::Unavailable {
                    lane: backend_library::Lane::Semantic,
                    reason: backend_library::Reason::Unconfigured,
                }
            )),
            "incomplete native authorities were promoted to complete semantic coverage"
        );
        // The terminal must not also be described as work in progress. A
        // fraction over rows that are written once and never rewritten tells
        // every surface to wait for a completion that cannot arrive.
        assert!(
            !readiness.coverage().iter().any(|coverage| matches!(
                coverage,
                backend_library::Coverage::Partial {
                    lane: backend_library::Lane::Semantic,
                    ..
                }
            )),
            "a terminal semantic lane was still reported as an advancing fraction"
        );
        let native_semantic_slots = readiness
            .capabilities()
            .as_slice()
            .iter()
            .filter(|status| {
                matches!(
                    status.family(),
                    CapabilityFamily::LanguageOracle {
                        task: LanguageOracleTask::TypeCheck | LanguageOracleTask::SemanticIndex,
                        ..
                    }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            native_semantic_slots.len(),
            LanguageProfile::PRODUCT_PROFILES.len() * 2
        );
        assert!(
            native_semantic_slots
                .iter()
                .filter(|status| { matches!(status.lifecycle(), CapabilityLifecycle::Installed) })
                .all(|status| {
                    matches!(
                        status.family(),
                        CapabilityFamily::LanguageOracle {
                            profile: LanguageProfile::Go(_) | LanguageProfile::Python(_),
                            task: LanguageOracleTask::TypeCheck | LanguageOracleTask::SemanticIndex,
                        }
                    )
                })
        );
        let go_installed = native_semantic_slots
            .iter()
            .filter(|status| {
                matches!(
                    status.family(),
                    CapabilityFamily::LanguageOracle {
                        profile: LanguageProfile::Go(_),
                        task: LanguageOracleTask::TypeCheck | LanguageOracleTask::SemanticIndex,
                    }
                ) && matches!(status.lifecycle(), CapabilityLifecycle::Installed)
            })
            .count();
        assert!(
            matches!(go_installed, 0 | 2),
            "Go type and semantic-index capability states diverged"
        );
        let python_installed = native_semantic_slots
            .iter()
            .filter(|status| {
                matches!(
                    status.family(),
                    CapabilityFamily::LanguageOracle {
                        profile: LanguageProfile::Python(_),
                        task: LanguageOracleTask::TypeCheck | LanguageOracleTask::SemanticIndex,
                    }
                ) && matches!(status.lifecycle(), CapabilityLifecycle::Installed)
            })
            .count();
        assert!(
            matches!(python_installed, 0 | 1),
            "Python advertised more native semantic capability than its bundled authority proves"
        );
        assert!(native_semantic_slots.iter().all(|status| {
            !matches!(status.lifecycle(), CapabilityLifecycle::Installed)
                || !matches!(
                    status.family(),
                    CapabilityFamily::LanguageOracle {
                        profile: LanguageProfile::Python(_),
                        task: LanguageOracleTask::TypeCheck,
                    }
                )
        }));
        assert!(
            native_semantic_slots.iter().all(|status| {
                matches!(status.lifecycle(), CapabilityLifecycle::Unavailable(_))
                    || matches!(status.lifecycle(), CapabilityLifecycle::Installed)
            }),
            "semantic capability advertised an unproved runtime lifecycle"
        );

        // Traverse the outline projection through the real socket one bounded
        // page at a time. Each opaque continuation is returned by the owner
        // and replayed unchanged; the client never manufactures an offset.
        let outline_label = project.to_string_lossy().into_owned();
        let mut paged_client = backend_mcp::Session::connect(&endpoint)
            .unwrap_or_else(|error| panic!("connect paged client: {error}"));
        let mut continuation = None;
        let mut page_count = 0usize;
        let mut outline_row_count = 0usize;
        loop {
            let page_reply = paged_client
                .outline_page(&outline_label, 1, continuation)
                .unwrap_or_else(|error| panic!("read outline page {page_count}: {error}"));
            let page = match page_reply.reply {
                CommandReply::ProjectionPage(page) => page,
                other => panic!("MCP outline page changed shape: {other:?}"),
            };
            assert_eq!(page.snapshot.root.basis().root, readiness.revision().root());
            outline_row_count += page.snapshot.root.rows().len();
            page_count += 1;
            continuation = match page.terminal {
                PageTerminal::More(next) => Some(next),
                PageTerminal::Complete => break,
                PageTerminal::Cancelled => panic!("outline traversal was cancelled"),
            };
        }
        assert!(
            page_count >= 2,
            "outline traversal did not cross a page boundary"
        );
        assert!(outline_row_count >= 2, "polyglot outline omitted rows");
        assert!(
            trustfall_returned_live_typescript,
            "MCP async Trustfall query did not return the live TypeScript declaration: {:?}",
            json_replies[2]
        );
        let desktop_search = backend_desktop::search_endpoint(&endpoint, "turing function", 8)
            .unwrap_or_else(|error| panic!("desktop owner search failed: {error}"));
        let turing_coordinate = format!("{}::src/main.ts:2::turing", project.display());
        assert!(
            desktop_search.contains(&backend_library::RowId::Symbol(
                backend_library::symbol_key(&turing_coordinate)
            )),
            "desktop search did not consume the daemon's reversed-token Tantivy result"
        );

        // The package projection and bounded snapshot hydration must both
        // resolve to the exact owner generation advertised by readiness.
        let packages: ReplyDto = backend_mcp::CommandTransport::request(
            &mut typed_mcp,
            CommandDto::new(2, Command::Packages),
        )
        .unwrap_or_else(|error| panic!("admit desktop seed projection: {error}"));
        let desktop_seed = match packages.reply {
            CommandReply::Packages(snapshot) => snapshot.root,
            other => panic!("MCP packages reply changed shape: {other:?}"),
        };
        let seed_basis = desktop_seed.basis().root;
        assert_eq!(seed_basis, readiness.revision().root());
        let mut desktop_transport = backend_desktop::UnixSubscriptionTransport::connect(&endpoint)
            .unwrap_or_else(|error| panic!("connect desktop transport: {error}"));
        let (desktop_root, desktop_cursor) = desktop_transport
            .bootstrap_root()
            .unwrap_or_else(|error| panic!("hydrate desktop owner generation: {error}"));
        assert_eq!(desktop_root.root(), readiness.revision().root());
        let desktop_basis = desktop_root.basis().root;
        let mut desktop = DesktopModel::try_new_at(desktop_root, desktop_cursor, desktop_basis)
            .unwrap_or_else(|error| panic!("desktop checked owner generation: {error}"));
        let read = desktop.poll_transport(&mut desktop_transport);
        assert!(read.is_ok(), "desktop subscription failed: {read:?}");
        assert_eq!(desktop.root().root(), readiness.revision().root());
        assert!(desktop.root().row_count() >= 15);
        assert!(
            cli_has_certificate && mcp_has_certificate && read.is_ok(),
            "proof-bearing client journey failed: cli_certificate={cli_has_certificate}, mcp_certificate={mcp_has_certificate}, desktop_subscription={:?}",
            read.as_ref().err()
        );

        drop(locald);

        let mut restarted = ChildGuard::spawn("backend-locald", &args, &[]);
        wait_for_socket(&endpoint, &mut restarted);
        let mut restarted_health = ProcessCommand::new(binary("backend-cli"));
        restarted_health
            .arg("--endpoint")
            .arg(&endpoint)
            .arg("--json")
            .arg("health");
        let reopened = bounded_command(restarted_health, "restarted backend-cli health", None);
        assert!(
            reopened.status.success(),
            "restarted health failed: {}",
            String::from_utf8_lossy(&reopened.stderr)
        );
        let reopened_json: serde_json::Value = serde_json::from_slice(&reopened.stdout)
            .unwrap_or_else(|error| panic!("decode restarted CLI JSON: {error}"));
        assert_eq!(
            reopened_json["reply"]["data"]["root"], cli_reply_root,
            "restart changed the immutable product view root"
        );
        assert!(
            workspace.join(backend_extension_turso::FILE_NAME).is_file(),
            "daemon restart did not retain its Turso projection"
        );
        for (symbol, source_path) in [
            ("ferris", "src/lib.rs"),
            ("monty", "src/main.py"),
            ("turing", "src/main.ts"),
            ("Gopher", "src/main.go"),
            ("Duke", "src/Main.java"),
            ("Anders", "src/Main.cs"),
            ("Bjarne", "src/main.cpp"),
        ] {
            let mut query = ProcessCommand::new(binary("backend-cli"));
            query
                .arg("--endpoint")
                .arg(&endpoint)
                .arg("name")
                .arg(symbol);
            let found = bounded_command(query, "restarted backend-cli name", None);
            let output = String::from_utf8_lossy(&found.stdout);
            assert!(
                found.status.success() && output.contains(symbol) && output.contains(source_path),
                "{symbol} disappeared after restart: stdout={} stderr={}",
                output,
                String::from_utf8_lossy(&found.stderr)
            );
        }
        drop(restarted);
        std::fs::remove_dir_all(root)
            .unwrap_or_else(|error| panic!("remove product-client fixture: {error}"));
    }

    // Keep the import in this process-level module tied to the same public
    // root type used by the client assertions above.
    fn _root_type_is_public(root: ViewRoot) -> ViewRoot {
        root
    }
}

#[cfg(not(unix))]
#[test]
fn process_cutover_journeys_require_unix_endpoints() {}
