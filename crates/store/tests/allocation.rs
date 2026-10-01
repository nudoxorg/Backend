//! Exercises the `backend_store::object_pack` tests allocation contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Isolated allocation contract for canonical object-pack preparation and writing.

use allocation_counter::{AllocationInfo, measure};
use backend_version::ContentId;
use backend_version::object::{ObjectLength, ObjectRef};
use backend_store::object_pack::{ObjectPackError, PackInput, PreparedObjectPack};
use backend_version::schema::SchemaId;
use thiserror::Error;

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Pack(#[from] ObjectPackError),
    #[error(transparent)]
    Length(#[from] core::num::TryFromIntError),
    #[error("allocation measurement did not execute its closure")]
    MeasurementDidNotRun,
}

fn input(bytes: &[u8]) -> Result<PackInput<'_>, TestError> {
    Ok(PackInput {
        reference: ObjectRef {
            content: ContentId::from_canonical_bytes(bytes),
            length: ObjectLength::from(u64::try_from(bytes.len())?),
            schema: SchemaId::Object,
            kind: 7_u16.into(),
        },
        bytes,
    })
}

#[test]
fn prepare_and_write_allocate_nothing_after_warmup() -> Result<(), TestError> {
    let first = input(b"a")?;
    let second = input(b"bc")?;
    let inputs = if first.reference.content < second.reference.content {
        [first, second]
    } else {
        [second, first]
    };
    let prepared = PreparedObjectPack::prepare(&inputs)?;
    let mut output = vec![0_u8; usize::from(prepared.required_bytes)];
    let pointer = output.as_ptr();
    let warm = PreparedObjectPack::prepare(&inputs)?;
    let warm_written = warm.write(&mut output)?;
    assert_eq!(warm_written.as_ptr(), pointer);
    let mut result = None;
    let allocations = measure(|| {
        result = Some(
            PreparedObjectPack::prepare(&inputs)
                .and_then(|measured| measured.write(&mut output).map(<[u8]>::as_ptr)),
        );
    });
    let written = match result {
        Some(Ok(written)) => written,
        Some(Err(error)) => return Err(TestError::Pack(error)),
        None => return Err(TestError::MeasurementDidNotRun),
    };
    assert_eq!(written, pointer);
    assert_eq!(
        allocations,
        AllocationInfo {
            count_total: 0,
            count_current: 0,
            count_max: 0,
            bytes_total: 0,
            bytes_current: 0,
            bytes_max: 0,
        },
        "allocation counter on {}-{} in cargo test debug profile",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    Ok(())
}
