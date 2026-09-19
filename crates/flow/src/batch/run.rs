//! Immutable sorted trace runs.

use super::{Microbatch, RowRef, RunRoot, consolidate_rows, run_root};
use crate::{CanonicalValue, Delta, FlowError, TraceSpine};
use std::{mem::size_of, sync::Arc};

/// One physical allocation shared by every logical segment cut from a sealed
/// row buffer. Charging lives on the allocation owner, so retaining any one
/// sibling segment retains and accounts for the complete backing allocation.
#[derive(Debug)]
pub(crate) struct RunOwner<V> {
    rows: Arc<[Delta<V>]>,
    pub(crate) retained_bytes: usize,
}

impl<V: CanonicalValue> RunOwner<V> {
    fn new(rows: Arc<[Delta<V>]>) -> Result<Arc<Self>, FlowError> {
        let retained_bytes = rows.iter().try_fold(0usize, |total, row| {
            total
                .checked_add(size_of::<Delta<V>>())
                .and_then(|total| total.checked_add(row.value.owned_bytes()))
                .ok_or(FlowError::Overflow)
        })?;
        Ok(Arc::new(Self {
            rows,
            retained_bytes,
        }))
    }
}

/// Immutable sorted trace run.
#[derive(Clone, Debug)]
pub struct Run<V> {
    owner: Arc<RunOwner<V>>,
    pub(crate) start: usize,
    pub(crate) end: usize,
    root: RunRoot,
    /// Progress inherited from its source batch.
    pub trace: TraceSpine,
}

impl<V: Clone + Ord + CanonicalValue> Run<V> {
    /// Builds a run from an already sealed batch.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when the allocation's physical byte
    /// charge cannot be represented.
    pub fn from_batch(batch: &Microbatch<V>) -> Result<Self, FlowError> {
        let owner = RunOwner::new(batch.rows.clone())?;
        let root = run_root(owner.rows.as_ref());
        Ok(Self {
            owner,
            start: 0,
            end: batch.rows.len(),
            root,
            trace: batch.trace.clone(),
        })
    }

    /// Creates a consolidated run from owned rows.
    pub(crate) fn from_rows(rows: Vec<Delta<V>>, trace: TraceSpine) -> Result<Self, FlowError> {
        let rows = consolidate_rows(rows)?;
        let root = run_root(&rows);
        let owner = RunOwner::new(Arc::from(rows.into_boxed_slice()))?;
        Ok(Self {
            end: owner.rows.len(),
            start: 0,
            owner,
            root,
            trace,
        })
    }

    /// Creates the single allocation owner reused by all segments of an
    /// incoming batch.
    pub(crate) fn owner(rows: Arc<[Delta<V>]>) -> Result<Arc<RunOwner<V>>, FlowError> {
        RunOwner::new(rows)
    }

    /// Creates a bounded segment over an existing sealed owner without
    /// cloning its payload values.
    pub(crate) fn segment(
        owner: Arc<RunOwner<V>>,
        start: usize,
        end: usize,
        trace: TraceSpine,
    ) -> Result<Self, FlowError> {
        if start >= end || end > owner.rows.len() {
            return Err(FlowError::InvalidCompactionBudget);
        }
        let root = run_root(&owner.rows[start..end]);
        Ok(Self {
            owner,
            start,
            end,
            root,
            trace,
        })
    }

    /// Returns the run's immutable content identity.
    #[must_use]
    pub const fn root(&self) -> RunRoot {
        self.root
    }

    /// Returns the number of retained rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Returns whether the run contains no retained updates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns an iterator over borrowed run rows.
    pub fn cursor(&self) -> impl Iterator<Item = RowRef<'_, V>> {
        self.slice().iter().map(|row| RowRef {
            key: row.key,
            value: &row.value,
            time: row.time,
            diff: row.diff,
        })
    }

    pub(crate) fn rows(&self) -> impl Iterator<Item = &Delta<V>> {
        self.slice().iter()
    }

    pub(crate) fn slice(&self) -> &[Delta<V>] {
        &self.owner.rows[self.start..self.end]
    }

    pub(crate) fn owner_identity(&self) -> usize {
        Arc::as_ptr(&self.owner) as usize
    }

    pub(crate) fn owner_retained_bytes(&self) -> usize {
        self.owner.retained_bytes
    }
}
