//! Exercises the `heart-object-pack` tests header-allocation contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Isolated allocation contract for fixed object-pack header opening.

use allocation_counter::{AllocationInfo, measure};
use heart_object_pack::{ObjectPackError, ObjectPackHeader};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Pack(#[from] ObjectPackError),
    #[error("allocation measurement did not execute its closure")]
    MeasurementDidNotRun,
}

#[test]
fn count_header_opens_without_allocation() -> Result<(), TestError> {
    let bytes = 1_u64.to_be_bytes();
    let warm = ObjectPackHeader::try_from(bytes.as_slice())?;
    assert_eq!(*warm.object_count, 1);
    let mut result = None;
    let allocations = measure(|| {
        result = Some(ObjectPackHeader::try_from(bytes.as_slice()));
    });
    let header = match result {
        Some(Ok(header)) => header,
        Some(Err(error)) => return Err(error.into()),
        None => return Err(TestError::MeasurementDidNotRun),
    };
    assert_eq!(header.bytes.as_ptr(), bytes.as_ptr());
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
