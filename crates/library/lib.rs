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
    DEFAULT_IGNORED_DIRECTORIES, DiscoveryError, DiscoveryPolicy, DiscoveredEntry, EntryKind,
    HARD_IGNORED_DIRECTORIES, is_hard_ignored_directory, is_hard_ignored_path,
};

mod arrangement;
pub mod canonical;
mod capability;
mod catalog;
mod command;
mod command_registry;
mod cursor;
mod delta;
mod error;
mod graph_query;
mod package_graph;
/// Transport-independent application service and reply vocabulary.
pub mod interface;
mod progress;
/// Bounded transport decoding and presentation for thin CLI and MCP consumers.
pub mod protocol;
mod surface;
mod view;
mod wire;

/// Maximum number of events admitted from one bounded subscription payload.
pub const MAX_SUBSCRIPTION_EVENTS: usize = 256;

pub use arrangement::QueryWork;
pub use backend_compile::{
    DeclarationKind, SourceExcerpt, SourceExcerptExtent, SourceLanguage, SourceLocation,
};
pub use backend_advisory::{
    AdvisoryCategory, AdvisoryCoverage, AdvisoryDecisionDto, AdvisoryPackageDto,
    AdvisoryStatus, AdvisorySurfaceDto, AffectedRange, AcquisitionDecision, FreshnessState,
    NativeAdvisoryId, OverrideEvidence, PolicyReason, SeverityLevel,
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
    PROTOCOL_SCHEMA, PackageKey, SemanticObject, SymbolKey, ViewDeltaId, ViewRecipeId,
    ViewRecipeSchema, ViewRelation, ViewStateRoot, ViewVersion, ViewVersionSchema, actor_key,
    admit_delta_against, admit_delta_transition, admit_intent_value, admit_key_against,
    admit_key_value, admit_root_against, admit_root_bytes, admit_version_against,
    admit_version_value, branch_key, decode_id, empty_view_relation_preimage, encode_id, intent_id,
    log_key, object_version, package_key, relation_row_key, symbol_key, view_identity_bytes,
    view_key, view_state_root, view_version, view_version_preimage, wire_delta, wire_intent,
    wire_key, wire_root, wire_version,
};
pub use capability::{
    CapabilityAuthority, CapabilityFamily, CapabilityId, CapabilityInventory,
    CapabilityInventoryError, CapabilityLifecycle, CapabilityStatus, CapabilityTarget,
    CapabilityUnavailable, EmbeddingCapabilityRecipe, EmbeddingEncoding, EmbeddingMetric,
    EmbeddingNormalization, EmbeddingPooling, EmbeddingRecipeId, EmbeddingSource,
    LanguageOracleTask, MAX_CAPABILITY_INVENTORY, PackageAuthorityIdentity,
    compiler_authority_recipe, embedding_authority_recipe,
};
pub use catalog::{Library, RankedSearchSnapshot};
pub use command::ReferenceFact;
pub use command::{
    Command, CommandFailure, CommandId, CommandReply, DocumentQuery, GraphNeighborhoodQuery,
    HealthReport, NameQuery, OutlineQuery, PageContinuation, PageRequest, PageTerminal,
    ProjectionPage, Query, QueryLimit, QueryRecord, RevisionReceipt, SymbolAddress, ViewRevision,
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
pub use graph_query::{
    GraphQueryControl, GraphQueryError, GraphQueryPage, GraphQueryRequest, GraphQueryRow,
    GraphValue, MAX_GRAPH_QUERY_BYTES, MAX_GRAPH_QUERY_FIELDS, MAX_GRAPH_VALUE_BYTES,
    MAX_GRAPH_VALUE_DEPTH,
};
pub use package_graph::{
    admit_dependency_rows, DependencyAuthority, DependencyEvidence, DependencyFacts,
    discover_source_entries, discover_source_files, source_selection_policy, DependencyScope,
    PackageDependencyRecord, PackageDependencySourceFacts, PackageDependencyTarget,
    MAX_PACKAGE_GRAPH_ROWS,
};
pub use progress::{
    FaultRows, IngestProgress, LanguageRows, MAX_PROGRESS_FAULTS, MAX_PROGRESS_LANGUAGES,
    ProgressError, SourceUnavailableReason,
};
pub use surface::{
    DeclarationChange, DeclarationRecord, DiffRecord, MAX_PRODUCT_ROWS, MAX_PRODUCT_TEXT_BYTES,
    PackageCoordinate, PackageReference, ProductAdmissionError, ProductText, ProjectId,
    ProjectName, ProjectRecord, ProjectSelector, ReferenceRecord, RegistryDownloadCount,
    RegistryEcosystem, RegistryMetadata, RegistryPackageRecord, RegistryReleaseStanding,
    ReleaseRecord, SemanticConfidence, SemanticDeclarationIdentity,
    SemanticGenerationId, SemanticLanguageProfile, SemanticLinkDelta, SemanticLinkEvidence,
    SemanticLinkKind, SemanticLinkTarget, SemanticSourceSpan, SemanticVersionRecord,
    SubscriptionRecord, SurfaceCommand, SurfaceReply, TreeNodeId, TreeNodeRecord, TreeOpener,
    TreeSubject,
};
pub use view::{
    Basis, CommittedViewDelta, CompleteViewProjection, Coverage, CoverageCapability, Document,
    Fragment, Freshness, GraphRelation, Lane, MAX_COVERAGE_EVIDENCE, MAX_SNAPSHOT_PAGE_ROWS, MAX_VIEW_PATCH_ROWS,
    NameRecord, Outline, OutlineExtent, OutlineNode, PreparedViewDelta, Reason, Row, RowChange,
    RowId, RowState, SourceAvailability, ViewDelta, ViewError, ViewPageCursor, ViewPageError,
    ViewProjection, ViewProjectionError, ViewRoot, ViewRootDescriptor, ViewRootDescriptorClaim,
    ViewSnapshot, ViewSnapshotPage,
};
pub use wire::{
    CommandDto, DTO_VERSION, EventDto, MAX_COMMAND_BODY, MAX_COMMAND_TEXT, ReplyAdmissionError,
    ReplyDto, RequestAdmissionError, SnapshotHydrator, SnapshotPageClaim, SnapshotPageDto,
    SubscriptionDto, ViewDto, WireCertificate, WireClaim, WireSchema, admit_reply,
    admit_reply_with_capability, admit_request, command_request_id, decode_command_body,
    decode_command_body_for_owner, decode_compact_view_event, decode_reply_body,
    decode_reply_body_with_verifier, encode_command_body, encode_compact_subscription,
    encode_compact_view_event, encode_view_root_descriptor, reply_memory_bound,
};

/// Returns the current command/reply/event wire schema version.
#[must_use]
pub const fn protocol_version() -> u16 {
    DTO_VERSION
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;
