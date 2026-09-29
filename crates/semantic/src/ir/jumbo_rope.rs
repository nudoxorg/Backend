//! Content-addressed storage for canonical documentation and source values
//! that are too large to fit in one semantic-plane segment.
//!
//! This module owns only semantic value and rope identities. A caller
//! persists the borrowed leaf bytes and small canonical interior-node payloads
//! through its existing object CAS. The returned descriptor is suitable for a
//! future typed row encoding; it does not split or reinterpret an SPIR record.
//!
//! Values use deterministic content-defined boundaries. The writer retains
//! one bounded leaf buffer and a logarithmic Merkle frontier. Transfer
//! receivers admit authenticated leaf paths independently, report missing
//! ordinal ranges, and mint a verified token only after every object has been
//! read back and checked.

use alloc::{boxed::Box, vec::Vec};
use core::{fmt, mem::size_of, ops::Range};
use std::io::{Read, Write};

use thiserror::Error;

use super::MAX_SEMANTIC_SEGMENT_BYTES;

/// Smallest non-final content-defined leaf, in bytes.
pub const JUMBO_ROPE_MIN_LEAF_BYTES: usize = 64 * 1024;
/// Target content-defined leaf size, in bytes.
pub const JUMBO_ROPE_TARGET_LEAF_BYTES: usize = 128 * 1024;
/// Hard maximum content-defined leaf size, in bytes.
pub const JUMBO_ROPE_MAX_LEAF_BYTES: usize = 256 * 1024;
/// Fixed input buffer used by the reader-based writer, in bytes.
pub const JUMBO_ROPE_STREAM_BUFFER_BYTES: usize = 64 * 1024;

const CUT_MASK: u64 = (1 << 17) - 1;
const GEAR_WINDOW_BYTES: usize = 64;
const GEAR_ROLLING_BASE: u64 = 257;
const GEAR_ROLLING_POWER: u64 = 257_u64.wrapping_pow(GEAR_WINDOW_BYTES as u32);
const MAX_PROOF_DEPTH: usize = 64;
const DESCRIPTOR_MAGIC: &[u8; 4] = b"JVD1";
const DESCRIPTOR_VERSION: u8 = 1;
const DESCRIPTOR_WIRE_BYTES: usize = 4 + 1 + 1 + 1 + 4 + 32 + 8 + 8 + 32;
const NODE_MAGIC: &[u8; 4] = b"JRN1";
const ROPE_NODE_WIRE_BYTES: usize = 4 + 8 + 8 + 8 + (1 + 32 + 8 + 8 + 8) * 2;

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

    fn validate(self) -> Result<Self, JumboRopeError> {
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

/// Opaque content identity for one leaf or interior rope node.
#[repr(transparent)]
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JumboRopeObjectId([u8; 32]);

impl JumboRopeObjectId {
    /// Returns the fixed-width content identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for JumboRopeObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JumboRopeObjectId(")?;
        for byte in &self.0[..6] {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str("…)")
    }
}

/// Content identity of the complete typed value descriptor.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JumboValueDescriptorId([u8; 32]);

impl JumboValueDescriptorId {
    /// Returns the fixed-width descriptor identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Wire descriptor whose field values have not been checked against policy.
///
/// A descriptor does not prove that any referenced object exists. Use check
/// before allocating receiver state, then verify each leaf path and the
/// complete closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UntrustedJumboValueDescriptor {
    owner: [u8; 32],
    family: JumboValueFamily,
    field_ordinal: u32,
    encoding: JumboValueEncoding,
    byte_length: u64,
    leaf_count: u64,
    root: [u8; 32],
}

impl UntrustedJumboValueDescriptor {
    /// Wraps descriptor fields received from an untrusted manifest or wire.
    #[must_use]
    pub const fn from_fields(
        owner: [u8; 32],
        family: JumboValueFamily,
        field_ordinal: u32,
        encoding: JumboValueEncoding,
        byte_length: u64,
        leaf_count: u64,
        root: [u8; 32],
    ) -> Self {
        Self {
            owner,
            family,
            field_ordinal,
            encoding,
            byte_length,
            leaf_count,
            root,
        }
    }

    /// Decodes the fixed-size descriptor wire form without trusting its
    /// lengths, leaf count, or root commitment.
    pub fn decode_wire(bytes: &[u8]) -> Result<Self, JumboRopeError> {
        if bytes.len() != DESCRIPTOR_WIRE_BYTES {
            return Err(JumboRopeError::DescriptorWireLength {
                expected: DESCRIPTOR_WIRE_BYTES,
                observed: bytes.len(),
            });
        }
        if bytes.get(..4) != Some(DESCRIPTOR_MAGIC.as_slice())
            || bytes.get(4) != Some(&DESCRIPTOR_VERSION)
        {
            return Err(JumboRopeError::DescriptorWireVersion);
        }
        let family = JumboValueFamily::from_code(bytes[5])?;
        let encoding = JumboValueEncoding::from_code(bytes[6])?;
        let field_ordinal = u32::from_be_bytes(read_array(bytes, 7)?);
        let owner = read_array(bytes, 11)?;
        let byte_length = u64::from_be_bytes(read_array(bytes, 43)?);
        let leaf_count = u64::from_be_bytes(read_array(bytes, 51)?);
        let root = read_array(bytes, 59)?;
        Ok(Self::from_fields(
            owner,
            family,
            field_ordinal,
            encoding,
            byte_length,
            leaf_count,
            root,
        ))
    }

    /// Serializes the descriptor in its fixed-size canonical wire form.
    #[must_use]
    pub fn encode_wire(self) -> [u8; DESCRIPTOR_WIRE_BYTES] {
        let mut bytes = [0; DESCRIPTOR_WIRE_BYTES];
        bytes[..4].copy_from_slice(DESCRIPTOR_MAGIC);
        bytes[4] = DESCRIPTOR_VERSION;
        bytes[5] = self.family as u8;
        bytes[6] = self.encoding as u8;
        bytes[7..11].copy_from_slice(&self.field_ordinal.to_be_bytes());
        bytes[11..43].copy_from_slice(&self.owner);
        bytes[43..51].copy_from_slice(&self.byte_length.to_be_bytes());
        bytes[51..59].copy_from_slice(&self.leaf_count.to_be_bytes());
        bytes[59..91].copy_from_slice(&self.root);
        bytes
    }

    /// Checks structural fields and configured resource limits.
    pub fn check(
        self,
        limits: JumboRopeLimits,
    ) -> Result<CheckedJumboValueDescriptor, JumboRopeError> {
        let limits = limits.validate()?;
        if self.byte_length > limits.max_value_bytes {
            return Err(JumboRopeError::ValueTooLarge {
                observed: self.byte_length,
                maximum: limits.max_value_bytes,
            });
        }
        if self.leaf_count > limits.max_leaf_count {
            return Err(JumboRopeError::TooManyLeaves {
                observed: self.leaf_count,
                maximum: limits.max_leaf_count,
            });
        }
        if self.byte_length == 0 {
            if self.leaf_count != 0 || self.root != empty_rope_root().0 {
                return Err(JumboRopeError::InvalidEmptyDescriptor);
            }
        } else {
            if self.leaf_count == 0 {
                return Err(JumboRopeError::InvalidLeafCount);
            }
            let maximum = self
                .leaf_count
                .checked_mul(JUMBO_ROPE_MAX_LEAF_BYTES as u64)
                .ok_or(JumboRopeError::LengthOverflow)?;
            let minimum = self
                .leaf_count
                .saturating_sub(1)
                .checked_mul(JUMBO_ROPE_MIN_LEAF_BYTES as u64)
                .and_then(|bytes| bytes.checked_add(1))
                .ok_or(JumboRopeError::LengthOverflow)?;
            if self.byte_length > maximum || self.byte_length < minimum {
                return Err(JumboRopeError::InvalidLeafCount);
            }
        }
        let count = usize::try_from(self.leaf_count).map_err(|_| JumboRopeError::LengthOverflow)?;
        let receipt_bytes = count
            .checked_mul(size_of::<Option<LeafReceipt>>())
            .ok_or(JumboRopeError::LengthOverflow)?;
        if receipt_bytes > limits.max_metadata_bytes {
            return Err(JumboRopeError::MetadataTooLarge {
                observed: receipt_bytes,
                maximum: limits.max_metadata_bytes,
            });
        }
        Ok(CheckedJumboValueDescriptor {
            owner: self.owner,
            family: self.family,
            field_ordinal: self.field_ordinal,
            encoding: self.encoding,
            byte_length: self.byte_length,
            leaf_count: self.leaf_count,
            root: JumboRopeObjectId(self.root),
        })
    }

    /// Owner row key claimed by this descriptor.
    #[must_use]
    pub const fn owner(self) -> [u8; 32] {
        self.owner
    }

    /// Family claimed by this descriptor.
    #[must_use]
    pub const fn family(self) -> JumboValueFamily {
        self.family
    }

    /// Field ordinal claimed by this descriptor.
    #[must_use]
    pub const fn field_ordinal(self) -> u32 {
        self.field_ordinal
    }

    /// Encoding claimed by this descriptor.
    #[must_use]
    pub const fn encoding(self) -> JumboValueEncoding {
        self.encoding
    }

    /// Exact total byte length claimed by this descriptor.
    #[must_use]
    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }

    /// Exact number of ordered leaves claimed by this descriptor.
    #[must_use]
    pub const fn leaf_count(self) -> u64 {
        self.leaf_count
    }

    /// Untrusted ordered rope root bytes claimed by this descriptor.
    #[must_use]
    pub const fn root_claim(self) -> &[u8; 32] {
        &self.root
    }
}

/// Structurally checked descriptor; its root remains unproven until a full
/// content-addressed leaf closure has been checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckedJumboValueDescriptor {
    owner: [u8; 32],
    family: JumboValueFamily,
    field_ordinal: u32,
    encoding: JumboValueEncoding,
    byte_length: u64,
    leaf_count: u64,
    root: JumboRopeObjectId,
}

impl CheckedJumboValueDescriptor {
    /// Stable row key that owns this field.
    #[must_use]
    pub const fn owner(&self) -> &[u8; 32] {
        &self.owner
    }

    /// Canonical row family that owns this field.
    #[must_use]
    pub const fn family(&self) -> JumboValueFamily {
        self.family
    }

    /// Ordinal of this field within its canonical owner row.
    #[must_use]
    pub const fn field_ordinal(&self) -> u32 {
        self.field_ordinal
    }

    /// Canonical byte interpretation for the value.
    #[must_use]
    pub const fn encoding(&self) -> JumboValueEncoding {
        self.encoding
    }

    /// Exact total byte length claimed by the descriptor.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Exact ordered leaf count claimed by the descriptor.
    #[must_use]
    pub const fn leaf_count(&self) -> u64 {
        self.leaf_count
    }

    /// Claimed content root. It becomes verified only with a complete rope.
    #[must_use]
    pub const fn root_claim(&self) -> JumboRopeObjectId {
        self.root
    }

    /// Returns canonical descriptor bytes for a future semantic-row schema.
    #[must_use]
    pub fn encode_wire(&self) -> [u8; DESCRIPTOR_WIRE_BYTES] {
        UntrustedJumboValueDescriptor::from_fields(
            self.owner,
            self.family,
            self.field_ordinal,
            self.encoding,
            self.byte_length,
            self.leaf_count,
            self.root.0,
        )
        .encode_wire()
    }

    /// Content identity of this complete typed descriptor claim.
    #[must_use]
    pub fn id(&self) -> JumboValueDescriptorId {
        descriptor_identity(&self.encode_wire())
    }

    fn root_ref(&self) -> Result<RopeObjectRef, JumboRopeError> {
        if self.leaf_count == 0 {
            return Err(JumboRopeError::EmptyRopeHasNoLeaf);
        }
        Ok(RopeObjectRef {
            kind: if self.leaf_count == 1 {
                RopeObjectKind::Leaf
            } else {
                RopeObjectKind::Interior
            },
            id: self.root,
            first_leaf: 0,
            leaf_count: self.leaf_count,
            byte_length: self.byte_length,
        })
    }
}

/// Work counters from one streaming rope write.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JumboRopeBuildMetrics {
    input_bytes: u64,
    leaf_count: u64,
    interior_node_count: u64,
    payload_hash_bytes: u64,
    chunk_scratch_capacity_bytes: u64,
    peak_live_scratch_bytes: u64,
    peak_frontier_entries: u64,
    input_buffer_bytes: u64,
}

impl JumboRopeBuildMetrics {
    /// Exact bytes read from the borrowed slice or input stream.
    #[must_use]
    pub const fn input_bytes(self) -> u64 {
        self.input_bytes
    }

    /// Number of content-addressed leaf objects emitted.
    #[must_use]
    pub const fn leaf_count(self) -> u64 {
        self.leaf_count
    }

    /// Number of authenticated interior objects emitted.
    #[must_use]
    pub const fn interior_node_count(self) -> u64 {
        self.interior_node_count
    }

    /// Exact source bytes passed through leaf hashing.
    #[must_use]
    pub const fn payload_hash_bytes(self) -> u64 {
        self.payload_hash_bytes
    }

    /// Capacity of the one reused bounded leaf buffer.
    #[must_use]
    pub const fn chunk_scratch_capacity_bytes(self) -> u64 {
        self.chunk_scratch_capacity_bytes
    }

    /// Peak internal scratch estimated from the live writer state, reserved
    /// leaf/frontier capacities, and (for reader writes) fixed input buffer.
    /// Caller-owned input and sink-owned storage are excluded.
    #[must_use]
    pub const fn peak_live_scratch_bytes(self) -> u64 {
        self.peak_live_scratch_bytes
    }

    /// Largest number of pending Merkle frontier spans retained at once.
    #[must_use]
    pub const fn peak_frontier_entries(self) -> u64 {
        self.peak_frontier_entries
    }

    /// Fixed reader buffer charged by the stream-based writer.
    #[must_use]
    pub const fn input_buffer_bytes(self) -> u64 {
        self.input_buffer_bytes
    }
}

/// Complete local write result. The descriptor token is produced only after
/// every leaf and interior write succeeded and the exact input was checked.
#[derive(Clone, Debug)]
pub struct JumboRopeWriteReceipt {
    verified: VerifiedJumboRope,
    metrics: JumboRopeBuildMetrics,
}

impl JumboRopeWriteReceipt {
    /// Complete verified value descriptor.
    #[must_use]
    pub const fn verified(&self) -> &VerifiedJumboRope {
        &self.verified
    }

    /// Work and bounded scratch counters from this write.
    #[must_use]
    pub const fn metrics(&self) -> JumboRopeBuildMetrics {
        self.metrics
    }
}

/// Complete verified descriptor token. This type can only be minted by a
/// successful local write or a complete receiver closure check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedJumboRope {
    descriptor: CheckedJumboValueDescriptor,
}

impl VerifiedJumboRope {
    /// Descriptor whose complete object closure was verified.
    #[must_use]
    pub const fn descriptor(&self) -> &CheckedJumboValueDescriptor {
        &self.descriptor
    }

    /// Content identity of the typed descriptor.
    #[must_use]
    pub fn descriptor_id(&self) -> JumboValueDescriptorId {
        self.descriptor.id()
    }

    /// Reads the complete ordered value into a caller-owned writer while
    /// checking every stored leaf again. Scratch remains bounded to one leaf.
    pub fn write_value_to<S, W>(
        &self,
        source: &mut S,
        writer: &mut W,
    ) -> Result<u64, JumboOperationError<S::Error>>
    where
        S: JumboRopeObjectSource + ?Sized,
        W: Write,
    {
        stream_verified_value(&self.descriptor, source, writer)
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

/// Checked canonical interior node. Its ID commits to both ordered children,
/// their exact leaf ranges, and their byte lengths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JumboRopeNode {
    first_leaf: u64,
    leaf_count: u64,
    byte_length: u64,
    left: RopeObjectRef,
    right: RopeObjectRef,
}

impl JumboRopeNode {
    /// Opaque semantic identity of this node.
    #[must_use]
    pub fn id(&self) -> JumboRopeObjectId {
        JumboRopeObjectId(interior_identity(&self.encode_wire()))
    }

    /// First leaf ordinal in this node's ordered range.
    #[must_use]
    pub const fn first_leaf(&self) -> u64 {
        self.first_leaf
    }

    /// Exact number of leaves in this node's subtree.
    #[must_use]
    pub const fn leaf_count(&self) -> u64 {
        self.leaf_count
    }

    /// Exact byte length of this node's subtree.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Left child identity and its exact authenticated range.
    #[must_use]
    pub const fn left(&self) -> JumboRopeProofSpan {
        self.left.to_claim()
    }

    /// Right child identity and its exact authenticated range.
    #[must_use]
    pub const fn right(&self) -> JumboRopeProofSpan {
        self.right.to_claim()
    }

    /// Fixed-size canonical object payload suitable for an existing typed CAS.
    #[must_use]
    pub fn encode_wire(&self) -> [u8; ROPE_NODE_WIRE_BYTES] {
        let mut bytes = [0; ROPE_NODE_WIRE_BYTES];
        bytes[..4].copy_from_slice(NODE_MAGIC);
        bytes[4..12].copy_from_slice(&self.first_leaf.to_be_bytes());
        bytes[12..20].copy_from_slice(&self.leaf_count.to_be_bytes());
        bytes[20..28].copy_from_slice(&self.byte_length.to_be_bytes());
        encode_child(&mut bytes[28..85], self.left);
        encode_child(&mut bytes[85..142], self.right);
        bytes
    }

    fn create(left: RopeObjectRef, right: RopeObjectRef) -> Result<Self, JumboRopeError> {
        validate_ref(left)?;
        validate_ref(right)?;
        let expected_right = left
            .first_leaf
            .checked_add(left.leaf_count)
            .ok_or(JumboRopeError::LengthOverflow)?;
        if right.first_leaf != expected_right {
            return Err(JumboRopeError::NonContiguousRopeRange);
        }
        let leaf_count = left
            .leaf_count
            .checked_add(right.leaf_count)
            .ok_or(JumboRopeError::LengthOverflow)?;
        let byte_length = left
            .byte_length
            .checked_add(right.byte_length)
            .ok_or(JumboRopeError::LengthOverflow)?;
        Ok(Self {
            first_leaf: left.first_leaf,
            leaf_count,
            byte_length,
            left,
            right,
        })
    }

    fn as_ref(self) -> RopeObjectRef {
        RopeObjectRef {
            kind: RopeObjectKind::Interior,
            id: self.id(),
            first_leaf: self.first_leaf,
            leaf_count: self.leaf_count,
            byte_length: self.byte_length,
        }
    }

    fn decode_wire(bytes: &[u8; ROPE_NODE_WIRE_BYTES]) -> Result<Self, JumboRopeError> {
        if bytes.get(..4) != Some(NODE_MAGIC.as_slice()) {
            return Err(JumboRopeError::InteriorObjectCorrupt);
        }
        let first_leaf = u64::from_be_bytes(read_array(bytes, 4)?);
        let leaf_count = u64::from_be_bytes(read_array(bytes, 12)?);
        let byte_length = u64::from_be_bytes(read_array(bytes, 20)?);
        let left = decode_child(
            bytes
                .get(28..85)
                .ok_or(JumboRopeError::InteriorObjectCorrupt)?,
        )?;
        let right = decode_child(
            bytes
                .get(85..142)
                .ok_or(JumboRopeError::InteriorObjectCorrupt)?,
        )?;
        let node = Self::create(left, right)?;
        if node.first_leaf != first_leaf
            || node.leaf_count != leaf_count
            || node.byte_length != byte_length
            || node.encode_wire() != *bytes
        {
            return Err(JumboRopeError::InteriorObjectCorrupt);
        }
        Ok(node)
    }
}

/// Object kind used by an untrusted proof span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum JumboRopeObjectKind {
    /// A content-defined leaf.
    Leaf = 0,
    /// An authenticated interior node.
    Interior = 1,
}

impl JumboRopeObjectKind {
    fn from_code(code: u8) -> Result<Self, JumboRopeError> {
        match code {
            0 => Ok(Self::Leaf),
            1 => Ok(Self::Interior),
            _ => Err(JumboRopeError::InteriorObjectCorrupt),
        }
    }
}

/// Untrusted object ID and ordered range carried by a leaf proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JumboRopeProofSpan {
    kind: JumboRopeObjectKind,
    id: [u8; 32],
    first_leaf: u64,
    leaf_count: u64,
    byte_length: u64,
}

impl JumboRopeProofSpan {
    /// Wraps one span from an untrusted transfer proof.
    #[must_use]
    pub const fn from_fields(
        kind: JumboRopeObjectKind,
        id: [u8; 32],
        first_leaf: u64,
        leaf_count: u64,
        byte_length: u64,
    ) -> Self {
        Self {
            kind,
            id,
            first_leaf,
            leaf_count,
            byte_length,
        }
    }

    /// Object kind claimed by the span.
    #[must_use]
    pub const fn kind(self) -> JumboRopeObjectKind {
        self.kind
    }

    /// Object identity bytes claimed by the span.
    #[must_use]
    pub const fn id(self) -> &[u8; 32] {
        &self.id
    }

    /// First leaf ordinal claimed by the span.
    #[must_use]
    pub const fn first_leaf(self) -> u64 {
        self.first_leaf
    }

    /// Leaf count claimed by the span.
    #[must_use]
    pub const fn leaf_count(self) -> u64 {
        self.leaf_count
    }

    /// Byte length claimed by the span.
    #[must_use]
    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }
}

/// Which side of the current leaf path contains a sibling subtree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JumboRopeProofSide {
    /// Sibling is ordered before the current subtree.
    Left,
    /// Sibling is ordered after the current subtree.
    Right,
}

/// One untrusted sibling claim in a bottom-up Merkle path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JumboRopeProofSibling {
    side: JumboRopeProofSide,
    sibling: JumboRopeProofSpan,
}

impl JumboRopeProofSibling {
    /// Creates one untrusted sibling claim.
    #[must_use]
    pub const fn new(side: JumboRopeProofSide, sibling: JumboRopeProofSpan) -> Self {
        Self { side, sibling }
    }

    /// Which side contains the sibling subtree.
    #[must_use]
    pub const fn side(self) -> JumboRopeProofSide {
        self.side
    }

    /// Sibling object ID and exact claimed range.
    #[must_use]
    pub const fn sibling(self) -> JumboRopeProofSpan {
        self.sibling
    }
}

/// Bottom-up authenticated path for one content-defined leaf.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JumboRopeProof {
    siblings: Box<[JumboRopeProofSibling]>,
}

impl JumboRopeProof {
    /// Builds a bounded path claim. Authenticity is established only when a
    /// checked descriptor admits the path and leaf bytes.
    pub fn new(siblings: Vec<JumboRopeProofSibling>) -> Result<Self, JumboRopeError> {
        if siblings.len() > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: siblings.len(),
                maximum: MAX_PROOF_DEPTH,
            });
        }
        Ok(Self {
            siblings: siblings.into_boxed_slice(),
        })
    }

    /// Bottom-up sibling claims.
    #[must_use]
    pub fn siblings(&self) -> &[JumboRopeProofSibling] {
        &self.siblings
    }
}

/// Leaf bytes whose ordinal, byte offset, content ID, and rope path have been
/// checked against one exact descriptor.
pub struct CheckedJumboLeaf<'bytes> {
    descriptor_id: JumboValueDescriptorId,
    id: JumboRopeObjectId,
    ordinal: u64,
    byte_offset: u64,
    bytes: &'bytes [u8],
    path_nodes: [Option<JumboRopeNode>; MAX_PROOF_DEPTH],
    path_node_count: usize,
}

impl<'bytes> CheckedJumboLeaf<'bytes> {
    /// Content identity of the checked leaf bytes.
    #[must_use]
    pub const fn id(&self) -> JumboRopeObjectId {
        self.id
    }

    /// Authenticated leaf ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Authenticated first byte offset in the complete value.
    #[must_use]
    pub const fn byte_offset(&self) -> u64 {
        self.byte_offset
    }

    /// Canonical bytes borrowed for this admission call only.
    #[must_use]
    pub const fn bytes(&self) -> &'bytes [u8] {
        self.bytes
    }
}

/// Resumable receiver for one checked descriptor. It records only bounded
/// per-leaf receipts and never exposes a complete token while leaves are
/// missing.
pub struct JumboRopeClosure {
    descriptor: CheckedJumboValueDescriptor,
    slots: Vec<Option<LeafReceipt>>,
    present_count: u64,
    missing_count: u64,
}

impl JumboRopeClosure {
    /// Checks and reserves bounded receipt metadata for a remote descriptor.
    pub fn new(
        descriptor: UntrustedJumboValueDescriptor,
        limits: JumboRopeLimits,
    ) -> Result<Self, JumboRopeError> {
        let limits = limits.validate()?;
        let descriptor = descriptor.check(limits)?;
        let leaf_count =
            usize::try_from(descriptor.leaf_count).map_err(|_| JumboRopeError::LengthOverflow)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(leaf_count)
            .map_err(|_| JumboRopeError::Allocation)?;
        let allocated_metadata = slots
            .capacity()
            .checked_mul(size_of::<Option<LeafReceipt>>())
            .ok_or(JumboRopeError::LengthOverflow)?;
        if allocated_metadata > limits.max_metadata_bytes {
            return Err(JumboRopeError::MetadataTooLarge {
                observed: allocated_metadata,
                maximum: limits.max_metadata_bytes,
            });
        }
        slots.resize(leaf_count, None);
        let missing_count = descriptor.leaf_count;
        Ok(Self {
            descriptor,
            slots,
            present_count: 0,
            missing_count,
        })
    }

    /// Structurally checked descriptor for this transfer.
    #[must_use]
    pub const fn descriptor(&self) -> &CheckedJumboValueDescriptor {
        &self.descriptor
    }

    /// Number of authenticated leaf objects admitted so far.
    #[must_use]
    pub const fn present_leaf_count(&self) -> u64 {
        self.present_count
    }

    /// Number of leaf ordinals still missing.
    #[must_use]
    pub const fn missing_leaf_count(&self) -> u64 {
        self.missing_count
    }

    /// Ordered missing ordinal ranges suitable for resumable range grants.
    #[must_use]
    pub fn missing_ranges(&self) -> MissingJumboLeafRanges<'_> {
        MissingJumboLeafRanges {
            slots: &self.slots,
            next: 0,
        }
    }

    /// Verifies one borrowed leaf against its content hash and complete rope
    /// path. The returned value is leaf-checked only; the whole descriptor is
    /// still unpublished until admission and final closure verification.
    pub fn check_leaf<'bytes>(
        &self,
        ordinal: u64,
        bytes: &'bytes [u8],
        proof: &JumboRopeProof,
    ) -> Result<CheckedJumboLeaf<'bytes>, JumboRopeError> {
        let index = usize::try_from(ordinal).map_err(|_| JumboRopeError::LengthOverflow)?;
        if self.slots.get(index).is_none() {
            return Err(JumboRopeError::LeafOrdinalOutOfRange {
                ordinal,
                leaf_count: self.descriptor.leaf_count,
            });
        }
        validate_leaf_length(ordinal, self.descriptor.leaf_count, bytes.len())?;
        if proof.siblings.len() > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: proof.siblings.len(),
                maximum: MAX_PROOF_DEPTH,
            });
        }
        let byte_length = u64::try_from(bytes.len()).map_err(|_| JumboRopeError::LengthOverflow)?;
        let id = JumboRopeObjectId(leaf_identity(bytes));
        let mut current = RopeObjectRef {
            kind: RopeObjectKind::Leaf,
            id,
            first_leaf: ordinal,
            leaf_count: 1,
            byte_length,
        };
        let mut byte_offset = 0_u64;
        let mut path_nodes = [None; MAX_PROOF_DEPTH];
        for (path_index, step) in proof.siblings.iter().copied().enumerate() {
            let sibling = RopeObjectRef::from_claim(step.sibling);
            validate_ref(sibling)?;
            let node = match step.side {
                JumboRopeProofSide::Left => {
                    let sibling_end = sibling
                        .first_leaf
                        .checked_add(sibling.leaf_count)
                        .ok_or(JumboRopeError::LengthOverflow)?;
                    let current_end = current
                        .first_leaf
                        .checked_add(current.leaf_count)
                        .ok_or(JumboRopeError::LengthOverflow)?;
                    if sibling_end != current.first_leaf
                        || ordinal < sibling.first_leaf
                        || ordinal < current.first_leaf
                        || ordinal >= current_end
                    {
                        return Err(JumboRopeError::NonContiguousRopeRange);
                    }
                    byte_offset = byte_offset
                        .checked_add(sibling.byte_length)
                        .ok_or(JumboRopeError::LengthOverflow)?;
                    JumboRopeNode::create(sibling, current)?
                }
                JumboRopeProofSide::Right => {
                    let current_end = current
                        .first_leaf
                        .checked_add(current.leaf_count)
                        .ok_or(JumboRopeError::LengthOverflow)?;
                    if current_end != sibling.first_leaf
                        || ordinal < current.first_leaf
                        || ordinal >= current_end
                    {
                        return Err(JumboRopeError::NonContiguousRopeRange);
                    }
                    JumboRopeNode::create(current, sibling)?
                }
            };
            current = node.as_ref();
            path_nodes[path_index] = Some(node);
        }
        if current != self.descriptor.root_ref()? {
            return Err(JumboRopeError::ProofRootMismatch);
        }
        Ok(CheckedJumboLeaf {
            descriptor_id: self.descriptor.id(),
            id,
            ordinal,
            byte_offset,
            bytes,
            path_nodes,
            path_node_count: proof.siblings.len(),
        })
    }

    /// Persists one checked leaf and its authenticated parent path. An exact
    /// retry is idempotent; a conflicting receipt for the same ordinal fails.
    pub fn admit_leaf<S: JumboRopeObjectSink + ?Sized>(
        &mut self,
        leaf: CheckedJumboLeaf<'_>,
        sink: &mut S,
    ) -> Result<(), JumboOperationError<S::Error>> {
        if leaf.descriptor_id != self.descriptor.id() {
            return Err(JumboRopeError::LeafDescriptorMismatch.into());
        }
        let index = usize::try_from(leaf.ordinal).map_err(|_| JumboRopeError::LengthOverflow)?;
        let Some(slot) = self.slots.get(index) else {
            return Err(JumboRopeError::LeafOrdinalOutOfRange {
                ordinal: leaf.ordinal,
                leaf_count: self.descriptor.leaf_count,
            }
            .into());
        };
        let receipt = LeafReceipt {
            id: leaf.id,
            byte_offset: leaf.byte_offset,
            byte_length: u64::try_from(leaf.bytes.len())
                .map_err(|_| JumboRopeError::LengthOverflow)?,
        };
        if let Some(previous) = slot {
            return if *previous == receipt {
                Ok(())
            } else {
                Err(JumboRopeError::ConflictingDuplicateLeaf.into())
            };
        }
        sink.write_leaf(JumboRopeLeafRef {
            id: leaf.id,
            ordinal: leaf.ordinal,
            byte_offset: leaf.byte_offset,
            bytes: leaf.bytes,
        })
        .map_err(JumboOperationError::Store)?;
        for node in leaf.path_nodes[..leaf.path_node_count].iter().flatten() {
            sink.write_interior(node)
                .map_err(JumboOperationError::Store)?;
        }
        self.slots[index] = Some(receipt);
        self.present_count += 1;
        self.missing_count -= 1;
        Ok(())
    }

    /// Verifies every ordinal, byte range, stored node, stored leaf, and
    /// complete UTF-8 value before minting the publication token.
    pub fn finish<S: JumboRopeObjectSource + ?Sized>(
        &self,
        source: &mut S,
    ) -> Result<VerifiedJumboRope, JumboOperationError<S::Error>> {
        if self.missing_count != 0 {
            return Err(JumboRopeError::MissingLeaves {
                missing: self.missing_count,
            }
            .into());
        }
        verify_complete_closure(&self.descriptor, &self.slots, source)?;
        Ok(VerifiedJumboRope {
            descriptor: self.descriptor,
        })
    }
}

/// Iterator over maximal adjacent missing leaf ranges.
pub struct MissingJumboLeafRanges<'closure> {
    slots: &'closure [Option<LeafReceipt>],
    next: usize,
}

impl Iterator for MissingJumboLeafRanges<'_> {
    type Item = Range<u64>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next < self.slots.len() && self.slots[self.next].is_some() {
            self.next += 1;
        }
        if self.next == self.slots.len() {
            return None;
        }
        let start = self.next;
        while self.next < self.slots.len() && self.slots[self.next].is_none() {
            self.next += 1;
        }
        Some(start as u64..self.next as u64)
    }
}

/// Writes a borrowed canonical value through a bounded chunk buffer and
/// object sink. The caller can put the returned descriptor in a future typed
/// Docs or SourceProvenance row schema.
pub fn write_jumbo_value<S: JumboRopeObjectSink + ?Sized>(
    context: JumboValueContext,
    bytes: &[u8],
    limits: JumboRopeLimits,
    sink: &mut S,
) -> Result<JumboRopeWriteReceipt, JumboOperationError<S::Error>> {
    let limits = limits.validate()?;
    let mut writer = RopeWriter::new(context, limits, sink)?;
    writer.push(bytes)?;
    writer.finish(0)
}

/// Streams a canonical value from a reader using a fixed input buffer and one
/// reusable bounded leaf buffer. The complete value is never materialized.
pub fn write_jumbo_value_from_reader<R, S>(
    context: JumboValueContext,
    reader: &mut R,
    limits: JumboRopeLimits,
    sink: &mut S,
) -> Result<JumboRopeWriteReceipt, JumboOperationError<S::Error>>
where
    R: Read + ?Sized,
    S: JumboRopeObjectSink + ?Sized,
{
    let limits = limits.validate()?;
    let mut writer = RopeWriter::new(context, limits, sink)?;
    let mut input = [0_u8; JUMBO_ROPE_STREAM_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut input)
            .map_err(JumboOperationError::Input)?;
        if read == 0 {
            break;
        }
        writer.push(&input[..read])?;
    }
    writer.finish(JUMBO_ROPE_STREAM_BUFFER_BYTES as u64)
}

/// Builds an authenticated inclusion path for one leaf from stored interior
/// objects. Every traversed node is rehashed and checked against its parent.
pub fn prove_jumbo_leaf<S: JumboRopeObjectSource + ?Sized>(
    descriptor: &CheckedJumboValueDescriptor,
    ordinal: u64,
    source: &mut S,
) -> Result<JumboRopeProof, JumboOperationError<S::Error>> {
    if ordinal >= descriptor.leaf_count {
        return Err(JumboRopeError::LeafOrdinalOutOfRange {
            ordinal,
            leaf_count: descriptor.leaf_count,
        }
        .into());
    }
    let mut current = descriptor.root_ref()?;
    let mut siblings = Vec::new();
    siblings
        .try_reserve(MAX_PROOF_DEPTH)
        .map_err(|_| JumboRopeError::Allocation)?;
    while current.kind == RopeObjectKind::Interior {
        if siblings.len() == MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: siblings.len() + 1,
                maximum: MAX_PROOF_DEPTH,
            }
            .into());
        }
        let bytes = source
            .read_interior(current.id)
            .map_err(JumboOperationError::Store)?
            .ok_or(JumboRopeError::MissingStoredInterior)?;
        let node = JumboRopeNode::decode_wire(&bytes)?;
        if node.id() != current.id || node.as_ref() != current {
            return Err(JumboRopeError::InteriorObjectCorrupt.into());
        }
        let target_end = ordinal
            .checked_add(1)
            .ok_or(JumboRopeError::LengthOverflow)?;
        let left_end = node
            .left
            .first_leaf
            .checked_add(node.left.leaf_count)
            .ok_or(JumboRopeError::LengthOverflow)?;
        if ordinal >= node.left.first_leaf && target_end <= left_end {
            siblings.push(JumboRopeProofSibling::new(
                JumboRopeProofSide::Right,
                node.right.to_claim(),
            ));
            current = node.left;
        } else if ordinal >= node.right.first_leaf
            && target_end
                <= node
                    .right
                    .first_leaf
                    .checked_add(node.right.leaf_count)
                    .ok_or(JumboRopeError::LengthOverflow)?
        {
            siblings.push(JumboRopeProofSibling::new(
                JumboRopeProofSide::Left,
                node.left.to_claim(),
            ));
            current = node.right;
        } else {
            return Err(JumboRopeError::InteriorObjectCorrupt.into());
        }
    }
    if current.first_leaf != ordinal || current.leaf_count != 1 {
        return Err(JumboRopeError::InteriorObjectCorrupt.into());
    }
    siblings.reverse();
    JumboRopeProof::new(siblings).map_err(JumboOperationError::Rope)
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
struct LeafReceipt {
    id: JumboRopeObjectId,
    byte_offset: u64,
    byte_length: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RopeObjectKind {
    Leaf,
    Interior,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RopeObjectRef {
    kind: RopeObjectKind,
    id: JumboRopeObjectId,
    first_leaf: u64,
    leaf_count: u64,
    byte_length: u64,
}

impl RopeObjectRef {
    fn from_claim(claim: JumboRopeProofSpan) -> Self {
        Self {
            kind: match claim.kind {
                JumboRopeObjectKind::Leaf => RopeObjectKind::Leaf,
                JumboRopeObjectKind::Interior => RopeObjectKind::Interior,
            },
            id: JumboRopeObjectId(claim.id),
            first_leaf: claim.first_leaf,
            leaf_count: claim.leaf_count,
            byte_length: claim.byte_length,
        }
    }

    const fn to_claim(self) -> JumboRopeProofSpan {
        JumboRopeProofSpan {
            kind: match self.kind {
                RopeObjectKind::Leaf => JumboRopeObjectKind::Leaf,
                RopeObjectKind::Interior => JumboRopeObjectKind::Interior,
            },
            id: self.id.0,
            first_leaf: self.first_leaf,
            leaf_count: self.leaf_count,
            byte_length: self.byte_length,
        }
    }
}

struct RopeWriter<'sink, S: JumboRopeObjectSink + ?Sized> {
    context: JumboValueContext,
    limits: JumboRopeLimits,
    sink: &'sink mut S,
    scratch: Vec<u8>,
    frontier: Vec<RopeObjectRef>,
    gear: [u64; 256],
    gear_window: [u8; GEAR_WINDOW_BYTES],
    gear_window_next: usize,
    gear_window_full: bool,
    rolling: u64,
    utf8: Utf8Validator,
    input_bytes: u64,
    emitted_bytes: u64,
    leaf_count: u64,
    interior_node_count: u64,
    peak_frontier_entries: usize,
}

impl<'sink, S: JumboRopeObjectSink + ?Sized> RopeWriter<'sink, S> {
    fn new(
        context: JumboValueContext,
        limits: JumboRopeLimits,
        sink: &'sink mut S,
    ) -> Result<Self, JumboOperationError<S::Error>> {
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(JUMBO_ROPE_MAX_LEAF_BYTES)
            .map_err(|_| JumboRopeError::Allocation)?;
        if scratch.capacity() > JUMBO_ROPE_MAX_LEAF_BYTES {
            return Err(JumboRopeError::ScratchCapacity {
                observed: scratch.capacity(),
                maximum: JUMBO_ROPE_MAX_LEAF_BYTES,
            }
            .into());
        }
        let mut frontier = Vec::new();
        frontier
            .try_reserve_exact(MAX_PROOF_DEPTH)
            .map_err(|_| JumboRopeError::Allocation)?;
        if frontier.capacity() > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::FrontierCapacity {
                observed: frontier.capacity(),
                maximum: MAX_PROOF_DEPTH,
            }
            .into());
        }
        Ok(Self {
            context,
            limits,
            sink,
            scratch,
            frontier,
            gear: gear_table(),
            gear_window: [0; GEAR_WINDOW_BYTES],
            gear_window_next: 0,
            gear_window_full: false,
            rolling: 0,
            utf8: Utf8Validator::default(),
            input_bytes: 0,
            emitted_bytes: 0,
            leaf_count: 0,
            interior_node_count: 0,
            peak_frontier_entries: 0,
        })
    }

    fn push(&mut self, input: &[u8]) -> Result<(), JumboOperationError<S::Error>> {
        for byte in input.iter().copied() {
            let next = self
                .input_bytes
                .checked_add(1)
                .ok_or(JumboRopeError::LengthOverflow)?;
            if next > self.limits.max_value_bytes {
                return Err(JumboRopeError::ValueTooLarge {
                    observed: next,
                    maximum: self.limits.max_value_bytes,
                }
                .into());
            }
            if self.context.encoding == JumboValueEncoding::Utf8 {
                self.utf8.push(byte)?;
            }
            self.scratch.push(byte);
            self.input_bytes = next;
            self.update_rolling_hash(byte);
            let reached_max = self.scratch.len() == JUMBO_ROPE_MAX_LEAF_BYTES;
            let target_cut =
                self.scratch.len() >= JUMBO_ROPE_MIN_LEAF_BYTES && (self.rolling & CUT_MASK) == 0;
            if reached_max || target_cut {
                self.flush_leaf()?;
            }
        }
        Ok(())
    }

    fn update_rolling_hash(&mut self, byte: u8) {
        let incoming = self.gear[byte as usize];
        self.rolling = self
            .rolling
            .wrapping_mul(GEAR_ROLLING_BASE)
            .wrapping_add(incoming);
        if self.gear_window_full {
            let outgoing = self.gear[self.gear_window[self.gear_window_next] as usize];
            self.rolling = self
                .rolling
                .wrapping_sub(outgoing.wrapping_mul(GEAR_ROLLING_POWER));
        }
        self.gear_window[self.gear_window_next] = byte;
        self.gear_window_next = (self.gear_window_next + 1) % GEAR_WINDOW_BYTES;
        if self.gear_window_next == 0 {
            self.gear_window_full = true;
        }
    }

    fn flush_leaf(&mut self) -> Result<(), JumboOperationError<S::Error>> {
        if self.scratch.is_empty() {
            return Ok(());
        }
        if self.leaf_count >= self.limits.max_leaf_count {
            return Err(JumboRopeError::TooManyLeaves {
                observed: self.leaf_count.saturating_add(1),
                maximum: self.limits.max_leaf_count,
            }
            .into());
        }
        let leaf_length =
            u64::try_from(self.scratch.len()).map_err(|_| JumboRopeError::LengthOverflow)?;
        let id = JumboRopeObjectId(leaf_identity(&self.scratch));
        self.sink
            .write_leaf(JumboRopeLeafRef {
                id,
                ordinal: self.leaf_count,
                byte_offset: self.emitted_bytes,
                bytes: &self.scratch,
            })
            .map_err(JumboOperationError::Store)?;
        let leaf = RopeObjectRef {
            kind: RopeObjectKind::Leaf,
            id,
            first_leaf: self.leaf_count,
            leaf_count: 1,
            byte_length: leaf_length,
        };
        self.leaf_count = self
            .leaf_count
            .checked_add(1)
            .ok_or(JumboRopeError::LengthOverflow)?;
        self.emitted_bytes = self
            .emitted_bytes
            .checked_add(leaf_length)
            .ok_or(JumboRopeError::LengthOverflow)?;
        self.frontier.push(leaf);
        self.peak_frontier_entries = self.peak_frontier_entries.max(self.frontier.len());
        self.scratch.clear();
        self.merge_equal_frontier()?;
        Ok(())
    }

    fn merge_equal_frontier(&mut self) -> Result<(), JumboOperationError<S::Error>> {
        while self.frontier.len() >= 2 {
            let right_index = self.frontier.len() - 1;
            let left_index = right_index - 1;
            if self.frontier[left_index].leaf_count != self.frontier[right_index].leaf_count {
                break;
            }
            let right = self
                .frontier
                .pop()
                .ok_or(JumboRopeError::ClosureCensusMismatch)?;
            let left = self
                .frontier
                .pop()
                .ok_or(JumboRopeError::ClosureCensusMismatch)?;
            let node = JumboRopeNode::create(left, right)?;
            self.sink
                .write_interior(&node)
                .map_err(JumboOperationError::Store)?;
            self.interior_node_count = self
                .interior_node_count
                .checked_add(1)
                .ok_or(JumboRopeError::LengthOverflow)?;
            self.frontier.push(node.as_ref());
        }
        self.peak_frontier_entries = self.peak_frontier_entries.max(self.frontier.len());
        Ok(())
    }

    fn finish(
        mut self,
        input_buffer_bytes: u64,
    ) -> Result<JumboRopeWriteReceipt, JumboOperationError<S::Error>> {
        if self.context.encoding == JumboValueEncoding::Utf8 {
            self.utf8.finish()?;
        }
        self.flush_leaf()?;
        while self.frontier.len() > 1 {
            let right = self
                .frontier
                .pop()
                .ok_or(JumboRopeError::ClosureCensusMismatch)?;
            let left = self
                .frontier
                .pop()
                .ok_or(JumboRopeError::ClosureCensusMismatch)?;
            let node = JumboRopeNode::create(left, right)?;
            self.sink
                .write_interior(&node)
                .map_err(JumboOperationError::Store)?;
            self.interior_node_count = self
                .interior_node_count
                .checked_add(1)
                .ok_or(JumboRopeError::LengthOverflow)?;
            self.frontier.push(node.as_ref());
        }
        if self.input_bytes != self.emitted_bytes {
            return Err(JumboRopeError::ClosureCensusMismatch.into());
        }
        let root = self
            .frontier
            .first()
            .map_or_else(empty_rope_root, |root| root.id);
        let untrusted = UntrustedJumboValueDescriptor::from_fields(
            self.context.owner,
            self.context.family,
            self.context.field_ordinal,
            self.context.encoding,
            self.input_bytes,
            self.leaf_count,
            root.0,
        );
        let descriptor = untrusted.check(self.limits)?;
        let verified = VerifiedJumboRope { descriptor };
        let frontier_bytes = self
            .frontier
            .capacity()
            .checked_mul(size_of::<RopeObjectRef>())
            .ok_or(JumboRopeError::LengthOverflow)?;
        let peak_live_scratch_bytes = size_of::<Self>()
            .checked_add(self.scratch.capacity())
            .and_then(|bytes| bytes.checked_add(frontier_bytes))
            .and_then(|bytes| bytes.checked_add(input_buffer_bytes as usize))
            .ok_or(JumboRopeError::LengthOverflow)?;
        let metrics = JumboRopeBuildMetrics {
            input_bytes: self.input_bytes,
            leaf_count: self.leaf_count,
            interior_node_count: self.interior_node_count,
            payload_hash_bytes: self.input_bytes,
            chunk_scratch_capacity_bytes: self.scratch.capacity() as u64,
            peak_live_scratch_bytes: peak_live_scratch_bytes as u64,
            peak_frontier_entries: self.peak_frontier_entries as u64,
            input_buffer_bytes,
        };
        Ok(JumboRopeWriteReceipt { verified, metrics })
    }
}

#[derive(Default)]
struct Utf8Validator {
    remaining: u8,
    next_min: u8,
    next_max: u8,
}

impl Utf8Validator {
    fn push(&mut self, byte: u8) -> Result<(), JumboRopeError> {
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

    fn finish(&self) -> Result<(), JumboRopeError> {
        if self.remaining == 0 {
            Ok(())
        } else {
            Err(JumboRopeError::InvalidUtf8)
        }
    }
}

fn validate_leaf_length(
    ordinal: u64,
    leaf_count: u64,
    observed: usize,
) -> Result<(), JumboRopeError> {
    let is_last = ordinal.saturating_add(1) == leaf_count;
    if observed == 0
        || observed > JUMBO_ROPE_MAX_LEAF_BYTES
        || (!is_last && observed < JUMBO_ROPE_MIN_LEAF_BYTES)
    {
        return Err(JumboRopeError::InvalidLeafLength { ordinal, observed });
    }
    Ok(())
}

fn validate_ref(reference: RopeObjectRef) -> Result<(), JumboRopeError> {
    if reference.leaf_count == 0
        || reference.byte_length == 0
        || (reference.kind == RopeObjectKind::Leaf && reference.leaf_count != 1)
        || (reference.kind == RopeObjectKind::Interior && reference.leaf_count < 2)
    {
        return Err(JumboRopeError::NonContiguousRopeRange);
    }
    reference
        .first_leaf
        .checked_add(reference.leaf_count)
        .ok_or(JumboRopeError::LengthOverflow)?;
    Ok(())
}

fn encode_child(output: &mut [u8], child: RopeObjectRef) {
    output[0] = match child.kind {
        RopeObjectKind::Leaf => JumboRopeObjectKind::Leaf as u8,
        RopeObjectKind::Interior => JumboRopeObjectKind::Interior as u8,
    };
    output[1..33].copy_from_slice(child.id.as_bytes());
    output[33..41].copy_from_slice(&child.first_leaf.to_be_bytes());
    output[41..49].copy_from_slice(&child.leaf_count.to_be_bytes());
    output[49..57].copy_from_slice(&child.byte_length.to_be_bytes());
}

fn decode_child(bytes: &[u8]) -> Result<RopeObjectRef, JumboRopeError> {
    if bytes.len() != 57 {
        return Err(JumboRopeError::InteriorObjectCorrupt);
    }
    let kind = match JumboRopeObjectKind::from_code(bytes[0])? {
        JumboRopeObjectKind::Leaf => RopeObjectKind::Leaf,
        JumboRopeObjectKind::Interior => RopeObjectKind::Interior,
    };
    let child = RopeObjectRef {
        kind,
        id: JumboRopeObjectId(read_array(bytes, 1)?),
        first_leaf: u64::from_be_bytes(read_array(bytes, 33)?),
        leaf_count: u64::from_be_bytes(read_array(bytes, 41)?),
        byte_length: u64::from_be_bytes(read_array(bytes, 49)?),
    };
    validate_ref(child)?;
    Ok(child)
}

fn read_array<const N: usize>(bytes: &[u8], start: usize) -> Result<[u8; N], JumboRopeError> {
    let end = start.checked_add(N).ok_or(JumboRopeError::LengthOverflow)?;
    bytes
        .get(start..end)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(JumboRopeError::DescriptorWireLength {
            expected: end,
            observed: bytes.len(),
        })
}

fn leaf_identity(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.leaf.v1");
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn interior_identity(bytes: &[u8; ROPE_NODE_WIRE_BYTES]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.interior.v1");
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn descriptor_identity(bytes: &[u8; DESCRIPTOR_WIRE_BYTES]) -> JumboValueDescriptorId {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.descriptor.v1");
    hasher.update(bytes);
    JumboValueDescriptorId(*hasher.finalize().as_bytes())
}

fn empty_rope_root() -> JumboRopeObjectId {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.empty.v1");
    hasher.update(b"empty\0");
    JumboRopeObjectId(*hasher.finalize().as_bytes())
}

fn gear_table() -> [u64; 256] {
    core::array::from_fn(|byte| {
        let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.gear.v1");
        hasher.update(&[byte as u8]);
        let digest = hasher.finalize();
        u64::from_le_bytes(digest.as_bytes()[..8].try_into().unwrap_or([0; 8]))
    })
}

fn verify_complete_closure<S: JumboRopeObjectSource + ?Sized>(
    descriptor: &CheckedJumboValueDescriptor,
    slots: &[Option<LeafReceipt>],
    source: &mut S,
) -> Result<(), JumboOperationError<S::Error>> {
    if descriptor.leaf_count == 0 {
        if descriptor.byte_length == 0 && descriptor.root == empty_rope_root() {
            return Ok(());
        }
        return Err(JumboRopeError::ClosureCensusMismatch.into());
    }
    let root = descriptor.root_ref()?;
    let mut stack = Vec::new();
    stack
        .try_reserve(MAX_PROOF_DEPTH)
        .map_err(|_| JumboRopeError::Allocation)?;
    stack.push((root, 0_usize));
    let mut next_leaf = 0_u64;
    let mut byte_offset = 0_u64;
    let mut node_count = 0_u64;
    let mut utf8 = Utf8Validator::default();
    let mut leaf_buffer = [0_u8; JUMBO_ROPE_MAX_LEAF_BYTES];
    while let Some((reference, depth)) = stack.pop() {
        if depth > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: depth,
                maximum: MAX_PROOF_DEPTH,
            }
            .into());
        }
        match reference.kind {
            RopeObjectKind::Leaf => {
                if reference.first_leaf != next_leaf || reference.leaf_count != 1 {
                    return Err(JumboRopeError::ClosureCensusMismatch.into());
                }
                let index =
                    usize::try_from(next_leaf).map_err(|_| JumboRopeError::LengthOverflow)?;
                let receipt = slots
                    .get(index)
                    .and_then(|slot| *slot)
                    .ok_or(JumboRopeError::ClosureCensusMismatch)?;
                if receipt.id != reference.id
                    || receipt.byte_offset != byte_offset
                    || receipt.byte_length != reference.byte_length
                {
                    return Err(JumboRopeError::ClosureCensusMismatch.into());
                }
                let byte_count = source
                    .read_leaf(reference.id, &mut leaf_buffer)
                    .map_err(JumboOperationError::Store)?
                    .ok_or(JumboRopeError::MissingStoredLeaf)?;
                let bytes = leaf_buffer
                    .get(..byte_count)
                    .ok_or(JumboRopeError::OversizedStoredLeaf)?;
                validate_leaf_length(next_leaf, descriptor.leaf_count, bytes.len())?;
                if u64::try_from(bytes.len()).map_err(|_| JumboRopeError::LengthOverflow)?
                    != reference.byte_length
                    || leaf_identity(bytes) != reference.id.0
                {
                    return Err(JumboRopeError::LeafObjectCorrupt.into());
                }
                if descriptor.encoding == JumboValueEncoding::Utf8 {
                    for byte in bytes.iter().copied() {
                        utf8.push(byte)?;
                    }
                }
                next_leaf = next_leaf
                    .checked_add(1)
                    .ok_or(JumboRopeError::LengthOverflow)?;
                byte_offset = byte_offset
                    .checked_add(reference.byte_length)
                    .ok_or(JumboRopeError::LengthOverflow)?;
            }
            RopeObjectKind::Interior => {
                let bytes = source
                    .read_interior(reference.id)
                    .map_err(JumboOperationError::Store)?
                    .ok_or(JumboRopeError::MissingStoredInterior)?;
                let node = JumboRopeNode::decode_wire(&bytes)?;
                if node.id() != reference.id || node.as_ref() != reference {
                    return Err(JumboRopeError::InteriorObjectCorrupt.into());
                }
                node_count = node_count
                    .checked_add(1)
                    .ok_or(JumboRopeError::LengthOverflow)?;
                stack.push((node.right, depth + 1));
                stack.push((node.left, depth + 1));
            }
        }
    }
    if next_leaf != descriptor.leaf_count
        || byte_offset != descriptor.byte_length
        || node_count != descriptor.leaf_count.saturating_sub(1)
    {
        return Err(JumboRopeError::ClosureCensusMismatch.into());
    }
    if descriptor.encoding == JumboValueEncoding::Utf8 {
        utf8.finish()?;
    }
    Ok(())
}

fn stream_verified_value<S, W>(
    descriptor: &CheckedJumboValueDescriptor,
    source: &mut S,
    writer: &mut W,
) -> Result<u64, JumboOperationError<S::Error>>
where
    S: JumboRopeObjectSource + ?Sized,
    W: Write,
{
    if descriptor.leaf_count == 0 {
        return Ok(0);
    }
    let mut stack = Vec::new();
    stack
        .try_reserve(MAX_PROOF_DEPTH)
        .map_err(|_| JumboRopeError::Allocation)?;
    stack.push((descriptor.root_ref()?, 0_usize));
    let mut next_leaf = 0_u64;
    let mut byte_offset = 0_u64;
    let mut utf8 = Utf8Validator::default();
    let mut leaf_buffer = [0_u8; JUMBO_ROPE_MAX_LEAF_BYTES];
    while let Some((reference, depth)) = stack.pop() {
        if depth > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: depth,
                maximum: MAX_PROOF_DEPTH,
            }
            .into());
        }
        match reference.kind {
            RopeObjectKind::Leaf => {
                if reference.first_leaf != next_leaf || reference.leaf_count != 1 {
                    return Err(JumboRopeError::ClosureCensusMismatch.into());
                }
                let byte_count = source
                    .read_leaf(reference.id, &mut leaf_buffer)
                    .map_err(JumboOperationError::Store)?
                    .ok_or(JumboRopeError::MissingStoredLeaf)?;
                let bytes = leaf_buffer
                    .get(..byte_count)
                    .ok_or(JumboRopeError::OversizedStoredLeaf)?;
                validate_leaf_length(next_leaf, descriptor.leaf_count, bytes.len())?;
                if u64::try_from(bytes.len()).map_err(|_| JumboRopeError::LengthOverflow)?
                    != reference.byte_length
                    || leaf_identity(bytes) != reference.id.0
                {
                    return Err(JumboRopeError::LeafObjectCorrupt.into());
                }
                if descriptor.encoding == JumboValueEncoding::Utf8 {
                    for byte in bytes.iter().copied() {
                        utf8.push(byte)?;
                    }
                }
                writer
                    .write_all(&bytes)
                    .map_err(JumboOperationError::Output)?;
                next_leaf = next_leaf
                    .checked_add(1)
                    .ok_or(JumboRopeError::LengthOverflow)?;
                byte_offset = byte_offset
                    .checked_add(reference.byte_length)
                    .ok_or(JumboRopeError::LengthOverflow)?;
            }
            RopeObjectKind::Interior => {
                let bytes = source
                    .read_interior(reference.id)
                    .map_err(JumboOperationError::Store)?
                    .ok_or(JumboRopeError::MissingStoredInterior)?;
                let node = JumboRopeNode::decode_wire(&bytes)?;
                if node.id() != reference.id || node.as_ref() != reference {
                    return Err(JumboRopeError::InteriorObjectCorrupt.into());
                }
                stack.push((node.right, depth + 1));
                stack.push((node.left, depth + 1));
            }
        }
    }
    if next_leaf != descriptor.leaf_count || byte_offset != descriptor.byte_length {
        return Err(JumboRopeError::ClosureCensusMismatch.into());
    }
    if descriptor.encoding == JumboValueEncoding::Utf8 {
        utf8.finish()?;
    }
    Ok(byte_offset)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use alloc::{collections::BTreeMap, vec};
    use core::convert::Infallible;
    use std::io::Cursor;

    use super::*;

    #[derive(Default)]
    struct MemoryObjects {
        leaves: BTreeMap<JumboRopeObjectId, Vec<u8>>,
        interiors: BTreeMap<JumboRopeObjectId, [u8; ROPE_NODE_WIRE_BYTES]>,
        leaf_order: Vec<JumboRopeObjectId>,
        report_oversized_leaf: bool,
    }

    impl JumboRopeObjectSink for MemoryObjects {
        type Error = Infallible;

        fn write_leaf(&mut self, leaf: JumboRopeLeafRef<'_>) -> Result<(), Self::Error> {
            assert_eq!(leaf.id.0, leaf_identity(leaf.bytes));
            let bytes = leaf.bytes.to_vec();
            if let Some(previous) = self.leaves.insert(leaf.id, bytes.clone()) {
                assert_eq!(previous, bytes);
            }
            self.leaf_order.push(leaf.id);
            Ok(())
        }

        fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error> {
            let id = node.id();
            let bytes = node.encode_wire();
            if let Some(previous) = self.interiors.insert(id, bytes) {
                assert_eq!(previous, bytes);
            }
            Ok(())
        }
    }

    impl JumboRopeObjectSource for MemoryObjects {
        type Error = Infallible;

        fn read_leaf(
            &mut self,
            id: JumboRopeObjectId,
            output: &mut [u8; JUMBO_ROPE_MAX_LEAF_BYTES],
        ) -> Result<Option<usize>, Self::Error> {
            if self.report_oversized_leaf {
                return Ok(Some(output.len() + 1));
            }
            let Some(bytes) = self.leaves.get(&id) else {
                return Ok(None);
            };
            if bytes.len() > output.len() {
                return Ok(Some(output.len() + 1));
            }
            output[..bytes.len()].copy_from_slice(bytes);
            Ok(Some(bytes.len()))
        }

        fn read_interior(
            &mut self,
            id: JumboRopeObjectId,
        ) -> Result<Option<[u8; ROPE_NODE_WIRE_BYTES]>, Self::Error> {
            Ok(self.interiors.get(&id).copied())
        }
    }

    fn context(encoding: JumboValueEncoding) -> JumboValueContext {
        JumboValueContext::new([0x5a; 32], JumboValueFamily::Documentation, 3, encoding)
    }

    fn deterministic_bytes(length: usize, seed: u64) -> Vec<u8> {
        let mut result = vec![0; length];
        let mut state = seed;
        for byte in &mut result {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        result
    }

    fn build(bytes: &[u8], encoding: JumboValueEncoding) -> (MemoryObjects, VerifiedJumboRope) {
        let mut objects = MemoryObjects::default();
        let written = write_jumbo_value(
            context(encoding),
            bytes,
            JumboRopeLimits::default(),
            &mut objects,
        )
        .expect("value should be admitted");
        (objects, written.verified().clone())
    }

    fn transfer_leaf(
        descriptor: CheckedJumboValueDescriptor,
        ordinal: u64,
        source: &mut MemoryObjects,
        closure: &mut JumboRopeClosure,
        destination: &mut MemoryObjects,
    ) {
        let proof =
            prove_jumbo_leaf(&descriptor, ordinal, source).expect("proof should be available");
        let leaf_id = *source
            .leaf_order
            .get(ordinal as usize)
            .expect("leaf ordinal should exist");
        let leaf_bytes = source
            .leaves
            .get(&leaf_id)
            .expect("source leaf should be present");
        let checked = closure
            .check_leaf(ordinal, &leaf_bytes, &proof)
            .expect("leaf path should be valid");
        closure
            .admit_leaf(checked, destination)
            .expect("checked leaf should be admitted");
    }

    #[test]
    fn descriptor_rejects_false_length_census_and_unknown_wire_tags() {
        let impossible = UntrustedJumboValueDescriptor::from_fields(
            [1; 32],
            JumboValueFamily::Documentation,
            0,
            JumboValueEncoding::Bytes,
            32,
            0,
            [2; 32],
        );
        assert_eq!(
            impossible.check(JumboRopeLimits::default()),
            Err(JumboRopeError::InvalidLeafCount)
        );

        let mut wire = UntrustedJumboValueDescriptor::from_fields(
            [1; 32],
            JumboValueFamily::SourceProvenance,
            9,
            JumboValueEncoding::Utf8,
            0,
            0,
            empty_rope_root().0,
        )
        .encode_wire();
        wire[5] = 99;
        assert_eq!(
            UntrustedJumboValueDescriptor::decode_wire(&wire),
            Err(JumboRopeError::UnknownFamily(99))
        );
    }

    #[test]
    fn empty_and_leaf_boundary_sizes_have_canonical_closures() {
        let sizes = [
            0,
            1,
            JUMBO_ROPE_MIN_LEAF_BYTES - 1,
            JUMBO_ROPE_MIN_LEAF_BYTES,
            JUMBO_ROPE_MAX_LEAF_BYTES,
            JUMBO_ROPE_MAX_LEAF_BYTES + 1,
        ];
        for size in sizes {
            let bytes = vec![0x41; size];
            let (mut source, verified) = build(&bytes, JumboValueEncoding::Bytes);
            let mut closure = JumboRopeClosure::new(
                UntrustedJumboValueDescriptor::decode_wire(&verified.descriptor().encode_wire())
                    .expect("descriptor should decode"),
                JumboRopeLimits::default(),
            )
            .expect("descriptor should check");
            let mut received = MemoryObjects::default();
            if size == 0 {
                assert_eq!(verified.descriptor().leaf_count(), 0);
                assert!(closure.missing_ranges().next().is_none());
            } else {
                for ordinal in 0..verified.descriptor().leaf_count() {
                    transfer_leaf(
                        *verified.descriptor(),
                        ordinal,
                        &mut source,
                        &mut closure,
                        &mut received,
                    );
                }
            }
            assert!(closure.finish(&mut received).is_ok());
        }
    }

    #[test]
    fn closure_rejects_a_source_that_reports_an_oversized_leaf() {
        let bytes = deterministic_bytes(700_000, 0xa11c_e55);
        let (mut original, verified) = build(&bytes, JumboValueEncoding::Bytes);
        let descriptor = *verified.descriptor();
        let mut closure = JumboRopeClosure::new(
            UntrustedJumboValueDescriptor::decode_wire(&descriptor.encode_wire())
                .expect("descriptor should decode"),
            JumboRopeLimits::default(),
        )
        .expect("descriptor should check");
        let mut received = MemoryObjects::default();
        for ordinal in 0..descriptor.leaf_count() {
            transfer_leaf(
                descriptor,
                ordinal,
                &mut original,
                &mut closure,
                &mut received,
            );
        }
        received.report_oversized_leaf = true;
        assert!(matches!(
            closure.finish(&mut received),
            Err(JumboOperationError::Rope(
                JumboRopeError::OversizedStoredLeaf
            ))
        ));
    }

    #[test]
    fn reader_blocks_do_not_change_content_defined_leaf_boundaries() {
        struct FragmentedReader<'bytes> {
            bytes: &'bytes [u8],
            next: usize,
            block: usize,
        }

        impl Read for FragmentedReader<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.next == self.bytes.len() {
                    return Ok(0);
                }
                let count = out.len().min(self.block).min(self.bytes.len() - self.next);
                out[..count].copy_from_slice(&self.bytes[self.next..self.next + count]);
                self.next += count;
                Ok(count)
            }
        }

        let bytes = deterministic_bytes(3 * 512 * 1024, 0xfeed_beef);
        let mut borrowed_objects = MemoryObjects::default();
        let borrowed = write_jumbo_value(
            context(JumboValueEncoding::Bytes),
            &bytes,
            JumboRopeLimits::default(),
            &mut borrowed_objects,
        )
        .expect("borrowed input should write");

        let mut reader_objects = MemoryObjects::default();
        let mut reader = FragmentedReader {
            bytes: &bytes,
            next: 0,
            block: 991,
        };
        let streamed = write_jumbo_value_from_reader(
            context(JumboValueEncoding::Bytes),
            &mut reader,
            JumboRopeLimits::default(),
            &mut reader_objects,
        )
        .expect("stream input should write");

        assert_eq!(
            borrowed.verified().descriptor(),
            streamed.verified().descriptor()
        );
        assert_eq!(borrowed_objects.leaf_order, reader_objects.leaf_order);
        assert!(
            streamed.metrics().chunk_scratch_capacity_bytes() <= JUMBO_ROPE_MAX_LEAF_BYTES as u64
        );
        let scratch_upper_bound = (JUMBO_ROPE_MAX_LEAF_BYTES
            + JUMBO_ROPE_STREAM_BUFFER_BYTES
            + MAX_PROOF_DEPTH * size_of::<RopeObjectRef>()
            + size_of::<RopeWriter<'static, MemoryObjects>>())
            as u64;
        assert!(streamed.metrics().peak_live_scratch_bytes() <= scratch_upper_bound);
        assert_eq!(
            streamed.metrics().input_buffer_bytes(),
            JUMBO_ROPE_STREAM_BUFFER_BYTES as u64
        );
    }

    #[test]
    fn utf8_round_trips_when_a_codepoint_crosses_reader_blocks() {
        let mut bytes = vec![b'a'; JUMBO_ROPE_STREAM_BUFFER_BYTES - 1];
        bytes.extend_from_slice("🧠".as_bytes());
        bytes.extend(vec![b'z'; 1_100_000]);
        let text = core::str::from_utf8(&bytes).expect("fixture is UTF-8");
        let mut original = MemoryObjects::default();
        let mut reader = Cursor::new(text.as_bytes());
        let written = write_jumbo_value_from_reader(
            context(JumboValueEncoding::Utf8),
            &mut reader,
            JumboRopeLimits::default(),
            &mut original,
        )
        .expect("UTF-8 value should write");
        let descriptor = *written.verified().descriptor();

        let mut closure = JumboRopeClosure::new(
            UntrustedJumboValueDescriptor::decode_wire(&descriptor.encode_wire())
                .expect("descriptor should decode"),
            JumboRopeLimits::default(),
        )
        .expect("descriptor should check");
        let mut received = MemoryObjects::default();
        for ordinal in (0..descriptor.leaf_count()).rev() {
            transfer_leaf(
                descriptor,
                ordinal,
                &mut original,
                &mut closure,
                &mut received,
            );
        }
        let complete = closure
            .finish(&mut received)
            .expect("full ordered UTF-8 closure should verify");
        let mut reassembled = Vec::new();
        complete
            .write_value_to(&mut received, &mut reassembled)
            .expect("verified value should reassemble");
        assert_eq!(reassembled, bytes);
        assert_eq!(core::str::from_utf8(&reassembled).ok(), Some(text));
    }

    #[test]
    fn missing_ranges_resume_and_bad_order_or_content_cannot_publish() {
        let bytes = deterministic_bytes(1536 * 1024, 0x1234_5678);
        let (mut original, verified) = build(&bytes, JumboValueEncoding::Bytes);
        let descriptor = *verified.descriptor();
        let mut closure = JumboRopeClosure::new(
            UntrustedJumboValueDescriptor::decode_wire(&descriptor.encode_wire())
                .expect("descriptor should decode"),
            JumboRopeLimits::default(),
        )
        .expect("descriptor should check");
        let mut received = MemoryObjects::default();
        let leaf_count = descriptor.leaf_count();

        let zero_proof =
            prove_jumbo_leaf(&descriptor, 0, &mut original).expect("first leaf proof should exist");
        let zero_id = original.leaf_order[0];
        let mut corrupt = original.leaves.get(&zero_id).expect("leaf exists").clone();
        corrupt[0] ^= 1;
        assert!(matches!(
            closure.check_leaf(0, &corrupt, &zero_proof),
            Err(JumboRopeError::ProofRootMismatch)
        ));

        let wrong_ordinal_id = original.leaf_order[1];
        let wrong_ordinal = original.leaves.get(&wrong_ordinal_id).expect("leaf exists");
        let wrong_proof =
            prove_jumbo_leaf(&descriptor, 1, &mut original).expect("proof should exist");
        assert!(closure.check_leaf(0, &wrong_ordinal, &wrong_proof).is_err());

        let checked = closure
            .check_leaf(
                0,
                &original.leaves.get(&zero_id).expect("leaf exists"),
                &zero_proof,
            )
            .expect("first leaf path should verify");
        closure
            .admit_leaf(checked, &mut received)
            .expect("first leaf should persist");
        let retry_bytes = original.leaves.get(&zero_id).expect("leaf exists");
        let retry = closure
            .check_leaf(0, &retry_bytes, &zero_proof)
            .expect("duplicate path should verify");
        closure
            .admit_leaf(retry, &mut received)
            .expect("an exact retry is idempotent");
        assert_eq!(closure.present_leaf_count(), 1);
        assert_eq!(closure.missing_leaf_count(), leaf_count - 1);
        assert_eq!(closure.missing_ranges().next(), Some(1..leaf_count));
        assert!(matches!(
            closure.finish(&mut received),
            Err(JumboOperationError::Rope(JumboRopeError::MissingLeaves {
                missing
            })) if missing == leaf_count - 1
        ));

        for ordinal in (1..leaf_count).rev() {
            transfer_leaf(
                descriptor,
                ordinal,
                &mut original,
                &mut closure,
                &mut received,
            );
        }
        assert!(closure.missing_ranges().next().is_none());
        assert!(closure.finish(&mut received).is_ok());
    }

    #[test]
    fn front_insertion_reuses_cdc_leaves_while_ordinal_blocks_shift() {
        fn ordinal_reused_bytes(before: &[u8], after: &[u8]) -> u64 {
            const BLOCK: usize = 512 * 1024;
            let old: BTreeMap<[u8; 32], usize> = before
                .chunks(BLOCK)
                .map(|chunk| (*blake3::hash(chunk).as_bytes(), chunk.len()))
                .collect();
            after
                .chunks(BLOCK)
                .filter_map(|chunk| {
                    old.get(blake3::hash(chunk).as_bytes())
                        .filter(|length| **length == chunk.len())
                        .map(|length| *length as u64)
                })
                .sum()
        }

        let before = deterministic_bytes(3 * 512 * 1024, 0x5eed_cafe);
        let mut after = vec![b'!'; 30];
        after.extend_from_slice(&before);
        let (mut before_store, before_rope) = build(&before, JumboValueEncoding::Bytes);
        let (mut after_store, after_rope) = build(&after, JumboValueEncoding::Bytes);

        let mut old_counts = BTreeMap::<JumboRopeObjectId, (u64, u64)>::new();
        for id in &before_store.leaf_order {
            let length = before_store
                .leaves
                .get(id)
                .expect("written leaf exists")
                .len() as u64;
            let entry = old_counts.entry(*id).or_default();
            entry.0 += 1;
            entry.1 = length;
        }
        let mut after_counts = BTreeMap::<JumboRopeObjectId, u64>::new();
        for id in &after_store.leaf_order {
            *after_counts.entry(*id).or_default() += 1;
        }
        let cdc_reused_occurrence_bytes = after_counts
            .iter()
            .filter_map(|(id, new_count)| {
                old_counts
                    .get(id)
                    .map(|(old_count, length)| old_count.min(new_count) * length)
            })
            .sum::<u64>();
        let ordinal_shared_bytes = ordinal_reused_bytes(&before, &after);
        assert_eq!(ordinal_shared_bytes, 0);
        assert!(cdc_reused_occurrence_bytes > ordinal_shared_bytes);
        assert_ne!(
            before_rope.descriptor().root_claim(),
            after_rope.descriptor().root_claim()
        );
        assert!(before_store.leaf_order.len() > 1);
        assert!(after_store.leaf_order.len() > 1);
    }

    #[test]
    fn repeated_and_low_entropy_inputs_stay_bounded_and_deterministic() {
        let repeated = vec![0x7f; 2 * 1024 * 1024];
        let (mut first_store, first) = build(&repeated, JumboValueEncoding::Bytes);
        let (mut second_store, second) = build(&repeated, JumboValueEncoding::Bytes);
        assert_eq!(first.descriptor(), second.descriptor());
        assert_eq!(first_store.leaf_order, second_store.leaf_order);
        assert!(first_store.leaf_order.iter().all(|id| {
            first_store
                .leaves
                .get(id)
                .is_some_and(|bytes| bytes.len() <= JUMBO_ROPE_MAX_LEAF_BYTES)
        }));
        let proof = prove_jumbo_leaf(first.descriptor(), 0, &mut first_store)
            .expect("repeated byte input still has a bounded proof");
        assert!(proof.siblings().len() <= 64);
    }
}
