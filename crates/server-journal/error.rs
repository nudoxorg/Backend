//! Defines error behavior for `server-journal`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the error invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{fs::TryLockError, io};

use backend_store::workflow::{ReductionError, WorkflowRecord, WorkflowRecordError};
use thiserror::Error;

use crate::{FrameSequence, JournalOffset};

/// Physical file operation that failed while opening or recovering a journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalIoStep {
    Open,
    Create,
    Inspect,
    ReadHeader,
    WriteHeader,
    ReadFrame,
    RepairTail,
    OpenParentDirectory,
    SyncParentDirectory,
    SyncHeader,
    SyncTailRepair,
}

/// Physical operation whose failure makes an append outcome uncertain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitIoStep {
    Position,
    WriteFrame,
    SyncFrame,
}

/// Exact invalid-header diagnosis.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum HeaderError {
    #[error("journal header is truncated: required {required:?}, observed {actual:?}")]
    Truncated {
        required: JournalOffset,
        actual: JournalOffset,
    },
    #[error("journal magic is unknown: {observed:?}")]
    Magic { observed: [u8; 8] },
    #[error("journal physical version {observed} is unsupported")]
    PhysicalVersion { observed: u16 },
    #[error("journal declares header width {observed}, expected {expected}")]
    HeaderWidth { expected: u16, observed: u16 },
    #[error("journal declares workflow-record width {observed}, expected {expected}")]
    RecordWidth { expected: u16, observed: u16 },
    #[error("journal reserved field is nonzero: {observed}")]
    Reserved { observed: u16 },
    #[error("journal header checksum is invalid")]
    Checksum,
}

/// Exact journal open, recovery, or replay failure.
#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O failed during {step:?}")]
    Io {
        step: JournalIoStep,
        #[source]
        source: io::Error,
    },
    #[error("journal already has or cannot establish an exclusive physical owner")]
    ExclusiveOwnership(#[source] TryLockError),
    #[error(transparent)]
    Header(#[from] HeaderError),
    #[error("journal frame checksum is invalid at {offset:?} for {sequence:?}")]
    FrameChecksum {
        sequence: FrameSequence,
        offset: JournalOffset,
    },
    #[error("journal sequence expected {expected:?}, observed {observed:?}")]
    Sequence {
        expected: FrameSequence,
        observed: FrameSequence,
    },
    #[error("journal offset cannot represent {sequence:?}")]
    OffsetOverflow { sequence: FrameSequence },
    #[error("workflow record decode failed")]
    Decode(#[source] WorkflowRecordError),
    #[error("workflow replay reduction failed")]
    Reduction(#[source] ReductionError),
    #[error("poisoned journal must be reopened before replay")]
    Poisoned,
}

impl JournalError {
    pub(crate) const fn io(step: JournalIoStep, source: io::Error) -> Self {
        Self::Io { step, source }
    }
}

/// Append failure. Only [`Self::Reduction`] proves that no write was attempted.
#[derive(Debug, Error)]
pub enum CommitError {
    #[error("workflow reduction rejected the event")]
    Reduction(#[source] ReductionError),
    #[error("journal is poisoned; reopen it before reconciling")]
    Poisoned,
    #[error("journal receipt cannot represent {sequence:?}")]
    ReceiptOverflow { sequence: FrameSequence },
    #[error("journal append outcome is unknown for {sequence:?} during {step:?}")]
    OutcomeUnknown {
        attempted: WorkflowRecord,
        sequence: FrameSequence,
        step: CommitIoStep,
        #[source]
        source: io::Error,
    },
}
