//! Arrangement construction, append admission, and bounded level merges.

mod compaction;

use super::{
    Arrangement, BatchRecord, CompactionBudget, CompactionPlan, DurableCache, WorkCounters,
    checked_u64_add,
};
use crate::batch::canonical_bytes;
use crate::{
    ArrangementKey, ArrangementRoot, CanonicalValue, FlowError, Microbatch, Run, TraceSpine,
    arrangement_coverage,
};
use backend_version::{
    AuthorizedCompleteCoverage, CoverageWitness, DeltaError, MapChange, prepare_delta_with_state,
};
use std::{collections::BTreeMap, fmt::Debug, sync::Arc};

const SEGMENT_ROWS: usize = 2048;

fn map_delta_error(error: DeltaError) -> FlowError {
    match error {
        DeltaError::IncompleteBase => FlowError::IncompleteFrontier,
        DeltaError::BaseMismatch
        | DeltaError::BeforeMismatch
        | DeltaError::Unsorted
        | DeltaError::DuplicateKey
        | DeltaError::TargetMismatch
        | DeltaError::NonAdjacent
        | DeltaError::CompositionMismatch
        | DeltaError::Canonical(_) => FlowError::InvalidRoot,
    }
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Arrangement<V> {
    /// Creates an empty arrangement with four-way leveled compaction.
    #[must_use]
    pub fn new() -> Self {
        Self::with_coverage(arrangement_coverage())
    }

    /// Creates an arrangement bound to an upstream admitted authority scope.
    ///
    /// Production callers should pass the capability received from their
    /// authority adapter.  [`Self::new`] intentionally creates an unbound
    /// storage object and is useful for staging and test construction only.
    #[must_use]
    pub fn with_authorized_coverage(coverage: AuthorizedCompleteCoverage) -> Self {
        Self::with_coverage(CoverageWitness::Complete(coverage))
    }

    fn with_coverage(coverage: CoverageWitness) -> Self {
        let state = backend_version::RelationState::empty(coverage);
        Self {
            levels: vec![Vec::new()],
            trace: TraceSpine::new(),
            next_batch: 0,
            work: WorkCounters::default(),
            state,
            visible_len: 0,
            pins: super::PinRegistry::new(),
            history: Vec::new(),
            history_rows: 0,
            history_bytes: 0,
            max_history: 1024,
            max_history_rows: 65_536,
            max_history_bytes: 8 * 1024 * 1024,
            max_subscription_events: 1024,
            max_subscription_rows: 65_536,
            max_runs_per_level: 4,
            max_retained_runs: 64,
            max_retained_bytes: 64 * 1024 * 1024,
            max_levels: 32,
            promotion_debt: 0,
            max_promotion_debt: 32,
            frontier_merge: None,
            durable_cache: Arc::new(std::sync::Mutex::new(DurableCache::default())),
        }
    }

    /// Configures the bounded number of runs retained per level.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidCompactionBudget`] when fewer than two
    /// runs are permitted at a level.
    pub fn with_level_run_limit(mut self, limit: usize) -> Result<Self, FlowError> {
        if limit < 2 {
            return Err(FlowError::InvalidCompactionBudget);
        }
        self.max_runs_per_level = limit;
        Ok(self)
    }

    /// Sets the bounded debt allowed for identity-only promotions.
    ///
    /// Once the debt is exhausted, a stalled merge returns backpressure
    /// instead of growing levels indefinitely.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidCompactionBudget`] for a zero limit or a
    /// limit below the currently accumulated debt.
    pub fn set_promotion_debt_limit(&mut self, limit: usize) -> Result<(), FlowError> {
        if limit == 0 || limit < self.promotion_debt {
            return Err(FlowError::InvalidCompactionBudget);
        }
        self.max_promotion_debt = limit;
        Ok(())
    }

    /// Sets global retained run, byte, and level debt bounds.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::CompactionBackpressure`] when the requested
    /// limits are below currently retained debt.
    pub fn set_retained_limits(
        &mut self,
        runs: usize,
        bytes: usize,
        levels: usize,
    ) -> Result<(), FlowError> {
        if runs < self.run_count() || bytes < self.retained_bytes() || levels < self.level_count() {
            return Err(FlowError::CompactionBackpressure);
        }
        self.max_retained_runs = runs;
        self.max_retained_bytes = bytes;
        self.max_levels = levels;
        Ok(())
    }

    /// Limits retained subscriber history.
    pub fn set_history_limit(&mut self, limit: usize) {
        self.max_history = limit;
        self.trim_history();
    }

    /// Limits the number of retained rows in subscriber history.
    ///
    /// A record-count limit alone can retain an unexpectedly large payload
    /// when each transition contains a broad replacement. This second bound
    /// keeps reset recovery and delta history proportional to a declared row
    /// envelope while preserving the newest transitions.
    pub fn set_history_row_limit(&mut self, limit: usize) {
        self.max_history_rows = limit;
        self.trim_history();
    }

    /// Limits the encoded bytes retained for subscriber history.
    ///
    /// The accounting uses the canonical delta encoding, including payload
    /// bytes, so a record-count or row-count limit cannot hide a broad value
    /// replacement behind a small number of rows.
    pub fn set_history_byte_limit(&mut self, limit: usize) {
        self.max_history_bytes = limit;
        self.trim_history();
    }

    /// Sets the maximum number of queued delta events admitted to one
    /// subscription. A reset event always occupies one queue slot.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidSubscriptionLimit`] for a zero limit.
    pub fn set_subscription_limit(&mut self, limit: usize) -> Result<(), FlowError> {
        if limit == 0 {
            return Err(FlowError::InvalidSubscriptionLimit);
        }
        self.max_subscription_events = limit;
        Ok(())
    }

    /// Limits the number of delta rows queued while recovering a subscriber.
    ///
    /// A reset remains one bounded event slot; a delta replay is converted to
    /// a reset before this row envelope can be exceeded.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidSubscriptionLimit`] for a zero limit.
    pub fn set_subscription_row_limit(&mut self, limit: usize) -> Result<(), FlowError> {
        if limit == 0 {
            return Err(FlowError::InvalidSubscriptionLimit);
        }
        self.max_subscription_rows = limit;
        Ok(())
    }

    /// Appends one immutable batch and performs at most one bounded level
    /// merge. Empty/canceled batches do not advance subscriber sequence.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::FrontierRegressed`] for a stale batch frontier,
    /// [`FlowError::Overflow`] for checked support/work arithmetic, or
    /// [`FlowError::InvalidRoot`] when canonical root admission fails.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "append takes ownership of an immutable batch for trace admission"
    )]
    pub fn append(&mut self, batch: Microbatch<V>) -> Result<(), FlowError> {
        let upper = batch.trace.upper();
        self.append_at_frontier(batch, upper)
    }

    /// Appends a batch while preserving an explicitly supplied upper
    /// antichain. This is the publication path for recursive lanes whose
    /// batch-local singleton frontier would otherwise erase incomparable
    /// progress.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidFrontier`] when a row is outside the
    /// supplied frontier, [`FlowError::FrontierRegressed`] for stale progress,
    /// [`FlowError::Overflow`] for checked support/work arithmetic, or
    /// [`FlowError::InvalidRoot`] when canonical root admission fails.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the frontier append facade owns the immutable batch at publication"
    )]
    pub fn append_at_frontier(
        &mut self,
        batch: Microbatch<V>,
        upper: crate::Frontier,
    ) -> Result<(), FlowError> {
        if batch.cursor().any(|row| !upper.covers(row.time)) {
            return Err(FlowError::InvalidFrontier);
        }
        if batch.is_empty() {
            return self.append_empty(&batch, upper);
        }
        self.append_nonempty(&batch, upper)
    }

    fn append_empty(
        &mut self,
        batch: &Microbatch<V>,
        batch_upper: crate::Frontier,
    ) -> Result<(), FlowError> {
        let input_rows = checked_u64_add(self.work.input_rows, batch.input_len)?;
        let no_op_rows = checked_u64_add(self.work.no_op_rows, batch.input_len)?;
        let mut next_trace = self.trace.clone();
        next_trace.advance_upper(batch_upper)?;
        self.work.input_rows = input_rows;
        self.work.no_op_rows = no_op_rows;
        self.trace = next_trace;
        Ok(())
    }

    fn pending_for_batch(
        &self,
        batch: &Microbatch<V>,
    ) -> Result<BTreeMap<ArrangementKey<V>, i64>, FlowError> {
        let mut pending = BTreeMap::<ArrangementKey<V>, i64>::new();
        for row in batch.cursor() {
            let key = ArrangementKey::new(row.key, row.value.clone());
            let old = pending
                .get(&key)
                .copied()
                .or_else(|| self.state.get(&key).copied())
                .map_or(0, |support| support);
            let next = old
                .checked_add(row.diff.value())
                .ok_or(FlowError::Overflow)?;
            pending.insert(key, next);
        }
        Ok(pending)
    }

    fn next_visible_len(
        &self,
        changes: &[MapChange<crate::ArrangementRelation<V>>],
    ) -> Result<usize, FlowError> {
        let mut visible_len = self.visible_len;
        for change in changes {
            let was_visible = change.before.is_some();
            let is_visible = change.after.is_some();
            match (was_visible, is_visible) {
                (false, true) => {
                    visible_len = visible_len.checked_add(1).ok_or(FlowError::Overflow)?;
                }
                (true, false) => {
                    visible_len = visible_len.checked_sub(1).ok_or(FlowError::InvalidRoot)?;
                }
                _ => {}
            }
        }
        Ok(visible_len)
    }

    fn record_transition(
        &mut self,
        sequence: u64,
        before: ArrangementRoot<V>,
        after: ArrangementRoot<V>,
        batch: &Microbatch<V>,
    ) {
        self.next_batch = sequence;
        let deltas = Arc::clone(&batch.rows);
        let bytes = canonical_bytes(&deltas, [0; 32]).len();
        self.history_rows = self.history_rows.saturating_add(deltas.len());
        self.history_bytes = self.history_bytes.saturating_add(bytes);
        self.history.push(BatchRecord {
            sequence,
            before,
            after,
            deltas,
            bytes,
        });
        self.trim_history();
    }

    #[allow(clippy::too_many_lines)]
    fn append_nonempty(
        &mut self,
        batch: &Microbatch<V>,
        batch_upper: crate::Frontier,
    ) -> Result<(), FlowError> {
        // Prepare every fallible part against the one published relation
        // state. Trace metadata and physical levels are changed only after
        // this complete state transition has been admitted.
        let mut next_trace = self.trace.clone();
        next_trace.advance_upper(batch_upper)?;
        let input_rows = checked_u64_add(self.work.input_rows, batch.input_len)?;
        let before = self.state.root();
        let pending = self.pending_for_batch(batch)?;
        let changes = pending
            .iter()
            .filter_map(|(key, next)| {
                let before = self.state.get(key).copied();
                let after = (*next != 0).then_some(*next);
                (before != after).then(|| MapChange {
                    key: key.clone(),
                    before,
                    after,
                })
            })
            .collect::<Vec<_>>();
        let has_changes = !changes.is_empty();
        let visible_len = self.next_visible_len(&changes)?;
        let (prepared, root_work) =
            prepare_delta_with_state(&self.state, changes).map_err(map_delta_error)?;
        let after = prepared.delta().target();
        let next_state = prepared.commit(&self.state).map_err(map_delta_error)?;
        let mut next_work = self.work;
        next_work.input_rows = input_rows;
        next_work.output_rows = checked_u64_add(self.work.output_rows, batch.len())?;
        if has_changes {
            next_work.root_rows = checked_u64_add(self.work.root_rows, root_work.rows)?;
            next_work.root_nodes = checked_u64_add(self.work.root_nodes, root_work.nodes)?;
            next_work.root_probes =
                checked_u64_add(self.work.root_probes, root_work.visited_nodes)?;
            next_work.root_reused_nodes =
                checked_u64_add(self.work.root_reused_nodes, root_work.reused_nodes)?;
        }
        let sequence = if before == after {
            None
        } else if let Some(sequence) = self.next_batch.checked_add(1) {
            Some(sequence)
        } else {
            return Err(FlowError::Overflow);
        };
        if before == after {
            next_work.no_op_rows = checked_u64_add(self.work.no_op_rows, batch.len())?;
        }
        let owner = Run::owner(batch.rows.clone())?;
        let incoming_bytes = owner.retained_bytes;
        let pending_runs = batch.len().div_ceil(SEGMENT_ROWS);
        if self.run_count().saturating_add(pending_runs) > self.max_retained_runs
            || self
                .retained_bytes()
                .checked_add(incoming_bytes)
                .is_none_or(|bytes| bytes > self.max_retained_bytes)
        {
            return Err(FlowError::CompactionBackpressure);
        }
        let segments = batch
            .rows
            .chunks(SEGMENT_ROWS)
            .enumerate()
            .map(|(index, rows)| {
                Run::segment(
                    Arc::clone(&owner),
                    index * SEGMENT_ROWS,
                    index * SEGMENT_ROWS + rows.len(),
                    next_trace.clone(),
                )
                .map(Arc::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pending_runs = segments.len();
        let compaction =
            self.plan_compaction_after_append(&next_trace, pending_runs, incoming_bytes)?;
        if let Some(plan) = compaction.as_ref() {
            next_work.compacted_rows = next_work
                .compacted_rows
                .checked_add(plan.input_rows)
                .ok_or(FlowError::Overflow)?;
            next_work.no_op_rows = next_work
                .no_op_rows
                .checked_add(plan.input_rows.saturating_sub(plan.output_rows))
                .ok_or(FlowError::Overflow)?;
            if plan.promoted {
                next_work.promoted_runs = next_work
                    .promoted_runs
                    .checked_add(1)
                    .ok_or(FlowError::Overflow)?;
            }
        }

        // Commit contains no fallible arithmetic or canonical construction.
        self.state = next_state;
        self.visible_len = visible_len;
        self.trace = next_trace;
        self.work = next_work;
        self.levels[0].extend(segments);
        if let Some(plan) = compaction {
            self.commit_compaction(plan);
        }
        // A segmented batch can add several L0 runs at once. Drain every
        // immediately admissible L0 debt in bounded steps so segmentation
        // never turns one append into an overfull level.
        let budget = CompactionBudget {
            max_rows: CompactionBudget::default().max_rows,
            max_runs: self.max_runs_per_level,
        };
        while self.levels[0].len() > self.max_runs_per_level {
            let Some(plan) = self.plan_compaction_level(0, budget, &self.trace, 0, 0, true)? else {
                break;
            };
            self.work.compacted_rows = self
                .work
                .compacted_rows
                .checked_add(plan.input_rows)
                .ok_or(FlowError::Overflow)?;
            self.work.no_op_rows = self
                .work
                .no_op_rows
                .checked_add(plan.input_rows.saturating_sub(plan.output_rows))
                .ok_or(FlowError::Overflow)?;
            self.commit_compaction(plan);
        }
        if let Some(sequence) = sequence {
            self.record_transition(sequence, before, after, batch);
        }
        Ok(())
    }
}
