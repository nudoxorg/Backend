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
mod arrangement;
mod contracts;
mod delta;
mod identity;
mod provider;
mod query;
pub mod server;

pub use admission::{AdapterError, Error, Projection, QueryInput, execute, incomplete_coverage};
pub use arrangement::{
    ArrangementLimits, ArrangementPlan, GraphArrangement, GraphBase, GraphOverlay, QueryPlan,
    RefreshKind, RefreshOutcome,
};
pub use contracts::{Cursor, GraphChange, GraphRow, Query, Read};
pub use delta::{GraphDelta, GraphState};
pub use identity::{
    Authority, AuthoritySchema, Binding, Frontier, FrontierSchema, Limits, QueryVersion,
    ReadManifest, ReadManifestSchema, Recipe, RecipeSchema, Root, SchemaVersion, SemanticRelation,
};
pub use provider::{Adapter, GraphPage, GraphSource, MemorySource, QueryRequest, QueryResult};
pub use query::{
    BoundSemanticQueryRow, CompilerExternalTargetEvidence, CompilerSemanticEvidence,
    PackageScopeEvidence, QueryError as SemanticQueryError, SemanticQueryCancelHandle,
    SemanticQueryCancellation, SemanticQueryCorpus, SemanticQueryEvent, SemanticQueryEvidence,
    SemanticQueryFact, SemanticQueryIdentity, SemanticQueryPresentation, SemanticQueryRequest,
    SemanticQueryStream, SemanticQueryTerminal, StructuralFallbackEvidence, execute_semantic_query,
    schema as semantic_query_schema,
};
pub use trustfall::{FieldValue, QueryResult as SemanticQueryRow, TransparentValue};

#[cfg(test)]
mod tests;
