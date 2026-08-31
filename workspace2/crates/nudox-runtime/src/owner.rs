//! Single-owner execution, exact terminal retention, and credit reclamation.
#![allow(
    missing_docs,
    reason = "the public terminal and fault vocabularies are closed"
)]
#![allow(
    clippy::indexing_slicing,
    clippy::missing_errors_doc,
    reason = "private permits originate from this fixed slot table"
)]

use crate::metrics::{Accounting, TerminalClass};
use crate::{
    RuntimeExecution, RuntimeProbeEvent, RuntimeTerminal,
    admission_bundle::DequeuedWork,
    budget::BoundedWork,
    runtime::Fabric,
    slot::{OwnedSlot, WorkHandle},
    storage::{RuntimePayloadTable, RuntimeStorage},
};
use nudox_observe::Probe;
use thiserror::Error;

/// Retained terminal outcome. Ordinary execution failure carries its concrete callback cause.
#[derive(Debug, Eq, PartialEq)]
pub enum TerminalOutcome<Failure> {
    Completed,
    Failed {
        failure: Failure,
    },
    Cancelled,
    StaleGeneration,
    /// The callback unwound; runtime resources were committed before unwinding continued.
    ExecutorUnwound,
}

impl<Failure> TerminalOutcome<Failure> {
    const fn class(&self) -> TerminalClass {
        match self {
            Self::Completed => TerminalClass::Completed,
            Self::Failed { .. } => TerminalClass::Failed,
            Self::Cancelled => TerminalClass::Cancelled,
            Self::StaleGeneration => TerminalClass::StaleGeneration,
            Self::ExecutorUnwound => TerminalClass::ExecutorUnwound,
        }
    }
}

/// Exact result of one owner drain attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerProgress {
    Idle,
    Terminalized,
}

/// Exact containment fault when a readiness capability encounters the other owned payload class.
///
/// The runtime restores the payload and republishes it through its correct readiness lane before
/// returning this diagnostic, so no semantic work or physical credit is lost.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum OwnerFault {
    #[error("work readiness selected terminal payload {handle:?}; payload was contained")]
    WorkReadyContainedTerminal { handle: WorkHandle },
    #[error("terminal readiness selected queued payload {handle:?}; payload was contained")]
    TerminalReadyContainedWork { handle: WorkHandle },
}

/// Owner-observed terminal fact.
#[derive(Debug, Eq, PartialEq)]
pub struct TerminalEvent<Generation, Failure> {
    pub handle: WorkHandle,
    pub generation: Generation,
    pub outcome: TerminalOutcome<Failure>,
}

/// An in-place terminal record stored in its original payload cell until observed.
pub(crate) struct TerminalRecord<Generation, Failure> {
    pub(crate) event: TerminalEvent<Generation, Failure>,
    pub(crate) permit: crate::work_permit::WorkPermit,
}

/// Returns a dequeued linear capability on unwind as an executor-unwound terminal.
struct CompletionGuard<
    'runtime,
    Generation,
    Work,
    Failure,
    AccountingPolicy: Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> {
    fabric: &'runtime Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
    phase: CompletionPhase<Generation, Work>,
}

enum CompletionPhase<Generation, Work> {
    Pending {
        dequeued: DequeuedWork<Generation, Work>,
        owned: OwnedSlot,
    },
    Finished(OwnerProgress),
}

/// The one executor-free owner paired with scoped [`Admission`](crate::Admission).
pub struct Owner<
    'runtime,
    Generation,
    Work,
    Failure = core::convert::Infallible,
    AccountingPolicy = (),
    StoragePolicy: RuntimeStorage<Generation, Work, Failure> = crate::RemoteStorage,
> {
    pub(crate) fabric: &'runtime Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
}

impl<
    Generation,
    Work: BoundedWork,
    Failure,
    AccountingPolicy: Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> Owner<'_, Generation, Work, Failure, AccountingPolicy, StoragePolicy>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    /// Runs one current-generation item synchronously and retains its exact terminal fact.
    ///
    /// A normal callback failure becomes `TerminalOutcome::Failed { failure }`; it is not erased
    /// into a closed terminal tag. A callback panic remains a panic. The unwind guard commits the
    /// `ExecutorUnwound` terminal and all runtime credits before that panic continues.
    pub fn execute_next(
        &mut self,
        current_generation: &Generation,
        execute: impl FnOnce(&mut Work) -> Result<(), Failure>,
    ) -> Result<OwnerProgress, OwnerFault>
    where
        Generation: Eq,
    {
        let Some(index) = self.fabric.ready.take() else {
            return Ok(OwnerProgress::Idle);
        };
        let (dequeued, owned) = match self.fabric.payload_slot(index).begin_owner() {
            crate::payload_slot::OwnerPayload::Queued(dequeued, owned) => (dequeued, owned),
            crate::payload_slot::OwnerPayload::Terminal(terminal) => {
                let handle = terminal.event.handle;
                self.fabric.payload_slot(index).restore_terminal(terminal);
                self.fabric.terminal_ready.publish(index);
                return Err(OwnerFault::WorkReadyContainedTerminal { handle });
            }
        };
        let mut completion = CompletionGuard::new(self.fabric, dequeued, owned);
        Ok(completion.run(current_generation, execute))
    }

    /// Drains one work lane and lazily records its aggregate outcome.
    pub fn execute_next_with_probe<Observation>(
        &mut self,
        current_generation: &Generation,
        execute: impl FnOnce(&mut Work) -> Result<(), Failure>,
        probe: &mut Observation,
    ) -> Result<OwnerProgress, OwnerFault>
    where
        Generation: Eq,
        Observation: Probe<RuntimeProbeEvent>,
    {
        let result = self.execute_next(current_generation, execute);
        let outcome = RuntimeExecution::from(result);
        probe.record_with(|| RuntimeProbeEvent::Execution(outcome));
        result
    }

    /// Observes one in-place terminal and restores or retires that exact work coordinate.
    ///
    /// Terminal reports are returned by physical-coordinate readiness, not callback completion
    /// order. Callers needing a stronger ordering rule must carry and order their own sequence.
    pub fn poll_terminal(
        &mut self,
    ) -> Result<Option<TerminalEvent<Generation, Failure>>, OwnerFault> {
        let Some(index) = self.fabric.terminal_ready.take() else {
            return Ok(None);
        };
        let (event, permit) = match self.fabric.payload_slot(index).take_terminal() {
            crate::payload_slot::TerminalPayload::Terminal(TerminalRecord { event, permit }) => {
                (event, permit)
            }
            crate::payload_slot::TerminalPayload::Queued(queued) => {
                let handle = queued.handle;
                self.fabric.payload_slot(index).restore_queued(queued);
                self.fabric.ready.publish(index);
                return Err(OwnerFault::TerminalReadyContainedWork { handle });
            }
        };
        self.fabric.work_permits.restore(permit);
        self.fabric.wake_ready_waiters();
        Ok(Some(event))
    }

    /// Drains one terminal lane and lazily records its aggregate outcome.
    pub fn poll_terminal_with_probe<Observation>(
        &mut self,
        probe: &mut Observation,
    ) -> Result<Option<TerminalEvent<Generation, Failure>>, OwnerFault>
    where
        Observation: Probe<RuntimeProbeEvent>,
    {
        let result = self.poll_terminal();
        let outcome = match &result {
            Ok(Some(event)) => RuntimeTerminal::Observed(event.outcome.class()),
            Ok(None) => RuntimeTerminal::Idle,
            Err(fault) => RuntimeTerminal::Contained((*fault).into()),
        };
        probe.record_with(|| RuntimeProbeEvent::Terminal(outcome));
        result
    }
}

impl<
    'runtime,
    Generation,
    Work,
    Failure,
    AccountingPolicy: Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> CompletionGuard<'runtime, Generation, Work, Failure, AccountingPolicy, StoragePolicy>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    const fn new(
        fabric: &'runtime Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
        queued: DequeuedWork<Generation, Work>,
        owned: OwnedSlot,
    ) -> Self {
        Self {
            fabric,
            phase: CompletionPhase::Pending {
                dequeued: queued,
                owned,
            },
        }
    }

    fn run(
        &mut self,
        current_generation: &Generation,
        callback: impl FnOnce(&mut Work) -> Result<(), Failure>,
    ) -> OwnerProgress
    where
        Generation: Eq,
    {
        let outcome = match &mut self.phase {
            CompletionPhase::Pending { owned, .. } if owned.was_cancelled() => {
                TerminalOutcome::Cancelled
            }
            CompletionPhase::Pending { dequeued, .. }
                if &dequeued.generation != current_generation =>
            {
                TerminalOutcome::StaleGeneration
            }
            CompletionPhase::Pending { dequeued, .. } => match callback(&mut dequeued.work) {
                Ok(()) => TerminalOutcome::Completed,
                Err(failure) => TerminalOutcome::Failed { failure },
            },
            // The only caller owns this guard until `run` returns. This arm makes a repeated
            // internal invocation coherent rather than manufacturing a false customer failure.
            CompletionPhase::Finished(progress) => return *progress,
        };
        self.terminalize(outcome)
    }

    fn terminalize(&mut self, outcome: TerminalOutcome<Failure>) -> OwnerProgress {
        let progress = OwnerProgress::Terminalized;
        let phase = core::mem::replace(&mut self.phase, CompletionPhase::Finished(progress));
        if let CompletionPhase::Pending { dequeued, owned } = phase {
            finish(self.fabric, dequeued, owned, outcome);
        }
        progress
    }
}

impl<
    Generation,
    Work,
    Failure,
    AccountingPolicy: Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
> Drop for CompletionGuard<'_, Generation, Work, Failure, AccountingPolicy, StoragePolicy>
where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    fn drop(&mut self) {
        let progress = OwnerProgress::Terminalized;
        let phase = core::mem::replace(&mut self.phase, CompletionPhase::Finished(progress));
        if let CompletionPhase::Pending { dequeued, owned } = phase {
            finish(
                self.fabric,
                dequeued,
                owned,
                TerminalOutcome::ExecutorUnwound,
            );
        }
    }
}

fn finish<
    Generation,
    Work,
    Failure,
    AccountingPolicy: Accounting,
    StoragePolicy: RuntimeStorage<Generation, Work, Failure>,
>(
    fabric: &Fabric<Generation, Work, Failure, AccountingPolicy, StoragePolicy>,
    queued: DequeuedWork<Generation, Work>,
    owned: OwnedSlot,
    outcome: TerminalOutcome<Failure>,
) where
    StoragePolicy::Payloads: RuntimePayloadTable<Generation, Work, Failure>,
{
    let completed = queued.complete(outcome);
    fabric.byte_credits.release(completed.byte_reservation);
    fabric.accounting.terminal(completed.event.outcome.class());
    let index = completed.permit.index;
    fabric.payload_slot(index).retain_terminal(
        owned,
        TerminalRecord {
            event: completed.event,
            permit: completed.permit,
        },
    );
    fabric.terminal_ready.publish(index);
    // Work destruction is outside runtime recovery semantics. The terminal and every runtime
    // resource above are already committed; a callback panic is guarded as `ExecutorUnwound`,
    // while a second destructor panic during unwinding is Rust's documented abort boundary.
    drop(completed.work);
}
