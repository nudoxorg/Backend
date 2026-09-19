//! Defines index behavior for `backend_store::object_pack`, whose purpose is to write and borrow indexed immutable object packs.
//! This module owns the index invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;

use backend_version::ObjectDomain;
use backend_version::object::{
    OBJECT_DESCRIPTOR_RECORD_BYTES, ObjectDescriptorDecodeError, ObjectLength, ObjectRef,
};
use zerocopy::TryFromBytes;

use super::{
    OBJECT_PACK_HEADER_BYTES, ObjectPackBytes, ObjectPackError, ObjectPackHeader,
    ObjectPackObjectCount,
    format::{DirectoryRecord, checked_add},
};

/// Immutable facts established by one complete canonical object-pack index.
pub struct ObjectPackIndexFacts<'bytes> {
    /// Exact borrowed header-and-directory bytes validated by this witness.
    pub bytes: &'bytes [u8],
    /// Exact number of validated fixed directory records.
    pub object_count: ObjectPackObjectCount,
    /// Exact header-and-directory extent borrowed in `bytes`.
    pub index_bytes: ObjectPackBytes,
    /// Exact full pack extent implied by the validated cumulative directory end.
    pub pack_bytes: ObjectPackBytes,
}

/// Borrowed validation witness for a complete fixed object-pack directory.
pub struct ObjectPackIndex<'bytes> {
    facts: ObjectPackIndexFacts<'bytes>,
    pub(crate) directory: &'bytes [DirectoryRecord],
}

impl<'bytes> Deref for ObjectPackIndex<'bytes> {
    type Target = ObjectPackIndexFacts<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'bytes> TryFrom<&'bytes [u8]> for ObjectPackIndex<'bytes> {
    type Error = ObjectPackError;

    fn try_from(bytes: &'bytes [u8]) -> Result<Self, Self::Error> {
        let header_bytes =
            bytes
                .get(..OBJECT_PACK_HEADER_BYTES)
                .ok_or(ObjectPackError::HeaderTruncated {
                    required: OBJECT_PACK_HEADER_BYTES.into(),
                    available: bytes.len().into(),
                })?;
        let header = ObjectPackHeader::try_from(header_bytes)?;
        Self::from_header(bytes, &header)
    }
}

impl<'bytes> ObjectPackIndex<'bytes> {
    pub(crate) fn complete(bytes: &'bytes [u8]) -> Result<Self, ObjectPackError> {
        let header_bytes =
            bytes
                .get(..OBJECT_PACK_HEADER_BYTES)
                .ok_or(ObjectPackError::HeaderTruncated {
                    required: OBJECT_PACK_HEADER_BYTES.into(),
                    available: bytes.len().into(),
                })?;
        let header = ObjectPackHeader::try_from(header_bytes)?;
        let index_bytes = usize::from(header.index_bytes);
        let index = bytes
            .get(..index_bytes)
            .ok_or(ObjectPackError::DirectoryExtent {
                expected: header.index_bytes,
                actual: bytes.len().into(),
            })?;
        let index = Self::from_header(index, &header)?;
        if bytes.len() != usize::from(index.pack_bytes) {
            return Err(ObjectPackError::PackExtent {
                expected: index.pack_bytes,
                actual: bytes.len().into(),
            });
        }
        Ok(index)
    }

    fn from_header(
        bytes: &'bytes [u8],
        header: &ObjectPackHeader<'bytes>,
    ) -> Result<Self, ObjectPackError> {
        let index_bytes = usize::from(header.index_bytes);
        if bytes.len() != usize::from(header.index_bytes) {
            return Err(ObjectPackError::DirectoryExtent {
                expected: header.index_bytes,
                actual: bytes.len().into(),
            });
        }
        let directory_bytes =
            bytes
                .get(OBJECT_PACK_HEADER_BYTES..)
                .ok_or(ObjectPackError::DirectoryExtent {
                    expected: header.index_bytes,
                    actual: bytes.len().into(),
                })?;
        validate_directory_schemas(directory_bytes)?;
        let directory = <[DirectoryRecord]>::try_ref_from_bytes_with_elems(
            directory_bytes,
            header.native_count,
        )
        .map_err(|source| ObjectPackError::DirectoryExtent {
            expected: header.index_bytes,
            actual: source.into_src().len().into(),
        })?;
        let body_bytes = validate(directory)?;
        Ok(Self {
            facts: ObjectPackIndexFacts {
                bytes,
                object_count: header.object_count,
                index_bytes: header.index_bytes,
                pack_bytes: checked_add(index_bytes, body_bytes)?.into(),
            },
            directory,
        })
    }
}

fn validate_directory_schemas(bytes: &[u8]) -> Result<(), ObjectPackError> {
    for (ordinal, row) in bytes
        .chunks_exact(super::format::DIRECTORY_BYTES)
        .enumerate()
    {
        let descriptor =
            row.get(..OBJECT_DESCRIPTOR_RECORD_BYTES)
                .ok_or(ObjectPackError::DirectoryExtent {
                    expected: bytes.len().into(),
                    actual: row.len().into(),
                })?;
        match ObjectRef::<ObjectDomain>::try_from(descriptor) {
            Ok(_) => {}
            Err(ObjectDescriptorDecodeError::Schema(source)) => {
                return Err(ObjectPackError::DirectorySchema { ordinal, source });
            }
            Err(ObjectDescriptorDecodeError::Width { actual }) => {
                return Err(ObjectPackError::DirectoryExtent {
                    expected: OBJECT_DESCRIPTOR_RECORD_BYTES.into(),
                    actual: actual.into(),
                });
            }
            Err(ObjectDescriptorDecodeError::Content(source)) => {
                return Err(ObjectPackError::DirectoryContent { ordinal, source });
            }
        }
    }
    Ok(())
}

fn validate(directory: &[DirectoryRecord]) -> Result<usize, ObjectPackError> {
    let mut previous = None;
    let mut expected = 0_usize;
    for (ordinal, row) in directory.iter().enumerate() {
        let descriptor = &row.descriptor;
        if let Some(previous) = previous
            && previous >= descriptor.content
        {
            return Err(ObjectPackError::DirectoryOrder {
                ordinal,
                previous,
                current: descriptor.content,
            });
        }
        let declared = ObjectLength::from(descriptor.length.get());
        let length = usize::try_from(*declared).map_err(|source| {
            ObjectPackError::DirectoryLengthAddressSpace {
                ordinal,
                declared,
                source,
            }
        })?;
        let left = expected;
        expected =
            left.checked_add(length)
                .ok_or(ObjectPackError::DirectoryCumulativeOverflow {
                    ordinal,
                    left: left.into(),
                    right: length.into(),
                })?;
        let observed = row.body_end.get();
        let observed = usize::try_from(observed).map_err(|source| {
            ObjectPackError::DirectoryEndAddressSpace {
                ordinal,
                observed,
                source,
            }
        })?;
        if expected != observed {
            return Err(ObjectPackError::DirectoryCumulativeEnd {
                ordinal,
                expected: expected.into(),
                observed: observed.into(),
            });
        }
        previous = Some(descriptor.content);
    }
    Ok(expected)
}
