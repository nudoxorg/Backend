//! Defines log behavior for `server-workflow`, whose purpose is to reduce durable workflow events into deterministic recovery state.
//! This module owns the log invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Bounded in-memory reducer harness for tests and local diagnostics.
#![allow(
    missing_docs,
    reason = "the log contract documents its small error payload fields"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "typed log errors are directly named by each durable API"
)]

use alloc::vec::Vec;
use core::ops::Deref;
use heart_observe::Probe;
use thiserror::Error;

use crate::{
    Recovery, Reduction, ReductionError, WorkflowDisposition, WorkflowEvent, WorkflowProbeEvent,
    WorkflowRejection, WorkflowState, reduce,
};

/// Bounded in-memory event log. This type does not claim durable commit semantics.
#[derive(Debug, Eq, PartialEq)]
pub struct MemoryWorkflowLog {
    events: Vec<WorkflowEvent>,
    capacity: usize,
    state: WorkflowState,
}

impl Deref for MemoryWorkflowLog {
    type Target = [WorkflowEvent];

    fn deref(&self) -> &Self::Target {
        &self.events
    }
}

/// Log creation failure.
#[derive(Debug, Error)]
pub enum LogConfigError {
    #[error("workflow log capacity must be non-zero")]
    ZeroCapacity,
    #[error("workflow log reservation failed")]
    Allocation {
        /// Exact allocator error retained for callers that can recover or report it.
        #[source]
        source: alloc::collections::TryReserveError,
    },
}

/// Failure from append-then-reduce. Effects are never returned on either error branch.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CommitError {
    #[error("workflow reduction rejected the event")]
    Reduction(#[source] ReductionError),
    #[error("workflow log is full before event can commit")]
    Full { event: WorkflowEvent },
}

impl MemoryWorkflowLog {
    /// Reserves fixed log retention before the first durable event.
    pub fn new(capacity: usize) -> Result<Self, LogConfigError> {
        if capacity == 0 {
            return Err(LogConfigError::ZeroCapacity);
        }
        let mut events = Vec::new();
        match events.try_reserve_exact(capacity) {
            Ok(()) => Ok(Self {
                events,
                capacity,
                state: WorkflowState::empty(),
            }),
            Err(source) => Err(LogConfigError::Allocation { source }),
        }
    }
    /// Appends and reduces one event in memory.
    pub fn append_then_reduce(&mut self, event: WorkflowEvent) -> Result<Reduction, CommitError> {
        let reduction = reduce(self.state, event).map_err(CommitError::Reduction)?;
        if self.events.len() == self.capacity {
            return Err(CommitError::Full { event });
        }
        self.events.push(event);
        self.state = reduction.state;
        Ok(reduction)
    }
    /// Appends one trigger and lazily records the exact aggregate commit/reduction outcome.
    pub fn append_then_reduce_with_probe<Observation>(
        &mut self,
        event: WorkflowEvent,
        probe: &mut Observation,
    ) -> Result<Reduction, CommitError>
    where
        Observation: Probe<WorkflowProbeEvent>,
    {
        let from = self.state.phase();
        let result = self.append_then_reduce(event);
        let disposition = match result {
            Ok(reduction) => WorkflowDisposition::Accepted {
                to: reduction.state.phase(),
            },
            Err(CommitError::Reduction(error)) => WorkflowDisposition::Rejected(error.into()),
            Err(CommitError::Full { .. }) => {
                WorkflowDisposition::Rejected(WorkflowRejection::LogFull)
            }
        };
        probe.record_with(|| WorkflowProbeEvent {
            from,
            event: event.kind.name(),
            disposition,
        });
        result
    }
    /// Reconstructs state and the idempotent command pending after a crash at any committed prefix.
    pub fn replay(&self) -> Result<Recovery, ReductionError> {
        let mut state = WorkflowState::empty();
        for event in &self.events {
            state = reduce(state, *event)?.state;
        }
        Ok(Recovery::from_state(state))
    }
}
