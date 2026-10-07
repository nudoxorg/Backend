//! Portable product contracts for the local-first versioned library.
//!
//! The crate façade exposes the stable product vocabulary. Versioned catalog
//! behavior lives in the catalog module, canonical identity schemas and
//! admission in [`canonical`], immutable view state in the view module, and
//! transport DTOs in the wire module. None of these modules owns a process, socket, filesystem,
//! scheduler, or native compiler implementation.

#![deny(unsafe_code)]

/// Shared filesystem source-selection policy used by local and package
/// discovery adapters.
pub use backend_discovery::{
    DEFAULT_IGNORED_DIRECTORIES, DiscoveredEntry, DiscoveryError as FilesystemDiscoveryError,
    DiscoveryPolicy, EntryKind, HARD_IGNORED_DIRECTORIES, is_hard_ignored_directory,
    is_hard_ignored_path,
};

mod arrangement;
pub mod browse;
pub mod canonical;
mod capability;
mod cargo_aliases;
mod cargo_source;
mod catalog;
mod command;
mod command_registry;
mod cursor;
mod delta;
mod error;
mod forge_identity;
mod graph_query;
/// Transport-independent application service and reply vocabulary.
pub mod interface;
/// Spelling of absolute fixture paths in the host's native form, for tests.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
#[allow(clippy::expect_used)]
pub mod native_test_paths;
mod package_graph;
mod package_graph_page;
mod progress;
mod python_project;
pub use python_project::{
    PYTHON_PROJECT_EXTRACTION_POLICY, PythonDependencyDeclaration, PythonMetadataEvidence,
    PythonMetadataFact, PythonProjectMetadata,
};
/// Bounded transport decoding and presentation for thin CLI and MCP consumers.
pub mod protocol;
mod registry_forge;
mod registry_native;
mod rich_graph;
mod semantic_shape;
mod source_atom;
mod source_discovery;
mod source_membership;
mod surface;
mod view;
mod wire;

/// Maximum number of events admitted from one bounded subscription payload.
pub const MAX_SUBSCRIPTION_EVENTS: usize = 256;

pub use arrangement::QueryWork;
pub use backend_advisory::{
    AcquisitionDecision, AdvisoryCategory, AdvisoryCoverage, AdvisoryDecisionDto,
    AdvisoryPackageDto, AdvisoryStatus, AdvisorySurfaceDto, AffectedRange, FreshnessState,
    NativeAdvisoryId, OverrideEvidence, PolicyReason, SeverityLevel,
};
pub use backend_compile::{
    DeclarationFacts, DeclarationKind, Deprecation, Fact, MAX_RUST_CARGO_ACTIVE_FEATURES,
    MAX_RUST_CARGO_DEPENDENCY_EDGES, MAX_RUST_CARGO_DEPENDENCY_KINDS, MAX_RUST_CARGO_FACT_ENTRIES,
    MAX_RUST_CARGO_FACT_TEXT_ENTRIES, MAX_RUST_CARGO_FEATURE_EDGES,
    MAX_RUST_CARGO_METADATA_TEXT_BYTES, MAX_RUST_CARGO_PROFILE_FEATURES,
    MAX_RUST_CARGO_RESOLVED_PACKAGES, MAX_RUST_CARGO_TARGETS, MAX_RUST_CARGO_TOTAL_TEXT_BYTES,
    MAX_RUST_CARGO_WORKSPACE_PACKAGES, Obligation, RustCargoBuildProfileV1,
    RustCargoDependencyKindFactV1, RustCargoFactsAdmissionError, RustCargoFeatureFactV1,
    RustCargoFeatureSelectionV1, RustCargoProfileRequestError, RustCargoProfileSelectionV1,
    RustCargoResolvedDependencyFactV1, RustCargoResolvedPackageFactV1, RustCargoTargetFactV1,
    RustCargoWorkspaceFactsV1, RustCargoWorkspacePackageFactV1, SourceExcerpt, SourceExcerptExtent,
    SourceLanguage, SourceLocation,
};
pub use backend_semantic::{Read, ReadManifest, ReadSelector};
pub use backend_version::{
    AdmittedProducerObservation, AuthorityScopeClaim, AuthorizedCompleteCoverage, CoverageWitness,
    IdAdmissionError, IdContext, IdDecodeError, ProducerObservationClaims,
    ProducerObservationVerifier, Relation, RelationState, Schema, ScopeRoot, UntrustedId,
    UntrustedProducerObservation, WireId, admit_complete_scope, admit_producer_observation,
};
pub use canonical::{
    ActorKey, BranchKey, DocumentSchema, DocumentVersion, Frontier, IntentId, IntentSchema, LogKey,
    LogSchema, NameSchema, NameVersion, ObjectSchema, OutlineSchema, OutlineVersion,
    PROTOCOL_SCHEMA, PackageKey, SemanticObject, SemanticShapeBatchSchema,
    SemanticShapeSourceSchema, SymbolKey, ViewDeltaId, ViewRecipeId, ViewRecipeSchema,
    ViewRelation, ViewStateRoot, ViewVersion, ViewVersionSchema, actor_key, admit_delta_against,
    admit_delta_transition, admit_intent_value, admit_key_against, admit_key_value,
    admit_root_against, admit_root_bytes, admit_version_against, admit_version_value, branch_key,
    decode_id, empty_view_relation_preimage, encode_id, intent_id, log_key, object_version,
    package_key, relation_row_key, symbol_key, view_identity_bytes, view_key, view_state_root,
    view_version, view_version_preimage, wire_delta, wire_intent, wire_key, wire_root,
    wire_version,
};
pub use capability::{
    CapabilityAuthority, CapabilityFamily, CapabilityId, CapabilityInventory,
    CapabilityInventoryError, CapabilityLifecycle, CapabilityStatus, CapabilityTarget,
    CapabilityUnavailable, EmbeddingCapabilityRecipe, EmbeddingEncoding, EmbeddingMetric,
    EmbeddingNormalization, EmbeddingPooling, EmbeddingRecipeId, EmbeddingSource,
    LanguageOracleTask, MAX_CAPABILITY_INVENTORY, PackageAuthorityIdentity,
    compiler_authority_recipe, embedding_authority_recipe,
};
pub use cargo_aliases::{
    CargoPackageAliasCargoFactsV1, CargoPackageAliasCoverageV1, CargoPackageAliasErrorV1,
    CargoPackageAliasEvidenceV1, CargoPackageAliasObservationV1, CargoPackageAliasUnavailableV1,
    CargoPackageAliasV1, CargoPackageNameV1, CargoTargetNameV1,
    MAX_CARGO_ALIAS_PROFILE_OBSERVATIONS, MAX_CARGO_PACKAGE_ALIAS_BYTES, MAX_CARGO_PACKAGE_ALIASES,
    RustCrateIdentifierV1,
};
pub use cargo_source::{
    CARGO_PACKAGE_SOURCE_AUTHORITY_SCHEMA, CargoPackageReadmeAbsenceV1,
    CargoPackageReadmeFailureV1, CargoPackageReadmeLinkFailureV1, CargoPackageReadmeLinkRequestV1,
    CargoPackageReadmeLinkResultV1, CargoPackageReadmeLinkTargetV1, CargoPackageReadmeManifestV1,
    CargoPackageReadmeOriginV1, CargoPackageReadmeRequestV1, CargoPackageReadmeResultV1,
    CargoPackageReadmeRootScopeV1, CargoPackageReadmeSelectionV1, CargoPackageReadmeV1,
    CargoPackageRootIdentityV1, CargoPackageSourceAuthorityFailureV1,
    CargoPackageSourceAuthorityStateV1, CargoPackageSourceAuthorityV1,
    CargoPackageSourceFileResultV1, CargoPackageSourceInventoryCoverageV1,
    CargoPackageSourceInventoryFailureV1, CargoPackageSourceInventoryGapV1,
    CargoPackageSourceInventoryResultV1, CargoPackageSourceInventoryV1, CargoPackageSourcePathV1,
    CargoPackageSourceReadFailureV1, CargoPackageSourceRequestV1,
    CargoPackageSourceSemanticStatusV1, CargoPackageSourceV1, CargoRegistrySourceSchemeV1,
    MAX_CARGO_PACKAGE_README_BYTES, MAX_CARGO_PACKAGE_README_LINK_BYTES,
    MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES, MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS,
    MAX_CARGO_PACKAGE_SOURCE_INVENTORY_SCAN_ENTRIES, MAX_CARGO_PACKAGE_SOURCE_PATH_BYTES,
    MAX_CARGO_SOURCE_COORDINATE_BYTES, MAX_CARGO_SOURCE_DETAIL_BYTES,
};
pub use catalog::{Library, QueryPageRecipe, RankedSearchSnapshot};
pub use command::ReferenceFact;
pub use command::{
    Command, CommandFailure, CommandId, CommandReply, CompileExecutionIntent, DocumentQuery,
    GraphNeighborhoodQuery, HealthReport, NameQuery, OutlineQuery, PageContinuation, PageRequest,
    PageTerminal, ProjectionPage, Query, QueryLimit, QueryRecord, RevisionReceipt,
    SemanticSearchReason, SemanticSearchStatus, SymbolAddress, ViewRevision,
};
pub use command_registry::{
    COMMANDS, CommandDomain, CommandMutation, CommandSpec, command_spec, command_spec_named,
};
pub use cursor::{
    CURSOR_CONTROL_BYTES, CURSOR_QUERY_BYTES, CURSOR_SCHEMA, Cursor, CursorError, CursorEvent,
    CursorRead, CursorResetReason, CursorSub,
};
pub use delta::{Delta, Intent, IntentError, IntentLog, IntentReceipt, Settings};
pub use error::LibraryError;
pub use forge_identity::{
    ForgeCoordinate, ForgeCoordinateError, ForgeHashAlgorithm, ForgeObjectId, ForgeProvider,
    ForgeRefName, ForgeRevision, ForgeUnavailableReason,
};
pub use graph_query::{
    AdmittedGraphQueryInput, GraphQueryControl, GraphQueryError, GraphQueryPage, GraphQueryRequest,
    GraphQueryRow, GraphValue, MAX_GRAPH_QUERY_BYTES, MAX_GRAPH_QUERY_FIELDS,
    MAX_GRAPH_VALUE_BYTES, MAX_GRAPH_VALUE_DEPTH,
};
pub use package_graph::{
    BorrowedPackageGraphSourceState, CheckedPackageGraphFacts, CheckedPackageGraphSourceWitness,
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope, DependentSources,
    IndexedCheckedPackageGraph, MAX_PACKAGE_GRAPH_ROWS, PackageDependencyLookup,
    PackageDependencyRecord, PackageDependencySourceFacts, PackageDependencyTarget,
    PackageGraphAdmissionError, PackageGraphIndexLimits, PackageGraphSourceAuthority,
    PackageGraphSourceKey, RegistryAuthorityId, admit_dependency_rows, collapse_dependency_rows,
    dependency_optional, discover_source_entries, discover_source_files, linear_dependent_sources,
    package_dependency_facts_witness, source_selection_policy,
};
pub use package_graph_page::{
    MAX_PACKAGE_GRAPH_AUTHORITIES, MAX_PACKAGE_GRAPH_PAGE_ROWS, PACKAGE_GRAPH_PAGE_SCHEMA,
    PackageGraphControl, PackageGraphCursor, PackageGraphDirection, PackageGraphKnowledge,
    PackageGraphPage, PackageGraphPageError, PackageGraphPageRequest, PackageGraphPageTerminal,
};
pub use progress::{
    FaultRows, IngestProgress, LanguageRows, MAX_PROGRESS_FAULTS, MAX_PROGRESS_LANGUAGES,
    ProgressError, SourceUnavailableReason,
};
pub use registry_forge::{
    MAX_REGISTRY_FORGE_ASSOCIATION_BYTES, MAX_REGISTRY_FORGE_ASSOCIATIONS,
    MAX_REGISTRY_FORGE_BLOBS, MAX_REGISTRY_FORGE_CANDIDATES, MAX_REGISTRY_FORGE_PAGES,
    REGISTRY_FORGE_ASSOCIATION_VERSION, REGISTRY_FORGE_BLOB_FRONTIER_VERSION,
    RegistryForgeAssociation, RegistryForgeAssociationError, RegistryForgeAssociationState,
    RegistryForgeBlobFrontier, RegistryForgeBlobKind, RegistryForgeBlobPage, RegistryForgeBlobRef,
    RegistryForgeCandidate, RegistryForgeConfidence, RegistryForgeProvenance,
    RegistryForgeSourceIdentity,
};
pub use registry_native::{
    CargoPublishTime, MAX_REGISTRY_NATIVE_METADATA_BYTES, MAX_REGISTRY_NATIVE_ROWS,
    MAX_REGISTRY_NATIVE_TEXT_BYTES, REGISTRY_NATIVE_METADATA_VERSION, RegistryCargoMetadata,
    RegistryConanMetadata, RegistryConanSourceAvailability, RegistryGoMetadata, RegistryGoRetract,
    RegistryGoSourceFacts, RegistryMavenChecksum, RegistryMavenMetadata, RegistryNativeArtifact,
    RegistryNativeArtifactKind, RegistryNativeAvailability, RegistryNativeChecksum,
    RegistryNativeChecksumAlgorithm, RegistryNativeDetails, RegistryNativeDistTag,
    RegistryNativeEvidenceClaim, RegistryNativeFeature, RegistryNativeMetadata,
    RegistryNativeMetadataCodecError, RegistryNativeObservation, RegistryNativeProvenance,
    RegistryNativeVulnerability, RegistryNpmMetadata, RegistryNugetMetadata, RegistryPypiMetadata,
};
pub use rich_graph::{
    GraphAuthority, GraphAvailability, GraphControl, GraphEdgeId, GraphEdgeKind, GraphLayoutEdge,
    GraphLayoutInput, GraphNodeId, GraphPageTerminal, GraphProvenance, GraphRelationFamily,
    MAX_RICH_GRAPH_DELTA_RECORDS, MAX_RICH_GRAPH_PAGE_EDGES, MAX_RICH_GRAPH_PAGE_ROWS,
    RICH_GRAPH_SCHEMA_VERSION, RichGraphBuilder, RichGraphCursor, RichGraphDelta, RichGraphEdge,
    RichGraphError, RichGraphNode, RichGraphPage, RichGraphRequest, RichGraphRevision,
    RichGraphSnapshot,
};
pub use semantic_shape::{
    MAX_SEMANTIC_SHAPE_BATCH, MAX_SEMANTIC_SHAPE_BYTES, MAX_SEMANTIC_SHAPE_DEPTH,
    MAX_SEMANTIC_SHAPE_IMAGE_BYTES, MAX_SEMANTIC_SHAPE_NODES,
    SEMANTIC_SHAPE_CARRIER_IDENTITY_BYTES, SemanticAnonymousCallableAnchor, SemanticArrayShape,
    SemanticCallableCarrierBindings, SemanticCallableShape, SemanticDeclarationShape,
    SemanticImagePayloadBytes, SemanticLiteral, SemanticObjectMember, SemanticPropertyKey,
    SemanticShapeAdmissionSummary, SemanticShapeBatch, SemanticShapeBudget, SemanticShapeEntry,
    SemanticShapeError, SemanticShapeFact, SemanticShapeImageOrigin, SemanticShapeLanguageFact,
    SemanticShapeLanguageFacts, SemanticShapeMember, SemanticShapeMemberName,
    SemanticShapeReadRequest, SemanticShapeRequest, SemanticShapeSelection,
    SemanticShapeSourceOrigin, SemanticShapeUnavailable, SemanticTypeElement, SemanticTypeExpr,
    SemanticTypeFact, SemanticTypeUnavailable, semantic_shape_source_key,
    semantic_shape_source_preimage,
};
pub use source_atom::SourceAtomText;
pub use source_discovery::{
    CratesSparseDependency, CratesSparseFeature, CratesSparseMetadata,
    DISCOVERY_BATCH_ENVELOPE_VERSION, DiscoveryAdvisory, DiscoveryAdvisorySummary, DiscoveryBatch,
    DiscoveryBatchDraft, DiscoveryCargoDependencySummary, DiscoveryCargoSparseSummary,
    DiscoveryCompleteness, DiscoveryCursor, DiscoveryDescriptionText, DiscoveryError,
    DiscoveryFacet, DiscoveryFact, DiscoveryFactCore, DiscoveryFactMetadataSummary,
    DiscoveryMetadata, DiscoveryMetadataDelivery, DiscoveryMetadataFacetState,
    DiscoveryMetadataPage, DiscoveryMetadataPageCursor, DiscoveryMetadataRow,
    DiscoveryMetadataSection, DiscoveryObservedAt, DiscoveryPackageRetraction,
    DiscoverySelectedHead, DiscoverySourceEvent, DiscoverySourceIdentity, DiscoveryStanding,
    DiscoveryTimestamp, MAX_DISCOVERY_BATCH_ENCODED_BYTES, MAX_DISCOVERY_COMMIT_ID_BYTES,
    MAX_DISCOVERY_COORDINATE_BYTES, MAX_DISCOVERY_CURSOR_BYTES, MAX_DISCOVERY_DESCRIPTION_BYTES,
    MAX_DISCOVERY_EVENT_TEXT_BYTES, MAX_DISCOVERY_METADATA_PAGE_ENCODED_BYTES,
    MAX_DISCOVERY_METADATA_PAGE_ROWS, MAX_DISCOVERY_PAGE_ITEMS, MAX_DISCOVERY_PROJECTS,
    MAX_DISCOVERY_REVISION_BYTES, RegistryFactReadError, RegistryFactVersionId,
};
pub use source_membership::{
    DEFAULT_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES, MAX_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES,
    MAX_PACKAGE_SOURCE_MEMBERSHIP_PATH_BYTES, PACKAGE_SOURCE_MEMBERSHIP_SCHEMA,
    PackageSourceMembershipCursorV1, PackageSourceMembershipExclusionsV1,
    PackageSourceMembershipFileV1, PackageSourceMembershipLanguageV1,
    PackageSourceMembershipPageRequestV1, PackageSourceMembershipPageResultV1,
    PackageSourceMembershipScopeV1, PackageSourceMembershipUnavailableV1,
};
pub use surface::{
    AuthorityClassFact, AuthorityPhaseFact, CompilerAuthorityDiagnosticFacts, CompilerLanguageFact,
    CompilerNativeToolFact, CompilerStageFact, DeclarationChange, DeclarationRecord, DiffRecord,
    ForgeDiscoveryCandidate, ForgeFact, ForgeManifestRecord, ForgePackageDetailRecord,
    ForgePackageManifestDetail, ForgePackagePin, ForgePackageRecord, ForgePackageRegistryEvidence,
    ForgeRepositoryMetadataRecord, IndexCancelReceipt, IndexCancelStatus, IndexJobObservation,
    IndexJobOutcome, IndexJobProgressEvent, IndexJobProgressKind, IndexJobStage, IndexJobTerminal,
    IndexJobTicket, IndexOperationFailureReason, IndexOperationKey, IndexOperationObservation,
    IndexOperationPriorSemantic, IndexOperationProfileRefusal, IndexOperationPublicationReceipt,
    IndexOperationSemanticCoverage, IndexOperationSemanticProfileState,
    IndexOperationSemanticUnavailableReason, IndexOperationSourceCaptureReceipt,
    IndexOperationSourceProfile, IndexOperationState, IndexOperationStatus,
    IndexOperationUnresolvedReason, IndexProgressPage, IndexSearchCursor, IndexSearchPage,
    IndexSearchResultCount, IndexStartResult, MAX_INDEX_PROGRESS_EVENTS,
    MAX_INDEX_SEARCH_CURSOR_BYTES, MAX_PRODUCT_ROWS, MAX_PRODUCT_TEXT_BYTES,
    MAX_SELECTED_PROJECT_FRONTIER_FILES, PackageCompilerFailure, PackageCompilerFailureCause,
    PackageCompilerFailurePhase, PackageCompilerFragmentFaultFacts, PackageCoordinate,
    PackageForeignKeyFaultFacts, PackageLanguageProjectionFault, PackageLineageFaultFacts,
    PackageLoweringFaultFacts, PackageParentageFact, PackageProjectionAdmissionFaultFacts,
    PackageProjectionConstructorFaultFacts, PackageProjectionSemanticTypeFaultFacts,
    PackageReference, PackageReferenceKind, PackageTypeCellFact,
    PackageTypeScriptProjectionFaultFacts, PackageTypeTagFact, ProductAdmissionError, ProductText,
    ProjectId, ProjectName, ProjectRecord, ProjectSelector, ReferenceRecord,
    RegistryDiscoveryAdvisory, RegistryDiscoveryCandidate, RegistryDiscoveryCompleteness,
    RegistryDiscoveryFreshness, RegistryDiscoveryMetadata, RegistryDiscoveryStanding,
    RegistryDownloadCount, RegistryEcosystem, RegistryEvidenceFacet, RegistryFactAvailability,
    RegistryMetadata, RegistryNegativeFactKind, RegistryPackageDiscoveryObservation,
    RegistryPackageFactAuthority, RegistryPackageFactCompleteness, RegistryPackageFactFreshness,
    RegistryPackageFactProof, RegistryPackageRecord, RegistryPackageSearchGroup,
    RegistryReleaseMatchScope, RegistryReleaseStanding, RegistrySearchGroupKind, RegistrySearchHit,
    RegistrySearchRelease, ReleaseRecord, SelectedProjectSourceFrontier, SemanticConfidence,
    SemanticDeclarationIdentity, SemanticGenerationId, SemanticHistoryImageIdentity,
    SemanticHistoryImagePublicationProof, SemanticHistoryInputReplayStatus,
    SemanticHistoryPublicationProof, SemanticHistoryPublicationStatus,
    SemanticHistorySelectionStamp, SemanticLanguageProfile, SemanticLinkDelta,
    SemanticLinkEvidence, SemanticLinkKind, SemanticLinkTarget, SemanticSourceSpan,
    SemanticVersionFreshness, SemanticVersionRecord, SubscriptionRecord, SurfaceCommand,
    SurfaceReply, TreeNodeId, TreeNodeRecord, TreeOpener, TreeSubject,
    index_operation_request_digest,
};
pub use view::{
    Basis, CommittedViewDelta, CompleteViewProjection, Coverage, CoverageCapability, Document,
    DocumentSelection, Fragment, Freshness, GraphRelation, Lane, MAX_COVERAGE_EVIDENCE,
    MAX_ROW_IDENTITY_PREIMAGE_BYTES, MAX_SNAPSHOT_PAGE_ROWS, MAX_VIEW_PATCH_ROWS, NameRecord,
    Outline, OutlineExtent, OutlineNode, PreparedViewDelta, Reason, Row, RowChange, RowId,
    RowIdentityPreimage, RowIdentityPreimageError, RowState, SourceAvailability, ViewDelta,
    ViewError, ViewPageCursor, ViewPageError, ViewProjection, ViewProjectionError, ViewRoot,
    ViewRootDescriptor, ViewRootDescriptorClaim, ViewSnapshot, ViewSnapshotPage,
};
pub use wire::SemanticShapeExport;
pub use wire::{
    CommandDto, DTO_VERSION, EventDto, JournalViewGrammarV3, MAX_COMMAND_BODY, MAX_COMMAND_TEXT,
    MAX_REPLY_BODY, ReplyAdmissionError, ReplyDto, RequestAdmissionError, SnapshotHydrator,
    SnapshotPageClaim, SnapshotPageDto, SubscriptionDto, ViewDto, WireCertificate, WireClaim,
    WireSchema, admit_reply, admit_reply_with_capability, admit_request, command_request_id,
    decode_command_body, decode_command_body_for_owner, decode_compact_view_event,
    decode_reply_body, decode_reply_body_with_verifier, encode_command_body,
    encode_compact_subscription, encode_compact_view_event, encode_view_root_descriptor,
    reply_memory_bound, semantic_shape_batch_key,
};

/// Returns the current command/reply/event wire schema version.
#[must_use]
pub const fn protocol_version() -> u16 {
    DTO_VERSION
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;
