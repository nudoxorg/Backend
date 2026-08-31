//! Defines pack error behavior for `server-index-publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack error invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed diagnostics split by pack encoding, owner validation, and filesystem publication.

mod encode;
mod open;
mod store;

pub use encode::IndexPackEncodeError;
pub use open::{IndexPackOpenError, RejectedIndexPack};
pub use store::{
    IndexPackCleanup, IndexPackConflict, IndexPackPathRole, IndexPackStoreError,
    IndexPackStorePhase, StoredIndexPack,
};

/// One independently encoded immutable segment lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexPackLane {
    /// Sorted exact-key segment bodies.
    Exact,
    /// Sorted lexical term/document segment bodies.
    Lexical,
}

/// One closed physical region in the index-pack grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexPackRegion {
    /// The fixed pack header.
    Header,
    /// The fixed directory record for a selected segment.
    Directory(IndexPackLane),
    /// One variable-width selected segment body.
    Segment(IndexPackLane),
    /// One variable-width row inside a selected segment body.
    Row(IndexPackLane),
}

/// A semantic row invariant rejected after structural row decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexPackRowInvariant {
    /// Row count exceeded the existing core segment bound.
    RowLimit,
    /// Row payload sizes exceeded the existing core segment byte budget.
    PayloadLimit,
    /// A key or term/document pair was not strictly canonical.
    Order,
    /// One key or term/document pair occurred twice.
    Duplicate,
}
