//! The JSON-LD schema/context for the emitted linked data.
//!
//! The document *shapes* are derived from the graph model (`crate::graph::model`)
//! via `terminusdb-schema-derive`, so this module does not hand-write a schema.
//! It provides the JSON-LD `@context` that binds the model's field names to the
//! graph store's vocabulary, and the base IRI prefixes documents resolve against.

use serde_json::Value;

/// The base IRI every emitted document's `@id` is minted under.
pub const BASE_IRI: &str = "https://nudox.org/ir/";

/// The JSON-LD `@context` for the emitted graph documents — the field→predicate
/// bindings the graph store ingests against. Derived from the graph model's
/// schema so it can never drift from what [`super::emit`] serializes.
pub fn context() -> Value {
	todo!("assemble the @context from the graph model's derived schema + BASE_IRI")
}
