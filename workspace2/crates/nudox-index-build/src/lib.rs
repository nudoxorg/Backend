#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
//! Allocation-free, compiler-authoritative construction of existing immutable index segments.
//!
//! This crate owns only the transformation from a reopened compiler publication to
//! `nudox-index-core` rows and proofs. It does not invent another segment identity grammar: exact
//! retrieval, Tantivy, and snapshot publication consume the same `ExactSegment` and
//! `LexicalSegment` returned here.

mod build;
mod error;
mod fact;
mod initialized;

pub use build::{
    IndexBuildCapacity, IndexBuildScratch, MAX_INDEX_ROWS, PreparedIndex, PreparedIndexView, build,
    preflight,
};
pub use error::{BuildAdmissionError, BuildDerivationError, BuildError, BuildRegion};
pub use fact::{
    ENTITY_VALUE_BYTES, EXACT_ENTITY_KEY_BYTES, EntityFact, EntityFactView, EntityProjection,
    ExactEntityKey, ExactEntityValue, ExactEntityValueError, ExactEntityValueView,
};
