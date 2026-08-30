#![no_std]
#![forbid(unsafe_code)]
//! Static operation contracts and the Wave 1 pinned-object cursor.

mod contract;
mod pinned_object;

pub use contract::{BatchSource, Operation, Provider, SourcePoll, TerminalSummary};
pub use pinned_object::{
    BoundLocalObjectProvider, BoundLocalObjectRun, LocalObjectError, LocalObjectProvider,
    LocalObjectRun, MissingObject, ObjectBatch, ObjectProvenance, PinnedObjectOperation,
    PinnedObjectRequest, VerifiedObjectBindError,
};
