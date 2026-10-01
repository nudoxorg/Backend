//! Canonical rope node records, object identities, and proof spans.
use alloc::{boxed::Box, vec::Vec};
use core::fmt;

use super::*;

const NODE_MAGIC: &[u8; 4] = b"JRN1";

/// Opaque content identity for one leaf or interior rope node.
#[repr(transparent)]
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JumboRopeObjectId(pub(super) [u8; 32]);

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

/// Checked canonical interior node. Its ID commits to both ordered children,
/// their exact leaf ranges, and their byte lengths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JumboRopeNode {
    pub(super) first_leaf: u64,
    pub(super) leaf_count: u64,
    pub(super) byte_length: u64,
    pub(super) left: RopeObjectRef,
    pub(super) right: RopeObjectRef,
    id: JumboRopeObjectId,
}

impl JumboRopeNode {
    /// Opaque semantic identity of this node.
    #[must_use]
    pub const fn id(&self) -> JumboRopeObjectId {
        self.id
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

    pub(super) fn create(
        left: RopeObjectRef,
        right: RopeObjectRef,
    ) -> Result<Self, JumboRopeError> {
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
        let mut node = Self {
            first_leaf: left.first_leaf,
            leaf_count,
            byte_length,
            left,
            right,
            id: JumboRopeObjectId([0; 32]),
        };
        node.id = JumboRopeObjectId(interior_identity(&node.encode_wire()));
        Ok(node)
    }

    pub(super) fn as_ref(self) -> RopeObjectRef {
        RopeObjectRef {
            kind: RopeObjectKind::Interior,
            id: self.id(),
            first_leaf: self.first_leaf,
            leaf_count: self.leaf_count,
            byte_length: self.byte_length,
        }
    }

    pub(super) fn decode_wire(bytes: &[u8; ROPE_NODE_WIRE_BYTES]) -> Result<Self, JumboRopeError> {
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
    pub const fn id(&self) -> &[u8; 32] {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RopeObjectKind {
    Leaf,
    Interior,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RopeObjectRef {
    pub(super) kind: RopeObjectKind,
    pub(super) id: JumboRopeObjectId,
    pub(super) first_leaf: u64,
    pub(super) leaf_count: u64,
    pub(super) byte_length: u64,
}

impl RopeObjectRef {
    pub(super) fn from_claim(claim: JumboRopeProofSpan) -> Self {
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

    pub(super) const fn to_claim(self) -> JumboRopeProofSpan {
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

pub(super) fn validate_leaf_length(
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

pub(super) fn validate_ref(reference: RopeObjectRef) -> Result<(), JumboRopeError> {
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

pub(super) fn read_array<const N: usize>(
    bytes: &[u8],
    start: usize,
) -> Result<[u8; N], JumboRopeError> {
    let end = start.checked_add(N).ok_or(JumboRopeError::LengthOverflow)?;
    bytes
        .get(start..end)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(JumboRopeError::DescriptorWireLength {
            expected: end,
            observed: bytes.len(),
        })
}

pub(super) fn leaf_identity(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.leaf.v1");
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

pub(super) fn interior_identity(bytes: &[u8; ROPE_NODE_WIRE_BYTES]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.interior.v1");
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}
