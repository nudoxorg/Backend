//! Defines format behavior for `heart-object-pack`, whose purpose is to write and borrow indexed immutable object packs.
//! This module owns the format invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{mem::size_of, ops::Deref};

use heart_object::ObjectDescriptorWireRecord;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned,
    byteorder::{BigEndian, U64},
};

#[repr(transparent)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
pub(crate) struct ObjectCountRecord(pub(crate) U64<BigEndian>);

#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned)]
pub(crate) struct DirectoryRecord {
    pub(crate) descriptor: ObjectDescriptorWireRecord,
    pub(crate) body_end: U64<BigEndian>,
}

pub(crate) const COUNT_BYTES: usize = size_of::<ObjectCountRecord>();
pub(crate) const DIRECTORY_BYTES: usize = size_of::<DirectoryRecord>();
const _: [(); size_of::<u64>()] = [(); COUNT_BYTES];
const _: [(); heart_object::OBJECT_DESCRIPTOR_RECORD_BYTES + size_of::<u64>()] =
    [(); DIRECTORY_BYTES];

/// Exact caller-buffer or canonical pack extent.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ObjectPackBytes(usize);

impl From<usize> for ObjectPackBytes {
    fn from(bytes: usize) -> Self {
        Self(bytes)
    }
}

impl From<ObjectPackBytes> for usize {
    fn from(bytes: ObjectPackBytes) -> Self {
        bytes.0
    }
}

impl Deref for ObjectPackBytes {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Exact object cardinality declared by one canonical pack header.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ObjectPackObjectCount(u64);

impl From<u64> for ObjectPackObjectCount {
    fn from(count: u64) -> Self {
        Self(count)
    }
}

impl Deref for ObjectPackObjectCount {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub(crate) fn checked_add(left: usize, right: usize) -> Result<usize, crate::ObjectPackError> {
    left.checked_add(right)
        .ok_or(crate::ObjectPackError::LayoutOverflow {
            left: left.into(),
            right: right.into(),
        })
}

pub(crate) fn index_bytes(count: usize) -> Result<usize, crate::ObjectPackError> {
    let directory =
        count
            .checked_mul(DIRECTORY_BYTES)
            .ok_or(crate::ObjectPackError::IndexLayoutOverflow {
                count: u64_from_usize(count).into(),
                row_bytes: DIRECTORY_BYTES.into(),
            })?;
    checked_add(COUNT_BYTES, directory)
}

#[allow(
    clippy::as_conversions,
    reason = "the crate target gate limits usize to 32 or 64 bits, both losslessly representable as u64"
)]
pub(crate) const fn u64_from_usize(value: usize) -> u64 {
    value as u64
}

#[cfg(test)]
mod tests {
    use crate::{ObjectPackBytes, ObjectPackError, ObjectPackObjectCount};

    use super::{DIRECTORY_BYTES, index_bytes, u64_from_usize};

    #[test]
    fn index_geometry_rejects_an_unrepresentable_directory() {
        assert_eq!(
            index_bytes(usize::MAX),
            Err(ObjectPackError::IndexLayoutOverflow {
                count: ObjectPackObjectCount::from(u64_from_usize(usize::MAX)),
                row_bytes: ObjectPackBytes::from(DIRECTORY_BYTES),
            })
        );
    }
}
