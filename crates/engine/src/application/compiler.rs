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
    CompiledFragment, DeclarationScope, PackageDeclarationScopeFault, ToolchainSelection,
    compile_semantic as compile_fused_semantic, rust_authority_diagnostic,
};
use crate::publication::{
    OpenSemanticPublicationScratch, PreparedSemanticOutput, PublishControl, PublishedCompilation,
    SemanticImageArtifactFacts, SemanticPublicationScratch, StagedSemanticObjectClaim,
    open_published_semantic, open_semantic_generation, prepare_semantic_bytes,
    publish_semantic_bytes, semantic_generation_requirements,
};
use backend_compile::{
    EmbeddingCacheSession, EmbeddingCoordinates, EmbeddingExecutable, EmbeddingExecutionIdentity,
    EmbeddingNormalization, EmbeddingPurpose,
};
use backend_frontend_go::legacy::oracle::GoPackageAuthorityWitness;
use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustAuthorityError, RustWorkspaceEditorBufferObserver, RustWorkspaceFile,
    RustWorkspaceReadFrontierObserver, RustWorkspaceSessionKey, RustWorkspaceSessionLane,
    RustWorkspaceSessionLease,
};
use backend_library::interface::{
    CompilerAttempt, CompilerCapability, CompilerCause, CompilerReadiness,
    CompilerRequest as ApplicationCompilerRequest, CompilerTerminal, FragmentCause,
    GeneratedArtifact, PackageCompilePhase, PackageCompileRequest, PackageDeclarationScopeCause,
    PackageSourceCause, PublicationAuthority, PublicationCause, PublicationPhase,
    SemanticImageAccessError, SemanticImageAuthority, SemanticImageSnapshot, SourceAuthority,
};
use backend_semantic::ir::{
    EmbeddingPlaneIdentity, MAX_SEMANTIC_SEGMENT_BYTES, SemanticBuildIdentity,
    SemanticImageReopenError, SemanticImageView, SemanticInputWitness, SemanticIrPlane,
    SemanticManifestError, SemanticPlane, SemanticPlaneKind, SemanticPlaneManifest,
    SemanticPlaneSegment, SemanticSegmentId,
};
use backend_semantic::registry::{AdapterRoute, FullRegistry};
use backend_semantic::vocabulary::AuthorityDiagnosticClass;
use backend_store::journal::{
    DurablePublisher, PublicationLimits, PublicationPaths, ShutdownError,
};
use backend_version::Coverage;
use backend_version::ScopeRoot;
use backend_version::{
    ArtifactId, ContentId, IrFragmentDomain, IrFragmentEncoding, SourceFactDomain,
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use thiserror::Error;

use crate::application::executor::StagedOutputLease;
use crate::application::package_authority::{
    enter_package_authority_with_go_authority_witness, enter_package_authority_with_rust_workspace,
};
use crate::application::{
    LocalCompilerConfig, LocalCompilerControl, LocalCompilerExecutionIdentity,
    LocalCompilerOpenError, LocalCompilerPath, LocalCompilerPlaneExecutionIdentity,
    LocalCompilerPlaneExecutionSeed, LocalCompilerScratch, LocalPackageRootSet,
    PackageAuthorityConfiguration, PackageAuthorityError, PackageAuthorityRequest,
    enter_package_authority, package_source,
    terminal::{compile_terminal, source_authority},
};

pub(crate) const MAX_PACKAGE_FRAGMENT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_PACKAGE_SEMANTIC_BYTES: usize = 512 * 1024 * 1024;
/// Maximum optional embedding payload bytes retained for one staged package.
pub const MAX_PACKAGE_EMBEDDING_BYTES: usize = 64 * 1024 * 1024;

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
    Available { identity: EmbeddingPlaneIdentity },
    /// Optional inference failed; IR remains usable and the plane has unavailable coverage.
    Unavailable {
        identity: EmbeddingPlaneIdentity,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageSourceCoverageGapCause {
    /// Rust-analyzer found the source in the package VFS but outside every active Cargo target.
    RustSourceOutsideActiveCargoTarget,
}

/// Exact source retained in the input frontier but omitted from semantic lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageSourceCoverageGap {
    source: SourceAuthority,
    relative_path: Box<str>,
    cause: PackageSourceCoverageGapCause,
}

impl PackageSourceCoverageGap {
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
        let path = std::path::Path::new(relative_path);
        let normalized = !relative_path.is_empty()
            && !relative_path.contains('\\')
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)));
        if !normalized {
            return Err(PackageSourceSetError::InvalidPath);
        }
        Ok(Self {
            relative_path,
            source,
        })
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
    package_root: &'source std::path::Path,
    sources: &'source [PackageSource<'source>],
    input_claim: Option<SemanticInputWitness>,
    embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
    go_authority_witness: Option<&'source GoPackageAuthorityWitness>,
}

impl<'source> PackageSourceSet<'source> {
    /// Admits a complete, strictly ordered source frontier for one package.
    ///
    /// # Errors
    /// Returns an error for a relative package root or an empty, oversized,
    /// duplicate, or unordered source frontier.
    pub fn new(
        request: &'source PackageCompileRequest,
        package_root: &'source std::path::Path,
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
        package_root: &'source std::path::Path,
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
            input_claim: None,
            embedding_provisioning_failure: None,
            go_authority_witness: None,
        })
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
    for source in package.compilation_sources() {
        let path = source.relative_path().as_bytes();
        let contents = source.source().as_bytes();
        input.update(&(path.len() as u64).to_be_bytes());
        input.update(path);
        input.update(&(contents.len() as u64).to_be_bytes());
        input.update(contents);
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
    })
}

/// Package-source-frontier admission failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PackageSourceSetError {
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

/// Rejection while binding bounded versioned-plane metadata to a staged package.
#[derive(Debug, Error)]
pub enum StagedVersionedPlaneError {
    /// The staged result has neither portable nor host-local admitted runtime identity.
    #[error("semantic-plane output requires an admitted compiler runtime identity")]
    ExecutionIdentityUnavailable,
    /// The runtime identity does not govern the exact staged package target/profile/stage.
    #[error("semantic-plane identity does not match the staged compiler target")]
    ExecutionIdentityMismatch,
    /// One canonical semantic image was empty and cannot form a nonempty segment range.
    #[error("semantic image {artifact_ordinal} is empty")]
    EmptySemanticImage { artifact_ordinal: usize },
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

impl StagedSemanticPackage {
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

    /// Builds one exact per-image manifest over bounded ranges of the canonical IR bytes.
    ///
    /// The portable runtime identity is preferred when available. Local-only authorities use a
    /// distinct host-scoped identity which commits the exact input witness but cannot be promoted
    /// to worker portability. A source-only witness remains Partial because it does not prove
    /// config files, negative reads, or ambient-read completeness. Payload bytes are borrowed from
    /// the existing canonical image buffer, so creating a range bundle does not copy a full image.
    pub fn versioned_planes(&self) -> Result<StagedVersionedPlanes<'_>, StagedVersionedPlaneError> {
        if self
            .plane_execution_identity
            .is_some_and(|identity| identity.input_witness() != self.input)
        {
            return Err(StagedVersionedPlaneError::ExecutionIdentityMismatch);
        }
        let (target, profile, stage, toolchain, environment, target_platform, recipe) =
            if let Some(identity) = self.execution_identity {
                let recipe = identity.invocation_recipe();
                (
                    identity.target(),
                    identity.profile(),
                    identity.stage(),
                    identity.toolchain_identity(),
                    identity.environment_identity(),
                    identity.target_platform_identity(),
                    *recipe.identity().as_ref(),
                )
            } else if let Some(identity) = self.plane_execution_identity {
                (
                    identity.target(),
                    identity.profile(),
                    identity.stage(),
                    identity.toolchain_identity(),
                    identity.environment_identity(),
                    identity.target_platform_identity(),
                    identity.recipe_identity().as_bytes(),
                )
            } else {
                return Err(StagedVersionedPlaneError::ExecutionIdentityUnavailable);
            };
        if target != self.target_identity || profile != self.profile || stage != self.stage {
            return Err(StagedVersionedPlaneError::ExecutionIdentityMismatch);
        }
        let build = SemanticBuildIdentity::new(
            self.package_identity,
            *target.as_ref(),
            profile,
            stage,
            recipe,
            toolchain,
            environment,
            target_platform,
        );
        let input = self.input;
        let mut output = Vec::new();
        output
            .try_reserve_exact(self.artifacts.len())
            .map_err(StagedVersionedPlaneError::Allocation)?;

        for artifact_ordinal in 0..self.artifacts.len() {
            let input_ordinal = *self
                .prepared
                .canonical_ordinals
                .get(artifact_ordinal)
                .ok_or(StagedVersionedPlaneError::ExecutionIdentityMismatch)?;
            let region = *self
                .staged
                .image_plan
                .get(input_ordinal)
                .ok_or(StagedVersionedPlaneError::ExecutionIdentityMismatch)?;
            let image = region
                .bytes(&self.staged.semantic_images)
                .ok_or(StagedVersionedPlaneError::ExecutionIdentityMismatch)?;
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
                        return Err(StagedVersionedPlaneError::ExecutionIdentityMismatch);
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
        let bytes = match ordinal {
            0 => self.prepared.manifest_bytes.as_ref(),
            value if value % 2 == 1 => {
                let artifact_ordinal = value / 2;
                let artifact = *self.prepared.canonical_ordinals.get(artifact_ordinal)?;
                self.staged.artifacts.get(artifact)?.fragment.as_ref()
            }
            value => {
                let artifact_ordinal = value / 2 - 1;
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
    /// A Go package's authority filesystem inputs could not be admitted.
    #[error(transparent)]
    GoAuthorityWitness(#[from] backend_frontend_go::legacy::oracle::GoPackageAuthorityWitnessError),
    /// One package-relative declaration scope was rejected.
    #[error("package declaration scope is malformed for {path}")]
    Scope {
        /// Rejected normalized source path.
        path: Box<str>,
    },
    /// One authority, toolchain, lowering, or cancellation terminal occurred.
    #[error("package semantic compilation failed for {path}: {terminal:?}")]
    Compile {
        /// Source member that reached the terminal.
        path: Box<str>,
        /// Exact closed compiler terminal.
        terminal: Box<CompilerTerminal>,
    },
    /// Required embedding inference could not produce a complete bounded output plane.
    #[error("required package embedding failed for {path}: {cause}")]
    Embedding {
        /// Source member that failed embedding admission.
        path: Box<str>,
        /// Exact embedding runtime or output-bound failure.
        cause: Box<str>,
    },
    /// A checked size computation overflowed or exceeded the package budget.
    #[error("package semantic publication exceeds its {lane} byte budget")]
    Capacity {
        /// Bounded output lane.
        lane: &'static str,
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
    #[error("package semantic publication failed")]
    Publish(#[source] crate::publication::PublishSemanticError),
    /// A canonical compiler output failed preparation before any local publication.
    #[error("package semantic output could not be prepared for transport")]
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
    embeddings: Option<StagedEmbeddingOutput>,
    embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
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
}

impl<'path, 'cancel> LocalCompilerExecution<'path, 'cancel> {
    pub(crate) fn with_native_work_directory(mut self, path: PathBuf) -> Self {
        self.native_work_directory = path.into_boxed_path();
        self
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
        execution_identity: Option<LocalCompilerExecutionIdentity>,
        plane_execution_seed: Option<LocalCompilerPlaneExecutionSeed>,
        embedding_runtime: Option<&EmbeddingExecutable>,
        embedding_cache_session: Option<&EmbeddingCacheSession>,
        embedding_requirement: EmbeddingRequirement,
        scratch: &mut LocalCompilerScratch,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(PackageCompilePhase),
    ) -> Result<StagedPackageCompilation, PackageSemanticError> {
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
        let control =
            self.compile_control(cancelled)
                .map_err(|timeout| PackageSemanticError::Compile {
                    path: first_source.relative_path.into(),
                    terminal: Box::new(CompilerTerminal::DeadlineConstruction {
                        source: first_authority,
                        language: target.profile.language(),
                        stage: target.stage,
                        timeout: *timeout,
                    }),
                })?;
        let source_count = package.compilation_sources().count();
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
                            PackageAuthorityError::RustProject(
                                backend_frontend_rust::legacy::RustAuthorityError::SourceBudget {
                                    actual: u64::try_from(source.source.len()).unwrap_or(u64::MAX),
                                    maximum: configuration.maximum_source_bytes,
                                },
                            ),
                        )),
                    });
                }
            }
        }
        if (embedding_runtime.is_none() || package.embedding_provisioning_failure.is_some())
            && embedding_requirement == EmbeddingRequirement::Required
        {
            return Err(PackageSemanticError::Embedding {
                path: first_source.relative_path.into(),
                cause: "required embedding runtime is not configured".into(),
            });
        }
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
        let rust_workspace_lease =
            if let backend_semantic::vocabulary::LanguageProfile::Rust(edition) = target.profile {
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
                let (toolchain_identity, environment_identity, local_authority_identity) =
                    execution_identity
                        .filter(|identity| {
                            identity.target() == package.package_target.target()
                                && identity.profile() == target.profile
                                && identity.stage() == target.stage
                        })
                        .map(|identity| {
                            (
                                Some(identity.toolchain_identity()),
                                Some(identity.environment_identity()),
                                Some(identity.local_authority_fingerprint()),
                            )
                        })
                        .or_else(|| {
                            plane_execution_seed
                                .filter(|identity| {
                                    identity.target() == package.package_target.target()
                                        && identity.profile() == target.profile
                                        && identity.stage() == target.stage
                                })
                                .map(|identity| {
                                    (
                                        Some(identity.toolchain_identity()),
                                        Some(identity.environment_identity()),
                                        Some(identity.local_authority_fingerprint()),
                                    )
                                })
                        })
                        .unwrap_or((None, None, None));
                let source_paths = package
                    .sources
                    .iter()
                    .map(|source| PathBuf::from(source.relative_path))
                    .collect::<Vec<_>>();
                let key = RustWorkspaceSessionKey::new(
                    package.package_root,
                    authority_configuration.toolchain,
                    edition,
                    target.stage,
                    authority_configuration.features,
                    toolchain_identity,
                    environment_identity,
                    local_authority_identity,
                    *package.package_target.target().as_ref(),
                    &source_paths,
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

        for source in package.compilation_sources() {
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
            let toolchain = self
                .toolchain(application_request)
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
            let transient_authority = if rust_workspace_authority.is_none() {
                Some(
                    enter_package_authority_with_go_authority_witness(
                        PackageAuthorityRequest {
                            package_root: package.package_root,
                            source_path: &source_path,
                            source: source.source.as_bytes(),
                            unit_key: package.package_target.unit_key(),
                            profile: target.profile,
                            toolchain,
                            control,
                            configuration: self.package_authority,
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
            let authority_owner = rust_workspace_authority
                .as_ref()
                .or(transient_authority.as_ref())
                .ok_or(PackageSemanticError::Capacity {
                    lane: "package authority owner",
                })?;
            let authority = authority_owner.input(&source_path);
            let compiled = match self.stage_prepared(
                application_request,
                source_authority,
                scope,
                toolchain,
                authority,
                control,
                &mut scratch.diagnostic_output,
                &mut scratch.fragment_output,
                progress,
            ) {
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
                    });
                    continue;
                }
                Err(terminal) => {
                    return Err(PackageSemanticError::Compile {
                        path: source.relative_path.into(),
                        terminal: Box::new(terminal),
                    });
                }
            };
            let fragment_length = compiled.fragment.len();
            fragment_bytes = checked_package_bytes(
                fragment_bytes,
                fragment_length,
                MAX_PACKAGE_FRAGMENT_BYTES,
                "compact fragment",
            )?;
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
                            match stage_embedding_artifact(identity, relative_path, coordinates) {
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
            plane_execution_identity: plane_execution_seed.map(|seed| seed.bind_input(input)),
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
}

impl<'path, 'scratch, 'cancel> LocalCompiler<'path, 'scratch, 'cancel> {
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
        let count = staged.artifacts.len();
        if count == 0 {
            return Err(PackageSemanticError::Capacity { lane: "manifest" });
        }
        let mut fragment_bytes = 0_usize;
        for artifact in &staged.artifacts {
            fragment_bytes = checked_package_bytes(
                fragment_bytes,
                artifact.fragment.len(),
                MAX_PACKAGE_FRAGMENT_BYTES,
                "compact fragment",
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
            fragment_bytes = checked_package_bytes(
                fragment_bytes,
                artifact.fragment.len(),
                MAX_PACKAGE_FRAGMENT_BYTES,
                "compact fragment",
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
        .map_err(PackageSemanticError::StagedOutput)?;
        drop(compiled);

        let required_claims = count
            .checked_mul(2)
            .and_then(|pairs| pairs.checked_add(1))
            .ok_or(PackageSemanticError::Capacity {
                lane: "generation claims",
            })?;
        if prepared.object_claims.len() != required_claims
            || prepared.canonical_ordinals.len() != count
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
            .try_reserve_exact(count)
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
            let Some(fragment_claim) = prepared
                .object_claims
                .get(1 + canonical_ordinal * 2)
                .copied()
            else {
                return Err(PackageSemanticError::Capacity {
                    lane: "generation claims",
                });
            };
            let Some(semantic_claim) = prepared
                .object_claims
                .get(2 + canonical_ordinal * 2)
                .copied()
            else {
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
            .map_err(|_| CompilerTerminal::Compile {
                attempted: CompilerAttempt {
                    source,
                    recipe: recipe.identity,
                },
                cause: CompilerCause::Fragment(FragmentCause::Validate),
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

    /// Compiles every member of one admitted package frontier and publishes
    /// the resulting compact fragments and semantic images as one generation.
    ///
    /// Each language authority enters with the complete package root while
    /// retaining the member's exact path. Publication becomes visible only
    /// after all members lower successfully, and returned images are copied
    /// only from the owner-reopened immutable closure.
    ///
    /// # Errors
    /// Returns the first exact authority, compilation, capacity, publication,
    /// reopen, or image-admission failure. No partial package generation is
    /// returned.
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
            None,
            None,
            None,
            None,
            EmbeddingRequirement::Optional,
            self.scratch,
            cancelled,
            progress,
        )?;
        self.publish_staged_package(staged, cancelled, progress)
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
            None,
            None,
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

fn copy_bytes(bytes: &[u8]) -> Result<Box<[u8]>, PackageSemanticError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(PackageSemanticError::Allocation)?;
    owned.extend_from_slice(bytes);
    Ok(owned.into_boxed_slice())
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
        PackageAuthorityError::Deadline { .. } => compiler_attempt_terminal(
            request,
            source,
            toolchain,
            backend_library::interface::CompilerCause::DeadlineExceeded { diagnostic: None },
        ),
        PackageAuthorityError::RustProject(cause) => {
            // Package authority failures happen before driver scratch is leased.
            // Reuse the driver's typed Rust projection with a fixed, path-free
            // diagnostic buffer instead of flattening every Rust failure into
            // Resolve/Authority with no explanation.
            let mut diagnostic_bytes =
                [0; backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES];
            let diagnostic = rust_authority_diagnostic(Some(&mut diagnostic_bytes), &cause, false);
            let failure = AuthorityFailure::Rust { diagnostic, cause };
            let projection = failure.projection();
            let diagnostic = backend_library::interface::CompilerDiagnostic::from_native(
                projection.diagnostic.primary,
                projection.diagnostic.observed,
                projection.diagnostic.truncated,
            );
            compiler_attempt_terminal(
                request,
                source,
                toolchain,
                backend_library::interface::CompilerCause::Authority {
                    phase: projection.phase,
                    class: projection.class,
                    diagnostic,
                },
            )
        }
        cause => {
            let (phase, class) = package_authority_projection(&cause);
            compiler_attempt_terminal(
                request,
                source,
                toolchain,
                backend_library::interface::CompilerCause::Authority {
                    phase,
                    class,
                    diagnostic: None,
                },
            )
        }
    }
}

fn compiler_attempt_terminal(
    request: ApplicationCompilerRequest<'_>,
    source: SourceAuthority,
    toolchain: ToolchainSelection<'_>,
    cause: backend_library::interface::CompilerCause,
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
        resolved.identity,
    );
    CompilerTerminal::Compile {
        attempted: backend_library::interface::CompilerAttempt {
            source,
            recipe: recipe.identity,
        },
        cause,
    }
}

const fn package_authority_projection(
    cause: &PackageAuthorityError,
) -> (
    backend_semantic::vocabulary::AuthorityPhase,
    backend_semantic::vocabulary::AuthorityDiagnosticClass,
) {
    use backend_semantic::vocabulary::{
        AuthorityDiagnosticClass as Class, AuthorityPhase as Phase,
    };

    match cause {
        PackageAuthorityError::SourceOutsidePackage { .. }
        | PackageAuthorityError::TypeScriptSourcePath { .. }
        | PackageAuthorityError::CompilationUnitMismatch { .. }
        | PackageAuthorityError::CompilationUnitSourceMismatch { .. }
        | PackageAuthorityError::RustToolchainExecutableMismatch { .. }
        | PackageAuthorityError::ClangToolchainExecutableMismatch { .. }
        | PackageAuthorityError::GoAuthorityInputsChanged { .. }
        | PackageAuthorityError::ClangProject(_) => (Phase::Open, Class::Binding),
        PackageAuthorityError::PythonSyntax(_) => (Phase::Parse, Class::Syntax),
        PackageAuthorityError::PythonPyrefly(_) => (Phase::TypeCheck, Class::Type),
        PackageAuthorityError::RustProject(_) | PackageAuthorityError::GoOracle(_) => {
            (Phase::Resolve, Class::Authority)
        }
        PackageAuthorityError::CSharp(_) => (Phase::TypeCheck, Class::Authority),
        PackageAuthorityError::TypeScript(_) => (Phase::TypeCheck, Class::Authority),
        PackageAuthorityError::JavaHarness(_)
        | PackageAuthorityError::GoAuthorityWitness(_)
        | PackageAuthorityError::ImageTooLarge { .. }
        | PackageAuthorityError::ToolchainUnavailable { .. }
        | PackageAuthorityError::Cancelled { .. }
        | PackageAuthorityError::Deadline { .. }
        | PackageAuthorityError::AdapterUnavailable { .. } => (Phase::Open, Class::Authority),
    }
}

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
    Missing {
        selected: backend_semantic::vocabulary::NativeTool,
    },
    ToolingUnavailable {
        tool: backend_semantic::vocabulary::NativeTool,
    },
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
    use std::{
        path::Path,
        sync::atomic::{AtomicBool, Ordering},
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
    use backend_frontend_rust::legacy::RustAuthorityError;

    use super::{
        CompilerTerminal, EmbeddingProvisioningFailure, PackageSource, PackageSourceSet,
        PackageSourceSetError, StagedEmbeddingStatus, ToolchainRouteError,
        package_authority_terminal, request_source, select_toolchain,
    };
    use crate::compiler_input_manifest_v2::{CompilationUnitKeyV2, CompilerPackageTargetV2};

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
            Path::new("/toolchain/bin/rustc"),
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
                active_hir_roots:
                    backend_frontend_rust::legacy::RustActiveHirRootInventory::default(),
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
        } = terminal
        else {
            panic!("Rust package refusal must keep its concrete authority projection");
        };
        assert_eq!(phase, AuthorityPhase::Resolve);
        assert_eq!(class, AuthorityDiagnosticClass::SourceScope);
        assert!(!diagnostic.truncated);
        let message = &diagnostic.bytes[..diagnostic.byte_len];
        assert_eq!(
            message,
            b"selected Rust source is cfg-inactive or detached from every active Cargo target."
        );
        assert!(
            !message
                .windows(b"/cache/".len())
                .any(|window| window == b"/cache/")
        );
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
            PackageSourceSet::new_for_unit(&request, &target_a, Path::new("/workspace"), &sources)
                .expect("unit A is present in the immutable frontier");
        let unit_b =
            PackageSourceSet::new_for_unit(&request, &target_b, Path::new("/workspace"), &sources)
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
                Path::new("/workspace"),
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
            Path::new("/workspace"),
            &sources,
        )
        .expect("the first project has a source in its exact directory");
        let beta = PackageSourceSet::new_for_unit(
            &request,
            &target_beta,
            Path::new("/workspace"),
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
                Path::new("/workspace"),
                &sources,
            )
            .err(),
            Some(PackageSourceSetError::CompilationUnitMismatch)
        );
    }
}
