//! The sole canonical byte record for one immutable object descriptor.

use core::mem::{align_of, offset_of, size_of};

use nudox_id::{ContentIdDecodeError, Domain, FixedCanonicalRecord};
use nudox_schema::{SchemaId, UnknownSchemaId};
use thiserror::Error;
use zerocopy::{
    Immutable, IntoBytes, KnownLayout, TryFromBytes, Unalign, Unaligned,
    byteorder::{BigEndian, U16, U64},
};

use crate::{ObjectKind, ObjectLength, ObjectRef};

/// Exact canonical object-descriptor record width, derived from its only wire declaration.
pub const OBJECT_DESCRIPTOR_RECORD_BYTES: usize = size_of::<ObjectDescriptorWireRecord>();

/// Declarative portable descriptor record shared by hashing, direct output,
/// streaming adapters, and borrowed artifact regions.
#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned)]
pub struct ObjectDescriptorWireRecord {
    /// Exact canonical content-identity cell.
    pub content: [u8; 32],
    /// Exact canonical object-length cell.
    pub length: U64<BigEndian>,
    /// Closed schema discriminant cell.
    pub schema: Unalign<SchemaId>,
    /// Opaque schema-kind cell.
    pub kind: U16<BigEndian>,
}

const _: [(); 1] = [(); align_of::<ObjectDescriptorWireRecord>()];
const _: [(); 46] = [(); size_of::<ObjectDescriptorWireRecord>()];
const SCHEMA_OFFSET: usize = offset_of!(ObjectDescriptorWireRecord, schema);
const SCHEMA_BYTES: usize = size_of::<SchemaId>();

/// Caller output shorter than the fixed canonical descriptor record.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error(
    "object descriptor output has {available} bytes but needs the fixed {OBJECT_DESCRIPTOR_RECORD_BYTES}-byte record"
)]
pub struct ObjectDescriptorOutputTooSmall {
    /// Complete caller-provided target length.
    pub available: usize,
}

/// Owned canonical descriptor decoding failure.
#[derive(Debug, Error)]
pub enum ObjectDescriptorDecodeError {
    /// Input was not exactly one canonical descriptor record.
    #[error(
        "object descriptor input has {actual} bytes, expected {OBJECT_DESCRIPTOR_RECORD_BYTES}"
    )]
    Width {
        /// Complete observed input length.
        actual: usize,
    },
    /// The record carried an unknown closed schema discriminant.
    #[error("object descriptor schema is unknown")]
    Schema(#[from] UnknownSchemaId),
    /// The record carried a content identity from another closed domain.
    #[error("object descriptor content identity is not an object identity")]
    Content(#[from] ContentIdDecodeError),
}

impl PartialEq for ObjectDescriptorDecodeError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Width { actual }, Self::Width { actual: other }) => actual == other,
            (Self::Schema(left), Self::Schema(right)) => left == right,
            (Self::Content(left), Self::Content(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for ObjectDescriptorDecodeError {}

impl<DomainTag> From<&ObjectRef<DomainTag>> for ObjectDescriptorWireRecord {
    fn from(reference: &ObjectRef<DomainTag>) -> Self {
        Self {
            content: *reference.content,
            length: U64::new(*reference.length),
            schema: Unalign::new(reference.schema),
            kind: U16::new(*reference.kind),
        }
    }
}

impl<DomainTag: Domain> TryFrom<&ObjectDescriptorWireRecord> for ObjectRef<DomainTag> {
    type Error = ObjectDescriptorDecodeError;

    fn try_from(record: &ObjectDescriptorWireRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            content: nudox_id::ContentId::try_from(record.content)?,
            length: ObjectLength::from(record.length.get()),
            schema: record.schema.get(),
            kind: ObjectKind::from(record.kind.get()),
        })
    }
}

impl<DomainTag: Domain> TryFrom<&[u8]> for ObjectRef<DomainTag> {
    type Error = ObjectDescriptorDecodeError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.len() != OBJECT_DESCRIPTOR_RECORD_BYTES {
            return Err(ObjectDescriptorDecodeError::Width {
                actual: bytes.len(),
            });
        }
        let schema = bytes
            .get(SCHEMA_OFFSET..SCHEMA_OFFSET + SCHEMA_BYTES)
            .and_then(|bytes| <&[u8; SCHEMA_BYTES]>::try_from(bytes).ok())
            .ok_or(ObjectDescriptorDecodeError::Width {
                actual: bytes.len(),
            })?;
        let observed = u32::from_be_bytes(*schema);
        let Ok(record) = ObjectDescriptorWireRecord::try_ref_from_bytes(bytes) else {
            return Err(ObjectDescriptorDecodeError::Schema(UnknownSchemaId(
                observed,
            )));
        };
        Self::try_from(record)
    }
}

impl ObjectDescriptorWireRecord {
    /// Writes this exact record into a caller-owned output prefix.
    ///
    /// Oversized output is accepted and its tail remains untouched.  A short
    /// target is rejected before the record changes any byte.
    ///
    /// # Errors
    ///
    /// Returns [`ObjectDescriptorOutputTooSmall`] before
    /// mutation when the caller output lacks this fixed record width.
    pub fn write_into(self, output: &mut [u8]) -> Result<(), ObjectDescriptorOutputTooSmall> {
        let available = output.len();
        let target = output
            .get_mut(..OBJECT_DESCRIPTOR_RECORD_BYTES)
            .ok_or(ObjectDescriptorOutputTooSmall { available })?;
        target.copy_from_slice(self.as_bytes());
        Ok(())
    }
}

impl FixedCanonicalRecord<OBJECT_DESCRIPTOR_RECORD_BYTES> for ObjectDescriptorWireRecord {
    fn canonical_bytes(&self) -> &[u8; OBJECT_DESCRIPTOR_RECORD_BYTES] {
        zerocopy::transmute_ref!(self)
    }
}

#[cfg(test)]
mod tests {
    use nudox_id::{
        ContentHasher, ContentId, DependencySetDomain, FixedCanonicalRecord, ObjectDomain,
    };
    use nudox_schema::SchemaId;
    use thiserror::Error;
    use zerocopy::IntoBytes;

    use crate::{
        ObjectDescriptorDecodeError, ObjectDescriptorOutputTooSmall, ObjectDescriptorWireRecord,
        ObjectRef,
    };

    use super::OBJECT_DESCRIPTOR_RECORD_BYTES;

    #[derive(Debug, Error)]
    enum RecordTestError {
        #[error("expected unknown schema rejection")]
        ExpectedUnknownSchema,
        #[error("fixed record prefix {length} was unexpectedly absent")]
        MissingPrefix { length: usize },
        #[error("unexpected descriptor decode error: {0}")]
        UnexpectedDecode(ObjectDescriptorDecodeError),
    }

    fn object() -> ObjectRef<ObjectDomain> {
        ObjectRef {
            content: ContentId::from_digest([7; 32]),
            length: 12_u64.into(),
            schema: SchemaId::Object,
            kind: 3_u16.into(),
        }
    }

    #[test]
    fn canonical_record_has_exact_golden_bytes_and_round_trips() {
        let reference = object();
        let record = ObjectDescriptorWireRecord::from(&reference);
        let mut expected = [7_u8; OBJECT_DESCRIPTOR_RECORD_BYTES];
        expected[0] = 1;
        expected[32..40].copy_from_slice(&12_u64.to_be_bytes());
        expected[40..44].copy_from_slice(&u32::from(SchemaId::Object).to_be_bytes());
        expected[44..46].copy_from_slice(&3_u16.to_be_bytes());
        assert_eq!(record.as_bytes(), expected);
        assert_eq!(ObjectRef::try_from(&record).ok(), Some(reference));
    }

    #[test]
    fn hash_and_direct_write_consume_the_same_record_bytes() {
        let record = ObjectDescriptorWireRecord::from(&object());
        let mut output = [0xa5; OBJECT_DESCRIPTOR_RECORD_BYTES + 1];
        assert_eq!(record.write_into(&mut output), Ok(()));
        assert_eq!(output.last(), Some(&0xa5));
        let mut hasher = ContentHasher::<DependencySetDomain>::new();
        hasher.write_record(&record);
        let from_sink = hasher.finalize();
        let from_bytes = ContentId::<DependencySetDomain>::from_canonical_bytes(
            &output[..OBJECT_DESCRIPTOR_RECORD_BYTES],
        );
        assert_eq!(from_sink, from_bytes);
    }

    #[test]
    fn every_short_prefix_and_unknown_schema_reject_exactly() -> Result<(), RecordTestError> {
        let record = ObjectDescriptorWireRecord::from(&object());
        for length in 0..OBJECT_DESCRIPTOR_RECORD_BYTES {
            let bytes = record
                .as_bytes()
                .get(..length)
                .ok_or(RecordTestError::MissingPrefix { length })?;
            match ObjectRef::<ObjectDomain>::try_from(bytes) {
                Err(ObjectDescriptorDecodeError::Width { actual }) => {
                    assert_eq!(actual, length);
                }
                Err(error) => return Err(RecordTestError::UnexpectedDecode(error)),
                Ok(_) => return Err(RecordTestError::ExpectedUnknownSchema),
            }
        }
        let mut short = [0_u8; OBJECT_DESCRIPTOR_RECORD_BYTES - 1];
        assert_eq!(
            record.write_into(&mut short),
            Err(ObjectDescriptorOutputTooSmall {
                available: OBJECT_DESCRIPTOR_RECORD_BYTES - 1,
            })
        );
        let mut bytes = *record.canonical_bytes();
        bytes[40..44].copy_from_slice(&99_u32.to_be_bytes());
        match ObjectRef::<ObjectDomain>::try_from(bytes.as_slice()) {
            Err(ObjectDescriptorDecodeError::Schema(nudox_schema::UnknownSchemaId(99))) => Ok(()),
            Err(error) => Err(RecordTestError::UnexpectedDecode(error)),
            Ok(_) => Err(RecordTestError::ExpectedUnknownSchema),
        }
    }
}
