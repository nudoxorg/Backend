//! Read-only boundary observations for the caller's existing native budget.
//! No extra checkpoints, provider calls, cache writes, or semantic fallbacks.
//! Detailed boundaries require subscriber target `compiler.typescript_project=debug`.
//! Admission and the first observed stop remain INFO. Enabling phase tracing can
//! perturb elapsed deadlines and log I/O; it is observational, not timing-neutral.
use backend_frontend_typescript::{
    TszExecutionCheckpoint, TszProjectExecutionBudget, TszProjectExecutionEvent,
    TszProjectExecutionStop,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

// Program admission bounds members at 16,384. Boundary-only observation has
// no per-statement or per-type logging. Disclose truncation rather than silently
// claiming a full trace if nested entrypoints exceed this independent log bound.
const MAX_BOUNDARY_EVENTS: u64 = 262_144;

pub(super) struct ObservedTypeScriptBudget<'cancel> {
    budget: TszProjectExecutionBudget<'cancel>,
    started: Instant,
    initial_work: u64,
    observed_events: AtomicU64,
    stop_observed: AtomicBool,
}

impl<'cancel> ObservedTypeScriptBudget<'cancel> {
    pub(super) fn new(
        deadline: Instant,
        cancelled: &'cancel AtomicBool,
        maximum_work: u64,
    ) -> Self {
        Self {
            budget: TszProjectExecutionBudget::new(deadline, cancelled, maximum_work),
            started: Instant::now(),
            initial_work: maximum_work,
            observed_events: AtomicU64::new(0),
            stop_observed: AtomicBool::new(false),
        }
    }
}

impl TszExecutionCheckpoint for ObservedTypeScriptBudget<'_> {
    fn checkpoint(&self, work_units: u64) -> Result<(), TszProjectExecutionStop> {
        // Exactly the same single caller-owned poll/charge as before.
        let result = self.budget.checkpoint(work_units);
        if let Err(stop) = result {
            if !self.stop_observed.swap(true, Ordering::AcqRel) {
                // A rejected charge does not subtract from the remaining
                // allowance. Keep its requested size and one counter snapshot
                // so exhaustion cannot be mistaken for spending the full cap.
                let remaining = self.budget.remaining_work_units();
                tracing::info!(target: "compiler.typescript_project", transition = "stopped",
                    reason = ?stop, project_elapsed_millis = self.started.elapsed().as_millis() as u64,
                    project_initial_work_units = self.initial_work,
                    project_attempted_work_units = work_units,
                    project_charged_work_units = self.initial_work.saturating_sub(remaining),
                    project_remaining_work_units = remaining,
                    "native TypeScript project execution stopped");
            }
        }
        result
    }

    fn observe(&self, event: TszProjectExecutionEvent<'_>) {
        if !tracing::enabled!(target: "compiler.typescript_project", tracing::Level::DEBUG) {
            return;
        }
        let sequence = self.observed_events.fetch_add(1, Ordering::Relaxed);
        if sequence >= MAX_BOUNDARY_EVENTS {
            if sequence == MAX_BOUNDARY_EVENTS {
                tracing::debug!(target: "compiler.typescript_project", transition = "trace_truncated",
                    maximum_events = MAX_BOUNDARY_EVENTS,
                    "native TypeScript project boundary trace truncated");
            }
            return;
        }
        let remaining = self.budget.remaining_work_units();
        let thread = std::thread::current();
        tracing::debug!(target: "compiler.typescript_project",
            sequence, phase = ?event.phase, transition = ?event.transition,
            file_index = event.file.map(|file| file.index),
            file = event.file.map(|file| file.path), worker = thread.name(),
            project_elapsed_millis = self.started.elapsed().as_millis() as u64,
            project_initial_work_units = self.initial_work,
            project_charged_work_units = self.initial_work.saturating_sub(remaining),
            project_remaining_work_units = remaining, latched_stop = ?self.budget.stop_reason(),
            "native TypeScript project boundary");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_frontend_typescript::TszProjectExecutionPhase;
    use std::time::Duration;

    #[test]
    fn observations_do_not_poll_or_spend_the_semantic_budget() {
        let cancelled = AtomicBool::new(false);
        let budget =
            ObservedTypeScriptBudget::new(Instant::now() - Duration::from_secs(1), &cancelled, 7);
        budget.observe(TszProjectExecutionEvent {
            phase: TszProjectExecutionPhase::CheckerPlan,
            transition: backend_frontend_typescript::TszProjectExecutionTransition::Entered,
            file: None,
        });
        assert_eq!(budget.budget.remaining_work_units(), 7);
        assert_eq!(budget.budget.stop_reason(), None);
        assert_eq!(budget.checkpoint(0), Err(TszProjectExecutionStop::Deadline));
    }
}
