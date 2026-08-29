//! Closed low-cardinality runtime observation vocabulary.
#![allow(
    missing_docs,
    reason = "the public probe vocabulary is closed and each type documents its aggregate boundary"
)]

use crate::{OwnerFault, OwnerProgress, RejectionReason, TerminalClass};

/// Outcome of one synchronous admission attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeAdmission {
    Admitted,
    Rejected(RejectionReason),
    SlotRetired,
    WaiterCapacity,
    WaiterRegistration,
}

/// Closed readiness-lane containment classification without work identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeContainment {
    WorkReadyContainedTerminal,
    TerminalReadyContainedWork,
}

impl From<OwnerFault> for RuntimeContainment {
    fn from(fault: OwnerFault) -> Self {
        match fault {
            OwnerFault::WorkReadyContainedTerminal { .. } => Self::WorkReadyContainedTerminal,
            OwnerFault::TerminalReadyContainedWork { .. } => Self::TerminalReadyContainedWork,
        }
    }
}

/// Outcome of one owner work-lane drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeExecution {
    Idle,
    Terminalized,
    Contained(RuntimeContainment),
}

impl From<Result<OwnerProgress, OwnerFault>> for RuntimeExecution {
    fn from(result: Result<OwnerProgress, OwnerFault>) -> Self {
        match result {
            Ok(OwnerProgress::Idle) => Self::Idle,
            Ok(OwnerProgress::Terminalized) => Self::Terminalized,
            Err(fault) => Self::Contained(fault.into()),
        }
    }
}

/// Outcome of one owner terminal-lane drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeTerminal {
    Idle,
    Observed(TerminalClass),
    Contained(RuntimeContainment),
}

/// One aggregate runtime boundary event; it never carries generations, handles, or payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeProbeEvent {
    Admission(RuntimeAdmission),
    Execution(RuntimeExecution),
    Terminal(RuntimeTerminal),
}
