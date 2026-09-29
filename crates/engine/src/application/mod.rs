//! The `backend-engine` application module binds application requests to native compilation and durable publication.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Concrete, explicitly configured local compiler and durable-publication capability.

mod cluster_coordinator;
mod compiler;
mod config;
mod documentation;
mod embedding_provision;
mod executor;
mod host;
mod package_authority;
mod package_source;
mod runtime;
mod terminal;
mod toolchain_probe;
mod unit_authority_v2;

pub use self::cluster_coordinator::{
    AdmittedRemoteCompilerCandidate, AdmittedSemanticInputWitnessV2, CheckedRemoteCompilerArtifact,
    CheckedRemoteCompilerOutput, CheckedRemoteCompilerPlane, CheckedRemoteCompilerPlaneArtifact,
    CheckedRemoteCompilerPlaneDescriptor, CheckedRemoteCompilerPlaneSegment,
    CompilerClusterCoordinator, CompilerInputAdmissionError, CompilerInputAdmissionEvidence,
    CompilerInputAdmissionVerifier, CompilerResultClosureIndex, CompilerResultEnvelopeSchema,
    CompilerResultEnvelopeV1, CompilerResultError, CompilerResultMemberRole,
    CompilerResultMemberV1, CompilerResultOutputClaim, CompilerResultOutputSchema,
    CompilerTrustedExecutionGrant, VerifiedCompilerInputAdmission, admit_remote_compiler_candidate,
    compiler_result_auxiliary_output_claim, compiler_result_auxiliary_output_object,
    compiler_result_envelope_object_from_members,
    compiler_result_envelope_object_from_members_with_planes,
    compiler_result_envelope_object_from_members_with_versioned_planes,
    compiler_result_output_claim, compiler_result_output_object,
    compiler_result_typed_object_claim, compiler_result_versioned_plane_output_claims,
    readmit_remote_compiler_candidate, reopen_compiler_result_envelope,
};
pub use self::compiler::{
    ActivatedSemanticPackage, EmbeddingProvisioningFailure, EmbeddingRequirement, LocalCompiler,
    MAX_PACKAGE_EMBEDDING_BYTES, PackageSemanticError, PackageSource, PackageSourceCoverageGap,
    PackageSourceCoverageGapCause, PackageSourceSet, PackageSourceSetError,
    PublishedSemanticPackage, StagedEmbeddingStatus, StagedSemanticArtifact,
    StagedSemanticOutputObject, StagedSemanticPackage, StagedSemanticReaderError,
    StagedSemanticReaderMetrics, StagedVersionedPlaneArtifact, StagedVersionedPlaneError,
    StagedVersionedPlaneSegment, StagedVersionedPlanes,
};
pub use self::config::{
    LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch, LocalCompilerScratchError,
    LocalCompilerTimeout, LocalCompilerTimeoutError, LocalPackageRoot, LocalPackageRootError,
    LocalPackageRootFacts, LocalPackageRootSet, LocalPackageRootSetError, LocalToolchainSet,
    LocalToolchainSetError, MAX_FRAGMENT_OUTPUT_BYTES, MAX_LOCAL_COMPILER_TIMEOUT,
    MAX_LOCAL_PACKAGE_ROOTS, MAX_LOCAL_TOOLCHAINS, MAX_LOCALITY_OUTPUT_BYTES, MAX_MANIFEST_ENTRIES,
    MAX_MANIFEST_OUTPUT_BYTES,
};
pub use self::documentation::{
    CanonicalDocumentationEntities, DocumentationEntity, DocumentationEntityView,
    DocumentationFragment, DocumentationFragments, DocumentationMembers,
    DocumentationProjectionError, DocumentationReference, DocumentationRelation,
    DocumentationRelations, DocumentationSession, DocumentationSessionView, DocumentationTarget,
    DocumentationTextPart, DocumentationType, DocumentationTypeView,
};
pub use self::embedding_provision::{
    EmbeddingRuntimeError, EmbeddingRuntimeInstall, EmbeddingRuntimeLimits,
    EmbeddingRuntimeProvision, EmbeddingRuntimeStatus, EmbeddingRuntimeSummary,
    EmbeddingUnavailableReason, embedding_runtime_resident_credit_bytes, inspect_embedding_runtime,
    install_embedding_runtime, remove_embedding_runtime,
};
pub use self::host::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostDirectory, LocalHostDiscovery,
    LocalHostEnvironment, LocalHostPathKind, LocalHostPathRole, LocalHostVariable,
    ProcessHostEnvironment, WorkspaceCompilerEnvironment,
};
pub use self::package_authority::{
    CSharpPackageAuthorityConfiguration, JavaPackageAuthorityConfiguration,
    PackageAuthorityConfiguration, PackageAuthorityError, PackageAuthorityOwner,
    PackageAuthorityRequest, PackageAuthorityStage, RustPackageAuthorityConfiguration,
    enter_package_authority,
};
pub use self::package_source::MAX_LOCAL_PACKAGE_SOURCE_BYTES;
pub(crate) use self::runtime::LocalCompilerPlaneExecutionSeed;
pub use self::runtime::{
    CompilerSessionLineage, ExactInputWitness, LocalCompilerCapabilities, LocalCompilerCapability,
    LocalCompilerCapabilityState, LocalCompilerClient, LocalCompilerExecutionIdentity,
    LocalCompilerPlaneExecutionIdentity, LocalCompilerPlaneRecipeIdentity,
    LocalCompilerRuntimeConfiguration, LocalCompilerRuntimeConfigurationError,
    LocalCompilerRuntimeOpenError, LocalCompilerRuntimePaths, LocalRuntimeCSharpAuthority,
    LocalRuntimeJavaAuthority, LocalRuntimePackageAuthority, LocalRuntimePackageRoot,
    LocalRuntimePackageRootFacts, LocalRuntimeRustAuthority, LocalRuntimeToolchain,
    LocalRuntimeToolchainFacts, LocalRuntimeToolchainState, OwnedPackageSource,
    OwnedPackageSourceSet, PackageSemanticRuntimeError, PyreflyToolchainIdentity,
};
pub use self::terminal::{LocalCompilerOpenError, LocalCompilerPath};
pub(crate) use self::toolchain_probe::{
    NATIVE_COMPILER_ENVIRONMENT_POLICY_ID, NativeCompilerEnvironment,
};
pub use self::toolchain_probe::{
    ToolchainProbeCleanupAction, ToolchainProbeError, ToolchainProbeLimitError,
    ToolchainProbeLimits, ToolchainProbeLimitsView, ToolchainProbePrimary,
    ToolchainProbeStreamError,
};
pub use self::unit_authority_v2::{
    AdmittedCompilationUnitV2, CapturedUnitMemberV2, CompilationUnitKindV2, CompilationUnitPlanV2,
    PortableOptionSnapshotV2, SelectedUnitSourceV2, SelectedUnitSourcesV2, UnitAuthorityV2Error,
    UnitExecutionFallbackReasonV2, UnitExecutionReadinessV2, UnitFrontendAvailabilityV2,
    UnsupportedClosureRequirementV2, admit_compilation_unit_v2, plan_compilation_unit_v2,
    select_unit_sources_from_workspace_tree_v2, unit_execution_readiness_v2,
    unit_remote_closure_requirement_v2,
};
pub use crate::compiler_input_capture_v2::{
    CaptureWorkspaceIdentityV2, CapturedFullWorkspaceV2, CapturedFullWorkspaceV2Expectation,
    CompilerInputCaptureCacheKeyV2, CompilerInputCaptureUpdateModeV2,
    CompilerInputCaptureUpdateStatsV2, CompilerInputCaptureV2Error, CompilerWorkspaceEntryKindV2,
    CompilerWorkspaceEntryV2, CompilerWorkspaceFileV2Schema, VerifiedFullWorkspaceClosureV2,
    WorkspaceSnapshotSourceV2, capture_full_workspace_v2, capture_full_workspace_v2_with_prior,
    verify_full_workspace_closure_v2,
};
pub use crate::compiler_input_manifest_v2::{
    CompilationUnitKeyV2, CompilerInputManifestV2, CompilerInputManifestV2Error,
    CompilerInvocationRecipeV2, CompilerPackageTargetV2,
};
pub use backend_compile::{
    EmbeddingArtifact, EmbeddingArtifactId, EmbeddingExecutable, EmbeddingExecutableError,
    EmbeddingExecutionIdentity, EmbeddingNormalization, EmbeddingRuntimeSpecError,
    EmbeddingRuntimeSpecV1, MAX_EMBEDDING_MODEL_BYTES, MAX_EMBEDDING_TOKENIZER_BYTES,
};
pub use backend_execution::{
    CompilerAssignment, CompilerAssignmentError, CompilerAssignmentOutcome,
    CompilerAssignmentRoute, CompilerAttemptToken, CompilerBackoffPolicy, CompilerBalancedRemote,
    CompilerBalancingRequest, CompilerByteCredits, CompilerClusterScheduler, CompilerCpuCredits,
    CompilerDemand, CompilerHedgeAssignments, CompilerIdentityError, CompilerInputIdentityClaim,
    CompilerInputScope, CompilerMemoryCredits, CompilerNodeCapacityClaim,
    CompilerNodeCapacityError, CompilerNodeCapacityVerifier, CompilerPeerId, CompilerPeerRestart,
    CompilerPeerRetryRecord, CompilerPlacement, CompilerPlacementPolicy,
    CompilerRemotePreflightError, CompilerRemotePreflightVerifier, CompilerRemoteProbeBinding,
    CompilerResourceCredits, CompilerRetrySnapshot, CompilerSessionAffinity, CompilerStealLease,
    CompilerWorkIdentity, CompilerWorkQueue, CompilerWorkQueueError, ExactCompileReadSetClaim,
    ExactCompileReadSetError, ExactCompileReadSetVerifier, FullWorkspaceInputClaim,
    FullWorkspaceInputError, FullWorkspaceInputVerifier, LocalCompilerAvailability,
    MAX_COMPILER_PROBE_WINDOW_MS, MAX_PACKAGE_LINEAGE_COMPONENT_BYTES, PackageLineageComponent,
    PackageLineageId, RemoteCompilerCapabilityClaim, RemoteCompilerCompletionClaim,
    RemoteCompilerCostClaim, RemoteCompilerPreflightClaim, RemoteHaveClaim,
    StoredCompilerCandidate, VerifiedCompileReadSet, VerifiedCompilerInput,
    VerifiedCompilerNodeCapacity, VerifiedRemoteCompiler, VerifierAcceptedFullWorkspaceInput,
    compiler_full_workspace_transfer_work_id, compiler_transfer_work_id,
};
pub use backend_frontend_go::legacy::oracle::GoPackageAuthorityWitness;
pub use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
};
