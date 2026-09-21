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
    ForgeRepositoryMetadataRecord, RegistryReleaseStanding,
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

/// Generic engine composition around one daemon owner.
pub struct Engine<M, V = UnconfiguredOutputValidator, A = UnconfiguredAuthorityVerifier>
where
    M: WorkspaceModel,
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    daemon: Daemon<M, V, A>,
}

/// Returns the owner clock persisted in durable lease and dispatch records.
///
/// `Instant` is intentionally unsuitable here: it is process-relative and
/// restarts at zero after a reopen, which would make an expired remote lease
/// appear fresh.  The durable owner protocol uses the wall-clock epoch in
/// milliseconds so a new process observes the same time domain as the one
/// that admitted the attempt.
fn owner_wall_clock_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
        .max(1)
}

impl<M, V, A> std::fmt::Debug for Engine<M, V, A>
where
    M: WorkspaceModel,
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("daemon", &self.daemon)
            .finish()
    }
}

impl<M, V, A> Engine<M, V, A>
where
    M: WorkspaceModel,
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: AttestationVerifier + Send + Sync + 'static,
{
    /// Opens the durable local engine and acquires workspace ownership.
    ///
    /// # Errors
    ///
    /// Returns an error when ownership, durable journals, or the composed
    /// daemon cannot be opened and recovered.
    pub fn open(
        directory: impl AsRef<std::path::Path>,
        model: M,
        genesis: WorkspaceHead,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
    ) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let owner = WorkspaceOwner::open(&directory, model, genesis)?;
        let (dispatch_journal, _recovery) = DispatchJournal::open(
            directory.join(daemon::DISPATCH_JOURNAL_FILE),
            DispatchJournalLimits::default(),
        )
        .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let now = owner_wall_clock_millis();
        let daemon =
            Daemon::new_with_dispatch_journal(owner, dispatcher, config, dispatch_journal, now)
                .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        Ok(Self { daemon })
    }

    /// Opens the durable engine with an explicit canonical relation registry.
    /// Process compositions that persist custom relation roots use this path
    /// so the store and recovery validate the same relation grammar.
    ///
    /// # Errors
    ///
    /// Returns an error when ownership, relation admission, durable journals,
    /// or the composed daemon cannot be opened and recovered.
    pub fn open_with_relation_registry(
        directory: impl AsRef<std::path::Path>,
        model: M,
        genesis: WorkspaceHead,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
        relation_registry: RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let owner =
            WorkspaceOwner::open_with_registry(&directory, model, genesis, relation_registry)?;
        let (dispatch_journal, _recovery) = DispatchJournal::open(
            directory.join(daemon::DISPATCH_JOURNAL_FILE),
            DispatchJournalLimits::default(),
        )
        .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let now = owner_wall_clock_millis();
        let daemon =
            Daemon::new_with_dispatch_journal(owner, dispatcher, config, dispatch_journal, now)
                .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        Ok(Self { daemon })
    }

    /// Opens the engine with production fault seams used by crash-boundary
    /// tests. The seams remain inside the owner's filesystem operations.
    ///
    /// # Errors
    ///
    /// Returns an error when ownership, fault-aware journals, or the composed
    /// daemon cannot be opened and recovered.
    pub fn open_with_faults(
        directory: impl AsRef<std::path::Path>,
        model: M,
        genesis: WorkspaceHead,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
        faults: std::sync::Arc<Faults>,
    ) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let owner = WorkspaceOwner::open_with_faults(&directory, model, genesis, faults)?;
        let (dispatch_journal, _recovery) = DispatchJournal::open_with_faults(
            directory.join(daemon::DISPATCH_JOURNAL_FILE),
            DispatchJournalLimits::default(),
            owner.faults(),
        )
        .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let now = owner_wall_clock_millis();
        let daemon =
            Daemon::new_with_dispatch_journal(owner, dispatcher, config, dispatch_journal, now)
                .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        Ok(Self { daemon })
    }

    /// Returns a bounded request handle.
    #[must_use]
    pub fn handle(&self) -> DaemonHandle<M::Intent> {
        self.daemon.handle()
    }

    /// Runs one fair owner-loop operation.
    pub fn serve_one(&mut self) -> bool {
        self.daemon.serve_one()
    }

    fn owner_now() -> u64 {
        owner_wall_clock_millis()
    }

    /// Plans one local-first execution while retaining the scheduler's
    /// interner, resource, cancellation, and fence guards in the returned
    /// plan. Applications should use the paired completion methods below so
    /// those guards cannot be bypassed by a second owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be admitted by the bounded
    /// scheduler or its exact identity and resource envelope is invalid.
    pub fn plan<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
    ) -> Result<DispatchPlan<R>, DispatchError> {
        self.daemon.dispatcher().plan(request)
    }

    /// Plans with an owner-admitted semantic capability. Reuse is available
    /// only when the capability's exact dependency binding matches the
    /// retained publication; callers cannot manufacture a reuse context.
    ///
    /// # Errors
    ///
    /// Returns an error when semantic admission or freshness validation
    /// fails, a retained binding is inconsistent, or bounded scheduling
    /// cannot admit the request.
    pub fn plan_with_semantic<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
        semantic: &CompleteSemanticCoverage,
    ) -> Result<DispatchPlan<R>, DispatchError> {
        self.daemon
            .dispatcher()
            .plan_with_semantic(request, semantic)
    }

    /// Converts an untrusted producer coverage claim into the semantic
    /// capability required by local or remote result admission.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim does not match the exact work identity
    /// or the configured authority rejects it.
    pub fn admit_semantic<R: Relation>(
        &self,
        identity: &VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
    ) -> Result<CompleteSemanticCoverage, DispatchError> {
        self.daemon.dispatcher().admit_semantic(identity, claim)
    }

    /// Admits a complete dependency manifest and retains it with the opaque
    /// semantic capability until publication. The dispatcher registers the
    /// manifest under the exact execution work key transactionally.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim or dependency manifest fails exact
    /// semantic admission.
    pub fn admit_semantic_with_manifest<R: Relation>(
        &self,
        identity: &VersionedWorkIdentity<R>,
        claim: UntrustedSemanticCoverageClaim,
        manifest: DependencyManifest,
    ) -> Result<CompleteSemanticCoverage, DispatchError> {
        self.daemon
            .dispatcher()
            .admit_semantic_with_manifest(identity, claim, manifest)
    }

    /// Invalidates retained execution outputs whose admitted dependency facts
    /// intersect the supplied semantic changes. The report contains exact
    /// work keys removed from reuse and quarantined while active.
    ///
    /// # Errors
    ///
    /// Returns an error when invalidation cannot update the bounded semantic
    /// registry.
    pub fn invalidate_semantic_changes(
        &self,
        changes: &[backend_semantic::DependencyChange],
    ) -> Result<SemanticInvalidationReport, DispatchError> {
        self.daemon
            .dispatcher()
            .invalidate_semantic_changes(changes)
    }

    /// Publishes a locally computed output through exact semantic, recipe,
    /// schema/CAS, cancellation, fence, and scheduler admission.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan, semantic capability, output bytes, or
    /// scheduler ownership fails exact admission.
    pub fn complete_local<R: Relation>(
        &mut self,
        plan: DispatchPlan<R>,
        bytes: &[u8],
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        self.daemon
            .complete_local(plan, bytes, semantic, now)
            .map_err(|error| DispatchError::Workspace(error.to_string()))
    }

    /// Allocation-preserving local completion for worker/CAS buffers already
    /// held by an immutable Arc owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan, semantic capability, output bytes, or
    /// scheduler ownership fails exact admission.
    pub fn complete_local_shared<R: Relation>(
        &mut self,
        plan: DispatchPlan<R>,
        bytes: std::sync::Arc<Vec<u8>>,
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchCompletion, DispatchError> {
        self.daemon
            .complete_local_shared(plan, bytes, semantic, now)
            .map_err(|error| DispatchError::Workspace(error.to_string()))
    }

    /// Plans through the owner-controlled durable reuse path. The semantic
    /// capability is checked against the current recipe/scope authority, the
    /// selected workspace catalog is queried for an exact binding, and any
    /// recovered generation is restored before a reusable plan is returned.
    /// Catalog misses fall through to ordinary local-first routing.
    ///
    /// # Errors
    ///
    /// Returns an error when semantic admission, catalog validation, route
    /// scheduling, or durable publication fails.
    pub fn plan_with_durable_reuse<R: Relation>(
        &mut self,
        request: ScheduleRequest<R>,
        semantic: CompleteSemanticCoverage,
        now: u64,
    ) -> Result<DispatchPlan<R>, DaemonError> {
        self.daemon.plan_with_durable_reuse(request, semantic, now)
    }

    /// Composes planning, negotiated remote admission, daemon registration,
    /// and one request send into the sole pending-remote path. The returned
    /// key is only a correlation capability; the daemon retains the affine
    /// plan, contract, cancellation, and scheduler reservations.
    ///
    /// # Errors
    ///
    /// Returns an error when planning, capability negotiation, pending-ticket
    /// admission, or request transmission fails.
    pub fn dispatch_remote<R: Relation + Send>(
        &mut self,
        request: ScheduleRequest<R>,
        contract: RemoteDispatchContract,
        capabilities: &CapabilityManifest,
    ) -> Result<PendingRemoteKey, DaemonError> {
        let plan = self.daemon.plan_with_durable_reuse(
            request,
            contract.semantic.clone(),
            Self::owner_now(),
        )?;
        self.daemon
            .dispatch_remote_pending(plan, contract, capabilities)
    }

    /// Completes the next authenticated result for a daemon-owned pending
    /// remote key. Control frames are retained and reported by the daemon.
    ///
    /// # Errors
    ///
    /// Returns an error when no pending result is available or its exact
    /// request, authority, and output evidence fails admission.
    pub fn receive_remote(&mut self, now: u64) -> Result<DispatchCompletion, DaemonError> {
        self.daemon.receive_remote(now)
    }

    /// Retries a daemon-owned pending request after transport replacement.
    /// A failed retry returns [`DaemonError::RemoteSendPending`] with the same
    /// key, preserving the affine fallback/cancellation capability.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or the replacement
    /// transport rejects the exact retained request.
    pub fn resend_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        capabilities: &CapabilityManifest,
    ) -> Result<(), DaemonError> {
        self.daemon.resend_remote_pending(key, capabilities)
    }

    /// Transfers a planned remote attempt into owner-held fallback custody
    /// without transmitting a recipe request. The returned key is consumed
    /// by the same cancellation, fallback, and publication APIs as a sent
    /// attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when ticket, journal, or bounded pending-relation
    /// admission fails.
    pub fn prepare_remote_pending<R: Relation + Send>(
        &mut self,
        plan: DispatchPlan<R>,
        contract: RemoteDispatchContract,
    ) -> Result<PendingRemoteKey, DaemonError> {
        self.daemon.prepare_remote_pending(plan, contract)
    }

    /// Cancels one exact pending remote attempt and propagates its full
    /// work/attempt/fence/cancellation identity to the peer.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or cancellation could
    /// not be transmitted to the bound peer.
    pub fn cancel_remote_pending(&mut self, key: PendingRemoteKey) -> Result<(), DaemonError> {
        self.daemon.cancel_remote_pending(key)
    }

    /// Switches one pending remote attempt to its owner-held local fallback,
    /// preserving the same semantic and output admission path.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or fallback output
    /// fails exact local admission.
    pub fn fallback_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        bytes: std::sync::Arc<Vec<u8>>,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        self.daemon.fallback_remote_pending(key, bytes, now)
    }

    /// Activates the same owner-held fallback immediately after a terminal
    /// remote failure while preserving every publication and authority fence.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or fallback output
    /// fails exact local admission.
    pub fn fail_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        bytes: std::sync::Arc<Vec<u8>>,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        self.daemon.fail_remote_pending(key, bytes, now)
    }

    /// Installs the daemon-owned remote transport path.
    pub fn set_remote_transport(&mut self, transport: Box<dyn RemoteTransport>) {
        self.daemon.set_remote_transport(transport);
    }

    /// Sends one bounded control prelude through the daemon-owned negotiated
    /// transport. Recipe results remain correlated through pending tickets.
    ///
    /// # Errors
    ///
    /// Returns an error when the control frame fails capability or transport
    /// admission.
    pub fn send_remote_control(
        &mut self,
        message: TransportMessage,
        capabilities: &CapabilityManifest,
    ) -> Result<NegotiatedCapabilities, DaemonError> {
        self.daemon.send_remote_control(message, capabilities)
    }

    /// Receives one bounded control response from the daemon-owned transport.
    ///
    /// # Errors
    ///
    /// Returns an error when the transport is unavailable or the response
    /// fails bounded frame admission.
    pub fn receive_remote_control(&mut self) -> Result<TransportMessage, DaemonError> {
        self.daemon.receive_remote_control()
    }

    /// Borrows the complete composed daemon.
    #[must_use]
    pub const fn daemon(&self) -> &Daemon<M, V, A> {
        &self.daemon
    }

    /// Mutably borrows the daemon for owner-loop setup and shutdown.
    #[must_use]
    pub fn daemon_mut(&mut self) -> &mut Daemon<M, V, A> {
        &mut self.daemon
    }
}
