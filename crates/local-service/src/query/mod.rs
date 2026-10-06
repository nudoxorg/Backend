//! Local-first query coordination over one immutable published view.
//!
//! Local lexical search closes the selected view before any optional provider
//! work starts. Semantic acceleration consumes that complete local answer and
//! may contribute only locally resolved canonical IDs under a bounded policy,
//! so provider availability and recall never become coverage authority.

mod embedding_cache;
mod local;
mod projection_state;
mod remote;
mod semantic;

pub use local::{
    CoverageBasis, Freshness, Lane, LaneReport, LeftOut, LexicalFailureCause, LexicalPhase,
    LocalAnswer, LocalQuery, QueryCoordinator, QueryError, QueryResult, RankedRow,
    SearchSnapshotOwner, SemanticDocument, SourceBasis,
};
pub use remote::{
    ActiveQdrant, ConfiguredQdrant, EMBEDDING_DEVICE_ENV, EMBEDDING_DIMENSIONS_ENV,
    EMBEDDING_DOCUMENT_TREATMENT_ENV, EMBEDDING_MODEL_ENV, EMBEDDING_MODEL_FILE_ENV,
    EMBEDDING_PROGRAM_ENV, EMBEDDING_PROTOCOL_ENV, EMBEDDING_QUERY_TREATMENT_ENV,
    EMBEDDING_TOKENIZER_ENV, EMBEDDING_TOKENIZER_FILE_ENV, QDRANT_API_KEY_ENV,
    QDRANT_COLLECTION_ENV, QDRANT_ENDPOINT_ENV, QdrantDocument, RemoteConfigError, RemoteSemantic,
};
pub use semantic::{CompositionPolicy, SemanticAcceleration, SemanticError};

/// Failure retained at the selected search-page application seam.
#[derive(Debug)]
pub(crate) enum SearchPageError {
    Local(QueryError),
    Projection(backend_library::LibraryError),
}

impl std::fmt::Display for SearchPageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(error) => error.fmt(formatter),
            Self::Projection(error) => error.fmt(formatter),
        }
    }
}

/// Projects the exact product route after the owner selects its immutable
/// corpus. Optional semantic work and library cursor authority stay intact.
pub(crate) fn search_page(
    coordinator: &QueryCoordinator,
    library: &backend_library::Library,
    remote: &mut RemoteSemantic,
    coverage: backend_version::CoverageWitness,
    query: &backend_engine::Query,
) -> Result<
    (
        backend_engine::ViewSnapshot,
        backend_library::SemanticSearchStatus,
    ),
    SearchPageError,
> {
    let local = coordinator
        .search_local_page(query)
        .map_err(SearchPageError::Local)?;
    let reconciliation_failed = remote.reconcile(coordinator, coverage).is_err();
    let (result, status) = remote.search_with_status(
        coordinator,
        coverage,
        local,
        query.text(),
        reconciliation_failed,
    );
    let ranked_ids = result
        .rows
        .iter()
        .map(|ranked| ranked.row.id)
        .collect::<Vec<_>>();
    let page = library
        .search_from_ranked_ids(query, &ranked_ids)
        .map_err(SearchPageError::Projection)?;
    Ok((page, status))
}

/// Times admitting a typed search corpus against reusing the resident one.
pub(super) fn measure_search_corpus() {
    local::measure_search_corpus();
}

#[cfg(test)]
mod tests;
