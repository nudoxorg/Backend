//! Read-only boundary observations for the caller's existing native budget.
//! No extra checkpoints, provider calls, cache writes, or semantic fallbacks.
//! Detailed boundaries require subscriber target `compiler.typescript_project=debug`.
//! Admission is INFO; the first observed stop is WARN so release_max_level_warn
//! retains the refusal's allowance/charge snapshot. Enabling phase tracing can
//! perturb elapsed deadlines and log I/O; it is observational, not timing-neutral.
use backend_frontend_typescript::{
    TszExecutionCheckpoint, TszProjectExecutionBudget, TszProjectExecutionEvent,
    TszProjectExecutionStop,
};
use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

// Program admission bounds members at 16,384. Boundary-only observation has
// no per-statement or per-type logging. Disclose truncation rather than silently
// claiming a full trace if nested entrypoints exceed this independent log bound.
const MAX_BOUNDARY_EVENTS: u64 = 262_144;

#[derive(Clone)]
struct ActiveBoundary {
    budget: usize,
    phase: backend_frontend_typescript::TszProjectExecutionPhase,
    file: Option<(usize, String)>,
}

thread_local! {
    // Boundary stacks belong to checker worker threads. They contain no
    // compiler handles or mutable semantic state, and are drained by the
    // existing span return/unwind observations.
    static ACTIVE_BOUNDARIES: RefCell<Vec<ActiveBoundary>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Default)]
struct ObservedStopContext {
    phase: Option<backend_frontend_typescript::TszProjectExecutionPhase>,
    file: Option<(usize, String)>,
}

pub(super) struct ObservedTypeScriptBudget<'cancel> {
    budget: TszProjectExecutionBudget<'cancel>,
    started: Instant,
    initial_work: u64,
    observed_events: AtomicU64,
    stop_observed: AtomicBool,
    stop_context: OnceLock<ObservedStopContext>,
    current_program: AtomicU64,
    admitted_sources: AtomicU64,
    admitted_libraries: AtomicU64,
    admitted_module_requests: AtomicU64,
    started_source_checks: AtomicU64,
    completed_source_checks: AtomicU64,
    reused_source_checks: AtomicU64,
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
            stop_context: OnceLock::new(),
            current_program: AtomicU64::new(u64::MAX),
            admitted_sources: AtomicU64::new(0),
            admitted_libraries: AtomicU64::new(0),
            admitted_module_requests: AtomicU64::new(0),
            started_source_checks: AtomicU64::new(0),
            completed_source_checks: AtomicU64::new(0),
            reused_source_checks: AtomicU64::new(0),
        }
    }

    pub(super) fn admit_program(
        &self,
        index: usize,
        sources: usize,
        libraries: usize,
        requests: usize,
    ) {
        self.current_program.store(index as u64, Ordering::Release);
        self.admitted_sources
            .fetch_add(sources as u64, Ordering::Relaxed);
        self.admitted_libraries
            .fetch_add(libraries as u64, Ordering::Relaxed);
        self.admitted_module_requests
            .fetch_add(requests as u64, Ordering::Relaxed);
    }

    pub(super) fn reused_program(&self, sources: usize) {
        self.reused_source_checks
            .fetch_add(sources as u64, Ordering::Relaxed);
    }

    fn active_context(&self) -> ObservedStopContext {
        let budget = std::ptr::from_ref(self) as usize;
        ACTIVE_BOUNDARIES.with(|active| {
            let active = active.borrow();
            ObservedStopContext {
                phase: active
                    .iter()
                    .rev()
                    .find(|entry| entry.budget == budget)
                    .map(|entry| entry.phase),
                file: active
                    .iter()
                    .rev()
                    .filter(|entry| entry.budget == budget)
                    .find_map(|entry| entry.file.clone()),
            }
        })
    }

    fn observe_context(&self, event: TszProjectExecutionEvent<'_>) {
        use backend_frontend_typescript::{
            TszProjectExecutionPhase as Phase, TszProjectExecutionTransition as Transition,
        };
        let budget = std::ptr::from_ref(self) as usize;
        ACTIVE_BOUNDARIES.with(|active| {
            let mut active = active.borrow_mut();
            match event.transition {
                Transition::Entered => active.push(ActiveBoundary {
                    budget,
                    phase: event.phase,
                    file: event.file.map(|file| (file.index, file.path.to_owned())),
                }),
                Transition::Returned | Transition::Unwound => {
                    if let Some(index) = active.iter().rposition(|entry| {
                        entry.budget == budget
                            && entry.phase == event.phase
                            && entry
                                .file
                                .as_ref()
                                .map(|(index, path)| (*index, path.as_str()))
                                == event.file.map(|file| (file.index, file.path))
                    }) {
                        active.remove(index);
                    }
                }
            }
        });
        match (event.phase, event.transition) {
            (Phase::FileCheck, Transition::Entered) => {
                self.started_source_checks.fetch_add(1, Ordering::Relaxed);
            }
            // FilePostprocess is entered only after the checker returned and
            // the shared stop poll succeeded. A FileCheck return alone is
            // insufficient: it may have converted a stop to a sentinel.
            (Phase::FilePostprocess, Transition::Returned) => {
                self.completed_source_checks.fetch_add(1, Ordering::AcqRel);
            }
            _ => {}
        }
    }
}

impl Drop for ObservedTypeScriptBudget<'_> {
    fn drop(&mut self) {
        let budget = std::ptr::from_ref(self) as usize;
        ACTIVE_BOUNDARIES.with(|active| active.borrow_mut().retain(|entry| entry.budget != budget));
    }
}

impl TszExecutionCheckpoint for ObservedTypeScriptBudget<'_> {
    fn execution_identity(
        &self,
    ) -> Option<backend_frontend_typescript::TszProjectExecutionIdentity> {
        self.budget.execution_identity()
    }

    fn checkpoint(&self, work_units: u64) -> Result<(), TszProjectExecutionStop> {
        // Exactly the same single caller-owned poll/charge as before.
        let result = self.budget.checkpoint(work_units);
        if let Err(stop) = result {
            if !self.stop_observed.swap(true, Ordering::AcqRel) {
                // A rejected charge does not subtract from the remaining
                // allowance. This call can also observe another worker's
                // already-latched stop: this first observed failed call's
                // argument is not necessarily the charge that originally
                // caused exhaustion. Identify that context and retain one
                // remaining/charged snapshot.
                let remaining = self.budget.remaining_work_units();
                let context = self.active_context();
                let _ = self.stop_context.set(context.clone());
                let program = self.current_program.load(Ordering::Acquire);
                let thread = std::thread::current();
                // Sample completions before starts. Each completed boundary
                // publishes its earlier start, so the counters cannot present
                // a completed check that this snapshot has not yet started.
                let completed = self.completed_source_checks.load(Ordering::Acquire);
                let started = self.started_source_checks.load(Ordering::Acquire);
                tracing::warn!(target: "compiler.typescript_project", transition = "stopped",
                    reason = ?stop, project_elapsed_millis = self.started.elapsed().as_millis() as u64,
                    project_initial_work_units = self.initial_work,
                    project_attempt_context = "first_observed_stopped_checkpoint",
                    project_observed_attempted_work_units = work_units,
                    project_charged_work_units = self.initial_work.saturating_sub(remaining),
                    project_remaining_work_units = remaining,
                    observed_program_index = (program != u64::MAX).then_some(program),
                    observed_active_phase = ?context.phase,
                    observed_active_file_index = context.file.as_ref().map(|(index, _)| *index),
                    observed_active_file = context.file.as_ref().map(|(_, path)| path.as_str()),
                    observed_worker = thread.name(),
                    admitted_source_count = self.admitted_sources.load(Ordering::Relaxed),
                    admitted_library_count = self.admitted_libraries.load(Ordering::Relaxed),
                    admitted_module_request_count = self.admitted_module_requests.load(Ordering::Relaxed),
                    started_source_check_count = started,
                    completed_source_check_count = completed,
                    reused_source_check_count = self.reused_source_checks.load(Ordering::Relaxed),
                    "native TypeScript project execution stopped");
            }
        }
        result
    }

    fn observe(&self, event: TszProjectExecutionEvent<'_>) {
        self.observe_context(event);
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

    #[derive(Default)]
    struct StopRecord {
        numeric: BTreeMap<&'static str, u64>,
        text: BTreeMap<&'static str, String>,
    }
    impl std::ops::Deref for StopRecord {
        type Target = BTreeMap<&'static str, u64>;
        fn deref(&self) -> &Self::Target {
            &self.numeric
        }
    }
    struct StopVisitor(StopRecord);

    impl tracing::field::Visit for StopVisitor {
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.numeric.insert(field.name(), value);
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.text.insert(field.name(), value.to_owned());
        }
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.text.insert(field.name(), format!("{value:?}"));
        }
    }

    struct StopSubscriber(Arc<Mutex<Vec<StopRecord>>>);

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
            let mut visitor = StopVisitor(StopRecord::default());
            event.record(&mut visitor);
            self.0.lock().unwrap().push(visitor.0);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn release_level_stop_reports_actual_spend_and_positive_remaining_once() {
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
            assert_eq!(budget.budget.remaining_work_units(), 4);
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
            ("project_charged_work_units", 3),
            ("project_remaining_work_units", 4),
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
        assert_eq!(record.get("project_charged_work_units"), Some(&3));
        assert_eq!(record.get("project_remaining_work_units"), Some(&4));
    }

    #[test]
    fn warn_only_stop_retains_nested_file_context_and_truthful_check_counts() {
        use backend_frontend_typescript::{
            TszProjectExecutionFile, TszProjectExecutionTransition as Transition,
        };
        let records = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(StopSubscriber(Arc::clone(&records)), || {
            let cancelled = AtomicBool::new(false);
            let budget = ObservedTypeScriptBudget::new(
                Instant::now() + Duration::from_secs(30),
                &cancelled,
                7,
            );
            budget.admit_program(3, 4, 2, 8);
            let first = TszProjectExecutionFile {
                index: 0,
                path: "src/completed.ts",
            };
            for (phase, transition) in [
                (TszProjectExecutionPhase::FileCheck, Transition::Entered),
                (TszProjectExecutionPhase::FileCheck, Transition::Returned),
                (
                    TszProjectExecutionPhase::FilePostprocess,
                    Transition::Entered,
                ),
                (
                    TszProjectExecutionPhase::FilePostprocess,
                    Transition::Returned,
                ),
            ] {
                budget.observe(TszProjectExecutionEvent {
                    phase,
                    transition,
                    file: Some(first),
                });
            }
            let active = TszProjectExecutionFile {
                index: 1,
                path: "src/burndown.spec.ts",
            };
            budget.observe(TszProjectExecutionEvent {
                phase: TszProjectExecutionPhase::FileCheck,
                transition: Transition::Entered,
                file: Some(active),
            });
            budget.observe(TszProjectExecutionEvent {
                phase: TszProjectExecutionPhase::SourceStatements,
                transition: Transition::Entered,
                file: None,
            });
            assert_eq!(budget.checkpoint(3), Ok(()));
            assert_eq!(
                budget.checkpoint(5),
                Err(TszProjectExecutionStop::WorkBudgetExhausted)
            );
            let context = budget.stop_context.get().unwrap();
            assert_eq!(
                context.phase,
                Some(TszProjectExecutionPhase::SourceStatements)
            );
            assert_eq!(
                context
                    .file
                    .as_ref()
                    .map(|(index, file)| (*index, file.as_str())),
                Some((1, "src/burndown.spec.ts"))
            );
            budget.observe(TszProjectExecutionEvent {
                phase: TszProjectExecutionPhase::SourceStatements,
                transition: Transition::Unwound,
                file: None,
            });
            budget.observe(TszProjectExecutionEvent {
                phase: TszProjectExecutionPhase::FileCheck,
                transition: Transition::Returned,
                file: Some(active),
            });
            assert!(budget.active_context().phase.is_none());
            assert_eq!(
                budget.completed_source_checks.load(Ordering::Relaxed),
                1,
                "a stopped check's return cannot certify completion"
            );
        });
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(
            record.text.get("observed_active_file").map(String::as_str),
            Some("src/burndown.spec.ts")
        );
        assert_eq!(
            record.text.get("observed_active_phase").map(String::as_str),
            Some("Some(SourceStatements)")
        );
        for (name, expected) in [
            ("observed_program_index", 3),
            ("observed_active_file_index", 1),
            ("admitted_source_count", 4),
            ("admitted_library_count", 2),
            ("admitted_module_request_count", 8),
            ("started_source_check_count", 2),
            ("completed_source_check_count", 1),
            ("reused_source_check_count", 0),
        ] {
            assert_eq!(record.get(name), Some(&expected), "{name}");
        }
    }

    #[test]
    fn file_query_stop_is_observed_without_claiming_source_check_work() {
        use backend_frontend_typescript::{
            TszProjectExecutionFile, TszProjectExecutionTransition as Transition,
        };
        let cancelled = AtomicBool::new(false);
        let budget =
            ObservedTypeScriptBudget::new(Instant::now() + Duration::from_secs(30), &cancelled, 0);
        budget.admit_program(0, 1, 0, 0);
        budget.reused_program(1);
        budget.observe(TszProjectExecutionEvent {
            phase: TszProjectExecutionPhase::FileQuery,
            transition: Transition::Entered,
            file: Some(TszProjectExecutionFile {
                index: 0,
                path: "src/main.ts",
            }),
        });
        assert_eq!(
            budget.checkpoint(1),
            Err(TszProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(
            budget.stop_context.get().unwrap().phase,
            Some(TszProjectExecutionPhase::FileQuery)
        );
        assert_eq!(budget.started_source_checks.load(Ordering::Relaxed), 0);
        assert_eq!(budget.completed_source_checks.load(Ordering::Relaxed), 0);
        assert_eq!(budget.reused_source_checks.load(Ordering::Relaxed), 1);
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
