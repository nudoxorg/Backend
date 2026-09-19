//! Provider-facing ANN request, page, and result contracts.

use crate::contracts::SearchQuality;
use crate::incremental::vector::VectorQuery;
use crate::{Binding, CandidateId, Cursor, SchemaVersion};
use backend_version::CoverageWitness;

/// Request given to an ANN provider for its immutable base root.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorSearchRequest {
    /// Exact base binding; fresh overlay is merged by the local owner.
    pub binding: Binding,
    /// Model/metric-bound query vector.
    pub query: VectorQuery,
    /// Maximum provider candidates requested.
    pub limit: usize,
}

/// ANN provider page before local admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnPage {
    /// Provider schema tag.
    pub schema: SchemaVersion,
    /// Exact immutable base binding.
    pub binding: Binding,
    /// Candidate IDs in provider order.
    pub ids: Vec<CandidateId>,
    /// Provider page cursor, if one exists.
    pub next: Option<Cursor>,
    /// Coverage of the selected base scope.
    pub coverage: CoverageWitness,
    /// Explicit exact/approximate quality.
    pub quality: SearchQuality,
}

/// Provider boundary for immutable ANN base search.
pub trait AnnSource {
    /// Provider-specific failure.
    type Error;

    /// Searches only the requested immutable base binding.
    ///
    /// # Errors
    ///
    /// Returns the provider's transport or backend-specific failure.
    fn fetch(&self, request: &VectorSearchRequest) -> Result<AnnPage, Self::Error>;
}

/// One exact reranked candidate and its deterministic score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScoredCandidate {
    /// Logical candidate identity.
    pub id: CandidateId,
    /// Lower-is-better metric score.
    pub score: f32,
}

/// Search result after base/overlay union and exact local reranking.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorSearchResult {
    /// Current exact binding, including fresh overlay root.
    pub binding: Binding,
    /// Base coverage retained through the overlay union.
    pub coverage: CoverageWitness,
    /// Deterministically reranked candidates.
    pub candidates: Vec<ScoredCandidate>,
    /// Provider quality; reranking cannot upgrade approximate recall.
    pub quality: SearchQuality,
}

impl VectorSearchResult {
    /// Returns the exact current result binding.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }
}
