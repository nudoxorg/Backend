//! One bounded, allocation-free task-wake transfer cell.
//!
//! The consumer endpoint is uniquely polled, so registration has one logical writer. Completion,
//! cancellation, and endpoint drop may race to take that registration. `WakeCell` follows the
//! small registering/waking bit protocol: `register()` owns the `Waker` cell while REGISTERING;
//! `take()` owns it while WAKING; a concurrent take sets WAKING and the registering side performs
//! the wake after it has finished writing. Therefore a wake is never lost between registration and
//! the required post-registration state recheck.

use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU8, Ordering},
    task::Waker,
};

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
    pub(super) const fn new() -> Self {
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
                // SAFETY: REGISTERING gives this call exclusive access to `waker`. A contender
                // only sets WAKING and leaves taking the value to this registration.
                unsafe {
                    let current = &mut *self.waker.get();
                    if current
                        .as_ref()
                        .is_none_or(|known| !known.will_wake(candidate))
                    {
                        *current = Some(candidate.clone());
                    }
                }
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
                    // SAFETY: REGISTERING | WAKING can only have been produced by `take` while
                    // this registration owned the cell, so this is still the sole move-out.
                    let registered = unsafe { (&mut *self.waker.get()).take() };
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
                // SAFETY: IDLE -> WAKING grants this call exclusive access to the cell.
                let registered = unsafe { (&mut *self.waker.get()).take() };
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

// SAFETY: see the `Send` proof; concurrent callers use the atomic bit protocol before touching
// `waker` and never form aliased mutable references.
unsafe impl Sync for WakeCell {}
