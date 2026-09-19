//! Journal records, identities, observations, and errors.

use super::admission::{HEADER_BYTES, JournalDomain};
use crate::schema::RecordId;
use std::fmt;
use std::io;
use std::marker::PhantomData;

/// A frame's domain-separated predecessor hash.
pub struct ChainHash<D> {
    bytes: [u8; 32],
    _domain: PhantomData<fn() -> D>,
}

impl<D> Copy for ChainHash<D> {}
impl<D> Clone for ChainHash<D> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<D> PartialEq for ChainHash<D> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl<D> Eq for ChainHash<D> {}
impl<D> std::hash::Hash for ChainHash<D> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}
impl<D> PartialOrd for ChainHash<D> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<D> Ord for ChainHash<D> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl<D> fmt::Debug for ChainHash<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ChainHash").field(&self.bytes).finish()
    }
}

impl<D> ChainHash<D> {
    /// The all-zero predecessor used for the first frame.
    #[must_use]
    pub const fn genesis() -> Self {
        Self {
            bytes: [0; 32],
            _domain: PhantomData,
        }
    }

    /// Creates a hash from bytes obtained from a validated frame.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            bytes,
            _domain: PhantomData,
        }
    }

    /// Returns the fixed-width hash.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

/// A validated physical journal frame.
pub struct JournalFrame<D: JournalDomain> {
    /// Monotonic frame number.
    pub sequence: u64,
    /// Hash of the previous frame.
    pub previous: ChainHash<D>,
    /// Content identity of the canonical record payload.
    pub record: RecordId<D>,
    /// Canonical payload bytes.
    pub payload: Box<[u8]>,
    /// Hash of this frame.
    pub chain: ChainHash<D>,
    /// Byte offset immediately following this frame.
    pub end_offset: u64,
}

impl<D: JournalDomain> JournalFrame<D> {
    /// Returns the byte offset at which this frame starts.
    #[must_use]
    pub fn start_offset(&self) -> u64 {
        self.end_offset
            .saturating_sub(u64::try_from(HEADER_BYTES).unwrap_or(u64::MAX))
            .saturating_sub(u64::try_from(self.payload.len()).unwrap_or(u64::MAX))
    }
}

impl<D: JournalDomain> Clone for JournalFrame<D> {
    fn clone(&self) -> Self {
        Self {
            sequence: self.sequence,
            previous: self.previous,
            record: self.record,
            payload: self.payload.clone(),
            chain: self.chain,
            end_offset: self.end_offset,
        }
    }
}
impl<D: JournalDomain> PartialEq for JournalFrame<D> {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence
            && self.previous == other.previous
            && self.record == other.record
            && self.payload == other.payload
            && self.chain == other.chain
            && self.end_offset == other.end_offset
    }
}
impl<D: JournalDomain> Eq for JournalFrame<D> {}
impl<D: JournalDomain> fmt::Debug for JournalFrame<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalFrame")
            .field("sequence", &self.sequence)
            .field("previous", &self.previous)
            .field("record", &self.record)
            .field("payload", &self.payload)
            .field("chain", &self.chain)
            .field("end_offset", &self.end_offset)
            .finish()
    }
}

/// Result of an append after its frame has been synced.
pub struct JournalReceipt<D> {
    /// Byte offset of the appended frame.
    pub offset: u64,
    /// Appended sequence.
    pub sequence: u64,
    /// New chain tip.
    pub chain: ChainHash<D>,
    /// Content identity of the record.
    pub record: RecordId<D>,
}

/// A durable frame identity that can be used as a restart checkpoint.
///
/// A checkpoint deliberately contains only fixed-width identity.  The frame
/// at `offset` is still read and checked before it is trusted; callers cannot
/// use this type to skip validation or to manufacture append state.
pub struct JournalCheckpoint<D> {
    /// Byte offset at which the checkpoint frame starts.
    pub offset: u64,
    /// Sequence carried by the checkpoint frame.
    pub sequence: u64,
    /// Chain digest carried by the checkpoint frame.
    pub chain: ChainHash<D>,
    /// Canonical record identity carried by the checkpoint frame.
    pub record: RecordId<D>,
}

impl<D> Copy for JournalCheckpoint<D> {}
impl<D> Clone for JournalCheckpoint<D> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<D> PartialEq for JournalCheckpoint<D> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
            && self.sequence == other.sequence
            && self.chain == other.chain
            && self.record == other.record
    }
}
impl<D> Eq for JournalCheckpoint<D> {}
impl<D> fmt::Debug for JournalCheckpoint<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalCheckpoint")
            .field("offset", &self.offset)
            .field("sequence", &self.sequence)
            .field("chain", &self.chain)
            .field("record", &self.record)
            .finish()
    }
}

impl<D> Copy for JournalReceipt<D> {}
impl<D> Clone for JournalReceipt<D> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<D> PartialEq for JournalReceipt<D> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
            && self.sequence == other.sequence
            && self.chain == other.chain
            && self.record == other.record
    }
}
impl<D> Eq for JournalReceipt<D> {}
impl<D> fmt::Debug for JournalReceipt<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalReceipt")
            .field("offset", &self.offset)
            .field("sequence", &self.sequence)
            .field("chain", &self.chain)
            .field("record", &self.record)
            .finish()
    }
}

impl<D> JournalReceipt<D> {
    /// Converts a synced append receipt into a restart checkpoint.
    #[must_use]
    pub const fn checkpoint(self) -> JournalCheckpoint<D> {
        JournalCheckpoint {
            offset: self.offset,
            sequence: self.sequence,
            chain: self.chain,
            record: self.record,
        }
    }
}

impl<D> From<JournalReceipt<D>> for JournalCheckpoint<D> {
    fn from(receipt: JournalReceipt<D>) -> Self {
        receipt.checkpoint()
    }
}

/// A borrowed frame delivered by [`crate::journal::HashChainJournal::scan_stream`].
///
/// The payload is valid only for the duration of the visitor call.  This is
/// the key distinction from [`JournalFrame`]: streaming consumers can fold a
/// journal without retaining a second copy of its history.
pub struct JournalFrameRef<'a, D: JournalDomain> {
    /// Monotonic frame number.
    pub sequence: u64,
    /// Hash of the previous frame.
    pub previous: ChainHash<D>,
    /// Content identity of the canonical record payload.
    pub record: RecordId<D>,
    /// Canonical payload bytes borrowed from the scanner's single frame slot.
    pub payload: &'a [u8],
    /// Hash of this frame.
    pub chain: ChainHash<D>,
    /// Byte offset at which this frame starts.
    pub offset: u64,
    /// Byte offset immediately following this frame.
    pub end_offset: u64,
}

impl<D: JournalDomain> JournalFrameRef<'_, D> {
    /// Returns the byte offset at which this borrowed frame starts.
    #[must_use]
    pub const fn start_offset(&self) -> u64 {
        self.offset
    }
}

impl<D: JournalDomain> fmt::Debug for JournalFrameRef<'_, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalFrameRef")
            .field("sequence", &self.sequence)
            .field("previous", &self.previous)
            .field("record", &self.record)
            .field("payload_len", &self.payload.len())
            .field("chain", &self.chain)
            .field("offset", &self.offset)
            .field("end_offset", &self.end_offset)
            .finish()
    }
}

/// Bounded observations from a streaming journal scan.
pub struct JournalScan<D: JournalDomain> {
    /// Number of complete frames delivered to the visitor.
    pub frames_scanned: usize,
    /// Number of tail bytes consumed after the optional checkpoint.
    pub bytes_scanned: u64,
    /// Largest payload buffer allocated by the scanner.
    pub peak_payload_bytes: usize,
    /// Offset immediately following the last valid frame.
    pub valid_offset: u64,
    /// Last valid sequence, including the checkpoint when one was supplied.
    pub last_sequence: Option<u64>,
    /// Last valid chain, including the checkpoint when one was supplied.
    pub chain: ChainHash<D>,
    /// Whether a partial trailing frame was found.
    pub truncated_tail: bool,
}

impl<D: JournalDomain> Copy for JournalScan<D> {}
impl<D: JournalDomain> Clone for JournalScan<D> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<D: JournalDomain> PartialEq for JournalScan<D> {
    fn eq(&self, other: &Self) -> bool {
        self.frames_scanned == other.frames_scanned
            && self.bytes_scanned == other.bytes_scanned
            && self.peak_payload_bytes == other.peak_payload_bytes
            && self.valid_offset == other.valid_offset
            && self.last_sequence == other.last_sequence
            && self.chain == other.chain
            && self.truncated_tail == other.truncated_tail
    }
}
impl<D: JournalDomain> Eq for JournalScan<D> {}
impl<D: JournalDomain> fmt::Debug for JournalScan<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalScan")
            .field("frames_scanned", &self.frames_scanned)
            .field("bytes_scanned", &self.bytes_scanned)
            .field("peak_payload_bytes", &self.peak_payload_bytes)
            .field("valid_offset", &self.valid_offset)
            .field("last_sequence", &self.last_sequence)
            .field("chain", &self.chain)
            .field("truncated_tail", &self.truncated_tail)
            .finish()
    }
}

/// Recovered journal state.
pub struct JournalRecovery<D: JournalDomain> {
    /// Valid frames in order.
    pub frames: Vec<JournalFrame<D>>,
    /// Last valid sequence, if one exists.
    pub last_sequence: Option<u64>,
    /// Last valid chain hash.
    pub chain: ChainHash<D>,
    /// Whether a partial trailing frame was removed.
    pub truncated_tail: bool,
}

impl<D: JournalDomain> Clone for JournalRecovery<D> {
    fn clone(&self) -> Self {
        Self {
            frames: self.frames.clone(),
            last_sequence: self.last_sequence,
            chain: self.chain,
            truncated_tail: self.truncated_tail,
        }
    }
}
impl<D: JournalDomain> PartialEq for JournalRecovery<D> {
    fn eq(&self, other: &Self) -> bool {
        self.frames == other.frames
            && self.last_sequence == other.last_sequence
            && self.chain == other.chain
            && self.truncated_tail == other.truncated_tail
    }
}
impl<D: JournalDomain> Eq for JournalRecovery<D> {}
impl<D: JournalDomain> fmt::Debug for JournalRecovery<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalRecovery")
            .field("frames", &self.frames)
            .field("last_sequence", &self.last_sequence)
            .field("chain", &self.chain)
            .field("truncated_tail", &self.truncated_tail)
            .finish()
    }
}

/// Journal failures. A complete invalid frame is never silently skipped.
#[derive(Debug)]
pub enum JournalError {
    /// Underlying filesystem failure.
    Io(io::Error),
    /// A complete frame violated the format or chain.
    Corrupt(&'static str),
    /// An append may have reached the filesystem, but its durability could
    /// not be established. The identity is included so a caller can retry
    /// the exact record and have the journal adopt it instead of appending a
    /// duplicate.
    AppendUncertain {
        /// Byte offset at which the attempted frame starts.
        offset: u64,
        /// Sequence assigned to the attempted frame.
        sequence: u64,
        /// Candidate chain tip.
        chain: [u8; 32],
        /// Candidate record identity.
        record: [u8; 32],
    },
    /// A declared payload exceeded the bounded decoder budget.
    Bounds,
    /// The journal domain did not match the codec.
    DomainMismatch,
    /// The domain-specific record could not be decoded.
    Record(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "journal I/O error: {error}"),
            Self::Corrupt(reason) => write!(f, "corrupt journal: {reason}"),
            Self::AppendUncertain {
                offset, sequence, ..
            } => write!(
                f,
                "journal append outcome is uncertain at offset {offset} (sequence {sequence})"
            ),
            Self::Bounds => write!(f, "journal frame exceeds bounds"),
            Self::DomainMismatch => write!(f, "journal domain mismatch"),
            Self::Record(error) => write!(f, "invalid journal record: {error}"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<io::Error> for JournalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl JournalError {
    /// Returns the candidate receipt carried by an uncertain append result.
    ///
    /// The receipt is an identity for retry/admission, not a claim that the
    /// frame is durable. Callers should retry the exact encoded record (or
    /// reopen and recover) before treating the append as committed.
    #[must_use]
    pub fn uncertain_receipt<D: JournalDomain>(&self) -> Option<JournalReceipt<D>> {
        let Self::AppendUncertain {
            offset,
            sequence,
            chain,
            record,
        } = self
        else {
            return None;
        };
        Some(JournalReceipt {
            offset: *offset,
            sequence: *sequence,
            chain: ChainHash::from_bytes(*chain),
            record: RecordId::from_bytes(*record),
        })
    }
}
