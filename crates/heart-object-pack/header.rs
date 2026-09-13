//! Defines header behavior for `heart-object-pack`, whose purpose is to write and borrow indexed immutable object packs.
//! This module owns the header invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;

use crate::{
    ObjectPackBytes, ObjectPackError, ObjectPackObjectCount,
    format::{COUNT_BYTES, index_bytes},
};

/// Exact fixed byte width of a canonical object-pack count header.
pub const OBJECT_PACK_HEADER_BYTES: usize = COUNT_BYTES;

/// Immutable facts established by one complete fixed pack count record.
pub struct ObjectPackHeaderFacts<'bytes> {
    /// The exact borrowed eight-byte count record.
    pub bytes: &'bytes [u8; OBJECT_PACK_HEADER_BYTES],
    /// Number of directory records selected by `bytes`.
    pub object_count: ObjectPackObjectCount,
    pub(crate) native_count: usize,
    /// Exact count-record-plus-directory extent to request next.
    pub index_bytes: ObjectPackBytes,
}

/// Borrowed fixed-width pack count header and its measured directory extent.
pub struct ObjectPackHeader<'bytes> {
    facts: ObjectPackHeaderFacts<'bytes>,
}

impl<'bytes> Deref for ObjectPackHeader<'bytes> {
    type Target = ObjectPackHeaderFacts<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'bytes> TryFrom<&'bytes [u8]> for ObjectPackHeader<'bytes> {
    type Error = ObjectPackError;

    fn try_from(bytes: &'bytes [u8]) -> Result<Self, Self::Error> {
        let expected = ObjectPackBytes::from(OBJECT_PACK_HEADER_BYTES);
        let bytes = match <&[u8; OBJECT_PACK_HEADER_BYTES]>::try_from(bytes) {
            Ok(bytes) => bytes,
            Err(_) if bytes.len() < OBJECT_PACK_HEADER_BYTES => {
                return Err(ObjectPackError::HeaderTruncated {
                    required: expected,
                    available: bytes.len().into(),
                });
            }
            Err(_) => {
                return Err(ObjectPackError::HeaderTrailing {
                    expected,
                    actual: bytes.len().into(),
                });
            }
        };
        let object_count = ObjectPackObjectCount::from(u64::from_be_bytes(*bytes));
        let count = usize::try_from(*object_count).map_err(|source| {
            ObjectPackError::CountAddressSpace {
                count: object_count,
                source,
            }
        })?;
        Ok(Self {
            facts: ObjectPackHeaderFacts {
                bytes,
                object_count,
                native_count: count,
                index_bytes: index_bytes(count)?.into(),
            },
        })
    }
}
