//! Bounded multi-waiter readiness registration for async admission.
//!
//! `AtomicWaker` is the sole vetted unsafe primitive: one unique future owns each slot, while
//! this module's generation/status atomics prevent a stale slot wake from targeting a later user.
#![allow(
    missing_docs,
    reason = "the public registration error retains the exact bounded-resource or retired-slot cause"
)]
#![allow(
    clippy::indexing_slicing,
    reason = "a ticket originates from this exact fixed registry and carries its validated slot index"
)]

use alloc::{collections::TryReserveError, vec::Vec};
use core::{mem::MaybeUninit, num::NonZeroU64, sync::atomic::AtomicUsize, task::Waker};

#[cfg(not(all(test, feature = "loom-model")))]
use core::sync::atomic::{AtomicU64, Ordering};
#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicU64, Ordering};

use atomic_waker::AtomicWaker;
use thiserror::Error;

use crate::{initialized_prefix::InitializedPrefix, slot::SlotIndex};

const FREE: u64 = 0;
const ACTIVE: u64 = 1;
const ARMED: u64 = 2;
const WAKING: u64 = 3;
const CANCELLED: u64 = 4;
const STATUS_BITS: u64 = 3;
const STATUS_MASK: u64 = (1 << STATUS_BITS) - 1;
const MAX_EPOCH: u64 = u64::MAX >> STATUS_BITS;

/// Exact registration failure before a pending future takes ownership of caller work.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WaiterRegistrationError {
    #[error("all bounded async waiter registrations are occupied")]
    Capacity,
    #[error("async waiter slot {index} retired at epoch {epoch}")]
    EpochExhausted { index: usize, epoch: u64 },
    #[error("free waiter permit for coordinate {index} did not match a free slot state")]
    SlotPermitMismatch { index: usize },
}

/// Unique generation-qualified registration held by exactly one pending admission future.
#[derive(Debug)]
pub(crate) struct WaiterTicket {
    index: SlotIndex,
    epoch: u64,
}

/// Result of a register/arm attempt before the required admission recheck.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArmResult {
    Armed,
    WakeInFlight,
}

/// Production waiter-state transition core, compiled with std or Loom atomics.
pub(crate) struct WaiterStateCore {
    state: AtomicU64,
}

impl WaiterStateCore {
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the identical Loom-selected constructor is not const"
    )]
    fn new() -> Self {
        Self {
            state: AtomicU64::new(FREE),
        }
    }

    fn activate(&self, index: usize) -> Result<Option<u64>, WaiterRegistrationError> {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if status(observed) != FREE {
                return Ok(None);
            }
            let prior_epoch = epoch(observed);
            if prior_epoch == MAX_EPOCH {
                return Err(WaiterRegistrationError::EpochExhausted {
                    index,
                    epoch: prior_epoch,
                });
            }
            let next_epoch = prior_epoch + 1;
            match self.state.compare_exchange_weak(
                observed,
                encode(next_epoch, ACTIVE),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Some(next_epoch)),
                Err(actual) => observed = actual,
            }
        }
    }

    fn arm(&self, epoch: u64) -> ArmResult {
        match self.state.compare_exchange(
            encode(epoch, ACTIVE),
            encode(epoch, ARMED),
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => ArmResult::Armed,
            Err(_) => ArmResult::WakeInFlight,
        }
    }

    fn release(&self, expected_epoch: u64) -> ReleaseDisposition {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if epoch(observed) != expected_epoch {
                return ReleaseDisposition::Stale;
            }
            let (next, disposition) = match status(observed) {
                WAKING => (
                    encode(expected_epoch, CANCELLED),
                    ReleaseDisposition::WakeInFlight,
                ),
                FREE => return ReleaseDisposition::Stale,
                _ => (encode(expected_epoch, FREE), ReleaseDisposition::Released),
            };
            if self
                .state
                .compare_exchange_weak(observed, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return disposition;
            }
            observed = self.state.load(Ordering::Acquire);
        }
    }

    fn begin_wake(&self) -> Option<u64> {
        let observed = self.state.load(Ordering::Acquire);
        if status(observed) != ARMED {
            return None;
        }
        let epoch = epoch(observed);
        self.state
            .compare_exchange(
                observed,
                encode(epoch, WAKING),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| epoch)
    }

    fn complete_wake(&self, expected_epoch: u64) -> WakeCompletion {
        let waking = encode(expected_epoch, WAKING);
        match self.state.compare_exchange(
            waking,
            encode(expected_epoch, ACTIVE),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => WakeCompletion::Active,
            Err(observed) if observed == encode(expected_epoch, CANCELLED) => {
                match self.state.compare_exchange(
                    observed,
                    encode(expected_epoch, FREE),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => WakeCompletion::Released,
                    Err(_) => WakeCompletion::Stale,
                }
            }
            Err(_) => WakeCompletion::Stale,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleaseDisposition {
    Released,
    WakeInFlight,
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakeCompletion {
    Active,
    Released,
    Stale,
}

/// One caller-arena addressable async waiter record.
///
/// The record has no public operations. It is exposed only as the element type of
/// [`LocalRuntimeArena`](crate::LocalRuntimeArena)'s caller-provided uninitialized storage.
pub struct WaiterSlot {
    state: WaiterStateCore,
    byte_credits: AtomicUsize,
    waker: AtomicWaker,
}

/// Monomorphic physical storage for a fixed waiter coordinate domain.
#[doc(hidden)]
pub trait WaiterTable {
    fn capacity(&self) -> usize;
    fn slot(&self, index: SlotIndex) -> &WaiterSlot;
}

/// Heap-backed waiter table selected by [`RemoteStorage`](crate::RemoteStorage).
#[doc(hidden)]
pub struct RemoteWaiterTable {
    slots: Vec<WaiterSlot>,
}

impl RemoteWaiterTable {
    pub(crate) fn try_new(capacity: usize) -> Result<Self, TryReserveError> {
        let mut slots = Vec::new();
        slots.try_reserve_exact(capacity)?;
        for _ in 0..capacity {
            slots.push(WaiterSlot::new());
        }
        Ok(Self { slots })
    }
}

impl WaiterTable for RemoteWaiterTable {
    fn capacity(&self) -> usize {
        self.slots.len()
    }

    fn slot(&self, index: SlotIndex) -> &WaiterSlot {
        &self.slots[index.array_index()]
    }
}

/// Inline waiter table selected by [`InlineStorage`](crate::InlineStorage).
#[doc(hidden)]
pub struct InlineWaiterTable<const CAPACITY: usize> {
    slots: [WaiterSlot; CAPACITY],
}

impl<const CAPACITY: usize> InlineWaiterTable<CAPACITY> {
    pub(crate) fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| WaiterSlot::new()),
        }
    }
}

impl<const CAPACITY: usize> WaiterTable for InlineWaiterTable<CAPACITY> {
    fn capacity(&self) -> usize {
        CAPACITY
    }

    fn slot(&self, index: SlotIndex) -> &WaiterSlot {
        &self.slots[index.array_index()]
    }
}

/// Borrowed caller-arena waiter table. Its elements are initialized exactly once by runtime
/// construction after every capacity validation has succeeded.
#[doc(hidden)]
pub struct LocalWaiterTable<'arena> {
    slots: InitializedPrefix<'arena, WaiterSlot>,
}

impl<'arena> LocalWaiterTable<'arena> {
    pub(crate) fn initialize(slots: &'arena mut [MaybeUninit<WaiterSlot>]) -> Self {
        Self {
            slots: InitializedPrefix::initialize_with(slots, WaiterSlot::new),
        }
    }
}

impl WaiterTable for LocalWaiterTable<'_> {
    fn capacity(&self) -> usize {
        self.slots.as_slice().len()
    }

    fn slot(&self, index: SlotIndex) -> &WaiterSlot {
        &self.slots.as_slice()[index.array_index()]
    }
}

impl WaiterSlot {
    fn new() -> Self {
        Self {
            state: WaiterStateCore::new(),
            byte_credits: AtomicUsize::new(0),
            waker: AtomicWaker::new(),
        }
    }

    fn activate(
        &self,
        index: SlotIndex,
        byte_credits: usize,
    ) -> Result<Option<WaiterTicket>, WaiterRegistrationError> {
        let Some(epoch) = self.state.activate(index.array_index())? else {
            return Ok(None);
        };
        self.byte_credits.store(byte_credits, Ordering::Release);
        Ok(Some(WaiterTicket { index, epoch }))
    }

    fn arm(&self, ticket: &WaiterTicket, waker: &Waker) -> ArmResult {
        self.waker.register(waker);
        self.state.arm(ticket.epoch)
    }

    fn release(&self, ticket: &WaiterTicket) -> ReleaseDisposition {
        self.state.release(ticket.epoch)
    }

    fn wake_if_fitting(&self, bytes_available: usize) -> WakeDisposition {
        if self.byte_credits.load(Ordering::Acquire) > bytes_available {
            return WakeDisposition::Retained;
        }
        let Some(epoch) = self.state.begin_wake() else {
            return WakeDisposition::Stale;
        };
        WakeDisposition::Claimed(epoch)
    }

    fn finish_wake(&self, epoch: u64) -> WakeCompletion {
        // Taking the old waker while the generation is WAKING prevents a later registration from
        // being woken by this stale notification. Release/free happens only after that take.
        let waker = self.waker.take();
        let completion = self.state.complete_wake(epoch);
        if let Some(waker) = waker {
            waker.wake();
        }
        completion
    }

    fn discard_waker(&self) {
        // A normal release has already published FREE in the slot state, but its free permit is
        // still withheld. Taking the old waker before publishing that permit prevents a new
        // registration from inheriting it.
        let _old_waker = self.waker.take();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakeDisposition {
    Retained,
    Claimed(u64),
    Stale,
}

/// Fixed registration slab; both bitmaps are permits over the same bounded coordinate domain.
pub(crate) struct WaiterRegistry<Table = RemoteWaiterTable> {
    slots: Table,
    index: WaiterIndex,
}

/// Compact permits over the fixed waiter domain; they contain no wakers or caller work.
struct WaiterIndex {
    free: AtomicU64,
    armed: AtomicU64,
}

impl WaiterIndex {
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the identical Loom-selected constructor is not const"
    )]
    fn new(capacity: usize) -> Self {
        Self {
            free: AtomicU64::new(initial_free_mask(capacity)),
            armed: AtomicU64::new(0),
        }
    }

    fn claim_free(&self) -> Option<SlotIndex> {
        let mut observed = self.free.load(Ordering::Acquire);
        loop {
            let bits = NonZeroU64::new(observed)?;
            let slot = SlotIndex::from_ready_word(bits);
            match self.free.compare_exchange_weak(
                observed,
                observed & !slot.mask(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(slot),
                Err(actual) => observed = actual,
            }
        }
    }

    fn publish_free(&self, slot: SlotIndex) {
        self.free.fetch_or(slot.mask(), Ordering::Release);
    }

    fn arm(&self, slot: SlotIndex) {
        self.armed.fetch_or(slot.mask(), Ordering::Release);
    }

    fn clear_armed(&self, slot: SlotIndex) {
        self.armed.fetch_and(!slot.mask(), Ordering::AcqRel);
    }

    fn armed_snapshot(&self) -> u64 {
        self.armed.load(Ordering::Acquire)
    }

    #[cfg(all(test, feature = "loom-model"))]
    fn free_snapshot(&self) -> u64 {
        self.free.load(Ordering::Acquire)
    }
}

impl WaiterRegistry<RemoteWaiterTable> {
    pub(crate) fn new(capacity: usize) -> Result<Self, TryReserveError> {
        Ok(Self::from_table(RemoteWaiterTable::try_new(capacity)?))
    }
}

impl<Table: WaiterTable> WaiterRegistry<Table> {
    pub(crate) fn from_table(slots: Table) -> Self {
        let index = WaiterIndex::new(slots.capacity());
        Self { slots, index }
    }

    pub(crate) fn register(
        &self,
        byte_credits: usize,
    ) -> Result<WaiterTicket, WaiterRegistrationError> {
        let Some(index) = self.index.claim_free() else {
            return Err(WaiterRegistrationError::Capacity);
        };
        match self.slots.slot(index).activate(index, byte_credits)? {
            Some(ticket) => Ok(ticket),
            None => {
                // The free permit is published only after the matching slot is FREE, so this is
                // a protocol breach rather than a capacity race. Keep the permit withheld.
                Err(WaiterRegistrationError::SlotPermitMismatch {
                    index: index.array_index(),
                })
            }
        }
    }

    pub(crate) fn arm(&self, ticket: &WaiterTicket, waker: &Waker) {
        let result = self.slots.slot(ticket.index).arm(ticket, waker);
        if result == ArmResult::Armed {
            self.index.arm(ticket.index);
        }
    }

    #[cfg(all(test, feature = "loom-model"))]
    fn arm_observed(&self, ticket: &WaiterTicket, waker: &Waker) -> ArmResult {
        let result = self.slots.slot(ticket.index).arm(ticket, waker);
        if result == ArmResult::Armed {
            self.index.arm(ticket.index);
        }
        result
    }

    pub(crate) fn release(&self, ticket: &WaiterTicket) {
        // Clear the unversioned armed bit before the state can become FREE. A next epoch cannot
        // claim the free permit until this method publishes it below, so old cleanup can never
        // clear an armed bit installed by a reused coordinate.
        self.index.clear_armed(ticket.index);
        match self.slots.slot(ticket.index).release(ticket) {
            ReleaseDisposition::Released => {
                self.slots.slot(ticket.index).discard_waker();
                self.index.publish_free(ticket.index);
            }
            ReleaseDisposition::WakeInFlight | ReleaseDisposition::Stale => {}
        }
    }

    pub(crate) fn armed_snapshot(&self) -> u64 {
        self.index.armed_snapshot()
    }

    pub(crate) fn wake_fitting(&self, mut candidates: u64, bytes_available: usize) {
        while let Some(bits) = NonZeroU64::new(candidates) {
            let index = SlotIndex::from_ready_word(bits);
            candidates &= !index.mask();
            match self.slots.slot(index).wake_if_fitting(bytes_available) {
                // A stale candidate is intentionally not cleared: free publication happens only
                // after the releasing generation cleared its bit, so this coordinate may already
                // carry an armed newer generation.
                WakeDisposition::Retained | WakeDisposition::Stale => {}
                WakeDisposition::Claimed(epoch) => {
                    // Clear while the slot is WAKING. A later arm cannot win until `finish_wake`
                    // restores ACTIVE, at which point it sets a fresh bit.
                    self.index.clear_armed(index);
                    if self.slots.slot(index).finish_wake(epoch) == WakeCompletion::Released {
                        self.index.publish_free(index);
                    }
                }
            }
        }
    }
}

const fn initial_free_mask(capacity: usize) -> u64 {
    if capacity == 64 {
        u64::MAX
    } else {
        (1_u64 << capacity) - 1
    }
}

const fn encode(epoch: u64, status: u64) -> u64 {
    (epoch << STATUS_BITS) | status
}
const fn epoch(state: u64) -> u64 {
    state >> STATUS_BITS
}
const fn status(state: u64) -> u64 {
    state & STATUS_MASK
}

#[cfg(all(test, feature = "loom-model"))]
#[path = "waiter/tests.rs"]
mod tests;
