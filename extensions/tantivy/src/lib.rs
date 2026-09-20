//! Bounded lexical-index extension contracts.
//!
//! The lexical index is a derived materialization of versioned document facts.
//! This crate owns admission and deterministic paging, while a concrete search
//! engine implements [`LexicalSource`]. The source never becomes the semantic
//! identity or workspace authority.
//!
//! Provider pages and results carry one [`Binding`] with the exact workspace,
//! relation root, recipe, read manifest, and completed frontier, together with
//! a [`backend_version::CoverageWitness`]. This optional leaf owns no workspace
//! head, snapshot lifecycle, or compare-and-set authority.
#![forbid(unsafe_code)]

mod admission;
mod contracts;
mod delta;
mod identity;
mod incremental;
mod provider;

pub use admission::{
    AdapterError, Error, Materialization, incomplete_coverage, materialize,
    materialize_with_limits, query,
};
pub use contracts::{Cursor, Query};
pub use delta::{DocumentChange, DocumentDelta, DocumentState};
pub use identity::{
    Authority, AuthoritySchema, Binding, Frontier, FrontierSchema, IndexRelation, Limits,
    QueryVersion, ReadManifest, ReadManifestSchema, Recipe, RecipeSchema, Root, SchemaVersion,
};
pub use incremental::{
    LexicalBase, LexicalOverlay, LexicalView, OverlayLimits, RefreshKind, RefreshOutcome,
    RefreshPlan,
};
pub use provider::{Adapter, LexicalPage, LexicalSource, MemorySource, QueryRequest, QueryResult};

#[cfg(test)]
mod tests;
