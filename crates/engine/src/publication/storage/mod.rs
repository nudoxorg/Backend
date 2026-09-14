//! Defines storage behavior for the `backend-engine` publication, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the storage invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed namespace naming and single-owner immutable filesystem transitions.

mod io;
mod namespace;
mod transition;
mod types;

pub(crate) use namespace::StorageNamespace;
pub(crate) use transition::ImmutableFileStore;
pub(crate) use types::StoredFile;
pub use types::{ImmutableFileError, ImmutableIoPhase};
