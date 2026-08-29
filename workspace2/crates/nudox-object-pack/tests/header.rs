//! Public fixed object-pack header contracts.

use core::mem::size_of;

use nudox_object::OBJECT_DESCRIPTOR_RECORD_BYTES;
use nudox_object_pack::{
    OBJECT_PACK_HEADER_BYTES, ObjectPackBytes, ObjectPackError, ObjectPackHeader,
    ObjectPackObjectCount,
};
use thiserror::Error;

const COUNT_BYTES: usize = OBJECT_PACK_HEADER_BYTES;
const DIRECTORY_BYTES: usize = OBJECT_DESCRIPTOR_RECORD_BYTES + size_of::<u64>();

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Pack(#[from] ObjectPackError),
    #[cfg(target_pointer_width = "32")]
    #[error("a complete fixed header unexpectedly failed")]
    CompleteHeader,
    #[error("a malformed fixed header unexpectedly succeeded")]
    MalformedHeader,
}

const fn count(value: u64) -> [u8; COUNT_BYTES] {
    value.to_be_bytes()
}

#[test]
fn count_header_measures_zero_one_and_borrows_the_exact_input() -> Result<(), TestError> {
    let zero = count(0);
    let zero_header = ObjectPackHeader::try_from(zero.as_slice())?;
    assert_eq!(*zero_header.object_count, 0);
    assert_eq!(usize::from(zero_header.index_bytes), COUNT_BYTES);
    assert_eq!(zero_header.bytes.as_ptr(), zero.as_ptr());
    let one = count(1);
    let one_header = ObjectPackHeader::try_from(one.as_slice())?;
    assert_eq!(*one_header.object_count, 1);
    assert_eq!(
        usize::from(one_header.index_bytes),
        COUNT_BYTES + DIRECTORY_BYTES
    );
    assert_eq!(one_header.bytes.as_ptr(), one.as_ptr());
    Ok(())
}

#[test]
fn count_header_rejects_every_short_prefix_and_one_trailing_byte() -> Result<(), TestError> {
    let header = count(1);
    for available in 0..COUNT_BYTES {
        let Some(prefix) = header.get(..available) else {
            return Err(TestError::MalformedHeader);
        };
        match ObjectPackHeader::try_from(prefix) {
            Err(ObjectPackError::HeaderTruncated {
                required,
                available: observed,
            }) => {
                assert_eq!(required, ObjectPackBytes::from(COUNT_BYTES));
                assert_eq!(observed, ObjectPackBytes::from(available));
            }
            Err(error) => return Err(error.into()),
            Ok(_) => return Err(TestError::MalformedHeader),
        }
    }
    let mut trailing = [0_u8; COUNT_BYTES + 1];
    trailing[..COUNT_BYTES].copy_from_slice(&header);
    match ObjectPackHeader::try_from(trailing.as_slice()) {
        Err(ObjectPackError::HeaderTrailing { expected, actual }) => {
            assert_eq!(expected, ObjectPackBytes::from(COUNT_BYTES));
            assert_eq!(actual, ObjectPackBytes::from(COUNT_BYTES + 1));
        }
        Err(error) => return Err(error.into()),
        Ok(_) => return Err(TestError::MalformedHeader),
    }
    Ok(())
}

#[cfg(target_pointer_width = "32")]
#[test]
fn count_header_preserves_the_native_count_conversion_source() -> Result<(), TestError> {
    let bytes = count(u64::MAX);
    let expected_source = match usize::try_from(u64::MAX) {
        Err(source) => source,
        Ok(_) => return Err(TestError::CompleteHeader),
    };
    match ObjectPackHeader::try_from(bytes.as_slice()) {
        Err(ObjectPackError::CountAddressSpace { count, source }) => {
            assert_eq!(count, ObjectPackObjectCount::from(u64::MAX));
            assert_eq!(source, expected_source);
        }
        Err(error) => return Err(error.into()),
        Ok(_) => return Err(TestError::MalformedHeader),
    }
    Ok(())
}

#[cfg(target_pointer_width = "64")]
#[test]
fn count_header_rejects_an_unrepresentable_directory() -> Result<(), TestError> {
    let bytes = count(u64::MAX);
    match ObjectPackHeader::try_from(bytes.as_slice()) {
        Err(ObjectPackError::IndexLayoutOverflow { count, row_bytes }) => {
            assert_eq!(count, ObjectPackObjectCount::from(u64::MAX));
            assert_eq!(row_bytes, ObjectPackBytes::from(DIRECTORY_BYTES));
        }
        Err(error) => return Err(error.into()),
        Ok(_) => return Err(TestError::MalformedHeader),
    }
    Ok(())
}
