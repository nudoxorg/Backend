//! Isolated allocation contract for non-empty object-pack directory validation.

#[path = "support/object_pack_index.rs"]
mod support;

use allocation_counter::{AllocationInfo, measure};
use nudox_object_pack::{ObjectPackError, ObjectPackIndex};
use thiserror::Error;
use zerocopy::{
    IntoBytes,
    byteorder::{BigEndian, U64},
};

use support::{TestIndex, three_rows};

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Pack(#[from] ObjectPackError),
    #[error("allocation measurement did not execute its closure")]
    MeasurementDidNotRun,
}

#[test]
fn nonempty_index_opens_without_allocation() -> Result<(), TestError> {
    let bytes = TestIndex {
        count: U64::<BigEndian>::new(3),
        rows: three_rows(),
    };
    let warm = ObjectPackIndex::try_from(bytes.as_bytes())?;
    assert_eq!(warm.bytes.as_ptr(), bytes.as_bytes().as_ptr());
    let mut result = None;
    let allocations = measure(|| {
        result = Some(ObjectPackIndex::try_from(bytes.as_bytes()));
    });
    let index = match result {
        Some(Ok(index)) => index,
        Some(Err(error)) => return Err(error.into()),
        None => return Err(TestError::MeasurementDidNotRun),
    };
    assert_eq!(index.bytes.as_ptr(), bytes.as_bytes().as_ptr());
    assert_eq!(
        allocations,
        AllocationInfo {
            count_total: 0,
            count_current: 0,
            count_max: 0,
            bytes_total: 0,
            bytes_current: 0,
            bytes_max: 0,
        }
    );
    Ok(())
}
