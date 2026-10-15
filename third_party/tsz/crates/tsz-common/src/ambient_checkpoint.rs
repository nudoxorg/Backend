//! Thread-ambient execution checkpoint for solver and checker hot loops.
//!
//! The explicit [`ExecutionCheckpoint`] reaches a computation only through the
//! database object that was handed to it. Deep recursive walks (variance
//! analysis, dependency collection, type-graph visitors, ...) frequently run
//! over a database that carries no checkpoint, or are written as plain
//! recursion with no database in hand, so a single hot query could run for
//! minutes without ever observing a deadline, a cancellation, or its file's
//! work slice.
//!
//! The owner of one file check or semantic query lends its checkpoint with
//! [`with_ambient_checkpoint`]. Audited variance guards and instantiation
//! walks call [`ambient_poll`]; other graph walkers retain their existing
//! controls until their cache publication paths support interruption.
//! A poll is a thread-local read when
//! nothing is installed, charges one work unit per [`AMBIENT_POLLS_PER_UNIT`]
//! polls when something is, and returns `false` once the checkpoint has
//! stopped. A `false` poll must be treated exactly like an exhausted
//! iteration budget: unwind quickly with a conservative placeholder. The
//! checkpoint stays latched, so the owner observes the stop through its own
//! explicit checkpoint and discards everything the interrupted computation
//! produced.

use crate::execution_budget::{ExecutionCheckpoint, ProjectExecutionStop};
use std::cell::Cell;
use std::ptr::NonNull;

/// Ambient polls that together charge one work unit to the installed
/// checkpoint. Batching keeps a poll to a counter decrement, and keeps the
/// charge small next to the units explicit checkpoints already spend on the
/// same work.
pub const AMBIENT_POLLS_PER_UNIT: u32 = 4;

/// Polls between two reads of the installed checkpoint (deadline, cancellation,
/// slice). One read charges `AMBIENT_POLLS_PER_READ / AMBIENT_POLLS_PER_UNIT`
/// units.
const AMBIENT_POLLS_PER_READ: u32 = 64;

type RawCheckpoint = NonNull<dyn ExecutionCheckpoint>;

/// One thread's ambient state, kept in a single thread-local so a poll resolves
/// thread-local storage once.
struct AmbientState {
    installed: Cell<Option<RawCheckpoint>>,
    countdown: Cell<u32>,
    stopped: Cell<Option<ProjectExecutionStop>>,
}

thread_local! {
    static AMBIENT: AmbientState = const {
        AmbientState {
            installed: Cell::new(None),
            countdown: Cell::new(AMBIENT_POLLS_PER_READ),
            stopped: Cell::new(None),
        }
    };
}

// Private and unforgeable: safe callers cannot forget or move the installation
// guard. Its Drop runs before the borrowed checkpoint can leave the callback.
struct AmbientCheckpointScope {
    previous: Option<RawCheckpoint>,
    previous_countdown: u32,
    previous_stop: Option<ProjectExecutionStop>,
}

/// Runs `work` with a borrowed thread-local checkpoint, restoring the prior
/// checkpoint on normal return and unwinding. No borrowed installation can
/// escape: the restoration guard is private and never passed to `work`.
/// `None` preserves an enclosing checkpoint, if there is one.
pub fn with_ambient_checkpoint<Output>(
    checkpoint: Option<&dyn ExecutionCheckpoint>,
    work: impl FnOnce() -> Output,
) -> Output {
    let Some(checkpoint) = checkpoint else {
        return work();
    };
    // SAFETY: the pointer is only read synchronously during `work`. The private
    // guard restores the previous pointer before this function returns or
    // unwinds. Safe callers never receive the guard and cannot leak it with
    // mem::forget. Nested callbacks restore their parent's still-live borrow.
    let erased: &'static dyn ExecutionCheckpoint = unsafe {
        std::mem::transmute::<&dyn ExecutionCheckpoint, &'static dyn ExecutionCheckpoint>(
            checkpoint,
        )
    };
    let _scope = AMBIENT.with(|state| AmbientCheckpointScope {
        previous: state.installed.replace(Some(NonNull::from(erased))),
        previous_countdown: state.countdown.replace(AMBIENT_POLLS_PER_READ),
        previous_stop: state.stopped.take(),
    });
    work()
}

impl Drop for AmbientCheckpointScope {
    fn drop(&mut self) {
        // `try_with`: a scope dropped while the thread is being torn down has
        // nothing left to restore.
        let _ = AMBIENT.try_with(|state| {
            state.installed.set(self.previous);
            state.countdown.set(self.previous_countdown);
            state.stopped.set(self.previous_stop);
        });
    }
}

/// Whether an ambient checkpoint is installed on this thread.
#[inline]
#[must_use]
pub fn ambient_checkpoint_installed() -> bool {
    AMBIENT.with(|state| state.installed.get().is_some())
}

/// Observes the ambient checkpoint from a hot loop. Returns `true` when the
/// computation may continue and `false` once the checkpoint has stopped.
///
/// With nothing installed this is one thread-local read and always `true`.
#[inline]
#[must_use]
pub fn ambient_poll() -> bool {
    AMBIENT.with(|state| match state.installed.get() {
        None => true,
        Some(checkpoint) => ambient_poll_installed(state, checkpoint),
    })
}

/// Whether the ambient checkpoint has already reported a stop, without
/// charging work or reading the checkpoint.
#[inline]
#[must_use]
pub fn ambient_stopped() -> bool {
    AMBIENT.with(|state| state.stopped.get().is_some())
}

#[inline]
fn ambient_poll_installed(state: &AmbientState, checkpoint: RawCheckpoint) -> bool {
    if state.stopped.get().is_some() {
        return false;
    }
    let remaining = state.countdown.get().saturating_sub(1);
    if remaining != 0 {
        state.countdown.set(remaining);
        return true;
    }
    ambient_read(state, checkpoint)
}

/// Reads the installed checkpoint once per [`AMBIENT_POLLS_PER_READ`] polls.
#[cold]
#[inline(never)]
fn ambient_read(state: &AmbientState, checkpoint: RawCheckpoint) -> bool {
    state.countdown.set(AMBIENT_POLLS_PER_READ);
    // SAFETY: see `with_ambient_checkpoint`; the private scope that installed this
    // pointer is alive because it is still the thread's installed checkpoint.
    let checkpoint = unsafe { checkpoint.as_ref() };
    match checkpoint.checkpoint(u64::from(AMBIENT_POLLS_PER_READ / AMBIENT_POLLS_PER_UNIT)) {
        Ok(()) => true,
        Err(stop) => {
            state.stopped.set(Some(stop));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_budget::{FileWorkSlice, ProjectExecutionBudget};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    struct Counting(AtomicU64);
    impl ExecutionCheckpoint for Counting {
        fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
            self.0.fetch_add(work_units, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn polling_without_an_installed_checkpoint_never_stops() {
        assert!(!ambient_checkpoint_installed());
        for _ in 0..10_000 {
            assert!(ambient_poll());
        }
        assert!(!ambient_stopped());
    }

    #[test]
    fn polls_charge_one_unit_per_batch_and_are_amortized() {
        let counting = Counting(AtomicU64::new(0));
        with_ambient_checkpoint(Some(&counting), || {
            for _ in 0..(AMBIENT_POLLS_PER_READ * 10) {
                assert!(ambient_poll());
            }
        });
        assert_eq!(
            counting.0.load(Ordering::Relaxed),
            u64::from(10 * AMBIENT_POLLS_PER_READ / AMBIENT_POLLS_PER_UNIT)
        );
        assert!(!ambient_checkpoint_installed());
    }

    #[test]
    fn a_deadline_stops_every_later_poll_and_the_stop_is_scoped() {
        let cancelled = AtomicBool::new(false);
        let budget = ProjectExecutionBudget::new(
            Instant::now() - Duration::from_millis(1),
            &cancelled,
            u64::MAX,
        );
        with_ambient_checkpoint(Some(&budget), || {
            let mut polls = 0_u32;
            while ambient_poll() {
                polls += 1;
                assert!(
                    polls <= AMBIENT_POLLS_PER_READ,
                    "stop must surface within one batch"
                );
            }
            assert!(ambient_stopped());
            assert!(
                !ambient_poll(),
                "a latched stop never recovers inside its scope"
            );
        });
        assert!(!ambient_stopped(), "the stop does not outlive its scope");
        assert!(ambient_poll());
    }

    #[test]
    fn cancellation_stops_within_one_read_batch_and_remains_latched() {
        let cancelled = AtomicBool::new(false);
        let budget = ProjectExecutionBudget::new(
            Instant::now() + Duration::from_secs(3600),
            &cancelled,
            u64::MAX,
        );
        with_ambient_checkpoint(Some(&budget), || {
            cancelled.store(true, Ordering::Release);
            let mut polls = 0;
            while ambient_poll() {
                polls += 1;
                assert!(polls < AMBIENT_POLLS_PER_READ);
            }
            assert_eq!(budget.stop_reason(), Some(ProjectExecutionStop::Cancelled));
            assert!(!ambient_poll());
        });
        assert!(!ambient_checkpoint_installed());
    }

    #[test]
    fn a_file_slice_bounds_unmetered_work_and_leaves_the_parent_running() {
        let cancelled = AtomicBool::new(false);
        let budget = ProjectExecutionBudget::new(
            Instant::now() + Duration::from_secs(3600),
            &cancelled,
            1_000_000,
        );
        let slice = FileWorkSlice::new(&budget, 100);
        let spent = with_ambient_checkpoint(Some(&slice), || {
            let mut polls = 0_u64;
            while ambient_poll() {
                polls += 1;
                assert!(polls < 1_000_000, "the slice must stop an unbounded loop");
            }
            polls
        });
        // 100 units at one unit per AMBIENT_POLLS_PER_UNIT polls.
        assert!(
            spent >= 100 * u64::from(AMBIENT_POLLS_PER_UNIT) - u64::from(AMBIENT_POLLS_PER_READ)
        );
        assert!(slice.is_exhausted());
        assert_eq!(
            budget.stop_reason(),
            None,
            "a slice stop must not latch the program"
        );
    }

    #[test]
    fn nested_scopes_shadow_and_restore() {
        let outer = Counting(AtomicU64::new(0));
        let inner = Counting(AtomicU64::new(0));
        with_ambient_checkpoint(Some(&outer), || {
            with_ambient_checkpoint(Some(&inner), || {
                for _ in 0..AMBIENT_POLLS_PER_READ {
                    assert!(ambient_poll());
                }
            });
            for _ in 0..AMBIENT_POLLS_PER_READ {
                assert!(ambient_poll());
            }
            assert_eq!(inner.0.load(Ordering::Relaxed), 16);
            assert_eq!(outer.0.load(Ordering::Relaxed), 16);
        });
    }
    #[test]
    fn unwind_restores_parent_and_none_keeps_parent_checkpoint() {
        let outer = Counting(AtomicU64::new(0));
        let inner = Counting(AtomicU64::new(0));
        with_ambient_checkpoint(Some(&outer), || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_ambient_checkpoint(Some(&inner), || panic!("callback stop"));
            }));
            assert!(result.is_err());
            with_ambient_checkpoint(None, || {
                for _ in 0..AMBIENT_POLLS_PER_READ {
                    assert!(ambient_poll());
                }
            });
        });
        assert_eq!(outer.0.load(Ordering::Relaxed), 16);
        assert_eq!(inner.0.load(Ordering::Relaxed), 0);
        assert!(!ambient_checkpoint_installed());
    }
}
