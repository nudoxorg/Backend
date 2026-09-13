//! Version-bound ANN index lifecycle and exact local reranking.

use super::provider::{
    AnnPage, AnnSource, ScoredCandidate, VectorSearchRequest, VectorSearchResult,
};
use super::quality::{combine_quality, quality_valid};
use super::score::score;
use crate::contracts::{CandidateSource, SearchQuality};
use crate::delta::CandidateDelta;
use crate::incremental::limits::{OverlayLimits, RefreshKind};
use crate::incremental::overlay::ExactOverlay;
use crate::incremental::plan::{RefreshPlan, validate_delta};
use crate::incremental::vector::{QueryVector, VectorFacts, VectorQuery};
use crate::{AdapterError, Binding, Error, Limits, SchemaVersion};
use backend_version::CoverageWitness;
use std::collections::BTreeSet;
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
    fn fetch_admitted_candidates(
        &self,
        query: &VectorQuery,
        limit: usize,
        limits: Limits,
    ) -> Result<(Vec<crate::CandidateId>, SearchQuality), AdapterError<S::Error>> {
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
        let target_candidates = limits
            .max_candidates
            .min(limit.saturating_mul(4).max(limit));
        let query_binding =
            super::provider::VectorQueryBinding::new(self.base.binding(), query, limit)
                .map_err(AdapterError::Extension)?;
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        let mut admitted = Vec::with_capacity(target_candidates);
        let mut quality = self.base.quality();
        let mut scanned = 0_usize;
        loop {
            let page_limit = limits
                .max_page
                .min(limits.max_candidates.saturating_sub(scanned))
                .min(target_candidates.saturating_sub(admitted.len()));
            if page_limit == 0 {
                break;
            }
            let request = VectorSearchRequest {
                binding: query_binding,
                query: query.clone(),
                limit: page_limit,
                cursor,
            };
            let page = self
                .source
                .fetch(&request)
                .map_err(AdapterError::Provider)?;
            validate_page(&page, &request, &self.base, limits)?;
            scanned = scanned
                .checked_add(page.ids.len())
                .ok_or(AdapterError::Extension(Error::SizeLimit))?;
            if scanned > limits.max_candidates {
                return Err(AdapterError::Extension(Error::SizeLimit));
            }
            quality = combine_quality(
                quality,
                page.quality,
                self.base.binding().recipe,
                self.base.model(),
            )
            .map_err(AdapterError::Extension)?;
            for id in page.ids {
                if !id.is_valid() || !seen.insert(id) {
                    return Err(AdapterError::Extension(Error::MalformedInput));
                }
                if self.candidate_is_live(id)? {
                    admitted.push(id);
                }
            }
            cursor = page.next;
            if admitted.len() >= target_candidates || cursor.is_none() {
                break;
            }
        }
        Ok((admitted, quality))
    }

    fn candidate_is_live(&self, id: crate::CandidateId) -> Result<bool, AdapterError<S::Error>> {
        if self.overlay.as_deref().is_some_and(|overlay| {
            overlay.tombstones().binary_search(&id).is_ok() && overlay.point(id).is_none()
        }) {
            return Ok(false);
        }
        if self
            .overlay
            .as_deref()
            .and_then(|overlay| overlay.point(id))
            .is_some()
        {
            return Ok(true);
        }
        self.facts
            .point(id)
            .map(|point| point.is_some())
            .map_err(AdapterError::Extension)
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
        let (mut ids, quality) = self.fetch_admitted_candidates(query, limit, limits)?;
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

    /// Searches with a query-side embedding and checks its full recipe before provider I/O.
    ///
    /// # Errors
    /// Returns an extension error on recipe mismatch or any error documented by [`Self::search`].
    pub fn search_embedding(
        &self,
        query: &QueryVector,
        limit: usize,
        limits: Limits,
    ) -> Result<VectorSearchResult, AdapterError<S::Error>> {
        if query.recipe().version() != self.base.binding().recipe {
            return Err(AdapterError::Extension(Error::ApproximationMismatch));
        }
        self.search(query.query(), limit, limits)
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
            &crate::SearchRequest {
                binding: request.binding.base,
                cursor: request
                    .cursor
                    .map(|cursor| crate::Cursor::new(request.binding.base, cursor.offset())),
                limit: request.limit,
            },
        )?;
        Ok(AnnPage {
            schema: page.schema,
            binding: request.binding,
            ids: page.ids,
            next: page
                .next
                .map(|cursor| super::provider::AnnCursor::new(request.binding, cursor.offset())),
            coverage: page.coverage,
            quality: page.quality,
        })
    }
}

fn validate_page<E>(
    page: &AnnPage,
    request: &VectorSearchRequest,
    base: &super::base::AnnBase,
    limits: Limits,
) -> Result<(), AdapterError<E>> {
    if page.schema != SchemaVersion::CURRENT {
        return Err(AdapterError::Extension(Error::SchemaDrift));
    }
    if page.binding != request.binding {
        return Err(AdapterError::Extension(Error::StaleRoot));
    }
    if page.coverage != base.coverage() || !matches!(page.coverage, CoverageWitness::Complete(_)) {
        return Err(AdapterError::Extension(Error::IncompleteCoverage));
    }
    if !quality_valid(page.quality, base.binding().recipe, base.model()) {
        return Err(AdapterError::Extension(Error::ApproximationMismatch));
    }
    if page.ids.len() > request.limit || page.ids.len() > limits.max_candidates {
        return Err(AdapterError::Extension(Error::SizeLimit));
    }
    let current = request.cursor.map_or(0, super::provider::AnnCursor::offset);
    let expected = current
        .checked_add(page.ids.len())
        .ok_or(AdapterError::Extension(Error::SizeLimit))?;
    if let Some(next) = page.next
        && (next.binding() != request.binding
            || next.offset() <= current
            || next.offset() != expected
            || next.offset() > limits.max_candidates)
    {
        return Err(AdapterError::Extension(Error::InvalidCursor));
    }
    Ok(())
}
