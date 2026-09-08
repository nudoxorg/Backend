//! Provider request, page, and result contracts.

use crate::Binding;
use crate::identity::{CandidateId, ModelVersion, Recipe, SchemaVersion};
use backend_version::CoverageWitness;

/// Recall evidence for an approximate ANN page, expressed in integer
/// thousandths of a percent to avoid an unbounded or non-deterministic float.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecallMetadata {
    /// Requested recall target in parts per million of the exact baseline.
    pub target_ppm: u32,
    /// Measured recall, when an evaluation has supplied one.
    pub observed_ppm: Option<u32>,
}

impl RecallMetadata {
    /// The largest representable recall value (100%).
    pub const FULL_SCALE: u32 = 1_000_000;

    /// Creates bounded recall metadata.
    ///
    /// # Errors
    ///
    /// Returns `Error::MalformedInput` if either value exceeds 100%.
    pub const fn new(target_ppm: u32, observed_ppm: Option<u32>) -> Result<Self, crate::Error> {
        if target_ppm == 0
            || target_ppm > Self::FULL_SCALE
            || match observed_ppm {
                Some(value) => value > Self::FULL_SCALE,
                None => false,
            }
        {
            Err(crate::Error::MalformedInput)
        } else {
            Ok(Self {
                target_ppm,
                observed_ppm,
            })
        }
    }

    pub(crate) fn is_valid(self) -> bool {
        self.target_ppm > 0
            && self.target_ppm <= Self::FULL_SCALE
            && self
                .observed_ppm
                .is_none_or(|value| value <= Self::FULL_SCALE)
    }
}

/// Explicit quality metadata for an ANN result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApproximationMetadata {
    /// Recipe that selected the approximate algorithm and parameters.
    pub recipe: Recipe,
    /// Immutable embedding/model revision used by the provider.
    pub model: ModelVersion,
    /// Declared recall target and optional measured recall.
    pub recall: RecallMetadata,
}

impl ApproximationMetadata {
    /// Creates metadata for one approximate recipe/model evaluation.
    #[must_use]
    pub const fn new(recipe: Recipe, model: ModelVersion, recall: RecallMetadata) -> Self {
        Self {
            recipe,
            model,
            recall,
        }
    }

    pub(crate) fn is_valid_for(self, recipe: Recipe) -> bool {
        self.recipe == recipe && self.recall.is_valid()
    }
}

/// Whether a provider page is exact or explicitly approximate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchQuality {
    /// Every candidate is claimed to satisfy the exact search contract.
    Exact,
    /// Candidate recall is bounded and must be surfaced to callers.
    Approximate(ApproximationMetadata),
}

impl SearchQuality {
    /// Returns whether this result is explicitly exact.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        matches!(self, Self::Exact)
    }

    pub(crate) fn validate(self, recipe: Recipe) -> bool {
        match self {
            Self::Exact => true,
            Self::Approximate(metadata) => metadata.is_valid_for(recipe),
        }
    }
}

/// A bounded page cursor bound to all query inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    binding: Binding,
    offset: usize,
}

impl Cursor {
    /// Creates a cursor at an offset. The adapter validates the offset against
    /// the provider result before using it.
    #[must_use]
    pub const fn new(binding: Binding, offset: usize) -> Self {
        Self { binding, offset }
    }

    /// Returns the exact binding carried by this cursor.
    #[must_use]
    pub(crate) const fn binding(self) -> Binding {
        self.binding
    }

    /// Returns the next logical result offset.
    #[must_use]
    pub(crate) const fn offset(self) -> usize {
        self.offset
    }
}

/// One provider page before adapter admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidatePage {
    /// Schema tag supplied by the provider.
    pub schema: SchemaVersion,
    /// Exact requested input binding.
    pub binding: Binding,
    /// Candidate identities in provider order.
    pub ids: Vec<CandidateId>,
    /// Cursor for the next page, when one exists.
    pub next: Option<Cursor>,
    /// Coverage supplied by the authority.
    pub coverage: CoverageWitness,
    /// Exact or explicitly approximate quality claim for this page.
    pub quality: SearchQuality,
}

/// A request sent to a deterministic or remote candidate source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    /// Exact source and recipe binding.
    pub binding: Binding,
    /// Optional page cursor.
    pub cursor: Option<Cursor>,
    /// Requested result count.
    pub limit: usize,
}

impl SearchRequest {
    /// Creates a first-page request.
    #[must_use]
    pub const fn first(binding: Binding, limit: usize) -> Self {
        Self {
            binding,
            cursor: None,
            limit,
        }
    }
}

/// Provider contract. A real service integration belongs outside this crate;
/// tests and local deployments can use [`crate::MemorySource`].
pub trait CandidateSource {
    /// Provider-specific failure type.
    type Error;

    /// Returns one bounded page for an exact request.
    ///
    /// # Errors
    ///
    /// Implementations return their provider error for unavailable data or a
    /// typed adapter error for stale and malformed requests.
    fn fetch(&self, request: &SearchRequest) -> Result<CandidatePage, Self::Error>;
}

/// An admitted page/result from the vector extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResult {
    /// Exact source and recipe binding.
    pub binding: Binding,
    /// Complete coverage of the requested source scope.
    pub coverage: CoverageWitness,
    /// Deterministically ordered candidate identities.
    pub ids: Vec<CandidateId>,
    /// Cursor bound to the same query inputs.
    pub next: Option<Cursor>,
    /// Exactness/recall metadata retained from the provider page.
    pub quality: SearchQuality,
}
