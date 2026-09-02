//! The `server-index-build` crate exists to project reopened compiler IR into immutable exact and lexical segments.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
//! Allocation-free, compiler-authoritative construction of existing immutable index segments.
//!
//! This crate owns only the transformation from a reopened compiler publication to
//! `server-index-core` rows and proofs. It does not invent another segment identity grammar: exact
//! retrieval, Tantivy, and snapshot publication consume the same `ExactSegment` and
//! `LexicalSegment` returned here.

mod construct;
mod error;
mod fact;
mod initialized;

pub use construct::{
    IndexBuildCapacity, IndexBuildScratch, MAX_INDEX_ROWS, PreparedIndex, PreparedIndexView, build,
    preflight,
};
pub use error::{BuildAdmissionError, BuildDerivationError, BuildError, BuildRegion};
pub use fact::{
    ENTITY_VALUE_BYTES, EXACT_ENTITY_KEY_BYTES, EntityFact, EntityFactView, EntityProjection,
    ExactEntityKey, ExactEntityValue, ExactEntityValueError, ExactEntityValueView,
};
