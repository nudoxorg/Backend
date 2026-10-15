//! Cooperative, shared work and stop control for one native TypeScript program.
//!
//! The budget is intentionally borrowed from the caller's cancellation token.
//! Parallel parser/checker workers share one atomic work allowance and latch
//! the first stop reason. A stopped computation must discard every partial
//! parse, bind, check, or query result.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;
use std::sync::Arc;

/// Opaque identity for one admitted budget. Retaining the allocation prevents
/// address reuse after the original budget ends; equality uses allocation
/// identity rather than matching deadlines, cancellation flags, or allowance.
#[derive(Clone, Debug)]
pub struct ProjectExecutionIdentity(Arc<()>);

impl PartialEq for ProjectExecutionIdentity {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}
impl Eq for ProjectExecutionIdentity {}

/// Borrowed cooperative work checkpoint shared by parser, binder, checker,
/// and lazy semantic queries.
pub trait ExecutionCheckpoint: Send + Sync {
    /// Charge work and report the program's latched terminal reason, if any.
    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop>;

    /// Observe an already-entered computation boundary without charging work,
    /// polling cancellation, resolving types, or changing semantic state.
    /// Implementations must not re-enter the compiler from this callback.
    fn observe(&self, _event: ProjectExecutionEvent<'_>) {}

    /// Stable admitted-budget identity. Custom unmetered/test checkpoints may
    /// return `None`; production budget wrappers must forward this value.
    fn execution_identity(&self) -> Option<ProjectExecutionIdentity> { None }
}

/// Computation boundaries of one admitted native program. These observations
/// locate work; none of them proves complete semantic output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectExecutionPhase {
    SourceFrontier,
    ParseBind,
    Merge,
    FileFormats,
    ModuleOutcomes,
    CheckerPlan,
    LibraryViews,
    BinderViews,
    OwnershipIndex,
    SharedCheckerIndexes,
    AffectedLibraries,
    AugmentationPrime,
    FileReset,
    FileCheck,
    FilePostprocess,
    FileQuery,
    SourcePreparation,
    SourceStatements,
    SourcePostchecks,
    LibraryBaseline,
    LibraryCheck,
    Aggregate,
}

/// A returned boundary can still have a latched stop. Unwound means only that
/// its lexical scope ended before `returned` was called, including `?`/panic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectExecutionTransition {
    Entered,
    Returned,
    Unwound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectExecutionFile<'file> {
    pub index: usize,
    /// Exact file name in the admitted program, never a guessed hot file.
    pub path: &'file str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectExecutionEvent<'file> {
    pub phase: ProjectExecutionPhase,
    pub transition: ProjectExecutionTransition,
    pub file: Option<ProjectExecutionFile<'file>>,
}

/// Borrowed, allocation-free boundary observation. It does not add any work
/// checkpoint or otherwise change the existing admission/stop policy.
pub struct ProjectExecutionSpan<'checkpoint, 'file> {
    checkpoint: Option<&'checkpoint dyn ExecutionCheckpoint>,
    phase: ProjectExecutionPhase,
    file: Option<ProjectExecutionFile<'file>>,
    observer: Option<&'checkpoint dyn for<'event> Fn(ProjectExecutionEvent<'event>)>,
    returned: bool,
}

impl<'checkpoint, 'file> ProjectExecutionSpan<'checkpoint, 'file> {
    pub fn new(
        checkpoint: Option<&'checkpoint dyn ExecutionCheckpoint>,
        phase: ProjectExecutionPhase,
        file: Option<ProjectExecutionFile<'file>>,
    ) -> Self {
        let span = Self {
            checkpoint,
            phase,
            file,
            observer: None,
            returned: false,
        };
        span.emit(ProjectExecutionTransition::Entered);
        span
    }

    /// The query database may be thread-local. Borrow only its observation
    /// callback; it remains on that thread and never acquires semantic authority.
    pub fn with_observer(
        observer: &'checkpoint dyn for<'event> Fn(ProjectExecutionEvent<'event>),
        phase: ProjectExecutionPhase,
        file: Option<ProjectExecutionFile<'file>>,
    ) -> Self {
        let span = Self {
            checkpoint: None,
            phase,
            file,
            observer: Some(observer),
            returned: false,
        };
        span.emit(ProjectExecutionTransition::Entered);
        span
    }

    pub fn returned(mut self) {
        self.returned = true;
        self.emit(ProjectExecutionTransition::Returned);
    }

    fn emit(&self, transition: ProjectExecutionTransition) {
        if let Some(observer) = self.observer {
            observer(ProjectExecutionEvent {
                phase: self.phase,
                transition,
                file: self.file,
            });
        }
        if let Some(checkpoint) = self.checkpoint {
            checkpoint.observe(ProjectExecutionEvent {
                phase: self.phase,
                transition,
                file: self.file,
            });
        }
    }
}

impl Drop for ProjectExecutionSpan<'_, '_> {
    fn drop(&mut self) {
        if !self.returned {
            self.emit(ProjectExecutionTransition::Unwound);
        }
    }
}

/// Scope an arbitrary checkpoint so its first stop remains visible to every
/// later poll, even when the wrapped implementation is one-shot.
///
/// This is useful at boundaries that translate a typed stop into a local
/// sentinel such as `TypeId::UNKNOWN`: the owning operation can poll the same
/// scope after a callback and refuse to publish the callback result.
pub struct LatchedExecutionCheckpoint<'checkpoint> {
    source: &'checkpoint dyn ExecutionCheckpoint,
    stop: AtomicU8,
}

impl<'checkpoint> LatchedExecutionCheckpoint<'checkpoint> {
    /// Creates a stop-latching scope over a borrowed checkpoint.
    #[must_use]
    pub const fn new(source: &'checkpoint dyn ExecutionCheckpoint) -> Self {
        Self {
            source,
            stop: AtomicU8::new(0),
        }
    }

    fn encode_stop(stop: ProjectExecutionStop) -> u8 {
        match stop {
            ProjectExecutionStop::Cancelled => 1,
            ProjectExecutionStop::Deadline => 2,
            ProjectExecutionStop::WorkBudgetExhausted => 3,
        }
    }

    fn decode_stop(encoded: u8) -> Option<ProjectExecutionStop> {
        match encoded {
            1 => Some(ProjectExecutionStop::Cancelled),
            2 => Some(ProjectExecutionStop::Deadline),
            3 => Some(ProjectExecutionStop::WorkBudgetExhausted),
            _ => None,
        }
    }

    /// Returns the first stop observed in this scope, if any.
    #[must_use]
    pub fn stopped_reason(&self) -> Option<ProjectExecutionStop> {
        Self::decode_stop(self.stop.load(Ordering::Acquire))
    }
}

impl ExecutionCheckpoint for LatchedExecutionCheckpoint<'_> {
    fn execution_identity(&self) -> Option<ProjectExecutionIdentity> { self.source.execution_identity() }
    fn observe(&self, event: ProjectExecutionEvent<'_>) {
        self.source.observe(event);
    }

    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        if let Some(stop) = self.stopped_reason() {
            return Err(stop);
        }

        match self.source.checkpoint(work_units) {
            Ok(()) => self.stopped_reason().map_or(Ok(()), Err),
            Err(stop) => {
                let _ = self.stop.compare_exchange(
                    0,
                    Self::encode_stop(stop),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                Err(self.stopped_reason().unwrap_or(stop))
            }
        }
    }
}

/// A file-local work allowance carved out of one shared program checkpoint.
///
/// The slice charges its own allowance first and forwards exactly the units it
/// admitted to the parent, so the parent's spend is the sum of what every slice
/// admitted. When a charge does not fit, the slice drains, forwards only the
/// part that fit, and refuses the rest: the parent is neither charged for the
/// refused remainder nor latched, so one hot file cannot starve the files that
/// still have work to do. Cancellation, deadline, and program-wide exhaustion
/// are parent stops; they pass through unchanged and always win over the
/// slice's own exhaustion.
///
/// A refused slice reports [`ProjectExecutionStop::WorkBudgetExhausted`] to the
/// computation it guards, which unwinds exactly as it does for a program-wide
/// stop. The slice's owner tells the two apart with [`Self::is_exhausted`] after
/// confirming that the parent itself has not stopped.
///
/// Everything a guarded computation produced is partial once the slice is
/// exhausted. The owner must discard its results and must not reuse any mutable
/// state that the computation could have written to; the type database cannot
/// encode execution provenance. Re-arming is for the next, independent
/// computation only.
pub struct FileWorkSlice<'checkpoint> {
    parent: &'checkpoint dyn ExecutionCheckpoint,
    allowance: AtomicU64,
    remaining: AtomicU64,
    exhausted: AtomicBool,
}

impl<'checkpoint> FileWorkSlice<'checkpoint> {
    /// Creates an armed slice of `allowance` work units over `parent`.
    #[must_use]
    pub fn new(parent: &'checkpoint dyn ExecutionCheckpoint, allowance: u64) -> Self {
        Self {
            parent,
            allowance: AtomicU64::new(allowance),
            remaining: AtomicU64::new(allowance),
            exhausted: AtomicBool::new(false),
        }
    }

    /// Starts a new independent computation with a full allowance. Any
    /// exhaustion latched by the previous computation is cleared.
    pub fn arm(&self, allowance: u64) {
        self.allowance.store(allowance, Ordering::Release);
        self.remaining.store(allowance, Ordering::Release);
        self.exhausted.store(false, Ordering::Release);
    }

    /// Work units the current computation may spend in total.
    #[must_use]
    pub fn allowance(&self) -> u64 {
        self.allowance.load(Ordering::Acquire)
    }

    /// Work units admitted to the current computation so far.
    #[must_use]
    pub fn spent(&self) -> u64 {
        self.allowance()
            .saturating_sub(self.remaining.load(Ordering::Acquire))
    }

    /// Whether this slice, rather than its parent, refused a charge.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Acquire)
    }
}

impl ExecutionCheckpoint for FileWorkSlice<'_> {
    fn execution_identity(&self) -> Option<ProjectExecutionIdentity> {
        self.parent.execution_identity()
    }

    fn observe(&self, event: ProjectExecutionEvent<'_>) {
        self.parent.observe(event);
    }

    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        // A later cancellation or deadline remains authoritative even after
        // this independent slice has already spent its own allowance.
        self.parent.checkpoint(0)?;
        if self.is_exhausted() {
            return Err(ProjectExecutionStop::WorkBudgetExhausted);
        }
        if work_units == 0 {
            return self.parent.checkpoint(0);
        }
        let before = self
            .remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                Some(remaining.saturating_sub(work_units))
            })
            .unwrap_or_else(|unchanged| unchanged);
        // Charge the parent for exactly the part the slice admitted. A parent
        // stop is program-wide and must not be reported as slice exhaustion.
        self.parent.checkpoint(before.min(work_units))?;
        if before < work_units {
            self.exhausted.store(true, Ordering::Release);
            return Err(ProjectExecutionStop::WorkBudgetExhausted);
        }
        Ok(())
    }
}

/// Amortizes cooperative checks inside synchronous compiler walks.
///
/// Call [`Self::tick`] for each bounded unit of work and [`Self::finish`] at
/// the end of the stage. The meter checks cancellation/deadline/work allowance
/// at most every 64 units, then charges the remaining units on finish. A stage
/// that returns an error must discard any partial output it built.
pub struct ExecutionWorkMeter<'checkpoint> {
    checkpoint: &'checkpoint dyn ExecutionCheckpoint,
    pending_work_units: u64,
}

impl<'checkpoint> ExecutionWorkMeter<'checkpoint> {
    /// Starts a metered stage and immediately observes an already-latched stop.
    pub fn new(
        checkpoint: &'checkpoint dyn ExecutionCheckpoint,
    ) -> Result<Self, ProjectExecutionStop> {
        checkpoint.checkpoint(0)?;
        Ok(Self {
            checkpoint,
            pending_work_units: 0,
        })
    }

    /// Records completed work and polls once each 64 units.
    pub fn tick(&mut self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        self.pending_work_units = self.pending_work_units.saturating_add(work_units);
        if self.pending_work_units >= 64 {
            let charged = std::mem::take(&mut self.pending_work_units);
            self.checkpoint.checkpoint(charged)?;
        }
        Ok(())
    }

    /// Flushes any remaining work and observes a stop before stage output is
    /// returned to its caller.
    pub fn finish(mut self) -> Result<(), ProjectExecutionStop> {
        let charged = std::mem::take(&mut self.pending_work_units);
        self.checkpoint.checkpoint(charged)
    }
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
    execution_identity: ProjectExecutionIdentity,
    deadline: Instant,
    cancelled: &'cancel AtomicBool,
    remaining_work: AtomicU64,
    stop: AtomicU8,
}

impl<'cancel> ProjectExecutionBudget<'cancel> {
    /// Creates one bounded program budget.
    #[must_use]
    pub fn new(
        deadline: Instant,
        cancelled: &'cancel AtomicBool,
        maximum_work_units: u64,
    ) -> Self {
        Self {
            execution_identity: ProjectExecutionIdentity(Arc::new(())),
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
        // A zero-work poll still observes cancellation, deadline, and a
        // previously latched stop above and below. It does not consume any
        // allowance, so avoid contending on the shared counter for the many
        // cache-boundary polls that use this value.
        if work_units != 0 {
            // A charge larger than what is left drains the allowance to zero
            // instead of leaving the remainder untouched. `initial - remaining`
            // is then exactly the work admitted when the stop is latched, and
            // concurrent workers cannot keep spending a stale remainder.
            let before = self
                .remaining_work
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    Some(remaining.saturating_sub(work_units))
                })
                .unwrap_or_else(|unchanged| unchanged);
            if before < work_units {
                return Err(self.latch(ProjectExecutionStop::WorkBudgetExhausted));
            }
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
    fn execution_identity(&self) -> Option<ProjectExecutionIdentity> { Some(self.execution_identity.clone()) }
    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        ProjectExecutionBudget::checkpoint(self, work_units)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExecutionCheckpoint, ExecutionWorkMeter, FileWorkSlice, LatchedExecutionCheckpoint,
        ProjectExecutionBudget, ProjectExecutionStop,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn boundary_observation_is_read_only_and_forwards_through_latched_scopes() {
        use super::{
            ProjectExecutionEvent, ProjectExecutionFile, ProjectExecutionPhase as P,
            ProjectExecutionSpan, ProjectExecutionTransition as T,
        };
        use std::sync::Mutex;
        use std::sync::atomic::AtomicU64;
        struct Observer {
            polls: AtomicU64,
            events: Mutex<Vec<(P, T, Option<(usize, String)>)>>,
        }
        impl ExecutionCheckpoint for Observer {
            fn checkpoint(&self, _: u64) -> Result<(), ProjectExecutionStop> {
                self.polls.fetch_add(1, Ordering::Relaxed);
                Err(ProjectExecutionStop::Cancelled)
            }
            fn observe(&self, event: ProjectExecutionEvent<'_>) {
                self.events.lock().unwrap().push((
                    event.phase,
                    event.transition,
                    event.file.map(|file| (file.index, file.path.to_owned())),
                ));
            }
        }
        let observer = Observer {
            polls: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
        };
        let inner = LatchedExecutionCheckpoint::new(&observer);
        let outer = LatchedExecutionCheckpoint::new(&inner);
        let file = Some(ProjectExecutionFile {
            index: 3,
            path: "src/exact.ts",
        });
        ProjectExecutionSpan::new(Some(&outer), P::FileCheck, file).returned();
        {
            let _unfinished = ProjectExecutionSpan::new(Some(&outer), P::SourcePreparation, file);
        }
        assert_eq!(observer.polls.load(Ordering::Relaxed), 0);
        assert_eq!(inner.stopped_reason(), None);
        assert_eq!(outer.stopped_reason(), None);
        let events = observer.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .map(|(_, transition, _)| *transition)
                .collect::<Vec<_>>(),
            [T::Entered, T::Returned, T::Entered, T::Unwound]
        );
        assert!(
            events
                .iter()
                .all(|(_, _, path)| path.as_ref() == Some(&(3, "src/exact.ts".to_owned())))
        );
        drop(events);
        assert_eq!(outer.checkpoint(0), Err(ProjectExecutionStop::Cancelled));
        assert_eq!(observer.polls.load(Ordering::Relaxed), 1);
    }

    struct OneShotStop(AtomicBool);

    impl ExecutionCheckpoint for OneShotStop {
        fn checkpoint(&self, _work_units: u64) -> Result<(), ProjectExecutionStop> {
            if self.0.swap(false, Ordering::AcqRel) {
                Err(ProjectExecutionStop::Cancelled)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn latched_checkpoint_preserves_one_shot_stop_for_outer_boundary() {
        let source = OneShotStop(AtomicBool::new(true));
        let scope = LatchedExecutionCheckpoint::new(&source);
        assert_eq!(scope.checkpoint(1), Err(ProjectExecutionStop::Cancelled));
        assert_eq!(
            scope.stopped_reason(),
            Some(ProjectExecutionStop::Cancelled)
        );
        assert_eq!(scope.checkpoint(0), Err(ProjectExecutionStop::Cancelled));
    }

    #[test]
    fn work_is_shared_and_exhaustion_is_latched() {
        let cancelled = AtomicBool::new(false);
        let budget =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 3);
        assert_eq!(budget.checkpoint(2), Ok(()));
        assert_eq!(budget.remaining_work_units(), 1);
        assert_eq!(
            budget.checkpoint(2),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
        // The refused charge drains the allowance: admitted work is exact.
        assert_eq!(budget.remaining_work_units(), 0);
        assert_eq!(
            budget.stop_reason(),
            Some(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(
            budget.checkpoint(0),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
    }

    #[test]
    fn zero_work_checkpoint_observes_stops_without_charging_allowance() {
        let cancelled = AtomicBool::new(false);
        let budget =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 0);
        assert_eq!(budget.remaining_work_units(), 0);
        assert_eq!(budget.checkpoint(0), Ok(()));
        assert_eq!(budget.remaining_work_units(), 0);
        assert_eq!(
            budget.checkpoint(1),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(budget.remaining_work_units(), 0);
    }

    #[test]
    fn cancellation_and_deadline_are_typed() {
        let cancelled = AtomicBool::new(true);
        let canceled_budget =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 10);
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

    #[test]
    fn work_meter_polls_during_a_long_stage_and_flushes_the_tail() {
        let cancelled = AtomicBool::new(false);
        let budget =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 70);
        let mut meter = ExecutionWorkMeter::new(&budget).expect("start meter");
        for _ in 0..64 {
            meter.tick(1).expect("first batch is admitted");
        }
        assert_eq!(budget.remaining_work_units(), 6);
        for _ in 0..7 {
            meter.tick(1).expect("tail is pending until finish");
        }
        assert_eq!(
            meter.finish(),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(
            budget.stop_reason(),
            Some(ProjectExecutionStop::WorkBudgetExhausted)
        );
    }

    #[test]
    fn oversized_charge_drains_the_allowance_so_admitted_work_is_exact() {
        let cancelled = AtomicBool::new(false);
        let budget =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 10);
        assert_eq!(budget.checkpoint(4), Ok(()));
        assert_eq!(
            budget.checkpoint(1_000),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(budget.remaining_work_units(), 0, "initial - remaining == admitted");
        // An exact fit is admitted and only the next unit exhausts.
        let fit =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 5);
        assert_eq!(fit.checkpoint(5), Ok(()));
        assert_eq!(fit.remaining_work_units(), 0);
        assert_eq!(fit.stop_reason(), None);
        assert_eq!(
            fit.checkpoint(1),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
    }

    #[test]
    fn file_slice_exhaustion_is_local_and_charges_the_parent_only_what_it_admitted() {
        let cancelled = AtomicBool::new(false);
        let budget =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 100);
        let slice = FileWorkSlice::new(&budget, 10);
        assert_eq!(slice.checkpoint(0), Ok(()));
        assert_eq!(slice.checkpoint(6), Ok(()));
        assert_eq!((slice.spent(), budget.remaining_work_units()), (6, 94));
        assert_eq!(
            slice.checkpoint(7),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert!(slice.is_exhausted());
        assert_eq!(slice.spent(), 10, "the slice drains");
        assert_eq!(
            budget.remaining_work_units(),
            90,
            "the parent paid for the 4 units the slice admitted, not for the refused 7"
        );
        assert_eq!(budget.stop_reason(), None, "a file stop never latches the program");
        // Zero-work polls observe the slice's stop too, without touching the parent.
        assert_eq!(
            slice.checkpoint(0),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert_eq!(budget.remaining_work_units(), 90);
        // Another file is unaffected and can use the rest of the program.
        slice.arm(50);
        assert!(!slice.is_exhausted());
        assert_eq!(slice.checkpoint(50), Ok(()));
        assert_eq!(budget.remaining_work_units(), 40);
        assert_eq!(budget.checkpoint(0), Ok(()));
    }

    #[test]
    fn program_stops_win_over_file_slice_exhaustion_and_do_not_mark_the_slice() {
        let cancelled = AtomicBool::new(false);
        let budget =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 5);
        let slice = FileWorkSlice::new(&budget, 1_000);
        assert_eq!(
            slice.checkpoint(8),
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        );
        assert!(
            !slice.is_exhausted(),
            "the program, not the file's share, ran out"
        );
        assert_eq!(
            budget.stop_reason(),
            Some(ProjectExecutionStop::WorkBudgetExhausted)
        );

        let roomy =
            ProjectExecutionBudget::new(Instant::now() + Duration::from_secs(2), &cancelled, 100);
        let slice = FileWorkSlice::new(&roomy, 10);
        cancelled.store(true, Ordering::Release);
        assert_eq!(slice.checkpoint(1), Err(ProjectExecutionStop::Cancelled));
        assert!(!slice.is_exhausted());
    }

    #[test]
    fn cancellation_after_file_exhaustion_keeps_the_program_stop_authoritative() {
        let cancelled = AtomicBool::new(false);
        let budget = ProjectExecutionBudget::new(
            Instant::now() + Duration::from_secs(2), &cancelled, 100,
        );
        let slice = FileWorkSlice::new(&budget, 1);
        assert_eq!(slice.checkpoint(2), Err(ProjectExecutionStop::WorkBudgetExhausted));
        assert_eq!(budget.stop_reason(), None);
        cancelled.store(true, Ordering::Release);
        assert_eq!(slice.checkpoint(0), Err(ProjectExecutionStop::Cancelled));
        assert_eq!(budget.stop_reason(), Some(ProjectExecutionStop::Cancelled));
    }
}
