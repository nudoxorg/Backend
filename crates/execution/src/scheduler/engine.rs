//! Scheduler construction, local-first entry points, and diagnostics.

use super::deadline::DeadlineQueue;
use super::guard::Scheduled;
use super::outcome::{ScheduleError, ScheduleOutcome};
use super::request::ScheduleRequest;
use crate::{Admission, AttemptManager, Budget, EnvelopeBudgets, OutputLookup, WorkInterner};
use backend_version::Relation;
use std::sync::Arc;

/// Local-first scheduler owning admission, attempt fences, coalescing, and
/// validated output lookup.
pub struct Scheduler {
    pub(super) admission: Admission,
    pub(super) attempts: Arc<AttemptManager>,
    pub(super) lookup: Arc<OutputLookup>,
    pub(super) interner: Arc<WorkInterner>,
    pub(super) deadlines: Arc<std::sync::Mutex<DeadlineQueue<crate::WorkKey>>>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("admission", &self.admission)
            .field("attempts", &self.attempts)
            .field("lookup", &self.lookup)
            .field("interner", &self.interner)
            .field(
                "deadline_entries",
                &self.deadlines.lock().map_or(0, |queue| queue.live_len()),
            )
            .finish()
    }
}

impl Scheduler {
    /// Creates a scheduler with interactive capacity.
    #[must_use]
    pub fn new(budget: Budget) -> Self {
        Self::with_components(
            Admission::new(budget),
            Arc::new(AttemptManager::new()),
            Arc::new(OutputLookup::with_limits(budget.operations, budget.bytes)),
            WorkInterner::new(4096, 256),
        )
    }

    /// Creates a scheduler with named policy envelopes.
    #[must_use]
    pub fn with_envelopes(budgets: EnvelopeBudgets) -> Self {
        Self::with_envelopes_and_lease_duration(budgets, AttemptManager::DEFAULT_LEASE_DURATION)
    }

    /// Creates a scheduler with named policy envelopes and an owner-clock
    /// attempt lease. Process compositions use this to bind publication
    /// authority to their admitted I/O and execution budget instead of the
    /// one-tick deterministic-test default.
    #[must_use]
    pub fn with_envelopes_and_lease_duration(
        budgets: EnvelopeBudgets,
        lease_duration: u64,
    ) -> Self {
        // The lookup bound is an aggregate of independent policy lanes. The
        // public constructor is infallible, so an impossible aggregate
        // overflow disables eviction by selecting the representable maximum;
        // admission itself still rejects overflowing reservations.
        let max_entries = aggregate_capacity([
            budgets.interactive.operations,
            budgets.background.operations,
            budgets.transfer.operations,
            budgets.compaction.operations,
        ]);
        let max_bytes = aggregate_capacity([
            budgets.interactive.bytes,
            budgets.background.bytes,
            budgets.transfer.bytes,
            budgets.compaction.bytes,
        ]);
        Self::with_components(
            Admission::with_envelopes(budgets),
            Arc::new(AttemptManager::with_lease_duration(lease_duration)),
            Arc::new(OutputLookup::with_limits(max_entries, max_bytes)),
            WorkInterner::new(4096, 256),
        )
    }

    /// Creates a scheduler from explicit shared control components.
    #[must_use]
    pub fn with_components(
        admission: Admission,
        attempts: Arc<AttemptManager>,
        lookup: Arc<OutputLookup>,
        interner: Arc<WorkInterner>,
    ) -> Self {
        Self {
            admission,
            attempts,
            lookup,
            interner,
            deadlines: Arc::new(std::sync::Mutex::new(DeadlineQueue::new())),
        }
    }

    /// Schedules a new attempt.  The generic scheduler has no semantic
    /// authority and therefore never selects a retained result; callers that
    /// hold an engine-issued reuse context use the explicit queue API.
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when no compatible route can be admitted, a
    /// request is coalesced behind an existing leader, or identity/fence
    /// arithmetic fails.
    pub fn schedule<R: Relation>(
        &self,
        request: ScheduleRequest<R>,
    ) -> Result<Scheduled<R>, ScheduleError> {
        // A lower scheduler has no semantic authority from which it can mint
        // a current reuse context. The dispatcher uses the explicit
        // context-bearing entry point when a validated publication is
        // eligible for reuse.
        match self.schedule_or_reuse_with_context(request, None)? {
            ScheduleOutcome::Scheduled(scheduled) => Ok(*scheduled),
            ScheduleOutcome::Reused(output) => Err(ScheduleError::Reusable(Box::new(output))),
            ScheduleOutcome::Waiting(_) => Err(ScheduleError::Coalesced),
        }
    }

    /// Returns the validated output index.
    #[must_use]
    pub fn output_lookup(&self) -> &OutputLookup {
        &self.lookup
    }

    /// Returns the attempt manager for heartbeat, takeover, and low-level
    /// receipt validation.
    #[must_use]
    pub fn attempts(&self) -> &AttemptManager {
        &self.attempts
    }

    /// Returns admission accounting for diagnostics and tests.
    #[must_use]
    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Returns the shared live-work interner.
    #[must_use]
    pub fn interner(&self) -> &WorkInterner {
        &self.interner
    }

    /// Removes elapsed local-fallback deadlines and returns their work keys.
    /// The owner holding each corresponding [`Scheduled`] value should call
    /// [`Scheduled::activate_fallback`] for the returned keys.
    pub fn drain_due_fallbacks(&self, now: u64) -> Vec<crate::WorkKey> {
        let mut deadlines = self
            .deadlines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut due = Vec::new();
        while let Some(key) = deadlines.pop_due(now) {
            due.push(key);
        }
        due
    }

    /// Returns current live and heap counts for deadline diagnostics.
    #[must_use]
    pub fn deadline_queue_lengths(&self) -> (usize, usize) {
        let deadlines = self
            .deadlines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (deadlines.live_len(), deadlines.heap_len())
    }
}

fn aggregate_capacity<const N: usize>(values: [u64; N]) -> u64 {
    // The scheduler constructor has no fallible return. An aggregate beyond
    // the representable range is treated as the maximum policy bound;
    // admission itself still uses checked arithmetic.
    values
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
        .unwrap_or(u64::MAX)
}
