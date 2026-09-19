//! Immutable columnar microbatches and compact time lanes.

use super::{
    BoundedLendingBatchCursor, Cursor, LendingBatchCursor, LocalRow, RowHandle, consolidate_rows,
};
use crate::{Delta, Epoch, FlowError, Frontier, Time, TraceSpine, WorkScope};
use std::{marker::PhantomData, sync::Arc};

/// Immutable consolidated columnar execution microbatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Microbatch<V> {
    /// One immutable owner for the sealed rows. Runs retain this allocation
    /// directly, avoiding a second payload clone at arrangement admission.
    pub(crate) rows: Arc<[Delta<V>]>,
    pub(crate) trace: TraceSpine,
    pub(crate) input_len: usize,
}

impl<V: Clone + Ord> Microbatch<V> {
    /// Sorts, checks, consolidates, and freezes weighted rows into columns.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] for an invalid row,
    /// [`FlowError::Overflow`] when time advancement or consolidation
    /// overflows, or another checked admission error.
    pub fn seal(deltas: &[Delta<V>]) -> Result<Self, FlowError> {
        Self::seal_owned(deltas.to_vec())
    }

    /// Seals an owned row buffer without copying it before consolidation.
    ///
    /// This is useful at owner boundaries where a parser or operator already
    /// owns its output vector. The borrowed [`Microbatch::seal`] entry point
    /// remains available for callers that need to retain their input rows.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] for an invalid row,
    /// [`FlowError::Overflow`] when time advancement or consolidation
    /// overflows, or another checked admission error.
    pub fn seal_owned(deltas: Vec<Delta<V>>) -> Result<Self, FlowError> {
        let input_len = deltas.len();
        let input_max_time = deltas.iter().map(|row| row.time).reduce(Time::join);
        let rows = consolidate_rows(deltas)?;
        let owned_rows: Arc<[Delta<V>]> = Arc::from(rows.into_boxed_slice());
        let upper = if let Some(max) = input_max_time {
            max.successor()?
        } else {
            Time::new(Epoch(0), 0)
        };
        let mut trace = TraceSpine::new();
        trace.advance_upper(Frontier::new(upper))?;
        Ok(Self {
            rows: owned_rows,
            trace,
            input_len,
        })
    }

    /// Number of consolidated rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Number of rows supplied before consolidation.
    #[must_use]
    pub const fn input_len(&self) -> usize {
        self.input_len
    }

    /// Returns true when all supplied rows canceled or the input was empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns true when the compact time header is shared by all rows.
    #[must_use]
    pub fn has_shared_time(&self) -> bool {
        self.rows
            .first()
            .is_some_and(|first| self.rows.iter().all(|row| row.time == first.time))
    }

    /// Returns a row's logical time.
    #[must_use]
    pub fn time_at(&self, index: usize) -> Option<Time> {
        self.rows.get(index).map(|row| row.time)
    }

    /// Returns an iterator over borrowed column rows.
    #[must_use]
    pub fn cursor(&self) -> Cursor<'_, V> {
        Cursor {
            batch: self,
            pos: 0,
        }
    }

    /// Returns a GAT lending cursor with branded local handles.
    #[must_use]
    pub fn lending_cursor(&self) -> LendingBatchCursor<'_, V> {
        LendingBatchCursor {
            batch: self,
            pos: 0,
        }
    }

    /// Returns a lending cursor that stops before exceeding `scope` rows.
    #[must_use]
    pub fn lending_cursor_bounded(&self, scope: WorkScope) -> BoundedLendingBatchCursor<'_, V> {
        BoundedLendingBatchCursor {
            inner: LendingBatchCursor {
                batch: self,
                pos: 0,
            },
            scope,
            exhausted: false,
        }
    }

    /// Returns a branded local row handle if the ordinal is initialized.
    #[must_use]
    pub fn handle_at(&self, index: usize) -> Option<RowHandle<'_>> {
        (index < self.len()).then_some(RowHandle {
            index,
            brand: std::ptr::from_ref(self) as usize,
            _marker: PhantomData,
        })
    }

    /// Resolves a branded row handle against this exact immutable batch.
    #[must_use]
    pub fn row<'a>(&'a self, handle: RowHandle<'a>) -> Option<LocalRow<'a, V>> {
        if handle.brand != std::ptr::from_ref(self) as usize {
            return None;
        }
        let row = self.rows.get(handle.index)?;
        let time = self.rows.get(handle.index)?.time;
        Some(LocalRow {
            handle,
            key: row.key,
            value: &row.value,
            time,
            diff: row.diff,
        })
    }
}
