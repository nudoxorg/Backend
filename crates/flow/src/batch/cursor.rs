//! Borrowed row and scratch cursors.

use super::{LendingCursor, Microbatch};
use crate::{RowKey, Time, Weight, WorkScope};
use std::{marker::PhantomData, mem::size_of};

/// A compact local row handle branded to one batch borrow.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowHandle<'batch> {
    pub(crate) index: usize,
    pub(crate) brand: usize,
    pub(crate) _marker: PhantomData<&'batch ()>,
}

impl RowHandle<'_> {
    /// Returns the local ordinal for diagnostics, never as a persisted ID.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }
}

/// Borrowed row carrying its batch-local brand.
#[derive(Clone, Copy, Debug)]
pub struct LocalRow<'batch, V> {
    pub(crate) handle: RowHandle<'batch>,
    /// Stable row key.
    pub key: RowKey,
    /// Borrowed payload.
    pub value: &'batch V,
    /// Logical time.
    pub time: Time,
    /// Signed multiplicity.
    pub diff: Weight,
}

impl<'batch, V> LocalRow<'batch, V> {
    /// Returns the branded local handle.
    #[must_use]
    pub const fn handle(&self) -> RowHandle<'batch> {
        self.handle
    }
}

/// Borrowed row view used by ordinary iterators.
#[derive(Clone, Copy, Debug)]
pub struct RowRef<'a, V> {
    /// Stable row key.
    pub key: RowKey,
    /// Borrowed payload.
    pub value: &'a V,
    /// Logical time.
    pub time: Time,
    /// Signed multiplicity.
    pub diff: Weight,
}

/// Ordinary immutable cursor retained for compatibility with iterator-based
/// callers. `lending_cursor` supplies the stricter GAT API.
pub struct Cursor<'a, V> {
    pub(crate) batch: &'a Microbatch<V>,
    pub(crate) pos: usize,
}

impl<'a, V: Clone + Ord> Iterator for Cursor<'a, V> {
    type Item = RowRef<'a, V>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.pos;
        let row = self.batch.rows.get(index)?;
        let time = row.time;
        self.pos = self.pos.checked_add(1)?;
        Some(RowRef {
            key: row.key,
            value: &row.value,
            time,
            diff: row.diff,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.batch.len().saturating_sub(self.pos);
        (n, Some(n))
    }
}

impl<V: Clone + Ord> ExactSizeIterator for Cursor<'_, V> {}

/// GAT cursor over one immutable microbatch.
pub struct LendingBatchCursor<'a, V> {
    pub(crate) batch: &'a Microbatch<V>,
    pub(crate) pos: usize,
}

impl<V: Clone + Ord> LendingCursor for LendingBatchCursor<'_, V> {
    type Value = V;
    type Item<'a>
        = LocalRow<'a, V>
    where
        Self: 'a,
        V: 'a;

    fn next(&mut self) -> Option<Self::Item<'_>> {
        let handle = self.batch.handle_at(self.pos)?;
        self.pos = self.pos.checked_add(1)?;
        self.batch.row(handle)
    }

    fn remaining(&self) -> usize {
        self.batch.len().saturating_sub(self.pos)
    }
}

/// A lending batch cursor fenced by a checked row work scope.
pub struct BoundedLendingBatchCursor<'a, V> {
    pub(crate) inner: LendingBatchCursor<'a, V>,
    pub(crate) scope: WorkScope,
    pub(crate) exhausted: bool,
}

impl<V: Clone + Ord> BoundedLendingBatchCursor<'_, V> {
    /// Returns the scope after the rows already borrowed.
    #[must_use]
    pub const fn work_scope(&self) -> WorkScope {
        self.scope
    }

    /// Returns whether the cursor stopped at its row budget.
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        self.exhausted
    }
}

impl<V: Clone + Ord> LendingCursor for BoundedLendingBatchCursor<'_, V> {
    type Value = V;
    type Item<'a>
        = LocalRow<'a, V>
    where
        Self: 'a,
        V: 'a;

    fn next(&mut self) -> Option<Self::Item<'_>> {
        if self.inner.remaining() == 0 {
            return None;
        }
        if self
            .scope
            .charge_row_bytes(1, size_of::<LocalRow<'_, V>>())
            .is_err()
        {
            self.exhausted = true;
            return None;
        }
        self.inner.next()
    }

    fn remaining(&self) -> usize {
        self.inner
            .remaining()
            .min(usize::try_from(self.scope.remaining_rows()).unwrap_or(usize::MAX))
            .min(
                usize::try_from(
                    self.scope.remaining_bytes()
                        / u64::try_from(size_of::<LocalRow<'_, V>>()).unwrap_or(u64::MAX),
                )
                .unwrap_or(usize::MAX),
            )
    }
}
