//! The `operation` module composes hydration, compilation, publication, and storage into public operations.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Static operation contracts and the Wave 1 pinned-object cursor.

mod contract;
mod pinned_object;

pub use contract::{BatchSource, Operation, Provider, SourcePoll, TerminalSummary};
pub use pinned_object::{
    BoundLocalObjectProvider, BoundLocalObjectRun, LocalObjectError, LocalObjectProvider,
    LocalObjectRun, MissingObject, ObjectBatch, ObjectProvenance, PinnedObjectOperation,
    PinnedObjectRequest, VerifiedObjectBindError,
};
