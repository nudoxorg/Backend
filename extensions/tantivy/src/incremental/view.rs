//! Exact lexical view advancement and query execution.

use super::base::LexicalBase;
use super::overlay::LexicalOverlay;
use super::plan::{OverlayLimits, RefreshKind, RefreshPlan, validate_delta_binding};
use crate::delta::{DocumentDelta, DocumentState};
use crate::{Binding, Cursor, Error, Query};
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
    pub fn search(&self, query: &Query) -> Result<Vec<u64>, Error> {
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
            return Ok(ids);
        }
        let mut candidates = self.posting(query.terms[0].as_str());
        for term in query.terms.iter().skip(1) {
            let posting = self.posting(term);
            candidates.retain(|id| posting.binary_search(id).is_ok());
        }
        Ok(candidates)
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
            && (cursor.binding() != self.binding()
                || cursor.query() != query.version
                || cursor.offset() > limits.max_documents)
        {
            return Err(Error::InvalidCursor);
        }
        let ids = self.search(query)?;
        let offset = cursor.map_or(0, Cursor::offset);
        if offset > ids.len() {
            return Err(Error::InvalidCursor);
        }
        let end = offset
            .checked_add(limit.min(limits.max_documents.saturating_sub(offset)))
            .ok_or(Error::SizeLimit)?
            .min(ids.len());
        Ok(crate::QueryResult {
            binding: self.binding(),
            query: query.version,
            coverage: self.base.coverage(),
            ids: ids[offset..end].to_vec(),
            next: (end < ids.len() && end < limits.max_documents)
                .then(|| Cursor::new(self.binding(), query.version, end)),
        })
    }

    fn posting(&self, term: &str) -> Vec<u64> {
        let overlay = self.overlay.as_deref();
        let base = self.base.posting(term).unwrap_or_default();
        let additions = overlay
            .and_then(|value| value.additions(term))
            .unwrap_or_default();
        let mut ids = Vec::with_capacity(base.len().saturating_add(additions.len()));
        for id in base {
            if overlay.is_none_or(|value| value.tombstones().binary_search(id).is_err()) {
                ids.push(*id);
            }
        }
        ids.extend_from_slice(additions);
        ids.sort_unstable();
        ids.dedup();
        ids
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
