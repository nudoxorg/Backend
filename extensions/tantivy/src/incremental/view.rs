//! Exact lexical view advancement and query execution.

use super::base::LexicalBase;
use super::overlay::LexicalOverlay;
use super::plan::{OverlayLimits, RefreshKind, RefreshPlan, validate_delta_binding};
use crate::delta::{DocumentDelta, DocumentState};
use crate::{Binding, Cursor, Error, Query, RankedHit, Relevance};
use backend_semantic::EntityId;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A version-bound lexical view with immutable base and bounded exact overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexicalView {
    base: Arc<LexicalBase>,
    overlay: Option<Arc<LexicalOverlay>>,
}

impl LexicalView {
    /// Creates a view whose base exactly represents `state`.
    ///
    /// # Errors
    ///
    /// Returns an error when the state has incomplete coverage or its posting
    /// byte estimate overflows.
    pub fn from_state(state: &DocumentState) -> Result<Self, Error> {
        Ok(Self {
            base: Arc::new(LexicalBase::from_state(state)?),
            overlay: None,
        })
    }

    /// Returns the target binding, including the currently completed root.
    #[must_use]
    pub fn binding(&self) -> Binding {
        match &self.overlay {
            Some(overlay) => overlay.binding(),
            None => self.base.binding(),
        }
    }

    /// Returns the immutable base materialization.
    #[must_use]
    pub fn base(&self) -> &LexicalBase {
        &self.base
    }

    /// Returns the current exact overlay, if one is retained.
    #[must_use]
    pub fn overlay(&self) -> Option<&LexicalOverlay> {
        self.overlay.as_deref()
    }

    /// Applies an exact transition when it fits the maintenance budget.
    ///
    /// A large transition yields [`RefreshOutcome::RebuildRequired`] with a
    /// bounded plan.  It is never silently truncated and never advertised as
    /// a complete lexical view.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition does not match this view's exact
    /// binding or when its maintenance limits are invalid.
    pub fn advance(
        &self,
        delta: &DocumentDelta,
        limits: OverlayLimits,
    ) -> Result<RefreshOutcome, Error> {
        let limits = limits.validate()?;
        validate_delta_binding(self.binding(), self.base.coverage(), delta)?;
        let plan = RefreshPlan::for_delta(self.binding().root, delta, limits)?;
        if plan.kind() == RefreshKind::Reuse {
            return Ok(RefreshOutcome::Reused(self.clone()));
        }
        if plan.kind() == RefreshKind::Rebuild {
            return Ok(RefreshOutcome::RebuildRequired(plan));
        }
        let overlay = match &self.overlay {
            Some(overlay) => overlay.merge_delta(delta)?,
            None => LexicalOverlay::from_delta(delta)?,
        };
        if overlay.tombstones().len() > limits.max_changed_documents
            || overlay.term_count() > limits.max_terms
            || overlay.bytes() > limits.max_bytes
        {
            return Ok(RefreshOutcome::RebuildRequired(plan.requiring_rebuild()));
        }
        Ok(RefreshOutcome::Advanced(Self {
            base: Arc::clone(&self.base),
            overlay: Some(Arc::new(overlay)),
        }))
    }

    /// Rebuilds an immutable base under an explicit document budget.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RebuildRequired`] when the state exceeds the rebuild
    /// budget, or a validation error when the state cannot form a base.
    pub fn rebuild(state: &DocumentState, limits: OverlayLimits) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if state.iter().count() > limits.max_rebuild_documents {
            return Err(Error::RebuildRequired);
        }
        Self::from_state(state)
    }

    /// Executes an exact canonical term query over base plus overlay.
    ///
    /// # Errors
    ///
    /// Returns a query validation error when the query exceeds the declared
    /// query limits.
    pub fn search(&self, query: &Query) -> Result<Vec<RankedHit>, Error> {
        query.validate(crate::Limits::default())?;
        if query.terms.is_empty() {
            let overlay = self.overlay.as_deref();
            let mut ids = self
                .base
                .documents()
                .iter()
                .copied()
                .filter(|id| {
                    overlay.is_none_or(|value| value.tombstones().binary_search(id).is_err())
                })
                .collect::<Vec<_>>();
            if let Some(overlay) = overlay {
                ids.extend(overlay.document_ids());
                ids.sort_unstable();
                ids.dedup();
            }
            return Ok(ids
                .into_iter()
                .map(|document| RankedHit {
                    document,
                    relevance: Relevance::all_documents(),
                })
                .collect());
        }
        let mut candidates = self.posting(query.terms[0].as_str(), query)?;
        for term in query.terms.iter().skip(1) {
            let posting = self.posting(term, query)?;
            let mut intersection = BTreeMap::new();
            for (document, relevance) in candidates {
                if let Some(next) = posting.get(&document) {
                    intersection.insert(document, relevance.combine(*next)?);
                }
            }
            candidates = intersection;
        }
        let mut hits = candidates
            .into_iter()
            .map(|(document, relevance)| RankedHit {
                document,
                relevance,
            })
            .collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| {
            right
                .relevance
                .cmp(&left.relevance)
                .then_with(|| left.document.cmp(&right.document))
        });
        Ok(hits)
    }

    /// Executes one bounded page without rebuilding a provider adapter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCursor`] for a cursor from another binding or
    /// [`Error::SizeLimit`] when page limits are outside the declared budget.
    pub fn page(
        &self,
        query: &Query,
        cursor: Option<Cursor>,
        limit: usize,
        limits: crate::Limits,
    ) -> Result<crate::QueryResult, Error> {
        let limits = limits.validate()?;
        if limit == 0 || limit > limits.max_page {
            return Err(Error::SizeLimit);
        }
        query.validate(limits)?;
        if let Some(cursor) = cursor
            && (cursor.binding() != self.binding() || cursor.query() != query.version)
        {
            return Err(Error::InvalidCursor);
        }
        let hits = self.search(query)?;
        let offset = cursor.map_or(0, Cursor::offset);
        if offset > hits.len() {
            return Err(Error::InvalidCursor);
        }
        let end = offset
            .checked_add(limit)
            .ok_or(Error::SizeLimit)?
            .min(hits.len());
        Ok(crate::QueryResult {
            binding: self.binding(),
            query: query.version,
            coverage: self.base.coverage(),
            hits: hits[offset..end].to_vec(),
            next: (end < hits.len()).then(|| Cursor::new(self.binding(), query.version, end)),
        })
    }

    fn posting(&self, term: &str, query: &Query) -> Result<BTreeMap<EntityId, Relevance>, Error> {
        let overlay = self.overlay.as_deref();
        let mut ranked: BTreeMap<EntityId, Relevance> = BTreeMap::new();
        for (key, ids) in self.base.postings() {
            if !key.selected_by(term, query.match_mode, &query.fields, query.case) {
                continue;
            }
            let relevance = Relevance::new(term.len(), key.term.len(), key.field_weight(), 1)?;
            for id in ids {
                if overlay.is_none_or(|value| value.tombstones().binary_search(id).is_err()) {
                    ranked
                        .entry(*id)
                        .and_modify(|current| *current = (*current).max(relevance))
                        .or_insert(relevance);
                }
            }
        }
        if let Some(overlay) = overlay {
            for (key, ids) in overlay.postings() {
                if !key.selected_by(term, query.match_mode, &query.fields, query.case) {
                    continue;
                }
                let relevance = Relevance::new(term.len(), key.term.len(), key.field_weight(), 1)?;
                for id in ids {
                    ranked
                        .entry(*id)
                        .and_modify(|current| *current = (*current).max(relevance))
                        .or_insert(relevance);
                }
            }
        }
        Ok(ranked)
    }
}

/// Result of attempting to advance an exact lexical view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefreshOutcome {
    /// No effective relation rows changed.
    Reused(LexicalView),
    /// The exact overlay advanced within its declared bound.
    Advanced(LexicalView),
    /// The owner must schedule a complete bounded rebuild.
    RebuildRequired(RefreshPlan),
}
