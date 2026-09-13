//! Version-bound graph arrangement lifecycle and bounded paging.

use super::base::GraphBase;
use super::limits::{ArrangementLimits, RefreshKind};
use super::overlay::GraphOverlay;
use super::plan::{ArrangementPlan, validate_delta};
use super::query::QueryPlan;
use crate::contracts::GraphRow;
use crate::delta::{GraphDelta, GraphState};
use crate::provider::QueryResult;
use crate::{Binding, Cursor, Error, Limits};
use std::sync::Arc;

/// Version-bound graph arrangement with one immutable base and exact overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphArrangement {
    base: Arc<GraphBase>,
    overlay: Option<Arc<GraphOverlay>>,
}

impl GraphArrangement {
    /// Creates an arrangement over a complete graph state.
    ///
    /// # Errors
    ///
    /// Returns an error when the state has incomplete coverage or its row
    /// byte accounting overflows.
    pub fn from_state(state: &GraphState) -> Result<Self, Error> {
        Ok(Self {
            base: Arc::new(GraphBase::from_state(state)?),
            overlay: None,
        })
    }

    /// Returns the exact target binding.
    #[must_use]
    pub fn binding(&self) -> Binding {
        match &self.overlay {
            Some(overlay) => overlay.binding(),
            None => self.base.binding(),
        }
    }

    /// Returns the immutable base.
    #[must_use]
    pub fn base(&self) -> &GraphBase {
        &self.base
    }

    /// Returns the current exact overlay.
    #[must_use]
    pub fn overlay(&self) -> Option<&GraphOverlay> {
        self.overlay.as_deref()
    }

    /// Advances the arrangement if the transition fits its memory budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the delta does not match the arrangement's exact
    /// binding or when limits are invalid.
    pub fn advance(
        &self,
        delta: &GraphDelta,
        limits: ArrangementLimits,
    ) -> Result<RefreshOutcome, Error> {
        let limits = limits.validate()?;
        validate_delta(self.binding(), self.base.coverage(), delta)?;
        let plan = ArrangementPlan::for_delta(self.binding().root, delta, limits)?;
        if plan.kind() == RefreshKind::Reuse {
            return Ok(RefreshOutcome::Reused(self.clone()));
        }
        if plan.kind() == RefreshKind::Rebuild {
            return Ok(RefreshOutcome::RebuildRequired(plan));
        }
        let overlay = match &self.overlay {
            Some(overlay) => overlay.merge_delta(delta)?,
            None => GraphOverlay::from_delta(delta)?,
        };
        if overlay.tombstones().len() > limits.max_changed_rows
            || overlay.bytes() > limits.max_bytes
        {
            return Ok(RefreshOutcome::RebuildRequired(plan.requiring_rebuild()));
        }
        Ok(RefreshOutcome::Advanced(Self {
            base: Arc::clone(&self.base),
            overlay: Some(Arc::new(overlay)),
        }))
    }

    /// Rebuilds a base from a complete target state under a row bound.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RebuildRequired`] when the state exceeds the rebuild
    /// budget, or a validation error when it cannot form a base.
    pub fn rebuild(state: &GraphState, limits: ArrangementLimits) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if state.iter().count() > limits.max_rebuild_rows {
            return Err(Error::RebuildRequired);
        }
        Self::from_state(state)
    }

    /// Executes a version-bound query and returns canonical rows.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StaleRoot`] when the plan binding does not exactly
    /// match this arrangement.
    pub fn execute(&self, plan: &QueryPlan) -> Result<Vec<GraphRow>, Error> {
        plan.validate_against(self)?;
        // The base and replacement maps are already independently sorted.
        // Merge them directly so a query does not construct a second index or
        // sort a full materialized copy before returning its rows.
        let mut rows = Vec::new();
        self.visit_visible(|row| {
            rows.push(row.clone());
            true
        });
        Ok(rows)
    }

    /// Executes one bounded version-bound result page.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCursor`] for a cursor from another binding or
    /// [`Error::SizeLimit`] when page limits are outside the declared budget.
    pub fn page(
        &self,
        plan: &QueryPlan,
        cursor: Option<Cursor>,
        limit: usize,
        limits: Limits,
    ) -> Result<QueryResult, Error> {
        let limits = limits.validate()?;
        if limit == 0 || limit > limits.max_page {
            return Err(Error::SizeLimit);
        }
        plan.validate_against(self)?;
        plan.query().validate(limits)?;
        if let Some(cursor) = cursor
            && (cursor.binding() != self.binding()
                || cursor.query() != plan.query().version
                || cursor.offset() > limits.max_rows)
        {
            return Err(Error::InvalidCursor);
        }
        let offset = cursor.map_or(0, Cursor::offset);
        // A caller may use a smaller result envelope than the state from
        // which this arrangement was built. Clamp the final page to that
        // envelope so no advertised cursor can move beyond its bound.
        let page_limit = limit.min(limits.max_rows.saturating_sub(offset));
        let mut rows = Vec::with_capacity(page_limit);
        let mut visited = 0usize;
        let mut has_more = false;
        self.visit_visible(|row| {
            if visited < offset {
                visited += 1;
                return true;
            }
            if rows.len() < page_limit {
                rows.push(row.clone());
                visited += 1;
                true
            } else {
                has_more = true;
                false
            }
        });
        if visited < offset {
            return Err(Error::InvalidCursor);
        }
        let end = offset.checked_add(rows.len()).ok_or(Error::SizeLimit)?;
        Ok(QueryResult {
            binding: self.binding(),
            query: plan.query().version,
            coverage: self.base.coverage(),
            rows,
            next: (has_more && end < limits.max_rows)
                .then(|| Cursor::new(self.binding(), plan.query().version, end)),
        })
    }

    /// Visits the sorted visible union and stops when the visitor returns
    /// `false`. The merge keeps base rows shared and only clones rows that the
    /// caller actually requests, which is especially important for bounded
    /// pages over a large immutable graph.
    fn visit_visible(&self, mut visit: impl FnMut(&GraphRow) -> bool) -> usize {
        let overlay = self.overlay.as_deref();
        let mut base = self.base.rows().iter().peekable();
        let mut replacements = overlay
            .into_iter()
            .flat_map(|value| value.replacements().iter())
            .peekable();
        let mut visited = 0usize;

        loop {
            let next = match (base.peek(), replacements.peek()) {
                (Some(base_row), Some((replacement_key, _))) => {
                    match base_row.key.cmp(&**replacement_key) {
                        std::cmp::Ordering::Less => base.next().map(|row| (row, false)),
                        std::cmp::Ordering::Equal => {
                            let _ = base.next();
                            replacements.next().map(|(_, row)| (row.as_ref(), true))
                        }
                        std::cmp::Ordering::Greater => {
                            replacements.next().map(|(_, row)| (row.as_ref(), true))
                        }
                    }
                }
                (Some(_), None) => base.next().map(|row| (row, false)),
                (None, Some(_)) => replacements.next().map(|(_, row)| (row.as_ref(), true)),
                (None, None) => break,
            };
            let Some((row, replacement)) = next else {
                break;
            };
            if !replacement
                && overlay.is_some_and(|value| value.tombstones().binary_search(&row.key).is_ok())
            {
                continue;
            }
            visited += 1;
            if !visit(row) {
                break;
            }
        }
        visited
    }
}

/// Result of one exact arrangement advancement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefreshOutcome {
    /// The previous arrangement already represented the target.
    Reused(GraphArrangement),
    /// The exact overlay advanced.
    Advanced(GraphArrangement),
    /// A complete rebuild is required before the target can be advertised.
    RebuildRequired(ArrangementPlan),
}
