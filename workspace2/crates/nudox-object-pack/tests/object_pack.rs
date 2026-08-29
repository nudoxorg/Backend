//! Public canonical object-pack writer contracts.

use core::mem::size_of;
use std::vec::Vec;

use nudox_id::{ContentId, ObjectDomain};
use nudox_object::{OBJECT_DESCRIPTOR_RECORD_BYTES, ObjectLength, ObjectRef};
use nudox_object_pack::{ObjectPackError, PackInput, PreparedObjectPack};
use nudox_schema::SchemaId;
use thiserror::Error;

const SENTINEL: u8 = 0xa5;
const DIRECTORY_BYTES: usize = OBJECT_DESCRIPTOR_RECORD_BYTES + size_of::<u64>();

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Pack(#[from] ObjectPackError),
    #[error(transparent)]
    Length(#[from] core::num::TryFromIntError),
    #[error("fixture cumulative object length overflowed")]
    LengthOverflow,
    #[error("invalid fixture unexpectedly prepared")]
    UnexpectedPrepared,
}

fn reference(bytes: &[u8]) -> Result<ObjectRef<ObjectDomain>, TestError> {
    Ok(ObjectRef {
        content: ContentId::from_canonical_bytes(bytes),
        length: ObjectLength::from(u64::try_from(bytes.len())?),
        schema: SchemaId::Object,
        kind: 7_u16.into(),
    })
}

fn input(bytes: &[u8]) -> Result<PackInput<'_>, TestError> {
    Ok(PackInput {
        reference: reference(bytes)?,
        bytes,
    })
}

fn write(inputs: &[PackInput<'_>]) -> Result<Vec<u8>, TestError> {
    let prepared = PreparedObjectPack::prepare(inputs)?;
    let mut output = vec![0_u8; usize::from(prepared.required_bytes)];
    let written = prepared.write(&mut output)?;
    assert_eq!(written.len(), output.len());
    Ok(output)
}

fn rejection(inputs: &[PackInput<'_>]) -> Result<ObjectPackError, TestError> {
    match PreparedObjectPack::prepare(inputs) {
        Err(error) => Ok(error),
        Ok(_) => Err(TestError::UnexpectedPrepared),
    }
}

fn append_descriptor(expected: &mut Vec<u8>, reference: ObjectRef<ObjectDomain>, end: u64) {
    expected.extend_from_slice(reference.content.as_ref());
    expected.extend_from_slice(&(*reference.length).to_be_bytes());
    expected.extend_from_slice(&u32::from(reference.schema).to_be_bytes());
    expected.extend_from_slice(&(*reference.kind).to_be_bytes());
    expected.extend_from_slice(&end.to_be_bytes());
}

#[test]
fn empty_pack_has_the_exact_count_only_golden() -> Result<(), TestError> {
    let bytes = write(&[])?;
    assert_eq!(bytes, Vec::from([0_u8; size_of::<u64>()]));
    Ok(())
}

#[test]
fn two_object_pack_has_exact_golden_directory_and_body_order() -> Result<(), TestError> {
    let first = input(b"a")?;
    let second = input(b"bc")?;
    let (first, second) = if first.reference.content < second.reference.content {
        (first, second)
    } else {
        (second, first)
    };
    let inputs = [first, second];
    let bytes = write(&inputs)?;
    let mut expected = Vec::from(u64::try_from(inputs.len())?.to_be_bytes());
    let first_end = *first.reference.length;
    let second_end = first_end
        .checked_add(*second.reference.length)
        .ok_or(TestError::LengthOverflow)?;
    append_descriptor(&mut expected, first.reference, first_end);
    append_descriptor(&mut expected, second.reference, second_end);
    expected.extend_from_slice(first.bytes);
    expected.extend_from_slice(second.bytes);
    assert_eq!(bytes, expected);
    assert_eq!(
        DIRECTORY_BYTES,
        OBJECT_DESCRIPTOR_RECORD_BYTES + size_of::<u64>()
    );
    Ok(())
}

#[test]
fn sparse_input_needs_no_root_and_writes_one_object() -> Result<(), TestError> {
    let only = input(b"present only")?;
    let bytes = write(&[only])?;
    let mut expected = Vec::from(1_u64.to_be_bytes());
    append_descriptor(&mut expected, only.reference, *only.reference.length);
    expected.extend_from_slice(only.bytes);
    assert_eq!(bytes, expected);
    Ok(())
}

#[test]
fn preparation_preserves_exact_length_content_order_and_duplicate_rejections()
-> Result<(), TestError> {
    let good = input(b"a")?;
    let declared = ObjectLength::from(u64::try_from(good.bytes.len() + 1)?);
    let length = PackInput {
        reference: ObjectRef {
            length: declared,
            ..good.reference
        },
        bytes: good.bytes,
    };
    assert_eq!(
        rejection(&[length])?,
        ObjectPackError::InputLength {
            ordinal: 0,
            declared,
            actual: good.bytes.len().into(),
        }
    );
    let content = PackInput {
        reference: ObjectRef {
            content: ContentId::from_canonical_bytes(b"different"),
            ..good.reference
        },
        bytes: good.bytes,
    };
    let expected = content.reference.content;
    let actual = ContentId::from_canonical_bytes(content.bytes);
    assert_eq!(
        rejection(&[content])?,
        ObjectPackError::InputContent {
            ordinal: 0,
            expected,
            actual,
        }
    );
    let other = input(b"b")?;
    let (lower, higher) = if good.reference.content < other.reference.content {
        (good, other)
    } else {
        (other, good)
    };
    assert_eq!(
        rejection(&[higher, lower])?,
        ObjectPackError::InputOrder {
            ordinal: 1,
            previous: higher.reference.content,
            current: lower.reference.content,
        }
    );
    assert_eq!(
        rejection(&[lower, lower])?,
        ObjectPackError::InputOrder {
            ordinal: 1,
            previous: lower.reference.content,
            current: lower.reference.content,
        }
    );
    Ok(())
}

#[test]
fn preflight_and_output_borrow_preserve_exact_sentinel_and_pointer_laws() -> Result<(), TestError> {
    let first = input(b"a")?;
    let second = input(b"bc")?;
    let inputs = if first.reference.content < second.reference.content {
        [first, second]
    } else {
        [second, first]
    };
    let prepared = PreparedObjectPack::prepare(&inputs)?;
    let required = prepared.required_bytes;
    let mut short = vec![SENTINEL; usize::from(required) - 1];
    assert_eq!(
        prepared.write(&mut short),
        Err(ObjectPackError::OutputTooSmall {
            required,
            available: (usize::from(required) - 1).into(),
        })
    );
    assert!(short.iter().all(|byte| *byte == SENTINEL));
    let mut exact = vec![SENTINEL; usize::from(required)];
    let body_start = size_of::<u64>() + DIRECTORY_BYTES * inputs.len();
    let exact_pointer = exact.as_ptr();
    let body_pointer = exact_pointer.wrapping_add(body_start);
    let written = prepared.write(&mut exact)?;
    assert_eq!(written.as_ptr(), exact_pointer);
    assert_eq!(written.len(), usize::from(required));
    assert_eq!(
        written.get(body_start..).map(<[u8]>::as_ptr),
        Some(body_pointer)
    );
    let mut oversized = vec![SENTINEL; usize::from(required) + 1];
    let oversized_pointer = oversized.as_ptr();
    let oversized_written = prepared.write(&mut oversized)?;
    assert_eq!(oversized_written.as_ptr(), oversized_pointer);
    assert_eq!(oversized.last(), Some(&SENTINEL));
    Ok(())
}
