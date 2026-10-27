//! Local-first query coordination over one immutable published view.
//!
//! Local lexical search closes the selected view before any optional provider
//! work starts. Semantic acceleration consumes that complete local answer and
//! may contribute only locally resolved canonical IDs under a bounded policy,
//! so provider availability and recall never become coverage authority.

mod candle_acquisition;
pub(in crate::builtin) mod controlled_sort;
mod embedding_cache;
mod local;
mod local_preset;
mod local_recipe;
pub(crate) mod local_semantic;
mod local_semantic_owner;
mod projection_state;
mod remote;
mod semantic;

pub use candle_acquisition::{DownloadedCandleArtifacts, download_candle_artifacts};
pub(crate) use local::SelectionPreparationDiagnostic;
pub use local::{
    CoverageBasis, Freshness, Lane, LaneReport, LeftOut, LexicalFailureCause, LexicalPhase,
    LocalAnswer, LocalQuery, QueryCoordinator, QueryError, QueryResult, RankedRow,
    SearchSnapshotOwner, SemanticDocument, SourceBasis,
};
pub use local_preset::{
    CandleArtifact, CandleSetupError, PinnedCandleSetupRequest, PreparedCandleSetup,
    admit_candle_setup_authority, inspect_candle_helper,
};
pub use local_recipe::{
    AdmittedLocalQueryRecipe, LocalEmbeddingTreatment, LocalQueryRecipeContract,
    LocalQueryRecipeDescriptor, LocalQueryRecipeError,
};
pub use local_semantic::{LocalSemantic, LocalSemanticError, ProjectionAllocation};
pub use local_semantic_owner::{LocalSemanticOwner, QueryCompletionPolicy};
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
    search_page_inner(coordinator, library, remote, coverage, query, None)
}

/// Uses the actual selected-owner operation flag; it does not represent client disconnect.
pub(crate) fn search_page_with_cancellation(
    coordinator: &QueryCoordinator,
    library: &backend_library::Library,
    provider: &mut RemoteSemantic,
    coverage: backend_version::CoverageWitness,
    query: &backend_engine::Query,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<
    (
        backend_engine::ViewSnapshot,
        backend_library::SemanticSearchStatus,
    ),
    SearchPageError,
> {
    search_page_inner(
        coordinator,
        library,
        provider,
        coverage,
        query,
        Some(cancelled),
    )
}

fn search_page_inner(
    coordinator: &QueryCoordinator,
    library: &backend_library::Library,
    remote: &mut RemoteSemantic,
    coverage: backend_version::CoverageWitness,
    query: &backend_engine::Query,
    cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<
    (
        backend_engine::ViewSnapshot,
        backend_library::SemanticSearchStatus,
    ),
    SearchPageError,
> {
    // Retire a completed selected projection even if the initial scan refuses.
    remote.poll_local(coordinator.workspace_root(), coordinator.corpus.view.root());
    let local = match cancelled.as_ref() {
        Some(selected) => coordinator.search_local_page_controlled(query, &mut || {
            if selected.load(std::sync::atomic::Ordering::Acquire) {
                Err(QueryError::SelectedViewRetired)
            } else {
                Ok(())
            }
        }),
        None => coordinator.search_local_page(query),
    }
    .map_err(SearchPageError::Local)?;
    let answer = if remote.is_local() {
        remote.search_local_with_cancellation(coordinator, coverage, local, query.text(), cancelled)
    } else {
        let reconciliation_failed = remote.reconcile(coordinator, coverage).is_err();
        let (result, status) = remote.search_with_status(
            coordinator,
            coverage,
            local,
            query.text(),
            reconciliation_failed,
        );
        local_semantic_owner::SemanticAnswer {
            result,
            status,
            _memory: None,
        }
    };
    let local_semantic_owner::SemanticAnswer {
        result,
        status,
        _memory: _semantic_memory,
    } = answer;
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
