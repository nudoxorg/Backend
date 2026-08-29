#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "closed adapter facts and errors are documented at their semantic boundaries"
)]

mod error;
mod format;
mod journal;

use core::ops::Deref;

pub use error::{CommitError, CommitIoStep, HeaderError, JournalError, JournalIoStep};
pub use format::{JOURNAL_FRAME_BYTES, JOURNAL_HEADER_BYTES};
pub use journal::FileJournal;

/// Physical ordinal of a committed journal frame.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FrameSequence(pub u64);

impl FrameSequence {
    pub const FIRST: Self = Self(0);

    pub(crate) const fn successor(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(next) => Some(Self(next)),
            None => None,
        }
    }
}

impl Deref for FrameSequence {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<u64> for FrameSequence {
    fn from(sequence: u64) -> Self {
        Self(sequence)
    }
}

/// Absolute byte position in the journal file.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct JournalOffset(pub u64);

impl Deref for JournalOffset {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<u64> for JournalOffset {
    fn from(offset: u64) -> Self {
        Self(offset)
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
/// use nudox_durable_journal::{FrameSequence, JournalOffset, ReceiptFacts, StableReceipt};
///
/// let _forged = StableReceipt {
///     facts: ReceiptFacts {
///         sequence: FrameSequence::FIRST,
///         durable_end: JournalOffset(124),
///     },
///     committed: (),
/// };
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableReceipt {
    facts: ReceiptFacts,
    committed: Committed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Committed;

impl StableReceipt {
    pub(crate) const fn committed(sequence: FrameSequence, durable_end: JournalOffset) -> Self {
        Self {
            facts: ReceiptFacts {
                sequence,
                durable_end,
            },
            committed: Committed,
        }
    }
}

impl Deref for StableReceipt {
    type Target = ReceiptFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}
