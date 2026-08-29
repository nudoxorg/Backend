//! Public borrowed object-pack directory validation contracts.

#[path = "support/object_pack_index.rs"]
mod support;

use core::mem::{offset_of, size_of};

use nudox_object::{OBJECT_DESCRIPTOR_RECORD_BYTES, ObjectDescriptorWireRecord};
use nudox_object_pack::{
    OBJECT_PACK_HEADER_BYTES, ObjectPackBytes, ObjectPackError, ObjectPackIndex,
    ObjectPackObjectCount,
};
use nudox_schema::UnknownSchemaId;
use thiserror::Error;
use zerocopy::{IntoBytes, byteorder::U64};

use support::{TestDirectoryRow, TestIndex, row, three_rows};

const UNKNOWN_SCHEMA: u32 = 99;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Pack(#[from] ObjectPackError),
    #[error("invalid directory fixture unexpectedly opened")]
    UnexpectedIndex,
}

fn three() -> TestIndex<3> {
    TestIndex {
        count: U64::new(3),
        rows: three_rows(),
    }
}

fn rejection(bytes: &[u8]) -> Result<ObjectPackError, TestError> {
    match ObjectPackIndex::try_from(bytes) {
        Err(error) => Ok(error),
        Ok(_) => Err(TestError::UnexpectedIndex),
    }
}

#[test]
fn empty_and_zero_length_indexes_borrow_and_derive_exact_pack_extents() -> Result<(), TestError> {
    let empty = [0_u8; OBJECT_PACK_HEADER_BYTES];
    let empty_index = ObjectPackIndex::try_from(empty.as_slice())?;
    assert_eq!(*empty_index.object_count, 0);
    assert_eq!(empty_index.pack_bytes, empty_index.index_bytes);
    assert_eq!(empty_index.bytes.as_ptr(), empty.as_ptr());
    let zero = TestIndex {
        count: U64::new(1),
        rows: [row([0; 32], 0, 0)],
    };
    let zero_index = ObjectPackIndex::try_from(zero.as_bytes())?;
    assert_eq!(zero_index.object_count, ObjectPackObjectCount::from(1));
    assert_eq!(zero_index.pack_bytes, zero_index.index_bytes);
    assert_eq!(zero_index.bytes.as_ptr(), zero.as_bytes().as_ptr());
    Ok(())
}

#[test]
fn index_rejects_every_prefix_and_trailing_byte_with_exact_extents() -> Result<(), TestError> {
    let index = three();
    let exact = index.as_bytes();
    let expected = ObjectPackBytes::from(exact.len());
    for available in 0..exact.len() {
        let prefix = exact.get(..available).ok_or(TestError::UnexpectedIndex)?;
        let error = rejection(prefix)?;
        if available < OBJECT_PACK_HEADER_BYTES {
            assert_eq!(
                error,
                ObjectPackError::HeaderTruncated {
                    required: ObjectPackBytes::from(OBJECT_PACK_HEADER_BYTES),
                    available: ObjectPackBytes::from(available),
                }
            );
        } else {
            assert_eq!(
                error,
                ObjectPackError::DirectoryExtent {
                    expected,
                    actual: ObjectPackBytes::from(available),
                }
            );
        }
    }
    let mut trailing = exact.to_vec();
    trailing.push(0);
    assert_eq!(
        rejection(trailing.as_slice())?,
        ObjectPackError::DirectoryExtent {
            expected,
            actual: ObjectPackBytes::from(exact.len() + 1),
        }
    );
    Ok(())
}

fn cumulative_mutations() -> Result<(), TestError> {
    for (ordinal, expected, observed) in [(0, 0, 1), (1, 1, 3), (2, 3, 6)] {
        let mut length = three();
        let row = length
            .rows
            .get_mut(ordinal)
            .ok_or(TestError::UnexpectedIndex)?;
        row.descriptor.length = U64::new(0);
        assert_eq!(
            rejection(length.as_bytes())?,
            ObjectPackError::DirectoryCumulativeEnd {
                ordinal,
                expected: ObjectPackBytes::from(expected),
                observed: ObjectPackBytes::from(observed),
            }
        );
        let mut end = three();
        let row = end
            .rows
            .get_mut(ordinal)
            .ok_or(TestError::UnexpectedIndex)?;
        row.body_end = U64::new(0);
        assert_eq!(
            rejection(end.as_bytes())?,
            ObjectPackError::DirectoryCumulativeEnd {
                ordinal,
                expected: ObjectPackBytes::from(observed),
                observed: ObjectPackBytes::from(0),
            }
        );
    }
    Ok(())
}

#[test]
fn directory_mutations_preserve_exact_schema_order_and_cumulative_diagnostics()
-> Result<(), TestError> {
    let schema = three();
    let mut schema_bytes = schema.as_bytes().to_vec();
    let schema_offset = offset_of!(TestIndex<3>, rows)
        + offset_of!(TestDirectoryRow, descriptor)
        + offset_of!(ObjectDescriptorWireRecord, schema);
    let target = schema_bytes
        .get_mut(schema_offset..schema_offset + size_of::<u32>())
        .ok_or(TestError::UnexpectedIndex)?;
    target.copy_from_slice(&UNKNOWN_SCHEMA.to_be_bytes());
    assert_eq!(
        rejection(schema_bytes.as_slice())?,
        ObjectPackError::DirectorySchema {
            ordinal: 0,
            source: UnknownSchemaId(UNKNOWN_SCHEMA),
        }
    );
    let mut duplicate = three();
    let (previous, current) = {
        let [first, second, _] = &mut duplicate.rows;
        second.descriptor.content = first.descriptor.content;
        (first.descriptor.content, second.descriptor.content)
    };
    assert_eq!(
        rejection(duplicate.as_bytes())?,
        ObjectPackError::DirectoryOrder {
            ordinal: 1,
            previous,
            current,
        }
    );
    let mut reordered = three();
    let (previous, current) = {
        let [first, second, _] = &mut reordered.rows;
        first.descriptor.content = [u8::MAX; 32];
        (first.descriptor.content, second.descriptor.content)
    };
    assert_eq!(
        rejection(reordered.as_bytes())?,
        ObjectPackError::DirectoryOrder {
            ordinal: 1,
            previous,
            current,
        }
    );
    cumulative_mutations()
}

#[cfg(target_pointer_width = "32")]
#[test]
fn directory_native_coordinate_sources_are_preserved() -> Result<(), TestError> {
    let source = match usize::try_from(u64::MAX) {
        Err(source) => source,
        Ok(_) => return Err(TestError::UnexpectedIndex),
    };
    let mut length = three();
    let [first, _, _] = &mut length.rows;
    first.descriptor.length = U64::new(u64::MAX);
    assert_eq!(
        rejection(length.as_bytes())?,
        ObjectPackError::DirectoryLengthAddressSpace {
            ordinal: 0,
            declared: u64::MAX.into(),
            source,
        }
    );
    let mut end = three();
    let [first, _, _] = &mut end.rows;
    first.body_end = U64::new(u64::MAX);
    assert_eq!(
        rejection(end.as_bytes())?,
        ObjectPackError::DirectoryEndAddressSpace {
            ordinal: 0,
            observed: u64::MAX,
            source,
        }
    );
    Ok(())
}

#[cfg(target_pointer_width = "64")]
#[test]
fn directory_and_full_pack_overflows_name_exact_operands() -> Result<(), TestError> {
    let mut cumulative = three();
    let [first, second, _] = &mut cumulative.rows;
    first.descriptor.length = U64::new(u64::MAX);
    first.body_end = U64::new(u64::MAX);
    second.descriptor.length = U64::new(1);
    assert_eq!(
        rejection(cumulative.as_bytes())?,
        ObjectPackError::DirectoryCumulativeOverflow {
            ordinal: 1,
            left: ObjectPackBytes::from(usize::MAX),
            right: ObjectPackBytes::from(1),
        }
    );
    let full = TestIndex {
        count: U64::new(1),
        rows: [row([1; 32], u64::MAX, u64::MAX)],
    };
    assert_eq!(
        rejection(full.as_bytes())?,
        ObjectPackError::LayoutOverflow {
            left: ObjectPackBytes::from(
                OBJECT_PACK_HEADER_BYTES + OBJECT_DESCRIPTOR_RECORD_BYTES + size_of::<u64>(),
            ),
            right: ObjectPackBytes::from(usize::MAX),
        }
    );
    Ok(())
}
