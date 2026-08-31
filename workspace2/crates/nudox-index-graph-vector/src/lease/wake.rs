//! One bounded, allocation-free task-wake transfer cell.
//!
//! The consumer endpoint is uniquely polled, so registration has one logical writer. Completion,
//! cancellation, and endpoint drop may race to take that registration. `WakeCell` follows the
//! small registering/waking bit protocol: `register()` owns the `Waker` cell while REGISTERING;
//! `take()` owns it while WAKING; a concurrent take sets WAKING and the registering side performs
//! the wake after it has finished writing. Therefore a wake is never lost between registration and
//! the required post-registration state recheck.

#[cfg(not(all(test, feature = "loom-model")))]
use core::sync::atomic::{AtomicU8, Ordering};
use core::task::Waker;
#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicU8, Ordering};

use super::cell::{self, UnsafeCell};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakeState {
    Idle = 0,
    Registering = 1,
    Waking = 2,
    RegisteringAndWaking = 3,
    Corrupt,
}

impl WakeState {
    fn observe(raw: u8) -> Self {
        match raw {
            raw if raw == Self::Idle as u8 => Self::Idle,
            raw if raw == Self::Registering as u8 => Self::Registering,
            raw if raw == Self::Waking as u8 => Self::Waking,
            raw if raw == Self::RegisteringAndWaking as u8 => Self::RegisteringAndWaking,
            _ => Self::Corrupt,
        }
    }
}

/// A single-registration, multi-wake transfer cell.
pub(super) struct WakeCell {
    state: AtomicU8,
    waker: UnsafeCell<Option<Waker>>,
}

impl WakeCell {
    pub(super) fn new() -> Self {
        Self {
            state: AtomicU8::new(WakeState::Idle as u8),
            waker: UnsafeCell::new(None),
        }
    }

    pub(super) fn register(&self, candidate: &Waker) {
        match self.state.compare_exchange(
            WakeState::Idle as u8,
            WakeState::Registering as u8,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // REGISTERING gives this call exclusive access to `waker`. A contender only sets
                // WAKING and leaves taking the value to this registration.
                cell::write(&self.waker, |current| {
                    // SAFETY: the REGISTERING phase grants this callback unique mutable access;
                    // the pointer never escapes the callback.
                    let current = unsafe { &mut *current };
                    if current
                        .as_ref()
                        .is_none_or(|known| !known.will_wake(candidate))
                    {
                        *current = Some(candidate.clone());
                    }
                });
                if self
                    .state
                    .compare_exchange(
                        WakeState::Registering as u8,
                        WakeState::Idle as u8,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    // REGISTERING | WAKING can only have been produced by `take` while this
                    // registration owned the cell, so this is still the sole move-out.
                    let registered = cell::write(&self.waker, |current| {
                        // SAFETY: REGISTERING | WAKING is owned by this registration, so the
                        // pointer remains exclusive for the callback and does not escape.
                        unsafe { (&mut *current).take() }
                    });
                    self.state.store(WakeState::Idle as u8, Ordering::Release);
                    if let Some(registered) = registered {
                        registered.wake();
                    }
                }
            }
            Err(raw) if WakeState::observe(raw) == WakeState::Waking => candidate.wake_by_ref(),
            Err(raw)
                if matches!(
                    WakeState::observe(raw),
                    WakeState::Registering | WakeState::RegisteringAndWaking
                ) =>
            {
                // A unique endpoint never registers concurrently. This defensive path preserves
                // memory safety if a caller violates that API law; the active registration will
                // recheck its state before returning Pending.
            }
            Err(_) => candidate.wake_by_ref(),
        }
    }

    pub(super) fn take(&self) -> Option<Waker> {
        match WakeState::observe(
            self.state
                .fetch_or(WakeState::Waking as u8, Ordering::AcqRel),
        ) {
            WakeState::Idle => {
                // IDLE -> WAKING grants this call exclusive access to the cell.
                let registered = cell::write(&self.waker, |current| {
                    // SAFETY: IDLE -> WAKING grants this call exclusive access to the waker cell;
                    // the pointer never escapes the callback.
                    unsafe { (&mut *current).take() }
                });
                self.state
                    .fetch_and(!(WakeState::Waking as u8), Ordering::Release);
                registered
            }
            WakeState::Registering | WakeState::RegisteringAndWaking | WakeState::Waking => None,
            WakeState::Corrupt => None,
        }
    }

    pub(super) fn wake(&self) {
        if let Some(registered) = self.take() {
            registered.wake();
        }
    }
}

impl core::fmt::Debug for WakeCell {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("WakeCell").finish_non_exhaustive()
    }
}

// SAFETY: concurrent callers use the atomic bit protocol before touching `waker` and never form
// aliased mutable references; Waker's own Send/Sync contract covers its transferred value.
unsafe impl Sync for WakeCell {}

#[cfg(all(test, feature = "loom-model"))]
mod loom_tests {
    use core::sync::atomic::Ordering;
    use std::{
        sync::Arc as StdArc,
        task::{Wake, Waker},
    };

    use loom::{
        sync::{Arc, atomic::AtomicUsize},
        thread,
    };

    use super::WakeCell;

    #[derive(Debug)]
    struct WakeProbe {
        wakes: AtomicUsize,
    }

    impl Wake for WakeProbe {
        fn wake(self: StdArc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &StdArc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn register_and_take_race_preserves_a_wake_or_a_pending_registration() {
        loom::model(|| {
            let cell = Arc::new(WakeCell::new());
            let probe = StdArc::new(WakeProbe {
                wakes: AtomicUsize::new(0),
            });
            let waker = Waker::from(StdArc::clone(&probe));

            let registering_cell = Arc::clone(&cell);
            let registering_waker = waker.clone();
            let registering = thread::spawn(move || {
                registering_cell.register(&registering_waker);
            });
            let taking_cell = Arc::clone(&cell);
            let taking = thread::spawn(move || taking_cell.take());

            assert!(registering.join().is_ok());
            let taken = taking.join();
            assert!(taken.is_ok());
            let Some(taken) = taken.ok().flatten() else {
                if let Some(pending) = cell.take() {
                    pending.wake();
                }
                assert_eq!(probe.wakes.load(Ordering::SeqCst), 1);
                return;
            };

            taken.wake();
            assert_eq!(probe.wakes.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn wake_takes_one_registration_and_leaves_the_cell_empty() {
        loom::model(|| {
            let cell = Arc::new(WakeCell::new());
            let probe = StdArc::new(WakeProbe {
                wakes: AtomicUsize::new(0),
            });
            let waker = Waker::from(StdArc::clone(&probe));
            cell.register(&waker);

            let waking_cell = Arc::clone(&cell);
            let waking = thread::spawn(move || waking_cell.wake());
            assert!(waking.join().is_ok());
            assert_eq!(probe.wakes.load(Ordering::SeqCst), 1);
            assert!(cell.take().is_none());
        });
    }
}
