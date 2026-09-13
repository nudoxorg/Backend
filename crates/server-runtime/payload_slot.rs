//! Defines payload-slot behavior for `server-runtime`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the payload-slot invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Permit-addressed work storage and its publication/drop proof.
#![allow(
    unsafe_code,
    reason = "this is the charter-approved, private payload-cell exception; every operation is proven below"
)]
#![deny(unsafe_op_in_unsafe_fn)]

use core::{cell::UnsafeCell, mem::MaybeUninit};

use crate::{
    WorkHandle,
    admission_bundle::{DequeuedWork, QueuedWork},
    owner::TerminalRecord,
    slot::{CancelResult, OwnedSlot, RuntimeIdentity, Slot, SlotClaimError, SlotIndex},
};

/// One physical work coordinate.
///
/// Reference model: a permit owns one coordinate from `WorkPermits::acquire` until owner finish or
/// retirement. `claim` creates its queue proof, `publish` initializes this cell and then releases
/// the matching ready bit. `ReadyBitmap::take` acquires that release and gives the sole owner the
/// bit; it changes the slot state before moving the payload. Cancellation only changes the slot
/// state and never touches this cell. Thus, a payload is initialized at most once and moved/dropped
/// exactly once before the permit can be restored or retired.
/// One caller-arena addressable work slot.
///
/// It has no public operations. Supply `MaybeUninit<PayloadSlot<_, _>>` through
/// [`LocalRuntimeArena`](crate::LocalRuntimeArena); runtime construction initializes it exactly
/// once for the duration of the borrowed runtime.
pub struct PayloadSlot<Generation, Work, Failure> {
    slot: Slot,
    payload: PayloadCell<Generation, Work, Failure>,
}

enum SlotPayload<Generation, Work, Failure> {
    Queued(QueuedWork<Generation, Work>),
    Terminal(TerminalRecord<Generation, Failure>),
}

/// The only raw storage operations for one physical slot's safe tagged payload cell.
struct PayloadCell<Generation, Work, Failure> {
    raw: UnsafeCell<MaybeUninit<SlotPayload<Generation, Work, Failure>>>,
}

pub(crate) enum OwnerPayload<Generation, Work, Failure> {
    Queued(DequeuedWork<Generation, Work>, OwnedSlot),
    Terminal(TerminalRecord<Generation, Failure>),
}

pub(crate) enum TerminalPayload<Generation, Work, Failure> {
    Terminal(TerminalRecord<Generation, Failure>),
    Queued(QueuedWork<Generation, Work>),
}

impl<Generation, Work, Failure> PayloadCell<Generation, Work, Failure> {
    const fn uninit() -> Self {
        Self {
            raw: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// # Safety
    /// The caller owns the matching work permit and no ready bit has been published.
    unsafe fn write_queued(&self, queued: QueuedWork<Generation, Work>) {
        // SAFETY: upheld by the caller; this is the sole initializer for this payload cell.
        unsafe {
            (*self.raw.get()).write(SlotPayload::Queued(queued));
        }
    }

    /// # Safety
    /// The caller owns the matching slot after its queued payload moved out.
    unsafe fn write_terminal(&self, terminal: TerminalRecord<Generation, Failure>) {
        // SAFETY: upheld by the caller; this is the sole initializer for this payload cell.
        unsafe {
            (*self.raw.get()).write(SlotPayload::Terminal(terminal));
        }
    }

    /// # Safety
    /// The caller owns the matching ready bit and exclusively moves this initialized enum value.
    unsafe fn take(&self) -> SlotPayload<Generation, Work, Failure> {
        // SAFETY: this bitmap owner is the sole mover of the initialized safe enum.
        let raw = self.raw.get();
        // SAFETY: caller proved this cell is initialized for this lifecycle edge.
        let raw = unsafe { &*raw };
        // SAFETY: caller proved this cell is initialized for this lifecycle edge.
        let payload = unsafe { raw.assume_init_ref() };
        // SAFETY: caller owns the sole move-out from the initialized enum payload.
        unsafe { core::ptr::read(payload) }
    }

    /// # Safety
    /// The caller's slot state proves this cell has one initialized safe enum value.
    unsafe fn drop_initialized(&mut self) {
        // SAFETY: this exclusive drop path moves and drops exactly the initialized enum payload.
        let raw = self.raw.get();
        // SAFETY: caller proved the enum payload is initialized and exclusively droppable.
        let payload = unsafe { (*raw).as_mut_ptr() };
        // SAFETY: `payload` identifies exactly one initialized enum value.
        unsafe { core::ptr::drop_in_place(payload) };
    }
}

// SAFETY: A shared reference can reach `payload` only through the linear permit/ready-bit protocol
// above. `Generation` and `Work` must be sendable because a scoped producer may initialize a cell
// while the owner thread later reads it after the release/acquire ready-bit hand-off.
unsafe impl<Generation: Send, Work: Send, Failure: Send> Sync
    for PayloadSlot<Generation, Work, Failure>
{
}

impl<Generation, Work, Failure> PayloadSlot<Generation, Work, Failure> {
    pub(crate) fn new() -> Self {
        Self {
            slot: Slot::new(),
            payload: PayloadCell::uninit(),
        }
    }

    pub(crate) fn claim(
        &self,
        runtime: RuntimeIdentity,
        index: SlotIndex,
    ) -> Result<(WorkHandle, crate::slot::QueuedSlot), SlotClaimError> {
        self.slot.claim(runtime, index)
    }

    pub(crate) fn cancel(&self, handle: WorkHandle) -> CancelResult {
        self.slot.cancel(handle)
    }

    /// Writes the only work-bearing cell before its coordinate is visible in the ready bitmap.
    pub(crate) fn publish(&self, queued: QueuedWork<Generation, Work>) {
        // SAFETY: `queued` carries the unique permit for this coordinate. No other producer can
        // access this cell until that permit is returned, and it is not published to an owner yet.
        unsafe { self.payload.write_queued(queued) };
    }

    /// Claims a ready queued payload and turns its slot into one-owner execution in one swap.
    pub(crate) fn begin_owner(&self) -> OwnerPayload<Generation, Work, Failure> {
        // SAFETY: the unique work-ready bit acquires queued publication and grants the only move.
        match unsafe { self.payload.take() } {
            SlotPayload::Queued(queued) => {
                let owned = self.slot.dequeue(&queued.slot);
                OwnerPayload::Queued(queued.into_dequeued(), owned)
            }
            SlotPayload::Terminal(terminal) => OwnerPayload::Terminal(terminal),
        }
    }

    /// Reuses the same physical cell for the terminal fact until its owner observes it.
    pub(crate) fn retain_terminal(
        &self,
        owned: OwnedSlot,
        terminal: TerminalRecord<Generation, Failure>,
    ) {
        // SAFETY: the owner moved the queued payload out after claiming the only ready bit, so
        // this cell is uninitialized and the still-linear `OwnedSlot` excludes every producer.
        unsafe { self.payload.write_terminal(terminal) };
        self.slot.terminalize(owned);
    }

    /// Moves the in-place terminal record out before returning its coordinate to FREE.
    pub(crate) fn take_terminal(&self) -> TerminalPayload<Generation, Work, Failure> {
        // SAFETY: the terminal-ready bit acquires the release that published this initialized
        // terminal variant; its unique claim excludes another owner from moving it.
        // SAFETY: terminal-ready publication follows initialization of the terminal enum value.
        match unsafe { self.payload.take() } {
            SlotPayload::Terminal(terminal) => {
                self.slot.release_terminal(terminal.event.handle);
                TerminalPayload::Terminal(terminal)
            }
            SlotPayload::Queued(queued) => TerminalPayload::Queued(queued),
        }
    }

    pub(crate) fn restore_queued(&self, queued: QueuedWork<Generation, Work>) {
        // SAFETY: recovery owns the exact moved enum value before its ready-bit re-publication.
        unsafe { self.payload.write_queued(queued) };
    }

    pub(crate) fn restore_terminal(&self, terminal: TerminalRecord<Generation, Failure>) {
        // SAFETY: recovery owns the exact moved enum value before terminal-ready re-publication.
        unsafe { self.payload.write_terminal(terminal) };
    }

    #[cfg(all(test, not(feature = "loom-model")))]
    pub(crate) fn exhaust_free_for_test(&self) {
        self.slot.exhaust_free_for_test();
    }
}

impl<Generation, Work, Failure> Drop for PayloadSlot<Generation, Work, Failure> {
    fn drop(&mut self) {
        let Some(_retained) = self.slot.retained_payload() else {
            return;
        };
        // SAFETY: Fabric cannot be dropped while an Admission or Owner borrows it. The decoded
        // slot state proves this cell contains one initialized safe enum value.
        unsafe { self.payload.drop_initialized() };
    }
}
