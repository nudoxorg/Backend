//! Composition root for the local-first versioned engine.
//!
//! The lower crates provide canonical identities, relation state, execution
//! scheduling, library views, and generic replication. This crate composes
//! them into one durable workspace owner, effect coordinator, daemon boundary,
//! and pure worker endpoint.
#![deny(unsafe_code)]
// Test modules intentionally use assertion-oriented `expect`/`unwrap` calls
// to keep failure context at the assertion site.  These allowances apply
// only to the test harness; production code remains fail-closed and warning
// free under the crate's strict lint policy.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

/// Re-exported so process applications depend only on `backend-engine` while
/// sharing the engine's pinned hashing and JSON implementations.
pub use blake3;
pub use serde_json;

pub mod acquisition;
pub mod application;
/// Typed advisory ingestion, durable frontier, and acquisition policy.
pub use backend_advisory as advisory;
pub mod builtin;
pub mod capability;
pub mod daemon;
pub mod dispatch;
pub mod driver;
pub mod effects;
pub mod fault;
/// Typed, content-addressed source acquisition from code forges.
pub mod forge;
pub use forge::{
    ForgeAcquisitionError, ForgeAcquisitionLimits, ForgeAcquisitionOutcome,
    ForgeAcquisitionPolicy, ForgeAcquisitionResult, ForgeAcquisitionService, ForgeArchive,
    ForgeArchiveFormat, ForgeAuthToken, ForgeCoordinate, ForgeCoordinateError,
    ForgeDelegatedObject, ForgeDelegationRequest, ForgeFact, ForgeHashAlgorithm, ForgeObjectId,
    ForgePackageManifest, ForgeProtocolError, ForgeProvider, ForgeReceipt, ForgeRefName,
    ForgeRejectReason, ForgeRepositoryMetadata, ForgeResolution, ForgeRevision, ForgeTransport,
    ForgeTransportError, ForgeUnavailableReason, HttpForgeTransport, verify_delegated_object,
};
pub mod index_build;
pub mod index_publish;
pub mod journal;
pub mod platform;
pub mod publication;
pub mod queue;
pub mod registry;
pub mod retrieval;
pub mod schema;
pub mod tcp;
pub mod telemetry;
pub mod worker;
pub mod workspace;

pub use backend_execution::{
    Admission, AdmissionError, AdmissionRequest, AttemptError, AttemptFence, AttemptLease,
    AttemptManager, AuthorityVersion, AuthorityVersionSchema, Budget, CancelHandle, Cancellation,
    CompletionCost, CostObservation, CostSnapshot, DeltaPlan, DeltaPlanner, Envelope,
    EnvelopeBudgets, ExportStatus, FamilySnapshot, HedgeError, HedgeRace, HedgeSide, InternError,
    Interned, LocalCapability, LocalState, MetricFamily, MetricOutcome, Observation,
    ObservationError, OutputAdmission, OutputAdmissionError, OutputEquivalence, OutputSchema,
    OutputVersion, PlacementClass, PlacementDecision, ReadManifestId, ReadManifestSchema,
    RebuildScope, RecipeId, RecipeSchema, RefreshChoice, RefreshCost, RemoteCapability,
    RemoteState, ResourceVector, ResultCoverage, ResultReceipt, ReuseContext, RuntimeSnapshot,
    ScheduleError, ScheduleOutcome, ScheduleReceipt, ScheduleRequest, Scheduled, Scheduler,
    Telemetry, TelemetryExporter, TelemetrySnapshot, UntrustedResultReceipt, VersionedWorkIdentity,
    WorkInterner, WorkKey, WorkKeySchema, acquisition_work_key, choose_refresh,
};
pub use backend_replication::{
    AdmittedAuthority, AdmittedChunk, AttemptId, Attestation, AttestationClass,
    AttestationMaterial, AttestationVerifier, AuthorityClaim, AuthorityEpoch, AuthorityExpectation,
    BoundClosureRoot, CancelAttempt, CancelAttemptExpectation, CancellationId, CanonicalCas,
    CanonicalDigest, CapabilityManifest, ChunkChain, ChunkChainDigest, ChunkParts,
    ClosureNeedRequest, ClosureObjectRequest, ClosurePageRequest, ClosurePageResponse,
    ClosureRootAck, ClosureRootOffer, ClosureSync, ClosureSyncCursor, ClosureSyncPage,
    ExecutionRequestExpectation, ExecutionResultExpectation, ExecutionScopeId, ExpectedIdentity,
    Fence, Frame, ImmutableObjectSchema, LOCAL_CONTROL_HEADER_BYTES, LOCAL_CONTROL_MAGIC,
    LOCAL_CONTROL_MAX_CURSOR, LOCAL_CONTROL_MAX_ERROR, LOCAL_CONTROL_MAX_FRAME,
    LOCAL_CONTROL_VERSION, LocalControlClient, LocalControlError, LocalControlLimits,
    LocalControlRequest, LocalControlResponse, LocalSubscriptionId, LocalSubscriptionOperation,
    LocalSubscriptionRequest, LocalSubscriptionResetReason, LocalSubscriptionResponse,
    MAX_UNIX_ENDPOINT_PATH_BYTES, MerkleChild, MerkleDelta, MerkleDeltaPage, MerkleLeafEntry,
    MerkleObject, MerklePage, MerklePageBody, MerklePageRequest, MerklePageSource,
    MerkleReconciler, MerkleRoot, MerkleRootClaim, NegotiatedCapabilities, NodeDigest,
    ObjectRequest, ObjectSummary, ObjectSummaryExpectation, PageCursor, ReceivingCas,
    ReceivingCasSink, ReceivingCheckpoint, RecipeCapability, ReconcileBudget, ReplicationError,
    ResourceEnvelope, RevocationVersion, RootSummary, RootSummaryExpectation, SchemaDescriptor,
    SparseCoverage, StagedExtent, TransferId, TransportLimits, TransportMessage, UnixEndpointPath,
    UnixEndpointPathError, UnixEndpointRef, VersionRange, WireAuthority, WireAuthorityPolicy,
    WireIdentity, WirePackClaim, WireRecipeRequest, WireRecipeResult, WireRootSummary,
    WorkspaceRootClaim, canonical_object_digest, claim_schema_object_key,
    claim_schema_object_version, control_request_id as local_control_request_id,
    decode_request as decode_local_control_request,
    decode_response as decode_local_control_response,
    encode_request as encode_local_control_request,
    encode_response as encode_local_control_response, encode_stream_frame,
    frame as frame_local_control, is_control as is_local_control, read_frame as read_local_frame,
    relation_identity_claim, schema_object_key_identity_claim,
    schema_object_version_identity_claim, unframe as unframe_local,
    write_frame as write_local_frame,
};
pub use backend_replication::{FramedStream, FramedStreamError};
pub use backend_advisory::{AcquisitionGate, OfflinePolicy};
// The process applications depend only on this composition crate. Keep the
// portable library DTOs, version primitives, and closure types available here
// so an application cannot accidentally grow a second direct dependency edge
// into one of the lower crates.
pub use backend_library::{
    AdvisoryCategory, AdvisoryCoverage, AdvisoryDecisionDto, AdvisoryPackageDto, AdvisoryStatus,
    AdvisorySurfaceDto, AffectedRange, AcquisitionDecision, FreshnessState, NativeAdvisoryId,
    PolicyReason, SeverityLevel,
    Basis, BranchKey, CURSOR_CONTROL_BYTES, CURSOR_SCHEMA, CapabilityAuthority, CapabilityFamily,
    CapabilityId, CapabilityInventory, CapabilityLifecycle, CapabilityStatus, CapabilityTarget,
    CapabilityUnavailable, Command, CommandDto, CommandFailure, CommandReply, CommittedViewDelta,
    CompleteViewProjection, Coverage as ViewCoverage, CoverageCapability, Cursor, CursorEvent,
    CursorResetReason, DTO_VERSION, DeclarationChange, DeclarationRecord, DiffRecord, Document,
    EmbeddingCapabilityRecipe, EmbeddingEncoding, EmbeddingMetric, EmbeddingNormalization,
    EmbeddingPooling, EmbeddingRecipeId, EmbeddingSource, EventDto, FaultRows, Fragment, Freshness,
    Frontier, GraphNeighborhoodQuery, GraphQueryControl, GraphQueryPage, GraphQueryRequest,
    GraphAuthority, GraphAvailability, GraphControl, GraphEdgeId, GraphEdgeKind, GraphLayoutEdge,
    GraphLayoutInput, GraphNodeId, GraphPageTerminal, GraphProvenance, GraphQueryRow,
    GraphRelation, GraphRelationFamily, GraphValue, HealthReport, IngestProgress, IntentId, Lane,
    LanguageOracleTask, RichGraphBuilder, RichGraphCursor, RichGraphDelta, RichGraphEdge,
    RichGraphError, RichGraphNode, RichGraphPage, RichGraphRequest, RichGraphRevision,
    RichGraphSnapshot,
    LanguageRows, LogKey, MAX_PRODUCT_ROWS, MAX_PROGRESS_FAULTS, MAX_PROGRESS_LANGUAGES,
    MAX_ROW_IDENTITY_PREIMAGE_BYTES,
    MAX_SNAPSHOT_PAGE_ROWS, MAX_SUBSCRIPTION_EVENTS, MAX_VIEW_PATCH_ROWS, Outline,
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencySourceFacts, PackageDependencyTarget,
    PackageAuthorityIdentity, PackageCoordinate as ProductPackageCoordinate, PackageKey,
    PackageReference, PageContinuation, PageRequest, PageTerminal, ProductAdmissionError,
    ProductText, ProjectId, ProjectName, ProjectRecord, ProjectSelector, ProjectionPage, Query,
    QueryLimit, Reason, ReferenceFact, ReferenceRecord, RegistryDownloadCount,
    RegistryEcosystem, RegistryFactAvailability, RegistryMetadata, RegistryPackageRecord,
    ForgeFact as ForgePackageFact, ForgeManifestRecord, ForgePackageRecord,
    ForgeRepositoryMetadataRecord, MAX_REGISTRY_FORGE_ASSOCIATIONS, MAX_REGISTRY_FORGE_BLOBS,
    MAX_REGISTRY_FORGE_CANDIDATES, MAX_REGISTRY_FORGE_ASSOCIATION_BYTES,
    REGISTRY_FORGE_ASSOCIATION_VERSION,
    RegistryForgeAssociation, RegistryForgeAssociationError, RegistryForgeAssociationState,
    RegistryForgeBlobFrontier, RegistryForgeBlobKind, RegistryForgeBlobPage,
    RegistryForgeBlobRef, RegistryForgeCandidate,
    RegistryForgeConfidence, RegistryForgeProvenance, RegistryForgeSourceIdentity,
    MAX_REGISTRY_FORGE_PAGES, REGISTRY_FORGE_BLOB_FRONTIER_VERSION,
    RegistryReleaseStanding,
    ReleaseRecord, ReplyDto, Row, RowChange, RowId, RowIdentityPreimage, RowIdentityPreimageError,
    SemanticConfidence,
    SemanticDeclarationIdentity, SemanticGenerationId, SemanticLanguageProfile, SemanticLinkDelta,
    SemanticLinkEvidence, SemanticLinkKind, SemanticLinkTarget, SemanticSourceSpan,
    SemanticVersionRecord, SnapshotHydrator, SnapshotPageClaim, SnapshotPageDto,
    SourceLanguage as ProductSourceLanguage, SubscriptionDto, SubscriptionRecord, SurfaceCommand,
    SurfaceReply, SymbolAddress, SymbolKey, TreeNodeId as ProductTreeNodeId, TreeNodeRecord,
    TreeOpener, TreeSubject, ViewDelta, ViewDto, ViewPageCursor, ViewPageError, ViewProjection,
    ViewProjectionError, ViewRecipeId, ViewRelation, ViewRoot, ViewRootDescriptor,
    ViewRootDescriptorClaim, ViewSnapshot, ViewSnapshotPage, ViewStateRoot, ViewVersion,
    WireCertificate, WireClaim, WireSchema, command_request_id, compiler_authority_recipe,
    decode_compact_view_event, encode_compact_subscription, encode_compact_view_event, encode_id,
    encode_view_root_descriptor, intent_id, object_version, package_key, protocol_version,
    symbol_key, view_identity_bytes, view_key, view_state_root, view_version_preimage,
};
pub use backend_store::{
    ClosureManifest, FileStore, GcLimits, GcReport, GcRoot, GcRoots, ObjectId,
    RelationAdmissionRegistry, TypedObject, WorkspaceClosure,
};
pub use backend_version::{
    AdmittedProducerObservation, AuthorityScopeClaim, AuthorizedCompleteCoverage, CanonicalNode,
    CanonicalNodeView, CanonicalRelation, CheckedCanonicalRoot, CheckedCommit,
    CheckedWorkspaceManifest, CheckedWorkspaceTransition, Commit, CommitProvenance, Coverage,
    CoverageWitness, IdContext, LazyPreparedUpdate, LazyTree, MapChange, ObjectClosure, ObjectKey,
    ObjectVersion, PersistedTreeRoot, ProducerObservationClaims, ProducerObservationVerifier,
    Relation, RelationBinding, RelationDecodeError, RelationState, RelationTransition, Schema,
    ScopeRoot, StateRoot, TreeChange, TreeNodeChildren, TreeNodeClosure, TreeNodeClosureWork,
    TreeNodeHandle, TreeNodeId, TreeNodeLoader, TreeNodeView, UntrustedId,
    UntrustedProducerObservation, UntrustedWorkspaceDelta, UntrustedWorkspaceManifest,
    WorkspaceDelta, WorkspaceManifest, WorkspaceRoot, admit_canonical_root,
    admit_canonical_root_claim, admit_complete_scope, admit_producer_observation, canonical_empty,
    commit_capability, commit_checked, partial_coverage, prepare_delta,
};
// Semantic recipe/read types are re-exported through the composition crate so
// process applications retain the exact one-edge dependency rule.  Keep the
// compact execution identities above distinct from these richer manifests.
pub use backend_semantic::{
    DependencyFact, DependencyManifest, FacetKind, FacetValue, FacetValueSchema,
    Read as SemanticRead, ReadDependencyFact, ReadManifest as SemanticReadManifest,
    Recipe as SemanticRecipe, RecipeSpec as SemanticRecipeSpec, ScopedRead,
};
pub use builtin::{
    BuiltinInputSchema, Container, DeclarationKind, DeclarationRetention, ECHO_AUTHORITY_BYTES,
    ECHO_EQUIVALENCE_BYTES, ECHO_OUTPUT_BYTES, ECHO_READ_BYTES, ECHO_RECIPE_BYTES,
    ECHO_WITNESS_BYTES, PRODUCT_AUTHORITY_BYTES, PRODUCT_EQUIVALENCE_BYTES,
    PRODUCT_EXECUTION_MEMORY_BYTES, PRODUCT_EXECUTION_NODE_BUDGET, PRODUCT_OUTPUT_BODY_BYTES,
    PRODUCT_OUTPUT_BYTES, PRODUCT_RECIPE_BYTES, PRODUCT_WITNESS_BYTES, ProductClosureManifestClaim,
    ProductInput, ProductProjection, ProductProjectionBuilder, ProductSemanticPublicationKey,
    ProductSemanticPublicationRecord, ProductSemanticPublicationRelation,
    ProductSemanticPublicationSnapshot, ProductSourceDeltaFacts, ProductSourceRecord,
    ProductSourceRelation, ProductSourceRetentionFacts, ProductSourceSnapshot,
    ProductSourceTransition, Profile, ProfileDescriptor, ProfileIds, RetainedDeclarations,
    SEMANTIC_OUTPUT_BODY_BYTES, SemanticPublicationInput, SemanticPublicationProjection,
    SemanticPublicationProjectionBuilder, SemanticPublicationRetentionFacts,
    SemanticPublicationSelection, SemanticPublicationSelectionError, SourceDeclaration,
    SourceLanguage, SourceLocation, SourceUnavailableReason, WorkspaceViewProducerAdmission,
    admit_product_closure_manifest, canonical_relation_row, coverage_from_admitted_authority,
    echo_row_bytes, execution_input_basis, execution_input_basis_from_source,
    execution_input_manifest, execution_input_manifest_from_source, execution_manifest,
    execution_resources, product_closure_manifest, product_closure_root,
    product_dependency_manifest, product_input_bytes, product_input_claim, product_input_version,
    product_output_bytes, product_read_manifest, product_source_file_key, product_source_fixture,
    product_source_fixture_with_authority, profile_descriptor, profile_ids, profile_output_len,
    semantic_execution_input_basis, semantic_execution_input_basis_from_snapshot,
    semantic_execution_input_manifest, semantic_execution_input_manifest_from_snapshot,
    semantic_input_bytes, semantic_input_claim, semantic_input_version,
    semantic_publication_fixture_with_authority, semantic_publication_output_bytes,
    validate_semantic_claim, validate_semantic_manifest,
};

/// Decodes one strict library command DTO at the process boundary.
///
/// The library DTO's custom deserializer requires producer certificates for
/// identity-bearing commands; identity-free commands such as health retain
/// only their bounded typed payload and correlation ID.
///
/// # Errors
///
/// Returns an error when the bytes do not contain a valid bounded command
/// envelope.
pub fn decode_command_dto(bytes: &[u8]) -> Result<CommandDto, String> {
    serde_json::from_slice(bytes).map_err(|error| error.to_string())
}

/// Decodes a command using the local durable owner's admitted cursor.
///
/// # Errors
///
/// Returns an error when the command or its owner-bound continuation claims
/// fail admission.
pub fn decode_command_dto_for_owner(bytes: &[u8], owner: Cursor) -> Result<CommandDto, String> {
    backend_library::decode_command_body_for_owner(bytes, owner)
}

/// Decodes one strict library reply DTO at the process boundary.
/// # Errors
///
/// Returns an error when the bytes do not contain the shared versioned reply
/// grammar or when an identity-bearing reply lacks its admission evidence.
pub fn decode_reply_dto(bytes: &[u8]) -> Result<ReplyDto, String> {
    backend_library::decode_reply_body(bytes)
}

/// Encodes one library reply DTO using the shared strict wire schema.
///
/// # Errors
///
/// Returns an error when the reply cannot be serialized under the bounded
/// wire schema.
pub fn encode_reply_dto(reply: &ReplyDto) -> Result<Vec<u8>, String> {
    serde_json::to_vec(reply).map_err(|error| error.to_string())
}

/// Encodes one producer-certified event DTO for a subscription payload.
/// Applications use this facade instead of adding a second direct dependency
/// on the portable library's serialization implementation.
///
/// # Errors
///
/// Returns an error when the event cannot be serialized under the bounded
/// wire schema.
pub fn encode_event_dto(event: &EventDto) -> Result<Vec<u8>, String> {
    serde_json::to_vec(event).map_err(|error| error.to_string())
}

/// Encodes one producer-certified view DTO for a subscription reset payload.
/// The resulting bytes retain the shared library wire schema and certificate.
///
/// # Errors
///
/// Returns an error when the view cannot be serialized under the bounded wire
/// schema.
pub fn encode_view_dto(view: &ViewDto) -> Result<Vec<u8>, String> {
    serde_json::to_vec(view).map_err(|error| error.to_string())
}

/// Encodes one typed subscription payload for the local control envelope.
/// Applications use this facade so subscription serialization remains owned
/// by the portable library protocol rather than growing another JSON shape.
///
/// # Errors
///
/// Returns an error when the subscription cannot be serialized under the
/// bounded wire schema.
pub fn encode_subscription_dto(subscription: &SubscriptionDto) -> Result<Vec<u8>, String> {
    serde_json::to_vec(subscription).map_err(|error| error.to_string())
}

/// Encodes one stateful subscription suffix using compact view events.
///
/// # Errors
///
/// Returns an error when the cursor or event suffix exceeds the bounded
/// subscription encoding.
pub fn encode_compact_subscription_dto(
    previous: Cursor,
    cursor: Cursor,
    events: &[EventDto],
) -> Result<Vec<u8>, String> {
    encode_compact_subscription(previous, cursor, events)
}

/// Encodes one bounded, producer-certified snapshot hydration page.
///
/// Keeping this beside [`encode_subscription_dto`] makes the local daemon's
/// public façade explicit while the portable protocol still owns the JSON
/// grammar and certificate-bearing projection.
///
/// # Errors
///
/// Returns an error when the page cannot be serialized under the bounded wire
/// schema.
pub fn encode_snapshot_page_dto(page: &SnapshotPageDto) -> Result<Vec<u8>, String> {
    serde_json::to_vec(page).map_err(|error| error.to_string())
}

#[cfg(any(unix, windows))]
pub use backend_replication::{
    AuthenticatedLocalPeer, LocalAddr, LocalListener, LocalPeerAuthenticationError, LocalStream,
};
pub use daemon::{
    CompletionNotice, Daemon, DaemonConfig, DaemonError, DaemonHandle, DaemonProtocolConfig,
    DaemonReply, DaemonRequest, Operation, QueryState, ReplicationReply, SubscriptionReply,
    ViewBinding, ViewBindingAdmission, ViewBindingState, ViewPersistence,
};
pub use dispatch::{
    AcceptedResultProof, AuthoritySnapshot, AuthorityTransition, Blake3AuthorityVerifier,
    CompleteSemanticCoverage, CompleteSemanticCoverageCapability, CompositeAdmissionValidator,
    DispatchAttemptKey, DispatchCompletion, DispatchCursor, DispatchError, DispatchJournal,
    DispatchJournalError, DispatchJournalLimits, DispatchJournalReplay, DispatchLog, DispatchPhase,
    DispatchPlan, DispatchRecord, DispatchRecordError, DispatchRecovery, DispatchRecoveryAction,
    DispatchRestartAuthority, DispatchRestartDecision, DispatchTicket, Dispatcher, ExpectedInput,
    FenceBinding, InFlightRemote, MAX_DISPATCH_CURSOR_BYTES, MAX_DISPATCH_PROOF_BYTES,
    MAX_DISPATCH_REQUEST_BYTES, NotificationCursor, OutputAdmissionValidator,
    OwnerRestartAuthority, PendingRemoteEnvelope, PendingRemoteKey, PublicationAck,
    RESTART_ALREADY_FENCED, RESTART_AUTHORITY_REVOKED, RESTART_LEASE_EXPIRED,
    RESTART_OWNER_TAKEOVER, RecipeScopeAuthorityValidator, RecoveredAttempt, RemoteAttemptIntent,
    RemoteAuthorityPolicy, RemoteCorrelationKey, RemoteDispatchContract, RemoteDispatchJournal,
    RemoteDispatchRecord, RemoteTransport, RestartAuthoritySnapshot, RetainedOutput,
    SemanticCoverage, SemanticCoverageAdmissionError, SemanticCoverageBinding,
    SemanticCoverageClaim, SemanticCoverageSchema, SemanticCoverageState,
    SemanticCoverageValidator, SemanticDependencyCoordinator, SemanticDependencyError,
    SemanticInvalidationReport, StreamTransport, TerminalState, TransferCheckpointRef,
    UnconfiguredAuthorityVerifier, UnconfiguredOutputValidator, UntrustedSemanticCoverage,
    UntrustedSemanticCoverageClaim, WorkerReceiptId, WorkerReceiptSchema, cancellation_id_for,
    worker_receipt_id,
};
pub use effects::{
    AmbiguousReason, EffectCoordinator, EffectError, EffectFence, EffectHandle,
    EffectJournalPersistence, EffectKey, EffectKeySchema, EffectLog, EffectPersistence,
    EffectPhase, EffectRecord, EffectSink, EffectSpec, EffectState, MemoryPersistence,
    RecoveredEffect, SinkApply, SinkError, SinkObservation, effect_key,
};
pub use fault::{Boundary, Faults, InjectedCrash};
pub use journal::{
    ChainHash, HashChainJournal, JournalCodec, JournalDomain, JournalError, JournalFrame,
    JournalLimits, JournalReceipt, JournalRecovery,
};
pub use platform::{AuthoritySecretError, read_authority_secret};
#[cfg(any(unix, windows))]
pub use backend_replication::{PeerCredentialError, peer_is_same_effective_uid};
#[cfg(unix)]
pub use platform::{
    PeerCredentials, current_effective_uid, peer_credentials,
};
pub use queue::{
    BoundedQueue, FairQueues, QueueBudget, QueueError, QueueLane, QueueSized, QueueUsage,
};
pub use tcp::{
    AuthenticatedReadHalf, AuthenticatedTcpStream, AuthenticatedWriteHalf, TcpAuthority,
    TcpHandshakeError,
};
pub use worker::{
    AdmittedInput, AdmittedInvocation, Blake3WorkerSigner, NoAttestationSigner, PreparedOutput,
    PureRecipeExecutor, PureWorkContext, WorkerAttestationSigner, WorkerCapabilities,
    WorkerEndpoint, WorkerError,
};
pub use workspace::{
    DerivedOutputEntry, DerivedOutputPublication, Durable, DurablePublication, HeadExpectation,
    OwnerLease, PersistedTransition, Prepared, PreparedPublication, PreparedTransition,
    PublicationStatus, Published, PublishedPublication, RelationIdentity, RelationKeyPrefix,
    TransactionId, TransactionSchema, TransactionVersion, TransitionWork, WorkspaceError,
    WorkspaceGcPin, WorkspaceHead, WorkspaceModel, WorkspaceOwner, WorkspaceRecord,
    WorkspaceRelationChild, WorkspaceRelationError, WorkspaceRelationFault,
    WorkspaceRelationHandle, WorkspaceRelationNodeHandle, WorkspaceRelationNodePage,
    WorkspaceRelationRejection, WorkspaceSnapshot,
};

mod session;

pub use session::Engine;

