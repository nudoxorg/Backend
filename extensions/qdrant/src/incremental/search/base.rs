//! Immutable ANN base metadata and root-bound candidate identity.

use super::quality::quality_valid;
use crate::contracts::SearchQuality;
use crate::incremental::vector::{Metric, VectorFacts};
use crate::{Binding, CandidateId, Error, Limits, ModelVersion};
use backend_version::CoverageWitness;
use std::sync::Arc;

/// Immutable ANN base metadata tied to the exact indexed facts root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnBase {
    binding: Binding,
    coverage: CoverageWitness,
    model: ModelVersion,
    metric: Metric,
    dimensions: usize,
    quality: SearchQuality,
    ids: Arc<[CandidateId]>,
}

impl AnnBase {
    /// Creates a base candidate set from a complete exact facts relation.
    ///
    /// The actual graph/quantizer may live in a provider; this descriptor is
    /// the admission proof that ties it to the canonical root and recipe.
    ///
    /// # Errors
    ///
    /// Returns an error when limits reject the candidate set or quality does
    /// not match the bound recipe and model.
    pub fn from_facts(
        facts: &VectorFacts,
        quality: SearchQuality,
        limits: Limits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if !matches!(
            facts.coverage(),
            CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
        ) {
            return Err(Error::IncompleteCoverage);
        }
        if !quality_valid(quality, facts.binding().recipe, facts.model()) {
            return Err(Error::ApproximationMismatch);
        }
        let ids = facts.ids().collect::<Vec<_>>();
        if ids.len() > limits.max_candidates {
            return Err(Error::SizeLimit);
        }
        Ok(Self {
            binding: facts.binding(),
            coverage: facts.coverage(),
            model: facts.model(),
            metric: facts.metric(),
            dimensions: facts.dimensions(),
            quality,
            ids: Arc::from(ids),
        })
    }

    /// Returns the exact indexed root binding.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the complete base coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Returns the model revision.
    #[must_use]
    pub const fn model(&self) -> ModelVersion {
        self.model
    }

    /// Returns the metric.
    #[must_use]
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Returns vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Returns explicit exact/approximate quality.
    #[must_use]
    pub const fn quality(&self) -> SearchQuality {
        self.quality
    }

    /// Returns immutable indexed candidate identities.
    #[must_use]
    pub fn ids(&self) -> &[CandidateId] {
        &self.ids
    }
}
