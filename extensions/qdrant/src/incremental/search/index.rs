//! Version-bound ANN index lifecycle and exact local reranking.

use super::provider::{
    AnnPage, AnnSource, ScoredCandidate, VectorSearchRequest, VectorSearchResult,
};
use super::quality::{combine_quality, quality_valid};
use super::score::score;
use crate::contracts::CandidateSource;
use crate::delta::CandidateDelta;
use crate::incremental::limits::{OverlayLimits, RefreshKind};
use crate::incremental::overlay::ExactOverlay;
use crate::incremental::plan::{RefreshPlan, validate_delta};
use crate::incremental::vector::{VectorFacts, VectorQuery};
use crate::{AdapterError, Binding, Error, Limits, SchemaVersion};
use std::sync::Arc;

/// A version-bound vector index with one ANN base and one exact overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VectorIndex<S> {
    base: super::base::AnnBase,
    facts: Arc<VectorFacts>,
    source: S,
    overlay: Option<Arc<ExactOverlay>>,
}

impl<S> VectorIndex<S> {
    /// Opens an index only when base and exact facts share all semantic inputs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StaleRoot`] when roots, coverage, model, metric, or
    /// dimensions disagree.
    pub fn new(base: super::base::AnnBase, facts: VectorFacts, source: S) -> Result<Self, Error> {
        if base.binding() != facts.binding()
            || base.coverage() != facts.coverage()
            || base.model() != facts.model()
            || base.metric() != facts.metric()
            || base.dimensions() != facts.dimensions()
        {
            return Err(Error::StaleRoot);
        }
        Ok(Self {
            base,
            facts: Arc::new(facts),
            source,
            overlay: None,
        })
    }

    /// Returns the current exact target binding.
    #[must_use]
    pub fn binding(&self) -> Binding {
        self.overlay
            .as_deref()
            .map_or_else(|| self.facts.binding(), ExactOverlay::binding)
    }

    /// Returns immutable ANN base metadata.
    #[must_use]
    pub const fn base(&self) -> &super::base::AnnBase {
        &self.base
    }

    /// Returns exact facts for current reranking.
    #[must_use]
    pub fn facts(&self) -> &VectorFacts {
        &self.facts
    }

    /// Returns the exact current overlay.
    #[must_use]
    pub fn overlay(&self) -> Option<&ExactOverlay> {
        self.overlay.as_deref()
    }

    /// Advances fresh exact points when the overlay remains bounded.
    ///
    /// # Errors
    ///
    /// Returns an error when the delta or target facts do not match the exact
    /// current binding, or when limits are invalid.
    pub fn advance(
        &self,
        delta: &CandidateDelta,
        facts: VectorFacts,
        limits: OverlayLimits,
    ) -> Result<RefreshOutcome<S>, Error>
    where
        S: Clone,
    {
        let limits = limits.validate()?;
        validate_delta(self.binding(), self.facts.coverage(), delta)?;
        if facts.binding().root != delta.delta.target()
            || facts.binding().workspace != self.binding().workspace
            || facts.binding().recipe != self.binding().recipe
            || facts.binding().authority != self.binding().authority
            || facts.binding().read_manifest != self.binding().read_manifest
            || facts.binding().frontier != self.binding().frontier
            || facts.coverage() != self.facts.coverage()
            || facts.model() != self.base.model()
            || facts.metric() != self.base.metric()
            || facts.dimensions() != self.base.dimensions()
        {
            return Err(Error::StaleRoot);
        }
        let plan = RefreshPlan::for_delta(self.binding().root, delta, limits)?;
        if plan.kind() == RefreshKind::Reuse {
            return Ok(RefreshOutcome::Reused(self.clone()));
        }
        if plan.kind() == RefreshKind::Rebuild {
            return Ok(RefreshOutcome::RebuildRequired(plan));
        }
        let overlay = match &self.overlay {
            Some(overlay) => overlay.merge_delta(delta)?,
            None => ExactOverlay::from_delta(
                delta,
                self.base.model(),
                self.base.metric(),
                self.base.dimensions(),
            )?,
        };
        if overlay.tombstones().len() > limits.max_changed_points
            || overlay.bytes() > limits.max_bytes
        {
            return Ok(RefreshOutcome::RebuildRequired(plan.requiring_rebuild()));
        }
        Ok(RefreshOutcome::Advanced(Self {
            base: self.base.clone(),
            facts: Arc::new(facts),
            source: self.source.clone(),
            overlay: Some(Arc::new(overlay)),
        }))
    }

    /// Rebuilds the provider descriptor from complete exact facts at the same
    /// workspace/recipe/authority/read-manifest fence. A recipe or authority
    /// change requires opening a new index with its provider capability.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RebuildRequired`] when facts exceed the rebuild bound,
    /// or another validation error when the new base is not admissible.
    pub fn rebuild(&self, facts: VectorFacts, limits: OverlayLimits) -> Result<Self, Error>
    where
        S: Clone,
    {
        let limits = limits.validate()?;
        if facts.binding().workspace != self.binding().workspace
            || facts.binding().recipe != self.binding().recipe
            || facts.binding().authority != self.binding().authority
            || facts.binding().read_manifest != self.binding().read_manifest
            || facts.binding().frontier != self.binding().frontier
            || facts.coverage() != self.facts.coverage()
            || facts.model() != self.base.model()
            || facts.metric() != self.base.metric()
            || facts.dimensions() != self.base.dimensions()
        {
            return Err(Error::StaleRoot);
        }
        if facts.ids().count() > limits.max_rebuild_points {
            return Err(Error::RebuildRequired);
        }
        // Rebuilding refreshes the layout at the same provider boundary. It
        // cannot manufacture an exactness claim when the existing provider
        // capability is approximate.
        // The rebuild budget is the admission bound for this operation. Use
        // it when rebuilding the candidate descriptor so a valid larger
        // relation is not accidentally rejected by the provider-page default
        // of 4,096 candidates.
        let base_limits = Limits {
            max_candidates: limits.max_rebuild_points.max(Limits::default().max_page),
            ..Limits::default()
        };
        let base = super::base::AnnBase::from_facts(&facts, self.base.quality(), base_limits)?;
        Ok(Self {
            base,
            facts: Arc::new(facts),
            source: self.source.clone(),
            overlay: None,
        })
    }
}

impl<S: AnnSource> VectorIndex<S> {
    fn fetch_admitted_page(
        &self,
        query: &VectorQuery,
        limit: usize,
        limits: Limits,
    ) -> Result<AnnPage, AdapterError<S::Error>> {
        let limits = limits.validate().map_err(AdapterError::Extension)?;
        if limit == 0 || limit > limits.max_page {
            return Err(AdapterError::Extension(Error::SizeLimit));
        }
        if query.model() != self.base.model()
            || query.metric() != self.base.metric()
            || query.values().len() != self.base.dimensions()
        {
            return Err(AdapterError::Extension(Error::DimensionMismatch));
        }
        let provider_limit = limits
            .max_candidates
            .min(limit.saturating_mul(4).max(limit));
        let page = self
            .source
            .fetch(&VectorSearchRequest {
                binding: self.base.binding(),
                query: query.clone(),
                limit: provider_limit,
            })
            .map_err(AdapterError::Provider)?;
        if page.schema != SchemaVersion::CURRENT
            || page.binding != self.base.binding()
            || page.coverage != self.base.coverage()
            || !matches!(page.coverage, backend_version::CoverageWitness::Complete(_))
            || !quality_valid(page.quality, self.base.binding().recipe, self.base.model())
        {
            return Err(AdapterError::Extension(
                if page.schema != SchemaVersion::CURRENT {
                    Error::SchemaDrift
                } else if page.coverage != self.base.coverage()
                    || !matches!(page.coverage, backend_version::CoverageWitness::Complete(_))
                {
                    Error::IncompleteCoverage
                } else if page.binding != self.base.binding() {
                    Error::StaleRoot
                } else {
                    Error::ApproximationMismatch
                },
            ));
        }
        if page.ids.len() > provider_limit || page.ids.len() > limits.max_candidates {
            return Err(AdapterError::Extension(Error::SizeLimit));
        }
        let expected_next = page.ids.len();
        if let Some(next) = page.next
            && (next.binding() != self.base.binding()
                || next.offset() == 0
                || next.offset() != expected_next
                || next.offset() > limits.max_candidates)
        {
            return Err(AdapterError::Extension(Error::InvalidCursor));
        }
        Ok(page)
    }

    /// Searches immutable base, unions fresh exact points, filters tombstones,
    /// and reranks surviving coordinates exactly.
    ///
    /// # Errors
    ///
    /// Returns a provider error or a fail-closed extension error when the page
    /// binding, schema, coverage, quality, or request limits do not match.
    pub fn search(
        &self,
        query: &VectorQuery,
        limit: usize,
        limits: Limits,
    ) -> Result<VectorSearchResult, AdapterError<S::Error>> {
        let page = self.fetch_admitted_page(query, limit, limits)?;
        let quality = combine_quality(
            self.base.quality(),
            page.quality,
            self.base.binding().recipe,
            self.base.model(),
        )
        .map_err(AdapterError::Extension)?;
        let mut ids = page.ids;
        if ids.iter().any(|id| !id.is_valid()) {
            return Err(AdapterError::Extension(Error::MalformedInput));
        }
        ids.sort_unstable();
        if ids.windows(2).any(|window| window[0] == window[1]) {
            return Err(AdapterError::Extension(Error::MalformedInput));
        }
        if let Some(overlay) = &self.overlay {
            ids.extend(overlay.points().keys().copied());
        }
        ids.sort_unstable();
        ids.dedup();
        let mut scored = Vec::with_capacity(ids.len());
        for id in ids {
            if self.overlay.as_deref().is_some_and(|overlay| {
                overlay.tombstones().binary_search(&id).is_ok() && overlay.point(id).is_none()
            }) {
                continue;
            }
            let point = self
                .overlay
                .as_deref()
                .and_then(|overlay| overlay.point(id))
                .map_or_else(|| self.facts.point(id), |point| Ok(Some(point.clone())))
                .map_err(AdapterError::Extension)?;
            let Some(point) = point else { continue };
            let score = score(self.base.metric(), query.values(), point.values());
            if !score.is_finite() {
                return Err(AdapterError::Extension(Error::MalformedInput));
            }
            scored.push(ScoredCandidate { id, score });
        }
        scored.sort_by(|left, right| {
            left.score
                .total_cmp(&right.score)
                .then_with(|| left.id.cmp(&right.id))
        });
        scored.truncate(limit);
        Ok(VectorSearchResult {
            binding: self.binding(),
            coverage: self.base.coverage(),
            candidates: scored,
            quality,
        })
    }
}

/// Typed refresh result for an existing provider index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefreshOutcome<S> {
    /// The existing index already represents the target root.
    Reused(VectorIndex<S>),
    /// The exact overlay advanced.
    Advanced(VectorIndex<S>),
    /// The owner must schedule a complete ANN rebuild.
    RebuildRequired(RefreshPlan),
}

impl AnnSource for crate::MemorySource {
    type Error = Error;

    fn fetch(&self, request: &VectorSearchRequest) -> Result<AnnPage, Self::Error> {
        let page = CandidateSource::fetch(
            self,
            &crate::SearchRequest::first(request.binding, request.limit),
        )?;
        Ok(AnnPage {
            schema: page.schema,
            binding: page.binding,
            ids: page.ids,
            next: page.next,
            coverage: page.coverage,
            quality: page.quality,
        })
    }
}
