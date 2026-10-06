//! Cooperative, shared work and stop control for one native TypeScript program.
//!
//! The budget is intentionally borrowed from the caller's cancellation token.
//! Parallel parser/checker workers share one atomic work allowance and latch
//! the first stop reason. A stopped computation must discard every partial
//! parse, bind, check, or query result.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::time::Instant;

/// Borrowed cooperative work checkpoint shared by parser, binder, checker,
/// and lazy semantic queries.
pub trait ExecutionCheckpoint: Send + Sync {
    /// Charge work and report the program's latched terminal reason, if any.
    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop>;
}

/// A program computation stopped before it produced complete semantic output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectExecutionStop {
    /// The caller canceled the native operation.
    Cancelled,
    /// The monotonic deadline elapsed.
    Deadline,
    /// The finite shared work allowance was exhausted.
    WorkBudgetExhausted,
}

/// Shared cancellation, deadline, and work accounting for one TSZ program.
///
/// Clone-free borrowing keeps all Rayon workers on one work counter and one
/// cancellation source. `checkpoint` charges deterministic caller-selected
/// work units and latches the first failure. Once stopped, the owner must not
/// publish results from this program.
pub struct ProjectExecutionBudget<'cancel> {
    deadline: Instant,
    cancelled: &'cancel AtomicBool,
    remaining_work: AtomicU64,
    stop: AtomicU8,
}

impl<'cancel> ProjectExecutionBudget<'cancel> {
    /// Creates one bounded program budget.
    #[must_use]
    pub const fn new(
        deadline: Instant,
        cancelled: &'cancel AtomicBool,
        maximum_work_units: u64,
    ) -> Self {
        Self {
            deadline,
            cancelled,
            remaining_work: AtomicU64::new(maximum_work_units),
            stop: AtomicU8::new(0),
        }
    }

    /// Charges `work_units`, checking caller cancellation and deadline.
    ///
    /// Every stage in a program must share this value. Any error is latched so
    /// concurrent workers observe the same terminal reason and callers cannot
    /// accidentally turn a partial result into a successful result.
    pub fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        if let Some(stop) = self.stop_reason() {
            return Err(stop);
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err(self.latch(ProjectExecutionStop::Cancelled));
        }
        if Instant::now() >= self.deadline {
            return Err(self.latch(ProjectExecutionStop::Deadline));
        }
        if self
            .remaining_work
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(work_units)
            })
            .is_err()
        {
            return Err(self.latch(ProjectExecutionStop::WorkBudgetExhausted));
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err(self.latch(ProjectExecutionStop::Cancelled));
        }
        if Instant::now() >= self.deadline {
            return Err(self.latch(ProjectExecutionStop::Deadline));
        }
        Ok(())
    }

    /// Returns the latched terminal reason, if any.
    #[must_use]
    pub fn stop_reason(&self) -> Option<ProjectExecutionStop> {
        match self.stop.load(Ordering::Acquire) {
            1 => Some(ProjectExecutionStop::Cancelled),
            2 => Some(ProjectExecutionStop::Deadline),
            3 => Some(ProjectExecutionStop::WorkBudgetExhausted),
            _ => None,
        }
    }

    /// Returns the remaining shared work allowance.
    #[must_use]
    pub fn remaining_work_units(&self) -> u64 {
        self.remaining_work.load(Ordering::Acquire)
    }

    fn latch(&self, reason: ProjectExecutionStop) -> ProjectExecutionStop {
        let value = match reason {
            ProjectExecutionStop::Cancelled => 1,
            ProjectExecutionStop::Deadline => 2,
            ProjectExecutionStop::WorkBudgetExhausted => 3,
        };
        let _ = self
            .stop
            .compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire);
        self.stop_reason().unwrap_or(reason)
    }
}

impl ExecutionCheckpoint for ProjectExecutionBudget<'_> {
    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        ProjectExecutionBudget::checkpoint(self, work_units)
    }
}

#[cfg(test)]
mod tests {
    use super::{ProjectExecutionBudget, ProjectExecutionStop};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    #[test]
    fn work_is_shared_and_exhaustion_is_latched() {
        let cancelled = AtomicBool::new(false);
        let budget = ProjectExecutionBudget::new(
            Instant::now() + Duration::from_secs(2),
            &cancelled,
            3,
        );
        assert_eq!(budget.checkpoint(2), Ok(()));
        assert_eq!(budget.remaining_work_units(), 1);
        assert_eq!(
            budget.checkpoint(2),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(
            budget.stop_reason(),
            Some(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(budget.checkpoint(0), Err(ProjectExecutionStop::WorkBudgetExhausted));
    }

    #[test]
    fn cancellation_and_deadline_are_typed() {
        let cancelled = AtomicBool::new(true);
        let canceled_budget = ProjectExecutionBudget::new(
            Instant::now() + Duration::from_secs(2),
            &cancelled,
            10,
        );
        assert_eq!(
            canceled_budget.checkpoint(0),
            Err(ProjectExecutionStop::Cancelled)
        );

        let not_cancelled = AtomicBool::new(false);
        let deadline_budget = ProjectExecutionBudget::new(
            Instant::now() - Duration::from_millis(1),
            &not_cancelled,
            10,
        );
        assert_eq!(
            deadline_budget.checkpoint(0),
            Err(ProjectExecutionStop::Deadline)
        );
    }
}
