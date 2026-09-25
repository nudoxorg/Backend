//! The ranking plane: signal extraction, the scoring cascade, safety gates,
//! rank fusion, per-ecosystem interleave, and evaluation.
//!
//! Retrieval and query parsing stay in the `search` root ([`super::pipeline`],
//! [`super::structured`], [`super::tantivy`], [`super::alias`], [`super::usages`],
//! [`super::spell`]); everything that turns retrieved candidates into an ordered,
//! gated, de-duplicated page lives here.
//!
//! The five-stage scoring cascade is [`cascade`]; the other modules are the
//! signals and passes it composes.

pub mod cascade;
mod fuse;
mod passes;
// The cascade is the public face of the ranking plane; re-export its surface so
// `ranking::Candidate` (etc.) resolves for callers that don't reach into the
// `cascade` submodule by name.
pub use cascade::*;

pub mod dependents;
pub mod enrich;
pub mod entity;
pub mod eval;
pub mod gates;
pub mod intent;
pub mod interleave;
pub mod listing_signals;
pub mod local_enrichment;
pub mod multi_parent;
pub mod policy;
pub mod popularity;
pub mod rrf;
pub mod squat;
