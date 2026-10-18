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
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// Program admission bounds members at 16,384. Boundary-only observation has
// no per-statement or per-type logging. Disclose truncation rather than silently
// claiming a full trace if nested entrypoints exceed this independent log bound.
const MAX_BOUNDARY_EVENTS: u64 = 262_144;

#[derive(Clone)]
struct ActiveBoundary {
    budget: usize,
    phase: backend_frontend_typescript::TszProjectExecutionPhase,
    file: Option<(usize, String)>,
    started: Option<Instant>,
    preparation: Option<PreparationTiming>,
}

thread_local! {
    // Boundary stacks belong to checker worker threads. They contain no
    // compiler handles or mutable semantic state, and are drained by the
    // existing span return/unwind observations.
    static ACTIVE_BOUNDARIES: RefCell<Vec<ActiveBoundary>> = const { RefCell::new(Vec::new()) };
}

// Seven fixed slots on the observing worker, never a global timing map.
// None means the subpass was not observed; an active subpass is a partial time.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct PreparationTiming {
    pub(super) durations: [Option<Duration>; 7],
    pub(super) active: Option<usize>,
}

/// Cumulative observed wall intervals for this budget, not CPU time. Slot zero
/// encloses the four disjoint projector passes; never sum it with those passes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct QueryTiming {
    pub(super) durations: [Option<Duration>; 5],
    pub(super) active: Option<usize>,
}

fn query_index(phase: backend_frontend_typescript::TszProjectExecutionPhase) -> Option<usize> {
    use backend_frontend_typescript::TszProjectExecutionPhase as P;
    match phase {
        P::FileQuery => Some(0),
        P::FileQueryAnalysis => Some(1),
        P::FileQueryStructure => Some(2),
        P::FileQueryOccurrences => Some(3),
        P::FileQueryTypes => Some(4),
        _ => None,
    }
}

fn preparation_index(phase: backend_frontend_typescript::TszProjectExecutionPhase) -> Option<usize> {
    use backend_frontend_typescript::TszProjectExecutionPhase as P;
    match phase {
        P::SourcePreparationFunctionDefs => Some(0),
        P::SourcePreparationWarmDefinitions => Some(1),
        P::SourcePreparationHeritage => Some(2),
        P::SourcePreparationWellKnownSymbols => Some(3),
        P::SourcePreparationTypeEnvironment => Some(4),
        P::SourcePreparationReconciliation => Some(5),
        P::SourcePreparationBoxedTypes => Some(6),
        _ => None,
    }
}

/// Boundary-only observations from the first failed checkpoint, not a hotspot
/// ranking. Counters belong to this budget; preparation times to this worker's
/// currently active source. Other threads' timings are deliberately absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StopMeasurement {
    pub(super) elapsed: Duration,
    pub(super) started: u64,
    pub(super) completed: u64,
    pub(super) merges: u64,
    pub(super) preparation: Option<PreparationTiming>,
    pub(super) query: Option<QueryTiming>,
}

impl StopMeasurement {
    pub(super) fn cause(&self) -> Option<backend_library::interface::TypeScriptFailureCause> {
        if let Some(query) = self.query.as_ref().filter(|_| self.preparation.is_none()) {
            let times: Vec<_> = query.durations.iter().map(|duration|
                duration.map_or_else(|| "-".to_owned(), |value| format!("{:.2e}", value.as_secs_f64()))
            ).collect();
            let active = query.active.and_then(|index| ["all", "syntax", "struct", "occur", "type"].get(index).copied()).unwrap_or("-");
            return backend_library::interface::TypeScriptFailureCause::new(&format!(
                "First stop; query-sum~s[all,syntax,struct,occur,type]={}; checks done/start={}/{}; merges={}; age~s={:.2e}; active={}; -=unknown",
                times.join(","), self.completed, self.started, self.merges, self.elapsed.as_secs_f64(), active,
            ));
        }
        let times: Vec<_> = (0..7).map(|index| {
            self.preparation.as_ref().and_then(|timing| timing.durations[index])
                .map_or_else(|| "-".to_owned(), |duration| format!("{:.2e}", duration.as_secs_f64()))
        }).collect();
        let active = self.preparation.as_ref().and_then(|timing| timing.active)
            .and_then(|index| ["func", "warm", "herit", "sym", "type", "env", "box"].get(index).copied())
            .unwrap_or("-");
        backend_library::interface::TypeScriptFailureCause::new(&format!(
            "First stop; prep~s[func,warm,herit,sym,type,env,box]={}; checks done/start={}/{}; merges={}; age~s={:.2e}; active={}; -=unknown",
            times.join(","), self.completed, self.started, self.merges,
            self.elapsed.as_secs_f64(), active,
        ))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ObservedTypeScriptProgram {
    #[default]
    NoProgram,
    Program(usize),
}

impl ObservedTypeScriptProgram {
    fn from_encoded(value: usize) -> Self {
        if value == usize::MAX {
            Self::NoProgram
        } else {
            Self::Program(value)
        }
    }

    fn encoded(self) -> usize {
        match self {
            Self::NoProgram => usize::MAX,
            Self::Program(index) => index,
        }
    }

    fn index(self) -> Option<usize> {
        match self {
            Self::NoProgram => None,
            Self::Program(index) => Some(index),
        }
    }
}

/// Borrowed observation frame for one serial forest operation. The field is
/// private; it cannot provide compiler authority or alter admission counts.
pub(super) struct TypeScriptProgramObservation<'budget, 'cancel> {
    frame: ProgramObservationFrame<'budget, 'cancel>,
}

struct ProgramObservationFrame<'budget, 'cancel> {
    budget: &'budget ObservedTypeScriptBudget<'cancel>,
    previous: ObservedTypeScriptProgram,
    // The coordinator that selects the program also restores its context.
    // This marker owns no allocation or mutable semantic state.
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for TypeScriptProgramObservation<'_, '_> {
    fn drop(&mut self) {
        self.frame
            .budget
            .current_program
            .store(self.frame.previous.encoded(), Ordering::Release);
    }
}

#[derive(Clone, Default)]
struct ObservedStopContext {
    program: ObservedTypeScriptProgram,
    phase: Option<backend_frontend_typescript::TszProjectExecutionPhase>,
    file: Option<(usize, String)>,
    preparation: Option<PreparationTiming>,
    query: Option<QueryTiming>,
}

pub(super) struct ObservedTypeScriptBudget<'cancel> {
    budget: TszProjectExecutionBudget<'cancel>,
    started: Instant,
    initial_work: u64,
    observed_events: AtomicU64,
    stop_observed: AtomicBool,
    stop_context: OnceLock<ObservedStopContext>,
    stop_site: Arc<OnceLock<ObservedStopSite>>,
    current_program: AtomicUsize,
    admitted_sources: AtomicU64,
    admitted_libraries: AtomicU64,
    admitted_module_requests: AtomicU64,
    started_source_checks: AtomicU64,
    completed_source_checks: AtomicU64,
    reused_source_checks: AtomicU64,
    merge_enters: AtomicU64,
    // At most five short additions per projected file, never per symbol/type.
    // Separate budgets cannot contaminate one another. Poison recovery affects
    // observations only and must not introduce a new compiler refusal.
    query_totals: std::sync::Mutex<[Option<Duration>; 5]>,
}

/// First observed stopped computation under this exact program budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ObservedStopSite {
    pub(super) reason: TszProjectExecutionStop,
    pub(super) phase: Option<backend_frontend_typescript::TszProjectExecutionPhase>,
    pub(super) file: Option<Box<str>>,
    pub(super) work_charged: u64,
    pub(super) work_allowance: u64,
    pub(super) measurement: Option<StopMeasurement>,
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
            stop_site: Arc::new(OnceLock::new()),
            current_program: AtomicUsize::new(usize::MAX),
            admitted_sources: AtomicU64::new(0),
            admitted_libraries: AtomicU64::new(0),
            admitted_module_requests: AtomicU64::new(0),
            started_source_checks: AtomicU64::new(0),
            completed_source_checks: AtomicU64::new(0),
            reused_source_checks: AtomicU64::new(0),
            merge_enters: AtomicU64::new(0),
            query_totals: std::sync::Mutex::new([None; 5]),
        }
    }

    pub(super) fn with_stop_sink(mut self, sink: Arc<OnceLock<ObservedStopSite>>) -> Self {
        self.stop_site = sink;
        self
    }

    pub(super) fn stop_reason(&self) -> Option<TszProjectExecutionStop> {
        self.budget.stop_reason()
    }

    pub(super) fn admit_program(&self, sources: usize, libraries: usize, requests: usize) {
        self.admitted_sources
            .fetch_add(sources as u64, Ordering::Relaxed);
        self.admitted_libraries
            .fetch_add(libraries as u64, Ordering::Relaxed);
        self.admitted_module_requests
            .fetch_add(requests as u64, Ordering::Relaxed);
    }

    pub(super) fn observe_program(
        &self,
        index: usize,
    ) -> TypeScriptProgramObservation<'_, 'cancel> {
        let previous = ObservedTypeScriptProgram::from_encoded(
            self.current_program.swap(index, Ordering::AcqRel),
        );
        TypeScriptProgramObservation {
            frame: ProgramObservationFrame {
                budget: self,
                previous,
                _not_send: std::marker::PhantomData,
            },
        }
    }

    fn observed_program(&self) -> ObservedTypeScriptProgram {
        ObservedTypeScriptProgram::from_encoded(self.current_program.load(Ordering::Acquire))
    }

    pub(super) fn reused_program(&self, sources: usize) {
        self.reused_source_checks
            .fetch_add(sources as u64, Ordering::Relaxed);
    }

    fn active_context(&self) -> ObservedStopContext {
        let budget = std::ptr::from_ref(self) as usize;
        let totals = *self.query_totals.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        ACTIVE_BOUNDARIES.with(|active| {
            let active = active.borrow();
            let query = active.iter().any(|entry| entry.budget == budget
                && entry.phase == backend_frontend_typescript::TszProjectExecutionPhase::FileQuery).then(|| {
                let mut timing = QueryTiming { durations: totals, active: None };
                let mut seen = [false; 5];
                for entry in active.iter().filter(|entry| entry.budget == budget) {
                    if let (Some(slot), Some(started)) = (query_index(entry.phase), entry.started) {
                        // A recursive query's enclosing time is already included
                        // in its outer interval. Add each active slot only once.
                        if !seen[slot] {
                            seen[slot] = true;
                            timing.durations[slot] = Some(timing.durations[slot].unwrap_or_default().saturating_add(started.elapsed()));
                            timing.active = Some(slot);
                        }
                    }
                }
                timing
            });
            let preparation = active.iter().rposition(|entry| entry.budget == budget
                && entry.phase == backend_frontend_typescript::TszProjectExecutionPhase::SourcePreparation)
                .and_then(|parent| {
                    let mut timing = active[parent].preparation.clone()?;
                    for entry in &active[parent + 1..] {
                        if entry.budget == budget {
                            if let (Some(index), Some(started)) = (preparation_index(entry.phase), entry.started) {
                                timing.durations[index] = Some(timing.durations[index].unwrap_or_default().saturating_add(started.elapsed()));
                                timing.active = Some(index);
                            }
                        }
                    }
                    Some(timing)
                });
            ObservedStopContext {
                program: self.observed_program(),
                preparation,
                query,
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
                    started: (preparation_index(event.phase).is_some() || query_index(event.phase).is_some()).then(Instant::now),
                    preparation: (event.phase == Phase::SourcePreparation).then(PreparationTiming::default),
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
                        let returned = active.remove(index);
                        if let (Some(slot), Some(started)) = (query_index(returned.phase), returned.started) {
                            let nested_same_slot = active.iter().any(|entry| entry.budget == budget
                                && query_index(entry.phase) == Some(slot));
                            if !nested_same_slot {
                                let mut totals = self.query_totals.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                totals[slot] = Some(totals[slot].unwrap_or_default().saturating_add(started.elapsed()));
                            }
                        }
                        if let (Some(slot), Some(started)) = (preparation_index(returned.phase), returned.started) {
                            if let Some(parent) = active.iter_mut().rev().find(|entry| entry.budget == budget
                                && entry.phase == Phase::SourcePreparation) {
                                if let Some(timing) = parent.preparation.as_mut() {
                                    timing.durations[slot] = Some(timing.durations[slot].unwrap_or_default().saturating_add(started.elapsed()));
                                }
                            }
                        }
                    }
                }
            }
        });
        match (event.phase, event.transition) {
            (Phase::Merge, Transition::Entered) => {
                // One counter increment per whole-program merge, not per type/query.
                self.merge_enters.fetch_add(1, Ordering::Relaxed);
            }
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
                // A rejected charge drains the remaining allowance exactly.
                // This call can also observe another worker's
                // already-latched stop: this first observed failed call's
                // argument is not necessarily the charge that originally
                // caused exhaustion. Identify that context and retain one
                // remaining/charged snapshot.
                let remaining = self.budget.remaining_work_units();
                let context = self.active_context();
                let completed = self.completed_source_checks.load(Ordering::Acquire);
                let started = self.started_source_checks.load(Ordering::Acquire);
                let _ = self.stop_site.set(ObservedStopSite {
                    reason: stop,
                    phase: context.phase,
                    file: context.file.as_ref().map(|(_, path)| path.as_str().into()),
                    work_charged: self.initial_work.saturating_sub(remaining),
                    work_allowance: self.initial_work,
                    measurement: Some(StopMeasurement {
                        elapsed: self.started.elapsed(),
                        started, completed,
                        merges: self.merge_enters.load(Ordering::Relaxed),
                        preparation: context.preparation.clone(),
                        query: context.query.clone(),
                    }),
                });
                let _ = self.stop_context.set(context.clone());
                let thread = std::thread::current();
                // Sample completions before starts. Each completed boundary
                // publishes its earlier start, so the counters cannot present
                // a completed check that this snapshot has not yet started.
                tracing::warn!(target: "compiler.typescript_project", transition = "stopped",
                    reason = ?stop, project_elapsed_millis = self.started.elapsed().as_millis() as u64,
                    project_initial_work_units = self.initial_work,
                    project_attempt_context = "first_observed_stopped_checkpoint",
                    project_observed_attempted_work_units = work_units,
                    project_charged_work_units = self.initial_work.saturating_sub(remaining),
                    project_remaining_work_units = remaining,
                    observed_program_index = context.program.index(),
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
    fn release_level_stop_reports_exact_drained_allowance_once() {
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
            assert_eq!(budget.budget.remaining_work_units(), 0);
            assert_eq!(
                budget.checkpoint(0),
                Err(TszProjectExecutionStop::WorkBudgetExhausted)
            );
            assert_eq!(
                budget.stop_context.get().unwrap().program,
                ObservedTypeScriptProgram::NoProgram
            );
        });
        let records = records.lock().unwrap();
        assert_eq!(
            records.len(),
            1,
            "the release-level refusal is retained exactly once"
        );
        let record = &records[0];
        assert!(
            !record.contains_key("observed_program_index"),
            "an unselected stop cannot invent a program"
        );
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
            budget.admit_program(4, 2, 8);
            let _program_observation = budget.observe_program(3);
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
        budget.admit_program(1, 0, 0);
        let _program_observation = budget.observe_program(0);
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
    fn nonlast_checked_program_query_stop_preserves_program_file_and_admission_counts() {
        use backend_frontend_typescript::{
            TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput, TszProjectAuthority,
            TszProjectCheckerExecution, TszProjectOptions, TszProjectQuerySessionError,
            TszProjectSemanticOptions,
        };
        let records = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(StopSubscriber(Arc::clone(&records)), || {
            let cancelled = AtomicBool::new(false);
            let budget = ObservedTypeScriptBudget::new(
                Instant::now() + Duration::from_secs(30),
                &cancelled,
                1_000_000,
            );
            let mut programs = [TszProjectAuthority::new(), TszProjectAuthority::new()];
            for (index, (program, path)) in programs
                .iter_mut()
                .zip(["src/first.ts", "src/last.ts"])
                .enumerate()
            {
                budget.admit_program(1, 0, 0);
                let _program_observation = budget.observe_program(index);
                program
                    .update_with_checker_execution(
                        vec![TszFileInput {
                            path: path.into(),
                            source: "export const value: number = 1;".into(),
                        }],
                        TszProjectOptions {
                            checker: TszCheckerOptions {
                                no_lib: true,
                                ..TszCheckerOptions::default()
                            },
                            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
                            program_diagnostics: Vec::new(),
                            module_resolutions: Vec::new(),
                            file_formats: Vec::new(),
                            environment: TszEnvironmentFingerprint::from_sha256(if index == 0 {
                                [0x41; 32]
                            } else {
                                [0x42; 32]
                            }),
                        },
                        &[],
                        TszProjectCheckerExecution {
                            checkpoint: &budget,
                            workers: std::num::NonZeroUsize::MIN,
                        },
                    )
                    .unwrap();
                assert_eq!(
                    budget.observed_program(),
                    ObservedTypeScriptProgram::Program(index)
                );
            }
            assert_eq!(
                budget.observed_program(),
                ObservedTypeScriptProgram::NoProgram
            );
            assert_eq!(budget.admitted_sources.load(Ordering::Relaxed), 2);
            assert_eq!(budget.started_source_checks.load(Ordering::Relaxed), 2);
            assert_eq!(budget.completed_source_checks.load(Ordering::Relaxed), 2);
            {
                // Both native programs completed checking. This is an actual
                // checked-image query on the earlier node, not a synthetic
                // import witness or a repeated program admission.
                let _program_observation = budget.observe_program(0);
                let project = programs[0].project().unwrap();
                let session = project.checked_query_session(&budget).unwrap();
                let file_index = session.file_index("src/first.ts").unwrap();
                let callback_entered = AtomicBool::new(false);
                cancelled.store(true, Ordering::Release);
                let stopped = session.with_file_checker_and_types(file_index, |_, _, _, _| {
                    callback_entered.store(true, Ordering::Release);
                });
                assert_eq!(
                    stopped,
                    Err(TszProjectQuerySessionError::ExecutionStopped(
                        TszProjectExecutionStop::Cancelled
                    ))
                );
                assert!(!callback_entered.load(Ordering::Acquire));
                let context = budget.stop_context.get().unwrap();
                assert_eq!(context.program, ObservedTypeScriptProgram::Program(0));
                assert_eq!(context.phase, Some(TszProjectExecutionPhase::FileQuery));
                assert_eq!(
                    context
                        .file
                        .as_ref()
                        .map(|(index, path)| (*index, path.as_str())),
                    Some((file_index, "src/first.ts"))
                );
            }
            assert_eq!(
                budget.observed_program(),
                ObservedTypeScriptProgram::NoProgram
            );
            assert_eq!(budget.admitted_sources.load(Ordering::Relaxed), 2);
            assert_eq!(budget.admitted_libraries.load(Ordering::Relaxed), 0);
            assert_eq!(budget.admitted_module_requests.load(Ordering::Relaxed), 0);
            assert_eq!(budget.completed_source_checks.load(Ordering::Relaxed), 2);
        });
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.get("observed_program_index"), Some(&0));
        assert_eq!(record.get("admitted_source_count"), Some(&2));
        assert_eq!(record.get("started_source_check_count"), Some(&2));
        assert_eq!(record.get("completed_source_check_count"), Some(&2));
        assert_eq!(
            record.text.get("observed_active_file").map(String::as_str),
            Some("src/first.ts")
        );
        assert_eq!(
            record.text.get("observed_active_phase").map(String::as_str),
            Some("Some(FileQuery)")
        );
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
    #[test]
    fn preparation_stop_retains_current_worker_times_counts_and_unobserved_slots() {
        use backend_frontend_typescript::{TszProjectExecutionFile, TszProjectExecutionPhase as P,
            TszProjectExecutionTransition as T};
        let cancelled = AtomicBool::new(false);
        let budget = ObservedTypeScriptBudget::new(Instant::now() + Duration::from_secs(30), &cancelled, 100);
        let event = |phase, transition, file| TszProjectExecutionEvent { phase, transition, file };
        let file = Some(TszProjectExecutionFile { index: 2, path: "src/actual.ts" });
        budget.observe(event(P::Merge, T::Entered, None));
        budget.observe(event(P::Merge, T::Returned, None));
        budget.observe(event(P::FileCheck, T::Entered, file));
        budget.observe(event(P::SourcePreparation, T::Entered, file));
        budget.observe(event(P::SourcePreparationFunctionDefs, T::Entered, None));
        budget.observe(event(P::SourcePreparationFunctionDefs, T::Returned, None));
        budget.observe(event(P::SourcePreparationTypeEnvironment, T::Entered, None));
        let other_cancelled = AtomicBool::new(false);
        let other = ObservedTypeScriptBudget::new(Instant::now() + Duration::from_secs(30), &other_cancelled, 100);
        other.observe(event(P::SourcePreparation, T::Entered,
            Some(TszProjectExecutionFile { index: 9, path: "src/unrelated.ts" })));
        other.observe(event(P::SourcePreparationBoxedTypes, T::Entered, None));
        // A deterministic clock origin proves that an active subpass contributes
        // partial elapsed time; no sleeping or elapsed-time performance assertion.
        let identity = std::ptr::from_ref(&budget) as usize;
        ACTIVE_BOUNDARIES.with(|active| {
            let mut active = active.borrow_mut();
            let frame = active.iter_mut().rev().find(|frame| frame.budget == identity
                && frame.phase == P::SourcePreparationTypeEnvironment).unwrap();
            frame.started = Instant::now().checked_sub(Duration::from_secs(3));
        });
        assert_eq!(budget.budget.remaining_work_units(), 100, "observation never charges work");
        assert_eq!(budget.budget.stop_reason(), None, "observation never polls the deadline");
        cancelled.store(true, Ordering::Release);
        assert_eq!(budget.checkpoint(0), Err(TszProjectExecutionStop::Cancelled));
        let stop = budget.stop_site.get().unwrap();
        assert_eq!(stop.phase, Some(P::SourcePreparationTypeEnvironment));
        assert_eq!(stop.file.as_deref(), Some("src/actual.ts"));
        let observed = stop.measurement.as_ref().unwrap();
        assert_eq!((observed.started, observed.completed, observed.merges), (1, 0, 1));
        let times = observed.preparation.as_ref().unwrap();
        assert_eq!(times.active, Some(4));
        assert!(times.durations[0].is_some());
        assert!(times.durations[4].unwrap() >= Duration::from_secs(3));
        for index in [1, 2, 3, 5, 6] { assert_eq!(times.durations[index], None); }
        assert!(observed.cause().unwrap().as_str().contains("active=type"));
        // Returning/unwinding these lexical observations cannot replace the
        // original failure or its immutable timing snapshot.
        budget.observe(event(P::SourcePreparationTypeEnvironment, T::Unwound, None));
        budget.observe(event(P::SourcePreparation, T::Unwound, file));
        budget.observe(event(P::FileCheck, T::Unwound, file));
        assert_eq!(budget.stop_site.get(), Some(stop));
    }

    #[test]
    fn preparation_cause_keeps_maximum_counts_and_all_seven_times_in_existing_bound() {
        let observed = StopMeasurement {
            elapsed: Duration::MAX, started: u64::MAX, completed: u64::MAX, merges: u64::MAX,
            preparation: Some(PreparationTiming { durations: [Some(Duration::MAX); 7], active: Some(6) }),
            query: None,
        };
        let cause = observed.cause().expect("bounded fixed labels and approximate elapsed values");
        assert!(cause.as_str().len() <= backend_library::interface::TypeScriptFailureCause::MAX_BYTES);
        assert_eq!(cause.as_str().matches(u64::MAX.to_string().as_str()).count(), 3,
            "source-check and merge counters are exact, never truncated");
        assert!(cause.as_str().contains("active=box"));
        assert!(!cause.as_str().contains("..."));
        let nested = StopMeasurement { query: Some(QueryTiming::default()), ..observed.clone() }.cause().unwrap();
        assert_eq!(nested, cause, "a preparation active inside a query retains its original slots");
        let unknown = StopMeasurement { preparation: None, ..observed }.cause().unwrap();
        assert!(unknown.as_str().contains("=-,-,-,-,-,-,-;"), "missing observation is not fabricated zero time");
    }

    #[test]
    fn query_stop_keeps_cumulative_and_active_intervals_without_cross_budget_or_nested_double_count() {
        use backend_frontend_typescript::{TszProjectExecutionFile, TszProjectExecutionPhase as P,
            TszProjectExecutionTransition as T};
        let cancelled = AtomicBool::new(false);
        let budget = ObservedTypeScriptBudget::new(Instant::now() + Duration::from_secs(30), &cancelled, 100);
        let event = |phase, transition, file| TszProjectExecutionEvent { phase, transition, file };
        let first = Some(TszProjectExecutionFile { index: 0, path: "src/earlier.ts" });
        let last = Some(TszProjectExecutionFile { index: 1, path: "src/current.ts" });
        budget.observe(event(P::FileQuery, T::Entered, first));
        budget.observe(event(P::FileQueryAnalysis, T::Entered, first));
        budget.observe(event(P::FileQueryAnalysis, T::Returned, first));
        budget.observe(event(P::FileQuery, T::Returned, first));
        assert!(budget.query_totals.lock().unwrap()[0].is_some());
        assert!(budget.query_totals.lock().unwrap()[1].is_some());
        budget.observe(event(P::FileQuery, T::Entered, last));
        budget.observe(event(P::FileQueryTypes, T::Entered, last));
        let other_cancelled = AtomicBool::new(false);
        let other = ObservedTypeScriptBudget::new(Instant::now() + Duration::from_secs(30), &other_cancelled, 100);
        other.observe(event(P::FileQuery, T::Entered, first));
        other.observe(event(P::FileQueryOccurrences, T::Entered, first));
        assert_eq!(budget.budget.remaining_work_units(), 100);
        assert_eq!(budget.budget.stop_reason(), None);
        cancelled.store(true, Ordering::Release);
        assert_eq!(budget.checkpoint(0), Err(TszProjectExecutionStop::Cancelled));
        let frozen = budget.stop_site.get().unwrap().clone();
        assert_eq!(frozen.file.as_deref(), Some("src/current.ts"));
        let timing = frozen.measurement.as_ref().unwrap().query.as_ref().unwrap();
        assert_eq!(timing.active, Some(4));
        for slot in [0, 1, 4] { assert!(timing.durations[slot].is_some()); }
        for slot in [2, 3] { assert_eq!(timing.durations[slot], None); }
        let cause = frozen.measurement.as_ref().unwrap().cause().unwrap();
        assert!(cause.as_str().contains("query-sum~s"));
        assert!(!cause.as_str().contains("prep~s"));
        assert!(cause.as_str().contains("active=type"));
        budget.observe(event(P::FileQueryTypes, T::Unwound, last));
        budget.observe(event(P::FileQuery, T::Unwound, last));
        // Later workers/unwinds cannot replace the first stop's owned snapshot.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                budget.observe(event(P::FileQuery, T::Entered, first));
                budget.observe(event(P::FileQueryOccurrences, T::Entered, first));
                assert_eq!(budget.checkpoint(0), Err(TszProjectExecutionStop::Cancelled));
                budget.observe(event(P::FileQueryOccurrences, T::Unwound, first));
                budget.observe(event(P::FileQuery, T::Unwound, first));
            }).join().unwrap();
        });
        assert_eq!(budget.stop_site.get(), Some(&frozen));

        let nested_cancelled = AtomicBool::new(false);
        let nested = ObservedTypeScriptBudget::new(Instant::now() + Duration::from_secs(30), &nested_cancelled, 100);
        nested.observe(event(P::FileQuery, T::Entered, first));
        nested.observe(event(P::FileQuery, T::Entered, last));
        nested.observe(event(P::FileQuery, T::Returned, last));
        assert_eq!(nested.query_totals.lock().unwrap()[0], None,
            "the inner interval is already enclosed by the outer query");
        nested.observe(event(P::FileQuery, T::Returned, first));
        assert!(nested.query_totals.lock().unwrap()[0].is_some());
    }

    #[test]
    fn query_cause_keeps_maximum_counts_and_unknown_slots_in_existing_bound() {
        let observed = StopMeasurement {
            elapsed: Duration::MAX, started: u64::MAX, completed: u64::MAX, merges: u64::MAX,
            preparation: None,
            query: Some(QueryTiming { durations: [Some(Duration::MAX); 5], active: Some(2) }),
        };
        let cause = observed.cause().expect("fixed query labels fit existing cause");
        assert!(cause.as_str().len() <= backend_library::interface::TypeScriptFailureCause::MAX_BYTES);
        assert_eq!(cause.as_str().matches(u64::MAX.to_string().as_str()).count(), 3);
        assert!(cause.as_str().contains("active=struct"));
        let unknown = StopMeasurement { query: Some(QueryTiming::default()), ..observed }.cause().unwrap();
        assert!(unknown.as_str().contains("=-,-,-,-,-;"));
        // Saturation is explicit and never wraps an observed interval.
        assert_eq!(Duration::MAX.saturating_add(Duration::from_secs(1)), Duration::MAX);
    }

}
