//! Shared arrangement state and compaction metadata.

use super::PinRegistry;
use crate::{
    ArrangementRelation, ArrangementRoot, CanonicalValue, Delta, FlowError, Frontier,
    RelationState, Run, TraceSpine,
};
use std::{
    collections::BTreeMap,
    fmt::Debug,
    sync::{Arc, Mutex},
};

pub(crate) fn checked_u64_add(left: u64, right: usize) -> Result<u64, FlowError> {
    left.checked_add(u64::try_from(right).map_err(|_| FlowError::Overflow)?)
        .ok_or(FlowError::Overflow)
}

/// Bounded compaction admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompactionBudget {
    /// Maximum input rows a merge may inspect.
    pub max_rows: usize,
    /// Maximum number of runs to merge during this call.
    pub max_runs: usize,
}

impl Default for CompactionBudget {
    fn default() -> Self {
        Self {
            max_rows: 4096,
            max_runs: 4,
        }
    }
}

/// Measured result of one bounded merge step.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompactionReport {
    /// Rows inspected by the merge.
    pub input_rows: u64,
    /// Rows retained after consolidation.
    pub output_rows: u64,
    /// Runs merged.
    pub merged_runs: u64,
    /// Whether this call reached a stable level shape.
    pub complete: bool,
}

/// Public structural counters for performance assertions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkCounters {
    /// Input rows accepted from batches.
    pub input_rows: u64,
    /// Rows admitted into retained runs.
    pub output_rows: u64,
    /// Rows inspected by compaction.
    pub compacted_rows: u64,
    /// Rows canceled or suppressed before downstream publication.
    pub no_op_rows: u64,
    /// Join output rows retained after consolidation.
    pub join_rows: u64,
    /// Join key probes performed against a batch index or retained runs.
    pub join_probes: u64,
    /// Candidate join pairs emitted before consolidation.
    pub join_fanout: u64,
    /// Approximate temporary bytes occupied by candidate join rows.
    pub join_bytes: u64,
    /// Rows inserted into a per-call batch join index.
    pub join_index_rows: u64,
    /// Arrangement probes/seeks performed.
    pub seek_probes: u64,
    /// Changed logical entries admitted while publishing a state root.
    ///
    /// This counter describes path-copy work; no-op batches skip it entirely.
    pub root_rows: u64,
    /// Canonical tree leaves and branches rebuilt while publishing a root.
    pub root_nodes: u64,
    /// Canonical nodes visited while locating changed paths.
    pub root_probes: u64,
    /// Immutable canonical child nodes retained by path copying.
    pub root_reused_nodes: u64,
    /// Immutable runs promoted without row inspection because the merge row
    /// envelope could not admit a pair of candidates.
    pub promoted_runs: u64,
}

/// A checked row/byte/probe envelope shared by bounded cursors and operators.
///
/// The scope is intentionally separate from [`WorkCounters`]: counters report
/// completed work, while a scope is an admission fence that stops a lending
/// cursor before it can exceed a caller's quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkScope {
    row_budget: u64,
    byte_budget: u64,
    probe_budget: u64,
    rows: u64,
    bytes: u64,
    probes: u64,
}

impl WorkScope {
    /// Creates a scope from checked row and byte budgets.
    #[must_use]
    pub fn new(max_rows: usize, max_bytes: usize) -> Self {
        Self {
            row_budget: u64::try_from(max_rows).unwrap_or(u64::MAX),
            byte_budget: u64::try_from(max_bytes).unwrap_or(u64::MAX),
            probe_budget: u64::MAX,
            rows: 0,
            bytes: 0,
            probes: 0,
        }
    }

    /// Creates a scope with an explicit ordered-probe envelope.
    #[must_use]
    pub fn with_probes(max_rows: usize, max_bytes: usize, max_probes: usize) -> Self {
        let mut scope = Self::new(max_rows, max_bytes);
        scope.probe_budget = u64::try_from(max_probes).unwrap_or(u64::MAX);
        scope
    }

    /// Charges rows against the scope.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::RecursionWorkLimit`] when the row envelope would
    /// be exceeded, or [`FlowError::Overflow`] when checked accounting wraps.
    pub fn charge_rows(&mut self, rows: usize) -> Result<(), FlowError> {
        let rows = u64::try_from(rows).map_err(|_| FlowError::Overflow)?;
        let next = self.rows.checked_add(rows).ok_or(FlowError::Overflow)?;
        if next > self.row_budget {
            return Err(FlowError::RecursionWorkLimit);
        }
        self.rows = next;
        Ok(())
    }

    /// Charges rows and their encoded bytes atomically.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::RecursionWorkLimit`] when either envelope would
    /// be exceeded, or [`FlowError::Overflow`] when accounting wraps.
    pub fn charge_row_bytes(&mut self, rows: usize, bytes: usize) -> Result<(), FlowError> {
        let rows = u64::try_from(rows).map_err(|_| FlowError::Overflow)?;
        let bytes = u64::try_from(bytes).map_err(|_| FlowError::Overflow)?;
        let next_rows = self.rows.checked_add(rows).ok_or(FlowError::Overflow)?;
        let next_bytes = self.bytes.checked_add(bytes).ok_or(FlowError::Overflow)?;
        if next_rows > self.row_budget || next_bytes > self.byte_budget {
            return Err(FlowError::RecursionWorkLimit);
        }
        self.rows = next_rows;
        self.bytes = next_bytes;
        Ok(())
    }

    /// Charges encoded bytes against the scope.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::RecursionWorkLimit`] when the byte envelope would
    /// be exceeded, or [`FlowError::Overflow`] when checked accounting wraps.
    pub fn charge_bytes(&mut self, bytes: usize) -> Result<(), FlowError> {
        let bytes = u64::try_from(bytes).map_err(|_| FlowError::Overflow)?;
        let next = self.bytes.checked_add(bytes).ok_or(FlowError::Overflow)?;
        if next > self.byte_budget {
            return Err(FlowError::RecursionWorkLimit);
        }
        self.bytes = next;
        Ok(())
    }

    /// Charges ordered probes against the scope.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::RecursionWorkLimit`] when the probe envelope
    /// would be exceeded, or [`FlowError::Overflow`] when accounting wraps.
    pub fn charge_probes(&mut self, probes: usize) -> Result<(), FlowError> {
        let probes = u64::try_from(probes).map_err(|_| FlowError::Overflow)?;
        let next = self.probes.checked_add(probes).ok_or(FlowError::Overflow)?;
        if next > self.probe_budget {
            return Err(FlowError::RecursionWorkLimit);
        }
        self.probes = next;
        Ok(())
    }

    /// Returns rows charged so far.
    #[must_use]
    pub const fn rows(&self) -> u64 {
        self.rows
    }

    /// Returns bytes charged so far.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns ordered probes charged so far.
    #[must_use]
    pub const fn probes(&self) -> u64 {
        self.probes
    }

    /// Returns remaining row capacity.
    #[must_use]
    pub const fn remaining_rows(&self) -> u64 {
        self.row_budget.saturating_sub(self.rows)
    }

    /// Returns remaining byte capacity.
    #[must_use]
    pub const fn remaining_bytes(&self) -> u64 {
        self.byte_budget.saturating_sub(self.bytes)
    }

    /// Returns remaining ordered-probe capacity.
    #[must_use]
    pub const fn remaining_probes(&self) -> u64 {
        self.probe_budget.saturating_sub(self.probes)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BatchRecord<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    pub(crate) sequence: u64,
    pub(crate) before: ArrangementRoot<V>,
    pub(crate) after: ArrangementRoot<V>,
    pub(crate) deltas: Arc<[Delta<V>]>,
    pub(crate) bytes: usize,
}

/// Object identities retained after a durable checkpoint.
///
/// The cache is deliberately an implementation detail of an arrangement. It
/// is keyed by authenticated logical roots, so it can safely be shared by
/// cloned arrangements while allowing checkpoint writers to avoid rebuilding
/// unchanged persistent-tree nodes and immutable runs.
#[derive(Clone, Debug, Default)]
pub(crate) struct DurableCache {
    pub(crate) nodes: BTreeMap<[u8; 32], [u8; 32]>,
    pub(crate) runs: BTreeMap<[u8; 32], [u8; 32]>,
    /// Most recently published reference-index object and its bounded chain
    /// depth.  A checkpoint can retain this immutable root when no new
    /// objects were introduced, avoiding an otherwise needless manifest
    /// object on no-op writes.
    pub(crate) latest_index: Option<([u8; 32], u64, [u8; 32], u16)>,
    /// Immutable history records keyed by their sequence.  A sequence is
    /// assigned once and never rewritten, so retaining its logical/object
    /// pair lets a later checkpoint skip re-encoding an unchanged tail run.
    pub(crate) history: BTreeMap<u64, ([u8; 32], [u8; 32])>,
}

type RetainedRun<V> = Arc<Run<V>>;

/// One immutable bounded output segment of a frontier merge.
#[derive(Clone, Debug)]
pub(crate) struct FrontierSegment<V> {
    pub(crate) before: bool,
    pub(crate) rows: Box<[Delta<V>]>,
}

/// Resumable frontier consolidation state. Runs are retained by identity and
/// rows are copied only into bounded immutable segments as the caller's work
/// envelope admits them; a stalled observer never owns an unbounded scratch
/// vector.
#[derive(Clone, Debug)]
pub(crate) struct FrontierMergeCursor<V> {
    pub(crate) since: Frontier,
    pub(crate) source: Vec<RetainedRun<V>>,
    pub(crate) run: usize,
    pub(crate) row: usize,
    pub(crate) segments: Vec<FrontierSegment<V>>,
    pub(crate) current_before: Option<bool>,
    pub(crate) current: Vec<Delta<V>>,
    /// Bytes copied into the resumable output buffers.  This is part of the
    /// arrangement's retained debt while a merge is paused.
    pub(crate) bytes: usize,
}

pub(crate) struct CompactionPlan<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    pub(crate) level: usize,
    pub(crate) target_level: usize,
    pub(crate) start: usize,
    pub(crate) take: usize,
    pub(crate) merged: Option<RetainedRun<V>>,
    pub(crate) input_rows: u64,
    pub(crate) output_rows: u64,
    pub(crate) promoted: bool,
}

/// A shared leveled arrangement of weighted relation updates.
#[derive(Clone, Debug)]
pub struct Arrangement<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    pub(crate) levels: Vec<Vec<RetainedRun<V>>>,
    pub(crate) trace: TraceSpine,
    pub(crate) next_batch: u64,
    pub(crate) work: WorkCounters,
    /// The sole logical visible state. Physical runs and history below are
    /// adapters bound to this exact relation root.
    pub(crate) state: RelationState<ArrangementRelation<V>>,
    pub(crate) visible_len: usize,
    pub(crate) pins: PinRegistry,
    pub(crate) history: Vec<BatchRecord<V>>,
    pub(crate) history_rows: usize,
    pub(crate) history_bytes: usize,
    pub(crate) max_history: usize,
    pub(crate) max_history_rows: usize,
    pub(crate) max_history_bytes: usize,
    pub(crate) max_subscription_events: usize,
    pub(crate) max_subscription_rows: usize,
    pub(crate) max_runs_per_level: usize,
    pub(crate) max_retained_runs: usize,
    pub(crate) max_retained_bytes: usize,
    pub(crate) max_levels: usize,
    /// Number of identity-only promotions admitted before backpressure.
    pub(crate) promotion_debt: usize,
    pub(crate) max_promotion_debt: usize,
    pub(crate) frontier_merge: Option<FrontierMergeCursor<V>>,
    pub(crate) durable_cache: Arc<Mutex<DurableCache>>,
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Default for Arrangement<V> {
    fn default() -> Self {
        Self::new()
    }
}
