//! Bounded graph-query extension contracts.
//!
//! The extension supplies a query/read boundary and deterministic in-memory
//! source for local execution and tests. A concrete graph query implementation
//! can implement [`GraphSource`], but it cannot publish a semantic root or
//! bypass the exact workspace, recipe, read-manifest, and coverage checks.
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
mod provider;

pub use admission::{
    AdapterError, Error, Projection, QueryInput, complete_coverage, execute, incomplete_coverage,
};
pub use contracts::{Cursor, GraphChange, GraphRow, Query, Read};
pub use delta::{GraphDelta, GraphState};
pub use identity::{
    Authority, AuthoritySchema, Binding, Frontier, FrontierSchema, Limits, QueryVersion,
    ReadManifest, ReadManifestSchema, Recipe, RecipeSchema, Root, SchemaVersion, SemanticRelation,
};
pub use provider::{Adapter, GraphPage, GraphSource, MemorySource, QueryRequest, QueryResult};

#[cfg(test)]
mod tests;
