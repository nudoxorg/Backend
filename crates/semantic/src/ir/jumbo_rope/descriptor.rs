//! Fixed-width descriptor wire form, policy checks, and descriptor identity.
use core::mem::size_of;

use super::wire::{RopeObjectKind, RopeObjectRef, read_array};
use super::*;

const DESCRIPTOR_MAGIC: &[u8; 4] = b"JVD1";
const DESCRIPTOR_VERSION: u8 = 1;
const DESCRIPTOR_WIRE_BYTES: usize = super::JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES;

/// Content identity of the complete typed value descriptor.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JumboValueDescriptorId(pub(super) [u8; 32]);

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
    pub const fn root_claim(&self) -> &[u8; 32] {
        &self.root
    }
}

/// Structurally checked descriptor; its root remains unproven until a full
/// content-addressed leaf closure has been checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckedJumboValueDescriptor {
    pub(super) owner: [u8; 32],
    pub(super) family: JumboValueFamily,
    pub(super) field_ordinal: u32,
    pub(super) encoding: JumboValueEncoding,
    pub(super) byte_length: u64,
    pub(super) leaf_count: u64,
    pub(super) root: JumboRopeObjectId,
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

    pub(super) fn root_ref(&self) -> Result<RopeObjectRef, JumboRopeError> {
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

pub(super) fn descriptor_identity(bytes: &[u8; DESCRIPTOR_WIRE_BYTES]) -> JumboValueDescriptorId {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.descriptor.v1");
    hasher.update(bytes);
    JumboValueDescriptorId(*hasher.finalize().as_bytes())
}

pub(super) fn empty_rope_root() -> JumboRopeObjectId {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.empty.v1");
    hasher.update(b"empty\0");
    JumboRopeObjectId(*hasher.finalize().as_bytes())
}
