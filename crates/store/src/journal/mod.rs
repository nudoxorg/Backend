//! The `crate::journal` module persists and recovers generation publication
//! with bounded ownership.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
mod error;
mod format;
mod journal;
mod publication;

use core::ops::Deref;

pub use error::{CommitError, CommitIoStep, HeaderError, JournalError, JournalIoStep};
pub use format::{JOURNAL_FRAME_BYTES, JOURNAL_HEADER_BYTES};
pub use journal::FileJournal;
#[cfg(not(all(test, feature = "loom-model")))]
pub use publication::{
    ArtifactName, CancelError, DurablePublisher, ImmutablePublicationIdentity,
    MAX_PUBLICATION_OWNER_PANIC_BYTES, PendingPublication, PublicationConflict, PublicationError,
    PublicationFacts, PublicationFailure, PublicationGenerationError, PublicationHeadIdentity,
    PublicationIoStep, PublicationLimitError, PublicationLimits, PublicationOpenError,
    PublicationOwnerPanic, PublicationOwnerPanicClass, PublicationOwnerPanicMessage,
    PublicationPaths, PublicationSnapshotError, PublicationStateConflict, PublishedGeneration,
    SharedCommitError, SharedPublicationFailure, ShutdownError, SubmitError,
};

/// Physical ordinal of a committed journal frame.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FrameSequence {
    value: u64,
}

impl FrameSequence {
    pub const FIRST: Self = Self { value: 0 };

    pub(crate) const fn successor(self) -> Option<Self> {
        match self.value.checked_add(1) {
            Some(value) => Some(Self { value }),
            None => None,
        }
    }
}

impl Deref for FrameSequence {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl From<u64> for FrameSequence {
    fn from(sequence: u64) -> Self {
        Self { value: sequence }
    }
}

/// Absolute byte position in the journal file.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct JournalOffset {
    value: u64,
}

impl Deref for JournalOffset {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl From<u64> for JournalOffset {
    fn from(offset: u64) -> Self {
        Self { value: offset }
    }
}

/// Readable facts carried by an unforgeable stable receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiptFacts {
    pub sequence: FrameSequence,
    pub durable_end: JournalOffset,
}

/// Proof that one complete frame passed the adapter's file-sync boundary.
///
/// The readable facts dereference directly, while the proof itself cannot be fabricated.
///
/// ```compile_fail
/// use crate::journal::{FrameSequence, JournalOffset, ReceiptFacts, StableReceipt};
///
/// let _forged = StableReceipt {
///     facts: ReceiptFacts {
///         sequence: FrameSequence::FIRST,
///         durable_end: JournalOffset::from(124),
///     },
/// };
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableReceipt {
    facts: ReceiptFacts,
}

impl StableReceipt {
    pub(crate) const fn committed(sequence: FrameSequence, durable_end: JournalOffset) -> Self {
        Self {
            facts: ReceiptFacts {
                sequence,
                durable_end,
            },
        }
    }
}

impl Deref for StableReceipt {
    type Target = ReceiptFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}
