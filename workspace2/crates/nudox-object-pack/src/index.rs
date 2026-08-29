use core::ops::Deref;

use nudox_object::ObjectLength;
use nudox_schema::SchemaId;
use zerocopy::FromBytes;

use crate::{
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
        let index_bytes = usize::from(header.index_bytes);
        if bytes.len() != index_bytes {
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
        let directory =
            <[DirectoryRecord]>::ref_from_bytes_with_elems(directory_bytes, header.native_count)
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

fn validate(directory: &[DirectoryRecord]) -> Result<usize, ObjectPackError> {
    let mut previous = None;
    let mut expected = 0_usize;
    for (ordinal, row) in directory.iter().enumerate() {
        let descriptor = &row.descriptor;
        SchemaId::try_from(descriptor.schema.get())
            .map_err(|source| ObjectPackError::DirectorySchema { ordinal, source })?;
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
