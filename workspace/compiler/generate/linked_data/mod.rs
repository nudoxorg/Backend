//! Processing the IR into linked data (JSON-LD) for the graph store.
//!
//! The document *schema* is derived from the graph model (`crate::graph`) via
//! `terminusdb-schema-derive`; this module owns the [`schema`] context and the
//! streaming [`emit`] pipeline that projects an `ir::Index` into graph documents
//! and ships them to a sink in bounded batches (never a whole-corpus `Vec`).

pub mod emit;
pub mod schema;

pub use emit::{DocumentSink, emit};
