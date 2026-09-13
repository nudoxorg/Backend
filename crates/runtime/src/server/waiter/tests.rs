//! Defines waiter tests behavior for `backend_runtime::server`, whose purpose is to schedule bounded work with explicit credits, ownership, and wakeups.
//! This module owns the waiter tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{mem::size_of, num::NonZeroU64, sync::atomic::Ordering, task::Waker};

use loom::{sync::Arc, thread};
use thiserror::Error;

use super::{
    ACTIVE, ARMED, ArmResult, SlotIndex, WaiterRegistrationError, WaiterRegistry, WaiterSlot,
    WaiterStateCore, WaiterTable, encode,
};

#[test]
fn fixed_waiter_records_and_indexes_have_the_measured_layout() {
    assert_eq!(size_of::<WaiterSlot>(), 40);
    assert_eq!(size_of::<WaiterRegistry>(), 40);
}

#[test]
fn loom_register_arm_wake_cancel_reuse_keeps_generations_distinct() {
    loom::model(|| crate::server::test_report::assert_loom(loom_transition));
}

#[test]
fn loom_registry_release_then_reuse_preserves_the_new_armed_bit() {
    loom::model(|| crate::server::test_report::assert_loom(loom_registry_transition));
}

fn loom_registry_transition() -> Result<(), WaiterTestError> {
    let slot = SlotIndex::from_ready_word(NonZeroU64::MIN);
    let registry = Arc::new(WaiterRegistry::new(1).map_err(WaiterTestError::RegistryAllocation)?);
    let ticket = registry
        .register(0)
        .map_err(WaiterTestError::InitialRegistration)?;
    let initial_arm = registry.arm_observed(&ticket, Waker::noop());
    if initial_arm != ArmResult::Armed {
        return Err(WaiterTestError::InitialArm {
            observed: initial_arm,
        });
    }

    wake_then_rearm(&registry, slot, &ticket)?;
    let old_epoch = ticket.epoch;
    let next = release_then_reuse(Arc::clone(&registry), ticket)?;
    if next.epoch == old_epoch {
        return Err(WaiterTestError::EpochReused { epoch: next.epoch });
    }
    assert_reused_registration(&registry, slot, &next)
}

fn wake_then_rearm(
    registry: &WaiterRegistry,
    slot: SlotIndex,
    ticket: &super::WaiterTicket,
) -> Result<(), WaiterTestError> {
    registry.wake_fitting(registry.armed_snapshot(), usize::MAX);
    let wake_state = registry
        .slots
        .slot(slot)
        .state
        .state
        .load(Ordering::Acquire);
    if wake_state != encode(ticket.epoch, ACTIVE) {
        return Err(WaiterTestError::UnexpectedState {
            expected: encode(ticket.epoch, ACTIVE),
            observed: wake_state,
        });
    }
    let rearm = registry.arm_observed(ticket, Waker::noop());
    if rearm == ArmResult::Armed {
        Ok(())
    } else {
        Err(WaiterTestError::ReuseArm { observed: rearm })
    }
}

fn release_then_reuse(
    registry: Arc<WaiterRegistry>,
    ticket: super::WaiterTicket,
) -> Result<super::WaiterTicket, WaiterTestError> {
    let releasing_registry = Arc::clone(&registry);
    let release = thread::spawn(move || releasing_registry.release(&ticket));
    let reuse = thread::spawn(move || {
        loop {
            match registry.register(0) {
                Ok(next) => {
                    let arm = registry.arm_observed(&next, Waker::noop());
                    return if arm == ArmResult::Armed {
                        Ok(next)
                    } else {
                        Err(WaiterTestError::ReuseArm { observed: arm })
                    };
                }
                Err(WaiterRegistrationError::Capacity) => thread::yield_now(),
                Err(error) => return Err(WaiterTestError::ReuseRegistration(error)),
            }
        }
    });
    crate::server::test_report::join(release.join())?;
    crate::server::test_report::join(reuse.join())?
}

fn assert_reused_registration(
    registry: &WaiterRegistry,
    slot: SlotIndex,
    next: &super::WaiterTicket,
) -> Result<(), WaiterTestError> {
    let state = registry
        .slots
        .slot(slot)
        .state
        .state
        .load(Ordering::Acquire);
    if state != encode(next.epoch, ARMED) {
        return Err(WaiterTestError::UnexpectedState {
            expected: encode(next.epoch, ARMED),
            observed: state,
        });
    }
    if registry.armed_snapshot() & slot.mask() == 0 {
        return Err(WaiterTestError::ArmedBitMissing);
    }
    if registry.index.free_snapshot() & slot.mask() != 0 {
        return Err(WaiterTestError::ClaimedPermitRepublished);
    }
    Ok(())
}

fn loom_transition() -> Result<(), WaiterTestError> {
    let core = Arc::new(WaiterStateCore::new());
    let Some(epoch) = core
        .activate(0)
        .map_err(WaiterTestError::InitialActivation)?
    else {
        return Err(WaiterTestError::InitialSlotOccupied);
    };
    let arm = core.arm(epoch);
    if arm != ArmResult::Armed {
        return Err(WaiterTestError::InitialArm { observed: arm });
    }
    let other = Arc::clone(&core);
    let wake = thread::spawn(move || {
        let claimed = other.begin_wake();
        if let Some(claimed_epoch) = claimed {
            other.complete_wake(claimed_epoch);
        }
        claimed
    });
    let _released = core.release(epoch);
    if let Some(observed) = crate::server::test_report::join(wake.join())?
        && observed != epoch
    {
        return Err(WaiterTestError::WakeGeneration {
            expected: epoch,
            observed,
        });
    }
    let Some(next) = core.activate(0).map_err(WaiterTestError::ReuseActivation)? else {
        return Err(WaiterTestError::ReuseSlotOccupied);
    };
    if epoch == next {
        return Err(WaiterTestError::EpochReused { epoch });
    }
    let state = core.state.load(Ordering::Acquire);
    if state != encode(next, ACTIVE) {
        return Err(WaiterTestError::UnexpectedState {
            expected: encode(next, ACTIVE),
            observed: state,
        });
    }
    let arm = core.arm(next);
    if arm != ArmResult::Armed {
        return Err(WaiterTestError::ReuseArm { observed: arm });
    }
    let state = core.state.load(Ordering::Acquire);
    if state != encode(next, ARMED) {
        return Err(WaiterTestError::UnexpectedState {
            expected: encode(next, ARMED),
            observed: state,
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
enum WaiterTestError {
    #[error("initial waiter activation failed")]
    InitialActivation(#[source] WaiterRegistrationError),
    #[error("waiter registry allocation failed")]
    RegistryAllocation(#[source] alloc::collections::TryReserveError),
    #[error("initial waiter registration failed")]
    InitialRegistration(#[source] WaiterRegistrationError),
    #[error("initial waiter slot was unexpectedly occupied")]
    InitialSlotOccupied,
    #[error("initial arm observed {observed:?}")]
    InitialArm { observed: ArmResult },
    #[error("waker thread panicked")]
    Join(#[from] crate::server::test_report::ThreadPanic),
    #[error("wake claimed generation {observed}, expected {expected}")]
    WakeGeneration { expected: u64, observed: u64 },
    #[error("reused waiter activation failed")]
    ReuseActivation(#[source] WaiterRegistrationError),
    #[error("reused waiter slot remained occupied")]
    ReuseSlotOccupied,
    #[error("waiter epoch {epoch} was reused")]
    EpochReused { epoch: u64 },
    #[error("expected packed waiter state {expected:#x}, observed {observed:#x}")]
    UnexpectedState { expected: u64, observed: u64 },
    #[error("reused arm observed {observed:?}")]
    ReuseArm { observed: ArmResult },
    #[error("an armed waiter lost its compact armed-index bit")]
    ArmedBitMissing,
    #[error("reused waiter registration failed")]
    ReuseRegistration(#[source] WaiterRegistrationError),
    #[error("a claimed waiter permit was republished")]
    ClaimedPermitRepublished,
}
