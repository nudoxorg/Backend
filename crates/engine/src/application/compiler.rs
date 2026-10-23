//! Defines compiler behavior for the `backend-engine` application, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the compiler invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One single-request local compiler specialization over explicit local ownership.

use crate::compiler_attempt_v2::CompilationAttemptId;
use crate::compiler_input_manifest_v2::{CompilationUnitKeyV2, CompilerPackageTargetV2};
use crate::compiler_read_observation_v2::{
    CompilerReadObservationChannelV2, CompilerReadObservationEventClassV2,
    CompilerReadObservationProducerV2, CompilerReadObservationRecorderV2,
};
use crate::driver::{
    AuthorityFailure, CompileControl, CompileOutput, CompileRequest, CompileScratch,
    CompiledFragment, DeclarationScope, PackageDeclarationScopeFault, ResolvedToolchain,
    ToolchainSelection, compile_semantic as compile_fused_semantic, go_authority_projection,
    rust_authority_diagnostic,
};
use crate::publication::{
    OpenSemanticPublicationScratch, PreparedSemanticOutput, PublishControl, PublishedCompilation,
    SemanticImageArtifactFacts, SemanticPublicationScratch, StagedSemanticObjectClaim,
    open_published_semantic, open_semantic_generation, prepare_semantic_bytes,
    publish_semantic_bytes, semantic_generation_requirements,
};
use backend_compile::{
    EmbeddingCacheSession, EmbeddingCoordinates, EmbeddingExecutable, EmbeddingExecutionIdentity,
    EmbeddingNormalization, EmbeddingPurpose, RustCargoWorkspaceFactsV1,
};
use backend_frontend_go::legacy::oracle::GoPackageAuthorityWitness;
use backend_frontend_python::legacy::checker::CheckerError as PythonCheckerError;
use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustAuthorityError, RustCargoTarget, RustCargoTargetSourceClosureV1,
    RustWorkspace, RustWorkspaceEditorBufferObserver,
    RustWorkspaceFile, RustWorkspaceReadFrontierObserver, RustWorkspaceSessionKey,
    RustWorkspaceSessionLane, RustWorkspaceSessionLease,
};
use backend_frontend_typescript::legacy::TypeScriptInvocationModeV1;
use backend_frontend_typescript::{TszFileWorkPolicy, TszProject, TszProjectCheckerExecution};
// Each admitted compiler lane owns its private checker pool. Two compiler
// lanes currently admit at most four file-check workers per owner; parse,
// bind and merge are separate stages with their existing execution controls.
const TSZ_CHECKER_WORKERS_PER_COMPILER_LANE: std::num::NonZeroUsize =
    std::num::NonZeroUsize::new(2).expect("checker worker policy is nonzero");

use backend_library::interface::{
    CompilerAttempt, CompilerCapability, CompilerCause, CompilerFragmentFailure, CompilerReadiness,
    CompilerRequest as ApplicationCompilerRequest, CompilerTerminal, GeneratedArtifact,
    PackageCompilePhase, PackageCompileRequest, PackageDeclarationScopeCause, PackageSourceCause,
    PublicationAuthority, PublicationCause, PublicationPhase, PythonAuthorityFailureKind,
    SemanticImageAccessError, SemanticImageAuthority, SemanticImageSnapshot, SourceAuthority,
};
use backend_semantic::ir::{
    EmbeddingPlaneIdentity, JumboRopeLimits, MAX_SEMANTIC_SEGMENT_BYTES, SemanticBuildIdentity,
    SemanticImageReopenError, SemanticImageView, SemanticInputClaimV2, SemanticInputWitness,
    SemanticIrPlane, SemanticManifestError, SemanticPlane, SemanticPlaneKind,
    SemanticPlaneManifest, SemanticPlaneSegment, SemanticSegmentId,
    SemanticTypedPlaneVerificationTierV2,
};
use backend_semantic::registry::{AdapterRoute, FullRegistry};
use backend_semantic::vocabulary::{AuthorityDiagnosticClass, LanguageProfile, NativeTool};
use backend_store::FileStore;
use backend_store::journal::{
    DurablePublisher, PublicationLimits, PublicationPaths, ShutdownError,
};
use backend_version::Coverage;
use backend_version::ScopeRoot;
use backend_version::{
    ArtifactId, ContentId, IrFragmentDomain, IrFragmentEncoding, SourceFactDomain,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use thiserror::Error;

use backend_frontend_java::legacy::harness::JavaSource;
use backend_replication::{
    ProducedSemanticTypedPlaneContentV3, SemanticTypedPlaneBoundaryPoliciesV3,
    produce_semantic_typed_plane_content_v3,
};

use crate::application::executor::StagedOutputLease;
use crate::application::package_authority::{
    GoPackageAuthority, enter_package_authority_with_go_authority_witness,
    enter_package_authority_with_rust_workspace,
};
use crate::application::{
    AdmittedRustRequestAuthority, LocalCompilerConfig, LocalCompilerControl,
    LocalCompilerExecutionIdentity, LocalCompilerOpenError, LocalCompilerPath,
    LocalCompilerPlaneExecutionIdentity, LocalCompilerPlaneExecutionSeed, LocalCompilerScratch,
    LocalPackageRootSet, PackageAuthorityConfiguration, PackageAuthorityError,
    PackageAuthorityRequest, enter_package_authority, package_source,
    terminal::{compile_terminal, source_authority},
};

/// Default whole-package compact-fragment limit; Go retains complete sibling-package
/// projections per source and uses the finite semantic-image envelope instead.
pub(crate) const MAX_PACKAGE_FRAGMENT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_PACKAGE_SEMANTIC_BYTES: usize = 512 * 1024 * 1024;
/// Maximum optional embedding payload bytes retained for one staged package.
pub const MAX_PACKAGE_EMBEDDING_BYTES: usize = 64 * 1024 * 1024;
const MAX_PACKAGE_AUTHORITY_ERROR_CAUSES: usize = 8;

struct RustEditorBufferObservation<'observer> {
    recorder: &'observer mut CompilerReadObservationRecorderV2,
    editor_producer: CompilerReadObservationProducerV2,
    vfs_producer: CompilerReadObservationProducerV2,
    module_producer: CompilerReadObservationProducerV2,
    authority_fs_producer: CompilerReadObservationProducerV2,
    editor_next_sequence: u64,
    vfs_next_sequence: u64,
    module_next_sequence: u64,
    authority_fs_next_sequence: u64,
}

impl RustWorkspaceEditorBufferObserver for RustEditorBufferObservation<'_> {
    fn observe_editor_buffer(&mut self, relative_path: &Path, contents: &[u8]) {
        let relative_path = relative_path.to_str().unwrap_or_default();
        let sequence = self.editor_next_sequence;
        self.editor_next_sequence = self.editor_next_sequence.saturating_add(1);
        // Observation failure is diagnostic-only. The compiler transaction
        // continues, while the recorder remains poisoned and cannot seal.
        let _ = self.recorder.observe_editor_buffer(
            &self.editor_producer,
            sequence,
            relative_path,
            contents,
        );
    }
}

impl RustWorkspaceReadFrontierObserver for RustEditorBufferObservation<'_> {
    fn observe_editor_buffer(&mut self, relative_path: &Path, contents: &[u8]) {
        RustWorkspaceEditorBufferObserver::observe_editor_buffer(self, relative_path, contents);
    }

    fn observe_ra_vfs_file(&mut self, absolute_path: &str, contents: &[u8]) -> bool {
        let sequence = self.vfs_next_sequence;
        self.vfs_next_sequence = self.vfs_next_sequence.saturating_add(1);
        let path_identity_digest = hash_read_observation_identity(
            b"backend.compiler.ra-vfs-path.v2\0",
            absolute_path.as_bytes(),
        );
        let evidence_digest = *blake3::hash(contents).as_bytes();
        self.recorder
            .observe_opaque_path_event(
                &self.vfs_producer,
                sequence,
                CompilerReadObservationEventClassV2::PresentFile,
                path_identity_digest,
                evidence_digest,
                u64::try_from(contents.len()).unwrap_or(u64::MAX),
            )
            .is_ok()
    }

    fn observe_rustdoc_input(&mut self, absolute_path: &str, contents: &[u8]) -> bool {
        let sequence = self.authority_fs_next_sequence;
        self.authority_fs_next_sequence = self.authority_fs_next_sequence.saturating_add(1);
        let path_identity_digest = hash_read_observation_identity(
            b"backend.compiler.rustdoc-include-path.v2\0",
            absolute_path.as_bytes(),
        );
        let evidence_digest = *blake3::hash(contents).as_bytes();
        self.recorder
            .observe_opaque_path_event(
                &self.authority_fs_producer,
                sequence,
                CompilerReadObservationEventClassV2::PresentFile,
                path_identity_digest,
                evidence_digest,
                u64::try_from(contents.len()).unwrap_or(u64::MAX),
            )
            .is_ok()
    }

    fn observe_unresolved_module_candidate(
        &mut self,
        crate_root_file: &str,
        declaring_file: &str,
        candidate: &str,
    ) -> bool {
        let sequence = self.module_next_sequence;
        self.module_next_sequence = self.module_next_sequence.saturating_add(1);
        let path_identity_digest = hash_read_observation_pair(
            b"backend.compiler.module-declaring-file.v2\0",
            crate_root_file.as_bytes(),
            declaring_file.as_bytes(),
        );
        let evidence_digest = hash_read_observation_identity(
            b"backend.compiler.module-candidate.v2\0",
            candidate.as_bytes(),
        );
        self.recorder
            .observe_opaque_path_event(
                &self.module_producer,
                sequence,
                CompilerReadObservationEventClassV2::ModuleResolution,
                path_identity_digest,
                evidence_digest,
                u64::try_from(
                    crate_root_file
                        .len()
                        .saturating_add(declaring_file.len())
                        .saturating_add(candidate.len()),
                )
                .unwrap_or(u64::MAX),
            )
            .is_ok()
    }
}

fn hash_read_observation_identity(domain: &[u8], value: &[u8]) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(domain);
    digest.update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
    *digest.finalize().as_bytes()
}

fn hash_read_observation_pair(domain: &[u8], first: &[u8], second: &[u8]) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(domain);
    digest.update(&u64::try_from(first.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(first);
    digest.update(
        &u64::try_from(second.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    digest.update(second);
    *digest.finalize().as_bytes()
}

fn begin_rust_workspace_with_observation<'lane>(
    lane: &'lane mut RustWorkspaceSessionLane,
    key: RustWorkspaceSessionKey,
    files: &[RustWorkspaceFile<'_>],
    control: RustAnalysisControl<'_>,
    attempt_id: CompilationAttemptId,
) -> Result<RustWorkspaceSessionLease<'lane>, RustAuthorityError> {
    if !tracing::enabled!(target: "compiler.read_frontier", tracing::Level::DEBUG) {
        return lane.begin(key, files, control);
    }
    let mut recorder = CompilerReadObservationRecorderV2::new(attempt_id);
    let Ok(producer) = recorder.register(CompilerReadObservationChannelV2::EditorOverlay) else {
        return lane.begin(key, files, control);
    };
    let Ok(vfs_producer) = recorder.register(CompilerReadObservationChannelV2::RaVfsLoader) else {
        return lane.begin(key, files, control);
    };
    let Ok(module_producer) =
        recorder.register(CompilerReadObservationChannelV2::RustModuleResolver)
    else {
        return lane.begin(key, files, control);
    };
    let Ok(authority_fs_producer) =
        recorder.register(CompilerReadObservationChannelV2::AuthorityFilesystem)
    else {
        return lane.begin(key, files, control);
    };
    let editor_seal_producer = producer;
    let vfs_seal_producer = vfs_producer;
    let module_seal_producer = module_producer;
    let mut observer = RustEditorBufferObservation {
        recorder: &mut recorder,
        editor_producer: producer,
        vfs_producer,
        module_producer,
        authority_fs_producer,
        editor_next_sequence: 0,
        vfs_next_sequence: 0,
        module_next_sequence: 0,
        authority_fs_next_sequence: 0,
    };
    let (lease, source_summary) =
        lane.begin_with_read_frontier_observer(key, files, control, &mut observer)?;
    let editor_final_count = u64::try_from(files.len()).unwrap_or(u64::MAX);
    drop(observer);
    let _ = recorder.seal(&editor_seal_producer, editor_final_count);
    if !source_summary.truncated && source_summary.unsupported_paths == 0 {
        let _ = recorder.seal(&vfs_seal_producer, source_summary.vfs_files_visited);
        let _ = recorder.seal(
            &module_seal_producer,
            source_summary.module_candidates_visited,
        );
    }
    let report = recorder.report();
    tracing::debug!(
        target: "compiler.read_frontier",
        attempt_counter = report.attempt_counter(),
        observed_events = report.events(),
        observed_bytes = report.bytes(),
        source_vfs_files_visited = source_summary.vfs_files_visited,
        source_vfs_events_delivered = source_summary.vfs_events_delivered,
        source_module_candidates_visited = source_summary.module_candidates_visited,
        source_module_candidate_events_delivered = source_summary.module_candidate_events_delivered,
        source_module_diagnostics_visited = source_summary.module_diagnostics_visited,
        source_rustdoc_inputs_visited = source_summary.rustdoc_inputs_visited,
        source_rustdoc_input_events_delivered = source_summary.rustdoc_input_events_delivered,
        unsupported_paths = source_summary.unsupported_paths,
        source_scan_truncated = source_summary.truncated,
        registered_channels = report.registered_channels(),
        sealed_channels = report.sealed_channels(),
        required_channels = report.required_channels(),
        all_required_producers_sealed = report.all_required_producers_sealed(),
        failure = ?report.failure(),
        missing_channels = ?report.missing_channel_labels(),
        transcript = ?report.transcript_digest(),
        "Rust compiler read observation remains diagnostic until every read channel is proven"
    );
    Ok(lease)
}

fn require_package_embedding_runtime(
    path: &str,
    configured: bool,
    failure: Option<EmbeddingProvisioningFailure>,
    requirement: EmbeddingRequirement,
) -> Result<(), PackageSemanticError> {
    if requirement != EmbeddingRequirement::Required {
        return Ok(());
    }
    if let Some(cause) = failure {
        return Err(PackageSemanticError::EmbeddingProvisioning {
            path: path.into(),
            cause,
        });
    }
    if !configured {
        return Err(PackageSemanticError::Embedding {
            path: path.into(),
            cause: "required embedding runtime is not configured".into(),
        });
    }
    Ok(())
}

/// Whether an embedding failure should leave a typed unavailable plane or fail compilation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EmbeddingRequirement {
    /// Keep IR available and record unavailable embedding coverage on inference failure.
    #[default]
    Optional,
    /// Require every source to produce one validated vector before staging succeeds.
    Required,
}

/// Closed cause for configured embedding provisioning that did not activate.
///
/// This is distinct from no embedding configuration and from an inference failure after a
/// concrete model identity has been activated. It intentionally carries no paths or arbitrary
/// process text.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EmbeddingProvisioningFailure {
    /// A required model artifact was unavailable to the owner.
    ModelUnavailable,
    /// A required tokenizer artifact was unavailable to the owner.
    TokenizerUnavailable,
    /// Model or tokenizer bytes did not match the configured content identities.
    ArtifactIdentityMismatch,
    /// The configured embedding executable was unavailable or did not match its identity.
    ExecutableUnavailable,
    /// Runtime activation or its bounded readiness probe failed.
    ActivationRejected,
    /// The platform could not enforce a required process resource limit.
    ResourceLimitUnavailable,
    /// Private artifact materialization is unsupported on this platform.
    PlatformUnsupported,
}

/// Typed embedding-plane state retained by staged output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StagedEmbeddingStatus<'reason> {
    /// No embedding runtime was configured for this compiler owner.
    NotConfigured,
    /// Embedding was configured, but verified artifacts could not activate a runtime.
    ProvisioningUnavailable {
        /// Closed safe-to-log provisioning cause.
        cause: EmbeddingProvisioningFailure,
    },
    /// Every artifact has a validated vector under this exact identity.
    Available {
        /// Identity of the embedding plane that contains every staged vector.
        identity: EmbeddingPlaneIdentity,
    },
    /// Optional inference failed; IR remains usable and the plane has unavailable coverage.
    Unavailable {
        /// Identity of the embedding plane whose optional inference failed.
        identity: EmbeddingPlaneIdentity,
        /// Closed, safe-to-display explanation for the unavailable plane.
        reason: &'reason str,
    },
}

/// One already-admitted source member of a package compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageSource<'source> {
    relative_path: &'source str,
    source: &'source str,
}

/// Why one exact package source contributed provenance but no semantic artifact.
pub use backend_semantic::ir::SourceCoverageGapCause as PackageSourceCoverageGapCause;
pub use backend_semantic::ir::SourceCoverageGapDiagnostic;

/// Exact source retained in the input frontier but omitted from semantic lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageSourceCoverageGap {
    source: SourceAuthority,
    relative_path: Box<str>,
    cause: PackageSourceCoverageGapCause,
    python_source_status:
        Option<backend_frontend_python::legacy::checker::PythonProjectSourceStatus>,
    native_python_diagnostics:
        Box<[backend_frontend_python::legacy::checker::PythonProjectDiagnostic]>,
    typescript_diagnostic: Option<SourceCoverageGapDiagnostic>,
}

impl PackageSourceCoverageGap {
    /// Exact per-file Python intake refusal, including raw coordinate codec faults.
    #[must_use]
    pub fn python_source_status(
        &self,
    ) -> Option<&backend_frontend_python::legacy::checker::PythonProjectSourceStatus> {
        self.python_source_status.as_ref()
    }

    /// Native diagnostics preserved for this unavailable Python resource.
    #[must_use]
    pub fn native_python_diagnostics(
        &self,
    ) -> &[backend_frontend_python::legacy::checker::PythonProjectDiagnostic] {
        &self.native_python_diagnostics
    }

    /// Exact owner context when TypeScript lowering observed a source-bound type cycle.
    #[must_use]
    pub fn typescript_diagnostic(&self) -> Option<&SourceCoverageGapDiagnostic> {
        self.typescript_diagnostic.as_ref()
    }

    /// Returns the exact source identity and byte length committed by the package input frontier.
    #[must_use]
    pub const fn source(&self) -> SourceAuthority {
        self.source
    }

    /// Returns the exact normalized package-relative path.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Returns the typed reason semantic lowering did not run for this source.
    #[must_use]
    pub const fn cause(&self) -> PackageSourceCoverageGapCause {
        self.cause
    }
}

/// Rebuild rounds one native TypeScript program may spend discarding files that
/// exhaust their own work allowance. Each round skips at least one more file, so
/// this only bounds the repeated planning cost; the program-wide allowance and
/// the deadline bound the checking cost.
const MAX_TYPESCRIPT_FILE_WORK_ROUNDS: u8 = 8;

/// Whole-package restarts allowed when a source's own lowering spends its
/// allowance. The restarted attempt skips that source from its first step.
const MAX_TYPESCRIPT_WORK_RESTARTS: u8 = 2;

/// Exact native TypeScript program files that spent their work allowance in an
/// earlier attempt of the same package compilation.
///
/// Keyed by program index in the configured forest and by the exact program path,
/// both of which are stable across attempts of one package request.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypeScriptWorkGaps {
    programs: BTreeMap<usize, BTreeSet<String>>,
    /// The first attempt's deadline. A restarted attempt inherits it: restarting
    /// must never extend the time the package request was allowed to take.
    deadline: Option<Instant>,
}

impl TypeScriptWorkGaps {
    fn contains(&self, program: usize, path: &str) -> bool {
        self.programs
            .get(&program)
            .is_some_and(|paths| paths.contains(path))
    }

    fn insert(&mut self, program: usize, path: impl Into<String>) {
        self.programs
            .entry(program)
            .or_default()
            .insert(path.into());
    }

    fn program_paths(&self, program: usize) -> BTreeSet<String> {
        self.programs.get(&program).cloned().unwrap_or_default()
    }
}

impl<'source> PackageSource<'source> {
    /// Admits one normalized package-relative source path with its exact bytes.
    ///
    /// # Errors
    /// Returns an error when the path is empty, absolute, contains traversal,
    /// or uses a platform-dependent separator.
    pub fn new(
        relative_path: &'source str,
        source: &'source str,
    ) -> Result<Self, PackageSourceSetError> {
        Self::validate_path(relative_path)?;
        Ok(Self {
            relative_path,
            source,
        })
    }

    pub(super) fn validate_path(relative_path: &str) -> Result<(), PackageSourceSetError> {
        let path = Path::new(relative_path);
        let normalized = !relative_path.is_empty()
            && !relative_path.contains('\\')
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)));
        if !normalized {
            return Err(PackageSourceSetError::InvalidPath);
        }
        Ok(())
    }

    /// Returns the normalized package-relative source path.
    #[must_use]
    pub const fn relative_path(self) -> &'source str {
        self.relative_path
    }

    /// Returns the exact admitted UTF-8 source.
    #[must_use]
    pub const fn source(self) -> &'source str {
        self.source
    }
}

/// Checked package-wide semantic compilation input.
#[derive(Clone, Debug)]
pub struct PackageSourceSet<'source> {
    request: &'source PackageCompileRequest,
    package_target: CompilerPackageTargetV2,
    package_root: &'source Path,
    sources: &'source [PackageSource<'source>],
    python_raw_sources: Option<
        &'source [backend_frontend_python::legacy::checker::PythonProjectBytesSource<'source>],
    >,
    input_claim: Option<SemanticInputWitness>,
    embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
    go_authority_witness: Option<&'source GoPackageAuthorityWitness>,
    go_compile_deadline: Option<std::time::Instant>,
    go_witness_deadline_phase: Option<&'static str>,
    python_environment:
        Option<&'source backend_frontend_python::legacy::checker::PythonEnvironmentInput>,
    native_clang_candidate: bool,
    expected_rust_target_source_closure: Option<[u8; 32]>,
}

impl<'source> PackageSourceSet<'source> {
    /// Admits a complete, strictly ordered source frontier for one package.
    ///
    /// # Errors
    /// Returns an error for a relative package root or an empty, oversized,
    /// duplicate, or unordered source frontier.
    pub fn new(
        request: &'source PackageCompileRequest,
        package_root: &'source Path,
        sources: &'source [PackageSource<'source>],
    ) -> Result<Self, PackageSourceSetError> {
        let package_target = CompilerPackageTargetV2::for_package(request.as_ref().clone());
        Self::new_for_unit(request, &package_target, package_root, sources)
    }

    /// Admits a package source frontier for one exact package-plus-native-unit target.
    ///
    /// # Errors
    /// Returns an error for a target bound to another package or a malformed source frontier.
    pub fn new_for_unit(
        request: &'source PackageCompileRequest,
        package_target: &CompilerPackageTargetV2,
        package_root: &'source Path,
        sources: &'source [PackageSource<'source>],
    ) -> Result<Self, PackageSourceSetError> {
        if !package_root.is_absolute() {
            return Err(PackageSourceSetError::RelativeRoot);
        }
        if package_target.package() != request.as_ref() {
            return Err(PackageSourceSetError::PackageTargetMismatch);
        }
        match package_target.unit_key() {
            CompilationUnitKeyV2::PackageRoot => {}
            CompilationUnitKeyV2::RustCrate { root, .. }
                if request.target.profile.language()
                    == backend_semantic::vocabulary::Language::Rust
                    && sources
                        .iter()
                        .any(|source| source.relative_path() == root.as_ref()) => {}
            CompilationUnitKeyV2::CSharpProject { .. }
                if request.target.profile.language()
                    == backend_semantic::vocabulary::Language::CSharp
                    && sources.iter().any(|source| {
                        unit_source_matches(package_target.unit_key(), source.relative_path())
                    }) => {}
            _ => return Err(PackageSourceSetError::CompilationUnitMismatch),
        }
        if sources.is_empty() || sources.len() > crate::application::MAX_MANIFEST_ENTRIES {
            return Err(PackageSourceSetError::Cardinality {
                observed: sources.len(),
                maximum: crate::application::MAX_MANIFEST_ENTRIES,
            });
        }
        if sources
            .windows(2)
            .any(|pair| pair[0].relative_path >= pair[1].relative_path)
        {
            return Err(PackageSourceSetError::Order);
        }
        Ok(Self {
            request,
            package_target: package_target.clone(),
            package_root,
            sources,
            python_raw_sources: None,
            input_claim: None,
            embedding_provisioning_failure: None,
            go_authority_witness: None,
            go_compile_deadline: None,
            go_witness_deadline_phase: None,
            python_environment: None,
            native_clang_candidate: false,
            expected_rust_target_source_closure: None,
        })
    }

    /// Marks this exact checked package frontier as compiler-only native Clang input.
    /// The owned-runtime admission boundary requires selected-frontier permits before
    /// calling this method; it is not available to public callers.
    pub(crate) fn with_native_clang_candidate(mut self) -> Result<Self, PackageSourceSetError> {
        if !matches!(
            self.request.target.profile,
            LanguageProfile::C(_) | LanguageProfile::Cxx(_)
        ) {
            return Err(PackageSourceSetError::NativeClangProfileMismatch);
        }
        self.native_clang_candidate = true;
        Ok(self)
    }

    /// Whether this package crossed the distinct checked native-candidate admission route.
    #[must_use]
    pub const fn is_native_clang_candidate(&self) -> bool {
        self.native_clang_candidate
    }

    pub(super) fn with_python_raw_sources(
        mut self,
        raw_sources: &'source [backend_frontend_python::legacy::checker::PythonProjectBytesSource<'source>],
    ) -> Result<Self, PackageSourceSetError> {
        if !matches!(
            self.request.target.profile,
            backend_semantic::vocabulary::LanguageProfile::Python(_)
        ) {
            return Err(PackageSourceSetError::RawSourceProfileMismatch);
        }
        if raw_sources.is_empty() || raw_sources.len() > crate::application::MAX_MANIFEST_ENTRIES {
            return Err(PackageSourceSetError::Cardinality {
                observed: raw_sources.len(),
                maximum: crate::application::MAX_MANIFEST_ENTRIES,
            });
        }
        if raw_sources
            .windows(2)
            .any(|pair| pair[0].relative_path >= pair[1].relative_path)
        {
            return Err(PackageSourceSetError::Order);
        }
        for raw in raw_sources {
            PackageSource::validate_path(raw.relative_path)?;
        }
        for decoded in self.sources {
            let raw = raw_sources
                .binary_search_by_key(&decoded.relative_path, |raw| raw.relative_path)
                .ok()
                .map(|index| &raw_sources[index])
                .ok_or(PackageSourceSetError::RawSourceMismatch)?;
            if raw.source != decoded.source.as_bytes() {
                return Err(PackageSourceSetError::RawSourceMismatch);
            }
        }
        self.python_raw_sources = Some(raw_sources);
        Ok(self)
    }

    pub(super) fn with_python_environment(
        mut self,
        environment: Option<
            &'source backend_frontend_python::legacy::checker::PythonEnvironmentInput,
        >,
    ) -> Self {
        self.python_environment = environment;
        self
    }

    pub(crate) fn with_expected_rust_target_source_closure(
        mut self,
        identity: [u8; 32],
    ) -> Result<Self, PackageSourceSetError> {
        if !matches!(self.package_target.unit_key(), CompilationUnitKeyV2::RustCrate { .. })
            || self.request.target.profile.language()
                != backend_semantic::vocabulary::Language::Rust
        {
            return Err(PackageSourceSetError::CompilationUnitMismatch);
        }
        self.expected_rust_target_source_closure = Some(identity);
        Ok(self)
    }

    /// Attaches an opaque input-manifest claim to the exact admitted source frontier.
    ///
    /// This records caller-provided provenance bytes only. It does not prove the manifest
    /// preimage or grant completeness authority; those checks remain at the input admission
    /// boundary.
    #[must_use]
    pub fn with_input_claim(mut self, input: SemanticInputWitness) -> Self {
        self.input_claim = Some(input);
        self
    }

    pub(super) fn with_go_compile_control(
        mut self,
        deadline: Option<std::time::Instant>,
        witness_deadline_phase: Option<&'static str>,
    ) -> Self {
        self.go_compile_deadline = deadline;
        self.go_witness_deadline_phase = witness_deadline_phase;
        self
    }

    pub(crate) fn with_go_authority_witness(
        mut self,
        witness: &'source GoPackageAuthorityWitness,
    ) -> Self {
        self.go_authority_witness = Some(witness);
        self
    }

    /// Records a configured embedding runtime that failed before activation.
    ///
    /// The caller must use this only when embedding was requested and provisioning failed. A
    /// genuinely absent configuration remains `None` and reports `NotConfigured`.
    #[must_use]
    pub(crate) fn with_embedding_provisioning_failure(
        mut self,
        cause: EmbeddingProvisioningFailure,
    ) -> Self {
        self.embedding_provisioning_failure = Some(cause);
        self
    }

    fn compilation_sources(&self) -> impl Iterator<Item = PackageSource<'source>> + '_ {
        self.sources.iter().copied().filter(move |source| {
            unit_source_matches(self.package_target.unit_key(), source.relative_path())
        })
    }
}

fn unit_source_matches(unit: &CompilationUnitKeyV2, relative_path: &str) -> bool {
    match unit {
        CompilationUnitKeyV2::PackageRoot => true,
        CompilationUnitKeyV2::RustCrate { root, .. } => relative_path == root.as_ref(),
        CompilationUnitKeyV2::CSharpProject { project_path } => {
            let Some((directory, _)) = project_path.rsplit_once('/') else {
                return true;
            };
            directory.is_empty()
                || relative_path
                    .strip_prefix(directory)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        }
        _ => false,
    }
}

fn package_source_input_witness(package: &PackageSourceSet<'_>) -> SemanticInputWitness {
    let mut input = blake3::Hasher::new();
    input.update(b"backend.compiler.package-source-frontier.v1\0");
    if let CompilationUnitKeyV2::RustCrate { name, root } = package.package_target.unit_key() {
        input.update(b"rust-cargo-target-unit.v1\0");
        input.update(&(name.len() as u64).to_be_bytes());
        input.update(name.as_bytes());
        input.update(&(root.len() as u64).to_be_bytes());
        input.update(root.as_bytes());
    }
    if let Some(environment) = package.python_environment {
        input.update(b"installed-environment-cohort.v1\0");
        input.update(&environment.identity());
    }
    if let Some(raw_sources) = package.python_raw_sources {
        for source in raw_sources {
            let path = source.relative_path.as_bytes();
            input.update(&(path.len() as u64).to_be_bytes());
            input.update(path);
            input.update(&(source.source.len() as u64).to_be_bytes());
            input.update(source.source);
        }
    } else {
        // A Rust crate's selected target compiles only its authority-proven
        // closure, but all admitted package bytes remain input provenance.
        // `compilation_sources()` is intentionally root-only for RustCrate.
        let sources = if matches!(
            package.package_target.unit_key(),
            CompilationUnitKeyV2::RustCrate { .. }
        ) {
            package.sources.to_vec()
        } else {
            package.compilation_sources().collect::<Vec<_>>()
        };
        for source in sources {
            let path = source.relative_path().as_bytes();
            let contents = source.source().as_bytes();
            input.update(&(path.len() as u64).to_be_bytes());
            input.update(path);
            input.update(&(contents.len() as u64).to_be_bytes());
            input.update(contents);
        }
    }
    let root = *input.finalize().as_bytes();
    SemanticInputWitness::claimed_state(root, ScopeRoot::from_bytes(root), Coverage::Partial)
}

fn semantic_embedding_identity(
    identity: EmbeddingExecutionIdentity,
) -> Result<EmbeddingPlaneIdentity, Box<str>> {
    let mut recipe = blake3::Hasher::new_derive_key("backend.engine.embedding-plane-launch.v1");
    recipe.update(&identity.recipe());
    recipe.update(&identity.launch_configuration());
    let recipe = *recipe.finalize().as_bytes();
    let normalization = match identity.normalization() {
        EmbeddingNormalization::None => backend_semantic::ir::EmbeddingNormalization::None,
        EmbeddingNormalization::L2 => backend_semantic::ir::EmbeddingNormalization::L2,
    };
    EmbeddingPlaneIdentity::new(
        identity.model(),
        identity.model_version(),
        identity.tokenizer(),
        identity.dimension(),
        normalization,
        identity.executable(),
        recipe,
    )
    .map_err(|error| error.to_string().into_boxed_str())
}

fn stage_embedding_artifact(
    identity: EmbeddingExecutionIdentity,
    relative_path: &str,
    coordinates: &EmbeddingCoordinates,
    memory: &backend_compile::EmbeddingMemoryBudget,
) -> Result<StagedEmbeddingArtifact, Box<str>> {
    if coordinates.purpose() != EmbeddingPurpose::Document
        || coordinates.recipe() != identity.recipe()
        || coordinates.model().as_bytes() != identity.model()
        || coordinates.tokenizer().as_bytes() != identity.tokenizer()
        || coordinates.normalization() != identity.normalization()
        || u32::try_from(coordinates.values().len()).ok() != Some(identity.dimension())
    {
        return Err("embedding response identity differs from its activated runtime".into());
    }
    let memory_lease = memory
        .reserve(
            u64::try_from(coordinates.canonical_payload_len())
                .map_err(|error| error.to_string().into_boxed_str())?,
        )
        .map_err(|error| error.to_string().into_boxed_str())?;
    let mut payload = Vec::new();
    payload
        .try_reserve_exact(coordinates.canonical_payload_len())
        .map_err(|error| error.to_string().into_boxed_str())?;
    payload.resize(coordinates.canonical_payload_len(), 0);
    coordinates
        .encode_canonical_payload(&mut payload)
        .map_err(|error| error.to_string().into_boxed_str())?;
    let mut key = blake3::Hasher::new_derive_key("backend.semantic.embedding.source-key.v1");
    key.update(&(relative_path.len() as u64).to_be_bytes());
    key.update(relative_path.as_bytes());
    Ok(StagedEmbeddingArtifact {
        key: *key.finalize().as_bytes(),
        payload: payload.into_boxed_slice(),
        _memory_lease: memory_lease,
    })
}

/// Package-source-frontier admission failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PackageSourceSetError {
    /// Raw source resources may only enter the explicit Python intake lane.
    #[error("raw source resources require the Python profile")]
    RawSourceProfileMismatch,
    /// The decoded and raw frontiers did not describe the same exact bytes.
    #[error("decoded source differs from the retained raw Python frontier")]
    RawSourceMismatch,
    /// The distinct native Clang candidate route was requested for a non-C/C++ profile.
    #[error("native Clang source candidates require a C or C++ profile and UTF-8 bytes")]
    NativeClangProfileMismatch,
    /// The distinct native C# candidate route was requested for a non-C# profile.
    #[error("native C# source candidates require a C# profile and UTF-8 bytes")]
    NativeCSharpProfileMismatch,
    /// Package roots must have host-independent absolute identity.
    #[error("package source root is relative")]
    RelativeRoot,
    /// A source path was not a normalized relative path.
    #[error("package source path is not normalized and relative")]
    InvalidPath,
    /// The source frontier was empty or exceeded the package manifest bound.
    #[error("package source frontier has {observed} members; maximum is {maximum}")]
    Cardinality {
        /// Observed source count.
        observed: usize,
        /// Maximum admitted source count.
        maximum: usize,
    },
    /// Source paths were duplicated or unordered.
    #[error("package source frontier is not strictly ordered")]
    Order,
    /// The exact unit target names a different canonical package than the request.
    #[error("compilation-unit target belongs to a different package")]
    PackageTargetMismatch,
    /// The native compilation unit is unsupported for this profile or absent from the frontier.
    #[error("compilation-unit target is incompatible with the profile or source frontier")]
    CompilationUnitMismatch,
    /// A name-only Cargo target selector was empty or used with a non-Rust profile.
    #[error("Rust Cargo target name selector is invalid for this package profile")]
    InvalidRustCargoTargetName,
}

/// One atomically published package generation and its reopened semantic images.
#[derive(Debug)]
pub struct PublishedSemanticPackage {
    /// Locality-independent generation, manifest, and binding identities.
    pub publication: PublishedCompilation,
    /// Complete semantic images copied only after the publication owner reopened the closure.
    pub images: Box<[SemanticImageSnapshot]>,
    /// Exact package source-frontier members that had no active semantic scope.
    ///
    /// Their bytes remain committed by the package input witness even though
    /// they have no corresponding artifact in `publication` or `images`.
    pub coverage_gaps: Box<[PackageSourceCoverageGap]>,
}

impl PublishedSemanticPackage {
    /// Returns exact source-frontier members retained in provenance without semantic artifacts.
    #[must_use]
    pub fn coverage_gaps(&self) -> &[PackageSourceCoverageGap] {
        &self.coverage_gaps
    }
}

/// Canonical member facts in one complete, not-yet-selected semantic compiler output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StagedSemanticArtifact {
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    semantic_image: SemanticImageArtifactFacts,
    fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    fragment_claim: StagedSemanticObjectClaim,
    semantic_claim: StagedSemanticObjectClaim,
}

impl StagedSemanticArtifact {
    /// Returns the exact source authority bound by the compact and semantic output.
    #[must_use]
    pub const fn source(self) -> backend_semantic::ir::SourceIdentity {
        self.source
    }

    /// Returns the exact toolchain/environment recipe bound by the output bytes.
    #[must_use]
    pub const fn recipe(self) -> backend_semantic::vocabulary::CompileRecipeFact {
        self.recipe
    }

    /// Returns the exact compact fragment identity derived from its canonical bytes.
    #[must_use]
    pub const fn fragment(self) -> ArtifactId<IrFragmentEncoding, IrFragmentDomain> {
        self.fragment
    }

    /// Returns the complete semantic image identity and byte length.
    #[must_use]
    pub const fn semantic_image(self) -> SemanticImageArtifactFacts {
        self.semantic_image
    }

    /// Returns the fragment's exact generation entry claim.
    #[must_use]
    pub const fn fragment_claim(self) -> StagedSemanticObjectClaim {
        self.fragment_claim
    }

    /// Returns the semantic image's exact generation entry claim.
    #[must_use]
    pub const fn semantic_claim(self) -> StagedSemanticObjectClaim {
        self.semantic_claim
    }
}

/// One borrowed canonical object payload ready for a bounded artifact sink.
#[derive(Clone, Copy, Debug)]
pub struct StagedSemanticOutputObject<'bytes> {
    claim: StagedSemanticObjectClaim,
    bytes: &'bytes [u8],
}

impl<'bytes> StagedSemanticOutputObject<'bytes> {
    /// Returns the exact object key, parent, and typed reference in the verified generation.
    #[must_use]
    pub const fn claim(self) -> StagedSemanticObjectClaim {
        self.claim
    }

    /// Returns canonical object bytes for this exact claim.
    #[must_use]
    pub const fn bytes(self) -> &'bytes [u8] {
        self.bytes
    }
}

/// Complete canonical compiler output prepared for transport or storage, before local selection.
///
/// This value owns the bounded lane output and keeps its global byte-credit lease alive until it
/// is dropped. The output object order and references are the same ones used by local publication.
pub struct StagedSemanticPackage {
    staged: StagedPackageCompilation,
    prepared: PreparedSemanticOutput,
    artifacts: Box<[StagedSemanticArtifact]>,
    package_identity: [u8; 32],
    target_identity: ContentId<backend_version::CompilationTargetDomain>,
    profile: backend_semantic::vocabulary::LanguageProfile,
    stage: backend_semantic::vocabulary::Stage,
    input: SemanticInputWitness,
    execution_identity: Option<LocalCompilerExecutionIdentity>,
    plane_execution_identity: Option<LocalCompilerPlaneExecutionIdentity>,
    _budget_lease: Option<StagedOutputLease>,
}

/// One borrowed, identity-checked semantic plane segment in a staged output.
#[derive(Clone, Copy, Debug)]
pub struct StagedVersionedPlaneSegment<'bytes> {
    kind: SemanticPlaneKind,
    id: SemanticSegmentId,
    payload: &'bytes [u8],
}

impl<'bytes> StagedVersionedPlaneSegment<'bytes> {
    /// Returns the exact IR or embedding plane identity committed by the segment ID.
    #[must_use]
    pub const fn kind(self) -> SemanticPlaneKind {
        self.kind
    }

    /// Returns the ID computed from the exact plane, key range, and borrowed payload bytes.
    #[must_use]
    pub const fn id(self) -> SemanticSegmentId {
        self.id
    }

    /// Returns the canonical payload without copying it.
    #[must_use]
    pub const fn payload(self) -> &'bytes [u8] {
        self.payload
    }
}

/// One exact artifact's canonical plane manifest and borrowed segment payloads.
#[derive(Clone, Debug)]
pub struct StagedVersionedPlaneArtifact<'bytes> {
    artifact_ordinal: usize,
    manifest_bytes: Box<[u8]>,
    segments: Box<[StagedVersionedPlaneSegment<'bytes>]>,
}

impl<'bytes> StagedVersionedPlaneArtifact<'bytes> {
    /// Returns the artifact ordinal in the canonical compiler manifest.
    #[must_use]
    pub const fn artifact_ordinal(&self) -> usize {
        self.artifact_ordinal
    }

    /// Returns exact canonical semantic-plane manifest bytes.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// Returns the number of payloads in manifest order.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Borrows one exact manifest segment and its payload.
    #[must_use]
    pub fn segment(&self, ordinal: usize) -> Option<StagedVersionedPlaneSegment<'bytes>> {
        self.segments.get(ordinal).copied()
    }
}

/// Complete per-artifact versioned-plane output borrowed from one staged package.
#[derive(Clone, Debug)]
pub struct StagedVersionedPlanes<'bytes> {
    artifacts: Box<[StagedVersionedPlaneArtifact<'bytes>]>,
    embedding_status: StagedEmbeddingStatus<'bytes>,
}

impl<'bytes> StagedVersionedPlanes<'bytes> {
    /// Returns per-artifact manifests in canonical compilation-manifest order.
    #[must_use]
    pub fn artifacts(&self) -> &[StagedVersionedPlaneArtifact<'bytes>] {
        &self.artifacts
    }

    /// Returns explicit embedding availability for the exact staged package.
    #[must_use]
    pub const fn embedding_status(&self) -> StagedEmbeddingStatus<'bytes> {
        self.embedding_status
    }
}

/// Seven-family c007 content streamed from one exact staged compiler package.
///
/// This value retains producer pins and the owner-minted in-memory compilation
/// attempt. It is a content receipt only: the embedded producer manifest
/// preserves the staged input coverage claim and does not authorize reuse.
/// Durable publication must bind it to the candidate attempt that owns the
/// staged package.
pub struct StagedTypedPlaneContentV3 {
    compilation_attempt_id: CompilationAttemptId,
    input_witness: SemanticInputWitness,
    artifacts: Box<[StagedTypedPlaneContentArtifact]>,
}

impl StagedTypedPlaneContentV3 {
    /// Per-image content receipts in the exact canonical compilation order.
    #[must_use]
    pub fn artifacts(&self) -> &[StagedTypedPlaneContentArtifact] {
        &self.artifacts
    }

    /// Exact live witness retained by the staging owner.
    #[must_use]
    pub const fn input_witness(&self) -> SemanticInputWitness {
        self.input_witness
    }
}

/// One c007 output whose reader was borrowed from the named staged artifact.
pub struct StagedTypedPlaneContentArtifact {
    artifact_ordinal: usize,
    semantic_image: SemanticImageArtifactFacts,
    build: SemanticBuildIdentity,
    input_witness: SemanticInputWitness,
    produced: ProducedSemanticTypedPlaneContentV3,
}

impl StagedTypedPlaneContentArtifact {
    /// Ordinal in the exact staged canonical compiler manifest.
    #[must_use]
    pub const fn artifact_ordinal(&self) -> usize {
        self.artifact_ordinal
    }

    /// Complete semantic image identity and byte length retained by staging.
    #[must_use]
    pub const fn semantic_image(&self) -> SemanticImageArtifactFacts {
        self.semantic_image
    }

    /// Exact runtime/toolchain/source recipe used by this staged image.
    #[must_use]
    pub const fn build(&self) -> SemanticBuildIdentity {
        self.build
    }

    /// Original staged witness, including its unmodified coverage state.
    #[must_use]
    pub const fn input_witness(&self) -> SemanticInputWitness {
        self.input_witness
    }

    /// Claim-only c007 content and durable FileStore receipts.
    #[must_use]
    pub const fn produced(&self) -> &ProducedSemanticTypedPlaneContentV3 {
        &self.produced
    }
}

/// Rejection while binding bounded versioned-plane metadata to a staged package.
#[derive(Debug, Error)]
pub enum StagedVersionedPlaneError {
    /// The staged result has neither portable nor host-local admitted runtime identity.
    #[error("semantic-plane output requires an admitted compiler runtime identity")]
    ExecutionIdentityUnavailable,
    /// The runtime identity does not govern the exact staged package target/profile/stage.
    #[error("semantic-plane identity does not match the staged compiler target: {0}")]
    ExecutionIdentityMismatch(StagedIdentityFacet),
    /// One canonical semantic image was empty and cannot form a nonempty segment range.
    #[error("semantic image {artifact_ordinal} is empty")]
    EmptySemanticImage {
        /// Zero-based artifact position whose semantic image contained no rows.
        artifact_ordinal: usize,
    },
    /// The segment constructor did not return an admitted ID for its exact in-memory payload.
    #[error("semantic segment claim did not contain its computed payload ID")]
    MissingAdmittedSegmentId,
    /// Exact semantic plane metadata or segment validation failed.
    #[error(transparent)]
    Manifest(#[from] SemanticManifestError),
    /// A bounded metadata allocation failed.
    #[error("semantic-plane output allocation failed")]
    Allocation(#[source] std::collections::TryReserveError),
}

/// The one staged identity fact that failed to bind compiler output to its admitted runtime.
///
/// Every staged-plane producer validates through [`StagedSemanticPackage::admitted_runtime`] and
/// [`StagedSemanticPackage::admitted_artifact_recipe`], so a refusal always names the exact fact
/// instead of collapsing several distinct checks into one message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StagedIdentityFacet {
    /// The host-local plane identity was sealed over another input/read witness.
    InputWitness,
    /// The runtime identity governs another package or translation-unit target.
    Target,
    /// The runtime identity governs another language profile.
    Profile,
    /// The runtime identity governs another semantic stage.
    Stage,
    /// One artifact was compiled under another language profile.
    ArtifactProfile {
        /// Zero-based staged artifact ordinal.
        artifact: usize,
    },
    /// One artifact was compiled at another semantic stage.
    ArtifactStage {
        /// Zero-based staged artifact ordinal.
        artifact: usize,
    },
    /// One artifact was compiled by another native tool family.
    ArtifactTool {
        /// Zero-based staged artifact ordinal.
        artifact: usize,
        /// Tool family named by the admitted runtime identity.
        expected: NativeTool,
        /// Tool family recorded by the artifact recipe.
        actual: NativeTool,
    },
    /// One artifact was compiled by another resolved toolchain.
    ArtifactToolchain {
        /// Zero-based staged artifact ordinal.
        artifact: usize,
        /// Toolchain identity named by the admitted runtime identity.
        expected: [u8; 32],
        /// Toolchain identity recorded by the artifact recipe.
        actual: [u8; 32],
    },
    /// The staged artifact table or image plan omits one artifact.
    ArtifactImage {
        /// Zero-based staged artifact ordinal.
        artifact: usize,
    },
    /// The embedding output omits one artifact's vector.
    EmbeddingArtifact {
        /// Zero-based staged artifact ordinal.
        artifact: usize,
    },
}

impl std::fmt::Display for StagedIdentityFacet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn short(bytes: &[u8; 32]) -> String {
            bytes
                .iter()
                .take(6)
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }
        match self {
            Self::InputWitness => formatter.write_str("host-local plane input witness differs"),
            Self::Target => {
                formatter.write_str("runtime identity names another compilation target")
            }
            Self::Profile => formatter.write_str("runtime identity names another language profile"),
            Self::Stage => formatter.write_str("runtime identity names another semantic stage"),
            Self::ArtifactProfile { artifact } => {
                write!(
                    formatter,
                    "artifact {artifact} was compiled under another language profile"
                )
            }
            Self::ArtifactStage { artifact } => {
                write!(
                    formatter,
                    "artifact {artifact} was compiled at another semantic stage"
                )
            }
            Self::ArtifactTool {
                artifact,
                expected,
                actual,
            } => write!(
                formatter,
                "artifact {artifact} was compiled by {actual:?}, but the admitted runtime is {expected:?}"
            ),
            Self::ArtifactToolchain {
                artifact,
                expected,
                actual,
            } => write!(
                formatter,
                "artifact {artifact} toolchain {}… differs from the admitted runtime toolchain {}…",
                short(actual),
                short(expected)
            ),
            Self::ArtifactImage { artifact } => {
                write!(
                    formatter,
                    "artifact {artifact} has no staged semantic image"
                )
            }
            Self::EmbeddingArtifact { artifact } => {
                write!(
                    formatter,
                    "artifact {artifact} has no staged embedding vector"
                )
            }
        }
    }
}

/// Runtime facts shared by every staged-plane producer after identity admission.
#[derive(Clone, Copy, Debug)]
struct AdmittedStagedRuntime {
    target: ContentId<backend_version::CompilationTargetDomain>,
    profile: backend_semantic::vocabulary::LanguageProfile,
    stage: backend_semantic::vocabulary::Stage,
    tool: NativeTool,
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
}

impl AdmittedStagedRuntime {
    fn admit_recipe(
        &self,
        artifact: usize,
        recipe: backend_semantic::vocabulary::CompileRecipeFact,
    ) -> Result<(), StagedIdentityFacet> {
        if recipe.profile != self.profile {
            return Err(StagedIdentityFacet::ArtifactProfile { artifact });
        }
        if recipe.stage != self.stage {
            return Err(StagedIdentityFacet::ArtifactStage { artifact });
        }
        if recipe.tool != self.tool {
            return Err(StagedIdentityFacet::ArtifactTool {
                artifact,
                expected: self.tool,
                actual: recipe.tool,
            });
        }
        if recipe.toolchain.as_ref() != &self.toolchain {
            return Err(StagedIdentityFacet::ArtifactToolchain {
                artifact,
                expected: self.toolchain,
                actual: *recipe.toolchain.as_ref(),
            });
        }
        Ok(())
    }

    /// Build identity is per semantic image. `CompileRecipeFact` binds the exact source image,
    /// while the remaining build fields retain the runtime, toolchain, environment, and platform
    /// authorities. The invocation/local-plane recipe is not interchangeable with this
    /// source-bound identity and must not be placed in this field.
    fn build_identity(
        &self,
        package_identity: [u8; 32],
        recipe: backend_semantic::vocabulary::CompileRecipeFact,
    ) -> SemanticBuildIdentity {
        SemanticBuildIdentity::new(
            package_identity,
            *self.target.as_ref(),
            self.profile,
            self.stage,
            *recipe.identity.as_ref(),
            self.toolchain,
            self.environment,
            self.target_platform,
        )
    }
}

/// Refusal from [`StagedSemanticPackage::admitted_runtime`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StagedRuntimeRefusal {
    Unavailable,
    Mismatch(StagedIdentityFacet),
}

impl From<StagedRuntimeRefusal> for StagedVersionedPlaneError {
    fn from(refusal: StagedRuntimeRefusal) -> Self {
        match refusal {
            StagedRuntimeRefusal::Unavailable => Self::ExecutionIdentityUnavailable,
            StagedRuntimeRefusal::Mismatch(facet) => Self::ExecutionIdentityMismatch(facet),
        }
    }
}

impl From<StagedRuntimeRefusal> for StagedTypedPlaneContentError {
    fn from(refusal: StagedRuntimeRefusal) -> Self {
        match refusal {
            StagedRuntimeRefusal::Unavailable => Self::ExecutionIdentityUnavailable,
            StagedRuntimeRefusal::Mismatch(facet) => Self::ExecutionIdentityMismatch(facet),
        }
    }
}

/// Failure while streaming stable-key c007 content from staged readers.
#[derive(Debug, Error)]
pub enum StagedTypedPlaneContentError {
    /// The staged result has neither portable nor host-local admitted runtime identity.
    #[error("typed semantic content requires an admitted compiler runtime identity")]
    ExecutionIdentityUnavailable,
    /// The runtime, input, or source recipe does not govern the staged package.
    #[error("typed semantic content identity does not match the staged compiler output: {0}")]
    ExecutionIdentityMismatch(StagedIdentityFacet),
    /// One c007 artifact did not retain the exact staged image or witness claim.
    #[error("typed semantic content differs from its exact staged image or input claim")]
    ContentIdentityMismatch,
    /// Bounded output receipt allocation failed.
    #[error("typed semantic content receipt allocation failed")]
    Allocation(#[source] std::collections::TryReserveError),
    /// Full-image validation or the exact reader callback failed.
    #[error(transparent)]
    Reader(#[from] StagedSemanticReaderError<io::Error>),
    /// The shared stable-key producer rejected the exact borrowed reader.
    #[error("stable-key typed semantic producer failed: {0}")]
    Producer(String),
}

/// Work performed before lending one staged canonical image to a reader callback.
#[must_use]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StagedSemanticReaderMetrics {
    image_validation_count: u64,
    full_image_validation_input_bytes: u64,
    canonical_image_copy_bytes: u64,
}

impl StagedSemanticReaderMetrics {
    /// Number of complete image validations performed for this callback.
    #[must_use]
    pub const fn image_validation_count(self) -> u64 {
        self.image_validation_count
    }

    /// Exact canonical image bytes presented to the full-image validator.
    #[must_use]
    pub const fn full_image_validation_input_bytes(self) -> u64 {
        self.full_image_validation_input_bytes
    }

    /// Full canonical image bytes copied to construct the borrowed reader.
    #[must_use]
    pub const fn canonical_image_copy_bytes(self) -> u64 {
        self.canonical_image_copy_bytes
    }
}

/// Failure while borrowing one validated canonical reader from a staged package.
#[derive(Debug, Error)]
pub enum StagedSemanticReaderError<CallbackError: std::error::Error + 'static> {
    /// The requested canonical artifact ordinal has no staged semantic image.
    #[error("staged semantic reader artifact ordinal is unavailable")]
    ArtifactOrdinal,
    /// The selected canonical image region is inconsistent with the staged package plan.
    #[error("staged semantic reader image region is unavailable")]
    ImageRegion,
    /// The canonical image could not be completely validated before borrowing.
    #[error(transparent)]
    Reopen(#[from] SemanticImageReopenError),
    /// A family encoder or its bounded output sink rejected the callback.
    #[error("staged semantic reader callback failed")]
    Callback(#[source] CallbackError),
}

fn typescript_snapshot_outcome_reusable(
    mut gaps: impl Iterator<Item = PackageSourceCoverageGapCause>,
    embedding: StagedEmbeddingStatus<'_>,
) -> bool {
    // Add is also the retry surface. Keep interrupted/refused semantic work
    // and activated-runtime inference failures eligible for another attempt.
    matches!(
        embedding,
        StagedEmbeddingStatus::NotConfigured
            | StagedEmbeddingStatus::Available { .. }
            | StagedEmbeddingStatus::ProvisioningUnavailable { .. }
    ) && gaps.all(|cause| {
        cause == PackageSourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram
    })
}

#[cfg(test)]
mod typescript_snapshot_outcome_tests {
    use super::*;

    #[test]
    fn snapshot_outcomes_preserve_add_retry_for_interrupted_sources_and_inference() {
        use PackageSourceCoverageGapCause as Gap;
        let identity = EmbeddingPlaneIdentity::new(
            [1; 32],
            [2; 32],
            [3; 32],
            1,
            backend_semantic::ir::EmbeddingNormalization::None,
            [4; 32],
            [5; 32],
        )
        .unwrap();
        for embedding in [
            StagedEmbeddingStatus::NotConfigured,
            StagedEmbeddingStatus::Available { identity },
            StagedEmbeddingStatus::ProvisioningUnavailable {
                cause: EmbeddingProvisioningFailure::ActivationRejected,
            },
        ] {
            assert!(typescript_snapshot_outcome_reusable(
                [].into_iter(),
                embedding
            ));
            assert!(typescript_snapshot_outcome_reusable(
                [Gap::TypeScriptSourceExcludedFromConfiguredProgram].into_iter(),
                embedding,
            ));
            for gap in [
                Gap::TypeScriptSourceWorkBudgetExhausted,
                Gap::CompileDeadlineExceeded,
                Gap::SourceAuthorityRefused,
                Gap::SourceRejectedByNativeCompiler,
                Gap::SourceNotRepresentable,
                Gap::RustSourceOutsideActiveCargoTarget,
                Gap::PythonSourceSyntaxUnavailable,
                Gap::PythonSourceEncodingUnavailable,
                Gap::PythonSourceDependencyUnavailable,
            ] {
                assert!(
                    !typescript_snapshot_outcome_reusable(
                        [Gap::TypeScriptSourceExcludedFromConfiguredProgram, gap,].into_iter(),
                        embedding
                    ),
                    "Add must retry {gap:?}"
                );
            }
        }
        let failed = StagedEmbeddingStatus::Unavailable {
            identity,
            reason: "transient inference failure",
        };
        assert!(!typescript_snapshot_outcome_reusable(
            [].into_iter(),
            failed
        ));
        assert!(!typescript_snapshot_outcome_reusable(
            [Gap::TypeScriptSourceExcludedFromConfiguredProgram].into_iter(),
            failed,
        ));
    }
}

impl StagedSemanticPackage {
    pub(crate) const fn typescript_snapshot_runtime_local_authority(&self) -> Option<[u8; 32]> {
        self.staged.typescript_runtime_local_authority
    }

    pub(crate) fn typescript_snapshot_program_environments(&self) -> Option<&[[u8; 32]]> {
        self.staged.typescript_program_environments.as_deref()
    }

    /// Encodes a bounded native TypeScript read witness for storage only in the
    /// authenticated metadata member of this staged publication.
    pub fn typescript_snapshot_certificate(&self, input: [u8; 32]) -> Option<Vec<u8>> {
        let host = self.staged.typescript_witness.as_ref()?;
        let root = host.native_project_root()?;
        super::runtime::encode_staged_snapshot_certificate(self, root, input)
    }

    pub(crate) fn typescript_snapshot_witness(
        &self,
        root: &Path,
    ) -> Option<(
        std::sync::Arc<crate::application::typescript_host::TypeScriptProjectWitness>,
        std::sync::Arc<crate::application::typescript_program::TypeScriptForestClosureWitness>,
    )> {
        if !typescript_snapshot_outcome_reusable(
            self.coverage_gaps()
                .iter()
                .map(PackageSourceCoverageGap::cause),
            self.embedding_status(),
        ) {
            return None;
        }
        let host = self.staged.typescript_witness.as_ref()?;
        if !host.is_native_for_root(root) {
            return None;
        }
        Some((
            std::sync::Arc::clone(host),
            std::sync::Arc::clone(self.staged.typescript_closure_witness.as_ref()?),
        ))
    }

    /// Retains the local captured-program fence for bounded snapshot reuse.
    /// This grants no ambient read completeness or complete semantic coverage.
    pub(crate) fn python_snapshot_witness(
        &self,
    ) -> Option<std::sync::Arc<backend_frontend_python::legacy::checker::PythonProjectWitness>>
    {
        self.staged
            .python_witness
            .as_ref()
            .map(|witness| std::sync::Arc::clone(&witness.witness))
    }
    /// Opaque owner identity retained for the exact compilation transaction that created this
    /// staged package. It correlates in-memory traces only; it does not prove read completeness.
    pub(crate) const fn compilation_attempt_id(&self) -> CompilationAttemptId {
        self.staged.compilation_attempt_id
    }

    /// Checks the narrow same-attempt and target/profile/stage handoff into a read trace.
    /// This correlation check does not prove that the trace observed every compiler read.
    pub(crate) fn matches_read_trace_scope(
        &self,
        manifest: &crate::compiler_input_manifest_v2::CompilerInputManifestV2,
        closure: &crate::compiler_unit_read_closure_v2::VerifiedUnitReadClosure,
    ) -> bool {
        closure.matches_attempt(self.compilation_attempt_id())
            && self.target_identity == manifest.package_target().target()
            && self.profile == manifest.invocation_recipe().profile()
            && self.stage == manifest.invocation_recipe().stage()
    }

    /// Exact compiler-observed coverage causes, authenticated with this publication.
    #[must_use]
    pub fn source_coverage(&self) -> Option<&backend_semantic::ir::SourceCoverageObservation> {
        self.staged.source_coverage.as_ref()
    }

    /// Exact full native program membership, including libraries and dependencies.
    /// This is not a complete-fact or complete-read certificate.
    #[must_use]
    pub fn native_program_sources(&self) -> Option<&backend_semantic::ir::NativeProgramSources> {
        self.staged.native_program_sources.as_ref()
    }

    /// Returns the exact verified generation root facts for this output closure.
    #[must_use]
    pub const fn generation_facts(&self) -> backend_store::hydration::VerifiedGenerationFacts {
        self.prepared.generation
    }

    /// Returns the canonical semantic manifest facts.
    #[must_use]
    pub const fn manifest_facts(&self) -> crate::publication::manifest::CompilationManifestFacts {
        self.prepared.manifest
    }

    /// Returns the canonical generation binding facts.
    #[must_use]
    pub const fn binding_facts(&self) -> crate::publication::binding::CompilationBindingFacts {
        self.prepared.binding
    }

    /// Returns exact canonical manifest bytes for the output closure.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.prepared.manifest_bytes
    }

    /// Returns exact generation-to-manifest binding bytes.
    #[must_use]
    pub fn binding_bytes(&self) -> &[u8] {
        &self.prepared.binding_bytes
    }

    /// Returns canonical member metadata in the exact manifest order.
    #[must_use]
    pub fn artifacts(&self) -> &[StagedSemanticArtifact] {
        &self.artifacts
    }

    /// Returns exact source-frontier members that had no active semantic scope.
    ///
    /// Their bytes remain committed by [`Self::input_witness`], while no
    /// semantic artifact is claimed for them.
    #[must_use]
    pub fn coverage_gaps(&self) -> &[PackageSourceCoverageGap] {
        &self.staged.coverage_gaps
    }

    /// Returns the exact opened runtime identity retained by this staged result, when present.
    #[must_use]
    pub const fn execution_identity(&self) -> Option<LocalCompilerExecutionIdentity> {
        self.execution_identity
    }

    /// Returns the host-scoped identity used for local versioned-plane publication.
    ///
    /// This is a distinct capability from the portable worker identity and retains the exact
    /// input witness claim without upgrading its coverage state.
    #[must_use]
    pub const fn plane_execution_identity(&self) -> Option<LocalCompilerPlaneExecutionIdentity> {
        self.plane_execution_identity
    }

    /// Returns the exact input/read witness claim carried by source admission.
    ///
    /// A claim from `with_input_claim` is opaque until a separate authority boundary admits its
    /// preimage and completeness; the source-only fallback remains Partial.
    #[must_use]
    pub const fn input_witness(&self) -> SemanticInputWitness {
        self.input
    }

    /// Exact full Cargo metadata facts produced by this staged Rust profile.
    ///
    /// The reference is absent when bounded metadata/config/toolchain binding
    /// was unavailable; a different profile's facts are never substituted.
    #[must_use]
    pub fn cargo_workspace_facts(&self) -> Option<&RustCargoWorkspaceFactsV1> {
        self.staged.cargo_workspace_facts.as_deref()
    }

    /// Exact Rust target and captured source closure used for this staged profile.
    ///
    /// Package inputs outside this closure remain committed by the input witness,
    /// but do not contribute semantic source rows or gaps for the selected target.
    #[must_use]
    pub fn rust_target_source_closure(&self) -> Option<&RustCargoTargetSourceClosureV1> {
        self.staged.rust_target_source_closure.as_ref()
    }

    /// Rust language profile whose workspace preflight produced these facts.
    #[must_use]
    pub const fn profile(&self) -> backend_semantic::vocabulary::LanguageProfile {
        self.profile
    }

    /// Returns the semantic VCS generation ID for one exact canonical image payload.
    ///
    /// This is the same byte identity used by `SemanticSnapshot::generation`. It is distinct
    /// from the durable publisher's selected root, generation binding, and plane-manifest root.
    #[must_use]
    pub fn semantic_vcs_generation(
        &self,
        artifact_ordinal: usize,
    ) -> Option<backend_semantic::ir::GenerationId> {
        let input_ordinal = *self.prepared.canonical_ordinals.get(artifact_ordinal)?;
        let region = *self.staged.image_plan.get(input_ordinal)?;
        let bytes = region.bytes(&self.staged.semantic_images)?;
        Some(backend_semantic::ir::GenerationId::from_canonical_bytes(
            bytes,
        ))
    }

    /// Returns explicit embedding availability retained during source staging.
    #[must_use]
    pub fn embedding_status(&self) -> StagedEmbeddingStatus<'_> {
        if let Some(cause) = self.staged.embedding_provisioning_failure {
            return StagedEmbeddingStatus::ProvisioningUnavailable { cause };
        }
        match self.staged.embeddings.as_ref() {
            None => StagedEmbeddingStatus::NotConfigured,
            Some(output) => match output.unavailable_reason.as_deref() {
                Some(reason) => StagedEmbeddingStatus::Unavailable {
                    identity: output.identity,
                    reason,
                },
                None => StagedEmbeddingStatus::Available {
                    identity: output.identity,
                },
            },
        }
    }

    /// Admits the one runtime identity that governs every staged plane of this package.
    ///
    /// A portable execution identity takes precedence over a host-local plane identity. Both
    /// must govern this exact target, profile, and stage, and a host-local identity must have
    /// been sealed over this exact input witness.
    fn admitted_runtime(&self) -> Result<AdmittedStagedRuntime, StagedRuntimeRefusal> {
        use StagedRuntimeRefusal::Mismatch;
        if self
            .plane_execution_identity
            .is_some_and(|identity| identity.input_witness() != self.input)
        {
            return Err(Mismatch(StagedIdentityFacet::InputWitness));
        }
        let runtime = if let Some(identity) = self.execution_identity {
            AdmittedStagedRuntime {
                target: identity.target(),
                profile: identity.profile(),
                stage: identity.stage(),
                tool: identity.toolchain(),
                toolchain: identity.toolchain_identity(),
                environment: identity.environment_identity(),
                target_platform: identity.target_platform_identity(),
            }
        } else if let Some(identity) = self.plane_execution_identity {
            AdmittedStagedRuntime {
                target: identity.target(),
                profile: identity.profile(),
                stage: identity.stage(),
                tool: identity.toolchain(),
                toolchain: identity.toolchain_identity(),
                environment: identity.environment_identity(),
                target_platform: identity.target_platform_identity(),
            }
        } else {
            return Err(StagedRuntimeRefusal::Unavailable);
        };
        if runtime.target != self.target_identity {
            return Err(Mismatch(StagedIdentityFacet::Target));
        }
        if runtime.profile != self.profile {
            return Err(Mismatch(StagedIdentityFacet::Profile));
        }
        if runtime.stage != self.stage {
            return Err(Mismatch(StagedIdentityFacet::Stage));
        }
        Ok(runtime)
    }

    /// Returns one staged artifact whose recipe was produced by the admitted runtime.
    fn admitted_artifact_recipe(
        &self,
        runtime: &AdmittedStagedRuntime,
        artifact: usize,
    ) -> Result<
        (
            &StagedSemanticArtifact,
            backend_semantic::vocabulary::CompileRecipeFact,
        ),
        StagedIdentityFacet,
    > {
        let staged = self
            .artifacts
            .get(artifact)
            .ok_or(StagedIdentityFacet::ArtifactImage { artifact })?;
        let recipe = staged.recipe();
        runtime.admit_recipe(artifact, recipe)?;
        Ok((staged, recipe))
    }

    /// Streams every staged image through the shared stable-key c007 producer.
    ///
    /// Each producer call runs inside [`Self::with_semantic_reader`], so the
    /// only reader accepted here is the fully validated borrowed view over
    /// this exact staged image. The returned receipts retain their FileStore
    /// pins and opaque compilation-attempt binding for publication. The
    /// producer copies the input claim as-is and never upgrades its coverage.
    pub fn produce_typed_plane_content_v3(
        &self,
        store: &FileStore,
        policies: SemanticTypedPlaneBoundaryPoliciesV3,
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
    ) -> Result<StagedTypedPlaneContentV3, StagedTypedPlaneContentError> {
        let runtime = self.admitted_runtime()?;

        let mut output = Vec::new();
        output
            .try_reserve_exact(self.artifacts.len())
            .map_err(StagedTypedPlaneContentError::Allocation)?;
        for artifact_ordinal in 0..self.artifacts.len() {
            let (artifact, recipe) = self
                .admitted_artifact_recipe(&runtime, artifact_ordinal)
                .map_err(StagedTypedPlaneContentError::ExecutionIdentityMismatch)?;
            let build = runtime.build_identity(self.package_identity, recipe);
            let semantic_image = artifact.semantic_image();
            let input_witness = self.input;
            let produced = self.with_semantic_reader(artifact_ordinal, |reader, _metrics| {
                let produced = produce_semantic_typed_plane_content_v3(
                    store,
                    reader,
                    build,
                    input_witness,
                    policies,
                    tier,
                    jumbo_limits,
                )
                .map_err(io::Error::other)?;
                if produced.manifest().build() != build
                    || produced.manifest().input_claim()
                        != SemanticInputClaimV2::from_witness(&input_witness)
                    || produced.manifest().image_facts() != **reader
                {
                    return Err(io::Error::other(
                        "stable-key output changed staged build, image, or input facts",
                    ));
                }
                Ok(produced)
            })?;
            output.push(StagedTypedPlaneContentArtifact {
                artifact_ordinal,
                semantic_image,
                build,
                input_witness,
                produced,
            });
        }
        Ok(StagedTypedPlaneContentV3 {
            compilation_attempt_id: self.staged.compilation_attempt_id,
            input_witness: self.input,
            artifacts: output.into_boxed_slice(),
        })
    }

    /// Confirms a content receipt was produced from this exact in-memory
    /// compilation attempt, rather than another staged package with similar
    /// names, versions, or image order.
    #[must_use]
    pub fn owns_typed_plane_content(&self, content: &StagedTypedPlaneContentV3) -> bool {
        content.compilation_attempt_id == self.staged.compilation_attempt_id
            && content.input_witness == self.input
            && content.artifacts.len() == self.artifacts.len()
            && content
                .artifacts
                .iter()
                .zip(&self.artifacts)
                .enumerate()
                .all(|(ordinal, (observed, expected))| {
                    observed.artifact_ordinal == ordinal
                        && observed.semantic_image == expected.semantic_image()
                        && observed.input_witness == self.input
                        && observed.produced.manifest().build() == observed.build
                        && observed.produced.input_witness() == self.input
                })
    }

    /// Builds one exact per-image manifest over bounded ranges of the canonical IR bytes.
    ///
    /// The portable or host-local runtime identity gates publication and binds the exact runtime
    /// fields. Each image retains its source-bound recipe; a source-only witness remains Partial
    /// because it does not prove config files, negative reads, or ambient-read completeness.
    /// Payload bytes are borrowed from the existing canonical image buffer, so creating a range
    /// bundle does not copy a full image.
    pub fn versioned_planes(&self) -> Result<StagedVersionedPlanes<'_>, StagedVersionedPlaneError> {
        let runtime = self.admitted_runtime()?;
        let input = self.input;
        let mut output = Vec::new();
        output
            .try_reserve_exact(self.artifacts.len())
            .map_err(StagedVersionedPlaneError::Allocation)?;

        for artifact_ordinal in 0..self.artifacts.len() {
            let (_, recipe) = self
                .admitted_artifact_recipe(&runtime, artifact_ordinal)
                .map_err(StagedVersionedPlaneError::ExecutionIdentityMismatch)?;
            let build = runtime.build_identity(self.package_identity, recipe);
            let missing_image = || {
                StagedVersionedPlaneError::ExecutionIdentityMismatch(
                    StagedIdentityFacet::ArtifactImage {
                        artifact: artifact_ordinal,
                    },
                )
            };
            let input_ordinal = *self
                .prepared
                .canonical_ordinals
                .get(artifact_ordinal)
                .ok_or_else(missing_image)?;
            let region = *self
                .staged
                .image_plan
                .get(input_ordinal)
                .ok_or_else(missing_image)?;
            let image = region
                .bytes(&self.staged.semantic_images)
                .ok_or_else(missing_image)?;
            if image.is_empty() {
                return Err(StagedVersionedPlaneError::EmptySemanticImage { artifact_ordinal });
            }
            let segment_count = image.len().div_ceil(MAX_SEMANTIC_SEGMENT_BYTES);
            let embedding = self.staged.embeddings.as_ref();
            let embedding_segment_count =
                usize::from(embedding.is_some_and(|output| output.unavailable_reason.is_none()));
            let mut segments = Vec::new();
            segments
                .try_reserve_exact(
                    segment_count
                        .checked_add(embedding_segment_count)
                        .ok_or(SemanticManifestError::CountOverflow)?,
                )
                .map_err(StagedVersionedPlaneError::Allocation)?;
            let mut metadata_segments = Vec::new();
            metadata_segments
                .try_reserve_exact(segment_count)
                .map_err(StagedVersionedPlaneError::Allocation)?;
            let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
            for (segment_ordinal, payload) in image.chunks(MAX_SEMANTIC_SEGMENT_BYTES).enumerate() {
                let segment_ordinal = u64::try_from(segment_ordinal)
                    .map_err(|_| SemanticManifestError::CountOverflow)?;
                let mut key = [0; 32];
                key[24..].copy_from_slice(&segment_ordinal.to_be_bytes());
                let segment = SemanticPlaneSegment::from_payload_with_witness(
                    kind, key, key, 1, payload, input,
                )?;
                let id = segment
                    .admitted_id()
                    .ok_or(StagedVersionedPlaneError::MissingAdmittedSegmentId)?;
                segments.push(StagedVersionedPlaneSegment { kind, id, payload });
                metadata_segments.push(segment);
            }
            let plane = SemanticPlane::claimed(kind, metadata_segments, Coverage::Complete)?;
            let generation = backend_semantic::ir::GenerationId::from_canonical_bytes(image);
            let mut planes = Vec::new();
            planes
                .try_reserve_exact(1 + usize::from(embedding.is_some()))
                .map_err(StagedVersionedPlaneError::Allocation)?;
            planes.push(plane);
            if let Some(embedding) = embedding {
                let kind = SemanticPlaneKind::Embeddings(embedding.identity);
                let mut vector_segments = Vec::new();
                let coverage = if embedding.unavailable_reason.is_some() {
                    Coverage::Unavailable
                } else {
                    let Some(vector) = embedding.artifacts.get(input_ordinal) else {
                        return Err(StagedVersionedPlaneError::ExecutionIdentityMismatch(
                            StagedIdentityFacet::EmbeddingArtifact {
                                artifact: artifact_ordinal,
                            },
                        ));
                    };
                    vector_segments
                        .try_reserve_exact(1)
                        .map_err(StagedVersionedPlaneError::Allocation)?;
                    let vector_segment = SemanticPlaneSegment::from_payload_with_witness(
                        kind,
                        vector.key,
                        vector.key,
                        1,
                        &vector.payload,
                        input,
                    )?;
                    let id = vector_segment
                        .admitted_id()
                        .ok_or(StagedVersionedPlaneError::MissingAdmittedSegmentId)?;
                    segments.push(StagedVersionedPlaneSegment {
                        kind,
                        id,
                        payload: &vector.payload,
                    });
                    vector_segments.push(vector_segment);
                    Coverage::Complete
                };
                planes.push(SemanticPlane::claimed(kind, vector_segments, coverage)?);
            }
            let manifest = SemanticPlaneManifest::new(generation, build, input, planes)?;
            output.push(StagedVersionedPlaneArtifact {
                artifact_ordinal,
                manifest_bytes: manifest.encode()?.into_boxed_slice(),
                segments: segments.into_boxed_slice(),
            });
        }
        Ok(StagedVersionedPlanes {
            artifacts: output.into_boxed_slice(),
            embedding_status: self.embedding_status(),
        })
    }

    /// Returns the currently supported IR plane bundle under the explicit IR name.
    pub fn versioned_ir_planes(
        &self,
    ) -> Result<StagedVersionedPlanes<'_>, StagedVersionedPlaneError> {
        self.versioned_planes()
    }

    /// Lends one fully validated reader over the exact canonical image bytes.
    ///
    /// The callback can stream multiple bounded row families through a
    /// [`backend_semantic::ir::CanonicalSemanticPlaneSegmentSink`] while this reader is alive.
    /// The image is validated once per callback, borrowed directly from the
    /// staged V1 bytes, and never copied by this seam. Callback return values
    /// cannot retain the stack-owned reader; any segment retention remains an
    /// explicit sink decision.
    pub fn with_semantic_reader<Output, CallbackError>(
        &self,
        artifact_ordinal: usize,
        use_reader: impl FnOnce(
            &SemanticImageView<'_>,
            StagedSemanticReaderMetrics,
        ) -> Result<Output, CallbackError>,
    ) -> Result<Output, StagedSemanticReaderError<CallbackError>>
    where
        CallbackError: std::error::Error + 'static,
    {
        let input_ordinal = *self
            .prepared
            .canonical_ordinals
            .get(artifact_ordinal)
            .ok_or(StagedSemanticReaderError::ArtifactOrdinal)?;
        let region = *self
            .staged
            .image_plan
            .get(input_ordinal)
            .ok_or(StagedSemanticReaderError::ArtifactOrdinal)?;
        let image = region
            .bytes(&self.staged.semantic_images)
            .ok_or(StagedSemanticReaderError::ImageRegion)?;
        let image_validation_input_bytes =
            u64::try_from(image.len()).map_err(|_| StagedSemanticReaderError::ImageRegion)?;
        let reader = SemanticImageView::reopen(image)?;
        let metrics = StagedSemanticReaderMetrics {
            image_validation_count: 1,
            full_image_validation_input_bytes: image_validation_input_bytes,
            canonical_image_copy_bytes: 0,
        };
        use_reader(&reader, metrics).map_err(StagedSemanticReaderError::Callback)
    }

    /// Returns the number of immutable objects in the generation closure.
    #[must_use]
    pub fn output_object_count(&self) -> usize {
        self.prepared.object_claims.len()
    }

    /// Returns one canonical generation claim and its payload bytes.
    #[must_use]
    pub fn output_object(&self, ordinal: usize) -> Option<StagedSemanticOutputObject<'_>> {
        let claim = *self.prepared.object_claims.get(ordinal)?;
        let key = *claim.key();
        let bytes = match ordinal {
            _ if key == 0 => self.prepared.manifest_bytes.as_ref(),
            _ if key % 2 == 1 => {
                let artifact_ordinal = usize::try_from((key - 1) / 2).ok()?;
                let artifact = *self.prepared.canonical_ordinals.get(artifact_ordinal)?;
                self.staged.artifacts.get(artifact)?.fragment.as_ref()
            }
            _ => {
                let artifact_ordinal = usize::try_from(key / 2 - 1).ok()?;
                let artifact = *self.prepared.canonical_ordinals.get(artifact_ordinal)?;
                let region = *self.staged.image_plan.get(artifact)?;
                region.bytes(&self.staged.semantic_images)?
            }
        };
        Some(StagedSemanticOutputObject { claim, bytes })
    }

    /// Returns the semantic image claim and bytes for one canonical artifact ordinal.
    ///
    /// This accessor binds through the prepared canonical order and keeps callers from deriving
    /// image object positions from the manifest/binding/fragment interleave.
    #[must_use]
    pub fn semantic_output_object(
        &self,
        artifact_ordinal: usize,
    ) -> Option<StagedSemanticOutputObject<'_>> {
        let artifact = *self.artifacts.get(artifact_ordinal)?;
        let input_ordinal = *self.prepared.canonical_ordinals.get(artifact_ordinal)?;
        let image_region = *self.staged.image_plan.get(input_ordinal)?;
        let bytes = image_region.bytes(&self.staged.semantic_images)?;
        Some(StagedSemanticOutputObject {
            claim: artifact.semantic_claim,
            bytes,
        })
    }

    pub(crate) fn retain_budget_lease(&mut self, lease: StagedOutputLease) {
        self._budget_lease = Some(lease);
    }
}

/// One immutable semantic generation reopened by exact claim rather than local journal head.
#[derive(Debug)]
pub struct ActivatedSemanticPackage {
    /// Manifest facts proven against the immutable manifest bytes.
    pub manifest: crate::publication::manifest::CompilationManifestFacts,
    /// Generation binding proven against its deterministic immutable address.
    pub binding: crate::publication::binding::CompilationBindingFacts,
    /// Complete semantic images copied only after closure verification.
    pub images: Box<[SemanticImageSnapshot]>,
}

/// Typed package-wide compilation or publication terminal.
#[derive(Debug, Error)]
pub enum PackageSemanticError {
    /// Canonical package lineage could not be constructed.
    #[error("package lineage is malformed")]
    Lineage,
    /// Internal restart request: a TypeScript source spent its own work
    /// allowance, so the attempt must be rerun from fresh native state without
    /// it. [`LocalCompilerExecution::stage_package_sources`] consumes this value;
    /// it never reaches a caller of the public staging entry point.
    #[error("TypeScript source work allowance spent; package compilation restarts without it")]
    TypeScriptWorkRestart,
    /// A Go package's authority filesystem inputs could not be admitted.
    #[error(transparent)]
    GoAuthorityWitness(#[from] backend_frontend_go::legacy::oracle::GoPackageAuthorityWitnessError),
    /// A fresh compiler workspace did not reproduce the closure selected before capture.
    #[error("Rust Cargo target source closure changed after target resolution")]
    RustTargetSourceClosureMismatch,
    /// One package-relative declaration scope was rejected.
    #[error("package declaration scope is malformed for {path}")]
    Scope {
        /// Rejected normalized source path.
        path: Box<str>,
    },
    /// One authority, toolchain, lowering, or cancellation terminal occurred.
    #[error(
        "package semantic compilation failed for {path}: {text}{host_hint}",
        text = super::compiler_terminal_text::TerminalText::new(path, terminal),
        host_hint = package_compile_host_hint(terminal)
    )]
    Compile {
        /// Source member that reached the terminal.
        path: Box<str>,
        /// Exact closed compiler terminal.
        terminal: Box<CompilerTerminal>,
    },
    /// An installed TypeScript source or toolchain witness changed before publication.
    #[error("admitted TypeScript project changed before publication")]
    TypeScriptProjectWitness(#[source] crate::application::TypeScriptProjectHostError),
    /// Captured Python source, configuration, or executable changed before admission.
    #[error("admitted Python project changed before publication")]
    PythonProjectWitness(#[source] backend_frontend_python::legacy::checker::CheckerError),
    /// Required embedding inference could not produce a complete bounded output plane.
    #[error("required package embedding failed for {path}: {cause}")]
    Embedding {
        /// Source member that failed embedding admission.
        path: Box<str>,
        /// Exact embedding runtime or output-bound failure.
        cause: Box<str>,
    },
    /// A configured required runtime failed cold provisioning before inference.
    #[error("required package embedding provisioning failed for {path}: {cause:?}")]
    EmbeddingProvisioning {
        /// Source member whose required runtime was unavailable.
        path: Box<str>,
        /// Closed owner-admitted provisioning failure, without private paths.
        cause: EmbeddingProvisioningFailure,
    },
    /// Full native program membership could not be canonically admitted.
    #[error("native TypeScript program source manifest is invalid")]
    NativeProgramSources(#[source] backend_semantic::ir::NativeProgramSourceManifestError),
    /// One produced forest image could not bind to its actual program owner.
    #[error("native TypeScript forest image is invalid")]
    NativeProgramImage(#[source] SemanticImageReopenError),
    /// A checked size computation overflowed or exceeded the package budget.
    #[error("package semantic publication exceeds its {lane} byte budget")]
    Capacity {
        /// Bounded output lane.
        lane: &'static str,
    },
    /// The complete profile-bound compact-fragment plane exceeded its finite limit.
    #[error(
        "package semantic publication exceeds compact fragment byte budget for {profile:?}: used {used_bytes} bytes, limit {limit_bytes} bytes"
    )]
    FragmentCapacity {
        /// Exact package profile that selected the finite fragment policy.
        profile: LanguageProfile,
        /// Attempted aggregate, including an addition wider than the host usize.
        used_bytes: u128,
        /// Maximum aggregate admitted for this profile.
        limit_bytes: usize,
    },
    /// Reusable publication scratch could not be prepared.
    #[error("package semantic publication scratch failed")]
    Scratch(#[source] crate::application::LocalCompilerScratchError),
    /// A retained compact fragment failed its second grammar admission.
    #[error("retained compact fragment {ordinal} failed admission")]
    Fragment {
        /// Canonical package artifact ordinal.
        ordinal: usize,
        /// Exact fragment grammar failure.
        #[source]
        source: backend_semantic::ir::FragmentError,
    },
    /// Immutable package publication failed.
    #[error("package semantic publication failed: {0}")]
    Publish(#[source] crate::publication::PublishSemanticError),
    /// A canonical compiler output failed preparation before any local publication.
    #[error("package semantic output could not be prepared for transport: {0}")]
    StagedOutput(#[source] crate::publication::PublishSemanticError),
    /// The publication owner could not reopen its selected closure.
    #[error("package semantic publication could not be reopened")]
    Reopen(#[source] crate::publication::OpenPublishedError),
    /// The durable owner did not select the just-published generation.
    #[error("package semantic publication disappeared before reopen")]
    MissingPublication,
    /// One manifest-bound semantic artifact failed admission.
    #[error("semantic artifact {ordinal} failed reopen admission")]
    Artifact {
        /// Canonical package artifact ordinal.
        ordinal: usize,
        /// Exact paired fragment/image failure.
        #[source]
        source: crate::publication::OpenedSemanticArtifactError,
    },
    /// A reopened semantic image could not become an immutable snapshot.
    #[error("semantic artifact {ordinal} failed snapshot admission")]
    Snapshot {
        /// Canonical package artifact ordinal.
        ordinal: usize,
        /// Exact extent or identity failure.
        cause: SemanticImageAccessError,
    },
    /// The reopened manifest did not yield the published source cardinality.
    #[error("semantic publication reopened {observed} artifacts; expected {expected}")]
    ReopenedCardinality {
        /// Expected admitted source count.
        expected: usize,
        /// Reopened artifact count.
        observed: usize,
    },
    /// A bounded owned lane could not reserve its exact capacity.
    #[error("package semantic publication allocation failed")]
    Allocation(#[source] std::collections::TryReserveError),
}

/// The source of truth is the exact bound program, not the output artifact list.
fn capture_native_program_sources<'paths>(
    project: &TszProject,
    package_paths: impl IntoIterator<Item = (&'paths str, &'paths str)>,
    toolchain: ContentId<backend_version::ToolchainDomain>,
    mut checkpoint: impl FnMut(u64) -> Result<(), PackageSemanticError>,
) -> Result<backend_semantic::ir::NativeProgramSourceManifest, PackageSemanticError> {
    use backend_semantic::ir::{
        MAX_NATIVE_PROGRAM_SOURCES, NativeProgramSource, NativeProgramSourceManifest,
        NativeProgramSourceManifestError,
    };
    use backend_version::ContentPayloadHasher;
    checkpoint(1)?;
    let files = &project.program().files;
    if files.is_empty() || files.len() > MAX_NATIVE_PROGRAM_SOURCES {
        return Err(PackageSemanticError::NativeProgramSources(
            NativeProgramSourceManifestError::Limit,
        ));
    }
    let mut mappings = BTreeMap::new();
    for (package, native) in package_paths {
        checkpoint(1)?;
        if mappings.insert(native, package).is_some() {
            return Err(PackageSemanticError::NativeProgramSources(
                NativeProgramSourceManifestError::Encoding,
            ));
        }
    }
    let mut sources = Vec::with_capacity(files.len());
    for file in files {
        checkpoint(1)?;
        let source = file
            .arena
            .get_source_file_at(file.source_file)
            .ok_or(PackageSemanticError::NativeProgramSources(
                NativeProgramSourceManifestError::Encoding,
            ))?
            .text
            .as_bytes();
        let byte_len = u32::try_from(source.len()).map_err(|_| {
            PackageSemanticError::NativeProgramSources(NativeProgramSourceManifestError::Limit)
        })?;
        let mut hasher = ContentPayloadHasher::<SourceFactDomain>::new(u64::from(byte_len));
        for chunk in source.chunks(32 * 1024) {
            checkpoint(1)?;
            hasher.push_chunk(chunk).map_err(|_| {
                PackageSemanticError::NativeProgramSources(
                    NativeProgramSourceManifestError::Encoding,
                )
            })?;
        }
        let identity = hasher.finish().map_err(|_| {
            PackageSemanticError::NativeProgramSources(NativeProgramSourceManifestError::Encoding)
        })?;
        sources.push(NativeProgramSource {
            program_path: file.file_name.clone(),
            source: backend_semantic::ir::SourceIdentity { identity, byte_len },
            package_path: mappings.remove(file.file_name.as_str()).map(str::to_owned),
        });
    }
    if !mappings.is_empty() {
        return Err(PackageSemanticError::NativeProgramSources(
            NativeProgramSourceManifestError::Encoding,
        ));
    }
    checkpoint(sources.len() as u64)?;
    let manifest = NativeProgramSourceManifest::from_sources(toolchain, sources)
        .map_err(PackageSemanticError::NativeProgramSources)?;
    checkpoint(0)?;
    Ok(manifest)
}

/// Sources of one package that failed on their own and became coverage gaps.
///
/// One source's refusal, native rejection, unrepresentable construct or deadline
/// must not hide the facts of the files that compiled: it becomes a typed gap
/// and the package publishes as Partial. Only a failure that makes the whole
/// package meaningless (no toolchain, cancellation, storage) stays a refusal,
/// and so does a package in which no source compiled at all.
#[derive(Default)]
struct DegradedSources {
    /// The first degraded source, reported as the refusal when nothing compiled.
    first: Option<(Box<str>, CompilerTerminal)>,
    /// The package deadline has passed: later sources are not attempted.
    deadline_passed: bool,
}

impl DegradedSources {
    /// Records `terminal` as `path`'s coverage gap when it is that source's own
    /// failure, and returns it as the package's refusal when it is not.
    fn degrade(
        &mut self,
        terminal: CompilerTerminal,
        path: &str,
        source: SourceAuthority,
        gaps: &mut Vec<PackageSourceCoverageGap>,
    ) -> Result<(), PackageSemanticError> {
        self.degrade_with_diagnostic(terminal, path, source, None, gaps)
    }

    fn degrade_with_diagnostic(
        &mut self,
        terminal: CompilerTerminal,
        path: &str,
        source: SourceAuthority,
        typescript_diagnostic: Option<SourceCoverageGapDiagnostic>,
        gaps: &mut Vec<PackageSourceCoverageGap>,
    ) -> Result<(), PackageSemanticError> {
        let Some(cause) = source_gap_cause(&terminal) else {
            return Err(PackageSemanticError::Compile {
                path: path.into(),
                terminal: Box::new(terminal),
            });
        };
        tracing::warn!(
            target: "compiler.package_source",
            path,
            reason = %super::compiler_terminal_text::TerminalText::new(path, &terminal),
            "source degraded to a coverage gap; the rest of the package keeps its facts"
        );
        self.deadline_passed |= cause == PackageSourceCoverageGapCause::CompileDeadlineExceeded;
        self.push_gap(path, source, cause, typescript_diagnostic, gaps);
        self.first.get_or_insert_with(|| (path.into(), terminal));
        Ok(())
    }

    /// Records a source that was never attempted because the deadline had passed.
    fn skip_after_deadline(
        &mut self,
        path: &str,
        source: SourceAuthority,
        gaps: &mut Vec<PackageSourceCoverageGap>,
    ) {
        self.push_gap(
            path,
            source,
            PackageSourceCoverageGapCause::CompileDeadlineExceeded,
            None,
            gaps,
        );
    }

    fn push_gap(
        &self,
        path: &str,
        source: SourceAuthority,
        cause: PackageSourceCoverageGapCause,
        typescript_diagnostic: Option<SourceCoverageGapDiagnostic>,
        gaps: &mut Vec<PackageSourceCoverageGap>,
    ) {
        gaps.push(PackageSourceCoverageGap {
            source,
            relative_path: path.into(),
            cause,
            python_source_status: None,
            native_python_diagnostics: Box::new([]),
            typescript_diagnostic,
        });
    }

    /// The first degraded source's terminal, when no source of the package
    /// compiled: such a package has nothing to publish and refuses with it.
    fn into_refusal_when_nothing_compiled(self, compiled: usize) -> Option<PackageSemanticError> {
        if compiled != 0 {
            return None;
        }
        self.first
            .map(|(path, terminal)| PackageSemanticError::Compile {
                path,
                terminal: Box::new(terminal),
            })
    }
}

/// The gap a source's own terminal degrades to, or `None` for a failure that
/// concerns the whole package.
fn source_gap_cause(terminal: &CompilerTerminal) -> Option<PackageSourceCoverageGapCause> {
    let CompilerTerminal::Compile { cause, .. } = terminal else {
        return None;
    };
    match cause {
        CompilerCause::Authority { .. } => {
            Some(PackageSourceCoverageGapCause::SourceAuthorityRefused)
        }
        CompilerCause::NativeRejected { .. } | CompilerCause::DiagnosticLimit { .. } => {
            Some(PackageSourceCoverageGapCause::SourceRejectedByNativeCompiler)
        }
        CompilerCause::Lowering(_)
        | CompilerCause::Fragment(_)
        | CompilerCause::FragmentFailure(_) => {
            Some(PackageSourceCoverageGapCause::SourceNotRepresentable)
        }
        CompilerCause::DeadlineExceeded { .. } => {
            Some(PackageSourceCoverageGapCause::CompileDeadlineExceeded)
        }
        // The work directory and the child process are shared by every source.
        CompilerCause::NativeWork(_) | CompilerCause::NativeIo { .. } => None,
    }
}

/// Projects the existing exact owner spans of a TSZ cycle into a bounded,
/// source-bounded coverage diagnostic. The caller passes the exact bytes for
/// the current compile input. These are owner coordinates;
/// the TSZ handle is deliberately kept opaque and is never presented as a name.
fn typescript_source_gap_diagnostic(
    terminal: &CompilerTerminal,
    source: &[u8],
) -> Option<SourceCoverageGapDiagnostic> {
    use backend_semantic::vocabulary::{
        LoweringUnsupported, ProjectionAdmissionFault, ProjectionSpan,
    };

    let CompilerTerminal::Compile {
        cause: CompilerCause::Lowering(lowering),
        ..
    } = terminal
    else {
        return None;
    };
    let LoweringUnsupported::FactRejected {
        cause:
            ProjectionAdmissionFault::TypeProjectionCycle {
                type_id,
                owner_span: Some(ProjectionSpan { start, end }),
                owner_name_span:
                    Some(ProjectionSpan {
                        start: name_start,
                        end: name_end,
                    }),
            },
        ..
    } = &**lowering
    else {
        return None;
    };

    let raw_source = std::str::from_utf8(source).ok()?;
    let _owner = raw_source.get(usize::try_from(*start).ok()?..usize::try_from(*end).ok()?)?;
    let owner_name = raw_source
        .get(usize::try_from(*name_start).ok()?..usize::try_from(*name_end).ok()?)?;
    if owner_name.is_empty()
        || *start >= *end
        || *name_start < *start
        || *name_end > *end
        || owner_name.len() != name_end.saturating_sub(*name_start) as usize
        || owner_name.len() > 256
        || owner_name.chars().any(char::is_control)
    {
        return None;
    }
    let prefix = raw_source.get(..usize::try_from(*start).ok()?)?;
    let mut line = 1u32;
    let mut column = 1u32;
    let mut chars = prefix.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            line = line.checked_add(1)?;
            column = 1;
        } else if matches!(character, '\n' | '\u{2028}' | '\u{2029}') {
            line = line.checked_add(1)?;
            column = 1;
        } else {
            column = column.checked_add(1)?;
        }
    }
    Some(SourceCoverageGapDiagnostic::TypeProjectionCycle {
        type_id: *type_id,
        owner_start: *start,
        owner_end: *end,
        owner_name_start: *name_start,
        owner_name_end: *name_end,
        line,
        column,
        owner_name: owner_name.to_owned(),
    })
}

/// TypeScript projects may retain siblings only when lowering or validating
/// this source's own fragment failed. Authority failures can describe a broken
/// source/project/session binding, so they must remain package refusals.
fn typescript_source_gap_cause(
    terminal: &CompilerTerminal,
    expected_source: SourceAuthority,
) -> Option<PackageSourceCoverageGapCause> {
    let CompilerTerminal::Compile { attempted, cause } = terminal else {
        return None;
    };
    if attempted.source != expected_source {
        return None;
    }
    match cause {
        CompilerCause::Lowering(_) | CompilerCause::FragmentFailure(_) => {
            Some(PackageSourceCoverageGapCause::SourceNotRepresentable)
        }
        _ => None,
    }
}

#[cfg(test)]
mod source_degradation_tests {
    use super::*;
    use backend_semantic::vocabulary::Stage;

    fn source() -> SourceAuthority {
        SourceAuthority {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"package a"),
            byte_len: 9,
        }
    }

    fn compile(cause: CompilerCause) -> CompilerTerminal {
        CompilerTerminal::Compile {
            attempted: backend_library::interface::CompilerAttempt {
                source: source(),
                recipe: ContentId::from_canonical_bytes(b"recipe"),
            },
            cause,
        }
    }

    #[test]
    fn a_sources_own_failures_become_typed_gaps_and_shared_ones_stay_refusals() {
        use PackageSourceCoverageGapCause as Gap;
        let own = [
            (
                compile(CompilerCause::Authority {
                    phase: backend_semantic::vocabulary::AuthorityPhase::Open,
                    class: AuthorityDiagnosticClass::Authority,
                    diagnostic: None,
                }),
                Gap::SourceAuthorityRefused,
            ),
            (
                compile(CompilerCause::NativeRejected {
                    code: Some(1),
                    diagnostic: None,
                }),
                Gap::SourceRejectedByNativeCompiler,
            ),
            (
                compile(CompilerCause::Fragment(
                    backend_library::interface::FragmentCause::Validate,
                )),
                Gap::SourceNotRepresentable,
            ),
            (
                compile(CompilerCause::DeadlineExceeded { diagnostic: None }),
                Gap::CompileDeadlineExceeded,
            ),
        ];
        for (terminal, expected) in own {
            assert_eq!(source_gap_cause(&terminal), Some(expected));
        }
        let target =
            ContentId::<backend_version::CompilationTargetDomain>::from_canonical_bytes(b"t");
        for shared in [
            CompilerTerminal::PackageCancelled {
                target,
                phase: PackageCompilePhase::Authority,
            },
            CompilerTerminal::Toolchain {
                source: source(),
                language: backend_semantic::vocabulary::Language::Go,
                stage: Stage::LowerIr,
                selected: NativeTool::GoCompiler,
                configured: None,
            },
            compile(CompilerCause::NativeIo {
                phase: backend_library::interface::NativeIoPhase::Start,
                cause: backend_library::interface::NativeIoFact {
                    kind: std::io::ErrorKind::NotFound,
                    raw_os_code: Some(2),
                },
            }),
        ] {
            assert_eq!(source_gap_cause(&shared), None);
        }
    }

    #[test]
    fn degrading_records_one_gap_per_source_and_refuses_only_when_nothing_compiled() {
        let mut gaps = Vec::new();
        let mut degraded = DegradedSources::default();
        degraded
            .degrade(
                compile(CompilerCause::NativeRejected {
                    code: None,
                    diagnostic: None,
                }),
                "a_test.go",
                source(),
                &mut gaps,
            )
            .expect("a source-local failure degrades");
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].relative_path(), "a_test.go");
        assert!(!degraded.deadline_passed);
        // A shared failure is the package's refusal and records no gap.
        let target =
            ContentId::<backend_version::CompilationTargetDomain>::from_canonical_bytes(b"t");
        let refusal = degraded.degrade(
            CompilerTerminal::PackageCancelled {
                target,
                phase: PackageCompilePhase::Authority,
            },
            "b.go",
            source(),
            &mut gaps,
        );
        assert!(matches!(refusal, Err(PackageSemanticError::Compile { .. })));
        assert_eq!(gaps.len(), 1);
        // Sources that compiled keep the package publishable; none compiled refuses with the first.
        assert!(degraded.into_refusal_when_nothing_compiled(3).is_none());
        let mut only = DegradedSources::default();
        only.degrade(
            compile(CompilerCause::DeadlineExceeded { diagnostic: None }),
            "c.go",
            source(),
            &mut gaps,
        )
        .expect("deadline degrades");
        assert!(only.deadline_passed);
        let refusal = only
            .into_refusal_when_nothing_compiled(0)
            .expect("nothing compiled");
        assert!(
            refusal.to_string().contains(
                "package semantic compilation failed for c.go: the compile deadline passed"
            )
        );
        assert!(!refusal.to_string().contains("DeadlineExceeded"));
    }
    #[test]
    fn typescript_gap_policy_requires_exact_source_and_local_lowering_or_fragment_failure() {
        use PackageSourceCoverageGapCause as Gap;
        use backend_library::interface::TypeScriptAuthorityFailureKind as TsFailure;
        use backend_semantic::vocabulary::LoweringUnsupported;

        let lowering = compile(CompilerCause::Lowering(
            backend_library::interface::LoweringCause::new(
                LoweringUnsupported::TypeScriptDeclarationForm,
            ),
        ));
        assert_eq!(
            typescript_source_gap_cause(&lowering, source()),
            Some(Gap::SourceNotRepresentable)
        );
        let fragment = compile(CompilerCause::FragmentFailure(Box::new(
            backend_library::interface::CompilerFragmentFailure::validate(
                backend_semantic::ir::FragmentError::TruncatedHeader {
                    required: 8,
                    actual: 0,
                },
            ),
        )));
        assert_eq!(
            typescript_source_gap_cause(&fragment, source()),
            Some(Gap::SourceNotRepresentable)
        );

        let other_source = SourceAuthority {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"other source"),
            byte_len: 12,
        };
        let wrong_source = CompilerTerminal::Compile {
            attempted: backend_library::interface::CompilerAttempt {
                source: other_source,
                recipe: ContentId::from_canonical_bytes(b"recipe"),
            },
            cause: CompilerCause::Lowering(backend_library::interface::LoweringCause::new(
                LoweringUnsupported::TypeScriptDeclarationForm,
            )),
        };
        assert_eq!(typescript_source_gap_cause(&wrong_source, source()), None);

        for failure in [
            TsFailure::NativeSourceMismatch,
            TsFailure::NativeProjectSessionMismatch,
        ] {
            let diagnostic = backend_library::interface::CompilerDiagnostic::from_native(
                b"TypeScript authority binding failed",
                b"TypeScript authority binding failed".len(),
                false,
            )
            .expect("bounded diagnostic")
            .with_typescript_failure(failure);
            let terminal = compile(CompilerCause::Authority {
                phase: backend_semantic::vocabulary::AuthorityPhase::Resolve,
                class: AuthorityDiagnosticClass::Authority,
                diagnostic: Some(diagnostic),
            });
            assert_eq!(
                typescript_source_gap_cause(&terminal, source()),
                None,
                "{failure:?}"
            );
        }

        let untyped_authority = compile(CompilerCause::Authority {
            phase: backend_semantic::vocabulary::AuthorityPhase::Resolve,
            class: AuthorityDiagnosticClass::Projection,
            diagnostic: None,
        });
        assert_eq!(
            typescript_source_gap_cause(&untyped_authority, source()),
            None
        );
        assert_eq!(
            typescript_source_gap_cause(
                &compile(CompilerCause::DeadlineExceeded { diagnostic: None }),
                source(),
            ),
            None
        );
    }

    #[test]
    fn tsz_cycle_gap_diagnostic_uses_only_validated_owner_source_spans() {
        use backend_semantic::vocabulary::{
            LoweringUnsupported, ProjectionAdmissionFault, ProjectionSpan,
        };

        let source = "  export type Layout = string;\n";
        let start = 2;
        let end = source.find('\n').expect("owner line") as u32;
        let name_start = source.find("Layout").expect("owner name") as u32;
        let name_end = name_start + "Layout".len() as u32;
        let terminal = compile(CompilerCause::Lowering(
            backend_library::interface::LoweringCause::new(
                LoweringUnsupported::FactRejected {
                    fact: 0,
                    name_len: u64::from(name_end - name_start),
                    cause: ProjectionAdmissionFault::TypeProjectionCycle {
                        type_id: 0xfeed,
                        owner_span: Some(ProjectionSpan { start, end }),
                        owner_name_span: Some(ProjectionSpan {
                            start: name_start,
                            end: name_end,
                        }),
                    },
                },
            ),
        ));

        assert!(matches!(
            typescript_source_gap_diagnostic(&terminal, source.as_bytes()),
            Some(SourceCoverageGapDiagnostic::TypeProjectionCycle {
                type_id: 0xfeed,
                owner_start: 2,
                owner_end,
                owner_name_start,
                owner_name_end,
                line: 1,
                column: 3,
                owner_name,
            }) if owner_end == end && owner_name_start == name_start && owner_name_end == name_end && owner_name == "Layout"
        ));
        let out_of_bounds = compile(CompilerCause::Lowering(
            backend_library::interface::LoweringCause::new(
                LoweringUnsupported::FactRejected {
                    fact: 0,
                    name_len: u64::from(name_end - name_start),
                    cause: ProjectionAdmissionFault::TypeProjectionCycle {
                        type_id: 0xfeed,
                        owner_span: Some(ProjectionSpan {
                            start,
                            end: source.len() as u32 + 1,
                        }),
                        owner_name_span: Some(ProjectionSpan {
                            start: name_start,
                            end: name_end,
                        }),
                    },
                },
            ),
        ));
        assert!(typescript_source_gap_diagnostic(&out_of_bounds, source.as_bytes()).is_none());

        let out_of_bounds_name = compile(CompilerCause::Lowering(
            backend_library::interface::LoweringCause::new(
                LoweringUnsupported::FactRejected {
                    fact: 0,
                    name_len: 1,
                    cause: ProjectionAdmissionFault::TypeProjectionCycle {
                        type_id: 0xfeed,
                        owner_span: Some(ProjectionSpan { start, end }),
                        owner_name_span: Some(ProjectionSpan {
                            start: source.len() as u32,
                            end: source.len() as u32 + 1,
                        }),
                    },
                },
            ),
        ));
        assert!(typescript_source_gap_diagnostic(&out_of_bounds_name, source.as_bytes()).is_none());

        let unicode_source = "  export type Layout = \"λ\";\n";
        let lambda_byte = unicode_source.find('λ').expect("unicode fixture") as u32;
        let invalid_utf8_name = compile(CompilerCause::Lowering(
            backend_library::interface::LoweringCause::new(
                LoweringUnsupported::FactRejected {
                    fact: 0,
                    name_len: 1,
                    cause: ProjectionAdmissionFault::TypeProjectionCycle {
                        type_id: 0xfeed,
                        owner_span: Some(ProjectionSpan {
                            start,
                            end: unicode_source.find('\n').expect("owner line") as u32,
                        }),
                        owner_name_span: Some(ProjectionSpan {
                            start: lambda_byte + 1,
                            end: lambda_byte + 2,
                        }),
                    },
                },
            ),
        ));
        assert!(
            typescript_source_gap_diagnostic(&invalid_utf8_name, unicode_source.as_bytes())
                .is_none()
        );

        let source_with_tail = format!("{source}extra");
        let tail_start = source_with_tail.find("extra").expect("tail fixture") as u32;
        let not_contained = compile(CompilerCause::Lowering(
            backend_library::interface::LoweringCause::new(
                LoweringUnsupported::FactRejected {
                    fact: 0,
                    name_len: 5,
                    cause: ProjectionAdmissionFault::TypeProjectionCycle {
                        type_id: 0xfeed,
                        owner_span: Some(ProjectionSpan { start, end }),
                        owner_name_span: Some(ProjectionSpan {
                            start: tail_start,
                            end: tail_start + 5,
                        }),
                    },
                },
            ),
        ));
        assert!(
            typescript_source_gap_diagnostic(&not_contained, source_with_tail.as_bytes()).is_none()
        );
    }

    #[test]
    fn tsz_cycle_gap_diagnostic_counts_ecmascript_line_terminators_and_scalar_columns() {
        use backend_semantic::vocabulary::{
            LoweringUnsupported, ProjectionAdmissionFault, ProjectionSpan,
        };

        let source = "α\r\nx\ry\u{2028}z\u{2029}  export type Layout = string;\n";
        let start = source.find("export").expect("owner declaration") as u32;
        let end = source.find('\n').expect("owner line") as u32;
        let name_start = source.find("Layout").expect("owner name") as u32;
        let name_end = name_start + "Layout".len() as u32;
        let terminal = compile(CompilerCause::Lowering(
            backend_library::interface::LoweringCause::new(
                LoweringUnsupported::FactRejected {
                    fact: 0,
                    name_len: u64::from(name_end - name_start),
                    cause: ProjectionAdmissionFault::TypeProjectionCycle {
                        type_id: 0xfeed,
                        owner_span: Some(ProjectionSpan { start, end }),
                        owner_name_span: Some(ProjectionSpan {
                            start: name_start,
                            end: name_end,
                        }),
                    },
                },
            ),
        ));
        assert!(matches!(
            typescript_source_gap_diagnostic(&terminal, source.as_bytes()),
            Some(SourceCoverageGapDiagnostic::TypeProjectionCycle {
                line: 5,
                column: 3,
                owner_name,
                ..
            }) if owner_name == "Layout"
        ));
    }
}

fn package_compile_host_hint(terminal: &CompilerTerminal) -> &'static str {
    if matches!(
        terminal,
        CompilerTerminal::Toolchain {
            selected: NativeTool::TypeScriptCompiler,
            configured: None,
            ..
        }
    ) {
        " TypeScript was not available from this project's installed dependencies or the host. Install the project's dependencies with npm or pnpm, or configure NUDOX_TSC, NUDOX_TYPESCRIPT_NODE, and NUDOX_TYPESCRIPT_MODULE_ROOT before starting backend-locald. A running backend-locald keeps the PATH and NUDOX_* settings it started with; stop and restart it after changing them."
    } else {
        ""
    }
}

/// Concrete local compiler with bounded explicit native toolchains, publisher, paths, and scratch.
pub struct LocalCompiler<'path, 'scratch, 'cancel> {
    config: LocalCompilerConfig<'path, 'cancel>,
    package_roots: LocalPackageRootSet<'path>,
    package_authority: PackageAuthorityConfiguration<'path>,
    publisher: DurablePublisher,
    scratch: &'scratch mut LocalCompilerScratch,
    retained_semantic_image: Option<SemanticImageAuthority>,
}

/// Native compilation inputs shared by the embedded client and service worker lanes.
///
/// A lane owns its scratch and cancellation token. This value only borrows immutable admitted
/// configuration; durable publication remains with [`LocalCompiler`].
#[derive(Clone)]
pub(crate) struct LocalCompilerExecution<'path, 'cancel> {
    config: LocalCompilerConfig<'path, 'cancel>,
    package_roots: LocalPackageRootSet<'path>,
    package_authority: PackageAuthorityConfiguration<'path>,
    native_work_directory: Box<Path>,
}

/// Provenance of identities supplied to a local package compiler lane.
/// A fixed identity is a constraint; startup capability identities are only
/// candidates until the request has admitted its installed TypeScript project.
#[derive(Clone, Copy)]
pub(crate) enum PackageExecutionAuthority {
    Fixed {
        execution: Option<LocalCompilerExecutionIdentity>,
        plane: Option<LocalCompilerPlaneExecutionSeed>,
    },
    RuntimeStartup {
        execution: Option<LocalCompilerExecutionIdentity>,
        plane: Option<LocalCompilerPlaneExecutionSeed>,
    },
}

impl PackageExecutionAuthority {
    pub(super) fn identities(
        self,
    ) -> (
        Option<LocalCompilerExecutionIdentity>,
        Option<LocalCompilerPlaneExecutionSeed>,
    ) {
        match self {
            Self::Fixed { execution, plane } | Self::RuntimeStartup { execution, plane } => {
                (execution, plane)
            }
        }
    }

    /// Only the runtime's startup candidate can transition to project-local
    /// authority. A fixed portable or local identity remains a strict constraint.
    pub(super) fn with_admitted_typescript_project(
        self,
        project: LocalCompilerPlaneExecutionSeed,
    ) -> Self {
        match self {
            Self::RuntimeStartup { .. } => Self::Fixed {
                execution: None,
                plane: Some(project),
            },
            Self::Fixed { execution, plane } => Self::Fixed {
                execution,
                plane: plane.or(Some(project)),
            },
        }
    }
}

/// Fully owned single-source native result waiting for the durable publication owner.
pub(crate) struct StagedCompilerArtifact {
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    fragment: Box<[u8]>,
    semantic_image: Box<[u8]>,
}

/// Fully owned complete package result waiting for the durable publication owner.
pub(crate) struct StagedPackageCompilation {
    compilation_attempt_id: CompilationAttemptId,
    artifacts: Vec<StagedPackageArtifact>,
    coverage_gaps: Box<[PackageSourceCoverageGap]>,
    image_plan: Box<[crate::publication::manifest::SemanticImageRegion]>,
    semantic_images: Box<[u8]>,
    package_identity: [u8; 32],
    target_identity: ContentId<backend_version::CompilationTargetDomain>,
    profile: backend_semantic::vocabulary::LanguageProfile,
    stage: backend_semantic::vocabulary::Stage,
    input: SemanticInputWitness,
    execution_identity: Option<LocalCompilerExecutionIdentity>,
    plane_execution_identity: Option<LocalCompilerPlaneExecutionIdentity>,
    typescript_runtime_local_authority: Option<[u8; 32]>,
    typescript_program_environments: Option<Vec<[u8; 32]>>,
    cargo_workspace_facts: Option<std::sync::Arc<RustCargoWorkspaceFactsV1>>,
    rust_target_source_closure: Option<RustCargoTargetSourceClosureV1>,
    typescript_witness:
        Option<std::sync::Arc<crate::application::typescript_host::TypeScriptProjectWitness>>,
    typescript_closure_witness: Option<
        std::sync::Arc<crate::application::typescript_program::TypeScriptForestClosureWitness>,
    >,
    native_program_sources: Option<backend_semantic::ir::NativeProgramSources>,
    source_coverage: Option<backend_semantic::ir::SourceCoverageObservation>,
    python_witness: Option<StagedPythonProjectWitness>,
    embeddings: Option<StagedEmbeddingOutput>,
    embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
}

struct StagedPythonProjectWitness {
    witness: std::sync::Arc<backend_frontend_python::legacy::checker::PythonProjectWitness>,
    deadline: std::time::Instant,
}

struct StagedPackageArtifact {
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    fragment: Box<[u8]>,
}

struct StagedEmbeddingOutput {
    identity: EmbeddingPlaneIdentity,
    artifacts: Box<[StagedEmbeddingArtifact]>,
    unavailable_reason: Option<Box<str>>,
}

struct StagedEmbeddingArtifact {
    key: [u8; 32],
    payload: Box<[u8]>,
    _memory_lease: backend_compile::EmbeddingMemoryReservation,
}

impl<'path, 'cancel> LocalCompilerExecution<'path, 'cancel> {
    pub(crate) fn with_native_work_directory(mut self, path: PathBuf) -> Self {
        self.native_work_directory = path.into_boxed_path();
        self
    }

    pub(crate) fn resolve_rust_cargo_target_name(
        &self,
        package: &PackageSourceSet<'_>,
        target_name: &str,
        cancelled: &AtomicBool,
    ) -> Result<(CompilerPackageTargetV2, RustCargoTargetSourceClosureV1), PackageSemanticError>
    {
        let Some(first_source) = package.sources.first().copied() else {
            return Err(PackageSemanticError::Capacity {
                lane: "Rust target resolver source frontier",
            });
        };
        let application_request = ApplicationCompilerRequest {
            profile: package.request.target.profile,
            stage: package.request.target.stage,
            source: first_source.source,
        };
        if !matches!(application_request.profile, LanguageProfile::Rust(_))
            || target_name.is_empty()
        {
            return Err(PackageSemanticError::Capacity {
                lane: "Rust target resolver request",
            });
        }
        let source_authority = request_source(application_request)
            .map_err(source_terminal)
            .map_err(|terminal| PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(terminal),
            })?;
        let control =
            self.compile_control(cancelled)
                .map_err(|timeout| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(CompilerTerminal::DeadlineConstruction {
                        source: source_authority,
                        language: application_request.profile.language(),
                        stage: application_request.stage,
                        timeout: *timeout,
                    }),
                })?;
        let selected = self.toolchain(application_request).map_err(|cause| {
            toolchain_terminal(source_authority, application_request, cause)
        })?;
        let resolved = match selected {
            ToolchainSelection::ResolvedNative(resolved) => resolved,
            ToolchainSelection::ExplicitlyUnavailable { tool } => {
                return Err(PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(package_authority_terminal(
                        package.package_target.target(),
                        application_request,
                        source_authority,
                        ToolchainSelection::ExplicitlyUnavailable { tool },
                        PackageAuthorityError::ToolchainUnavailable {
                            profile: application_request.profile,
                            tool,
                        },
                    )),
                });
            }
        };
        let external = resolved.external().map_err(|source| {
            PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(package_authority_terminal(
                    package.package_target.target(),
                    application_request,
                    source_authority,
                    selected,
                    PackageAuthorityError::ExternalToolchainRequired {
                        profile: application_request.profile,
                        source,
                    },
                )),
            }
        })?;
        let authority = self.package_authority.rust.ok_or_else(|| {
            PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(package_authority_terminal(
                    package.package_target.target(),
                    application_request,
                    source_authority,
                    selected,
                    PackageAuthorityError::AdapterUnavailable {
                        profile: application_request.profile,
                        stage: crate::application::package_authority::PackageAuthorityStage::RustProject,
                    },
                )),
            }
        })?;
        if authority.toolchain.selected_rustc() != external.executable() {
            return Err(PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(package_authority_terminal(
                    package.package_target.target(),
                    application_request,
                    source_authority,
                    selected,
                    PackageAuthorityError::RustToolchainExecutableMismatch {
                        profile: application_request.profile,
                        configured: authority
                            .toolchain
                            .selected_rustc()
                            .to_path_buf()
                            .into_boxed_path(),
                        resolved: external.executable().to_path_buf().into_boxed_path(),
                    },
                )),
            });
        }
        let rust_toolchain = authority
            .resolve_toolchain(application_request.profile, control)
            .map_err(|cause| {
                package_authority_terminal(
                    package.package_target.target(),
                    application_request,
                    source_authority,
                    selected,
                    cause,
                )
            })
            .map_err(|terminal| PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(terminal),
            })?;
        let backend_semantic::vocabulary::LanguageProfile::Rust(edition) =
            application_request.profile
        else {
            unreachable!("profile was checked above")
        };
        let workspace = RustWorkspace::open_with_features_and_metadata_policy(
            package.package_root,
            rust_toolchain,
            edition,
            authority.features,
            authority.metadata_policy,
            RustAnalysisControl {
                cancelled,
                maximum_source_bytes: authority.maximum_source_bytes,
                deadline: control.deadline,
            },
        )
        .map_err(|cause| {
            package_authority_terminal(
                package.package_target.target(),
                application_request,
                source_authority,
                selected,
                PackageAuthorityError::RustProject(cause),
            )
        })
        .map_err(|terminal| PackageSemanticError::Compile {
            path: first_source.relative_path.into(),
            terminal: Box::new(terminal),
        })?;
        let target = workspace
            .cargo_target_for_name(target_name)
            .ok_or_else(|| {
                package_authority_terminal(
                    package.package_target.target(),
                    application_request,
                    source_authority,
                    selected,
                    PackageAuthorityError::RustProject(
                        RustAuthorityError::CargoTargetMetadataMismatch,
                    ),
                )
            })
            .map_err(|terminal| PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(terminal),
            })?;
        let root = target.root.to_str().ok_or(PackageSemanticError::Capacity {
            lane: "Rust Cargo target root encoding",
        })?;
        let target_root = target.root.to_path_buf();
        if !package
            .sources
            .iter()
            .any(|source| source.relative_path == root)
        {
            return Err(PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(package_authority_terminal(
                    package.package_target.target(),
                    application_request,
                    source_authority,
                    selected,
                    PackageAuthorityError::RustProject(RustAuthorityError::SessionSourcePath {
                        path: target.root.to_path_buf(),
                    }),
                )),
            });
        }
        let package_target = CompilerPackageTargetV2::new(
            package.package_target.package().clone(),
            CompilationUnitKeyV2::RustCrate {
                name: target.name.into(),
                root: root.into(),
            },
        )
        .map_err(|_| PackageSemanticError::Lineage)?;
        let selected_target = RustCargoTarget {
            name: target_name,
            root: &target_root,
        };
        let source_paths = package
            .sources
            .iter()
            .map(|source| PathBuf::from(source.relative_path))
            .collect::<Vec<_>>();
        let source_frontier = package
            .sources
            .iter()
            .map(|source| RustWorkspaceFile {
                relative_path: Path::new(source.relative_path),
                source: source.source,
            })
            .collect::<Vec<_>>();
        let key = RustWorkspaceSessionKey::new(
            package.package_root,
            rust_toolchain,
            edition,
            application_request.stage,
            authority.features,
            authority.metadata_policy,
            None,
            None,
            None,
            [0x5a; 32],
            &source_paths,
        )
        .and_then(|key| key.with_cargo_target(selected_target))
        .map_err(|cause| {
            package_authority_terminal(
                package.package_target.target(),
                application_request,
                source_authority,
                selected,
                PackageAuthorityError::RustProject(cause),
            )
        })
        .map_err(|terminal| PackageSemanticError::Compile {
            path: first_source.relative_path.into(),
            terminal: Box::new(terminal),
        })?;
        let mut lane = RustWorkspaceSessionLane::default();
        let lease = lane
            .begin(
                key,
                &source_frontier,
                RustAnalysisControl {
                    cancelled,
                    maximum_source_bytes: authority.maximum_source_bytes,
                    deadline: control.deadline,
                },
            )
            .map_err(|cause| {
                package_authority_terminal(
                    package.package_target.target(),
                    application_request,
                    source_authority,
                    selected,
                    PackageAuthorityError::RustProject(cause),
                )
            })
            .map_err(|terminal| PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(terminal),
            })?;
        let closure = lease
            .workspace()
            .source_closure_for_target(
                selected_target,
                RustAnalysisControl {
                    cancelled,
                    maximum_source_bytes: authority.maximum_source_bytes,
                    deadline: control.deadline,
                },
            )
            .map_err(|cause| {
                package_authority_terminal(
                    package.package_target.target(),
                    application_request,
                    source_authority,
                    selected,
                    PackageAuthorityError::RustProject(cause),
                )
            })
            .map_err(|terminal| PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(terminal),
            })?;
        Ok((package_target, closure))
    }

    pub(crate) fn stage_generate(
        &self,
        request: ApplicationCompilerRequest<'_>,
        scratch: &mut LocalCompilerScratch,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(PackageCompilePhase),
    ) -> Result<StagedCompilerArtifact, CompilerTerminal> {
        let source = request_source(request).map_err(source_terminal)?;
        let toolchain = self
            .toolchain(request)
            .map_err(|cause| toolchain_terminal(source, request, cause))?;
        let control = self.compile_control(cancelled).map_err(|timeout| {
            CompilerTerminal::DeadlineConstruction {
                source,
                language: request.profile.language(),
                stage: request.stage,
                timeout: *timeout,
            }
        })?;
        let authority = match (request.profile, self.package_authority.clang) {
            (
                backend_semantic::vocabulary::LanguageProfile::C(_)
                | backend_semantic::vocabulary::LanguageProfile::Cxx(_),
                Some(environment),
            ) => crate::driver::SemanticAuthorityInput::ClangBuffer { environment },
            _ => crate::driver::SemanticAuthorityInput::None,
        };
        self.stage_prepared(
            request,
            source,
            DeclarationScope::standalone(request.profile),
            toolchain,
            authority,
            control,
            &mut scratch.diagnostic_output,
            &mut scratch.fragment_output,
            progress,
        )
    }

    pub(crate) fn stage_package(
        &self,
        request: &PackageCompileRequest,
        scratch: &mut LocalCompilerScratch,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(PackageCompilePhase),
    ) -> Result<StagedCompilerArtifact, CompilerTerminal> {
        let target = request.as_ref().identity;
        if cancelled.load(Ordering::Acquire) {
            return Err(CompilerTerminal::PackageCancelled {
                target,
                phase: PackageCompilePhase::Locate,
            });
        }
        progress(PackageCompilePhase::Locate);
        let resolved =
            package_source::resolve(self.package_roots, request.as_ref()).map_err(|cause| {
                CompilerTerminal::PackageSource {
                    target,
                    phase: PackageCompilePhase::Locate,
                    cause,
                }
            })?;
        progress(PackageCompilePhase::EnterSource);
        let source = std::str::from_utf8(&resolved.bytes).map_err(|cause| {
            CompilerTerminal::PackageSource {
                target,
                phase: PackageCompilePhase::EnterSource,
                cause: PackageSourceCause::InvalidUtf8 {
                    valid_up_to: cause.valid_up_to(),
                    error_len: cause
                        .error_len()
                        .and_then(|length| u8::try_from(length).ok()),
                },
            }
        })?;
        let package = request.as_ref();
        let scope =
            DeclarationScope::for_package(package, &resolved.relative_source).map_err(|cause| {
                CompilerTerminal::PackageSource {
                    target,
                    phase: PackageCompilePhase::EnterSource,
                    cause: PackageSourceCause::DeclarationScope {
                        cause: declaration_scope_cause(cause),
                    },
                }
            })?;
        progress(PackageCompilePhase::Authority);
        let application_request = ApplicationCompilerRequest {
            profile: request.target.profile,
            stage: request.target.stage,
            source,
        };
        let source_authority = request_source(application_request).map_err(source_terminal)?;
        let toolchain = self
            .toolchain(application_request)
            .map_err(|cause| toolchain_terminal(source_authority, application_request, cause))?;
        let control = self.compile_control(cancelled).map_err(|timeout| {
            CompilerTerminal::DeadlineConstruction {
                source: source_authority,
                language: application_request.profile.language(),
                stage: application_request.stage,
                timeout: *timeout,
            }
        })?;
        let unit_key = CompilationUnitKeyV2::PackageRoot;
        let authority = enter_package_authority(PackageAuthorityRequest {
            package_root: &resolved.package_root,
            source_path: &resolved.source_path,
            source: &resolved.bytes,
            java_sourcepath: &[],
            unit_key: &unit_key,
            profile: application_request.profile,
            toolchain,
            control,
            configuration: self.package_authority,
        })
        .map_err(|cause| {
            package_authority_terminal(
                target,
                application_request,
                source_authority,
                toolchain,
                cause,
            )
        })?;
        self.stage_prepared(
            application_request,
            source_authority,
            scope,
            toolchain,
            authority.input(&resolved.source_path),
            control,
            &mut scratch.diagnostic_output,
            &mut scratch.fragment_output,
            progress,
        )
    }

    pub(crate) fn stage_package_sources(
        &self,
        package: PackageSourceSet<'_>,
        authority: PackageExecutionAuthority,
        embedding_runtime: Option<&EmbeddingExecutable>,
        embedding_cache_session: Option<&EmbeddingCacheSession>,
        embedding_requirement: EmbeddingRequirement,
        scratch: &mut LocalCompilerScratch,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(PackageCompilePhase),
    ) -> Result<StagedPackageCompilation, PackageSemanticError> {
        // Move exclusive lane ownership out while this attempt borrows the
        // checked forest and the remaining output scratch. Every ordinary exit
        // restores it. An unwinding attempt drops its authority rather than
        // preserving potentially incomplete mutable solver state.
        let mut typescript_lane = std::mem::take(&mut scratch.typescript_program_lane);
        let result = (|| {
            // A TypeScript source whose own lowering spends its work allowance can have
            // written partial state into the program-wide tables that every later
            // source reads. The attempt is then discarded whole and rerun from fresh
            // native state with that source skipped, so every published fact comes from
            // a state that no interrupted query touched. The final attempt degrades to
            // a coverage gap for the interrupted source and everything after it instead.
            let mut work_gaps = TypeScriptWorkGaps::default();
            let mut restarts_left = MAX_TYPESCRIPT_WORK_RESTARTS;
            loop {
                // Where this attempt's native program stopped, kept outside the attempt so
                // the refusal can name the file and phase even after early returns.
                let stop_site = std::sync::Arc::new(std::sync::OnceLock::new());
                match self.stage_package_sources_attempt(
                    package.clone(),
                    authority,
                    embedding_runtime,
                    embedding_cache_session,
                    embedding_requirement,
                    scratch,
                    cancelled,
                    progress,
                    &mut work_gaps,
                    restarts_left > 0,
                    &stop_site,
                    &mut typescript_lane,
                ) {
                    Err(PackageSemanticError::TypeScriptWorkRestart) if restarts_left > 0 => {
                        restarts_left -= 1;
                        tracing::warn!(target: "compiler.typescript_project", restarts_left,
                        "TypeScript source spent its own work allowance; restarting the package compilation without it");
                    }
                    Err(PackageSemanticError::Compile { path, terminal }) => {
                        return Err(PackageSemanticError::Compile {
                            path,
                            terminal: Box::new(with_typescript_stop_site(
                                *terminal,
                                stop_site.get(),
                            )),
                        });
                    }
                    result => return result,
                }
            }
        })();
        scratch.typescript_program_lane = typescript_lane;
        result
    }

    fn stage_package_sources_attempt(
        &self,
        package: PackageSourceSet<'_>,
        authority: PackageExecutionAuthority,
        embedding_runtime: Option<&EmbeddingExecutable>,
        embedding_cache_session: Option<&EmbeddingCacheSession>,
        embedding_requirement: EmbeddingRequirement,
        scratch: &mut LocalCompilerScratch,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(PackageCompilePhase),
        typescript_work_gaps: &mut TypeScriptWorkGaps,
        may_restart: bool,
        stop_site: &std::sync::Arc<
            std::sync::OnceLock<super::compiler_typescript_observation::ObservedStopSite>,
        >,
        typescript_lane: &mut super::typescript_program_lane::TypeScriptProgramAuthorityLane,
    ) -> Result<StagedPackageCompilation, PackageSemanticError> {
        let (mut execution_identity, plane_execution_seed) = authority.identities();
        let compilation_attempt_id =
            CompilationAttemptId::mint().ok_or(PackageSemanticError::Capacity {
                lane: "compilation attempt identity",
            })?;
        let request = package.request.as_ref();
        let target = package.request.target;
        let mut input = package
            .input_claim
            .unwrap_or_else(|| package_source_input_witness(&package));
        let first_source =
            package
                .compilation_sources()
                .next()
                .ok_or(PackageSemanticError::Capacity {
                    lane: "compilation unit source",
                })?;
        let first_application_request = ApplicationCompilerRequest {
            profile: target.profile,
            stage: target.stage,
            source: first_source.source,
        };
        let first_authority = request_source(first_application_request)
            .map_err(source_terminal)
            .map_err(|terminal| PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(terminal),
            })?;
        let mut control = match package.go_compile_deadline {
            Some(deadline) => CompileControl {
                deadline,
                cancelled,
            },
            None => self.compile_control(cancelled).map_err(|timeout| {
                PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(CompilerTerminal::DeadlineConstruction {
                        source: first_authority,
                        language: target.profile.language(),
                        stage: target.stage,
                        timeout: *timeout,
                    }),
                }
            })?,
        };
        if target.profile.language() == backend_semantic::vocabulary::Language::Go {
            let stop = backend_frontend_go::legacy::GoOracleControl::new(
                Some(cancelled),
                control.deadline,
            )
            .check(
                package
                    .go_witness_deadline_phase
                    .unwrap_or("compiler_lane_wait"),
            )
            .err();
            if let Some(stop) = stop {
                let toolchain = self.toolchain(first_application_request).unwrap_or(
                    ToolchainSelection::ExplicitlyUnavailable {
                        tool: NativeTool::GoCompiler,
                    },
                );
                return Err(PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        PackageAuthorityError::GoOracle(stop),
                    )),
                });
            }
        }
        control.deadline = *typescript_work_gaps
            .deadline
            .get_or_insert(control.deadline);
        let use_report_program = self.package_authority.typescript.is_some_and(|checker| {
            checker.portable_invocation_mode() == TypeScriptInvocationModeV1::ReportProgram
        });
        let typescript_project = if target.profile.language()
            == backend_semantic::vocabulary::Language::TypeScript
            && !use_report_program
        {
            if let Some(host) = self.package_authority.typescript_project_host {
                match host.admit_controlled(package.package_root, &mut || {
                    super::typescript_program::check_admission_control(control.deadline, cancelled)
                }) {
                    Ok(project) => project,
                    Err(cause) => {
                        let toolchain = self.toolchain(first_application_request).unwrap_or(
                            ToolchainSelection::ExplicitlyUnavailable {
                                tool: NativeTool::TypeScriptCompiler,
                            },
                        );
                        let terminal = package_authority_terminal(
                            package.package_target.target(),
                            first_application_request,
                            first_authority,
                            toolchain,
                            PackageAuthorityError::TypeScriptProjectHost(
                                super::compiler_typescript_failure::describe_host_error(
                                    cause,
                                    &[package.package_root],
                                ),
                            ),
                        );
                        return Err(PackageSemanticError::Compile {
                            path: first_source.relative_path.into(),
                            terminal: Box::new(terminal),
                        });
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        let typescript_toolchain = match typescript_project.as_ref() {
            Some(project) => match project.resolved_toolchain() {
                Ok(toolchain) => Some(toolchain),
                Err(source) => {
                    let toolchain = self.toolchain(first_application_request).unwrap_or(
                        ToolchainSelection::ExplicitlyUnavailable {
                            tool: NativeTool::TypeScriptCompiler,
                        },
                    );
                    let terminal = package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        PackageAuthorityError::TypeScriptProjectHost(
                            super::compiler_typescript_failure::describe_host_error(
                                source,
                                &[package.package_root, project.inputs().workspace_root],
                            ),
                        ),
                    );
                    return Err(PackageSemanticError::Compile {
                        path: first_source.relative_path.into(),
                        terminal: Box::new(terminal),
                    });
                }
            },
            None => None,
        };
        let mut package_authority_configuration = self.package_authority;
        if let Some(project) = typescript_project.as_ref() {
            package_authority_configuration.typescript = project.checker();
        }
        // A configured TypeScript package is resolved once into the exact
        // compiler-owned program, then all package files borrow one TSZ
        // checker session. The same finite checkpoint covers parse, bind,
        // merge, check, and every lazy per-file query.
        let mut tsz_authorities = Vec::new();
        let mut tsz_budget = None;
        let mut tsz_package_membership = BTreeMap::new();
        let mut typescript_closure_witness = None;
        let mut native_program_sources = None;
        let mut tsz_node_metadata = Vec::new();
        let mut tsz_file_work_units = Vec::new();
        let mut forest_root = 0;
        let mut persist_configured_programs = false;
        let mut independent_project_set = false;
        let mut typescript_project_execution_fingerprint = None;
        let mut typescript_program_environments = None;
        if let Some(project) = typescript_project.as_ref() {
            let inputs = project.inputs();
            let package_sources = package.compilation_sources().collect::<Vec<_>>();
            let forest = crate::application::typescript_program::build_native_forest_inputs(
                &inputs,
                &package_sources,
                control.deadline,
                cancelled,
            )
            .map_err(|cause| {
                let toolchain = self
                    .toolchain_for_package(first_application_request, typescript_toolchain)
                    .unwrap_or(ToolchainSelection::ExplicitlyUnavailable {
                        tool: NativeTool::TypeScriptCompiler,
                    });
                PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        PackageAuthorityError::TypeScriptProjectHost(
                            super::compiler_typescript_failure::describe_program_admission_error(
                                cause, &inputs,
                            ),
                        ),
                    )),
                }
            })?;
            typescript_program_environments = Some(
                forest
                    .nodes
                    .iter()
                    .map(|node| *node.native.options.environment.as_bytes())
                    .collect(),
            );
            typescript_project_execution_fingerprint = Some(
                crate::application::typescript_program::native_program_execution_fingerprint(
                    inputs.fingerprint,
                    forest
                        .nodes
                        .iter()
                        .map(|node| node.native.options.environment),
                ),
            );
            forest_root = forest.root;
            independent_project_set = forest.project_set;
            // Even one configured program retains its exact config/environment
            // node. The leaf's native program identity and semantic image bytes
            // are unchanged; only its durable ownership envelope gains context.
            persist_configured_programs =
                forest.configured_forest || forest.nodes.iter().any(|node| node.config.is_some());
            tsz_package_membership = forest.package_membership;
            tsz_budget = Some(
                super::compiler_typescript_observation::ObservedTypeScriptBudget::new(
                    control.deadline,
                    cancelled,
                    forest.work_units,
                )
                .with_stop_sink(std::sync::Arc::clone(stop_site)),
            );
            let budget = tsz_budget.as_ref().expect("shared forest budget");
            let active_programs = forest
                .nodes
                .iter()
                .filter(|node| node.native.requires_retained_authority())
                .map(|node| node.config.as_ref().map(|(path, _)| path.clone()))
                .collect::<Vec<_>>();
            typescript_lane
                .prepare(package.package_root, &active_programs)
                .map_err(|_| PackageSemanticError::Capacity {
                    lane: "distinct admitted TypeScript program authorities",
                })?;
            let mut witnesses = Vec::with_capacity(forest.nodes.len());
            for (program_index, node) in forest.nodes.into_iter().enumerate() {
                let program_key = node.config.as_ref().map(|(path, _)| path.clone());
                let native = node.native;
                let retain_authority = native.requires_retained_authority();
                tsz_node_metadata.push((
                    node.config,
                    node.references,
                    *native.options.environment.as_bytes(),
                ));
                witnesses.push(native.closure_witness);
                tsz_file_work_units.push(native.file_work_units);
                if !retain_authority {
                    tsz_authorities.push((None, native.solution));
                    continue;
                }
                let program_sources = native.sources.len();
                budget.admit_program(
                    program_sources,
                    native.libraries.len(),
                    native.options.module_resolutions.len(),
                );
                let _program_observation = budget.observe_program(program_index);
                tracing::info!(target: "compiler.typescript_project",
                    phase = "admitted_program", program_environment = ?native.options.environment,
                    sources = native.sources.len(), libraries = native.libraries.len(),
                    module_requests = native.options.module_resolutions.len(),
                    "native TypeScript project inputs admitted");
                // Each file's semantic check runs under its own slice of the shared
                // allowance. A file that spends its slice is skipped by a rebuilt
                // project, and later becomes a coverage gap instead of a refusal.
                let file_work = TszFileWorkPolicy {
                    allowance: native.file_work_units,
                    skipped: typescript_work_gaps.program_paths(program_index),
                    max_retries: MAX_TYPESCRIPT_FILE_WORK_ROUNDS,
                    // Nothing here reads checker diagnostics, only semantic facts. Checking
                    // every declaration file of every dependency for diagnostics is the
                    // dominant cost of a project that does not set `skipLibCheck`, and no
                    // fact depends on it: declaration types are still resolved on demand.
                    check_declaration_files: false,
                };
                let report = typescript_lane.authority(program_key.clone())
                    .update_with_checker_execution_and_file_work(
                        native.sources,
                        native.options,
                        &native.libraries,
                        TszProjectCheckerExecution {
                            checkpoint: budget,
                            workers: TSZ_CHECKER_WORKERS_PER_COMPILER_LANE,
                        },
                        &file_work,
                    )
                    .map_err(|cause| {
                        // Files still spending their own slice after every permitted
                        // rebuild: name the first, with the slice it spent.
                        if let backend_frontend_typescript::TszAuthorityError::FileWorkExhausted(files) = &cause
                            && let Some(file) = files.first()
                        {
                            let _ = stop_site.set(super::compiler_typescript_observation::ObservedStopSite {
                                reason: backend_frontend_typescript::TszProjectExecutionStop::WorkBudgetExhausted,
                                phase: Some(backend_frontend_typescript::TszProjectExecutionPhase::FileCheck),
                                file: Some(file.path.as_str().into()),
                                work_charged: file.allowance,
                                work_allowance: file.allowance,
                                measurement: None,
                            });
                        }
                        let toolchain = self
                            .toolchain_for_package(first_application_request, typescript_toolchain)
                            .unwrap_or(ToolchainSelection::ExplicitlyUnavailable {
                                tool: NativeTool::TypeScriptCompiler,
                            });
                        PackageSemanticError::Compile {
                            path: first_source.relative_path.into(),
                            terminal: Box::new(package_authority_terminal(
                                package.package_target.target(),
                                first_application_request,
                                first_authority,
                                toolchain,
                                PackageAuthorityError::TypeScriptTsz(cause),
                            )),
                        }
                    })?;
                tracing::info!(target: "compiler.typescript_project", phase = "checked_program",
                    parsed_and_bound = report.parsed_and_bound, reused_binds = report.reused_binds,
                    reused_checked_program = report.reused_project_result,
                    "native TypeScript checked authority admitted for projection");
                if report.reused_project_result {
                    budget.reused_program(program_sources);
                }
                typescript_lane.record(report);
                if let Some(project) = typescript_lane
                    .project(&program_key)
                    .filter(|project| !project.file_work_skipped().is_empty())
                {
                    tracing::warn!(target: "compiler.typescript_project", program = program_index,
                        skipped = ?project.file_work_skipped(),
                        "TypeScript files spent their own work allowance and are not semantically covered");
                }
                tsz_authorities.push((Some(program_key), native.solution));
            }
            typescript_closure_witness = Some(
                crate::application::typescript_program::TypeScriptForestClosureWitness(witnesses),
            );
        }
        // Every retained native image is available to the forest consumer,
        // including the program_diagnostics() of source-free solutions.
        let tsz_images = tsz_authorities
            .iter()
            .map(|(key, _)| {
                key.as_ref()
                    .map(|key| {
                        typescript_lane
                            .project(key)
                            .ok_or(PackageSemanticError::Capacity {
                                lane: "complete admitted TypeScript checked authority",
                            })
                    })
                    .transpose()
            })
            .collect::<Result<Vec<_>, PackageSemanticError>>()?;
        // A solution diagnostic owner contributes no semantic forest leaf or
        // per-source query session. Its checked image remains separately held.
        let tsz_projects = tsz_images
            .iter()
            .zip(&tsz_authorities)
            .map(|(image, (_, solution))| if *solution { None } else { *image })
            .collect::<Vec<_>>();
        if let (Some(budget), Some(toolchain)) = (tsz_budget.as_ref(), typescript_toolchain) {
            let stopped = |cause| PackageSemanticError::Compile {
                path: first_source.relative_path.into(),
                terminal: Box::new(package_authority_terminal(
                    package.package_target.target(),
                    first_application_request,
                    first_authority,
                    ToolchainSelection::ResolvedNative(toolchain),
                    PackageAuthorityError::TypeScriptTsz(
                        backend_frontend_typescript::TszAuthorityError::ExecutionStopped(cause),
                    ),
                )),
            };
            let mut leaves = Vec::with_capacity(tsz_projects.len());
            for (index, project) in tsz_projects.iter().enumerate() {
                leaves.push(project.map(|project| {
                    let _program_observation = budget.observe_program(index);
                    capture_native_program_sources(
                        project,
                        tsz_package_membership.iter().filter_map(|(package, membership)| {
                            match membership {
                                crate::application::typescript_program::TypeScriptForestSourceMembership::Selected { program, program_path } if *program == index => Some((package.as_ref(), program_path.as_ref())),
                                _ => None,
                            }
                        }),
                        toolchain.identity,
                        |units| backend_frontend_typescript::TszExecutionCheckpoint::checkpoint(budget, units).map_err(&stopped),
                    )
                }).transpose()?);
            }
            native_program_sources = Some(if persist_configured_programs {
                let nodes = tsz_node_metadata
                    .into_iter()
                    .zip(leaves)
                    .map(|((config, references, environment), program)| {
                        let (config_path, config) =
                            config.ok_or(PackageSemanticError::Capacity {
                                lane: "TypeScript forest config identity",
                            })?;
                        Ok(backend_semantic::ir::NativeProgramNode {
                            config_path,
                            config,
                            environment,
                            references,
                            program,
                        })
                    })
                    .collect::<Result<Vec<_>, PackageSemanticError>>()?;
                let source_owner = if independent_project_set {
                    backend_semantic::ir::NativeProgramSources::from_project_set(
                        toolchain.identity,
                        forest_root,
                        nodes,
                    )
                } else {
                    backend_semantic::ir::NativeProgramSources::from_forest(
                        toolchain.identity,
                        forest_root,
                        nodes,
                    )
                };
                source_owner.map_err(PackageSemanticError::NativeProgramSources)?
            } else {
                leaves
                    .into_iter()
                    .next()
                    .flatten()
                    .ok_or(PackageSemanticError::Capacity {
                        lane: "TypeScript single program membership",
                    })?
                    .into()
            });
        }
        let tsz_sessions = tsz_projects.iter().enumerate().map(|(program_index, project)| {
            project.map(|project| {
                let budget = tsz_budget.as_ref().ok_or(PackageSemanticError::Capacity { lane: "TypeScript forest checker budget" })?;
                let _program_observation = budget.observe_program(program_index);
                let file_work_units = tsz_file_work_units.get(program_index).copied().ok_or(PackageSemanticError::Capacity { lane: "TypeScript program file work allowance" })?;
                let sources = native_program_sources.as_ref().ok_or(PackageSemanticError::Capacity { lane: "TypeScript query source membership" })?;
                let manifest = match sources.nodes() {
                    Some((_, nodes)) => nodes.get(program_index).and_then(|node| node.program.as_ref()),
                    None if program_index == 0 => sources.single_program(),
                    None => None,
                }.ok_or(PackageSemanticError::Capacity { lane: "TypeScript owning query source membership" })?;
                project.checked_query_session_with_file_work(budget, file_work_units)
                    .map_err(backend_frontend_typescript::TszAuthorityError::ProjectCheckerSession)
                    .and_then(|session| session.authenticate_native_program_sources(manifest))
                    .map_err(|cause| {
                    let toolchain = self.toolchain_for_package(first_application_request, typescript_toolchain)
                        .unwrap_or(ToolchainSelection::ExplicitlyUnavailable { tool: NativeTool::TypeScriptCompiler });
                    PackageSemanticError::Compile {
                        path: first_source.relative_path.into(),
                        terminal: Box::new(package_authority_terminal(
                            package.package_target.target(), first_application_request, first_authority, toolchain,
                            PackageAuthorityError::TypeScriptTsz(cause),
                        )),
                    }
                })
            }).transpose()
        }).collect::<Result<Vec<_>, PackageSemanticError>>()?;
        // One fresh native Python State owns the entire selected frontier.
        // It is deliberately not reused: external/configuration negative reads
        // have no complete observation contract yet.
        let python_project = if target.profile.language()
            == backend_semantic::vocabulary::Language::Python
            && matches!(
                package_authority_configuration.python_checker,
                super::LocalRuntimePythonCheckerAdmission::Native { .. }
            ) {
            let toolchain = self
                .toolchain(first_application_request)
                .map_err(|cause| {
                    toolchain_terminal(first_authority, first_application_request, cause)
                })
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?;
            let sources = if let Some(raw) = package.python_raw_sources {
                raw.to_vec()
            } else {
                package
                    .sources
                    .iter()
                    .map(|source| {
                        backend_frontend_python::legacy::checker::PythonProjectBytesSource {
                            relative_path: source.relative_path,
                            source: source.source.as_bytes(),
                        }
                    })
                    .collect::<Vec<_>>()
            };
            let first_source_path = package.package_root.join(first_source.relative_path);
            Some(
                super::package_authority::enter_python_project_authority(
                    PackageAuthorityRequest {
                        package_root: package.package_root,
                        source_path: &first_source_path,
                        source: first_source.source.as_bytes(),
                        java_sourcepath: &[],
                        unit_key: package.package_target.unit_key(),
                        profile: target.profile,
                        toolchain,
                        control,
                        configuration: package_authority_configuration,
                    },
                    request.lineage_name(),
                    &sources,
                    package.python_environment,
                )
                .map_err(|cause| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        cause,
                    )),
                })?,
            )
        } else {
            None
        };
        if python_project.is_none()
            && package.python_raw_sources.is_some_and(|sources| {
                sources.iter().any(|source| {
                    backend_frontend_python::legacy::checker::decode_python_source(source.source)
                        .is_err()
                })
            })
        {
            return Err(PackageSemanticError::Capacity {
                lane: "unavailable Python byte intake requires native project authority",
            });
        }
        // Captured configuration and actual producer facts must affect identity,
        // including direct callers whose source-only fallback claim omits config.
        let mut plane_execution_seed = if let Some(project) = python_project.as_ref() {
            let fingerprint = project.witness().fingerprint();
            // An admitted caller claim identifies its exact publication attempt.
            // Preserve that opaque provenance; the execution seed below still
            // binds captured configuration and producer facts for every caller.
            // Only the compiler's source-only fallback needs an expanded root.
            if package.input_claim.is_none() {
                let mut root = blake3::Hasher::new();
                root.update(b"compiler.python.project-input.v1\0");
                root.update(input.input_root());
                root.update(input.read_manifest_root().as_bytes());
                root.update(&fingerprint.as_bytes());
                let root = *root.finalize().as_bytes();
                input = SemanticInputWitness::claimed_state(
                    root,
                    ScopeRoot::from_bytes(root),
                    Coverage::Partial,
                );
            }
            plane_execution_seed.map(|seed| seed.with_python_project(fingerprint))
        } else {
            plane_execution_seed
        };
        let source_count = package
            .python_raw_sources
            .map_or_else(|| package.compilation_sources().count(), |raw| raw.len());
        if target.profile.language() == backend_semantic::vocabulary::Language::Rust
            && let Some(configuration) = self.package_authority.rust
        {
            for source in package.compilation_sources() {
                if cancelled.load(Ordering::Acquire) {
                    return Err(PackageSemanticError::Compile {
                        path: source.relative_path.into(),
                        terminal: Box::new(CompilerTerminal::PackageCancelled {
                            target: package.package_target.target(),
                            phase: PackageCompilePhase::Authority,
                        }),
                    });
                }
                let application_request = ApplicationCompilerRequest {
                    profile: target.profile,
                    stage: target.stage,
                    source: source.source,
                };
                let source_authority = request_source(application_request)
                    .map_err(source_terminal)
                    .map_err(|terminal| PackageSemanticError::Compile {
                        path: source.relative_path.into(),
                        terminal: Box::new(terminal),
                    })?;
                let toolchain = self
                    .toolchain(application_request)
                    .map_err(|cause| {
                        toolchain_terminal(source_authority, application_request, cause)
                    })
                    .map_err(|terminal| PackageSemanticError::Compile {
                        path: source.relative_path.into(),
                        terminal: Box::new(terminal),
                    })?;
                if Instant::now() >= control.deadline {
                    return Err(PackageSemanticError::Compile {
                        path: source.relative_path.into(),
                        terminal: Box::new(package_authority_terminal(
                            package.package_target.target(),
                            application_request,
                            source_authority,
                            toolchain,
                            PackageAuthorityError::Deadline {
                                profile: target.profile,
                                stage: crate::application::package_authority::PackageAuthorityStage::RustProject,
                            },
                        )),
                    });
                }
                if source.source.len() > *configuration.maximum_source_bytes as usize {
                    return Err(PackageSemanticError::Compile {
                        path: source.relative_path.into(),
                        terminal: Box::new(package_authority_terminal(
                            package.package_target.target(),
                            application_request,
                            source_authority,
                            toolchain,
                            PackageAuthorityError::RustProject(RustAuthorityError::SourceBudget {
                                actual: u64::try_from(source.source.len()).unwrap_or(u64::MAX),
                                maximum: configuration.maximum_source_bytes,
                            }),
                        )),
                    });
                }
            }
        }
        require_package_embedding_runtime(
            first_source.relative_path,
            embedding_runtime.is_some(),
            package.embedding_provisioning_failure,
            embedding_requirement,
        )?;
        let embedding_execution_identity =
            embedding_runtime.map(EmbeddingExecutable::execution_identity);
        let embedding_identity = embedding_execution_identity
            .map(semantic_embedding_identity)
            .transpose()
            .map_err(|cause| PackageSemanticError::Embedding {
                path: first_source.relative_path.into(),
                cause,
            })?;
        let embedding_extent =
            embedding_runtime.map_or(0, EmbeddingExecutable::canonical_payload_max_bytes);
        let mut embedding_unavailable = source_count
            .checked_mul(embedding_extent)
            .filter(|extent| *extent > MAX_PACKAGE_EMBEDDING_BYTES)
            .map(|_| "package embedding output exceeds its bounded byte budget".into());
        if embedding_requirement == EmbeddingRequirement::Required
            && embedding_unavailable.is_some()
        {
            return Err(PackageSemanticError::Embedding {
                path: first_source.relative_path.into(),
                cause: "package embedding output exceeds its bounded byte budget".into(),
            });
        }
        let mut embedding_artifacts = Vec::new();
        let mut embedding_sources = Vec::new();
        if embedding_runtime.is_some() && embedding_unavailable.is_none() {
            if let Err(error) = embedding_artifacts.try_reserve_exact(source_count) {
                if embedding_requirement == EmbeddingRequirement::Required {
                    return Err(PackageSemanticError::Embedding {
                        path: first_source.relative_path.into(),
                        cause: error.to_string().into(),
                    });
                }
                embedding_unavailable = Some("embedding output allocation failed".into());
            }
            if embedding_unavailable.is_none()
                && embedding_sources.try_reserve_exact(source_count).is_err()
            {
                if embedding_requirement == EmbeddingRequirement::Required {
                    return Err(PackageSemanticError::Embedding {
                        path: first_source.relative_path.into(),
                        cause: "embedding input allocation failed".into(),
                    });
                }
                embedding_unavailable = Some("embedding input allocation failed".into());
            }
        }
        let mut artifacts = Vec::new();
        artifacts
            .try_reserve_exact(source_count)
            .map_err(PackageSemanticError::Allocation)?;
        let mut coverage_gaps = Vec::new();
        let mut degraded = DegradedSources::default();
        if let (Some(project), Some(raw_sources)) =
            (python_project.as_ref(), package.python_raw_sources)
        {
            for raw in raw_sources {
                if package
                    .sources
                    .binary_search_by_key(&raw.relative_path, |source| source.relative_path)
                    .is_ok()
                {
                    continue;
                }
                let status = project.source_status(raw.relative_path).ok_or(
                    PackageSemanticError::Capacity {
                        lane: "raw Python resource status",
                    },
                )?;
                if !matches!(
                    status,
                    backend_frontend_python::legacy::checker::PythonProjectSourceStatus::UnavailableEncoding(_)
                ) {
                    return Err(PackageSemanticError::Capacity {
                        lane: "raw Python resource has unexpected decoded status",
                    });
                }
                let byte_len = u32::try_from(raw.source.len()).map_err(|_| {
                    PackageSemanticError::Capacity {
                        lane: "raw Python source extent",
                    }
                })?;
                coverage_gaps.push(PackageSourceCoverageGap {
                    source: SourceAuthority {
                        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(raw.source),
                        byte_len,
                    },
                    relative_path: raw.relative_path.into(),
                    cause: PackageSourceCoverageGapCause::PythonSourceEncodingUnavailable,
                    python_source_status: Some(status.clone()),
                    native_python_diagnostics: project
                        .diagnostics()
                        .iter()
                        .filter(|diagnostic| diagnostic.relative_path.as_ref() == raw.relative_path)
                        .cloned()
                        .collect(),
                    typescript_diagnostic: None,
                });
            }
        }

        let mut image_plan = Vec::new();
        image_plan
            .try_reserve_exact(source_count)
            .map_err(PackageSemanticError::Allocation)?;
        let mut semantic_images = Vec::new();
        let mut fragment_bytes = 0_usize;
        let mut semantic_bytes = 0_usize;

        // Cross-call reuse is disabled because the RA owner cannot report all
        // positive and negative reads. This operation's lease keeps its fresh
        // database alive through staging; every exit drops it after completion.
        let mut deferred_rust_authority = false;
        let mut rust_request_authority = None;
        let rust_workspace_lease =
            if let backend_semantic::vocabulary::LanguageProfile::Rust(edition) = target.profile {
                let toolchain = self
                    .toolchain(first_application_request)
                    .map_err(|cause| {
                        toolchain_terminal(first_authority, first_application_request, cause)
                    })
                    .map_err(|terminal| PackageSemanticError::Compile {
                        path: first_source.relative_path.into(),
                        terminal: Box::new(terminal),
                    })?;
                let authority_configuration = self.package_authority.rust.ok_or_else(|| {
                    let cause = PackageAuthorityError::AdapterUnavailable {
                    profile: target.profile,
                    stage:
                        crate::application::package_authority::PackageAuthorityStage::RustProject,
                };
                    PackageSemanticError::Compile {
                        path: first_source.relative_path.into(),
                        terminal: Box::new(package_authority_terminal(
                            package.package_target.target(),
                            first_application_request,
                            first_authority,
                            toolchain,
                            cause,
                        )),
                    }
                })?;
                // Compare exact selected executables before request-time cache creation/probing.
                let resolved = match toolchain {
                    ToolchainSelection::ResolvedNative(resolved) => resolved,
                    ToolchainSelection::ExplicitlyUnavailable { tool } => {
                        return Err(PackageSemanticError::Compile {
                            path: first_source.relative_path.into(),
                            terminal: Box::new(package_authority_terminal(
                                package.package_target.target(),
                                first_application_request,
                                first_authority,
                                toolchain,
                                PackageAuthorityError::ToolchainUnavailable {
                                    profile: target.profile,
                                    tool,
                                },
                            )),
                        });
                    }
                };
                let external =
                    resolved
                        .external()
                        .map_err(|source| PackageSemanticError::Compile {
                            path: first_source.relative_path.into(),
                            terminal: Box::new(package_authority_terminal(
                                package.package_target.target(),
                                first_application_request,
                                first_authority,
                                toolchain,
                                PackageAuthorityError::ExternalToolchainRequired {
                                    profile: target.profile,
                                    source,
                                },
                            )),
                        })?;
                if authority_configuration.toolchain.selected_rustc() != external.executable() {
                    return Err(PackageSemanticError::Compile {
                        path: first_source.relative_path.into(),
                        terminal: Box::new(package_authority_terminal(
                            package.package_target.target(),
                            first_application_request,
                            first_authority,
                            toolchain,
                            PackageAuthorityError::RustToolchainExecutableMismatch {
                                profile: target.profile,
                                configured: authority_configuration
                                    .toolchain
                                    .selected_rustc()
                                    .to_path_buf()
                                    .into_boxed_path(),
                                resolved: external.executable().to_path_buf().into_boxed_path(),
                            },
                        )),
                    });
                }
                let rust_toolchain = authority_configuration
                    .resolve_toolchain(target.profile, control)
                    .map_err(|cause| {
                        package_authority_terminal(
                            package.package_target.target(),
                            first_application_request,
                            first_authority,
                            toolchain,
                            cause,
                        )
                    })
                    .map_err(|terminal| PackageSemanticError::Compile {
                        path: first_source.relative_path.into(),
                        terminal: Box::new(terminal),
                    })?;
                let request_seed = AdmittedRustRequestAuthority::new(
                    package.package_target.target(),
                    target.profile,
                    target.stage,
                    resolved,
                    rust_toolchain,
                    authority_configuration,
                    self.package_authority.maximum_image_bytes,
                )
                .ok_or(PackageSemanticError::Capacity {
                    lane: "admitted Rust request authority identity",
                })?;
                deferred_rust_authority = authority_configuration.toolchain.is_deferred();
                if deferred_rust_authority {
                    // No caller/startup path-policy identity may survive actual Rust admission.
                    execution_identity = None;
                }
                plane_execution_seed = None;
                rust_request_authority = Some(request_seed);
                let toolchain_identity = Some(request_seed.toolchain_identity());
                let environment_identity = Some(request_seed.environment_identity());
                let local_authority_identity = Some(request_seed.local_authority_fingerprint());
                let source_paths = package
                    .sources
                    .iter()
                    .map(|source| PathBuf::from(source.relative_path))
                    .collect::<Vec<_>>();
                let key = RustWorkspaceSessionKey::new(
                    package.package_root,
                    rust_toolchain,
                    edition,
                    target.stage,
                    authority_configuration.features,
                    authority_configuration.metadata_policy,
                    toolchain_identity,
                    environment_identity,
                    local_authority_identity,
                    *package.package_target.target().as_ref(),
                    &source_paths,
                )
                .and_then(|key| match package.package_target.unit_key() {
                    CompilationUnitKeyV2::RustCrate { name, root } => {
                        key.with_cargo_target(RustCargoTarget {
                            name: name.as_ref(),
                            root: Path::new(root.as_ref()),
                        })
                    }
                    CompilationUnitKeyV2::PackageRoot => Ok(key),
                    _ => unreachable!("Rust package authority validates its compilation-unit key"),
                })
                .map_err(|cause| {
                    package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        PackageAuthorityError::RustProject(cause),
                    )
                })
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?;
                let source_frontier = package
                    .sources
                    .iter()
                    .map(|source| RustWorkspaceFile {
                        relative_path: Path::new(source.relative_path),
                        source: source.source,
                    })
                    .collect::<Vec<_>>();
                Some(
                    begin_rust_workspace_with_observation(
                        &mut scratch.rust_workspace_session_lane,
                        key,
                        &source_frontier,
                        RustAnalysisControl {
                            cancelled: control.cancelled,
                            maximum_source_bytes: authority_configuration.maximum_source_bytes,
                            deadline: control.deadline,
                        },
                        compilation_attempt_id,
                    )
                    .map_err(|cause| {
                        package_authority_terminal(
                            package.package_target.target(),
                            first_application_request,
                            first_authority,
                            toolchain,
                            PackageAuthorityError::RustProject(cause),
                        )
                    })
                    .map_err(|terminal| PackageSemanticError::Compile {
                        path: first_source.relative_path.into(),
                        terminal: Box::new(terminal),
                    })?,
                )
            } else {
                None
            };
        let rust_target_source_closure = match package.package_target.unit_key() {
            CompilationUnitKeyV2::RustCrate { name, root } => {
                let lease = rust_workspace_lease.as_ref().ok_or(
                    PackageSemanticError::Capacity {
                        lane: "selected Rust Cargo target workspace",
                    },
                )?;
                let selected_target = RustCargoTarget {
                    name: name.as_ref(),
                    root: Path::new(root.as_ref()),
                };
                let toolchain = self
                    .toolchain(first_application_request)
                    .map_err(|cause| {
                        toolchain_terminal(first_authority, first_application_request, cause)
                    })
                    .map_err(|terminal| PackageSemanticError::Compile {
                        path: first_source.relative_path.into(),
                        terminal: Box::new(terminal),
                    })?;
                Some(
                    lease
                        .workspace()
                        .source_closure_for_target(
                            selected_target,
                            RustAnalysisControl {
                                cancelled: control.cancelled,
                                maximum_source_bytes: self.package_authority.rust
                                    .ok_or(PackageSemanticError::Capacity {
                                        lane: "Rust target closure authority configuration",
                                    })?
                                    .maximum_source_bytes,
                                deadline: control.deadline,
                            },
                        )
                        .map_err(|cause| {
                            package_authority_terminal(
                                package.package_target.target(),
                                first_application_request,
                                first_authority,
                                toolchain,
                                PackageAuthorityError::RustProject(cause),
                            )
                        })
                        .map_err(|terminal| PackageSemanticError::Compile {
                            path: first_source.relative_path.into(),
                            terminal: Box::new(terminal),
                        })?,
                )
            }
            CompilationUnitKeyV2::PackageRoot => None,
            _ => None,
        };
        if package
            .expected_rust_target_source_closure
            .is_some_and(|expected| {
                rust_target_source_closure
                    .as_ref()
                    .is_none_or(|observed| observed.identity() != expected)
            })
        {
            return Err(PackageSemanticError::RustTargetSourceClosureMismatch);
        }
        let rust_compilation_sources = if let Some(closure) = &rust_target_source_closure {
            if target.profile.language() != backend_semantic::vocabulary::Language::Rust {
                return Err(PackageSemanticError::Capacity {
                    lane: "Rust target closure profile binding",
                });
            }
            let mut selected = Vec::with_capacity(closure.source_count());
            for member in closure.sources() {
                let source = package
                    .sources
                    .binary_search_by(|source| source.relative_path().cmp(member.relative_path()))
                    .ok()
                    .and_then(|index| package.sources.get(index).copied())
                    .ok_or(PackageSemanticError::Capacity {
                        lane: "Rust target closure source membership",
                    })?;
                let observed = backend_semantic::ir::SourceIdentity::from_bytes(
                    source.source().as_bytes(),
                )
                .ok_or(PackageSemanticError::Capacity {
                    lane: "Rust target closure source identity",
                })?;
                if observed != member.source() {
                    return Err(PackageSemanticError::Capacity {
                        lane: "Rust target closure source binding",
                    });
                }
                selected.push(source);
            }
            selected
        } else {
            package.compilation_sources().collect::<Vec<_>>()
        };
        if let (CompilationUnitKeyV2::RustCrate { .. }, Some(closure)) = (
            package.package_target.unit_key(),
            rust_target_source_closure.as_ref(),
        ) {
            // Bind reuse and staged identity to the exact selected target
            // frontier, even when a direct caller supplied an input claim.
            let mut identity = blake3::Hasher::new();
            identity.update(b"backend.compiler.rust-target-input-frontier.v1\0");
            identity.update(input.input_root());
            identity.update(input.read_manifest_root().as_bytes());
            identity.update(&closure.identity());
            let root = *identity.finalize().as_bytes();
            input = SemanticInputWitness::claimed_state(
                root,
                ScopeRoot::from_bytes(root),
                Coverage::Partial,
            );
        }
        let rust_workspace_authority = if let Some(lease) = rust_workspace_lease.as_ref() {
            let source_path = package.package_root.join(first_source.relative_path);
            let toolchain = self
                .toolchain(first_application_request)
                .map_err(|cause| {
                    toolchain_terminal(first_authority, first_application_request, cause)
                })
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?;
            Some(
                enter_package_authority_with_rust_workspace(
                    PackageAuthorityRequest {
                        package_root: package.package_root,
                        source_path: &source_path,
                        source: first_source.source.as_bytes(),
                        java_sourcepath: &[],
                        unit_key: package.package_target.unit_key(),
                        profile: target.profile,
                        toolchain,
                        control,
                        configuration: self.package_authority,
                    },
                    package.go_authority_witness,
                    lease.workspace(),
                )
                .map_err(|cause| {
                    package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        cause,
                    )
                })
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?,
            )
        } else {
            None
        };

        // Set once the program can no longer lower sources soundly (the final
        // attempt's interrupted source, or the program-wide allowance running out).
        // Sources lowered before that point are complete; every later TypeScript
        // source is reported as a coverage gap instead of failing the package.
        let mut typescript_work_degraded = false;
        // A Go package is admitted once and its sources are loaded one directory at
        // a time: entering authority per source re-proved the toolchain and the
        // dependency closure and re-ran the loader for every file.
        let go_first_source_path = package.package_root.join(first_source.relative_path);
        let mut go_authority = if target.profile.language()
            == backend_semantic::vocabulary::Language::Go
            && package_authority_configuration.go.is_some_and(|oracle| {
                oracle.portable_invocation_options().mode()
                    == backend_frontend_go::legacy::GoOracleInvocationModeV1::GoToolchain
            })
            && rust_workspace_authority.is_none()
            && typescript_project.is_none()
            && python_project.is_none()
        {
            let toolchain = self
                .toolchain(first_application_request)
                .map_err(|cause| {
                    toolchain_terminal(first_authority, first_application_request, cause)
                })
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?;
            let go_sources = package
                .compilation_sources()
                .map(|source| package.package_root.join(source.relative_path))
                .collect::<Vec<_>>();
            Some(
                GoPackageAuthority::begin(
                    PackageAuthorityRequest {
                        package_root: package.package_root,
                        source_path: &go_first_source_path,
                        source: first_source.source.as_bytes(),
                        java_sourcepath: &[],
                        unit_key: package.package_target.unit_key(),
                        profile: target.profile,
                        toolchain,
                        control,
                        configuration: package_authority_configuration,
                    },
                    package.go_authority_witness,
                    &go_sources,
                )
                .map_err(|cause| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        cause,
                    )),
                })?,
            )
        } else {
            None
        };

        for source in rust_compilation_sources.iter().copied() {
            if cancelled.load(Ordering::Acquire) {
                return Err(PackageSemanticError::Compile {
                    path: source.relative_path.into(),
                    terminal: Box::new(CompilerTerminal::PackageCancelled {
                        target: package.package_target.target(),
                        phase: PackageCompilePhase::Lower,
                    }),
                });
            }
            progress(PackageCompilePhase::Authority);
            let application_request = ApplicationCompilerRequest {
                profile: target.profile,
                stage: target.stage,
                source: source.source,
            };
            let source_authority = request_source(application_request)
                .map_err(source_terminal)
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?;
            if degraded.deadline_passed {
                degraded.skip_after_deadline(
                    source.relative_path,
                    source_authority,
                    &mut coverage_gaps,
                );
                continue;
            }
            if let Some(budget) = tsz_budget.as_ref().filter(|_| !typescript_work_degraded) {
                match backend_frontend_typescript::TszExecutionCheckpoint::checkpoint(budget, 1) {
                    // True program-wide exhaustion: keep what is already complete.
                    Err(
                        backend_frontend_typescript::TszProjectExecutionStop::WorkBudgetExhausted,
                    ) if !artifacts.is_empty() => {
                        typescript_work_degraded = true;
                        Ok(())
                    }
                    result => result,
                }
                .map_err(|cause| PackageSemanticError::Compile {
                    path: source.relative_path.into(),
                    terminal: Box::new(package_authority_terminal(
                        package.package_target.target(),
                        application_request,
                        source_authority,
                        self.toolchain_for_package(application_request, typescript_toolchain)
                            .unwrap_or(ToolchainSelection::ExplicitlyUnavailable {
                                tool: NativeTool::TypeScriptCompiler,
                            }),
                        PackageAuthorityError::TypeScriptTsz(
                            backend_frontend_typescript::TszAuthorityError::ExecutionStopped(cause),
                        ),
                    )),
                })?;
            }
            let mut typescript_owner: Option<(usize, &str)> = None;
            if typescript_project.is_some() {
                let membership = tsz_package_membership.get(source.relative_path()).ok_or(
                    PackageSemanticError::Capacity {
                        lane: "TypeScript package source membership",
                    },
                )?;
                if matches!(
                    membership,
                    crate::application::typescript_program::TypeScriptForestSourceMembership::ExcludedFromConfiguredProgram
                ) {
                    coverage_gaps.push(PackageSourceCoverageGap {
                        source: source_authority,
                        relative_path: source.relative_path.into(),
                        cause: PackageSourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,
                        python_source_status: None,
                        native_python_diagnostics: Box::new([]),
                        typescript_diagnostic: None,
                    });
                    continue;
                }
                if let crate::application::typescript_program::TypeScriptForestSourceMembership::Selected { program, program_path } = membership {
                    typescript_owner = Some((*program, &**program_path));
                    // Skipped by this attempt's own checked project, named by an earlier
                    // attempt, or past the point where lowering stopped being sound.
                    let work_gap = typescript_work_degraded
                        || typescript_work_gaps.contains(*program, program_path)
                        || tsz_projects
                            .get(*program)
                            .copied()
                            .flatten()
                            .is_some_and(|project| project.file_work_skipped().contains(&**program_path));
                    if work_gap {
                        coverage_gaps.push(PackageSourceCoverageGap {
                            source: source_authority,
                            relative_path: source.relative_path.into(),
                            cause: PackageSourceCoverageGapCause::TypeScriptSourceWorkBudgetExhausted,
                            python_source_status: None,
                            native_python_diagnostics: Box::new([]),
                            typescript_diagnostic: None,
                        });
                        continue;
                    }
                }
            }
            if let Some(project) = python_project.as_ref() {
                use backend_frontend_python::legacy::checker::PythonProjectSourceStatus;
                let status = project.source_status(source.relative_path).ok_or(
                    PackageSemanticError::Capacity {
                        lane: "Python project source admission status",
                    },
                )?;
                let cause = match status {
                    PythonProjectSourceStatus::Analyzed => None,
                    PythonProjectSourceStatus::UnavailableSyntax => {
                        Some(PackageSourceCoverageGapCause::PythonSourceSyntaxUnavailable)
                    }
                    PythonProjectSourceStatus::UnavailableEncoding(_) => {
                        Some(PackageSourceCoverageGapCause::PythonSourceEncodingUnavailable)
                    }
                    PythonProjectSourceStatus::UnavailableDependency { .. }
                    | PythonProjectSourceStatus::UnavailableCompiledImport { .. } => {
                        Some(PackageSourceCoverageGapCause::PythonSourceDependencyUnavailable)
                    }
                };
                if let Some(cause) = cause {
                    coverage_gaps.push(PackageSourceCoverageGap {
                        source: source_authority,
                        relative_path: source.relative_path.into(),
                        cause,
                        python_source_status: Some(status.clone()),
                        native_python_diagnostics: project
                            .diagnostics()
                            .iter()
                            .filter(|diagnostic| {
                                diagnostic.relative_path.as_ref() == source.relative_path
                            })
                            .cloned()
                            .collect(),
                        typescript_diagnostic: None,
                    });
                    continue;
                }
            }
            let toolchain = self
                .toolchain_for_package(application_request, typescript_toolchain)
                .map_err(|cause| toolchain_terminal(source_authority, application_request, cause))
                .map_err(|terminal| PackageSemanticError::Compile {
                    path: source.relative_path.into(),
                    terminal: Box::new(terminal),
                })?;
            let scope =
                DeclarationScope::for_package(request, source.relative_path).map_err(|_| {
                    PackageSemanticError::Scope {
                        path: source.relative_path.into(),
                    }
                })?;
            let source_path = package.package_root.join(source.relative_path);
            let java_sourcepath = if matches!(target.profile, LanguageProfile::Java(_)) {
                package
                    .compilation_sources()
                    .filter(|captured| captured.relative_path() != source.relative_path)
                    .map(|captured| JavaSource {
                        name: Path::new(captured.relative_path()),
                        bytes: captured.source().as_bytes(),
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let go_owner = match go_authority.as_mut() {
                Some(go) => match go.owner(&source_path) {
                    Ok(owner) => Some(owner),
                    Err(cause) => {
                        let terminal = package_authority_terminal(
                            package.package_target.target(),
                            application_request,
                            source_authority,
                            toolchain,
                            cause,
                        );
                        degraded.degrade(
                            terminal,
                            source.relative_path,
                            source_authority,
                            &mut coverage_gaps,
                        )?;
                        continue;
                    }
                },
                None => None,
            };
            let transient_authority = if go_owner.is_some() {
                go_owner
            } else if rust_workspace_authority.is_none()
                && typescript_project.is_none()
                && python_project.is_none()
            {
                Some(
                    enter_package_authority_with_go_authority_witness(
                        PackageAuthorityRequest {
                            package_root: package.package_root,
                            source_path: &source_path,
                            source: source.source.as_bytes(),
                            java_sourcepath: &java_sourcepath,
                            unit_key: package.package_target.unit_key(),
                            profile: target.profile,
                            toolchain,
                            control,
                            configuration: package_authority_configuration,
                        },
                        package.go_authority_witness,
                    )
                    .map_err(|cause| {
                        package_authority_terminal(
                            package.package_target.target(),
                            application_request,
                            source_authority,
                            toolchain,
                            cause,
                        )
                    })
                    .map_err(|terminal| PackageSemanticError::Compile {
                        path: source.relative_path.into(),
                        terminal: Box::new(terminal),
                    })?,
                )
            } else {
                None
            };
            let _typescript_program_observation = if let Some(
                crate::application::typescript_program::TypeScriptForestSourceMembership::Selected { program, .. }
            ) = tsz_package_membership.get(source.relative_path()) {
                let budget = tsz_budget.as_ref().ok_or(PackageSemanticError::Capacity { lane: "TypeScript selected source observation budget" })?;
                Some(budget.observe_program(*program))
            } else {
                None
            };
            let authority = if typescript_project.is_some() {
                let Some(crate::application::typescript_program::TypeScriptForestSourceMembership::Selected { program, program_path }) = tsz_package_membership.get(source.relative_path()) else {
                        return Err(PackageSemanticError::Capacity { lane: "TypeScript selected source owner" });
                    };
                let project = tsz_projects.get(*program).copied().flatten().ok_or(
                    PackageSemanticError::Capacity {
                        lane: "TypeScript owning program",
                    },
                )?;
                let session = tsz_sessions.get(*program).and_then(Option::as_ref).ok_or(
                    PackageSemanticError::Capacity {
                        lane: "TypeScript owning query session",
                    },
                )?;
                crate::driver::SemanticAuthorityInput::TypeScriptTsz {
                    project,
                    session,
                    source_path: program_path,
                }
            } else if let Some(project) = python_project.as_ref() {
                let report =
                    project
                        .module(source.relative_path)
                        .ok_or(PackageSemanticError::Capacity {
                            lane: "Python project module report",
                        })?;
                crate::driver::SemanticAuthorityInput::Python { report }
            } else {
                let authority_owner = rust_workspace_authority
                    .as_ref()
                    .or(transient_authority.as_ref())
                    .ok_or(PackageSemanticError::Capacity {
                        lane: "package authority owner",
                    })?;
                authority_owner.input(&source_path)
            };
            let lowered = self.stage_prepared(
                application_request,
                source_authority,
                scope,
                toolchain,
                authority,
                control,
                &mut scratch.diagnostic_output,
                &mut scratch.fragment_output,
                progress,
            );
            if let Some((program, program_path)) = typescript_owner {
                let file_exhaustion = tsz_sessions
                    .get(program)
                    .and_then(Option::as_ref)
                    .and_then(|session| session.take_file_work_exhaustion());
                let file_exhausted = file_exhaustion.is_some();
                let program_exhausted = lowered.is_err()
                    && tsz_budget.as_ref().is_some_and(|budget| {
                        budget.stop_reason() == Some(backend_frontend_typescript::TszProjectExecutionStop::WorkBudgetExhausted)
                    });
                if file_exhausted || (program_exhausted && !artifacts.is_empty()) {
                    // The interrupted queries may have written partial state into the
                    // program-wide tables every later source reads, and this source has
                    // no complete facts. Rerun from fresh native state without it...
                    if file_exhausted && may_restart {
                        typescript_work_gaps.insert(program, program_path);
                        for (index, project) in tsz_projects.iter().enumerate() {
                            for path in project.map_or_else(Default::default, |project| {
                                project.file_work_skipped().clone()
                            }) {
                                typescript_work_gaps.insert(index, path);
                            }
                        }
                        return Err(PackageSemanticError::TypeScriptWorkRestart);
                    }
                    // ...or, with no restart left, keep the sources that are already
                    // complete and report this one and every later one as gaps.
                    if !artifacts.is_empty() {
                        typescript_work_degraded = true;
                        coverage_gaps.push(PackageSourceCoverageGap {
                            source: source_authority,
                            relative_path: source.relative_path.into(),
                            cause:
                                PackageSourceCoverageGapCause::TypeScriptSourceWorkBudgetExhausted,
                            python_source_status: None,
                            native_python_diagnostics: Box::new([]),
                            typescript_diagnostic: None,
                        });
                        continue;
                    }
                    // Nothing complete to publish and no restart left: the refusal below
                    // names this source and the slice it spent.
                    if let Some(exhaustion) = file_exhaustion {
                        let _ = stop_site.set(super::compiler_typescript_observation::ObservedStopSite {
                            reason: backend_frontend_typescript::TszProjectExecutionStop::WorkBudgetExhausted,
                            phase: None,
                            file: Some(exhaustion.path.as_str().into()),
                            work_charged: exhaustion.allowance,
                            work_allowance: exhaustion.allowance,
                            measurement: None,
                        });
                        return Err(PackageSemanticError::Compile {
                            path: source.relative_path.into(),
                            terminal: Box::new(package_authority_terminal(
                                package.package_target.target(), application_request, source_authority,
                                toolchain,
                                PackageAuthorityError::TypeScriptTsz(
                                    backend_frontend_typescript::TszAuthorityError::FileWorkExhausted(vec![exhaustion]),
                                ),
                            )),
                        });
                    }
                }
            }
            let compiled = match lowered {
                Ok(compiled) => compiled,
                Err(terminal)
                    if matches!(
                        &terminal,
                        CompilerTerminal::Compile {
                            attempted,
                            cause: CompilerCause::Authority {
                                class: AuthorityDiagnosticClass::SourceScope,
                                ..
                            },
                        } if target.profile.language() == backend_semantic::vocabulary::Language::Rust
                            && attempted.source == source_authority
                    ) =>
                {
                    coverage_gaps.push(PackageSourceCoverageGap {
                        source: source_authority,
                        relative_path: source.relative_path.into(),
                        cause: PackageSourceCoverageGapCause::RustSourceOutsideActiveCargoTarget,
                        python_source_status: None,
                        native_python_diagnostics: Box::new([]),
                        typescript_diagnostic: None,
                    });
                    continue;
                }
                Err(terminal)
                    if target.profile.language()
                        == backend_semantic::vocabulary::Language::TypeScript =>
                {
                    // Only failures proven to belong to this source's lowering
                    // or fragment validation may leave its siblings publishable.
                    // Source/project/session authority failures remain terminal.
                    if typescript_source_gap_cause(&terminal, source_authority).is_none() {
                        return Err(PackageSemanticError::Compile {
                            path: source.relative_path.into(),
                            terminal: Box::new(terminal),
                        });
                    }
                    let diagnostic = typescript_source_gap_diagnostic(
                        &terminal,
                        source.source.as_bytes(),
                    );
                    degraded.degrade_with_diagnostic(
                        terminal,
                        source.relative_path,
                        source_authority,
                        diagnostic,
                        &mut coverage_gaps,
                    )?;
                    continue;
                }
                Err(terminal) => {
                    if target.profile.language() != backend_semantic::vocabulary::Language::Go {
                        return Err(PackageSemanticError::Compile {
                            path: source.relative_path.into(),
                            terminal: Box::new(terminal),
                        });
                    }
                    degraded.degrade(
                        terminal,
                        source.relative_path,
                        source_authority,
                        &mut coverage_gaps,
                    )?;
                    continue;
                }
            };
            let fragment_length = compiled.fragment.len();
            fragment_bytes =
                checked_package_fragment_bytes(fragment_bytes, fragment_length, target.profile)?;
            let image_bytes = compiled.semantic_image.len();
            semantic_bytes = checked_package_bytes(
                semantic_bytes,
                image_bytes,
                MAX_PACKAGE_SEMANTIC_BYTES,
                "semantic image",
            )?;
            let byte_length =
                u32::try_from(image_bytes).map_err(|_| PackageSemanticError::Capacity {
                    lane: "semantic image",
                })?;
            let offset = semantic_images.len();
            semantic_images
                .try_reserve_exact(image_bytes)
                .map_err(PackageSemanticError::Allocation)?;
            semantic_images.extend_from_slice(&compiled.semantic_image);
            image_plan.push(
                crate::publication::manifest::SemanticImageRegion::from_measurement(
                    offset,
                    byte_length,
                ),
            );
            artifacts.push(StagedPackageArtifact {
                source: compiled.source,
                recipe: compiled.recipe,
                fragment: compiled.fragment,
            });
            if embedding_unavailable.is_none()
                && embedding_runtime.is_some()
                && embedding_execution_identity.is_some()
            {
                embedding_sources.push((source.relative_path, source.source));
            }
        }
        if let Some(refusal) = degraded.into_refusal_when_nothing_compiled(artifacts.len()) {
            return Err(refusal);
        }
        if artifacts.is_empty()
            && let Some(gap) = coverage_gaps.iter().find(|gap| {
                gap.cause == PackageSourceCoverageGapCause::TypeScriptSourceWorkBudgetExhausted
            })
        {
            let source = package
                .compilation_sources()
                .find(|source| source.relative_path == gap.relative_path.as_ref())
                .ok_or(PackageSemanticError::Capacity {
                    lane: "TypeScript work-gap source authority",
                })?;
            let request = ApplicationCompilerRequest {
                profile: target.profile,
                stage: target.stage,
                source: source.source,
            };
            let toolchain = self
                .toolchain_for_package(request, typescript_toolchain)
                .unwrap_or(ToolchainSelection::ExplicitlyUnavailable {
                    tool: NativeTool::TypeScriptCompiler,
                });
            return Err(PackageSemanticError::Compile {
                path: gap.relative_path.clone(),
                terminal: Box::new(package_authority_terminal(
                    package.package_target.target(), request, gap.source, toolchain,
                    PackageAuthorityError::TypeScriptTsz(backend_frontend_typescript::TszAuthorityError::ProjectCheck(
                        backend_frontend_typescript::TszProjectProgramCheckError::ExecutionStopped(
                            backend_frontend_typescript::TszProjectExecutionStop::WorkBudgetExhausted,
                        ),
                    )),
                )),
            });
        }
        if let Some(go) = go_authority.take() {
            // Every image of this package is only publishable while the inputs it
            // was produced from still match their admitted witness.
            go.finish().map_err(|cause| {
                let toolchain = self.toolchain(first_application_request).unwrap_or(
                    ToolchainSelection::ExplicitlyUnavailable {
                        tool: NativeTool::GoCompiler,
                    },
                );
                PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        cause,
                    )),
                }
            })?;
        }
        if embedding_unavailable.is_none()
            && let (Some(runtime), Some(identity)) =
                (embedding_runtime, embedding_execution_identity)
            && !embedding_sources.is_empty()
        {
            if cancelled.load(Ordering::Acquire) {
                return Err(PackageSemanticError::Compile {
                    path: embedding_sources[0].0.into(),
                    terminal: Box::new(CompilerTerminal::PackageCancelled {
                        target: package.package_target.target(),
                        phase: PackageCompilePhase::Lower,
                    }),
                });
            }
            let mut texts = Vec::new();
            if texts.try_reserve_exact(embedding_sources.len()).is_err() {
                let cause: Box<str> = "embedding request allocation failed".into();
                if embedding_requirement == EmbeddingRequirement::Required {
                    return Err(PackageSemanticError::Embedding {
                        path: embedding_sources[0].0.into(),
                        cause,
                    });
                }
                embedding_unavailable = Some(cause);
            } else {
                texts.extend(embedding_sources.iter().map(|(_, source)| *source));
                let inference = match embedding_cache_session {
                    Some(cache_session) => runtime.infer_batch_with_cache_session(
                        EmbeddingPurpose::Document,
                        &texts,
                        cancelled,
                        cache_session,
                    ),
                    None => runtime.infer_batch_with_cancellation_flag(
                        EmbeddingPurpose::Document,
                        &texts,
                        cancelled,
                    ),
                };
                match inference {
                    Ok(coordinates) if coordinates.len() == embedding_sources.len() => {
                        for ((relative_path, _), coordinates) in
                            embedding_sources.iter().zip(&coordinates)
                        {
                            match stage_embedding_artifact(
                                identity,
                                relative_path,
                                coordinates,
                                runtime.memory_budget(),
                            ) {
                                Ok(artifact) => embedding_artifacts.push(artifact),
                                Err(cause)
                                    if embedding_requirement == EmbeddingRequirement::Required =>
                                {
                                    return Err(PackageSemanticError::Embedding {
                                        path: (*relative_path).into(),
                                        cause,
                                    });
                                }
                                Err(cause) => {
                                    embedding_artifacts.clear();
                                    embedding_unavailable = Some(cause);
                                    break;
                                }
                            }
                        }
                    }
                    Ok(coordinates) => {
                        let cause: Box<str> = format!(
                            "embedding batch returned {} coordinates for {} sources",
                            coordinates.len(),
                            embedding_sources.len()
                        )
                        .into();
                        if embedding_requirement == EmbeddingRequirement::Required {
                            return Err(PackageSemanticError::Embedding {
                                path: embedding_sources[0].0.into(),
                                cause,
                            });
                        }
                        embedding_artifacts.clear();
                        embedding_unavailable = Some(cause);
                    }
                    Err(_) if cancelled.load(Ordering::Acquire) => {
                        return Err(PackageSemanticError::Compile {
                            path: embedding_sources[0].0.into(),
                            terminal: Box::new(CompilerTerminal::PackageCancelled {
                                target: package.package_target.target(),
                                phase: PackageCompilePhase::Lower,
                            }),
                        });
                    }
                    Err(error) if embedding_requirement == EmbeddingRequirement::Required => {
                        return Err(PackageSemanticError::Embedding {
                            path: embedding_sources[0].0.into(),
                            cause: error.to_string().into(),
                        });
                    }
                    Err(error) => {
                        embedding_artifacts.clear();
                        embedding_unavailable = Some(error.to_string().into());
                    }
                }
            }
        }
        if !coverage_gaps.is_empty() {
            // A source-frontier claim can bind bytes without claiming that all
            // selected source files had active semantic authority. Keep both
            // opaque roots while removing any completeness capability.
            input = SemanticInputWitness::claimed_state(
                *input.input_root(),
                input.read_manifest_root(),
                Coverage::Partial,
            );
        }
        match (
            typescript_project.as_ref(),
            typescript_closure_witness.as_ref(),
        ) {
            (Some(project), Some(resolver_witness)) => resolver_witness
                .validate_current(project.witness().as_ref())
                .map_err(PackageSemanticError::TypeScriptProjectWitness)?,
            (None, None) => {}
            _ => {
                return Err(PackageSemanticError::Capacity {
                    lane: "TypeScript compiler read closure",
                });
            }
        }
        if native_program_sources
            .as_ref()
            .is_some_and(|sources| sources.is_forest())
        {
            use backend_semantic::ir::{
                ImageProvenance, SemanticCoreReader, SemanticImageIdentity,
            };
            let owners = native_program_sources.as_ref().expect("forest present");
            let toolchain = owners.toolchain();
            let mappings = owners
                .sources()
                .filter_map(|row| row.package_path.map(|path| (path.to_owned(), row.source)))
                .collect::<BTreeMap<_, _>>();
            let mut bindings = Vec::with_capacity(image_plan.len());
            for region in &image_plan {
                let bytes =
                    region
                        .bytes(&semantic_images)
                        .ok_or(PackageSemanticError::Capacity {
                            lane: "TypeScript forest image region",
                        })?;
                let image = SemanticImageView::reopen(bytes)
                    .map_err(PackageSemanticError::NativeProgramImage)?;
                let ImageProvenance::Captured {
                    source,
                    recipe,
                    scope,
                    ..
                } = image.image_facts().provenance
                else {
                    return Err(PackageSemanticError::Capacity {
                        lane: "TypeScript forest image provenance",
                    });
                };
                if recipe.profile.language() != backend_semantic::vocabulary::Language::TypeScript {
                    continue;
                }
                let path = image
                    .atom(scope.path)
                    .and_then(|path| std::str::from_utf8(path).ok())
                    .ok_or(PackageSemanticError::Capacity {
                        lane: "TypeScript forest image path",
                    })?;
                if recipe.toolchain != toolchain || mappings.get(path) != Some(&source) {
                    return Err(PackageSemanticError::Capacity {
                        lane: "TypeScript forest image source owner",
                    });
                }
                bindings.push((
                    path.to_owned(),
                    *SemanticImageIdentity::from_encoded_bytes(bytes).as_ref(),
                ));
            }
            native_program_sources = Some(
                native_program_sources
                    .take()
                    .expect("forest present")
                    .with_image_bindings(bindings)
                    .map_err(PackageSemanticError::NativeProgramSources)?,
            );
        }
        let project_plane_seed = typescript_toolchain.and_then(|toolchain| {
            typescript_project.as_ref().and_then(|_| {
                LocalCompilerPlaneExecutionSeed::for_typescript_project(
                    package.package_target.target(),
                    target.profile,
                    target.stage,
                    toolchain.identity,
                    typescript_project_execution_fingerprint?,
                )
            })
        });
        let typescript_runtime_local_authority = if typescript_project.is_some() {
            execution_identity
                .map(LocalCompilerExecutionIdentity::local_authority_fingerprint)
                .or_else(|| {
                    plane_execution_seed
                        .map(|seed| seed.bind_input(input).local_authority_fingerprint())
                })
        } else {
            None
        };
        let (execution_identity, plane_execution_seed) = if typescript_project.is_some() {
            let project = project_plane_seed.ok_or(PackageSemanticError::Capacity {
                lane: "admitted TypeScript project runtime identity",
            })?;
            authority
                .with_admitted_typescript_project(project)
                .identities()
        } else {
            (execution_identity, plane_execution_seed)
        };
        let mut rust_plane_execution_identity = None;
        if let Some(lease) = rust_workspace_lease.as_ref() {
            if let Some(facts) = lease.workspace().cargo_workspace_facts() {
                rust_plane_execution_identity =
                    rust_request_authority.map(|authority| authority.bind_workspace(facts, input));
            } else if deferred_rust_authority {
                let toolchain = self.toolchain(first_application_request).unwrap_or(
                    ToolchainSelection::ExplicitlyUnavailable {
                        tool: NativeTool::Rustc,
                    },
                );
                return Err(PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(package_authority_terminal(
                        package.package_target.target(),
                        first_application_request,
                        first_authority,
                        toolchain,
                        PackageAuthorityError::RustWorkspaceWitnessUnavailable {
                            profile: target.profile,
                        },
                    )),
                });
            }
        }
        let source_coverage = if typescript_project.is_some() {
            use backend_semantic::ir::{
                SourceCoverageManifest, SourceCoverageObservation, SourceCoverageRow,
                SourceIdentity,
            };
            let mut remaining = package
                .compilation_sources()
                .map(|source| {
                    SourceIdentity::from_bytes(source.source.as_bytes())
                        .map(|identity| (source.relative_path, identity))
                        .ok_or(PackageSemanticError::Capacity {
                            lane: "coverage source extent",
                        })
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            let source_count = remaining.len();
            let mut gaps = coverage_gaps.iter().collect::<Vec<_>>();
            gaps.sort_by(|a, b| a.relative_path().cmp(b.relative_path()));
            for gap in &gaps {
                let identity = remaining.remove(gap.relative_path()).ok_or(
                    PackageSemanticError::Capacity {
                        lane: "coverage gap source partition",
                    },
                )?;
                if identity.identity != gap.source().identity
                    || identity.byte_len != gap.source().byte_len
                    || !gap.cause().is_typescript()
                {
                    return Err(PackageSemanticError::Capacity {
                        lane: "coverage gap source identity or cause",
                    });
                }
            }
            let source_key = |source: SourceIdentity| (*source.identity.as_ref(), source.byte_len);
            let mut expected = remaining.into_values().map(source_key).collect::<Vec<_>>();
            let mut observed = artifacts
                .iter()
                .map(|artifact| source_key(artifact.source))
                .collect::<Vec<_>>();
            expected.sort();
            observed.sort();
            if expected != observed {
                return Err(PackageSemanticError::Capacity {
                    lane: "coverage artifact source partition",
                });
            }
            let program =
                native_program_sources
                    .as_ref()
                    .ok_or(PackageSemanticError::Capacity {
                        lane: "coverage native program identity",
                    })?;
            let configs = program.nodes().map_or(&[][..], |(_, nodes)| nodes);
            let source_count =
                u32::try_from(source_count).map_err(|_| PackageSemanticError::Capacity {
                    lane: "coverage source count",
                })?;
            let completed =
                u32::try_from(artifacts.len()).map_err(|_| PackageSemanticError::Capacity {
                    lane: "coverage artifact count",
                })?;
            // Forecast the entire explanation before copying any path. A volume
            // refusal is an authenticated availability fact, never a new semantic failure.
            let capacity = SourceCoverageObservation::forecast(
                configs.iter().map(|node| node.config_path.as_str()),
                gaps.iter().map(|gap| gap.relative_path()),
            )
            .map_err(|_| PackageSemanticError::Capacity {
                lane: "coverage path consistency",
            })?;
            let recording = if let Some(reason) = capacity {
                SourceCoverageObservation::not_recorded_capacity(
                    *input.input_root(),
                    <[u8; 2]>::from(target.profile),
                    program.membership_identity(),
                    source_count,
                    completed,
                    reason,
                )
            } else {
                let mut rows = configs
                    .iter()
                    .map(|node| SourceCoverageRow::Configuration {
                        path: node.config_path.clone(),
                        source: *node.config.as_ref(),
                        environment: node.environment,
                    })
                    .collect::<Vec<_>>();
                rows.extend(gaps.into_iter().map(|gap| SourceCoverageRow::Gap {
                    path: gap.relative_path().to_owned(),
                    source: *gap.source().identity.as_ref(),
                    byte_len: gap.source().byte_len,
                    cause: gap.cause(),
                    diagnostic: gap.typescript_diagnostic().cloned(),
                }));
                SourceCoverageManifest::new(
                    *input.input_root(),
                    <[u8; 2]>::from(target.profile),
                    program.membership_identity(),
                    source_count,
                    completed,
                    rows,
                )
                .and_then(SourceCoverageObservation::from_catalog)
            }
            .map_err(|_| PackageSemanticError::Capacity {
                lane: "coverage recording admission",
            })?;
            Some(recording)
        } else {
            None
        };

        let staged = StagedPackageCompilation {
            compilation_attempt_id,
            artifacts,
            coverage_gaps: coverage_gaps.into_boxed_slice(),
            image_plan: image_plan.into_boxed_slice(),
            semantic_images: semantic_images.into_boxed_slice(),
            package_identity: *package.request.as_ref().identity.as_ref(),
            target_identity: package.package_target.target(),
            profile: target.profile,
            stage: target.stage,
            input,
            execution_identity,
            plane_execution_identity: rust_plane_execution_identity
                .or_else(|| plane_execution_seed.map(|seed| seed.bind_input(input))),
            typescript_runtime_local_authority,
            typescript_program_environments,
            cargo_workspace_facts: rust_workspace_lease.as_ref().and_then(|lease| {
                lease
                    .workspace()
                    .cargo_workspace_facts()
                    .map(std::sync::Arc::clone)
            }),
            rust_target_source_closure,
            typescript_witness: typescript_project
                .as_ref()
                .map(|project| std::sync::Arc::clone(project.witness())),
            typescript_closure_witness: typescript_closure_witness.map(std::sync::Arc::new),
            native_program_sources,
            source_coverage,
            python_witness: python_project
                .as_ref()
                .map(|project| StagedPythonProjectWitness {
                    witness: std::sync::Arc::clone(project.witness()),
                    deadline: control.deadline,
                }),
            embeddings: embedding_identity.map(|identity| StagedEmbeddingOutput {
                identity,
                artifacts: if embedding_unavailable.is_some() {
                    Box::new([])
                } else {
                    embedding_artifacts.into_boxed_slice()
                },
                unavailable_reason: embedding_unavailable,
            }),
            embedding_provisioning_failure: package.embedding_provisioning_failure,
        };
        drop(rust_workspace_authority);
        if let Some(lease) = rust_workspace_lease {
            lease.commit();
        }
        Ok(staged)
    }

    fn stage_prepared(
        &self,
        request: ApplicationCompilerRequest<'_>,
        _source: SourceAuthority,
        declaration_scope: DeclarationScope<'_>,
        toolchain: ToolchainSelection<'_>,
        authority: crate::driver::SemanticAuthorityInput<'_>,
        control: CompileControl<'_>,
        diagnostic_output: &mut [u8; backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES],
        fragment_output: &mut crate::application::config::FragmentOutput,
        progress: &mut impl FnMut(PackageCompilePhase),
    ) -> Result<StagedCompilerArtifact, CompilerTerminal> {
        progress(PackageCompilePhase::Lower);
        let compiled = compile_fused_semantic(
            CompileRequest {
                profile: request.profile,
                stage: request.stage,
                source: request.source.as_bytes(),
                declaration_scope,
                toolchain,
                authority,
                control,
            },
            CompileScratch {
                diagnostic_output,
                native_work: &self.native_work_directory,
            },
            CompileOutput {
                fragment_output: fragment_output.as_mut(),
            },
        )
        .map_err(compile_terminal)?;
        let mut fragment = Vec::new();
        fragment
            .try_reserve_exact(compiled.artifact.fragment.as_ref().len())
            .map_err(|_| {
                staged_output_terminal(
                    compiled.artifact.source,
                    compiled.artifact.recipe,
                    PublicationPhase::Fragment,
                )
            })?;
        fragment.extend_from_slice(compiled.artifact.fragment.as_ref());

        let prepared =
            backend_semantic::ir::PreparedFullSemanticImage::new(&compiled.ir).map_err(|_| {
                staged_output_terminal(
                    compiled.artifact.source,
                    compiled.artifact.recipe,
                    PublicationPhase::SemanticImage,
                )
            })?;
        if prepared.byte_len() > MAX_PACKAGE_SEMANTIC_BYTES {
            return Err(staged_output_terminal(
                compiled.artifact.source,
                compiled.artifact.recipe,
                PublicationPhase::SemanticImage,
            ));
        }
        let mut semantic_image = Vec::new();
        semantic_image
            .try_reserve_exact(prepared.byte_len())
            .map_err(|_| {
                staged_output_terminal(
                    compiled.artifact.source,
                    compiled.artifact.recipe,
                    PublicationPhase::SemanticImage,
                )
            })?;
        semantic_image.resize(prepared.byte_len(), 0);
        prepared.encode_into(&mut semantic_image).map_err(|_| {
            staged_output_terminal(
                compiled.artifact.source,
                compiled.artifact.recipe,
                PublicationPhase::SemanticImage,
            )
        })?;
        Ok(StagedCompilerArtifact {
            source: compiled.artifact.source,
            recipe: compiled.artifact.recipe,
            fragment: fragment.into_boxed_slice(),
            semantic_image: semantic_image.into_boxed_slice(),
        })
    }

    fn compile_control<'a>(
        &self,
        cancelled: &'a AtomicBool,
    ) -> Result<CompileControl<'a>, crate::application::LocalCompilerTimeout> {
        let timeout = LocalCompilerControl {
            timeout: self.config.control.timeout,
            cancelled,
        };
        Ok(CompileControl {
            deadline: timeout.deadline()?,
            cancelled,
        })
    }

    fn toolchain(
        &self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<ToolchainSelection<'path>, ToolchainRouteError> {
        let route = FullRegistry
            .route(request.profile.language(), request.stage)
            .map_err(ToolchainRouteError::UnsupportedStage)?;
        select_toolchain(self.config.toolchains, route)
    }

    fn toolchain_for_package<'toolchain>(
        &'toolchain self,
        request: ApplicationCompilerRequest<'_>,
        typescript_project: Option<ResolvedToolchain<'toolchain>>,
    ) -> Result<ToolchainSelection<'toolchain>, ToolchainRouteError> {
        if request.profile.language() == backend_semantic::vocabulary::Language::TypeScript
            && let Some(toolchain) = typescript_project
        {
            return Ok(ToolchainSelection::ResolvedNative(toolchain));
        }
        self.toolchain(request)
    }
}

impl<'path, 'scratch, 'cancel> LocalCompiler<'path, 'scratch, 'cancel> {
    pub(crate) fn resolve_rust_cargo_target_name(
        &mut self,
        package: PackageSourceSet<'_>,
        target_name: &str,
        cancelled: &AtomicBool,
    ) -> Result<(CompilerPackageTargetV2, RustCargoTargetSourceClosureV1), PackageSemanticError>
    {
        self.execution()
            .resolve_rust_cargo_target_name(&package, target_name, cancelled)
    }

    /// Creates the only durable publication owner used by this configured local compiler.
    ///
    /// The caller supplies a validated explicit toolchain table, artifact directory, journal
    /// directory, native work directory, cancellation authority, and all reusable scratch. No
    /// process-global discovery or heap-backed per-request arena is introduced here.
    ///
    /// # Errors
    ///
    /// Returns [`LocalCompilerOpenError`] when one explicit local path is relative or the durable
    /// publisher cannot establish its journal owner.
    pub fn create(
        config: LocalCompilerConfig<'path, 'cancel>,
        limits: PublicationLimits,
        scratch: &'scratch mut LocalCompilerScratch,
    ) -> Result<Self, LocalCompilerOpenError> {
        Self::create_with_package_roots(config, LocalPackageRootSet::EMPTY, limits, scratch)
    }

    /// Creates a durable compiler with explicit local package-store roots.
    ///
    /// The root table has already proven absolute, unique, ordered ecosystem ownership. No
    /// package command consults process environment or walks outside these roots.
    ///
    /// # Errors
    ///
    /// Returns the same exact configuration and durable-publisher terminal as [`Self::create`].
    pub fn create_with_package_roots(
        config: LocalCompilerConfig<'path, 'cancel>,
        package_roots: LocalPackageRootSet<'path>,
        limits: PublicationLimits,
        scratch: &'scratch mut LocalCompilerScratch,
    ) -> Result<Self, LocalCompilerOpenError> {
        Self::create_with_package_authority(
            config,
            package_roots,
            PackageAuthorityConfiguration::UNAVAILABLE,
            limits,
            scratch,
        )
    }

    /// Creates a durable compiler with explicit package roots and sidecar authority producers.
    ///
    /// The configuration is borrowed for the compiler owner's complete lifetime. Package work
    /// therefore cannot outlive a checker, oracle, JDK, or rust-analyzer configuration it uses.
    /// C and C++ continue to use the driver's direct libclang authority.
    ///
    /// # Errors
    ///
    /// Returns the same exact path or publisher terminal as [`Self::create`].
    pub fn create_with_package_authority(
        config: LocalCompilerConfig<'path, 'cancel>,
        package_roots: LocalPackageRootSet<'path>,
        package_authority: PackageAuthorityConfiguration<'path>,
        limits: PublicationLimits,
        scratch: &'scratch mut LocalCompilerScratch,
    ) -> Result<Self, LocalCompilerOpenError> {
        for (path, role) in [
            (config.artifact_directory, LocalCompilerPath::Artifacts),
            (config.journal_directory, LocalCompilerPath::Journal),
            (config.native_work_directory, LocalCompilerPath::NativeWork),
        ] {
            if !path.is_absolute() {
                return Err(LocalCompilerOpenError::RelativePath { path: role });
            }
        }
        let journal = PublicationPaths::in_directory(config.journal_directory);
        let publisher = DurablePublisher::open_or_create(&journal, limits)
            .map_err(LocalCompilerOpenError::Publisher)?;
        Ok(Self {
            config,
            package_roots,
            package_authority,
            publisher,
            scratch,
            retained_semantic_image: None,
        })
    }

    pub(crate) fn execution(&self) -> LocalCompilerExecution<'path, 'cancel> {
        LocalCompilerExecution {
            config: self.config,
            package_roots: self.package_roots,
            package_authority: self.package_authority,
            native_work_directory: self
                .config
                .native_work_directory
                .to_path_buf()
                .into_boxed_path(),
        }
    }

    /// Publishes a completed lane result through this compiler's one durable authority.
    pub(crate) fn publish_staged_package(
        &mut self,
        staged: StagedPackageCompilation,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(PackageCompilePhase),
    ) -> Result<PublishedSemanticPackage, PackageSemanticError> {
        if let Some(witness) = staged.python_witness.as_ref() {
            witness
                .witness
                .validate_current(
                    backend_frontend_python::legacy::checker::PythonProjectControl {
                        cancelled,
                        deadline: witness.deadline,
                    },
                )
                .map_err(PackageSemanticError::PythonProjectWitness)?;
        }
        match (
            staged.typescript_witness.as_ref(),
            staged.typescript_closure_witness.as_ref(),
        ) {
            (Some(project), Some(resolver)) => resolver
                .validate_current(project.as_ref())
                .map_err(PackageSemanticError::TypeScriptProjectWitness)?,
            (None, None) => {}
            _ => {
                return Err(PackageSemanticError::Capacity {
                    lane: "TypeScript compiler read closure",
                });
            }
        }
        let count = staged.artifacts.len();
        if count == 0 {
            return Err(PackageSemanticError::Capacity { lane: "manifest" });
        }
        let mut fragment_bytes = 0_usize;
        for artifact in &staged.artifacts {
            fragment_bytes = checked_package_fragment_bytes(
                fragment_bytes,
                artifact.fragment.len(),
                staged.profile,
            )?;
        }
        let semantic_bytes = staged.semantic_images.len();
        if semantic_bytes > MAX_PACKAGE_SEMANTIC_BYTES {
            return Err(PackageSemanticError::Capacity {
                lane: "semantic image",
            });
        }

        let manifest_bytes = crate::publication::manifest::COMPILATION_MANIFEST_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(
                        crate::publication::manifest::COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES,
                    )
                    .ok_or(PackageSemanticError::Capacity { lane: "manifest" })?,
            )
            .ok_or(PackageSemanticError::Capacity { lane: "manifest" })?;
        self.scratch
            .prepare_publication(count, manifest_bytes, fragment_bytes, semantic_bytes)
            .map_err(PackageSemanticError::Scratch)?;

        let mut compiled = Vec::new();
        compiled
            .try_reserve_exact(count)
            .map_err(PackageSemanticError::Allocation)?;
        for (ordinal, artifact) in staged.artifacts.iter().enumerate() {
            let fragment = backend_semantic::ir::FragmentView::validate(&artifact.fragment)
                .map_err(|source| PackageSemanticError::Fragment { ordinal, source })?;
            compiled.push(CompiledFragment {
                source: artifact.source,
                recipe: artifact.recipe,
                fragment,
            });
        }

        progress(PackageCompilePhase::Publish);
        let publication = publish_semantic_bytes(
            &self.publisher,
            self.config.artifact_directory,
            &compiled,
            &staged.image_plan,
            &staged.semantic_images,
            PublishControl::Observe(cancelled),
            SemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                ordinals: &mut self.scratch.ordinals,
                semantic_image_plan: &mut self.scratch.semantic_image_plan,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
                binding_output: &mut self.scratch.binding_output,
            },
        )
        .map_err(PackageSemanticError::Publish)?;
        drop(compiled);

        progress(PackageCompilePhase::Reopen);
        let opened = open_published_semantic(
            &self.publisher,
            self.config.artifact_directory,
            OpenSemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                fragment_output: &mut self.scratch.reopened_fragment_output,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
            },
        )
        .map_err(PackageSemanticError::Reopen)?
        .ok_or(PackageSemanticError::MissingPublication)?;
        let mut images = Vec::new();
        images
            .try_reserve_exact(count)
            .map_err(PackageSemanticError::Allocation)?;
        for (ordinal, artifact) in opened.artifacts().enumerate() {
            let artifact =
                artifact.map_err(|source| PackageSemanticError::Artifact { ordinal, source })?;
            let facts = artifact.fragment.facts.semantic_image.ok_or(
                PackageSemanticError::ReopenedCardinality {
                    expected: count,
                    observed: ordinal,
                },
            )?;
            images.push(
                SemanticImageSnapshot::try_from_reopened(
                    SemanticImageAuthority {
                        identity: facts.identity,
                        byte_len: facts.byte_length,
                    },
                    artifact.semantic_image.as_ref(),
                )
                .map_err(|cause| PackageSemanticError::Snapshot { ordinal, cause })?,
            );
        }
        if images.len() != count {
            return Err(PackageSemanticError::ReopenedCardinality {
                expected: count,
                observed: images.len(),
            });
        }
        Ok(PublishedSemanticPackage {
            publication,
            images: images.into_boxed_slice(),
            coverage_gaps: staged.coverage_gaps,
        })
    }

    pub(crate) fn prepare_staged_package(
        &mut self,
        staged: StagedPackageCompilation,
        cancelled: &AtomicBool,
    ) -> Result<StagedSemanticPackage, PackageSemanticError> {
        if let Some(witness) = staged.python_witness.as_ref() {
            witness
                .witness
                .validate_current(
                    backend_frontend_python::legacy::checker::PythonProjectControl {
                        cancelled,
                        deadline: witness.deadline,
                    },
                )
                .map_err(PackageSemanticError::PythonProjectWitness)?;
        }
        match (
            staged.typescript_witness.as_ref(),
            staged.typescript_closure_witness.as_ref(),
        ) {
            (Some(project), Some(resolver)) => resolver
                .validate_current(project.as_ref())
                .map_err(PackageSemanticError::TypeScriptProjectWitness)?,
            (None, None) => {}
            _ => {
                return Err(PackageSemanticError::Capacity {
                    lane: "TypeScript compiler read closure",
                });
            }
        }
        let package_identity = staged.package_identity;
        let target_identity = staged.target_identity;
        let profile = staged.profile;
        let stage = staged.stage;
        let input = staged.input;
        let execution_identity = staged.execution_identity;
        let plane_execution_identity = staged.plane_execution_identity;
        let count = staged.artifacts.len();
        if count == 0 {
            return Err(PackageSemanticError::Capacity { lane: "manifest" });
        }
        let mut fragment_bytes = 0_usize;
        for artifact in &staged.artifacts {
            fragment_bytes =
                checked_package_fragment_bytes(fragment_bytes, artifact.fragment.len(), profile)?;
        }
        let semantic_bytes = staged.semantic_images.len();
        if semantic_bytes > MAX_PACKAGE_SEMANTIC_BYTES {
            return Err(PackageSemanticError::Capacity {
                lane: "semantic image",
            });
        }
        let manifest_bytes = crate::publication::manifest::COMPILATION_MANIFEST_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(
                        crate::publication::manifest::COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES,
                    )
                    .ok_or(PackageSemanticError::Capacity { lane: "manifest" })?,
            )
            .ok_or(PackageSemanticError::Capacity { lane: "manifest" })?;
        self.scratch
            .prepare_publication(count, manifest_bytes, fragment_bytes, semantic_bytes)
            .map_err(PackageSemanticError::Scratch)?;

        let mut compiled = Vec::new();
        compiled
            .try_reserve_exact(count)
            .map_err(PackageSemanticError::Allocation)?;
        for (ordinal, artifact) in staged.artifacts.iter().enumerate() {
            let fragment = backend_semantic::ir::FragmentView::validate(&artifact.fragment)
                .map_err(|source| PackageSemanticError::Fragment { ordinal, source })?;
            compiled.push(CompiledFragment {
                source: artifact.source,
                recipe: artifact.recipe,
                fragment,
            });
        }

        let prepared = prepare_semantic_bytes(
            &compiled,
            &staged.image_plan,
            &staged.semantic_images,
            PublishControl::Observe(cancelled),
            SemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                ordinals: &mut self.scratch.ordinals,
                semantic_image_plan: &mut self.scratch.semantic_image_plan,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
                binding_output: &mut self.scratch.binding_output,
            },
        )
        .map_err(|error| {
            // Keep logging on the same bounded typed diagnostic path as the public refusal.
            let diagnostic = bounded_error_chain(&error);
            eprintln!(
                "nudox: semantic output preparation failed: {}",
                diagnostic.text
            );
            PackageSemanticError::StagedOutput(error)
        })?;
        drop(compiled);

        let expected_claim_capacity = prepared
            .canonical_ordinals
            .len()
            .checked_mul(2)
            .and_then(|pairs| pairs.checked_add(1))
            .ok_or(PackageSemanticError::Capacity {
                lane: "generation claims",
            })?;
        let mut expected_claim_edges = Vec::new();
        expected_claim_edges
            .try_reserve_exact(expected_claim_capacity)
            .map_err(PackageSemanticError::Allocation)?;
        expected_claim_edges.push((0_u64, None));
        let mut artifact_claim_keys = Vec::new();
        artifact_claim_keys
            .try_reserve_exact(prepared.canonical_ordinals.len())
            .map_err(PackageSemanticError::Allocation)?;
        let mut next_claim_key = 1_u64;
        for _ in prepared.canonical_ordinals.iter() {
            let fragment_key = next_claim_key;
            next_claim_key =
                next_claim_key
                    .checked_add(1)
                    .ok_or(PackageSemanticError::Capacity {
                        lane: "generation claims",
                    })?;
            let image_key = next_claim_key;
            next_claim_key =
                next_claim_key
                    .checked_add(1)
                    .ok_or(PackageSemanticError::Capacity {
                        lane: "generation claims",
                    })?;
            expected_claim_edges.push((fragment_key, Some(0)));
            expected_claim_edges.push((image_key, Some(0)));
            artifact_claim_keys.push((fragment_key, image_key));
        }
        let mut observed_claim_edges = Vec::new();
        observed_claim_edges
            .try_reserve_exact(prepared.object_claims.len())
            .map_err(PackageSemanticError::Allocation)?;
        let mut claims_by_key = BTreeMap::new();
        for claim in prepared.object_claims.iter().copied() {
            let key = *claim.key();
            let parent = claim.parent().map(|parent| *parent);
            observed_claim_edges.push((key, parent));
            if claims_by_key.insert(key, claim).is_some() {
                return Err(PackageSemanticError::Capacity {
                    lane: "generation claims",
                });
            }
        }
        expected_claim_edges.sort_unstable();
        observed_claim_edges.sort_unstable();
        if observed_claim_edges != expected_claim_edges
            || prepared.canonical_ordinals.is_empty()
            || prepared
                .canonical_ordinals
                .iter()
                .any(|ordinal| *ordinal >= count)
        {
            return Err(PackageSemanticError::Capacity {
                lane: "generation claims",
            });
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(PackageSemanticError::StagedOutput(
                crate::publication::PublishSemanticError::CancelledBeforeStorage,
            ));
        }

        let mut artifacts = Vec::new();
        artifacts
            .try_reserve_exact(prepared.canonical_ordinals.len())
            .map_err(PackageSemanticError::Allocation)?;
        for (canonical_ordinal, input_ordinal) in
            prepared.canonical_ordinals.iter().copied().enumerate()
        {
            let Some(artifact) = staged.artifacts.get(input_ordinal) else {
                return Err(PackageSemanticError::Capacity {
                    lane: "generation claims",
                });
            };
            let Some(region) = staged.image_plan.get(input_ordinal).copied() else {
                return Err(PackageSemanticError::Capacity {
                    lane: "generation claims",
                });
            };
            let Some(semantic_image) = region.facts(&staged.semantic_images) else {
                return Err(PackageSemanticError::Capacity {
                    lane: "generation claims",
                });
            };
            let Some((fragment_key, image_key)) = artifact_claim_keys.get(canonical_ordinal) else {
                return Err(PackageSemanticError::Capacity {
                    lane: "generation claims",
                });
            };
            let Some(fragment_claim) = claims_by_key.get(fragment_key).copied() else {
                return Err(PackageSemanticError::Capacity {
                    lane: "generation claims",
                });
            };
            let Some(semantic_claim) = claims_by_key.get(image_key).copied() else {
                return Err(PackageSemanticError::Capacity {
                    lane: "generation claims",
                });
            };
            artifacts.push(StagedSemanticArtifact {
                source: artifact.source,
                recipe: artifact.recipe,
                semantic_image,
                fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                    artifact.fragment.as_ref(),
                ),
                fragment_claim,
                semantic_claim,
            });
        }

        Ok(StagedSemanticPackage {
            staged,
            prepared,
            artifacts: artifacts.into_boxed_slice(),
            package_identity,
            target_identity,
            profile,
            stage,
            input,
            execution_identity,
            plane_execution_identity,
            _budget_lease: None,
        })
    }

    /// Publishes one completed compile lane result and captures its exact reopened image.
    pub(crate) fn publish_staged_single(
        &mut self,
        staged: StagedCompilerArtifact,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(PackageCompilePhase),
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        let artifact = staged;
        self.retained_semantic_image = None;
        let source = source_authority(artifact.source);
        let recipe = artifact.recipe;
        let fragment = ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            artifact.fragment.as_ref(),
        );
        let fragment_view = backend_semantic::ir::FragmentView::validate(&artifact.fragment)
            .map_err(|cause| CompilerTerminal::Compile {
                attempted: CompilerAttempt {
                    source,
                    recipe: recipe.identity,
                },
                cause: CompilerCause::FragmentFailure(Box::new(CompilerFragmentFailure::validate(
                    cause,
                ))),
            })?;
        let compiled = CompiledFragment {
            source: artifact.source,
            recipe,
            fragment: fragment_view,
        };
        let semantic_length = artifact.semantic_image.len();
        self.scratch
            .prepare_publication(
                1,
                crate::publication::manifest::COMPILATION_MANIFEST_HEADER_BYTES
                    + crate::publication::manifest::COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES,
                artifact.fragment.len(),
                semantic_length,
            )
            .map_err(|_| {
                staged_output_terminal(artifact.source, recipe, PublicationPhase::SemanticImage)
            })?;
        let image_length = u32::try_from(semantic_length).map_err(|_| {
            staged_output_terminal(artifact.source, recipe, PublicationPhase::SemanticImage)
        })?;
        let image_plan = [
            crate::publication::manifest::SemanticImageRegion::from_measurement(0, image_length),
        ];
        progress(PackageCompilePhase::Publish);
        let publication = publish_semantic_bytes(
            &self.publisher,
            self.config.artifact_directory,
            core::slice::from_ref(&compiled),
            &image_plan,
            &artifact.semantic_image,
            PublishControl::Observe(cancelled),
            SemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                ordinals: &mut self.scratch.ordinals,
                semantic_image_plan: &mut self.scratch.semantic_image_plan,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
                binding_output: &mut self.scratch.binding_output,
            },
        )
        .map_err(|error| {
            crate::application::terminal::semantic_publication_terminal(source, recipe, error)
        })?;
        drop(compiled);
        progress(PackageCompilePhase::Reopen);
        let opened = open_published_semantic(
            &self.publisher,
            self.config.artifact_directory,
            OpenSemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                fragment_output: self.scratch.fragment_output.as_mut(),
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
            },
        )
        .map_err(|error| {
            crate::application::terminal::semantic_reopen_terminal(source, recipe, error)
        })?
        .ok_or_else(|| crate::application::terminal::semantic_reopen_absent(source, recipe))?;
        let mut artifacts = opened.artifacts();
        let semantic = artifacts
            .next()
            .ok_or_else(|| crate::application::terminal::semantic_reopen_absent(source, recipe))?
            .map_err(|error| {
                crate::application::terminal::semantic_artifact_terminal(source, recipe, error)
            })?;
        let semantic_facts =
            semantic.fragment.facts.semantic_image.ok_or_else(|| {
                crate::application::terminal::semantic_reopen_absent(source, recipe)
            })?;
        if artifacts.next().is_some() {
            return Err(crate::application::terminal::semantic_reopen_cardinality(
                source, recipe,
            ));
        }
        let generated = generated(source, recipe, fragment, semantic_facts, &publication);
        self.retained_semantic_image = Some(generated.semantic_image);
        Ok(generated)
    }

    /// Copies the one currently retained, already-reopened semantic image into an owned snapshot.
    ///
    /// # Errors
    ///
    /// Returns a typed supersession, authority mismatch, or exact allocation failure. A failed or
    /// newer compile never exposes stale bytes under the requested identity.
    pub fn semantic_image_snapshot(
        &self,
        requested: SemanticImageAuthority,
    ) -> Result<SemanticImageSnapshot, SemanticImageAccessError> {
        if self.retained_semantic_image != Some(requested) {
            return Err(SemanticImageAccessError::Superseded {
                requested,
                retained: self.retained_semantic_image,
            });
        }
        SemanticImageSnapshot::try_from_reopened(requested, &self.scratch.semantic_image_output)
    }

    /// Stops durable publication admission and joins its earned single owner.
    ///
    /// # Errors
    ///
    /// Returns [`ShutdownError`] when the durable publisher cannot finish its owned shutdown.
    pub fn shutdown(self) -> Result<(), ShutdownError> {
        self.publisher.shutdown()
    }

    /// Compiles the active semantic members of one admitted package frontier and
    /// publishes their compact fragments and semantic images as one generation.
    ///
    /// Every captured source is accounted for by an artifact or an explicit
    /// coverage gap. Out-of-scope sources retain their exact input provenance
    /// and make package coverage Partial. Returned images are copied only from
    /// the owner-reopened immutable closure.
    ///
    /// # Errors
    /// Returns the first exact authority, compilation, capacity, publication,
    /// reopen, or image-admission failure. A frontier with no active semantic
    /// member cannot produce a semantic generation.
    pub fn compile_package_sources<Progress>(
        &mut self,
        package: PackageSourceSet<'_>,
        progress: &mut Progress,
    ) -> Result<PublishedSemanticPackage, PackageSemanticError>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        let execution = self.execution();
        let cancelled = self.config.control.cancelled;
        let staged = execution.stage_package_sources(
            package,
            PackageExecutionAuthority::Fixed {
                execution: None,
                plane: None,
            },
            None,
            None,
            EmbeddingRequirement::Optional,
            self.scratch,
            cancelled,
            progress,
        )?;
        self.publish_staged_package(staged, cancelled, progress)
    }

    #[cfg(test)]
    pub(super) fn typescript_program_work_for_test(
        &self,
    ) -> super::typescript_program_lane::TypeScriptProgramWork {
        self.scratch.typescript_program_lane.work()
    }

    /// Compiles an exact package source frontier into canonical output bytes without selecting a
    /// local journal head.
    ///
    /// The caller supplies the package root used by language authorities. A worker may mount a
    /// complete immutable workspace closure there and pass the matching ordered source frontier;
    /// this operation deliberately makes no precise-read-set or completeness claim.
    ///
    /// # Errors
    ///
    /// Returns the first exact authority, compilation, cancellation, capacity, or canonical
    /// output preparation failure. The result can be admitted by an artifact sink independently
    /// of this compiler's local publisher.
    pub fn compile_package_sources_staged<Progress>(
        &mut self,
        package: PackageSourceSet<'_>,
        progress: &mut Progress,
    ) -> Result<StagedSemanticPackage, PackageSemanticError>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        let execution = self.execution();
        let cancelled = self.config.control.cancelled;
        let staged = execution.stage_package_sources(
            package,
            PackageExecutionAuthority::Fixed {
                execution: None,
                plane: None,
            },
            None,
            None,
            EmbeddingRequirement::Optional,
            self.scratch,
            cancelled,
            progress,
        )?;
        progress(PackageCompilePhase::Publish);
        self.prepare_staged_package(staged, cancelled)
    }

    /// Reopens one exact immutable semantic claim independently of the local journal head.
    ///
    /// # Errors
    ///
    /// Returns exact manifest, binding, artifact, closure, allocation, or image-validation facts.
    pub fn activate_semantic_generation(
        &mut self,
        manifest: crate::publication::manifest::CompilationManifestFacts,
        binding: crate::publication::binding::CompilationBindingFacts,
    ) -> Result<ActivatedSemanticPackage, PackageSemanticError> {
        let artifact_count = usize::try_from(manifest.fragment_count)
            .map_err(|_| PackageSemanticError::Capacity { lane: "manifest" })?;
        let manifest_bytes = usize::try_from(manifest.byte_length)
            .map_err(|_| PackageSemanticError::Capacity { lane: "manifest" })?;
        self.scratch
            .prepare_publication(artifact_count, manifest_bytes, 0, 0)
            .map_err(PackageSemanticError::Scratch)?;
        let requirements = semantic_generation_requirements(
            manifest,
            binding,
            self.config.artifact_directory,
            &mut self.scratch.manifest_output,
            &mut self.scratch.manifest_facts,
        )
        .map_err(PackageSemanticError::Reopen)?;
        // The immutable manifest supplies every exact recipe profile. A Go entry
        // cannot widen the aggregate if any retained entry belongs to another profile.
        checked_package_bytes(
            0,
            requirements.semantic_image_bytes,
            MAX_PACKAGE_SEMANTIC_BYTES,
            "semantic image",
        )?;
        checked_manifest_fragment_bytes(
            self.scratch
                .manifest_facts
                .iter()
                .take(artifact_count)
                .flatten()
                .map(|fact| (fact.recipe.profile, fact.fragment_length as usize)),
        )?;
        self.scratch
            .prepare_publication(
                artifact_count,
                manifest_bytes,
                requirements.fragment_bytes,
                requirements.semantic_image_bytes,
            )
            .map_err(PackageSemanticError::Scratch)?;
        let opened = open_semantic_generation(
            manifest,
            binding,
            self.config.artifact_directory,
            OpenSemanticPublicationScratch {
                manifest_output: &mut self.scratch.manifest_output,
                manifest_facts: &mut self.scratch.manifest_facts,
                fragment_output: &mut self.scratch.reopened_fragment_output,
                semantic_image_output: &mut self.scratch.semantic_image_output,
                locality_output: &mut self.scratch.locality_output,
            },
        )
        .map_err(PackageSemanticError::Reopen)?;
        let mut images = Vec::new();
        images
            .try_reserve_exact(artifact_count)
            .map_err(PackageSemanticError::Allocation)?;
        for (ordinal, artifact) in opened.artifacts().enumerate() {
            let artifact =
                artifact.map_err(|source| PackageSemanticError::Artifact { ordinal, source })?;
            let facts = artifact.fragment.facts.semantic_image.ok_or(
                PackageSemanticError::ReopenedCardinality {
                    expected: artifact_count,
                    observed: ordinal,
                },
            )?;
            images.push(
                SemanticImageSnapshot::try_from_reopened(
                    SemanticImageAuthority {
                        identity: facts.identity,
                        byte_len: facts.byte_length,
                    },
                    artifact.semantic_image.as_ref(),
                )
                .map_err(|cause| PackageSemanticError::Snapshot { ordinal, cause })?,
            );
        }
        if images.len() != artifact_count {
            return Err(PackageSemanticError::ReopenedCardinality {
                expected: artifact_count,
                observed: images.len(),
            });
        }
        Ok(ActivatedSemanticPackage {
            manifest,
            binding,
            images: images.into_boxed_slice(),
        })
    }

    fn compile_and_publish(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        let execution = self.execution();
        let cancelled = self.config.control.cancelled;
        let mut ignored = |_| {};
        let staged = execution.stage_generate(request, self.scratch, cancelled, &mut ignored)?;
        self.publish_staged_single(staged, cancelled, &mut ignored)
    }
}

fn select_toolchain(
    toolchains: crate::application::LocalToolchainSet<'_>,
    route: AdapterRoute,
) -> Result<ToolchainSelection<'_>, ToolchainRouteError> {
    match route {
        AdapterRoute::Native { tool } => toolchains
            .select(tool)
            .ok_or(ToolchainRouteError::Missing { selected: tool }),
        AdapterRoute::ToolingUnavailable { tool } => {
            Err(ToolchainRouteError::ToolingUnavailable { tool })
        }
    }
}

impl CompilerCapability for LocalCompiler<'_, '_, '_> {
    fn readiness(&self) -> CompilerReadiness {
        CompilerReadiness::Ready
    }

    fn semantic_image_snapshot(
        &mut self,
        requested: SemanticImageAuthority,
    ) -> Result<SemanticImageSnapshot, SemanticImageAccessError> {
        Self::semantic_image_snapshot(self, requested)
    }

    fn generate(
        &mut self,
        request: ApplicationCompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        self.compile_and_publish(request)
    }

    fn compile_package<Progress>(
        &mut self,
        request: &PackageCompileRequest,
        progress: &mut Progress,
    ) -> Result<GeneratedArtifact, CompilerTerminal>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        let execution = self.execution();
        let cancelled = self.config.control.cancelled;
        let staged = execution.stage_package(request, self.scratch, cancelled, progress)?;
        self.publish_staged_single(staged, cancelled, progress)
    }
}

// Go's per-source fragments retain the admitted sibling-package projection. Charge
// every retained byte, without deduplication, against the same finite envelope as
// semantic images. Selecting this policy never widens another language's budget.
pub(crate) const fn package_fragment_limit(profile: LanguageProfile) -> usize {
    match profile {
        LanguageProfile::Go(_) => MAX_PACKAGE_SEMANTIC_BYTES,
        _ => MAX_PACKAGE_FRAGMENT_BYTES,
    }
}

fn checked_manifest_fragment_bytes(
    fragments: impl IntoIterator<Item = (LanguageProfile, usize)>,
) -> Result<usize, PackageSemanticError> {
    let mut total = 0;
    let mut limiting_profile = None;
    for (profile, length) in fragments {
        let profile = match limiting_profile {
            Some(previous)
                if package_fragment_limit(previous) <= package_fragment_limit(profile) =>
            {
                previous
            }
            _ => profile,
        };
        limiting_profile = Some(profile);
        total = checked_package_fragment_bytes(total, length, profile)?;
    }
    Ok(total)
}

fn checked_package_fragment_bytes(
    accumulated: usize,
    additional: usize,
    profile: LanguageProfile,
) -> Result<usize, PackageSemanticError> {
    let limit_bytes = package_fragment_limit(profile);
    match accumulated.checked_add(additional) {
        Some(total) if total <= limit_bytes => Ok(total),
        _ => Err(PackageSemanticError::FragmentCapacity {
            profile,
            used_bytes: accumulated as u128 + additional as u128,
            limit_bytes,
        }),
    }
}

fn checked_package_bytes(
    accumulated: usize,
    additional: usize,
    maximum: usize,
    lane: &'static str,
) -> Result<usize, PackageSemanticError> {
    let total = accumulated
        .checked_add(additional)
        .ok_or(PackageSemanticError::Capacity { lane })?;
    if total > maximum {
        return Err(PackageSemanticError::Capacity { lane });
    }
    Ok(total)
}

fn staged_output_terminal(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    phase: PublicationPhase,
) -> CompilerTerminal {
    CompilerTerminal::Publication {
        attempted: CompilerAttempt {
            source: source_authority(source),
            recipe: recipe.identity,
        },
        cause: PublicationCause::Rejected(phase),
    }
}

const fn lineage_cause(
    cause: backend_semantic::ir::PackageLineageFault,
) -> PackageDeclarationScopeCause {
    match cause {
        backend_semantic::ir::PackageLineageFault::EmptyEcosystem => {
            PackageDeclarationScopeCause::EmptyEcosystem
        }
        backend_semantic::ir::PackageLineageFault::EmptyName => {
            PackageDeclarationScopeCause::EmptyPackage
        }
        backend_semantic::ir::PackageLineageFault::SeparatorInEcosystem => {
            PackageDeclarationScopeCause::EcosystemSeparator
        }
        backend_semantic::ir::PackageLineageFault::SeparatorInName => {
            PackageDeclarationScopeCause::PackageSeparator
        }
        backend_semantic::ir::PackageLineageFault::Backslash { segment } => {
            PackageDeclarationScopeCause::LineageBackslash { segment }
        }
    }
}

const fn declaration_scope_cause(
    cause: PackageDeclarationScopeFault,
) -> PackageDeclarationScopeCause {
    match cause {
        PackageDeclarationScopeFault::Lineage(cause) => lineage_cause(cause),
        PackageDeclarationScopeFault::Declaration(
            backend_semantic::ir::DeclarationKeyFault::Path(
                backend_semantic::ir::DeclarationPathFault::Empty,
            ),
        ) => PackageDeclarationScopeCause::EmptySourcePath,
        PackageDeclarationScopeFault::Declaration(
            backend_semantic::ir::DeclarationKeyFault::Path(
                backend_semantic::ir::DeclarationPathFault::Backslash,
            ),
        ) => PackageDeclarationScopeCause::SourceBackslash,
        PackageDeclarationScopeFault::Declaration(
            backend_semantic::ir::DeclarationKeyFault::EmptyName,
        ) => PackageDeclarationScopeCause::EmptyPackage,
    }
}

fn generated(
    source: SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    semantic_image: SemanticImageArtifactFacts,
    publication: &PublishedCompilation,
) -> GeneratedArtifact {
    GeneratedArtifact {
        source,
        recipe,
        fragment,
        semantic_image: SemanticImageAuthority {
            identity: semantic_image.identity,
            byte_len: semantic_image.byte_length,
        },
        publication: PublicationAuthority {
            generation: backend_library::interface::GenerationAuthority {
                pinned_root: publication.publication.generation.pinned_root,
                dep_set: publication.publication.generation.dep_set,
            },
            manifest: publication.manifest.identity,
            binding: publication.binding.identity,
            receipt: backend_library::interface::DurableReceiptAuthority {
                sequence: *publication.publication.stable.sequence,
                durable_end: *publication.publication.stable.durable_end,
                immutable_checksum: publication.publication.immutable.checksum,
                head_checksum: publication.publication.head.checksum,
            },
        },
    }
}

fn request_source(request: ApplicationCompilerRequest<'_>) -> Result<SourceAuthority, SourceError> {
    let byte_len = u32::try_from(request.source.len()).map_err(|_| SourceError::Length {
        actual: request.source.len(),
    })?;
    Ok(SourceAuthority {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(request.source.as_bytes()),
        byte_len,
    })
}

/// Closed public projection of the native phase a TypeScript program stopped in.
const fn typescript_stop_phase(
    phase: backend_frontend_typescript::TszProjectExecutionPhase,
) -> backend_library::interface::TypeScriptStopPhase {
    use backend_frontend_typescript::TszProjectExecutionPhase as P;
    use backend_library::interface::TypeScriptStopPhase as S;
    match phase {
        P::SourceFrontier => S::SourceFrontier,
        P::ParseBind => S::ParseBind,
        P::Merge => S::Merge,
        P::FileFormats => S::FileFormats,
        P::ModuleOutcomes => S::ModuleOutcomes,
        P::CheckerPlan => S::CheckerPlan,
        P::LibraryViews => S::LibraryViews,
        P::BinderViews => S::BinderViews,
        P::OwnershipIndex => S::OwnershipIndex,
        P::SharedCheckerIndexes => S::SharedCheckerIndexes,
        P::AffectedLibraries => S::AffectedLibraries,
        P::AugmentationPrime => S::AugmentationPrime,
        P::FileReset => S::FileReset,
        P::FileCheck => S::FileCheck,
        P::FilePostprocess => S::FilePostprocess,
        P::FileQuery
        | P::FileQueryAnalysis
        | P::FileQueryStructure
        | P::FileQueryOccurrences
        | P::FileQueryTypes => S::FileQuery,
        P::SourcePreparation
        | P::SourcePreparationFunctionDefs
        | P::SourcePreparationWarmDefinitions
        | P::SourcePreparationHeritage
        | P::SourcePreparationWellKnownSymbols
        | P::SourcePreparationTypeEnvironment
        | P::SourcePreparationReconciliation
        | P::SourcePreparationBoxedTypes => S::SourcePreparation,
        P::SourceStatements => S::SourceStatements,
        P::SourcePostchecks => S::SourcePostchecks,
        P::LibraryBaseline => S::LibraryBaseline,
        P::LibraryCheck => S::LibraryCheck,
        P::Aggregate => S::Aggregate,
    }
}

/// Names the file and phase a native TypeScript program was in when it stopped.
///
/// Only an authority refusal that already carries a closed program-execution
/// cause can take the site; every other terminal, and a stop that observed no
/// boundary, is returned unchanged.
fn with_typescript_stop_site(
    terminal: CompilerTerminal,
    site: Option<&super::compiler_typescript_observation::ObservedStopSite>,
) -> CompilerTerminal {
    let Some(site) = site else {
        return terminal;
    };
    match terminal {
        CompilerTerminal::Compile {
            attempted,
            cause:
                CompilerCause::Authority {
                    phase,
                    class,
                    diagnostic: Some(diagnostic),
                },
        } => CompilerTerminal::Compile {
            attempted,
            cause: CompilerCause::Authority {
                phase,
                class,
                diagnostic: Some({
                    let diagnostic = diagnostic.with_typescript_stop_site(
                        backend_library::interface::TypeScriptStopSite::new(
                            site.phase.map(typescript_stop_phase),
                            site.file.as_deref(),
                            site.work_charged,
                            site.work_allowance,
                        ),
                    );
                    // Retain the existing closed cause and public coarse phase.
                    // Measurements are advisory fixed labels/numbers in the
                    // already bounded optional cause slot, never new wire facts.
                    if diagnostic.typescript_stop_site.is_some()
                        && diagnostic.typescript_cause.is_none()
                    {
                        match site
                            .measurement
                            .as_ref()
                            .and_then(|measurement| measurement.cause())
                        {
                            Some(cause) => diagnostic.with_typescript_cause(cause),
                            None => diagnostic,
                        }
                    } else {
                        diagnostic
                    }
                }),
            },
        },
        other => other,
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the public compiler terminal retains one bounded cold authority diagnostic"
)]
fn package_authority_terminal(
    target: ContentId<backend_version::CompilationTargetDomain>,
    request: ApplicationCompilerRequest<'_>,
    source: SourceAuthority,
    toolchain: ToolchainSelection<'_>,
    cause: PackageAuthorityError,
) -> CompilerTerminal {
    if matches!(
        &cause,
        PackageAuthorityError::PythonPyrefly(PythonCheckerError::Cancelled { .. })
            | PackageAuthorityError::GoOracle(backend_frontend_go::legacy::OracleError::Cancelled)
    ) || super::compiler_typescript_failure::failure_kind(&cause)
        .is_some_and(|failure| failure.is_cancelled())
    {
        return CompilerTerminal::PackageCancelled {
            target,
            phase: PackageCompilePhase::Authority,
        };
    }
    if let PackageAuthorityError::GoOracle(error) = &cause
        && let Some(phase) = backend_frontend_go::legacy::GoOracleControl::deadline_phase(error)
    {
        return compiler_attempt_terminal(
            request,
            source,
            toolchain,
            CompilerCause::DeadlineExceeded {
                diagnostic: backend_library::interface::CompilerDiagnostic::from_native(
                    phase.as_bytes(),
                    phase.len(),
                    false,
                ),
            },
        );
    }
    match cause {
        PackageAuthorityError::Cancelled { .. } => CompilerTerminal::PackageCancelled {
            target,
            phase: PackageCompilePhase::Authority,
        },
        PackageAuthorityError::AdapterUnavailable { .. } => CompilerTerminal::Unavailable {
            language: request.profile.language(),
            stage: request.stage,
        },
        PackageAuthorityError::ToolchainUnavailable { tool, .. } => CompilerTerminal::Toolchain {
            source,
            language: request.profile.language(),
            stage: request.stage,
            selected: tool,
            configured: None,
        },
        PackageAuthorityError::RequiredTool { issue, .. } => CompilerTerminal::RequiredTool {
            source,
            language: request.profile.language(),
            stage: request.stage,
            issue,
        },
        PackageAuthorityError::Deadline { .. } => compiler_attempt_terminal(
            request,
            source,
            toolchain,
            CompilerCause::DeadlineExceeded { diagnostic: None },
        ),
        PackageAuthorityError::RustProject(cause) => {
            // Package authority failures happen before driver scratch is leased.
            // Reuse the driver's typed Rust projection with a fixed, path-free
            // diagnostic buffer instead of flattening every Rust failure into
            // Resolve/Authority with no explanation.
            let mut diagnostic_bytes =
                [0; backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES];
            let diagnostic = rust_authority_diagnostic(Some(&mut diagnostic_bytes), &cause, false);
            let rust_scope = crate::application::terminal::rust_source_scope_failure(&cause);
            let failure = AuthorityFailure::Rust { diagnostic, cause };
            let projection = failure.projection();
            let diagnostic = backend_library::interface::CompilerDiagnostic::from_native(
                projection.diagnostic.primary,
                projection.diagnostic.observed,
                projection.diagnostic.truncated,
            );
            let diagnostic = match (diagnostic, rust_scope) {
                (Some(diagnostic), Some(scope)) => {
                    Some(diagnostic.with_rust_source_scope_failure(scope))
                }
                (None, Some(scope)) => Some(
                    backend_library::interface::CompilerDiagnostic::from_rust_source_scope_failure(
                        scope,
                    ),
                ),
                (diagnostic, None) => diagnostic,
            };
            compiler_attempt_terminal(
                request,
                source,
                toolchain,
                CompilerCause::Authority {
                    phase: projection.phase,
                    class: projection.class,
                    diagnostic,
                },
            )
        }
        cause => {
            let pre_recipe_typescript_admission =
                matches!(&cause, PackageAuthorityError::TypeScriptProjectHost(_))
                    && request.profile.language()
                        == backend_semantic::vocabulary::Language::TypeScript
                    && matches!(
                        toolchain,
                        ToolchainSelection::ExplicitlyUnavailable {
                            tool: NativeTool::TypeScriptCompiler,
                        }
                    );
            let (phase, class) = package_authority_projection(&cause);
            let typescript_failure = super::compiler_typescript_failure::failure_kind(&cause);
            let typescript_admission = super::compiler_typescript_failure::admission_detail(&cause);
            let typescript_cause = super::compiler_typescript_failure::failure_cause(&cause);
            let go_failure = match &cause {
                PackageAuthorityError::GoOracle(
                    backend_frontend_go::legacy::OracleError::DependencyClosureUnavailable {
                        failure,
                    },
                ) => {
                    use backend_frontend_go::legacy::oracle::GoDependencyClosureFailure as G;
                    use backend_library::interface::GoAuthorityFailureKind as F;
                    Some(match failure {
                        G::Invocation => F::DependencyInvocation,
                        G::Protocol => F::DependencyProtocol,
                        G::PackageLoad => F::DependencyPackageLoad,
                        G::EmptyGraph => F::DependencyEmptyGraph,
                        G::IncompleteFiles => F::DependencyIncompleteFiles,
                        G::UnsafePath => F::DependencyUnsafePath,
                        G::Limit => F::DependencyLimit,
                    })
                }
                _ => None,
            };
            let python_failure = match &cause {
                PackageAuthorityError::NativePythonToolchainIdentityMismatch { .. } => {
                    Some(PythonAuthorityFailureKind::ProducerIdentityMismatch)
                }
                PackageAuthorityError::PythonPyrefly(error) => {
                    Some(python_authority_failure_kind(error))
                }
                _ => None,
            };
            // Keep the concrete cause chain instead of an empty diagnostic, so
            // a checker that ran but failed (or never ran) explains itself.
            let message = bounded_error_chain(&cause);
            let diagnostic = backend_library::interface::CompilerDiagnostic::from_native(
                message.text.as_bytes(),
                message.text.len(),
                message.truncated,
            )
            .map(|diagnostic| match python_failure {
                Some(failure) => {
                    let diagnostic = diagnostic.with_python_failure(failure);
                    match &cause {
                        PackageAuthorityError::PythonPyrefly(error) => {
                            retain_python_configuration(diagnostic, error)
                        }
                        _ => diagnostic,
                    }
                }
                None => match typescript_failure {
                    Some(failure) => {
                        let diagnostic = diagnostic.with_typescript_failure(failure);
                        match typescript_admission {
                            Some(detail) => diagnostic.with_typescript_admission(detail),
                            None => match typescript_cause {
                                Some(cause) => diagnostic.with_typescript_cause(cause),
                                None => diagnostic,
                            },
                        }
                    }
                    None => match go_failure {
                        Some(failure) => diagnostic.with_go_failure(failure),
                        None => diagnostic,
                    },
                },
            });
            if pre_recipe_typescript_admission {
                CompilerTerminal::AuthorityAdmission {
                    source,
                    language: request.profile.language(),
                    stage: request.stage,
                    phase,
                    class,
                    diagnostic,
                }
            } else {
                compiler_attempt_terminal(
                    request,
                    source,
                    toolchain,
                    CompilerCause::Authority {
                        phase,
                        class,
                        diagnostic,
                    },
                )
            }
        }
    }
}

pub(crate) fn python_authority_failure_kind(
    error: &PythonCheckerError,
) -> PythonAuthorityFailureKind {
    use PythonCheckerError as E;
    match error {
        E::UnsupportedPythonEnvironmentVersion { .. }
        | E::PythonEnvironmentVersionMismatch { .. }
        | E::PythonInterpreterSelectionMismatch { .. }
        | E::PythonInterpreterSelectionUnavailable { .. } => {
            PythonAuthorityFailureKind::EnvironmentCaptureRefused
        }
        E::EnvironmentCaptureMissing => PythonAuthorityFailureKind::EnvironmentInputMissing,
        E::EnvironmentCapture { source } => match source.as_ref() {
            E::Cancelled { .. } => PythonAuthorityFailureKind::Cancelled,
            E::Deadline { .. } | E::ProjectDeadline { .. } => PythonAuthorityFailureKind::Deadline,
            _ => PythonAuthorityFailureKind::EnvironmentCaptureRefused,
        },
        E::ProjectPanic => PythonAuthorityFailureKind::ProjectPanic,
        E::Cancelled { .. } => PythonAuthorityFailureKind::Cancelled,
        E::Deadline { .. } | E::ProjectDeadline { .. } => PythonAuthorityFailureKind::Deadline,
        E::ProjectSyntax { .. } => PythonAuthorityFailureKind::SyntaxRejected,
        E::UncapturedDependency { .. } => PythonAuthorityFailureKind::DependencyOutsideCapture,
        E::NativeProducerIdentity { .. } => PythonAuthorityFailureKind::ProducerIdentityMismatch,
        E::IncompleteSourceFrontier { .. } => PythonAuthorityFailureKind::IncompleteSourceFrontier,
        E::NativeTypeProjection { .. } => PythonAuthorityFailureKind::TypeProjectionUnsupported,
        E::ProjectReport { .. } => PythonAuthorityFailureKind::ProjectReportMismatch,
        E::ProjectConfiguration { .. } => PythonAuthorityFailureKind::ConfigurationRefused,
        E::SourceDigest { .. } => PythonAuthorityFailureKind::SourceDigestMismatch,
        E::PackageRoot { .. } => PythonAuthorityFailureKind::InvalidPackageRoot,
        E::Spawn { .. } => PythonAuthorityFailureKind::CheckerSpawnFailed,
        E::Pipe { .. } => PythonAuthorityFailureKind::CheckerIoFailed,
        E::WorkerPanic { .. } => PythonAuthorityFailureKind::CheckerWorkerPanic,
        E::Exit { .. } => PythonAuthorityFailureKind::CheckerExited,
        E::Decode { .. } => PythonAuthorityFailureKind::CheckerProtocolInvalid,
        E::OutputLimit { .. } => PythonAuthorityFailureKind::CheckerOutputLimit,
        E::Timeout { .. } => PythonAuthorityFailureKind::CheckerTimeout,
        E::Workspace { .. } => PythonAuthorityFailureKind::WorkspaceIoFailed,
        E::InvalidSpan { .. } => PythonAuthorityFailureKind::InvalidFactSpan,
    }
}

/// Config bytes and paths remain private; copy the immutable source authority
/// before the checker error is flattened into bounded local-debug text.
fn retain_python_configuration(
    diagnostic: backend_library::interface::CompilerDiagnostic,
    error: &PythonCheckerError,
) -> backend_library::interface::CompilerDiagnostic {
    if let PythonCheckerError::ProjectConfiguration {
        source_identity,
        source_byte_len,
        omitted_faults,
        evidence_truncated,
        ..
    } = error
    {
        diagnostic.with_python_configuration(
            backend_library::interface::PythonConfigurationRefusalFacts {
                source_identity: *source_identity,
                source_byte_len: *source_byte_len,
                omitted_faults: *omitted_faults,
                evidence_truncated: *evidence_truncated,
            },
        )
    } else {
        diagnostic
    }
}

struct BoundedErrorChain {
    text: String,
    truncated: bool,
}

/// Formats the typed source chain without allocating proportional to arbitrary error messages
/// or following a maliciously cyclic/unbounded `Error::source` chain.
fn bounded_error_chain(error: &(dyn std::error::Error + 'static)) -> BoundedErrorChain {
    const MAX_BYTES: usize = backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES;
    const CAUSE_SEPARATOR: &str = ": ";
    const INTERMEDIATE_OMISSION: &str = " … intermediate causes omitted … ";

    let mut messages = Vec::with_capacity(MAX_PACKAGE_AUTHORITY_ERROR_CAUSES + 1);
    let mut seen: [Option<&(dyn std::error::Error + 'static)>;
        MAX_PACKAGE_AUTHORITY_ERROR_CAUSES + 1] = [None; MAX_PACKAGE_AUTHORITY_ERROR_CAUSES + 1];
    seen[0] = Some(error);
    let mut seen_count = 1;
    let first = bounded_error_display(error, MAX_BYTES);
    let mut truncated = first.truncated;
    messages.push(first.text);
    let mut next = error.source();
    let mut cycle_detected = false;

    for _ in 0..MAX_PACKAGE_AUTHORITY_ERROR_CAUSES {
        let Some(cause) = next else {
            break;
        };
        if seen[..seen_count]
            .iter()
            .flatten()
            .any(|visited| std::ptr::eq(*visited, cause))
        {
            cycle_detected = true;
            break;
        }
        seen[seen_count] = Some(cause);
        seen_count += 1;

        let message = bounded_error_display(cause, MAX_BYTES);
        truncated |= message.truncated;
        if !messages
            .last()
            .is_some_and(|previous| previous.ends_with(&message.text))
        {
            messages.push(message.text);
        }
        next = cause.source();
    }

    let depth_truncated = if !cycle_detected {
        if let Some(cause) = next {
            if seen[..seen_count]
                .iter()
                .flatten()
                .any(|visited| std::ptr::eq(*visited, cause))
            {
                cycle_detected = true;
                false
            } else {
                true
            }
        } else {
            false
        }
    } else {
        false
    };
    let status = if cycle_detected {
        Some(" [error source cycle detected]")
    } else if depth_truncated {
        Some(" [additional causes omitted after depth limit]")
    } else {
        None
    };
    truncated |= cycle_detected || depth_truncated;

    let mut complete_bytes = status.map_or(0, str::len);
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            complete_bytes = complete_bytes.saturating_add(CAUSE_SEPARATOR.len());
        }
        complete_bytes = complete_bytes.saturating_add(message.len());
    }
    let mut text = String::with_capacity(MAX_BYTES);
    if complete_bytes <= MAX_BYTES {
        for (index, message) in messages.iter().enumerate() {
            if index > 0 {
                text.push_str(CAUSE_SEPARATOR);
            }
            text.push_str(message);
        }
        if let Some(status) = status {
            text.push_str(status);
        }
    } else {
        truncated = true;
        if messages.len() == 1 {
            let status_bytes = status.map_or(0, str::len);
            append_error_prefix(
                &mut text,
                &messages[0],
                MAX_BYTES.saturating_sub(status_bytes),
            );
        } else {
            let omission = if messages.len() > 2 {
                INTERMEDIATE_OMISSION
            } else {
                " … "
            };
            let status_bytes = status.map_or(0, str::len);
            let message_budget = MAX_BYTES
                .saturating_sub(omission.len())
                .saturating_sub(status_bytes);
            let first = messages.first().map(String::as_str).unwrap_or_default();
            let last = messages.last().map(String::as_str).unwrap_or_default();
            let mut first_budget = first.len().min(message_budget.div_ceil(2));
            let mut last_budget = last.len().min(message_budget.saturating_sub(first_budget));
            let mut unused = message_budget.saturating_sub(first_budget + last_budget);
            let first_extra = unused.min(first.len().saturating_sub(first_budget));
            first_budget += first_extra;
            unused -= first_extra;
            last_budget += unused.min(last.len().saturating_sub(last_budget));
            append_error_prefix(&mut text, first, first_budget);
            text.push_str(omission);
            append_error_suffix(&mut text, last, last_budget);
        }
        if let Some(status) = status {
            text.push_str(status);
        }
    }
    BoundedErrorChain { text, truncated }
}

struct BoundedErrorMessage {
    text: String,
    truncated: bool,
}

fn bounded_error_display(
    error: &dyn std::fmt::Display,
    maximum_bytes: usize,
) -> BoundedErrorMessage {
    const HEAD_BYTES: usize = 96;
    const TRUNCATION_MARKER: &str = "…";
    let tail_bytes = maximum_bytes
        .saturating_sub(HEAD_BYTES)
        .saturating_sub(TRUNCATION_MARKER.len());
    let mut output = BoundedErrorMessageWriter {
        text: String::with_capacity(maximum_bytes),
        maximum_bytes,
        tail: std::collections::VecDeque::with_capacity(tail_bytes),
        tail_bytes,
        current_tail_bytes: 0,
        observed_bytes: 0,
    };
    let formatting_failed = write!(&mut output, "{error}").is_err();
    let truncated = formatting_failed || output.observed_bytes > maximum_bytes;
    let text = if output.observed_bytes > maximum_bytes {
        let tail: String = output.tail.into_iter().collect();
        let head_budget = maximum_bytes
            .saturating_sub(TRUNCATION_MARKER.len())
            .saturating_sub(tail.len())
            .min(HEAD_BYTES);
        let mut boundary = output.text.len().min(head_budget);
        while !output.text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let mut clipped = String::with_capacity(maximum_bytes);
        clipped.push_str(&output.text[..boundary]);
        clipped.push_str(TRUNCATION_MARKER);
        clipped.push_str(&tail);
        clipped
    } else {
        output.text
    };
    BoundedErrorMessage { text, truncated }
}

struct BoundedErrorMessageWriter {
    text: String,
    maximum_bytes: usize,
    tail: std::collections::VecDeque<char>,
    tail_bytes: usize,
    current_tail_bytes: usize,
    observed_bytes: usize,
}

impl std::fmt::Write for BoundedErrorMessageWriter {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        // Consume the whole Display stream so an oversized message still retains its true tail;
        // the prefix and rolling tail buffers have fixed byte limits.
        for character in value.chars() {
            let character = if character == '\0' { ' ' } else { character };
            let character_bytes = character.len_utf8();
            self.observed_bytes = self.observed_bytes.saturating_add(character_bytes);
            if self.text.len().saturating_add(character_bytes) <= self.maximum_bytes {
                self.text.push(character);
            }
            if self.tail_bytes != 0 {
                self.tail.push_back(character);
                self.current_tail_bytes += character_bytes;
                while self.current_tail_bytes > self.tail_bytes {
                    if let Some(removed) = self.tail.pop_front() {
                        self.current_tail_bytes -= removed.len_utf8();
                    }
                }
            }
        }
        Ok(())
    }
}

fn append_error_prefix(output: &mut String, value: &str, maximum_bytes: usize) {
    if value.len() <= maximum_bytes {
        output.push_str(value);
        return;
    }
    const MARKER: &str = "…";
    let content_budget = maximum_bytes.saturating_sub(MARKER.len());
    let mut boundary = value.len().min(content_budget);
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    output.push_str(&value[..boundary]);
    if maximum_bytes >= MARKER.len() {
        output.push_str(MARKER);
    }
}

fn append_error_suffix(output: &mut String, value: &str, maximum_bytes: usize) {
    if value.len() <= maximum_bytes {
        output.push_str(value);
        return;
    }
    const MARKER: &str = "…";
    let suffix_budget = maximum_bytes.saturating_sub(MARKER.len());
    let mut start = value.len().saturating_sub(suffix_budget);
    while !value.is_char_boundary(start) {
        start += 1;
    }
    if maximum_bytes >= MARKER.len() {
        output.push_str(MARKER);
    }
    output.push_str(&value[start..]);
}

fn compiler_attempt_terminal(
    request: ApplicationCompilerRequest<'_>,
    source: SourceAuthority,
    toolchain: ToolchainSelection<'_>,
    cause: CompilerCause,
) -> CompilerTerminal {
    let ToolchainSelection::ResolvedNative(resolved) = toolchain else {
        return CompilerTerminal::Toolchain {
            source,
            language: request.profile.language(),
            stage: request.stage,
            selected: match toolchain {
                ToolchainSelection::ExplicitlyUnavailable { tool } => tool,
                ToolchainSelection::ResolvedNative(_) => unreachable!(),
            },
            configured: None,
        };
    };
    let recipe = backend_semantic::vocabulary::CompileRecipeFact::derive(
        request.profile,
        request.stage,
        resolved.tool,
        source.identity,
        resolved.invocation_identity(),
    );
    CompilerTerminal::Compile {
        attempted: CompilerAttempt {
            source,
            recipe: recipe.identity,
        },
        cause,
    }
}

fn package_authority_projection(
    cause: &PackageAuthorityError,
) -> (
    backend_semantic::vocabulary::AuthorityPhase,
    AuthorityDiagnosticClass,
) {
    use backend_semantic::vocabulary::{
        AuthorityDiagnosticClass as Class, AuthorityPhase as Phase,
    };

    match cause {
        PackageAuthorityError::SourceOutsidePackage { .. }
        | PackageAuthorityError::ExternalToolchainRequired { .. }
        | PackageAuthorityError::TypeScriptSourcePath { .. }
        | PackageAuthorityError::CompilationUnitMismatch { .. }
        | PackageAuthorityError::CompilationUnitSourceMismatch { .. }
        | PackageAuthorityError::RustToolchainExecutableMismatch { .. }
        | PackageAuthorityError::ClangToolchainExecutableMismatch { .. }
        | PackageAuthorityError::NativePythonToolchainIdentityMismatch { .. }
        | PackageAuthorityError::GoAuthorityInputsChanged { .. }
        | PackageAuthorityError::GoModuleLanguage { .. }
        | PackageAuthorityError::GoAuthorityUnavailable { .. }
        | PackageAuthorityError::ClangProject(_) => (Phase::Open, Class::Binding),
        PackageAuthorityError::PythonSyntax(_) => (Phase::Parse, Class::Syntax),
        PackageAuthorityError::PythonPyrefly(PythonCheckerError::ProjectConfiguration {
            ..
        }) => (Phase::Parse, Class::Authority),
        PackageAuthorityError::PythonPyrefly(PythonCheckerError::ProjectDeadline { phase }) => {
            python_project_deadline_projection(*phase)
        }
        PackageAuthorityError::PythonPyrefly(PythonCheckerError::EnvironmentCapture { .. }) => {
            (Phase::Open, Class::Authority)
        }
        PackageAuthorityError::PythonPyrefly(_) => (Phase::TypeCheck, Class::Type),
        PackageAuthorityError::RustProject(_)
        | PackageAuthorityError::RustWorkspaceWitnessUnavailable { .. } => {
            (Phase::Resolve, Class::Authority)
        }
        PackageAuthorityError::GoOracle(cause) => go_authority_projection(cause),
        PackageAuthorityError::CSharp(_) => (Phase::TypeCheck, Class::Authority),
        PackageAuthorityError::TypeScript(_) => (Phase::TypeCheck, Class::Authority),
        PackageAuthorityError::TypeScriptTsz(_) => (Phase::Resolve, Class::Authority),
        PackageAuthorityError::TypeScriptProjectHost(_) => (Phase::TypeCheck, Class::Authority),
        PackageAuthorityError::JavaHarness(_)
        | PackageAuthorityError::GoAuthorityWitness(_)
        | PackageAuthorityError::ImageTooLarge { .. }
        | PackageAuthorityError::RustToolchainAdmission { .. }
        | PackageAuthorityError::ToolchainUnavailable { .. }
        | PackageAuthorityError::Cancelled { .. }
        | PackageAuthorityError::Deadline { .. }
        | PackageAuthorityError::AdapterUnavailable { .. }
        | PackageAuthorityError::RequiredTool { .. } => (Phase::Open, Class::Authority),
    }
}

#[cfg(test)]
#[path = "compiler/typescript_installed_detail_tests.rs"]
mod typescript_installed_detail_tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceError {
    Length { actual: usize },
}

const fn source_terminal(cause: SourceError) -> CompilerTerminal {
    match cause {
        SourceError::Length { actual } => CompilerTerminal::SourceLength { actual },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolchainRouteError {
    UnsupportedStage(backend_semantic::vocabulary::FrontendError),
    Missing { selected: NativeTool },
    ToolingUnavailable { tool: NativeTool },
}

const fn toolchain_terminal(
    source: SourceAuthority,
    request: ApplicationCompilerRequest<'_>,
    cause: ToolchainRouteError,
) -> CompilerTerminal {
    match cause {
        ToolchainRouteError::UnsupportedStage(cause) => {
            CompilerTerminal::UnsupportedStage { source, cause }
        }
        ToolchainRouteError::Missing { selected } => CompilerTerminal::Toolchain {
            source,
            language: request.profile.language(),
            stage: request.stage,
            selected,
            configured: None,
        },
        ToolchainRouteError::ToolingUnavailable { tool } => CompilerTerminal::ToolingUnavailable {
            source,
            language: request.profile.language(),
            stage: request.stage,
            tool,
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::host_path;
    use std::{
        error::Error as StdError,
        fmt,
        path::Path,
        sync::{OnceLock, atomic::AtomicBool},
    };

    use crate::driver::{ResolvedToolchain, ToolchainResolutionError, ToolchainSelection};
    use backend_library::interface::{
        AuthorityDiagnosticClass, AuthorityPhase, CompilerCause, CorrelationId, GenerateTarget,
        PackageCompileRequest,
    };
    use backend_semantic::registry::AdapterRoute;
    use backend_semantic::vocabulary::NativeTool;
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, RustEdition, Stage};
    use thiserror::Error;

    use crate::application::{LocalToolchainSet, LocalToolchainSetError, PackageAuthorityError};
    use backend_frontend_go::legacy::OracleError;
    use backend_frontend_rust::legacy::RustAuthorityError;

    use super::{
        CompilerTerminal, EmbeddingProvisioningFailure, PackageSemanticError, PackageSource,
        PackageSourceSet, PackageSourceSetError, StagedEmbeddingStatus, ToolchainRouteError,
        bounded_error_chain, package_authority_projection, package_authority_terminal,
        request_source, select_toolchain,
    };
    use crate::compiler_input_manifest_v2::{CompilationUnitKeyV2, CompilerPackageTargetV2};
    use backend_version::{ContentId, SourceFactDomain};

    #[test]
    fn go_package_fragments_admit_materialized_over_64_mib_without_widening_other_profiles() {
        use backend_semantic::vocabulary::GoVersion;
        // Exercise real resident fragment lengths beyond the former aggregate cap.
        // These bytes test sizing only; they are not claimed as admitted semantic IR.
        let fragments = vec![0_u8; 65 * 1024 * 1024];
        let go = LanguageProfile::Go(GoVersion::Go124);
        let mut total = 0;
        for fragment in fragments.chunks(1024 * 1024) {
            total = super::checked_package_fragment_bytes(total, fragment.len(), go)
                .expect("Go sibling-package fragments fit the finite 512 MiB plane");
        }
        assert_eq!(total, fragments.len());
        assert!(total > super::MAX_PACKAGE_FRAGMENT_BYTES);
        assert!(total < super::MAX_PACKAGE_SEMANTIC_BYTES);
        for profile in LanguageProfile::PRODUCT_PROFILES {
            if matches!(profile, LanguageProfile::Go(_)) {
                continue;
            }
            // Re-admission is profile-bound, including a retained aggregate first sized as Go.
            let error = super::checked_package_fragment_bytes(total, 0, profile)
                .expect_err("another profile must retain its 64 MiB fragment budget");
            assert!(matches!(
                error,
                PackageSemanticError::FragmentCapacity {
                    profile: observed,
                    used_bytes,
                    limit_bytes: super::MAX_PACKAGE_FRAGMENT_BYTES,
                } if observed == profile && used_bytes == fragments.len() as u128
            ));
        }
    }

    #[test]
    fn reopened_fragment_manifest_cannot_borrow_a_go_budget_for_another_profile() {
        use backend_semantic::vocabulary::{GoVersion, PythonVersion};
        let go = LanguageProfile::Go(GoVersion::Go124);
        let python = LanguageProfile::Python(PythonVersion::Python314);
        let mib = 1024 * 1024;
        assert_eq!(
            super::checked_manifest_fragment_bytes([(go, 64 * mib), (go, mib)])
                .expect("all-Go retained fragments"),
            65 * mib
        );
        for entries in [[(go, 65 * mib), (python, 0)], [(python, 0), (go, 65 * mib)]] {
            let error = super::checked_manifest_fragment_bytes(entries)
                .expect_err("mixed profiles retain the tighter whole-manifest cap");
            assert!(matches!(error, PackageSemanticError::FragmentCapacity {
                profile,
                used_bytes,
                limit_bytes,
            } if profile == python && used_bytes == 65 * mib as u128 && limit_bytes == 64 * mib));
        }
        assert_eq!(
            super::checked_manifest_fragment_bytes([(go, 32 * mib), (python, 32 * mib)])
                .expect("mixed manifest at exact tighter boundary"),
            64 * mib
        );
        assert_eq!(
            super::checked_manifest_fragment_bytes([(go, 256 * mib), (go, 256 * mib)])
                .expect("exact all-Go reopen boundary"),
            512 * mib
        );
        assert!(super::checked_manifest_fragment_bytes([(go, 512 * mib), (go, 1)]).is_err());
        assert!(super::checked_manifest_fragment_bytes([(python, 64 * mib), (go, 1)]).is_err());
        assert!(super::checked_manifest_fragment_bytes([(go, usize::MAX), (go, 1)]).is_err());
    }

    #[test]
    fn package_fragment_limits_are_exact_profile_bound_and_overflow_safe() {
        use backend_semantic::vocabulary::GoVersion;
        for version in [
            GoVersion::Go122,
            GoVersion::Go123,
            GoVersion::Go124,
            GoVersion::Go125,
            GoVersion::Go126,
        ] {
            let profile = LanguageProfile::Go(version);
            assert_eq!(super::package_fragment_limit(profile), 512 * 1024 * 1024);
        }
        for profile in LanguageProfile::PRODUCT_PROFILES {
            let limit = if matches!(profile, LanguageProfile::Go(_)) {
                512 * 1024 * 1024
            } else {
                64 * 1024 * 1024
            };
            assert_eq!(super::package_fragment_limit(profile), limit);
            assert_eq!(
                super::checked_package_fragment_bytes(limit - 1, 1, profile)
                    .expect("exact finite boundary"),
                limit
            );
            for (accumulated, additional) in [(limit, 1), (usize::MAX, 1), (usize::MAX, usize::MAX)]
            {
                let error = super::checked_package_fragment_bytes(accumulated, additional, profile)
                    .expect_err("over-budget and overflowing lengths must refuse");
                let diagnostic = error.to_string();
                let attempted = accumulated as u128 + additional as u128;
                assert!(diagnostic.contains(&format!("{profile:?}")));
                assert!(diagnostic.contains(&format!("used {attempted} bytes")));
                assert!(diagnostic.contains(&format!("limit {limit} bytes")));
                assert!(diagnostic.len() < 256);
                assert!(matches!(
                    error,
                    PackageSemanticError::FragmentCapacity {
                        profile: observed,
                        used_bytes,
                        limit_bytes,
                    } if observed == profile && used_bytes == attempted && limit_bytes == limit
                ));
            }
        }
        // The independent semantic-image and optional embedding planes remain unchanged.
        assert_eq!(super::MAX_PACKAGE_SEMANTIC_BYTES, 512 * 1024 * 1024);
        assert_eq!(super::MAX_PACKAGE_EMBEDDING_BYTES, 64 * 1024 * 1024);
    }

    #[test]
    fn required_embedding_admission_preserves_configured_provisioning_cause() {
        use super::{EmbeddingRequirement, require_package_embedding_runtime};
        for cause in [
            EmbeddingProvisioningFailure::ModelUnavailable,
            EmbeddingProvisioningFailure::TokenizerUnavailable,
            EmbeddingProvisioningFailure::ArtifactIdentityMismatch,
            EmbeddingProvisioningFailure::ExecutableUnavailable,
            EmbeddingProvisioningFailure::ActivationRejected,
            EmbeddingProvisioningFailure::ResourceLimitUnavailable,
            EmbeddingProvisioningFailure::PlatformUnsupported,
        ] {
            let error = require_package_embedding_runtime(
                "main.ts",
                false,
                Some(cause),
                EmbeddingRequirement::Required,
            )
            .expect_err("required provisioning must refuse");
            assert!(
                matches!(&error, PackageSemanticError::EmbeddingProvisioning { cause: actual, .. } if *actual == cause)
            );
            assert!(!error.to_string().contains("not configured"));
            assert!(
                require_package_embedding_runtime(
                    "main.ts",
                    false,
                    Some(cause),
                    EmbeddingRequirement::Optional
                )
                .is_ok()
            );
        }
        assert!(matches!(
            require_package_embedding_runtime(
                "main.ts",
                false,
                None,
                EmbeddingRequirement::Required
            ),
            Err(PackageSemanticError::Embedding { .. })
        ));
        assert!(
            require_package_embedding_runtime(
                "main.ts",
                true,
                None,
                EmbeddingRequirement::Required
            )
            .is_ok()
        );
    }

    #[test]
    fn go_oracle_workspace_setup_errors_keep_open_authority_projection_and_cause_detail() {
        let directory_error =
            PackageAuthorityError::GoOracle(OracleError::GoOracleSourceDirectory(
                std::io::Error::other("private temporary directory unavailable"),
            ));
        let file_error = PackageAuthorityError::GoOracle(OracleError::GoOracleSourceFile {
            path: Path::new("/tmp/nudox-go-oracle/main.go").to_path_buf(),
            source: std::io::Error::other("private source write denied"),
        });

        for error in [&directory_error, &file_error] {
            assert_eq!(
                package_authority_projection(error),
                (AuthorityPhase::Open, AuthorityDiagnosticClass::Authority)
            );
        }
        let directory_detail = bounded_error_chain(&directory_error);
        assert!(
            directory_detail
                .text
                .contains("private temporary directory unavailable")
        );
        let file_detail = bounded_error_chain(&file_error);
        assert!(file_detail.text.contains("/tmp/nudox-go-oracle/main.go"));
        assert!(file_detail.text.contains("private source write denied"));
    }

    #[test]
    fn go_package_queue_keeps_capture_deadline_and_cancellation_precedence() {
        use crate::application::{
            LocalCompilerConfig, LocalCompilerControl, LocalCompilerTimeout, LocalPackageRootSet,
        };
        use backend_library::interface::GenerateTarget;
        let cancelled = AtomicBool::new(false);
        // A metadata tuple is sufficient to classify this pre-execution stop.
        // No oracle, executable, package files or native authority are supplied.
        let selected = [ToolchainSelection::ResolvedNative(
            ResolvedToolchain::from_version(
                NativeTool::GoCompiler,
                host_path("/unopened/queue-test/go"),
                b"go version go1.25.0 linux/amd64",
            )
            .unwrap(),
        )];
        let execution = super::LocalCompilerExecution {
            config: LocalCompilerConfig {
                toolchains: LocalToolchainSet::validate(&selected).unwrap(),
                artifact_directory: Path::new("/unopened/queue-test"),
                journal_directory: Path::new("/unopened/queue-test"),
                native_work_directory: Path::new("/unopened/queue-test"),
                control: LocalCompilerControl {
                    timeout: LocalCompilerTimeout::new(std::time::Duration::from_secs(600))
                        .unwrap(),
                    cancelled: &cancelled,
                },
            },
            package_roots: LocalPackageRootSet::EMPTY,
            package_authority: super::PackageAuthorityConfiguration::UNAVAILABLE,
            native_work_directory: Path::new("/unopened/queue-test").into(),
        };
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(0xd1),
                profile: LanguageProfile::Go(backend_semantic::vocabulary::GoVersion::Go125),
                stage: Stage::LowerIr,
            },
            PackageUrl::parse("pkg:golang/deadline.test/app@1.0.0".to_owned()).unwrap(),
        )
        .unwrap();
        let sources =
            [super::PackageSource::new("main.go", "package app\nfunc Main() {}\n").unwrap()];
        let expired = std::time::Instant::now() - std::time::Duration::from_millis(1);
        for (phase, expected) in [
            (Some("witness_capture"), "witness_capture"),
            (None, "compiler_lane_wait"),
        ] {
            let package =
                super::PackageSourceSet::new(&request, Path::new("/unopened/queue-test"), &sources)
                    .unwrap()
                    .with_go_compile_control(Some(expired), phase);
            assert_eq!(package.go_compile_deadline, Some(expired));
            let mut scratch =
                super::LocalCompilerScratch::with_fragment_capacity(std::num::NonZeroUsize::MIN)
                    .unwrap();
            let run = |scratch: &mut super::LocalCompilerScratch| {
                execution.stage_package_sources(
                    package.clone(),
                    super::PackageExecutionAuthority::Fixed {
                        execution: None,
                        plane: None,
                    },
                    None,
                    None,
                    super::EmbeddingRequirement::Optional,
                    scratch,
                    &cancelled,
                    &mut |_| {},
                )
            };
            let Err(error) = run(&mut scratch) else {
                panic!("queueing cannot grant a new 600 seconds")
            };
            let PackageSemanticError::Compile { terminal, .. } = error else {
                panic!("typed compiler terminal required")
            };
            assert!(matches!(
                terminal.as_ref(),
                CompilerTerminal::Compile {
                    cause: backend_library::interface::CompilerCause::DeadlineExceeded {
                        diagnostic: Some(_)
                    },
                    ..
                }
            ));
            let text = format!(
                "{}",
                super::super::compiler_terminal_text::TerminalText::new("main.go", &terminal)
            );
            assert!(text.contains(expected), "{text}");
            cancelled.store(true, std::sync::atomic::Ordering::Release);
            let Err(error) = run(&mut scratch) else {
                panic!("external cancellation remains distinct")
            };
            assert!(
                matches!(error, PackageSemanticError::Compile { terminal, .. }
                if matches!(terminal.as_ref(), CompilerTerminal::PackageCancelled { .. }))
            );
            cancelled.store(false, std::sync::atomic::Ordering::Release);
        }
    }

    #[test]
    fn go_absolute_deadline_keeps_typed_deadline_and_phase_not_external_cancel() {
        let request = super::ApplicationCompilerRequest {
            profile: LanguageProfile::Go(backend_semantic::vocabulary::GoVersion::Go125),
            stage: Stage::LowerIr,
            source: "package app\nfunc Main() {}\n",
        };
        let target = ContentId::<backend_version::CompilationTargetDomain>::from_canonical_bytes(
            b"go-absolute-deadline",
        );
        let toolchain = || {
            ToolchainSelection::ResolvedNative(
                ResolvedToolchain::from_version(
                    NativeTool::GoCompiler,
                    host_path("/toolchain/bin/go"),
                    b"go version go1.25.0 linux/amd64",
                )
                .unwrap(),
            )
        };
        let deadline = package_authority_terminal(
            target,
            request,
            request_source(request).unwrap(),
            toolchain(),
            PackageAuthorityError::GoOracle(
                backend_frontend_go::legacy::GoOracleControl::new(None, std::time::Instant::now())
                    .check("witness_reproof")
                    .unwrap_err(),
            ),
        );
        assert!(matches!(
            &deadline,
            CompilerTerminal::Compile {
                cause: backend_library::interface::CompilerCause::DeadlineExceeded {
                    diagnostic: Some(_)
                },
                ..
            }
        ));
        let text = format!(
            "{}",
            super::super::compiler_terminal_text::TerminalText::new("main.go", &deadline)
        );
        assert!(text.contains("deadline") && text.contains("witness_reproof"));
        let cancelled = package_authority_terminal(
            target,
            request,
            request_source(request).unwrap(),
            toolchain(),
            PackageAuthorityError::GoOracle(OracleError::Cancelled),
        );
        assert!(matches!(
            cancelled,
            CompilerTerminal::PackageCancelled { .. }
        ));
        for (phase, milliseconds) in [
            ("collect", 10),
            ("absolute_package/arbitrary", 0),
            ("absolute_package/witness_capture", 600_000),
        ] {
            let ordinary = package_authority_terminal(
                target,
                request,
                request_source(request).unwrap(),
                toolchain(),
                PackageAuthorityError::GoOracle(OracleError::Timeout {
                    phase,
                    milliseconds,
                }),
            );
            assert!(
                matches!(
                    ordinary,
                    CompilerTerminal::Compile {
                        cause: backend_library::interface::CompilerCause::Authority { .. },
                        ..
                    }
                ),
                "only the exact reserved absolute-package carrier may become DeadlineExceeded"
            );
        }
    }

    #[test]
    fn go_dependency_refusal_preserves_exact_safe_cause_and_normal_setup_command() {
        use backend_frontend_go::legacy::oracle::GoDependencyClosureFailure;
        use backend_library::interface::GoAuthorityFailureKind;
        let request = super::ApplicationCompilerRequest {
            profile: LanguageProfile::Go(backend_semantic::vocabulary::GoVersion::Go125),
            stage: Stage::LowerIr,
            source: "package app\nfunc Main() {}\n",
        };
        let source = request_source(request).unwrap();
        let target = ContentId::<backend_version::CompilationTargetDomain>::from_canonical_bytes(
            b"go-dependency-refusal",
        );
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::GoCompiler,
            host_path("/toolchain/bin/go"),
            b"go version go1.25.0 linux/amd64",
        )
        .unwrap();
        let terminal = package_authority_terminal(
            target,
            request,
            source,
            ToolchainSelection::ResolvedNative(toolchain),
            PackageAuthorityError::GoOracle(OracleError::DependencyClosureUnavailable {
                failure: GoDependencyClosureFailure::PackageLoad,
            }),
        );
        let failure =
            backend_library::PackageCompilerFailure::from_package_terminal("main.go", &terminal)
                .unwrap()
                .unwrap();
        assert_eq!(
            failure.kind_tag(),
            GoAuthorityFailureKind::DependencyPackageLoad.kind_tag()
        );
        assert!(failure.detail().contains("go mod download"));
        assert!(!failure.detail().contains("restart"));
        assert!(!failure.cause().requires_tool_configuration());
        let encoded = serde_json::to_string(&failure).unwrap();
        assert!(encoded.contains("dependency_package_load"));
        assert!(!encoded.contains("/toolchain"));
    }

    #[test]
    fn project_typescript_admission_fault_is_not_missing_global_tsc() {
        use backend_semantic::vocabulary::{Language, TypeScriptSource};

        let request = super::ApplicationCompilerRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source: "module.exports = {};",
        };
        let source = request_source(request).expect("exact original source");
        let target = backend_version::ContentId::from_canonical_bytes(b"project admission target");
        let unavailable = ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::TypeScriptCompiler,
        };
        let terminal = package_authority_terminal(
            target,
            request,
            source,
            unavailable,
            PackageAuthorityError::TypeScriptProjectHost(
                crate::application::TypeScriptProjectHostError::CompilerApiBridge {
                    message: "private original admission evidence".into(),
                },
            ),
        );
        let CompilerTerminal::AuthorityAdmission {
            source: retained_source,
            language: Language::TypeScript,
            stage: Stage::LowerIr,
            diagnostic: Some(diagnostic),
            ..
        } = &terminal
        else {
            panic!("the concrete host error must survive without a global tsc recipe");
        };
        assert_eq!(*retained_source, source);
        assert!(
            std::str::from_utf8(diagnostic.retained_bytes_for_local_debug())
                .unwrap()
                .contains("private original admission evidence")
        );
        let failure = backend_library::PackageCompilerFailure::from_package_terminal(
            ".lintstagedrc.js",
            &terminal,
        )
        .unwrap()
        .unwrap();
        assert_eq!(failure.recipe_identity(), None);
        assert_eq!(failure.kind_tag(), "typescript_host_compiler_api_bridge");
        assert!(!failure.cause().requires_tool_configuration());
        let encoded = failure.encode_bounded_json().unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("private original"));
        let reopened =
            backend_library::PackageCompilerFailure::decode_bounded_json(&encoded).unwrap();
        assert_eq!(reopened, failure);
        assert_eq!(reopened.recipe_identity(), None);

        let missing = package_authority_terminal(
            target,
            request,
            source,
            unavailable,
            PackageAuthorityError::ToolchainUnavailable {
                profile: request.profile,
                tool: NativeTool::TypeScriptCompiler,
            },
        );
        assert!(matches!(
            missing,
            CompilerTerminal::Toolchain {
                selected: NativeTool::TypeScriptCompiler,
                configured: None,
                ..
            }
        ));
    }

    #[test]
    fn described_typescript_host_cause_reaches_the_public_failure_detail_and_roundtrips() {
        use backend_semantic::vocabulary::TypeScriptSource;
        let request = super::ApplicationCompilerRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source: "export const value = 1;",
        };
        let source = request_source(request).expect("exact original source");
        let target = backend_version::ContentId::from_canonical_bytes(b"described target");
        let unavailable = ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::TypeScriptCompiler,
        };
        let root = Path::new("/private/home/user/app");
        let error = super::super::compiler_typescript_failure::describe_host_error(
            crate::application::TypeScriptProjectHostError::ConfigInvalid {
                config: root.join("tsconfig.app.json").into_boxed_path(),
                message: "expected value at line 3 column 5".into(),
            },
            &[root],
        );
        let terminal = package_authority_terminal(
            target,
            request,
            source,
            unavailable,
            PackageAuthorityError::TypeScriptProjectHost(error),
        );
        let failure = backend_library::PackageCompilerFailure::from_package_terminal(
            "src/main.tsx",
            &terminal,
        )
        .unwrap()
        .unwrap();
        assert_eq!(failure.kind_tag(), "typescript_host_config_invalid");
        assert_eq!(
            failure.detail(),
            "TypeScript project configuration could not be decoded: \
             tsconfig.app.json is malformed: expected value at line 3 column 5"
        );
        let encoded = failure.encode_bounded_json().unwrap();
        let text = String::from_utf8_lossy(&encoded);
        assert!(text.contains("\"typescript_cause\""));
        assert!(!text.contains("/private/home"), "{text}");
        let reopened =
            backend_library::PackageCompilerFailure::decode_bounded_json(&encoded).unwrap();
        assert_eq!(reopened, failure);
        assert_eq!(reopened.detail(), failure.detail());

        // Text that disagrees with the retained cause is refused on decode.
        let mut forged: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let slot = forged
            .pointer_mut("/cause/fault/diagnostic/typescript_cause")
            .expect("cause fact is serialized next to the closed kind");
        *slot = serde_json::json!("some other cause");
        assert!(
            backend_library::PackageCompilerFailure::decode_bounded_json(
                &serde_json::to_vec(&forged).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn project_typescript_fault_retains_its_admitted_recipe_without_global_tsc() {
        use crate::application::{
            LocalCompilerConfig, LocalCompilerControl, LocalCompilerTimeout, LocalPackageRootSet,
        };
        use backend_semantic::vocabulary::TypeScriptSource;

        let cancelled = AtomicBool::new(false);
        let unavailable = [ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::TypeScriptCompiler,
        }];
        let execution = super::LocalCompilerExecution {
            config: LocalCompilerConfig {
                toolchains: LocalToolchainSet::validate(&unavailable)
                    .expect("explicit global absence"),
                artifact_directory: Path::new("/tmp"),
                journal_directory: Path::new("/tmp"),
                native_work_directory: Path::new("/tmp"),
                control: LocalCompilerControl {
                    timeout: LocalCompilerTimeout::new(std::time::Duration::from_secs(30))
                        .expect("finite time"),
                    cancelled: &cancelled,
                },
            },
            package_roots: LocalPackageRootSet::EMPTY,
            package_authority: super::PackageAuthorityConfiguration::UNAVAILABLE,
            native_work_directory: Path::new("/tmp").into(),
        };
        let project = ResolvedToolchain::from_version(
            NativeTool::TypeScriptCompiler,
            host_path("/host/project-typescript"),
            b"Version 5.7.2\n",
        )
        .expect("unit project tuple");
        let request = super::ApplicationCompilerRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source: "export const configured = true;",
        };
        let source = request_source(request).expect("exact input source");
        let selected = execution
            .toolchain_for_package(request, Some(project))
            .expect("request-scoped project tuple takes precedence");
        let terminal = package_authority_terminal(
            backend_version::ContentId::from_canonical_bytes(b"project target"),
            request,
            source,
            selected,
            PackageAuthorityError::TypeScriptProjectHost(
                crate::application::TypeScriptProjectHostError::CompilerApiBridge {
                    message: "original postcss.config.js is outside the selected program".into(),
                },
            ),
        );
        let CompilerTerminal::Compile {
            attempted,
            cause: CompilerCause::Authority { diagnostic, .. },
        } = &terminal
        else {
            panic!("a witnessed project fault must not become missing global tsc");
        };
        let expected = backend_semantic::vocabulary::CompileRecipeFact::derive(
            request.profile,
            request.stage,
            project.tool,
            source.identity,
            project.invocation_identity(),
        );
        assert_eq!(attempted.source, source);
        assert_eq!(attempted.recipe, expected.identity);
        let diagnostic = diagnostic.as_ref().expect("retain concrete original cause");
        assert!(
            std::str::from_utf8(diagnostic.retained_bytes_for_local_debug())
                .expect("UTF-8 diagnostic")
                .contains("postcss.config.js")
        );
        let failure =
            backend_library::PackageCompilerFailure::from_package_terminal("entry.ts", &terminal)
                .expect("terminal projection")
                .expect("typed project failure");
        assert_eq!(failure.kind_tag(), "typescript_host_compiler_api_bridge");
        assert_eq!(
            failure.detail(),
            backend_library::interface::TypeScriptAuthorityFailureKind::HostCompilerApiBridge
                .detail(),
            "an undescribed bridge error keeps the closed sentence and no native text"
        );
        assert!(
            !serde_json::to_string(&failure)
                .unwrap()
                .contains("postcss.config.js")
        );
        assert!(!failure.cause().requires_tool_configuration());
        assert!(matches!(
            execution
                .toolchain(request)
                .expect("global admission is unchanged"),
            ToolchainSelection::ExplicitlyUnavailable {
                tool: NativeTool::TypeScriptCompiler
            }
        ));
    }

    #[test]
    fn native_program_sources_capture_bound_libraries_and_exact_package_mapping() {
        use backend_frontend_typescript::{
            TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput, TszLibraryInput,
            TszProjectAuthority, TszProjectOptions, TszProjectSemanticOptions,
        };
        use std::collections::BTreeMap;
        let mut authority = TszProjectAuthority::new();
        let library = TszLibraryInput::from_utf8(
            "@compiler/lib.checker-only.d.ts",
            b"interface CheckerOnly {}".to_vec(),
        )
        .expect("admitted library")
        .into_lib_file();
        authority
            .update(
                vec![
                    TszFileInput {
                        path: "workspace/pkg/main.ts".into(),
                        source: "export {};".into(),
                    },
                    TszFileInput {
                        path: "workspace/pkg/omitted.ts".into(),
                        source: "export const value = 1;".into(),
                    },
                    // An unmapped declaration input is a real bound program
                    // member. Checker-only LibFiles are retained separately.
                    TszFileInput {
                        path: "@compiler/lib.fixture.d.ts".into(),
                        source: "interface Fixture {}".into(),
                    },
                ],
                TszProjectOptions {
                    checker: TszCheckerOptions::default(),
                    semantic_options: TszProjectSemanticOptions::structural(),
                    program_diagnostics: Vec::new(),
                    module_resolutions: Vec::new(),
                    file_formats: Vec::new(),
                    environment: TszEnvironmentFingerprint::from_sha256([7; 32]),
                },
                &[library],
            )
            .expect("real native bound program");
        let project = authority.project().expect("project retained");
        let mappings = BTreeMap::from([
            (
                Box::<str>::from("main.ts"),
                Box::<str>::from("workspace/pkg/main.ts"),
            ),
            (
                Box::<str>::from("omitted.ts"),
                Box::<str>::from("workspace/pkg/omitted.ts"),
            ),
        ]);
        let mut polls = 0;
        let manifest = super::capture_native_program_sources(
            project,
            mappings
                .iter()
                .map(|(package, native)| (package.as_ref(), native.as_ref())),
            ContentId::from_canonical_bytes(b"captured compiler"),
            |_| {
                polls += 1;
                Ok(())
            },
        )
        .expect("manifest");
        assert_eq!(manifest.sources().len(), project.program().files.len());
        assert!(
            manifest
                .sources()
                .any(|row| row.program_path == "@compiler/lib.fixture.d.ts"
                    && row.package_path.is_none())
        );
        assert_eq!(
            project.source_text("@compiler/lib.checker-only.d.ts"),
            Some("interface CheckerOnly {}")
        );
        assert!(
            !manifest
                .sources()
                .any(|row| row.program_path == "@compiler/lib.checker-only.d.ts")
        );
        assert_eq!(
            manifest
                .sources()
                .filter(|row| row.package_path.is_some())
                .count(),
            2
        );
        assert!(polls > 3);
        let wrong = BTreeMap::from([(
            Box::<str>::from("main.ts"),
            Box::<str>::from("absent/main.ts"),
        )]);
        assert!(matches!(
            super::capture_native_program_sources(
                project,
                wrong
                    .iter()
                    .map(|(package, native)| (package.as_ref(), native.as_ref())),
                ContentId::from_canonical_bytes(b"captured compiler"),
                |_| Ok(())
            ),
            Err(PackageSemanticError::NativeProgramSources(_))
        ));
        assert!(
            matches!(
                super::capture_native_program_sources(
                    project,
                    mappings
                        .iter()
                        .map(|(package, native)| (package.as_ref(), native.as_ref())),
                    ContentId::from_canonical_bytes(b"captured compiler"),
                    |_| Err(PackageSemanticError::MissingPublication)
                ),
                Err(PackageSemanticError::MissingPublication)
            ),
            "original checkpoint refusal is retained"
        );
    }

    #[test]
    fn typescript_real_native_authority_stop_preserves_typed_cause_and_terminal() {
        use backend_frontend_typescript::{
            TszAuthorityError, TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput,
            TszProjectAuthority, TszProjectExecutionBudget, TszProjectOptions,
            TszProjectSemanticOptions,
        };
        use backend_library::interface::TypeScriptAuthorityFailureKind as F;
        let profile =
            LanguageProfile::TypeScript(backend_semantic::vocabulary::TypeScriptSource::TypeScript);
        let request = super::ApplicationCompilerRequest {
            profile,
            stage: Stage::LowerIr,
            source: "export const x = 1;",
        };
        let source = request_source(request).expect("source authority");
        let target = ContentId::<backend_version::CompilationTargetDomain>::from_canonical_bytes(
            b"native-stop-control",
        );
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::TypeScriptCompiler,
            host_path("/toolchain/bin/tsc"),
            b"Version 5.9.0",
        )
        .expect("resolved control toolchain");
        for cancelled in [false, true] {
            let flag = std::sync::atomic::AtomicBool::new(cancelled);
            let deadline = std::time::Instant::now() - std::time::Duration::from_millis(1);
            let budget = TszProjectExecutionBudget::new(deadline, &flag, 1_000_000);
            let mut authority = TszProjectAuthority::new();
            let error = authority
                .update_with_execution_checkpoint(
                    vec![TszFileInput {
                        path: "entry.ts".into(),
                        source: request.source.into(),
                    }],
                    TszProjectOptions {
                        checker: TszCheckerOptions::default(),
                        semantic_options: TszProjectSemanticOptions::structural(),
                        program_diagnostics: Vec::new(),
                        module_resolutions: Vec::new(),
                        file_formats: Vec::new(),
                        environment: TszEnvironmentFingerprint::from_sha256([7; 32]),
                    },
                    &[],
                    &budget,
                )
                .expect_err("real authority checkpoint refuses before publication");
            assert!(authority.project().is_none());
            let terminal = package_authority_terminal(
                target,
                request,
                source,
                ToolchainSelection::ResolvedNative(toolchain),
                PackageAuthorityError::TypeScriptTsz(error),
            );
            if cancelled {
                assert!(matches!(
                    terminal,
                    CompilerTerminal::PackageCancelled { .. }
                ));
            } else {
                let failure = backend_library::PackageCompilerFailure::from_package_terminal(
                    "entry.ts", &terminal,
                )
                .expect("projection")
                .expect("deadline refusal");
                assert_eq!(failure.kind_tag(), F::NativeUpdateDeadline.kind_tag());
                assert!(!failure.cause().requires_tool_configuration());
                assert!(failure.detail().contains("admitted deadline"));
            }
        }
        // The historical reported Next call site stays distinct from a real update stop.
        let error = TszAuthorityError::ProjectCheck(
            backend_frontend_typescript::TszProjectProgramCheckError::ExecutionStopped(
                backend_frontend_typescript::TszProjectExecutionStop::Deadline,
            ),
        );
        let terminal = package_authority_terminal(
            target,
            request,
            source,
            ToolchainSelection::ResolvedNative(toolchain),
            PackageAuthorityError::TypeScriptTsz(error),
        );
        let failure =
            backend_library::PackageCompilerFailure::from_package_terminal("entry.ts", &terminal)
                .expect("projection")
                .expect("project-check deadline");
        assert_eq!(failure.kind_tag(), F::NativeProjectCheckDeadline.kind_tag());
    }

    #[test]
    fn typescript_nested_cancellation_is_a_package_cancelled_terminal() {
        use backend_frontend_typescript::{
            TszAuthorityError as E, TszProjectExecutionStop as S, TszProjectProgramCheckError as P,
            TszProjectQuerySessionError as Q,
        };
        let profile =
            LanguageProfile::TypeScript(backend_semantic::vocabulary::TypeScriptSource::TypeScript);
        let source_bytes = "export const x = 1;";
        let request = backend_library::interface::CompilerRequest {
            profile,
            stage: Stage::LowerIr,
            source: source_bytes,
        };
        let source = request_source(request).expect("source authority");
        // No compiler is entered: authority has already reported an exact cooperative stop.
        let toolchain = ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::TypeScriptCompiler,
        };
        let target = backend_version::ContentId::<backend_version::CompilationTargetDomain>::from_canonical_bytes(b"cancelled-ts-target");
        for error in [
            E::ExecutionStopped(S::Cancelled),
            E::ProjectCheck(P::ExecutionStopped(S::Cancelled)),
            E::ProjectCheckerSession(Q::ExecutionStopped(S::Cancelled)),
        ] {
            let terminal = package_authority_terminal(
                target,
                request,
                source,
                toolchain,
                PackageAuthorityError::TypeScriptTsz(error),
            );
            assert!(
                matches!(terminal, CompilerTerminal::PackageCancelled { target: actual, phase: backend_library::interface::PackageCompilePhase::Authority } if actual == target)
            );
            assert!(
                backend_library::PackageCompilerFailure::from_package_terminal(
                    "entry.ts", &terminal
                )
                .expect("terminal projection")
                .is_none(),
                "cancelled authority cannot become a profile refusal"
            );
        }
    }

    #[test]
    fn missing_typescript_host_error_explains_dependencies_and_warm_daemon_restart() {
        let terminal = CompilerTerminal::Toolchain {
            source: backend_library::interface::SourceAuthority {
                identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"const x = 1;"),
                byte_len: 12,
            },
            language: backend_semantic::vocabulary::Language::TypeScript,
            stage: Stage::LowerIr,
            selected: NativeTool::TypeScriptCompiler,
            configured: None,
        };
        let message = PackageSemanticError::Compile {
            path: "eslint.config.mjs".into(),
            terminal: Box::new(terminal),
        }
        .to_string();
        assert!(message.contains("npm or pnpm"), "{message}");
        assert!(message.contains("NUDOX_TSC"), "{message}");
        assert!(
            message.contains("backend-locald keeps the PATH"),
            "{message}"
        );
        assert!(message.contains("stop and restart"), "{message}");
    }

    #[test]
    fn python_configuration_terminal_retains_typed_source_evidence_after_serialization() {
        use backend_frontend_python::legacy::checker::CheckerError;
        use backend_library::PackageCompilerFailure;
        use backend_library::interface::PythonAuthorityFailureKind;
        let error = CheckerError::ProjectConfiguration {
            path: "/private/captured/pyproject.toml".into(),
            source_identity: [37; 32],
            source_byte_len: 8192,
            omitted_faults: 100,
            evidence_truncated: true,
            faults: Box::new([]),
        };
        assert_eq!(
            super::python_authority_failure_kind(&error),
            PythonAuthorityFailureKind::ConfigurationRefused
        );
        let request = super::ApplicationCompilerRequest {
            profile: LanguageProfile::Python(
                backend_semantic::vocabulary::PythonVersion::Python313,
            ),
            stage: Stage::LowerIr,
            source: "value = 1\n",
        };
        let source = request_source(request).unwrap();
        let target = ContentId::<backend_version::CompilationTargetDomain>::from_canonical_bytes(
            b"python-config-refusal",
        );
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Python,
            host_path("/toolchain/bin/python3"),
            b"Python 3.13.0",
        )
        .unwrap();
        let terminal = package_authority_terminal(
            target,
            request,
            source,
            ToolchainSelection::ResolvedNative(toolchain),
            PackageAuthorityError::PythonPyrefly(error),
        );
        let failure = PackageCompilerFailure::from_package_terminal("pkg/module.py", &terminal)
            .unwrap()
            .unwrap();
        let encoded = failure.encode_bounded_json().unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("/private/captured"));
        let reopened = PackageCompilerFailure::decode_bounded_json(&encoded).unwrap();
        assert_eq!(reopened.kind_tag(), "python_configuration_refused");
        let backend_library::PackageCompilerFailureCause::Authority {
            phase,
            class,
            diagnostic: Some(facts),
        } = reopened.cause()
        else {
            panic!("configuration authority")
        };
        assert_eq!(*phase, backend_library::AuthorityPhaseFact::Parse);
        assert_eq!(*class, backend_library::AuthorityClassFact::Authority);
        let configuration = facts.python_configuration.unwrap();
        assert_eq!(configuration.source_identity, [37; 32]);
        assert_eq!(configuration.source_byte_len, 8192);
        assert_eq!(configuration.omitted_faults, 100);
        assert!(configuration.evidence_truncated);
        assert!(reopened.retained_diagnostic_for_local_debug().is_none());
    }

    #[derive(Debug)]
    struct ChainDiagnosticError {
        message: String,
        source: Option<Box<dyn StdError>>,
    }

    impl fmt::Display for ChainDiagnosticError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.message)
        }
    }

    impl StdError for ChainDiagnosticError {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            self.source.as_deref()
        }
    }

    #[test]
    fn bounded_authority_diagnostic_keeps_headline_and_numeric_terminal_cause() {
        let mut source: Box<dyn StdError> = Box::new(ChainDiagnosticError {
            message: "stored semantic-image bytes contain 2048 bytes, expected exactly 4096"
                .to_owned(),
            source: None,
        });
        for ordinal in (0..5).rev() {
            source = Box::new(ChainDiagnosticError {
                message: format!("intermediate {ordinal}: {}", "m".repeat(512)),
                source: Some(source),
            });
        }
        let error = ChainDiagnosticError {
            message: "package semantic output could not be prepared for transport".to_owned(),
            source: Some(source),
        };

        let diagnostic = bounded_error_chain(&error);

        assert!(
            diagnostic
                .text
                .starts_with("package semantic output could not be prepared for transport")
        );
        assert!(
            diagnostic
                .text
                .contains("stored semantic-image bytes contain 2048")
        );
        assert!(diagnostic.text.contains("… intermediate causes omitted …"));
        assert!(diagnostic.truncated);
        assert!(diagnostic.text.len() <= backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn bounded_authority_diagnostic_sanitizes_nul_and_preserves_utf8() {
        let error = ChainDiagnosticError {
            message: format!("résultat\0{}", "🦀".repeat(200)),
            source: None,
        };

        let diagnostic = bounded_error_chain(&error);

        assert!(diagnostic.text.starts_with("résultat "));
        assert!(!diagnostic.text.contains('\0'));
        assert!(diagnostic.text.is_char_boundary(diagnostic.text.len()));
        assert!(diagnostic.text.contains('…'));
        assert!(
            diagnostic.text.ends_with('🦀'),
            "the bounded terminal suffix stays intact"
        );
        assert!(diagnostic.truncated);
        assert!(diagnostic.text.len() <= backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn bounded_authority_diagnostic_keeps_long_terminal_tail_at_exact_utf8_limit() {
        let terminal = format!(
            "{}🧭 expected length 4096 but observed length 2048",
            "x".repeat(600)
        );
        let error = ChainDiagnosticError {
            message: "authority failure for résumé".to_owned(),
            source: Some(Box::new(ChainDiagnosticError {
                message: terminal,
                source: None,
            })),
        };

        let diagnostic = bounded_error_chain(&error);

        assert_eq!(
            diagnostic.text.len(),
            backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES
        );
        assert!(diagnostic.text.starts_with("authority failure for résumé"));
        assert!(diagnostic.text.contains('🧭'));
        assert!(diagnostic.text.ends_with("observed length 2048"));
        assert!(diagnostic.text.is_char_boundary(diagnostic.text.len()));
        assert!(diagnostic.truncated);
    }

    #[derive(Debug)]
    struct CyclicDiagnosticError;

    static CYCLIC_DIAGNOSTIC_ERROR: OnceLock<CyclicDiagnosticError> = OnceLock::new();

    impl fmt::Display for CyclicDiagnosticError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("cyclic package authority error")
        }
    }

    impl StdError for CyclicDiagnosticError {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            CYCLIC_DIAGNOSTIC_ERROR
                .get()
                .map(|error| error as &(dyn StdError + 'static))
        }
    }

    #[test]
    fn bounded_authority_diagnostic_stops_a_cyclic_source_chain() {
        let error = CYCLIC_DIAGNOSTIC_ERROR.get_or_init(|| CyclicDiagnosticError);

        let diagnostic = bounded_error_chain(error);

        assert!(diagnostic.text.contains("error source cycle detected"));
        assert!(diagnostic.truncated);
        assert!(diagnostic.text.len() <= backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn bounded_authority_diagnostic_marks_causes_beyond_its_depth_limit() {
        let mut source: Box<dyn StdError> = Box::new(ChainDiagnosticError {
            message: "cause beyond depth limit".to_owned(),
            source: None,
        });
        for ordinal in (0..=super::MAX_PACKAGE_AUTHORITY_ERROR_CAUSES).rev() {
            source = Box::new(ChainDiagnosticError {
                message: format!("cause {ordinal}"),
                source: Some(source),
            });
        }
        let error = ChainDiagnosticError {
            message: "package authority failed".to_owned(),
            source: Some(source),
        };

        let diagnostic = bounded_error_chain(&error);

        assert!(diagnostic.text.contains("cause 7"));
        assert!(
            diagnostic
                .text
                .contains("additional causes omitted after depth limit")
        );
        assert!(!diagnostic.text.contains("cause beyond depth limit"));
        assert!(diagnostic.truncated);
        assert!(diagnostic.text.len() <= backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn staged_output_diagnostic_exposes_typed_transport_cause_and_keeps_source_chain() {
        let cause = crate::publication::PublishSemanticError::ImageBytesLength {
            expected: 512,
            observed: 256,
        };
        let error = PackageSemanticError::StagedOutput(cause);

        assert_eq!(
            error.to_string(),
            "package semantic output could not be prepared for transport: semantic image bytes contain 256 bytes, expected exactly 512"
        );
        let source = std::error::Error::source(&error).expect("typed publication source");
        assert!(source.is::<crate::publication::PublishSemanticError>());
        let diagnostic = bounded_error_chain(&error);
        assert!(
            diagnostic
                .text
                .contains("semantic image bytes contain 256 bytes")
        );
        assert!(!diagnostic.truncated);
    }

    #[derive(Debug, Error)]
    enum RouteTestError {
        #[error("resolved toolchain fixture was rejected")]
        Resolved(#[from] ToolchainResolutionError),
        #[error("toolchain table fixture was rejected")]
        Table(#[from] LocalToolchainSetError),
        #[error("registry-unavailable route selected a resolved local toolchain")]
        Accepted,
        #[error("registry-unavailable route retained the wrong typed route cause")]
        Route { observed: ToolchainRouteError },
    }

    #[test]
    fn unavailable_registry_route_cannot_execute_a_resolved_local_toolchain()
    -> Result<(), RouteTestError> {
        let selections = [ToolchainSelection::ResolvedNative(
            ResolvedToolchain::from_version(
                NativeTool::GoCompiler,
                Path::new("/caller/probed/go"),
                b"go-version-provenance",
            )?,
        )];
        match select_toolchain(
            LocalToolchainSet::validate(&selections)?,
            AdapterRoute::ToolingUnavailable {
                tool: NativeTool::GoCompiler,
            },
        ) {
            Err(ToolchainRouteError::ToolingUnavailable {
                tool: NativeTool::GoCompiler,
            }) => Ok(()),
            Err(observed) => Err(RouteTestError::Route { observed }),
            Ok(_) => Err(RouteTestError::Accepted),
        }
    }

    #[test]
    fn package_rust_detached_source_keeps_its_typed_scope_diagnostic() {
        let request = super::ApplicationCompilerRequest {
            profile: LanguageProfile::Rust(RustEdition::Rust2021),
            stage: Stage::LowerIr,
            source: "pub fn decode() {}",
        };
        let source = request_source(request).expect("small source has a u32 length");
        let package = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(41),
                profile: request.profile,
                stage: request.stage,
            },
            PackageUrl::parse("pkg:cargo/toml@0.8.23").expect("canonical package URL"),
        )
        .expect("Rust profile matches Cargo package");
        let target = CompilerPackageTargetV2::for_package(package.as_ref().clone()).target();
        let toolchain = ResolvedToolchain::from_version(
            NativeTool::Rustc,
            host_path("/toolchain/bin/rustc"),
            b"rustc 1.90.0",
        )
        .expect("absolute fixture toolchain path");

        let terminal = package_authority_terminal(
            target,
            request,
            source,
            ToolchainSelection::ResolvedNative(toolchain),
            PackageAuthorityError::RustProject(RustAuthorityError::DetachedSource {
                path: Path::new("/cache/toml-0.8.23/examples/decode.rs").to_path_buf(),
                active_hir_roots: backend_frontend_rust::legacy::RustActiveHirRootInventory {
                    package_crate_count: 4,
                    package_relative_roots: Box::new([
                        Path::new("src/lib.rs").to_path_buf(),
                        Path::new("src/bin/tool.rs").to_path_buf(),
                    ]),
                    omitted_package_crates: 2,
                },
            }),
        );
        let CompilerTerminal::Compile {
            cause:
                CompilerCause::Authority {
                    phase,
                    class,
                    diagnostic: Some(diagnostic),
                },
            ..
        } = &terminal
        else {
            panic!("Rust package refusal must keep its concrete authority projection");
        };
        assert_eq!(*phase, AuthorityPhase::Resolve);
        assert_eq!(*class, AuthorityDiagnosticClass::SourceScope);
        assert!(!diagnostic.truncated);
        let message = &diagnostic.bytes[..diagnostic.byte_len];
        assert!(message.starts_with(
            b"selected Rust source is cfg-inactive or detached from every active Cargo target; active package HIR roots: 4 [src/lib.rs, src/bin/tool.rs] (+2 omitted)"
        ));
        assert!(
            !message
                .windows(b"/cache/".len())
                .any(|window| window == b"/cache/")
        );

        let failure = backend_library::PackageCompilerFailure::from_package_terminal(
            "src/detached.rs",
            &terminal,
        )
        .expect("bounded package failure")
        .expect("Rust package refusal");
        let encoded = failure
            .encode_bounded_json()
            .expect("bounded canonical package failure");
        let wire = String::from_utf8(encoded.clone()).expect("failure JSON is UTF-8");
        assert!(!wire.contains("/cache/toml-0.8.23"));
        let reopened = backend_library::PackageCompilerFailure::decode_bounded_json(&encoded)
            .expect("typed package diagnostic survives bounded roundtrip");
        let backend_library::PackageCompilerFailureCause::Authority {
            diagnostic: Some(facts),
            ..
        } = reopened.cause()
        else {
            panic!("Rust package failure retains its typed scope diagnostic");
        };
        assert_eq!(
            facts.rust_source_scope_failure,
            Some(
                backend_library::interface::RustSourceScopeFailureKind::DetachedSource {
                    active_hir_root_count: 4,
                }
            )
        );
        assert!(reopened.detail().contains("4 active roots observed"));
        assert!(!reopened.detail().contains("/cache/toml-0.8.23"));
    }

    #[test]
    fn python_raw_frontier_admission_binds_unavailable_members_and_rejects_mutations() {
        use backend_frontend_python::legacy::checker::PythonProjectBytesSource;
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(1),
                profile: LanguageProfile::Python(
                    backend_semantic::vocabulary::PythonVersion::Python314,
                ),
                stage: Stage::LowerIr,
            },
            PackageUrl::parse("pkg:pypi/intake-fixture@1.0.0").unwrap(),
        )
        .unwrap();
        let root = host_path("/python-raw-frontier");
        let decoded = [PackageSource::new("good.py", "answer = 42\n").unwrap()];
        let raw = [
            PythonProjectBytesSource {
                relative_path: "good.py",
                source: b"answer = 42\n",
            },
            PythonProjectBytesSource {
                relative_path: "latin.py",
                source: b"# coding: latin-1\nvalue = '\xe9'\n",
            },
        ];
        let package = PackageSourceSet::new(&request, root, &decoded)
            .unwrap()
            .with_python_raw_sources(&raw)
            .unwrap();
        assert_eq!(package.python_raw_sources.unwrap().len(), 2);
        let raw_input = super::package_source_input_witness(&package);
        let decoded_input = super::package_source_input_witness(
            &PackageSourceSet::new(&request, root, &decoded).unwrap(),
        );
        assert_ne!(raw_input.input_root(), decoded_input.input_root());
        let mismatched = [PythonProjectBytesSource {
            relative_path: "good.py",
            source: b"changed = 42\n",
        }];
        assert!(matches!(
            PackageSourceSet::new(&request, root, &decoded)
                .unwrap()
                .with_python_raw_sources(&mismatched),
            Err(PackageSourceSetError::RawSourceMismatch)
        ));
        let duplicate = [raw[0], raw[0]];
        assert!(matches!(
            PackageSourceSet::new(&request, root, &decoded)
                .unwrap()
                .with_python_raw_sources(&duplicate),
            Err(PackageSourceSetError::Order)
        ));
    }

    #[test]
    fn rust_crate_units_select_the_exact_root_and_reject_unit_mutations() {
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(1),
                profile: LanguageProfile::Rust(RustEdition::Rust2024),
                stage: Stage::LowerIr,
            },
            PackageUrl::parse("pkg:cargo/workspace@1.0.0")
                .expect("fixture package URL is canonical"),
        )
        .expect("package and profile agree");
        let sources = [
            PackageSource::new("src/a.rs", "pub fn a() {}")
                .expect("first source path is normalized"),
            PackageSource::new("src/b.rs", "pub fn b() {}")
                .expect("second source path is normalized"),
        ];
        let target_a = CompilerPackageTargetV2::new(
            request.as_ref().clone(),
            CompilationUnitKeyV2::RustCrate {
                name: "a".into(),
                root: "src/a.rs".into(),
            },
        )
        .expect("unit A is canonical");
        let target_b = CompilerPackageTargetV2::new(
            request.as_ref().clone(),
            CompilationUnitKeyV2::RustCrate {
                name: "b".into(),
                root: "src/b.rs".into(),
            },
        )
        .expect("unit B is canonical");
        assert_ne!(target_a.target(), target_b.target());

        let unit_a =
            PackageSourceSet::new_for_unit(&request, &target_a, host_path("/workspace"), &sources)
                .expect("unit A is present in the immutable frontier");
        let unit_b =
            PackageSourceSet::new_for_unit(&request, &target_b, host_path("/workspace"), &sources)
                .expect("unit B is present in the immutable frontier");
        assert_eq!(unit_a.embedding_provisioning_failure, None);
        let configured_failure = unit_a
            .clone()
            .with_embedding_provisioning_failure(EmbeddingProvisioningFailure::ModelUnavailable);
        assert_eq!(
            configured_failure.embedding_provisioning_failure,
            Some(EmbeddingProvisioningFailure::ModelUnavailable)
        );
        assert_ne!(
            StagedEmbeddingStatus::NotConfigured,
            StagedEmbeddingStatus::ProvisioningUnavailable {
                cause: EmbeddingProvisioningFailure::ModelUnavailable,
            }
        );
        assert_eq!(
            unit_a
                .compilation_sources()
                .map(|source| source.relative_path())
                .collect::<Vec<_>>(),
            ["src/a.rs"]
        );
        assert_eq!(
            unit_b
                .compilation_sources()
                .map(|source| source.relative_path())
                .collect::<Vec<_>>(),
            ["src/b.rs"]
        );

        let wrong_unit = CompilerPackageTargetV2::new(
            request.as_ref().clone(),
            CompilationUnitKeyV2::CSharpProject {
                project_path: "src/a.csproj".into(),
            },
        )
        .expect("wire unit itself is canonical");
        assert_eq!(
            PackageSourceSet::new_for_unit(
                &request,
                &wrong_unit,
                host_path("/workspace"),
                &sources,
            )
            .err(),
            Some(PackageSourceSetError::CompilationUnitMismatch)
        );
    }

    #[test]
    fn csharp_project_units_select_distinct_source_roots_and_reject_missing_roots() {
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(2),
                profile: LanguageProfile::CSharp(
                    backend_semantic::vocabulary::CSharpVersion::CSharp14,
                ),
                stage: Stage::LowerIr,
            },
            PackageUrl::parse("pkg:nuget/workspace@1.0.0")
                .expect("fixture package URL is canonical"),
        )
        .expect("package and profile agree");
        let sources = [
            PackageSource::new("src/alpha/Alpha.cs", "public sealed class Alpha {}")
                .expect("first source path is normalized"),
            PackageSource::new("src/beta/Beta.cs", "public sealed class Beta {}")
                .expect("second source path is normalized"),
        ];
        let target_alpha = CompilerPackageTargetV2::new(
            request.as_ref().clone(),
            CompilationUnitKeyV2::CSharpProject {
                project_path: "src/alpha/Alpha.csproj".into(),
            },
        )
        .expect("first C# project target is canonical");
        let target_beta = CompilerPackageTargetV2::new(
            request.as_ref().clone(),
            CompilationUnitKeyV2::CSharpProject {
                project_path: "src/beta/Beta.csproj".into(),
            },
        )
        .expect("second C# project target is canonical");
        assert_ne!(target_alpha.target(), target_beta.target());

        let alpha = PackageSourceSet::new_for_unit(
            &request,
            &target_alpha,
            host_path("/workspace"),
            &sources,
        )
        .expect("the first project has a source in its exact directory");
        let beta = PackageSourceSet::new_for_unit(
            &request,
            &target_beta,
            host_path("/workspace"),
            &sources,
        )
        .expect("the second project has a source in its exact directory");
        assert_eq!(
            alpha
                .compilation_sources()
                .map(|source| source.relative_path())
                .collect::<Vec<_>>(),
            ["src/alpha/Alpha.cs"]
        );
        assert_eq!(
            beta.compilation_sources()
                .map(|source| source.relative_path())
                .collect::<Vec<_>>(),
            ["src/beta/Beta.cs"]
        );

        let mutated_target = CompilerPackageTargetV2::new(
            request.as_ref().clone(),
            CompilationUnitKeyV2::CSharpProject {
                project_path: "src/missing/Missing.csproj".into(),
            },
        )
        .expect("the mutated target is itself canonical");
        assert_eq!(
            PackageSourceSet::new_for_unit(
                &request,
                &mutated_target,
                host_path("/workspace"),
                &sources,
            )
            .err(),
            Some(PackageSourceSetError::CompilationUnitMismatch)
        );
    }
}

fn python_project_deadline_projection(
    phase: backend_frontend_python::legacy::checker::PythonProjectPhase,
) -> (
    backend_semantic::vocabulary::AuthorityPhase,
    AuthorityDiagnosticClass,
) {
    use backend_frontend_python::legacy::checker::PythonProjectPhase as P;
    use backend_semantic::vocabulary::AuthorityPhase;
    let phase = match phase {
        P::EnvironmentValidation
        | P::EnvironmentMaterialization
        | P::ProducerValidation
        | P::SourceCapture => AuthorityPhase::Open,
        P::NativeAnalysis => AuthorityPhase::TypeCheck,
        P::ReportProjection => AuthorityPhase::Project,
        P::FinalValidation => AuthorityPhase::Resolve,
    };
    (phase, AuthorityDiagnosticClass::Authority)
}

#[cfg(test)]
mod python_project_phase_tests {
    use super::*;
    use backend_frontend_python::legacy::checker::PythonProjectPhase as P;
    use backend_library::interface::PythonAuthorityFailureKind as F;
    use backend_semantic::vocabulary::AuthorityPhase;

    #[test]
    fn typed_project_deadlines_keep_the_closed_public_kind_and_truthful_authority_phase() {
        for (phase, expected) in [
            (P::EnvironmentValidation, AuthorityPhase::Open),
            (P::EnvironmentMaterialization, AuthorityPhase::Open),
            (P::ProducerValidation, AuthorityPhase::Open),
            (P::SourceCapture, AuthorityPhase::Open),
            (P::NativeAnalysis, AuthorityPhase::TypeCheck),
            (P::ReportProjection, AuthorityPhase::Project),
            (P::FinalValidation, AuthorityPhase::Resolve),
        ] {
            let error = PythonCheckerError::ProjectDeadline { phase };
            let kind = python_authority_failure_kind(&error);
            assert_eq!(kind, F::Deadline);
            assert_eq!(kind.kind_tag(), "python_deadline");
            let cause = PackageAuthorityError::PythonPyrefly(error);
            assert_eq!(
                package_authority_projection(&cause),
                (expected, AuthorityDiagnosticClass::Authority)
            );
            let message = bounded_error_chain(&cause);
            assert!(message.text.contains(&phase.to_string()));
            assert!(!message.truncated);
        }
    }

    #[test]
    fn unavailable_environment_inputs_keep_their_envelope_and_existing_public_kind() {
        for (source, expected) in [
            (
                PythonCheckerError::Deadline { phase: "project" },
                F::Deadline,
            ),
            (
                PythonCheckerError::Cancelled { phase: "project" },
                F::Cancelled,
            ),
            (
                PythonCheckerError::Workspace {
                    source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                },
                F::EnvironmentCaptureRefused,
            ),
        ] {
            let source = std::sync::Arc::new(source);
            let error = PythonCheckerError::EnvironmentCapture {
                source: std::sync::Arc::clone(&source),
            };
            assert_eq!(python_authority_failure_kind(&error), expected);
            let cause = PackageAuthorityError::PythonPyrefly(error);
            assert_eq!(
                package_authority_projection(&cause),
                (AuthorityPhase::Open, AuthorityDiagnosticClass::Authority)
            );
            let PackageAuthorityError::PythonPyrefly(PythonCheckerError::EnvironmentCapture {
                source: retained,
            }) = cause
            else {
                panic!("environment input envelope must remain intact")
            };
            assert!(std::sync::Arc::ptr_eq(&retained, &source));
        }
    }
}

#[cfg(test)]
#[path = "compiler/typescript_membership_control_tests.rs"]
mod typescript_membership_control_tests;

#[cfg(test)]
mod typescript_work_gap_tests {
    use super::{
        CompilerCause, CompilerTerminal, TypeScriptWorkGaps, typescript_stop_phase,
        with_typescript_stop_site,
    };
    use crate::application::compiler_typescript_observation::ObservedStopSite;
    use backend_frontend_typescript::{TszProjectExecutionPhase as P, TszProjectExecutionStop};
    use backend_library::interface::{
        AuthorityDiagnosticClass, AuthorityPhase, CompilerAttempt, CompilerDiagnostic,
        SourceAuthority, TypeScriptAuthorityFailureKind, TypeScriptStopPhase,
    };
    use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain};

    #[test]
    fn work_gaps_are_keyed_by_program_and_exact_path_and_never_cross_programs() {
        let mut gaps = TypeScriptWorkGaps::default();
        assert!(!gaps.contains(0, "src/hot.ts"));
        gaps.insert(1, "src/hot.ts");
        gaps.insert(1, "src/hot.ts");
        assert!(gaps.contains(1, "src/hot.ts"));
        assert!(
            !gaps.contains(0, "src/hot.ts"),
            "another program's file is not this one"
        );
        assert!(!gaps.contains(1, "src/Hot.ts"), "paths compare exactly");
        assert_eq!(gaps.program_paths(1).len(), 1);
        assert!(gaps.program_paths(0).is_empty());
    }

    #[test]
    fn every_native_phase_has_a_distinct_public_phase() {
        let phases = [
            P::SourceFrontier,
            P::ParseBind,
            P::Merge,
            P::FileFormats,
            P::ModuleOutcomes,
            P::CheckerPlan,
            P::LibraryViews,
            P::BinderViews,
            P::OwnershipIndex,
            P::SharedCheckerIndexes,
            P::AffectedLibraries,
            P::AugmentationPrime,
            P::FileReset,
            P::FileCheck,
            P::FilePostprocess,
            P::FileQuery,
            P::SourcePreparation,
            P::SourceStatements,
            P::SourcePostchecks,
            P::LibraryBaseline,
            P::LibraryCheck,
            P::Aggregate,
        ];
        let tags = phases
            .into_iter()
            .map(|phase| typescript_stop_phase(phase).tag())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(tags.len(), phases.len());
        assert_eq!(
            typescript_stop_phase(P::FileCheck),
            TypeScriptStopPhase::FileCheck
        );
    }

    #[test]
    fn preparation_measurements_keep_closed_public_phase_and_failure_authority() {
        use super::super::compiler_typescript_observation::{PreparationTiming, StopMeasurement};
        for phase in [
            P::SourcePreparationFunctionDefs,
            P::SourcePreparationWarmDefinitions,
            P::SourcePreparationHeritage,
            P::SourcePreparationWellKnownSymbols,
            P::SourcePreparationTypeEnvironment,
            P::SourcePreparationReconciliation,
            P::SourcePreparationBoxedTypes,
        ] {
            assert_eq!(
                typescript_stop_phase(phase),
                TypeScriptStopPhase::SourcePreparation
            );
        }
        for phase in [
            P::FileQueryAnalysis,
            P::FileQueryStructure,
            P::FileQueryOccurrences,
            P::FileQueryTypes,
        ] {
            assert_eq!(typescript_stop_phase(phase), TypeScriptStopPhase::FileQuery);
        }
        let terminal = |kind| CompilerTerminal::Compile {
            attempted: CompilerAttempt {
                source: SourceAuthority {
                    identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"x"),
                    byte_len: 1,
                },
                recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"measurement"),
            },
            cause: CompilerCause::Authority {
                phase: AuthorityPhase::TypeCheck,
                class: AuthorityDiagnosticClass::Authority,
                diagnostic: CompilerDiagnostic::from_native(b"native", 6, false)
                    .map(|diagnostic| diagnostic.with_typescript_failure(kind)),
            },
        };
        let site = ObservedStopSite {
            reason: TszProjectExecutionStop::Deadline,
            phase: Some(P::SourcePreparationTypeEnvironment),
            file: Some("src/value.ts".into()),
            work_charged: 4,
            work_allowance: 10,
            measurement: Some(StopMeasurement {
                elapsed: std::time::Duration::from_secs(6),
                started: 3,
                completed: 2,
                merges: 1,
                preparation: Some(PreparationTiming {
                    durations: [Some(std::time::Duration::from_millis(1)); 7],
                    active: Some(4),
                }),
                query: None,
            }),
        };
        for (kind, expected) in [
            (
                TypeScriptAuthorityFailureKind::NativeProjectCheckDeadline,
                true,
            ),
            (TypeScriptAuthorityFailureKind::CheckerExit, false),
        ] {
            let CompilerTerminal::Compile {
                cause:
                    CompilerCause::Authority {
                        diagnostic: Some(facts),
                        ..
                    },
                ..
            } = with_typescript_stop_site(terminal(kind), Some(&site))
            else {
                panic!("same typed refusal");
            };
            assert_eq!(facts.typescript_failure, Some(kind));
            assert_eq!(facts.typescript_cause.is_some(), expected);
            assert_eq!(facts.typescript_stop_site.is_some(), expected);
            if expected {
                assert_eq!(
                    facts.typescript_stop_site.as_ref().unwrap().phase,
                    Some(TypeScriptStopPhase::SourcePreparation)
                );
                let cause = facts.typescript_cause.as_ref().unwrap();
                assert!(cause.as_str().contains("active=type"));
                assert!(
                    !cause.as_str().contains("src/value.ts"),
                    "only the existing typed stop site carries the exact source path"
                );
                assert!(
                    backend_library::interface::TypeScriptFailureCause::new(cause.as_str())
                        .is_some()
                );
            }
        }
        let prior =
            backend_library::interface::TypeScriptFailureCause::new("previous retained cause")
                .unwrap();
        let mut previous = terminal(TypeScriptAuthorityFailureKind::NativeProjectCheckDeadline);
        if let CompilerTerminal::Compile {
            cause: CompilerCause::Authority { diagnostic, .. },
            ..
        } = &mut previous
        {
            *diagnostic = diagnostic
                .take()
                .map(|diagnostic| diagnostic.with_typescript_cause(prior.clone()));
        }
        let CompilerTerminal::Compile {
            cause:
                CompilerCause::Authority {
                    diagnostic: Some(retained),
                    ..
                },
            ..
        } = with_typescript_stop_site(previous, Some(&site))
        else {
            panic!("same existing cause");
        };
        assert_eq!(
            retained.typescript_cause.as_ref(),
            Some(&prior),
            "advisory timings never replace an existing cause"
        );
    }

    #[test]
    fn the_observed_stop_site_rides_the_authority_refusal_and_nothing_else() {
        let terminal = |kind| CompilerTerminal::Compile {
            attempted: CompilerAttempt {
                source: SourceAuthority {
                    identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"x"),
                    byte_len: 1,
                },
                recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"site"),
            },
            cause: CompilerCause::Authority {
                phase: AuthorityPhase::TypeCheck,
                class: AuthorityDiagnosticClass::Authority,
                diagnostic: CompilerDiagnostic::from_native(b"native", 6, false)
                    .map(|diagnostic| diagnostic.with_typescript_failure(kind)),
            },
        };
        let site = ObservedStopSite {
            reason: TszProjectExecutionStop::WorkBudgetExhausted,
            phase: Some(P::SourceStatements),
            file: Some("src/app/index.tsx".into()),
            work_charged: 9,
            work_allowance: 9,
            measurement: None,
        };
        let CompilerTerminal::Compile {
            cause:
                CompilerCause::Authority {
                    diagnostic: Some(named),
                    ..
                },
            ..
        } = with_typescript_stop_site(
            terminal(TypeScriptAuthorityFailureKind::NativeProjectCheckWorkBudgetExhausted),
            Some(&site),
        )
        else {
            panic!("authority refusal keeps its shape");
        };
        let facts = named.typescript_stop_site.as_ref().expect("site attached");
        assert_eq!(facts.phase, Some(TypeScriptStopPhase::SourceStatements));
        assert_eq!(facts.file.as_deref(), Some("src/app/index.tsx"));
        assert_eq!((facts.work_charged, facts.work_allowance), (9, 9));
        // A stop that observed nothing, or a failure that is not a program stop, gains no site.
        for unchanged in [
            with_typescript_stop_site(
                terminal(TypeScriptAuthorityFailureKind::NativeProjectCheckWorkBudgetExhausted),
                None,
            ),
            with_typescript_stop_site(
                terminal(TypeScriptAuthorityFailureKind::CheckerExit),
                Some(&site),
            ),
        ] {
            let CompilerTerminal::Compile {
                cause:
                    CompilerCause::Authority {
                        diagnostic: Some(plain),
                        ..
                    },
                ..
            } = unchanged
            else {
                panic!("authority refusal keeps its shape");
            };
            assert!(plain.typescript_stop_site.is_none());
        }
    }
}

#[cfg(test)]
mod staged_identity_recipe_tests {
    use super::*;
    use backend_semantic::vocabulary::{
        CompileRecipeFact, LanguageProfile, Stage, TypeScriptSource,
    };

    #[test]
    fn project_recipe_admission_is_exact_and_foreign_toolchains_still_refuse() {
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let runtime = AdmittedStagedRuntime {
            target: ContentId::from_canonical_bytes(b"exact target"),
            profile,
            stage: Stage::LowerIr,
            tool: NativeTool::TypeScriptCompiler,
            toolchain: [41; 32],
            environment: [42; 32],
            target_platform: [43; 32],
        };
        let recipe = CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            ContentId::from_canonical_bytes(b"exact source bytes"),
            ContentId::from_digest([41; 32]),
        );
        assert_eq!(runtime.admit_recipe(0, recipe), Ok(()));
        let build = runtime.build_identity([44; 32], recipe);
        assert_eq!(
            build.recipe(),
            recipe.identity.as_ref(),
            "per-image source-bound recipe is preserved"
        );
        assert_eq!(build.environment(), &runtime.environment);
        let foreign = AdmittedStagedRuntime {
            toolchain: [91; 32],
            ..runtime
        };
        assert_eq!(
            foreign.admit_recipe(7, recipe),
            Err(StagedIdentityFacet::ArtifactToolchain {
                artifact: 7,
                expected: [91; 32],
                actual: [41; 32],
            }),
            "portable and local constraints use this same strict recipe admission"
        );
        let changed_source = CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            ContentId::from_canonical_bytes(b"different source bytes"),
            recipe.toolchain,
        );
        assert_ne!(
            runtime.build_identity([44; 32], changed_source).recipe(),
            build.recipe()
        );
    }
}
