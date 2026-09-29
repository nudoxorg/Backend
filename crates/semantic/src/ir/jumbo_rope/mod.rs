//! Content-addressed storage for canonical documentation and source values
//! that are too large to fit in one semantic-plane segment.
//!
//! The implementation is split by trust boundary: descriptor checks, object
//! wire identities, bounded writing, and resumable receiving. A caller persists
//! leaves and fixed-size nodes through its existing object CAS; this module does
//! not split or reinterpret SPIR record bytes.

use thiserror::Error;

use super::MAX_SEMANTIC_SEGMENT_BYTES;

mod descriptor;
mod receiver;
mod wire;
mod writer;

pub use descriptor::{
    CheckedJumboValueDescriptor, JumboValueDescriptorId, UntrustedJumboValueDescriptor,
};
pub use receiver::{
    CheckedJumboLeaf, JumboRopeClosure, MissingJumboLeafRanges, VerifiedJumboRope, prove_jumbo_leaf,
};
pub use wire::{
    JumboRopeNode, JumboRopeObjectId, JumboRopeObjectKind, JumboRopeProof, JumboRopeProofSibling,
    JumboRopeProofSide, JumboRopeProofSpan,
};
pub use writer::{
    JumboRopeBuildMetrics, JumboRopeStreamWriter, JumboRopeWriteReceipt, write_jumbo_value,
    write_jumbo_value_from_reader,
};

/// Smallest non-final content-defined leaf, in bytes.
pub const JUMBO_ROPE_MIN_LEAF_BYTES: usize = 64 * 1024;
/// Target content-defined leaf size, in bytes.
pub const JUMBO_ROPE_TARGET_LEAF_BYTES: usize = 128 * 1024;
/// Hard maximum content-defined leaf size, in bytes.
pub const JUMBO_ROPE_MAX_LEAF_BYTES: usize = 256 * 1024;
/// Fixed input buffer used by the reader-based writer, in bytes.
pub const JUMBO_ROPE_STREAM_BUFFER_BYTES: usize = 64 * 1024;
/// Fixed wire length of one typed jumbo value descriptor, in bytes.
pub const JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES: usize = 91;

pub(super) const MAX_PROOF_DEPTH: usize = 64;
/// Fixed wire length of one canonical interior-node payload, in bytes.
pub const ROPE_NODE_WIRE_BYTES: usize = 4 + 8 + 8 + 8 + (1 + 32 + 8 + 8 + 8) * 2;

/// Whether a canonical value needs a separate jumbo descriptor.
#[must_use]
pub const fn requires_jumbo_rope(byte_length: usize) -> bool {
    byte_length > MAX_SEMANTIC_SEGMENT_BYTES
}

/// Canonical row family that owns a jumbo value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum JumboValueFamily {
    /// A documentation fragment value.
    Documentation = 1,
    /// A source provenance value such as a source path.
    SourceProvenance = 2,
}

impl JumboValueFamily {
    fn from_code(code: u8) -> Result<Self, JumboRopeError> {
        match code {
            1 => Ok(Self::Documentation),
            2 => Ok(Self::SourceProvenance),
            _ => Err(JumboRopeError::UnknownFamily(code)),
        }
    }
}

/// Canonical byte interpretation for one jumbo value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum JumboValueEncoding {
    /// Arbitrary canonical bytes.
    Bytes = 1,
    /// UTF-8 text, validated across the complete ordered leaf closure.
    Utf8 = 2,
}

impl JumboValueEncoding {
    fn from_code(code: u8) -> Result<Self, JumboRopeError> {
        match code {
            1 => Ok(Self::Bytes),
            2 => Ok(Self::Utf8),
            _ => Err(JumboRopeError::UnknownEncoding(code)),
        }
    }
}

/// Stable row owner and field selector for a jumbo semantic value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JumboValueContext {
    owner: [u8; 32],
    family: JumboValueFamily,
    field_ordinal: u32,
    encoding: JumboValueEncoding,
}

impl JumboValueContext {
    /// Creates the identity context for one documentation or source value.
    #[must_use]
    pub const fn new(
        owner: [u8; 32],
        family: JumboValueFamily,
        field_ordinal: u32,
        encoding: JumboValueEncoding,
    ) -> Self {
        Self {
            owner,
            family,
            field_ordinal,
            encoding,
        }
    }

    /// Stable row key that owns this field.
    #[must_use]
    pub const fn owner(self) -> [u8; 32] {
        self.owner
    }

    /// Canonical row family that owns this field.
    #[must_use]
    pub const fn family(self) -> JumboValueFamily {
        self.family
    }

    /// Ordinal of this field within its canonical owner row.
    #[must_use]
    pub const fn field_ordinal(self) -> u32 {
        self.field_ordinal
    }

    /// Encoding that must hold after ordered reassembly.
    #[must_use]
    pub const fn encoding(self) -> JumboValueEncoding {
        self.encoding
    }
}

/// Resource policy applied before admitting an untrusted jumbo descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JumboRopeLimits {
    /// Largest complete value accepted by one rope.
    pub max_value_bytes: u64,
    /// Largest number of leaves admitted into a resumable closure.
    pub max_leaf_count: u64,
    /// Largest in-memory leaf receipt table accepted by a receiver.
    pub max_metadata_bytes: usize,
}

impl JumboRopeLimits {
    /// Creates an explicit byte-, leaf-, and metadata-bounded policy.
    #[must_use]
    pub const fn new(max_value_bytes: u64, max_leaf_count: u64, max_metadata_bytes: usize) -> Self {
        Self {
            max_value_bytes,
            max_leaf_count,
            max_metadata_bytes,
        }
    }

    pub(super) fn validate(self) -> Result<Self, JumboRopeError> {
        if self.max_leaf_count == 0 || self.max_metadata_bytes == 0 {
            return Err(JumboRopeError::InvalidLimits);
        }
        Ok(self)
    }
}

impl Default for JumboRopeLimits {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024 * 1024, 250_000, 64 * 1024 * 1024)
    }
}

/// Borrowed content-addressed leaf offered to an object CAS.
#[derive(Clone, Copy, Debug)]
pub struct JumboRopeLeafRef<'bytes> {
    id: JumboRopeObjectId,
    ordinal: u64,
    byte_offset: u64,
    bytes: &'bytes [u8],
}

impl<'bytes> JumboRopeLeafRef<'bytes> {
    /// Content identity of the exact borrowed leaf bytes.
    #[must_use]
    pub const fn id(self) -> JumboRopeObjectId {
        self.id
    }

    /// Leaf position in the complete ordered rope.
    #[must_use]
    pub const fn ordinal(self) -> u64 {
        self.ordinal
    }

    /// Exact byte offset committed by the authenticated tree path.
    #[must_use]
    pub const fn byte_offset(self) -> u64 {
        self.byte_offset
    }

    /// Canonical leaf content borrowed for the sink call only.
    #[must_use]
    pub const fn bytes(self) -> &'bytes [u8] {
        self.bytes
    }
}

/// One bounded semantic rope object sink.
///
/// Implementations should admit each payload through the repository's
/// existing typed object CAS and return only after that object is durable.
/// Interior payloads are fixed-size canonical node records; leaves are the
/// original value bytes. A failed write may leave unreachable immutable
/// objects, but no descriptor token is returned.
pub trait JumboRopeObjectSink {
    /// Sink-specific durable admission failure.
    type Error;

    /// Durably admits a leaf under its semantic content identity.
    fn write_leaf(&mut self, leaf: JumboRopeLeafRef<'_>) -> Result<(), Self::Error>;

    /// Durably admits one fixed-size canonical interior-node payload.
    fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error>;
}

/// Object reader used for proofs and closure read-back.
pub trait JumboRopeObjectSource {
    /// Source-specific object read failure.
    type Error;

    /// Reads one leaf by semantic identity into the caller's fixed-size
    /// buffer. Return the exact initialized byte count; return `None` when the
    /// object is absent. The output slice is bounded to the maximum leaf size,
    /// so adapters can reject oversized objects before allocating their body.
    fn read_leaf(
        &mut self,
        id: JumboRopeObjectId,
        output: &mut [u8; JUMBO_ROPE_MAX_LEAF_BYTES],
    ) -> Result<Option<usize>, Self::Error>;

    /// Reads one exact fixed-size interior-node payload by semantic identity.
    fn read_interior(
        &mut self,
        id: JumboRopeObjectId,
    ) -> Result<Option<[u8; ROPE_NODE_WIRE_BYTES]>, Self::Error>;
}

/// Failures while checking a descriptor, leaf path, or complete closure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum JumboRopeError {
    /// Invalid resource policy.
    #[error("jumbo rope limits must bound leaves and metadata")]
    InvalidLimits,
    /// Descriptor wire payload has the wrong exact size.
    #[error("descriptor wire size is {observed}, expected {expected}")]
    DescriptorWireLength { expected: usize, observed: usize },
    /// Descriptor magic or wire version is unsupported.
    #[error("descriptor wire magic or version is invalid")]
    DescriptorWireVersion,
    /// Descriptor names an unsupported family code.
    #[error("unknown jumbo value family code {0}")]
    UnknownFamily(u8),
    /// Descriptor names an unsupported encoding code.
    #[error("unknown jumbo value encoding code {0}")]
    UnknownEncoding(u8),
    /// Descriptor's total byte length exceeds configured limits.
    #[error("jumbo value has {observed} bytes; limit is {maximum}")]
    ValueTooLarge { observed: u64, maximum: u64 },
    /// Descriptor's leaf count exceeds configured limits.
    #[error("jumbo value has {observed} leaves; limit is {maximum}")]
    TooManyLeaves { observed: u64, maximum: u64 },
    /// Descriptor's fixed receipt metadata exceeds configured limits.
    #[error("jumbo closure metadata uses {observed} bytes; limit is {maximum}")]
    MetadataTooLarge { observed: usize, maximum: usize },
    /// Empty values must use the unique empty root and no leaves.
    #[error("empty jumbo descriptor has a non-empty root or leaf count")]
    InvalidEmptyDescriptor,
    /// Leaf count is impossible for the declared value length and bounds.
    #[error("jumbo descriptor leaf count is inconsistent with its byte length")]
    InvalidLeafCount,
    /// Checked arithmetic overflowed.
    #[error("jumbo rope length or ordinal overflowed")]
    LengthOverflow,
    /// A bounded allocation failed.
    #[error("bounded jumbo rope scratch allocation failed")]
    Allocation,
    /// The allocator provided more leaf scratch than the configured bound.
    #[error("leaf scratch capacity {observed} exceeds {maximum}")]
    ScratchCapacity { observed: usize, maximum: usize },
    /// The allocator provided more frontier storage than the configured bound.
    #[error("rope frontier capacity {observed} exceeds {maximum}")]
    FrontierCapacity { observed: usize, maximum: usize },
    /// One leaf length violates the min/maximum policy.
    #[error("leaf length {observed} is invalid at ordinal {ordinal}")]
    InvalidLeafLength { ordinal: u64, observed: usize },
    /// A leaf ordinal is outside the descriptor's exact range.
    #[error("leaf ordinal {ordinal} is outside {leaf_count} leaves")]
    LeafOrdinalOutOfRange { ordinal: u64, leaf_count: u64 },
    /// Proof exceeds the bounded Merkle depth.
    #[error("proof depth {observed} exceeds {maximum}")]
    ProofTooDeep { observed: usize, maximum: usize },
    /// Proof ranges do not form one exact ordered sequence.
    #[error("rope proof has a gap, overlap, or reordered range")]
    NonContiguousRopeRange,
    /// A leaf path does not reach the descriptor's exact root and ranges.
    #[error("leaf proof does not match the descriptor root")]
    ProofRootMismatch,
    /// A checked leaf belongs to another descriptor.
    #[error("checked leaf belongs to a different descriptor")]
    LeafDescriptorMismatch,
    /// A repeated leaf ordinal claimed different checked content.
    #[error("duplicate leaf ordinal has conflicting content or offset")]
    ConflictingDuplicateLeaf,
    /// A value closure is incomplete and therefore cannot be published.
    #[error("jumbo value is missing {missing} leaves")]
    MissingLeaves { missing: u64 },
    /// Empty ropes do not have a leaf path.
    #[error("empty rope has no leaf object")]
    EmptyRopeHasNoLeaf,
    /// No object was returned for an admitted leaf.
    #[error("an admitted rope leaf is absent from the object store")]
    MissingStoredLeaf,
    /// No object was returned for an authenticated interior node.
    #[error("an authenticated rope interior object is absent from the object store")]
    MissingStoredInterior,
    /// Stored leaf bytes do not match their content identity and length.
    #[error("stored rope leaf content is corrupt")]
    LeafObjectCorrupt,
    /// Stored interior bytes do not match their ID or canonical tree shape.
    #[error("stored rope interior object is corrupt")]
    InteriorObjectCorrupt,
    /// Ordered reassembly is not valid UTF-8 for a text descriptor.
    #[error("ordered jumbo value is not valid UTF-8")]
    InvalidUtf8,
    /// A source reader returned an object larger than its bounded policy.
    #[error("source returned an oversized rope leaf")]
    OversizedStoredLeaf,
    /// The verified tree does not contain the descriptor's exact census.
    #[error("rope tree does not cover the descriptor's exact leaf and byte ranges")]
    ClosureCensusMismatch,
}

/// I/O or object-store failure while writing, receiving, or reading a rope.
#[derive(Debug, Error)]
pub enum JumboOperationError<E> {
    /// Descriptor, proof, closure, or stored-object verification failed.
    #[error(transparent)]
    Rope(#[from] JumboRopeError),
    /// Input stream failed before a descriptor could be completed.
    #[error("jumbo value input stream failed")]
    Input(#[source] std::io::Error),
    /// Caller-owned output writer failed during reassembly.
    #[error("jumbo value output stream failed")]
    Output(#[source] std::io::Error),
    /// Existing object-store operation failed.
    #[error("jumbo rope object store failed")]
    Store(#[source] E),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LeafReceipt {
    pub(super) id: JumboRopeObjectId,
    pub(super) byte_offset: u64,
    pub(super) byte_length: u64,
}

#[derive(Default)]
pub(super) struct Utf8Validator {
    remaining: u8,
    next_min: u8,
    next_max: u8,
}

impl Utf8Validator {
    pub(super) fn push(&mut self, byte: u8) -> Result<(), JumboRopeError> {
        if self.remaining > 0 {
            if byte < self.next_min || byte > self.next_max {
                return Err(JumboRopeError::InvalidUtf8);
            }
            self.remaining -= 1;
            self.next_min = 0x80;
            self.next_max = 0xbf;
            return Ok(());
        }
        match byte {
            0x00..=0x7f => {}
            0xc2..=0xdf => self.start_sequence(1, 0x80, 0xbf),
            0xe0 => self.start_sequence(2, 0xa0, 0xbf),
            0xe1..=0xec | 0xee..=0xef => self.start_sequence(2, 0x80, 0xbf),
            0xed => self.start_sequence(2, 0x80, 0x9f),
            0xf0 => self.start_sequence(3, 0x90, 0xbf),
            0xf1..=0xf3 => self.start_sequence(3, 0x80, 0xbf),
            0xf4 => self.start_sequence(3, 0x80, 0x8f),
            _ => return Err(JumboRopeError::InvalidUtf8),
        }
        Ok(())
    }

    fn start_sequence(&mut self, continuation_bytes: u8, next_min: u8, next_max: u8) {
        self.remaining = continuation_bytes;
        self.next_min = next_min;
        self.next_max = next_max;
    }

    pub(super) fn finish(&self) -> Result<(), JumboRopeError> {
        if self.remaining == 0 {
            Ok(())
        } else {
            Err(JumboRopeError::InvalidUtf8)
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;
