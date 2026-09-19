//! Portable product contracts for the local-first versioned library.
//!
//! The crate façade exposes the stable product vocabulary. Versioned catalog
//! behavior lives in the catalog module, canonical identity schemas and
//! admission in [`canonical`], immutable view state in the view module, and
//! transport DTOs in the protocol module. None of these modules owns a process, socket, filesystem,
//! scheduler, or native compiler implementation.

#![deny(unsafe_code)]

mod arrangement;
pub mod canonical;
mod catalog;
mod command;
mod cursor;
mod delta;
mod error;
mod protocol;
mod view;

/// Maximum number of events admitted from one bounded subscription payload.
pub const MAX_SUBSCRIPTION_EVENTS: usize = 256;

pub use arrangement::QueryWork;
pub use backend_compile::{DeclarationKind, SourceLocation};
pub use backend_semantic::{Read, ReadManifest, ReadSelector};
pub use backend_version::{
    AdmittedProducerObservation, AuthorityScopeClaim, AuthorizedCompleteCoverage, CoverageWitness,
    IdAdmissionError, IdContext, IdDecodeError, ProducerObservationVerifier, Relation,
    RelationState, Schema, ScopeRoot, UntrustedId, UntrustedProducerObservation, WireId,
    admit_complete_scope, admit_producer_observation,
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
pub use catalog::Library;
pub use command::{
    Command, CommandId, CommandReply, DocumentQuery, GraphQuery, NameQuery, OutlineQuery, Query,
    QueryLimit, QueryRecord, RevisionReceipt, ViewRevision,
};
pub use cursor::{
    CURSOR_CONTROL_BYTES, CURSOR_SCHEMA, Cursor, CursorError, CursorEvent, CursorRead,
    CursorResetReason, CursorSub,
};
pub use delta::{Delta, Intent, IntentError, IntentLog, IntentReceipt, Settings};
pub use error::LibraryError;
pub use protocol::{
    CommandDto, DTO_VERSION, EventDto, MAX_COMMAND_TEXT, ReplyAdmissionError, ReplyDto,
    RequestAdmissionError, SnapshotHydrator, SnapshotPageClaim, SnapshotPageDto, SubscriptionDto,
    ViewDto, WireCertificate, WireClaim, WireSchema, admit_reply, admit_reply_with_capability,
    admit_request, command_request_id, decode_command_body, decode_compact_view_event,
    decode_reply_body, decode_reply_body_with_verifier, encode_command_body,
    encode_compact_subscription, encode_compact_view_event, encode_view_root_descriptor,
    reply_memory_bound,
};
pub use view::{
    Basis, CommittedViewDelta, CompleteViewProjection, Coverage, CoverageCapability, Document,
    Fragment, Freshness, Lane, MAX_COVERAGE_EVIDENCE, MAX_SNAPSHOT_PAGE_ROWS, MAX_VIEW_PATCH_ROWS,
    NameRecord, Outline, OutlineNode, PreparedViewDelta, Reason, Row, RowChange, RowId, RowState,
    ViewDelta, ViewError, ViewPageCursor, ViewPageError, ViewProjection, ViewProjectionError,
    ViewRoot, ViewRootDescriptor, ViewRootDescriptorClaim, ViewSnapshot, ViewSnapshotPage,
};

/// Returns the current command/reply/event wire schema version.
#[must_use]
pub const fn protocol_version() -> u16 {
    DTO_VERSION
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;
