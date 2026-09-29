//! Bounded, version-aware replication transport.
//!
//! Replication owns wire claims, negotiation, immutable object transfer, and
//! generic envelopes. Execution retains ownership of admitted recipe, read,
//! authority, work-key, output-equivalence, receipt, and fence semantics.
#![deny(unsafe_code)]
// Test fixtures intentionally use compact `expect` assertions to keep each
// protocol invariant local. Production code remains subject to the workspace
// panic/expect lints above; this test-only allowance keeps the all-target
// quality gate focused on implementation diagnostics.
#![cfg_attr(test, allow(clippy::expect_used, clippy::too_many_lines))]

mod authority;
mod codec;
mod coverage;
mod execution;
mod identities;
mod ir_generation_store;
mod ir_hydration;
mod ir_hydration_store;
mod ir_hydration_wire;
mod ir_image_store;
mod ir_producer_store;
mod ir_residency;
mod local_peer;
mod negotiation;
mod reconcile;
mod transfer;
mod transport;
mod unix_endpoint;

pub use authority::*;
pub use codec::{decode_message, encode_message};
pub use coverage::*;
pub use execution::*;
pub use identities::*;
pub use ir_generation_store::{
    AdmittedHistoryCommit, HistoricalSemanticPlaneBinding, HistoryAdmissionReceipt,
    HistoryCommitId, HistoryGcProgress, HistoryGcStats, HistoryGenerationRoot,
    HistoryMaterialization, HistoryProposalError, HistoryRefAncestryProof, HistoryRefKind,
    HistoryRefName, HistoryRefUpdateReceipt, HistoryReplay, HistoryReplayCursor,
    HistoryReplayEntry, HistorySegmentDeltas, HistoryTypedV2JumboObject, HistoryTypedV2LocatorId,
    HistoryTypedV2RootClaim, HistoryTypedV2SegmentObject, LocalSemanticGeneration,
    LocalSemanticGenerationId, MAX_HISTORY_REPLAY_COMMITS, SelectedHistoryRef,
    TypedV2HistoryReplay, UnpublishedHistoryProposal,
};
pub use ir_hydration::*;
pub use ir_hydration_store::*;
pub use ir_hydration_wire::*;
pub use ir_image_store::{SemanticImageCacheError, SemanticImageResume};
pub use ir_producer_store::{
    DurableSemanticObjectAdmission, DurableSemanticObjectPin, FileSemanticJumboRopeSink,
    FileSemanticPlaneSegmentSink, ProducedSemanticObjectIdentity, ProducedSemanticObjectKind,
    SemanticObjectAdmissionBuffer, SemanticObjectAdmissionSink, SemanticProducerStoreMetrics,
};
pub use ir_residency::*;
pub use local_peer::*;
pub use negotiation::*;
pub use reconcile::*;
pub use transfer::*;
pub use transport::*;
pub use unix_endpoint::*;

#[cfg(test)]
mod tests;
