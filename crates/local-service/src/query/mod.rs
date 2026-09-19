//! Local-first query coordination over one immutable published view.
//!
//! Local lexical search closes the selected view before any optional provider
//! work starts. Semantic acceleration consumes that complete local answer and
//! may contribute only locally resolved canonical IDs under a bounded policy,
//! so provider availability and recall never become coverage authority.

mod local;
mod remote;
mod semantic;

pub use local::{
    CoverageBasis, Freshness, Lane, LaneReport, LocalAnswer, LocalQuery, QueryCoordinator,
    QueryError, QueryResult, RankedRow, SearchSnapshotOwner, SemanticDocument, SourceBasis,
};
pub use remote::{
    ActiveQdrant, ConfiguredQdrant, EMBEDDING_DIMENSIONS_ENV, EMBEDDING_DOCUMENT_TREATMENT_ENV,
    EMBEDDING_MODEL_ENV, EMBEDDING_MODEL_FILE_ENV, EMBEDDING_PROGRAM_ENV,
    EMBEDDING_QUERY_TREATMENT_ENV, EMBEDDING_TOKENIZER_ENV, EMBEDDING_TOKENIZER_FILE_ENV,
    QDRANT_API_KEY_ENV, QDRANT_COLLECTION_ENV, QDRANT_ENDPOINT_ENV, QdrantDocument,
    RemoteConfigError, RemoteSemantic,
};
pub use semantic::{CompositionPolicy, SemanticAcceleration, SemanticError};

#[cfg(test)]
mod tests;
