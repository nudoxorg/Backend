//! Read-only boundary observations for the caller's existing native budget.
//! No extra checkpoints, provider calls, cache writes, or semantic fallbacks.
//! Detailed boundaries require subscriber target `compiler.typescript_project=debug`.
//! Admission is INFO; the first observed stop is WARN so release_max_level_warn
//! retains the refusal's allowance/charge snapshot. Enabling phase tracing can
//! perturb elapsed deadlines and log I/O; it is observational, not timing-neutral.
use backend_frontend_typescript::{
    TszExecutionCheckpoint, TszProjectExecutionBudget, TszProjectExecutionEvent,
    TszProjectExecutionPhase, TszProjectExecutionStop, TszProjectExecutionTransition,
};
use std::cell::RefCell;
use std::sync::{Arc, OnceLock};
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
    stop_site: Arc<OnceLock<ObservedStopSite>>,
}

/// The boundary a worker was inside when the program's first stop was observed.
///
/// This is the file and phase that actually ran out of budget, deadline, or
/// patience; it is unrelated to whichever package source happened to be named
/// first when the compile was requested.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ObservedStopSite {
    /// Why the program stopped.
    pub(super) reason: TszProjectExecutionStop,
    /// Innermost computation boundary of the observing worker, if it was inside one.
    pub(super) phase: Option<TszProjectExecutionPhase>,
    /// Exact program path of that boundary's file, when it names one.
    pub(super) file: Option<Box<str>>,
    /// Work the program had admitted when the stop was observed.
    pub(super) work_charged: u64,
    /// The program's whole work allowance.
    pub(super) work_allowance: u64,
}

thread_local! {
    /// Open computation boundaries of this worker, innermost last. Spans are
    /// lexically scoped, so entries and exits balance even across `?` and panics.
    static OPEN_BOUNDARIES: RefCell<Vec<(TszProjectExecutionPhase, Option<Box<str>>)>> =
        const { RefCell::new(Vec::new()) };
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
            stop_site: Arc::new(OnceLock::new()),
        }
    }

    /// Records the first stop into `sink`, which the caller keeps so the stop site
    /// survives the budget being dropped on an early error return.
    #[must_use]
    pub(super) fn with_stop_sink(mut self, sink: Arc<OnceLock<ObservedStopSite>>) -> Self {
        self.stop_site = sink;
        self
    }

    /// Where the first observed stop happened, once one has.
    pub(super) fn stop_site(&self) -> Option<&ObservedStopSite> {
        self.stop_site.get()
    }
}

impl ObservedTypeScriptBudget<'_> {
    /// The latched program-wide stop, if any. A file's own work slice never latches it.
    pub(super) fn stop_reason(&self) -> Option<TszProjectExecutionStop> {
        self.budget.stop_reason()
    }
}

impl TszExecutionCheckpoint for ObservedTypeScriptBudget<'_> {
    fn execution_identity(&self) -> Option<backend_frontend_typescript::TszProjectExecutionIdentity> {
        self.budget.execution_identity()
    }

    fn checkpoint(&self, work_units: u64) -> Result<(), TszProjectExecutionStop> {
        // Exactly the same single caller-owned poll/charge as before.
        let result = self.budget.checkpoint(work_units);
        if let Err(stop) = result {
            if !self.stop_observed.swap(true, Ordering::AcqRel) {
                // A rejected charge drains the remaining allowance, so charged work
                // is exact. This call can also observe another worker's
                // already-latched stop: this first observed failed call's
                // argument is not necessarily the charge that originally
                // caused exhaustion. Identify that context and retain one
                // remaining/charged snapshot.
                let remaining = self.budget.remaining_work_units();
                let (phase, file) = OPEN_BOUNDARIES.with(|open| {
                    open.borrow()
                        .last()
                        .map_or((None, None), |(phase, file)| (Some(*phase), file.clone()))
                });
                let _ = self.stop_site.set(ObservedStopSite {
                    reason: stop,
                    phase,
                    file: file.clone(),
                    work_charged: self.initial_work.saturating_sub(remaining),
                    work_allowance: self.initial_work,
                });
                tracing::warn!(target: "compiler.typescript_project", transition = "stopped",
                    stop_phase = ?phase, stop_file = file.as_deref(),
                    reason = ?stop, project_elapsed_millis = self.started.elapsed().as_millis() as u64,
                    project_initial_work_units = self.initial_work,
                    project_attempt_context = "first_observed_stopped_checkpoint",
                    project_observed_attempted_work_units = work_units,
                    project_charged_work_units = self.initial_work.saturating_sub(remaining),
                    project_remaining_work_units = remaining,
                    "native TypeScript project execution stopped");
            }
        }
        result
    }

    fn observe(&self, event: TszProjectExecutionEvent<'_>) {
        OPEN_BOUNDARIES.with(|open| {
            let mut open = open.borrow_mut();
            match event.transition {
                TszProjectExecutionTransition::Entered => {
                    open.push((event.phase, event.file.map(|file| Box::from(file.path))));
                }
                TszProjectExecutionTransition::Returned
                | TszProjectExecutionTransition::Unwound => {
                    open.pop();
                }
            }
        });
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
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct StopVisitor(BTreeMap<&'static str, u64>);

    impl tracing::field::Visit for StopVisitor {
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.insert(field.name(), value);
        }

        fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    }

    struct StopSubscriber(Arc<Mutex<Vec<BTreeMap<&'static str, u64>>>>);

    impl tracing::Subscriber for StopSubscriber {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            metadata.target() == "compiler.typescript_project"
                && *metadata.level() == tracing::Level::WARN
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = StopVisitor(BTreeMap::new());
            event.record(&mut visitor);
            self.0.lock().unwrap().push(visitor.0);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn release_level_stop_reports_exact_spend_and_a_drained_allowance_once() {
        let records = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(StopSubscriber(Arc::clone(&records)), || {
            let cancelled = AtomicBool::new(false);
            let budget = ObservedTypeScriptBudget::new(
                Instant::now() + Duration::from_secs(30),
                &cancelled,
                7,
            );
            assert_eq!(budget.checkpoint(3), Ok(()));
            assert_eq!(budget.budget.remaining_work_units(), 4);
            assert_eq!(
                budget.checkpoint(5),
                Err(TszProjectExecutionStop::WorkBudgetExhausted)
            );
            // The rejected charge drains the allowance: nothing is left to overshoot.
            assert_eq!(budget.budget.remaining_work_units(), 0);
            assert_eq!(
                budget.checkpoint(0),
                Err(TszProjectExecutionStop::WorkBudgetExhausted)
            );
        });
        let records = records.lock().unwrap();
        assert_eq!(
            records.len(),
            1,
            "the release-level refusal is retained exactly once"
        );
        let record = &records[0];
        for (name, expected) in [
            ("project_initial_work_units", 7),
            ("project_observed_attempted_work_units", 5),
            ("project_charged_work_units", 7),
            ("project_remaining_work_units", 0),
        ] {
            assert_eq!(record.get(name), Some(&expected), "{name}");
        }
    }

    #[test]
    fn release_level_latched_stop_reports_its_observed_poll_without_inventing_a_charge() {
        let records = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(StopSubscriber(Arc::clone(&records)), || {
            let cancelled = AtomicBool::new(false);
            let budget = ObservedTypeScriptBudget::new(
                Instant::now() + Duration::from_secs(30),
                &cancelled,
                7,
            );
            let latched = std::sync::Barrier::new(2);
            std::thread::scope(|workers| {
                let charging_worker = workers.spawn(|| {
                    let accepted = budget.budget.checkpoint(3);
                    let rejected = budget.budget.checkpoint(5);
                    latched.wait();
                    (accepted, rejected)
                });
                // Another worker has latched exhaustion before the wrapper's
                // first observed failure. This zero-work poll did not reject 5.
                latched.wait();
                let observed_poll = budget.checkpoint(0);
                assert_eq!(
                    charging_worker.join().unwrap(),
                    (Ok(()), Err(TszProjectExecutionStop::WorkBudgetExhausted))
                );
                assert_eq!(
                    observed_poll,
                    Err(TszProjectExecutionStop::WorkBudgetExhausted)
                );
            });
        });
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(
            record.get("project_observed_attempted_work_units"),
            Some(&0)
        );
        assert!(!record.contains_key("project_attempted_work_units"));
        assert_eq!(record.get("project_initial_work_units"), Some(&7));
        assert_eq!(record.get("project_charged_work_units"), Some(&7));
        assert_eq!(record.get("project_remaining_work_units"), Some(&0));
    }

    #[test]
    fn first_stop_names_the_innermost_open_boundary_and_the_exact_spend() {
        use backend_frontend_typescript::TszProjectExecutionFile;
        let cancelled = AtomicBool::new(false);
        let budget = ObservedTypeScriptBudget::new(
            Instant::now() + Duration::from_secs(30),
            &cancelled,
            10,
        );
        let event = |phase, transition, file| TszProjectExecutionEvent {
            phase,
            transition,
            file,
        };
        let file = Some(TszProjectExecutionFile {
            index: 3,
            path: "node_modules/pkg/index.d.ts",
        });
        budget.observe(event(
            TszProjectExecutionPhase::FileCheck,
            TszProjectExecutionTransition::Entered,
            file,
        ));
        budget.observe(event(
            TszProjectExecutionPhase::SourceStatements,
            TszProjectExecutionTransition::Entered,
            file,
        ));
        assert_eq!(budget.stop_site(), None, "no stop, no site");
        assert_eq!(budget.checkpoint(4), Ok(()));
        assert_eq!(
            budget.checkpoint(60),
            Err(TszProjectExecutionStop::WorkBudgetExhausted)
        );
        // Later boundaries and stops never rewrite the first observation.
        budget.observe(event(
            TszProjectExecutionPhase::SourceStatements,
            TszProjectExecutionTransition::Unwound,
            file,
        ));
        budget.observe(event(
            TszProjectExecutionPhase::FileCheck,
            TszProjectExecutionTransition::Unwound,
            file,
        ));
        assert_eq!(
            budget.checkpoint(1),
            Err(TszProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(
            budget.stop_site(),
            Some(&ObservedStopSite {
                reason: TszProjectExecutionStop::WorkBudgetExhausted,
                phase: Some(TszProjectExecutionPhase::SourceStatements),
                file: Some("node_modules/pkg/index.d.ts".into()),
                work_charged: 10,
                work_allowance: 10,
            })
        );
    }

    #[test]
    fn a_stop_outside_every_boundary_reports_no_file_or_phase() {
        let cancelled = AtomicBool::new(false);
        let budget = ObservedTypeScriptBudget::new(
            Instant::now() + Duration::from_secs(30),
            &cancelled,
            2,
        );
        assert_eq!(
            budget.checkpoint(3),
            Err(TszProjectExecutionStop::WorkBudgetExhausted)
        );
        let site = budget.stop_site().expect("stop recorded");
        assert_eq!((site.phase, site.file.as_deref()), (None, None));
    }

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

// TEMP-LOCAL-AID: remove before commit.
impl ObservedTypeScriptBudget<'_> {
    /// Work charged to the program so far.
    pub(super) fn dbg_charged(&self) -> u64 {
        self.initial_work.saturating_sub(self.budget.remaining_work_units())
    }
}
