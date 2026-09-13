//! Provider-facing ANN request, page, and result contracts.

use crate::contracts::SearchQuality;
use crate::incremental::vector::VectorQuery;
use crate::{Binding, CandidateId, QueryVersion, SchemaVersion};
use backend_version::CoverageWitness;

/// Exact identity of query coordinates, base authority, and requested result shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorQueryBinding {
    /// Immutable ANN base selected for this query.
    pub base: Binding,
    /// Cryptographic identity of base, coordinates, model, and metric.
    pub query: QueryVersion,
    /// Final number of results requested by the caller.
    pub requested: usize,
}

impl VectorQueryBinding {
    pub(crate) fn new(
        base: Binding,
        query: &VectorQuery,
        requested: usize,
    ) -> Result<Self, crate::Error> {
        let requested_u64 = u64::try_from(requested).map_err(|_| crate::Error::SizeLimit)?;
        let dimensions =
            u64::try_from(query.values().len()).map_err(|_| crate::Error::SizeLimit)?;
        let mut value = Vec::with_capacity(256 + query.values().len().saturating_mul(4));
        value.extend_from_slice(b"backend.qdrant.vector-query.v1\0");
        value.extend_from_slice(base.workspace.as_bytes());
        value.extend_from_slice(base.root.as_bytes());
        value.extend_from_slice(base.recipe.as_bytes());
        value.extend_from_slice(base.authority.as_bytes());
        value.extend_from_slice(base.read_manifest.as_bytes());
        value.extend_from_slice(base.frontier.as_bytes());
        value.extend_from_slice(query.model().as_bytes());
        value.push(query.metric() as u8);
        value.extend_from_slice(&dimensions.to_be_bytes());
        for coordinate in query.values() {
            value.extend_from_slice(&coordinate.to_bits().to_be_bytes());
        }
        value.extend_from_slice(&requested_u64.to_be_bytes());
        Ok(Self {
            base,
            query: QueryVersion::from_value(&value),
            requested,
        })
    }
}

/// Provider continuation that cannot be replayed for another vector query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnCursor {
    binding: VectorQueryBinding,
    offset: usize,
}

impl AnnCursor {
    /// Creates a continuation for a provider response.
    #[must_use]
    pub const fn new(binding: VectorQueryBinding, offset: usize) -> Self {
        Self { binding, offset }
    }
    /// Query identity carried by this continuation.
    #[must_use]
    pub const fn binding(self) -> VectorQueryBinding {
        self.binding
    }
    /// Provider-relative result offset.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }
}

/// Request given to an ANN provider for its immutable base root.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorSearchRequest {
    /// Query/base/result-shape identity; fresh overlay is merged by the local owner.
    pub binding: VectorQueryBinding,
    /// Model/metric-bound query vector.
    pub query: VectorQuery,
    /// Maximum provider candidates requested.
    pub limit: usize,
    /// Query-bound continuation for a later provider page.
    pub cursor: Option<AnnCursor>,
}

/// ANN provider page before local admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnPage {
    /// Provider schema tag.
    pub schema: SchemaVersion,
    /// Exact immutable base binding.
    pub binding: VectorQueryBinding,
    /// Candidate IDs in provider order.
    pub ids: Vec<CandidateId>,
    /// Provider page cursor, if one exists.
    pub next: Option<AnnCursor>,
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
