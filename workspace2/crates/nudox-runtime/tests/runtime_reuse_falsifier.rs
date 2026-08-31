//! Production slot reuse journey for stale handles and payload destruction.
#![allow(
    clippy::result_large_err,
    reason = "the cold typed failure retains both exact runtime metric snapshots without allocation or error erasure"
)]

use core::convert::Infallible;
use std::rc::Rc;

use nudox_runtime::{
    AdmissionError, BoundedWork, ByteBudget, ByteBudgetError, ByteQuantum, CancelResult,
    OwnerFault, OwnerProgress, RemoteRuntime, RetainedBytes, RuntimeConfigError, RuntimeMetrics,
    TerminalOutcome, WorkHandle,
};
use std::cell::Cell;
use thiserror::Error;

const WORK_BYTES: usize = 1;
const INITIAL_GENERATION: u8 = 1;
const CURRENT_GENERATION: u8 = 1;
const STALE_GENERATION: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReusePhase {
    FirstCancellation,
    FirstTerminal,
    SecondStaleCancellation,
    SecondTerminal,
    ThirdTerminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DropCheckpoint {
    FirstTerminal,
    SecondTerminal,
    ThirdTerminal,
    QueuedRuntimeDrop,
}

impl DropCheckpoint {
    const fn expected(self) -> usize {
        match self {
            Self::FirstTerminal => 1,
            Self::SecondTerminal => 2,
            Self::ThirdTerminal => 3,
            Self::QueuedRuntimeDrop => 4,
        }
    }
}

#[derive(Debug)]
struct DropCounter {
    count: Rc<Cell<usize>>,
}

impl DropCounter {
    fn new() -> Self {
        Self {
            count: Rc::new(Cell::new(0)),
        }
    }

    fn work(&self) -> DropWork {
        DropWork {
            bytes: RetainedBytes::from(WORK_BYTES),
            drops: Rc::clone(&self.count),
        }
    }
}

#[derive(Debug)]
struct DropWork {
    bytes: RetainedBytes,
    drops: Rc<Cell<usize>>,
}

impl BoundedWork for DropWork {
    fn retained_bytes(&self) -> RetainedBytes {
        self.bytes
    }
}

impl Drop for DropWork {
    fn drop(&mut self) {
        self.drops.set(self.drops.get().saturating_add(1));
    }
}

#[derive(Debug, Error)]
enum ReuseError {
    #[error("byte budget setup failed")]
    Budget(#[from] ByteBudgetError),
    #[error("runtime setup failed")]
    Runtime(#[from] RuntimeConfigError),
    #[error("admission failed")]
    Admission(#[from] AdmissionError<DropWork>),
    #[error("owner containment failed")]
    Owner(#[from] OwnerFault),
    #[error("runtime metrics were not conserved: observed {observed:?}, expected {expected:?}")]
    Metrics {
        observed: RuntimeMetrics,
        expected: RuntimeMetrics,
    },
    #[error("{phase:?} did not terminalize work: {observed:?}")]
    Progress {
        phase: ReusePhase,
        observed: OwnerProgress,
    },
    #[error("{phase:?} had no retained terminal")]
    MissingTerminal { phase: ReusePhase },
    #[error("{phase:?} returned {observed:?}, expected {expected:?}")]
    Cancellation {
        phase: ReusePhase,
        observed: CancelResult,
        expected: CancelResult,
    },
    #[error("{phase:?} returned terminal outcome {observed:?}, expected {expected:?}")]
    Terminal {
        phase: ReusePhase,
        observed: TerminalOutcome<Infallible>,
        expected: TerminalOutcome<Infallible>,
    },
    #[error("{phase:?} reported handle {observed:?}, expected {expected:?}")]
    Handle {
        phase: ReusePhase,
        observed: WorkHandle,
        expected: WorkHandle,
    },
    #[error("{phase:?} reported generation {observed}, expected {expected}")]
    Generation {
        phase: ReusePhase,
        observed: u8,
        expected: u8,
    },
    #[error("{checkpoint:?} observed {observed} drops, expected {expected}")]
    Drops {
        checkpoint: DropCheckpoint,
        observed: usize,
        expected: usize,
    },
}

fn budget() -> Result<ByteBudget, ByteBudgetError> {
    ByteBudget::new(
        RetainedBytes::from(WORK_BYTES),
        ByteQuantum::try_from(WORK_BYTES)?,
    )
}

fn require_progress(phase: ReusePhase, observed: OwnerProgress) -> Result<(), ReuseError> {
    if observed == OwnerProgress::Terminalized {
        Ok(())
    } else {
        Err(ReuseError::Progress { phase, observed })
    }
}

fn require_terminal(
    owner: &mut nudox_runtime::Owner<'_, u8, DropWork>,
    phase: ReusePhase,
    expected_handle: WorkHandle,
    expected_generation: u8,
    expected_outcome: TerminalOutcome<Infallible>,
) -> Result<(), ReuseError> {
    let event = owner
        .poll_terminal()?
        .ok_or(ReuseError::MissingTerminal { phase })?;
    if event.handle != expected_handle {
        return Err(ReuseError::Handle {
            phase,
            observed: event.handle,
            expected: expected_handle,
        });
    }
    if event.generation != expected_generation {
        return Err(ReuseError::Generation {
            phase,
            observed: event.generation,
            expected: expected_generation,
        });
    }
    if event.outcome != expected_outcome {
        return Err(ReuseError::Terminal {
            phase,
            observed: event.outcome,
            expected: expected_outcome,
        });
    }
    Ok(())
}

fn require_drops(counter: &DropCounter, checkpoint: DropCheckpoint) -> Result<(), ReuseError> {
    let observed = counter.count.get();
    let expected = checkpoint.expected();
    if observed == expected {
        Ok(())
    } else {
        Err(ReuseError::Drops {
            checkpoint,
            observed,
            expected,
        })
    }
}

fn first_cycle(
    admission: nudox_runtime::Admission<'_, u8, DropWork>,
    owner: &mut nudox_runtime::Owner<'_, u8, DropWork>,
    counter: &DropCounter,
) -> Result<WorkHandle, ReuseError> {
    let handle = admission.admit(INITIAL_GENERATION, counter.work())?;
    let observed = admission.cancel_queued(handle);
    if observed != CancelResult::Cancelled {
        return Err(ReuseError::Cancellation {
            phase: ReusePhase::FirstCancellation,
            observed,
            expected: CancelResult::Cancelled,
        });
    }
    require_progress(
        ReusePhase::FirstCancellation,
        owner.execute_next(&CURRENT_GENERATION, |_| Ok::<(), Infallible>(()))?,
    )?;
    require_terminal(
        owner,
        ReusePhase::FirstTerminal,
        handle,
        INITIAL_GENERATION,
        TerminalOutcome::Cancelled,
    )?;
    require_drops(counter, DropCheckpoint::FirstTerminal)?;
    Ok(handle)
}

fn second_cycle(
    admission: nudox_runtime::Admission<'_, u8, DropWork>,
    owner: &mut nudox_runtime::Owner<'_, u8, DropWork>,
    counter: &DropCounter,
    stale_handle: WorkHandle,
) -> Result<(), ReuseError> {
    let handle = admission.admit(INITIAL_GENERATION, counter.work())?;
    let observed = admission.cancel_queued(stale_handle);
    if observed != CancelResult::NotQueued {
        return Err(ReuseError::Cancellation {
            phase: ReusePhase::SecondStaleCancellation,
            observed,
            expected: CancelResult::NotQueued,
        });
    }
    require_progress(
        ReusePhase::SecondStaleCancellation,
        owner.execute_next(&CURRENT_GENERATION, |_| Ok::<(), Infallible>(()))?,
    )?;
    require_terminal(
        owner,
        ReusePhase::SecondTerminal,
        handle,
        INITIAL_GENERATION,
        TerminalOutcome::Completed,
    )?;
    require_drops(counter, DropCheckpoint::SecondTerminal)
}

fn third_cycle(
    admission: nudox_runtime::Admission<'_, u8, DropWork>,
    owner: &mut nudox_runtime::Owner<'_, u8, DropWork>,
    counter: &DropCounter,
) -> Result<(), ReuseError> {
    let handle = admission.admit(INITIAL_GENERATION, counter.work())?;
    require_progress(
        ReusePhase::ThirdTerminal,
        owner.execute_next(&STALE_GENERATION, |_| Ok::<(), Infallible>(()))?,
    )?;
    require_terminal(
        owner,
        ReusePhase::ThirdTerminal,
        handle,
        INITIAL_GENERATION,
        TerminalOutcome::StaleGeneration,
    )?;
    require_drops(counter, DropCheckpoint::ThirdTerminal)
}

#[test]
fn stale_handle_cannot_cancel_reused_slot_and_every_payload_drops_once() -> Result<(), ReuseError> {
    let counter = DropCounter::new();
    let mut runtime = RemoteRuntime::<u8, DropWork>::new(1, budget()?)?;

    {
        let (admission, mut owner) = runtime.split();
        let first = first_cycle(admission, &mut owner, &counter)?;
        second_cycle(admission, &mut owner, &counter, first)?;
        third_cycle(admission, &mut owner, &counter)?;
    }

    let expected_metrics = RuntimeMetrics {
        capacity: 1,
        active_capacity: 1,
        available: 1,
        checked_out: 0,
        reserved_bytes: 0,
        retired_work_slots: 0,
        terminal_occupied: 0,
    };
    let observed_metrics = runtime.metrics();
    if observed_metrics != expected_metrics {
        return Err(ReuseError::Metrics {
            observed: observed_metrics,
            expected: expected_metrics,
        });
    }

    {
        let (admission, _owner) = runtime.split();
        admission.admit(INITIAL_GENERATION, counter.work())?;
    }
    drop(runtime);
    require_drops(&counter, DropCheckpoint::QueuedRuntimeDrop)
}
