//! Arrangement reads, range cursors, and visible state projections.

use super::{Arrangement, WorkCounters, WorkScope};
use crate::batch::{lower_bound, row_cmp, upper_bound};
use crate::{
    ArrangementRelation, ArrangementRoot, CanonicalValue, Delta, FlowError, Frontier,
    RelationState, RowKey, RowRef, Time, TraceSpine, Weight,
};
use backend_version::TreeNodeHandle;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    mem::size_of,
    ops::RangeInclusive,
};

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Arrangement<V> {
    fn delta_work_bytes(value: &V) -> Result<usize, FlowError> {
        size_of::<Delta<V>>()
            .checked_add(value.owned_bytes())
            .ok_or(FlowError::Overflow)
    }

    /// Number of retained runs across all levels.
    #[must_use]
    pub fn run_count(&self) -> usize {
        self.levels.iter().map(Vec::len).sum()
    }

    /// Returns the current retained immutable run count.
    #[must_use]
    pub fn retained_run_debt(&self) -> usize {
        self.run_count()
    }

    /// Returns the actual heap payload envelope retained by physical runs.
    ///
    /// The calculation includes each row's value storage, so variable-sized
    /// payloads cannot hide behind the fixed `Delta` layout.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        let mut owners = BTreeSet::new();
        let runs = self
            .levels
            .iter()
            .flat_map(|level| level.iter())
            .filter(|run| owners.insert(run.owner_identity()))
            .fold(0usize, |total, run| {
                total.saturating_add(run.owner_retained_bytes())
            });
        runs.saturating_add(
            self.frontier_merge
                .as_ref()
                .map_or(0, |cursor| cursor.bytes),
        )
    }

    /// Returns the number of stalled identity promotions awaiting a merge.
    #[must_use]
    pub const fn promotion_debt(&self) -> usize {
        self.promotion_debt
    }

    /// Number of allocated levels.
    #[must_use]
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// Number of runs at one level.
    #[must_use]
    pub fn runs_at_level(&self, level: usize) -> usize {
        self.levels.get(level).map_or(0, Vec::len)
    }

    /// Returns measured work counters.
    #[must_use]
    pub const fn work_counters(&self) -> WorkCounters {
        self.work
    }

    /// Returns the schema-marked logical state root.
    #[must_use]
    pub const fn state_root(&self) -> ArrangementRoot<V> {
        self.state.root()
    }

    /// Returns the schema-marked logical root.
    #[must_use]
    pub const fn root(&self) -> ArrangementRoot<V> {
        self.state.root()
    }

    /// Borrows the sole typed logical state behind this physical arrangement.
    ///
    /// Physical runs, compaction levels, and checkpoint history are adapters
    /// for this state root; callers that need authenticated node cursors can
    /// use the returned version-kernel view without exporting a row map.
    #[must_use]
    pub const fn state(&self) -> &RelationState<ArrangementRelation<V>> {
        &self.state
    }

    /// Returns an owning handle to the authenticated logical root node.
    ///
    /// The handle is O(1) to create and keeps the immutable root generation
    /// alive without exporting visible rows. Storage adapters can use it to
    /// publish or retain a root while preserving path-copy sharing.
    #[must_use]
    pub fn root_handle(&self) -> TreeNodeHandle<ArrangementRelation<V>> {
        self.state.root_handle()
    }

    /// Returns the current upper frontier.
    #[must_use]
    pub fn upper(&self) -> Frontier {
        self.trace.upper()
    }

    /// Returns the current since frontier.
    #[must_use]
    pub fn since(&self) -> Time {
        self.trace.since()
    }

    /// Returns the complete retained since frontier.
    #[must_use]
    pub fn since_frontier(&self) -> Frontier {
        self.trace.since_frontier()
    }

    /// Returns the number of rows retained for delta subscriptions.
    #[must_use]
    pub const fn history_rows(&self) -> usize {
        self.history_rows
    }

    /// Returns canonical encoded bytes retained for delta subscriptions.
    #[must_use]
    pub const fn history_bytes(&self) -> usize {
        self.history_bytes
    }

    /// Returns the number of nonzero entries in the logical state.
    ///
    /// The value is maintained from checked transition metadata and does not
    /// scan the persistent relation tree.
    #[must_use]
    pub const fn visible_len(&self) -> usize {
        self.visible_len
    }

    /// Returns a shared trace view.
    #[must_use]
    pub fn trace(&self) -> TraceSpine {
        self.trace.clone()
    }

    /// Returns all retained update rows in deterministic order.
    pub fn rows(&self) -> impl Iterator<Item = RowRef<'_, V>> {
        self.levels
            .iter()
            .flat_map(|level| level.iter())
            .flat_map(|run| run.cursor())
    }

    /// Borrows retained rows whose row key falls in `range`.
    ///
    /// This cursor intentionally does not merge runs or allocate a result
    /// vector. Callers that need a globally ordered snapshot can continue to
    /// use [`Arrangement::range_seek`], while streaming consumers avoid
    /// materializing a potentially large fan-out.
    pub fn iter_range(&self, range: RangeInclusive<RowKey>) -> impl Iterator<Item = RowRef<'_, V>> {
        let (start, end) = range.into_inner();
        let valid = start <= end;
        self.levels.iter().flat_map(move |level| {
            level.iter().flat_map(move |run| {
                let rows = run.slice();
                let (begin, finish) = if valid {
                    (lower_bound(rows, &start), upper_bound(rows, &end))
                } else {
                    (0, 0)
                };
                rows[begin..finish].iter().map(|row| RowRef {
                    key: row.key,
                    value: &row.value,
                    time: row.time,
                    diff: row.diff,
                })
            })
        })
    }

    /// Returns the consolidated visible support map.
    #[must_use]
    pub fn visible(&self) -> BTreeMap<(RowKey, V), i64> {
        self.state
            .iter()
            .map(|(key, support)| ((key.row(), key.value().clone()), *support))
            .collect()
    }

    /// Borrows the current visible support entries without cloning payloads.
    pub fn visible_iter(&self) -> impl Iterator<Item = (RowKey, &V, i64)> {
        self.state
            .iter()
            .map(|(key, support)| (key.row(), key.value(), *support))
    }

    /// Borrows current visible rows without allocating a complete snapshot.
    pub fn visible_rows_iter(&self) -> impl Iterator<Item = RowRef<'_, V>> {
        let time = self.trace.upper().upper();
        self.state.iter().map(move |(key, support)| RowRef {
            key: key.row(),
            value: key.value(),
            time,
            diff: Weight::from_visible_support(*support),
        })
    }

    /// Returns the current visible rows, omitting zero support.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<Delta<V>> {
        let time = self.trace.upper().upper();
        self.state
            .iter()
            .map(|(key, diff)| Delta {
                key: key.row(),
                value: key.value().clone(),
                time,
                diff: Weight::from_visible_support(*diff),
            })
            .collect()
    }

    /// Reconstructs a snapshot with checked support arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ObservationOutsideRetention`] when the requested
    /// time predates retained history, [`FlowError::ObservationBeyondUpper`]
    /// when it exceeds upper, or [`FlowError::Overflow`] on support overflow.
    pub fn snapshot_at_checked(
        &self,
        as_of: Time,
    ) -> Result<BTreeMap<(RowKey, V), i64>, FlowError> {
        let mut scope = WorkScope::new(usize::MAX, usize::MAX);
        self.snapshot_at_checked_budgeted(as_of, &mut scope)
    }

    /// Reconstructs a snapshot while charging rows and bytes to `scope`.
    ///
    /// The returned map is committed only after the complete scan fits the
    /// caller's envelope; a budget failure leaves the arrangement unchanged.
    ///
    /// # Errors
    ///
    /// Returns the same observation errors as [`Self::snapshot_at_checked`]
    /// and [`FlowError::RecursionWorkLimit`] when the scan exceeds `scope`.
    pub fn snapshot_at_checked_budgeted(
        &self,
        as_of: Time,
        scope: &mut WorkScope,
    ) -> Result<BTreeMap<(RowKey, V), i64>, FlowError> {
        if !self.trace.since_frontier().at_or_after(as_of) {
            return Err(FlowError::ObservationOutsideRetention);
        }
        if !self.trace.upper().allows_time(as_of) {
            return Err(FlowError::ObservationBeyondUpper);
        }
        let upper = self.trace.upper();
        if upper.elements().len() == 1 && as_of == upper.upper() {
            for (key, _) in self.state.iter() {
                scope.charge_row_bytes(1, Self::delta_work_bytes(key.value())?)?;
            }
            return Ok(self.visible());
        }
        let mut out: BTreeMap<(RowKey, V), i64> = BTreeMap::new();
        for row in self.rows() {
            scope.charge_row_bytes(1, Self::delta_work_bytes(row.value)?)?;
            if row.time.less_equal(as_of) {
                let key = (row.key, row.value.clone());
                let next = out
                    .get(&key)
                    .copied()
                    .map_or(0, |support| support)
                    .checked_add(row.diff.value())
                    .ok_or(FlowError::Overflow)?;
                if next == 0 {
                    out.remove(&key);
                } else {
                    out.insert(key, next);
                }
            }
        }
        Ok(out)
    }

    /// Returns a sorted copy of rows whose row key is in `[start, end]`.
    #[must_use]
    pub fn range_seek(&mut self, start: RowKey, end: RowKey) -> Vec<Delta<V>> {
        let mut scope = WorkScope::new(usize::MAX, usize::MAX);
        self.range_seek_budgeted(start, end, &mut scope)
            .unwrap_or_default()
    }

    /// Returns a sorted range copy under a checked row/byte/probe budget.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::RecursionWorkLimit`] when the requested range
    /// exceeds `scope`, or [`FlowError::Overflow`] on checked accounting.
    pub fn range_seek_budgeted(
        &mut self,
        start: RowKey,
        end: RowKey,
        scope: &mut WorkScope,
    ) -> Result<Vec<Delta<V>>, FlowError> {
        if start > end {
            return Ok(Vec::new());
        }
        let mut rows = Vec::new();
        let mut probes = 0_u64;
        for level in &self.levels {
            for run in level {
                let rows_view = run.slice();
                scope.charge_probes(1)?;
                let begin = lower_bound(rows_view, &start);
                let finish = upper_bound(rows_view, &end);
                probes = probes
                    .checked_add(u64::from(run.len().max(1).ilog2()) + 2)
                    .ok_or(FlowError::Overflow)?;
                for row in &rows_view[begin..finish] {
                    scope.charge_row_bytes(1, Self::delta_work_bytes(&row.value)?)?;
                    rows.push(Delta {
                        key: row.key,
                        value: row.value.clone(),
                        time: row.time,
                        diff: row.diff,
                    });
                }
            }
        }
        self.work.seek_probes = self
            .work
            .seek_probes
            .checked_add(probes)
            .ok_or(FlowError::Overflow)?;
        if rows
            .windows(2)
            .any(|window| row_cmp(&window[0], &window[1]) == std::cmp::Ordering::Greater)
        {
            rows.sort_unstable_by(row_cmp);
        }
        Ok(rows)
    }

    /// Borrowing-friendly alias for [`Arrangement::range_seek`].
    #[must_use]
    pub fn seek_range(&mut self, range: RangeInclusive<RowKey>) -> Vec<Delta<V>> {
        let (start, end) = range.into_inner();
        self.range_seek(start, end)
    }

    /// Seeks all retained rows for one exact row key.
    #[must_use]
    pub fn seek_key(&mut self, key: &RowKey) -> Vec<Delta<V>> {
        self.range_seek(*key, *key)
    }
}
