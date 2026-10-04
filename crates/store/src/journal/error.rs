//! Defines error behavior for `backend-store`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the error invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{fs::TryLockError, io};

use crate::workflow::{ReductionError, WorkflowRecord, WorkflowRecordError};
use thiserror::Error;

use crate::journal::{FrameSequence, JournalOffset};

/// Physical file operation that failed while opening or recovering a journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalIoStep {
    /// Opening an existing journal file.
    Open,
    /// Creating a new journal file.
    Create,
    /// Inspecting the journal's physical length.
    Inspect,
    /// Reading the journal header.
    ReadHeader,
    /// Writing the initial journal header.
    WriteHeader,
    /// Reading a journal frame during recovery or replay.
    ReadFrame,
    /// Truncating an incomplete tail during recovery.
    RepairTail,
    /// Opening the parent directory needed for durability.
    OpenParentDirectory,
    /// Syncing the parent directory after journal creation.
    SyncParentDirectory,
    /// Syncing the journal header.
    SyncHeader,
    /// Syncing a repaired journal tail.
    SyncTailRepair,
}

/// Physical operation whose failure makes an append outcome uncertain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitIoStep {
    /// Positioning the file before appending a frame.
    Position,
    /// Writing the encoded frame.
    WriteFrame,
    /// Syncing the appended frame.
    SyncFrame,
}

/// Exact invalid-header diagnosis.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum HeaderError {
    #[error("journal header is truncated: required {required:?}, observed {actual:?}")]
    /// The available header bytes are shorter than the minimum header width.
    Truncated {
        /// Minimum header length required by this format.
        required: JournalOffset,
        /// Header length present in the file.
        actual: JournalOffset,
    },
    #[error("journal magic is unknown: {observed:?}")]
    /// The header's magic field does not identify this journal format.
    Magic {
        /// Eight-byte magic value read from the file.
        observed: [u8; 8],
    },
    #[error("journal physical version {observed} is unsupported")]
    /// The header names a physical format version this implementation does not read.
    PhysicalVersion {
        /// Physical format version found in the header.
        observed: u16,
    },
    #[error("journal declares header width {observed}, expected {expected}")]
    /// The stored header width differs from this implementation's width.
    HeaderWidth {
        /// Header width required by this implementation.
        expected: u16,
        /// Header width recorded in the file.
        observed: u16,
    },
    #[error("journal declares workflow-record width {observed}, expected {expected}")]
    /// The stored workflow-record width differs from this implementation's width.
    RecordWidth {
        /// Workflow-record width required by this implementation.
        expected: u16,
        /// Workflow-record width recorded in the file.
        observed: u16,
    },
    #[error("journal reserved field is nonzero: {observed}")]
    /// A field reserved by the format contains a nonzero value.
    Reserved {
        /// Unexpected value in the reserved header field.
        observed: u16,
    },
    /// The header checksum does not match its contents.
    #[error("journal header checksum is invalid")]
    Checksum,
}

/// Exact journal open, recovery, or replay failure.
#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O failed during {step:?}")]
    /// An operating-system I/O operation failed while opening, recovering, or replaying.
    Io {
        /// Physical journal operation that failed.
        step: JournalIoStep,
        /// Original operating-system error.
        #[source]
        source: io::Error,
    },
    /// The journal's exclusive file ownership could not be established.
    #[error("journal already has or cannot establish an exclusive physical owner")]
    ExclusiveOwnership(#[source] TryLockError),
    /// The journal header is invalid.
    #[error(transparent)]
    Header(#[from] HeaderError),
    #[error("journal frame checksum is invalid at {offset:?} for {sequence:?}")]
    /// A journal frame's stored checksum does not match its contents.
    FrameChecksum {
        /// Frame whose checksum failed validation.
        sequence: FrameSequence,
        /// Byte offset of the invalid frame.
        offset: JournalOffset,
    },
    #[error("journal sequence expected {expected:?}, observed {observed:?}")]
    /// A frame carries a sequence other than the next sequence required by replay.
    Sequence {
        /// Next sequence required by the journal.
        expected: FrameSequence,
        /// Sequence stored in the encountered frame.
        observed: FrameSequence,
    },
    #[error("journal offset cannot represent {sequence:?}")]
    /// The byte offset for a frame sequence cannot be represented by the journal offset type.
    OffsetOverflow {
        /// Sequence whose frame offset could not be represented.
        sequence: FrameSequence,
    },
    /// A frame did not decode as a workflow record.
    #[error("workflow record decode failed")]
    Decode(#[source] WorkflowRecordError),
    /// A decoded event could not be reduced into workflow state.
    #[error("workflow replay reduction failed")]
    Reduction(#[source] ReductionError),
    /// A prior uncertain append poisoned this in-memory journal owner.
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
    /// The event was rejected before any bytes were written.
    #[error("workflow reduction rejected the event")]
    Reduction(#[source] ReductionError),
    /// A prior uncertain append requires reopening the journal.
    #[error("journal is poisoned; reopen it before reconciling")]
    Poisoned,
    #[error("journal receipt cannot represent {sequence:?}")]
    /// The appended event's sequence cannot be represented in its commit receipt.
    ReceiptOverflow {
        /// Sequence whose receipt could not be represented.
        sequence: FrameSequence,
    },
    #[error("journal append outcome is unknown for {sequence:?} during {step:?}")]
    /// The append could not be confirmed; reopening is required to determine its durable state.
    OutcomeUnknown {
        /// Event whose append may have reached durable storage.
        attempted: WorkflowRecord,
        /// Frame sequence assigned to the attempted append.
        sequence: FrameSequence,
        /// Physical operation during which the outcome became uncertain.
        step: CommitIoStep,
        /// Original operating-system failure.
        #[source]
        source: io::Error,
    },
}
