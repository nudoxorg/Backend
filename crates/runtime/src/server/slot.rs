//! Defines slot behavior for `backend_runtime::server`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the slot invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! ABA-safe cancellation states with linear owner transition proofs.
use core::num::{NonZeroU8, NonZeroU64};

#[cfg(not(all(test, feature = "loom-model")))]
use core::sync::atomic::{AtomicU64, Ordering};
#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotStatus {
    Free = 0,
    Queued = 1,
    Cancelled = 2,
    InFlight = 3,
    Terminal = 4,
}

impl SlotStatus {
    const fn bits(self) -> u64 {
        match self {
            Self::Free => 0,
            Self::Queued => 1,
            Self::Cancelled => 2,
            Self::InFlight => 3,
            Self::Terminal => 4,
        }
    }
}

/// Largest epoch that can be packed without overwriting status bits.
pub(crate) const MAX_EPOCH: u64 = u64::MAX >> 6;
const SLOT_BITS: u32 = 6;
const SLOT_MASK: u64 = (1 << SLOT_BITS) - 1;

/// Opaque identity allocated once for a concrete runtime fabric.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RuntimeIdentity(pub(crate) u64);
/// A checked coordinate in the one-word physical-slot domain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SlotIndex(NonZeroU8);

impl SlotIndex {
    /// Derives one physical coordinate from a nonzero machine-word ready word.
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "trailing_zeros of a NonZeroU64 is exactly 0..=63"
    )]
    pub(crate) fn from_ready_word(bits: NonZeroU64) -> Self {
        let coordinate = bits.trailing_zeros() as u8;
        Self::from_coordinate(coordinate)
    }

    pub(crate) fn mask(self) -> u64 {
        1_u64 << u32::from(self.coordinate())
    }

    pub(crate) fn array_index(self) -> usize {
        usize::from(self.coordinate())
    }

    fn packed(self) -> u64 {
        u64::from(self.coordinate())
    }

    const fn coordinate(self) -> u8 {
        self.0.get() >> 1
    }

    fn from_coordinate(coordinate: u8) -> Self {
        // A ready-word coordinate is 0..=63. Shifting it leaves bit zero clear; OR with the
        // `NonZeroU8` one marker safely constructs a bijective odd encoding 1..=127.
        Self(NonZeroU8::MIN | (coordinate << 1))
    }
}

/// Generation-qualified, runtime-branded process-local work handle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WorkHandle {
    pub(crate) runtime: RuntimeIdentity,
    coordinate_epoch: u64,
}

impl WorkHandle {
    fn new(runtime: RuntimeIdentity, index: SlotIndex, epoch: u64) -> Self {
        Self {
            runtime,
            coordinate_epoch: (epoch << SLOT_BITS) | index.packed(),
        }
    }

    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "SLOT_MASK proves the packed coordinate is exactly 0..=63"
    )]
    pub(crate) fn index(self) -> SlotIndex {
        let coordinate = (self.coordinate_epoch & SLOT_MASK) as u8;
        Self::slot_index_from_packed_coordinate(coordinate)
    }

    pub(crate) const fn epoch(self) -> u64 {
        self.coordinate_epoch >> SLOT_BITS
    }

    fn slot_index_from_packed_coordinate(coordinate: u8) -> SlotIndex {
        SlotIndex::from_coordinate(coordinate)
    }
}
/// Atomic cancellation result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelResult {
    /// This call changed the matching queued coordinate to cancelled.
    Cancelled,
    /// A prior call already changed the matching queued coordinate to cancelled.
    AlreadyCancelled,
    /// The handle is stale, in flight, terminal, or has no queued coordinate.
    NotQueued,
    /// The handle belongs to another runtime fabric.
    ForeignRuntime,
}

/// Explicit retirement of a slot whose packed epoch cannot advance without ABA reuse.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SlotClaimError {
    /// The final ABA-safe epoch was already issued for this physical coordinate.
    #[error("slot epoch {epoch} is exhausted and has been retired")]
    EpochExhausted {
        /// The last issued epoch that cannot advance without reuse.
        epoch: u64,
    },
}

/// Queue ownership proof. Only admission can create it and only the owner can transition it.
pub(crate) struct QueuedSlot {
    handle: WorkHandle,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DequeuedDisposition {
    InFlight,
    Cancelled,
}

/// The initialized payload class retained by a non-free physical coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedPayload {
    Queued,
    Terminal,
}
/// Owner proof after an exact queued-to-in-flight or queued-to-cancelled transition.
pub(crate) struct OwnedSlot {
    handle: WorkHandle,
    disposition: DequeuedDisposition,
}
impl OwnedSlot {
    pub(crate) const fn was_cancelled(&self) -> bool {
        matches!(self.disposition, DequeuedDisposition::Cancelled)
    }
}
/// Production transition core, compiled with core atomics or Loom atomics in feature-selected tests.
pub(crate) struct SlotCore {
    state: AtomicU64,
}
pub(crate) type Slot = SlotCore;

/// The only packed representation admitted to the slot atomic.
#[derive(Clone, Copy)]
struct PackedSlotState(u64);

impl PackedSlotState {
    const fn new(epoch: u64, status: SlotStatus) -> Self {
        Self((epoch << 3) | status.bits())
    }

    const fn raw(self) -> u64 {
        self.0
    }

    const fn epoch(self) -> u64 {
        self.0 >> 3
    }

    const fn status_bits(self) -> u64 {
        self.0 & 0b111
    }
}

impl SlotCore {
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the identical Loom-selected constructor is not const"
    )]
    pub(crate) fn new() -> Self {
        Self {
            state: AtomicU64::new(PackedSlotState::new(0, SlotStatus::Free).raw()),
        }
    }
    pub(crate) fn claim(
        &self,
        runtime: RuntimeIdentity,
        index: SlotIndex,
    ) -> Result<(WorkHandle, QueuedSlot), SlotClaimError> {
        let observed = PackedSlotState(self.state.load(Ordering::Acquire));
        let prior_epoch = observed.epoch();
        if prior_epoch == MAX_EPOCH {
            return Err(SlotClaimError::EpochExhausted { epoch: prior_epoch });
        }
        let handle = WorkHandle::new(runtime, index, prior_epoch + 1);
        // `WorkPermit` is the unique linear proof that this exact slot has been released. No
        // other producer can mutate it between the epoch read and this publication.
        self.state.store(
            PackedSlotState::new(handle.epoch(), SlotStatus::Queued).raw(),
            Ordering::Release,
        );
        Ok((handle, QueuedSlot { handle }))
    }

    pub(crate) fn cancel(&self, handle: WorkHandle) -> CancelResult {
        let expected = PackedSlotState::new(handle.epoch(), SlotStatus::Queued);
        match self.state.compare_exchange(
            expected.raw(),
            PackedSlotState::new(handle.epoch(), SlotStatus::Cancelled).raw(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => CancelResult::Cancelled,
            Err(observed)
                if observed
                    == PackedSlotState::new(handle.epoch(), SlotStatus::Cancelled).raw() =>
            {
                CancelResult::AlreadyCancelled
            }
            Err(_) => CancelResult::NotQueued,
        }
    }

    /// Claims a unique ready coordinate in one exchange; cancellation is its only racing edge.
    pub(crate) fn dequeue(&self, queued: &QueuedSlot) -> OwnedSlot {
        let observed = PackedSlotState(self.state.swap(
            PackedSlotState::new(queued.handle.epoch(), SlotStatus::InFlight).raw(),
            Ordering::AcqRel,
        ));
        let cancelled = observed.raw()
            == PackedSlotState::new(queued.handle.epoch(), SlotStatus::Cancelled).raw();
        OwnedSlot {
            handle: queued.handle,
            disposition: if cancelled {
                DequeuedDisposition::Cancelled
            } else {
                DequeuedDisposition::InFlight
            },
        }
    }

    pub(crate) fn retained_payload(&self) -> Option<RetainedPayload> {
        let observed = PackedSlotState(self.state.load(Ordering::Acquire));
        match observed.status_bits() {
            status
                if status == SlotStatus::Queued.bits()
                    || status == SlotStatus::Cancelled.bits() =>
            {
                Some(RetainedPayload::Queued)
            }
            status if status == SlotStatus::Terminal.bits() => Some(RetainedPayload::Terminal),
            _ => None,
        }
    }

    /// Marks a completed payload as terminal-retained until the single owner observes it.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "consuming the non-copy owner proof closes the in-flight lifecycle exactly once"
    )]
    pub(crate) fn terminalize(&self, owned: OwnedSlot) {
        self.state.store(
            PackedSlotState::new(owned.handle.epoch(), SlotStatus::Terminal).raw(),
            Ordering::Release,
        );
    }

    /// Returns a terminal-retained coordinate to FREE after its in-place event moved out.
    pub(crate) fn release_terminal(&self, handle: WorkHandle) {
        self.state.store(
            PackedSlotState::new(handle.epoch(), SlotStatus::Free).raw(),
            Ordering::Release,
        );
    }

    #[cfg(all(test, not(feature = "loom-model")))]
    pub(crate) fn exhaust_free_for_test(&self) {
        self.state.store(
            PackedSlotState::new(MAX_EPOCH, SlotStatus::Free).raw(),
            Ordering::Release,
        );
    }

    #[cfg(all(test, feature = "loom-model"))]
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the Loom-selected atomic constructor is not const"
    )]
    fn from_state(state: PackedSlotState) -> Self {
        Self {
            state: AtomicU64::new(state.raw()),
        }
    }
}

#[cfg(all(test, feature = "loom-model"))]
mod tests {
    use core::{mem::size_of, num::NonZeroU64};

    use loom::{sync::Arc, thread};
    use thiserror::Error;

    use super::{
        CancelResult, MAX_EPOCH, PackedSlotState, RuntimeIdentity, SlotClaimError, SlotCore,
        SlotIndex, SlotStatus, WorkHandle,
    };

    #[test]
    fn slot_index_has_a_one_byte_option_niche() {
        assert_eq!(size_of::<SlotIndex>(), 1);
        assert_eq!(size_of::<Option<SlotIndex>>(), 1);
        assert_eq!(size_of::<WorkHandle>(), 16);
    }

    #[test]
    fn loom_claim_cancel_dequeue_release_and_reclaim_are_aba_safe() {
        loom::model(|| crate::server::test_report::assert_loom(loom_transition));
    }
    #[test]
    fn loom_epoch_exhaustion_never_wraps_to_an_aba_epoch() {
        loom::model(|| crate::server::test_report::assert_loom(exhaustion_transition));
    }

    fn exhaustion_transition() -> Result<(), SlotTestError> {
        let slot = SlotCore::from_state(PackedSlotState::new(MAX_EPOCH, SlotStatus::Free));
        match slot.claim(RuntimeIdentity(1), first_slot()) {
            Err(SlotClaimError::EpochExhausted { epoch }) => {
                assert_eq!(epoch, MAX_EPOCH);
                Ok(())
            }
            Ok((handle, _)) => Err(SlotTestError::UnexpectedClaim { handle }),
        }
    }

    fn loom_transition() -> Result<(), SlotTestError> {
        let slot = Arc::new(SlotCore::new());
        let (handle, queued) = slot
            .claim(RuntimeIdentity(7), first_slot())
            .map_err(SlotTestError::InitialClaim)?;
        let cancellation_slot = Arc::clone(&slot);
        let cancellation = thread::spawn(move || cancellation_slot.cancel(handle));
        let owned = slot.dequeue(&queued);
        let cancelled = crate::server::test_report::join(cancellation.join())?;
        match cancelled {
            CancelResult::Cancelled | CancelResult::NotQueued => {}
            observed => return Err(SlotTestError::CancelResult { observed }),
        }
        if owned.was_cancelled() != matches!(cancelled, CancelResult::Cancelled) {
            return Err(SlotTestError::CancelDisposition {
                cancel: cancelled,
                dequeued_cancelled: owned.was_cancelled(),
            });
        }
        slot.terminalize(owned);
        slot.release_terminal(handle);
        let (next, _) = slot
            .claim(RuntimeIdentity(7), first_slot())
            .map_err(SlotTestError::Reclaim)?;
        if handle.epoch() == next.epoch() {
            return Err(SlotTestError::EpochReused { handle, next });
        }
        let stale = slot.cancel(handle);
        if stale != CancelResult::NotQueued {
            return Err(SlotTestError::StaleCancellation { observed: stale });
        }
        Ok(())
    }

    fn first_slot() -> SlotIndex {
        SlotIndex::from_ready_word(NonZeroU64::MIN)
    }

    #[derive(Debug, Error)]
    enum SlotTestError {
        #[error("initial production claim failed")]
        InitialClaim(#[source] SlotClaimError),
        #[error("the cancellation test thread panicked")]
        Join(#[from] crate::server::test_report::ThreadPanic),
        #[error("reclaimed production claim failed")]
        Reclaim(#[source] SlotClaimError),
        #[error("unexpected cancellation result {observed:?}")]
        CancelResult { observed: CancelResult },
        #[error("cancel result {cancel:?} disagreed with owner disposition {dequeued_cancelled}")]
        CancelDisposition {
            cancel: CancelResult,
            dequeued_cancelled: bool,
        },
        #[error("slot epoch was reused: {handle:?} -> {next:?}")]
        EpochReused {
            handle: WorkHandle,
            next: WorkHandle,
        },
        #[error("stale handle cancellation observed {observed:?}")]
        StaleCancellation { observed: CancelResult },
        #[error("an exhausted slot unexpectedly claimed {handle:?}")]
        UnexpectedClaim { handle: WorkHandle },
    }
}
