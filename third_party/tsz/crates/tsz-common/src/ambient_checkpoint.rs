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
//! An *ambient* checkpoint closes that class of defect structurally: the owner
//! of one unit of work (one file check, one lazy file query, one planning
//! phase) installs its checkpoint for the calling thread with
//! [`enter_ambient_checkpoint`], and the shared chokepoints every long
//! computation must pass through (recursion guards, type-graph walkers, type
//! construction) call [`ambient_poll`]. A poll is a thread-local read when
//! nothing is installed, charges one work unit per [`AMBIENT_POLLS_PER_UNIT`]
//! polls when something is, and returns `false` once the checkpoint has
//! stopped. A `false` poll must be treated exactly like an exhausted
//! iteration budget: unwind quickly with a conservative placeholder. The
//! checkpoint stays latched, so the owner observes the stop through its own
//! explicit checkpoint and discards everything the interrupted computation
//! produced.

use crate::execution_budget::{ExecutionCheckpoint, ProjectExecutionStop};
use std::cell::Cell;
use std::marker::PhantomData;
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

/// Keeps one thread's ambient checkpoint installed; restores the previous one
/// (and its latched stop) when dropped.
///
/// The scope is neither `Send` nor `Sync`: the installation is thread-local and
/// must be undone on the thread that made it.
#[must_use = "dropping the scope immediately uninstalls the ambient checkpoint"]
pub struct AmbientCheckpointScope<'checkpoint> {
    previous: Option<RawCheckpoint>,
    previous_countdown: u32,
    previous_stop: Option<ProjectExecutionStop>,
    _borrow: PhantomData<&'checkpoint dyn ExecutionCheckpoint>,
    _not_send: PhantomData<*const ()>,
}

/// Installs `checkpoint` as this thread's ambient checkpoint until the
/// returned scope is dropped. Scopes nest: an inner scope shadows the outer one
/// and restores it, including the outer scope's latched stop, on drop.
pub fn enter_ambient_checkpoint(
    checkpoint: &dyn ExecutionCheckpoint,
) -> AmbientCheckpointScope<'_> {
    // SAFETY: only the pointer's lifetime is erased. The returned scope borrows
    // `checkpoint` for its whole life and is `!Send`, so the pointer is read only
    // on this thread and only while the scope (which removes it on drop) exists.
    let erased: &'static dyn ExecutionCheckpoint = unsafe {
        std::mem::transmute::<&dyn ExecutionCheckpoint, &'static dyn ExecutionCheckpoint>(
            checkpoint,
        )
    };
    let installed = if nofix("poll") { None } else { Some(NonNull::from(erased)) }; // TEMP-LOCAL-AID
    AMBIENT.with(|state| AmbientCheckpointScope {
        previous: state.installed.replace(installed),
        previous_countdown: state.countdown.replace(AMBIENT_POLLS_PER_READ),
        previous_stop: state.stopped.take(),
        _borrow: PhantomData,
        _not_send: PhantomData,
    })
}

impl Drop for AmbientCheckpointScope<'_> {
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
    // SAFETY: see `enter_ambient_checkpoint`; the scope that installed this
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
        {
            let _scope = enter_ambient_checkpoint(&counting);
            for _ in 0..(AMBIENT_POLLS_PER_READ * 10) {
                assert!(ambient_poll());
            }
        }
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
        {
            let _scope = enter_ambient_checkpoint(&budget);
            let mut polls = 0_u32;
            while ambient_poll() {
                polls += 1;
                assert!(polls <= AMBIENT_POLLS_PER_READ, "stop must surface within one batch");
            }
            assert!(ambient_stopped());
            assert!(!ambient_poll(), "a latched stop never recovers inside its scope");
        }
        assert!(!ambient_stopped(), "the stop does not outlive its scope");
        assert!(ambient_poll());
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
        let spent = {
            let _scope = enter_ambient_checkpoint(&slice);
            let mut polls = 0_u64;
            while ambient_poll() {
                polls += 1;
                assert!(polls < 1_000_000, "the slice must stop an unbounded loop");
            }
            polls
        };
        // 100 units at one unit per AMBIENT_POLLS_PER_UNIT polls.
        assert!(spent >= 100 * u64::from(AMBIENT_POLLS_PER_UNIT) - u64::from(AMBIENT_POLLS_PER_READ));
        assert!(slice.is_exhausted());
        assert_eq!(budget.stop_reason(), None, "a slice stop must not latch the program");
    }

    #[test]
    fn nested_scopes_shadow_and_restore() {
        let outer = Counting(AtomicU64::new(0));
        let inner = Counting(AtomicU64::new(0));
        let _outer_scope = enter_ambient_checkpoint(&outer);
        {
            let _inner_scope = enter_ambient_checkpoint(&inner);
            for _ in 0..AMBIENT_POLLS_PER_READ {
                assert!(ambient_poll());
            }
        }
        for _ in 0..AMBIENT_POLLS_PER_READ {
            assert!(ambient_poll());
        }
        assert_eq!(inner.0.load(Ordering::Relaxed), 16);
        assert_eq!(outer.0.load(Ordering::Relaxed), 16);
    }
}

// TEMP-LOCAL-AID: remove before commit. `TSZ_NOFIX=a,b` turns named fixes off at run time.
/// Local ablation aid: whether `TSZ_NOFIX` names `name` (or `all`).
pub fn nofix(name: &str) -> bool {
    use std::sync::OnceLock;
    static NOFIX: OnceLock<Vec<String>> = OnceLock::new();
    NOFIX
        .get_or_init(|| {
            std::env::var("TSZ_NOFIX")
                .map(|v| v.split(',').map(str::to_owned).collect())
                .unwrap_or_default()
        })
        .iter()
        .any(|n| n == name || n == "all")
}
