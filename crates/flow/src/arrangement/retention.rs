//! Arrangement pins, since/frontier retention, and compaction.

use super::{Arrangement, CompactionBudget, FrontierMergeCursor, WorkScope};
use crate::CompactionReport;
use crate::{CanonicalValue, Delta, FlowError, Frontier, Pin, RowKey, Run, Time, Weight};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
    fmt::Debug,
    mem::{self, size_of},
    sync::Arc,
};

const FRONTIER_SEGMENT_ROWS: usize = 2048;
type FrontierOutput<V> = (Vec<Vec<Delta<V>>>, usize);

/// One owned head of a bounded merge stream.  The row is moved out of the
/// source segment when it is popped, so compaction never clones a payload to
/// build a second full trace.
struct FrontierHeapItem<V> {
    before: bool,
    row: Delta<V>,
    stream: usize,
}

impl<V: Ord> PartialEq for FrontierHeapItem<V> {
    fn eq(&self, other: &Self) -> bool {
        self.before == other.before
            && self.row.key == other.row.key
            && self.row.value == other.row.value
            && self.row.time == other.row.time
    }
}

impl<V: Ord> Eq for FrontierHeapItem<V> {}

impl<V: Ord> PartialOrd for FrontierHeapItem<V> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<V: Ord> Ord for FrontierHeapItem<V> {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max heap; reverse the canonical row order to pop
        // the smallest key first.  Before rows sort ahead of post-frontier
        // rows so the representative fold is a single bounded group.
        other
            .row
            .key
            .cmp(&self.row.key)
            .then_with(|| other.row.value.cmp(&self.row.value))
            .then_with(|| other.row.time.cmp(&self.row.time))
            .then_with(|| other.before.cmp(&self.before))
    }
}

fn merge_frontier_segments<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static>(
    segments: Vec<super::FrontierSegment<V>>,
    representative: Time,
) -> Result<FrontierOutput<V>, FlowError> {
    let input_rows = segments.iter().try_fold(0usize, |total, segment| {
        total
            .checked_add(segment.rows.len())
            .ok_or(FlowError::Overflow)
    })?;
    let mut streams: Vec<(bool, VecDeque<Delta<V>>)> = segments
        .into_iter()
        .map(|segment| {
            let super::FrontierSegment { before, rows } = segment;
            let mut rows = rows.into_vec();
            rows.sort_unstable_by(crate::batch::row_cmp);
            (before, rows.into())
        })
        .collect();
    let mut heap = BinaryHeap::new();
    for (stream, (before, rows)) in streams.iter_mut().enumerate() {
        if let Some(row) = rows.pop_front() {
            heap.push(FrontierHeapItem {
                before: *before,
                row,
                stream,
            });
        }
    }

    let mut output_segments = Vec::new();
    let mut output = Vec::with_capacity(FRONTIER_SEGMENT_ROWS);
    let mut group: Option<(bool, RowKey, V, Time, i64)> = None;
    while let Some(item) = heap.pop() {
        let FrontierHeapItem {
            before,
            row,
            stream,
        } = item;
        if let Some(next) = streams[stream].1.pop_front() {
            heap.push(FrontierHeapItem {
                before: streams[stream].0,
                row: next,
                stream,
            });
        }
        let row_time = row.time;
        let row_diff = row.diff.value();
        let same = group
            .as_ref()
            .is_some_and(|(old_before, key, value, time, _)| {
                *old_before == before
                    && *key == row.key
                    && *value == row.value
                    && (before || *time == row_time)
            });
        if same {
            let (_, _, _, _, diff) = group.as_mut().ok_or(FlowError::InvalidRoot)?;
            *diff = diff.checked_add(row_diff).ok_or(FlowError::Overflow)?;
        } else {
            if let Some((old_before, key, value, time, diff)) = group.take()
                && diff != 0
            {
                output.push(Delta {
                    key,
                    value,
                    time: if old_before { representative } else { time },
                    diff: Weight::from_nonzero(diff)?,
                });
                if output.len() == FRONTIER_SEGMENT_ROWS {
                    output_segments.push(mem::take(&mut output));
                    output.reserve(FRONTIER_SEGMENT_ROWS);
                }
            }
            group = Some((before, row.key, row.value, row_time, row_diff));
        }
    }
    if let Some((old_before, key, value, time, diff)) = group
        && diff != 0
    {
        output.push(Delta {
            key,
            value,
            time: if old_before { representative } else { time },
            diff: Weight::from_nonzero(diff)?,
        });
    }
    if !output.is_empty() {
        output_segments.push(output);
    }
    Ok((output_segments, input_rows))
}

fn flush_frontier_segment<V>(cursor: &mut FrontierMergeCursor<V>) {
    if let Some(before) = cursor.current_before.take()
        && !cursor.current.is_empty()
    {
        let rows = mem::take(&mut cursor.current);
        cursor.segments.push(super::FrontierSegment {
            before,
            rows: rows.into_boxed_slice(),
        });
    }
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Arrangement<V> {
    /// Creates a pin at the current since frontier.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when pin identity allocation is
    /// exhausted.
    pub fn pin(&mut self) -> Result<Pin, FlowError> {
        let frontier = self.trace.since_frontier();
        if !self.pins.admits(&frontier) {
            return Err(FlowError::PinCapacity);
        }
        let pin = self.trace.pin()?;
        self.pins.insert(pin, frontier, None)?;
        Ok(pin)
    }

    /// Creates a pin at an explicit observation time.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidPin`] when the time is outside the
    /// retained/upper frontiers, or [`FlowError::Overflow`] on allocation.
    pub fn pin_at(&mut self, time: Time) -> Result<Pin, FlowError> {
        self.pin_frontier(Frontier::new(time))
    }

    /// Creates a pin for an antichain of observation times.
    ///
    /// Every lane must be within the current upper frontier and at or after
    /// the retained since frontier. Keeping the full antichain prevents a
    /// scalar representative from allowing compaction to cross an
    /// incomparable recursive lane.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidPin`] when a lane is outside the retained
    /// or upper frontiers, or [`FlowError::Overflow`] on allocation.
    pub fn pin_frontier(&mut self, frontier: Frontier) -> Result<Pin, FlowError> {
        if !frontier.advances_from(&self.trace.since_frontier())
            || !frontier
                .elements()
                .iter()
                .all(|time| self.trace.upper().allows_time(*time))
        {
            return Err(FlowError::InvalidPin);
        }
        if !self.pins.admits(&frontier) {
            return Err(FlowError::PinCapacity);
        }
        let pin = self.trace.pin()?;
        self.pins.insert(pin, frontier, None)?;
        Ok(pin)
    }

    /// Creates a pin that expires once `expiry` is reached.
    ///
    /// The expiry must be at or after every lane in the retained frontier.
    /// Expired leases are removed automatically before consolidation and can
    /// also be collected explicitly with [`Self::expire_pins`].
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidPin`] when the expiry is before a retained
    /// lane, or [`FlowError::PinCapacity`] when the lease bound is exhausted.
    pub fn pin_until(&mut self, expiry: Time) -> Result<Pin, FlowError> {
        self.pin_frontier_until(self.trace.since_frontier(), expiry)
    }

    /// Creates an antichain pin with a checked expiry time.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidPin`] when either the frontier or expiry is
    /// outside the current retention contract, and [`FlowError::PinCapacity`]
    /// when the bounded lease registry is full.
    pub fn pin_frontier_until(
        &mut self,
        frontier: Frontier,
        expiry: Time,
    ) -> Result<Pin, FlowError> {
        if !frontier.advances_from(&self.trace.since_frontier())
            || !frontier
                .elements()
                .iter()
                .all(|time| self.trace.upper().allows_time(*time))
            || frontier
                .elements()
                .iter()
                .any(|time| !time.less_equal(expiry))
        {
            return Err(FlowError::InvalidPin);
        }
        if !self.pins.admits(&frontier) {
            return Err(FlowError::PinCapacity);
        }
        let pin = self.trace.pin()?;
        self.pins.insert(pin, frontier, Some(expiry))?;
        Ok(pin)
    }

    /// Sets the maximum number of concurrently retained observation leases.
    ///
    /// Shrinking below the current count is rejected so an already admitted
    /// arrangement cannot silently lose a retention guarantee.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::PinCapacity`] for a zero limit or a limit below
    /// the number of active leases.
    pub fn set_pin_limit(&mut self, limit: usize) -> Result<(), FlowError> {
        self.pins.set_limit(limit)
    }

    /// Sets the maximum retained frontier bytes across active observation
    /// leases.  A lease consumes one fixed-width slot for each frontier lane.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::PinCapacity`] for a zero limit or a limit below
    /// the bytes already retained by active leases.
    pub fn set_pin_byte_limit(&mut self, limit: usize) -> Result<(), FlowError> {
        self.pins.set_byte_limit(limit)
    }

    /// Sets both active lease bounds atomically.
    ///
    /// The arrangement keeps the previous limits when either requested bound
    /// cannot admit the current leases.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::PinCapacity`] for a zero bound or a bound below
    /// the currently retained lease count/bytes.
    pub fn set_pin_limits(&mut self, count: usize, bytes: usize) -> Result<(), FlowError> {
        if count == 0 || count < self.pins.len() || bytes == 0 || bytes < self.pins.bytes() {
            return Err(FlowError::PinCapacity);
        }
        self.pins.set_limit(count)?;
        self.pins.set_byte_limit(bytes)
    }

    /// Expires all leases at or before `now` and returns the number released.
    pub fn expire_pins(&mut self, now: Time) -> usize {
        self.pins.expire(now)
    }

    /// Releases a pin and allows later compaction to advance.
    pub fn release_pin(&mut self, pin: Pin) -> bool {
        self.pins.remove(pin)
    }

    /// Returns the number of active observation pins.
    #[must_use]
    pub fn pin_count(&self) -> usize {
        self.pins.len()
    }

    /// Returns the frontier bytes retained by active observation leases.
    #[must_use]
    pub fn pin_bytes(&self) -> usize {
        self.pins.bytes()
    }

    /// Returns the maximum frontier bytes admitted for observation leases.
    #[must_use]
    pub fn pin_byte_limit(&self) -> usize {
        self.pins.byte_limit()
    }

    /// Performs a bounded leveled merge.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidCompactionBudget`] for an unusable budget,
    /// [`FlowError::Overflow`] for checked counters, or
    /// [`FlowError::InvalidRoot`] when a merged run cannot be admitted.
    pub fn compact_budgeted(
        &mut self,
        budget: CompactionBudget,
    ) -> Result<CompactionReport, FlowError> {
        if budget.max_rows == 0 || budget.max_runs < 2 {
            return Err(FlowError::InvalidCompactionBudget);
        }
        for level in 0..self.levels.len() {
            if self.levels[level].len() < 2 {
                continue;
            }
            let Some(plan) = self.plan_compaction_level(level, budget, &self.trace, 0, 0, true)?
            else {
                continue;
            };
            let new_compacted = self
                .work
                .compacted_rows
                .checked_add(plan.input_rows)
                .ok_or(FlowError::Overflow)?;
            let new_no_op = self
                .work
                .no_op_rows
                .checked_add(plan.input_rows.saturating_sub(plan.output_rows))
                .ok_or(FlowError::Overflow)?;
            let promoted_runs = self
                .work
                .promoted_runs
                .checked_add(u64::from(plan.promoted))
                .ok_or(FlowError::Overflow)?;
            let report = CompactionReport {
                input_rows: plan.input_rows,
                output_rows: plan.output_rows,
                merged_runs: u64::try_from(plan.take).map_err(|_| FlowError::Overflow)?,
                complete: false,
            };
            self.commit_compaction(plan);
            self.work.compacted_rows = new_compacted;
            self.work.no_op_rows = new_no_op;
            self.work.promoted_runs = promoted_runs;
            return Ok(CompactionReport {
                complete: self
                    .levels
                    .iter()
                    .all(|runs| runs.len() <= self.max_runs_per_level),
                ..report
            });
        }
        Ok(CompactionReport {
            complete: self
                .levels
                .iter()
                .all(|runs| runs.len() <= self.max_runs_per_level),
            ..CompactionReport::default()
        })
    }

    /// Consolidates the trace at a since frontier while retaining all
    /// observations permitted at or after that frontier.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::SinceBeyondUpper`] for a future bound,
    /// [`FlowError::PinnedFrontier`] when an active pin would be crossed,
    /// [`FlowError::Overflow`] for checked counters, or another checked
    /// consolidation error.
    pub fn consolidate(&mut self, since: Time) -> Result<(), FlowError> {
        self.consolidate_frontier(Frontier::new(since))
    }

    /// Consolidates the trace at an antichain since frontier.
    ///
    /// Rows before any lane are folded to the deterministic representative for
    /// physical storage, while the complete antichain remains bound to the
    /// trace and fences future observations. Existing scalar callers should
    /// use [`Self::consolidate`]; recursive callers should pass all lanes here.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::SinceBeyondUpper`] for a future bound,
    /// [`FlowError::SinceRegressed`] for a regressed frontier,
    /// [`FlowError::PinnedFrontier`] when an active pin would be crossed,
    /// [`FlowError::Overflow`] for checked counters, or another checked
    /// consolidation error.
    pub fn consolidate_frontier(&mut self, since: Frontier) -> Result<(), FlowError> {
        let mut scope = WorkScope::new(usize::MAX, usize::MAX);
        self.consolidate_frontier_budgeted(since, &mut scope)
    }

    /// Consolidates an antichain frontier under a checked work envelope.
    ///
    /// The arrangement is changed only after the retained rows fit the
    /// supplied scope and the replacement run has been admitted.
    ///
    /// # Errors
    ///
    /// Returns the same frontier/pin errors as [`Self::consolidate_frontier`]
    /// and [`FlowError::RecursionWorkLimit`] when retained rows exceed
    /// `scope`.
    #[allow(clippy::too_many_lines)]
    pub fn consolidate_frontier_budgeted(
        &mut self,
        since: Frontier,
        scope: &mut WorkScope,
    ) -> Result<(), FlowError> {
        if !since
            .elements()
            .iter()
            .all(|time| self.trace.upper().allows_time(*time))
        {
            return Err(FlowError::SinceBeyondUpper);
        }
        if !since.advances_from(&self.trace.since_frontier()) {
            return Err(FlowError::SinceRegressed);
        }
        let _ = self.pins.expire(since.upper());
        if self.pins.blocks(&since) {
            return Err(FlowError::PinnedFrontier);
        }
        let representative = since.upper();
        let mut cursor = self
            .frontier_merge
            .take()
            .unwrap_or_else(|| FrontierMergeCursor {
                since: since.clone(),
                source: self
                    .levels
                    .iter()
                    .flat_map(|level| level.iter().cloned())
                    .collect(),
                run: 0,
                row: 0,
                segments: Vec::new(),
                current_before: None,
                current: Vec::new(),
                bytes: 0,
            });
        if cursor.since != since {
            return Err(FlowError::InvalidCompactionBudget);
        }
        while cursor.run < cursor.source.len() {
            let run = &cursor.source[cursor.run];
            let Some(row) = run.slice().get(cursor.row) else {
                cursor.run = cursor.run.saturating_add(1);
                cursor.row = 0;
                continue;
            };
            let bytes = size_of::<Delta<V>>()
                .checked_add(row.value.owned_bytes())
                .ok_or(FlowError::Overflow)?;
            if let Err(error) = scope.charge_row_bytes(1, bytes) {
                self.frontier_merge = Some(cursor);
                return Err(error);
            }
            // `cursor` is temporarily detached from `self` while this call
            // advances. Include its already admitted output debt explicitly;
            // otherwise repeated pauses could grow the detached cursor past
            // the arrangement's retained-byte envelope.
            let retained = self
                .retained_bytes()
                .checked_add(cursor.bytes)
                .and_then(|total| total.checked_add(bytes));
            if retained.is_none_or(|total| total > self.max_retained_bytes) {
                self.frontier_merge = Some(cursor);
                return Err(FlowError::CompactionBackpressure);
            }
            let owned_row = row.clone();
            let before = since.covers(owned_row.time);
            if cursor.current_before != Some(before)
                || cursor.current.len() >= FRONTIER_SEGMENT_ROWS
            {
                flush_frontier_segment(&mut cursor);
                cursor.current_before = Some(before);
            }
            cursor.current.push(owned_row);
            cursor.bytes = cursor.bytes.checked_add(bytes).ok_or(FlowError::Overflow)?;
            cursor.row = cursor.row.saturating_add(1);
        }
        flush_frontier_segment(&mut cursor);
        // Merge the bounded immutable segments with a k-way heap. Only one
        // current key aggregate and one row per segment are live during the
        // merge; no flattened before/after vectors or whole-trace map is
        // allocated.
        let (compacted_segments, input_rows) =
            merge_frontier_segments(cursor.segments, representative)?;
        let output_rows = compacted_segments.iter().map(Vec::len).sum();
        let mut next_trace = self.trace.clone();
        next_trace.advance_since_frontier(since)?;
        let mut runs = Vec::with_capacity(compacted_segments.len());
        for segment in compacted_segments {
            let run = Run::from_rows(segment, next_trace.clone())?;
            runs.push(Arc::new(run));
        }
        self.levels = vec![runs];
        self.trace = next_trace;
        self.frontier_merge = None;
        self.work.compacted_rows = self
            .work
            .compacted_rows
            .checked_add(u64::try_from(input_rows).map_err(|_| FlowError::Overflow)?)
            .ok_or(FlowError::Overflow)?;
        self.work.no_op_rows = self
            .work
            .no_op_rows
            .checked_add(
                u64::try_from(input_rows.saturating_sub(output_rows))
                    .map_err(|_| FlowError::Overflow)?,
            )
            .ok_or(FlowError::Overflow)?;
        Ok(())
    }
}
