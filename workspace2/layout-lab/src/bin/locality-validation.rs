use std::{hint::black_box, mem::size_of, time::Instant};

use nudox_id::{ContentId, ObjectDomain};
use nudox_object::{ObjectRef, RemoteBase};
use nudox_root::{
    EntryKey, GenerationRoot, LocalityError, LocalityException, LocalityValidator,
    LocalityWriteError, NonResident, PreparedLocality, RootBuildError, RootEntry,
    ValidatedLocality,
};
use nudox_schema::SchemaId;
use thiserror::Error;

const COUNTS: &[u32] = &[0, 1, 4, 8, 16, 32, 64, 128, 256, 1_024, 4_096, 16_384];
const TARGET_ROWS: u32 = 8_000_000;
const WARMUP_ROUNDS: u32 = 128;

#[derive(Debug, Error)]
enum BenchmarkError {
    #[error("benchmark root construction failed")]
    Root(#[from] RootBuildError),
    #[error("benchmark locality validation or preparation failed")]
    Locality(#[from] LocalityError),
    #[error("benchmark locality output failed")]
    Write(#[from] LocalityWriteError),
    #[error("benchmark root did not issue a locality coordinate for {key:?}")]
    MissingCoordinate { key: EntryKey },
}

fn main() -> Result<(), BenchmarkError> {
    let validator = LocalityValidator::new();
    println!(
        "{{\"type\":\"layout\",\"validated_locality_bytes\":{},\"validator_bytes\":{}}}",
        size_of::<ValidatedLocality<'static, ObjectDomain>>(),
        size_of::<LocalityValidator>(),
    );
    for &count in COUNTS {
        let bytes = artifact(count)?;
        for _warmup_round in 0..WARMUP_ROUNDS {
            black_box(ValidatedLocality::<ObjectDomain>::try_from(
                bytes.as_slice(),
            )?);
            black_box(validator.validate::<ObjectDomain>(bytes.as_slice())?);
        }
        let iterations = (TARGET_ROWS / count.max(1)).max(32);
        let scalar = elapsed(iterations, || {
            let view = ValidatedLocality::<ObjectDomain>::try_from(black_box(bytes.as_slice()))?;
            black_box(view.metadata_bytes());
            Ok::<(), LocalityError>(())
        })?;
        let simd = elapsed(iterations, || {
            let view = validator.validate::<ObjectDomain>(black_box(bytes.as_slice()))?;
            black_box(view.metadata_bytes());
            Ok::<(), LocalityError>(())
        })?;
        println!(
            "{{\"count\":{count},\"bytes\":{},\"iterations\":{iterations},\"scalar_ns\":{},\"simd_ns\":{}}}",
            bytes.len(),
            scalar.as_nanos(),
            simd.as_nanos(),
        );
    }
    Ok(())
}

fn elapsed<Error>(
    iterations: u32,
    mut parse: impl FnMut() -> Result<(), Error>,
) -> Result<std::time::Duration, Error> {
    let start = Instant::now();
    for _iteration in 0..iterations {
        black_box(parse()?);
    }
    Ok(start.elapsed())
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "the benchmark deliberately cycles content fixture bytes while row coordinates remain full u32 values"
)]
fn artifact(count: u32) -> Result<Vec<u8>, BenchmarkError> {
    let mut entries: Vec<RootEntry<ObjectDomain>> = Vec::new();
    for row in 0..count {
        entries.push(RootEntry {
            key: EntryKey::from(u64::from(row)),
            parent: None,
            object: ObjectRef {
                content: ContentId::from_digest([row as u8; 32]),
                length: 1_u64.into(),
                schema: SchemaId::Object,
                kind: 1_u16.into(),
            },
        });
    }
    let root = GenerationRoot::new(entries)?;
    let mut facts = Vec::new();
    for row in 0..count {
        let key = EntryKey::from(u64::from(row));
        let coordinate = root
            .locality_row(key)
            .ok_or(BenchmarkError::MissingCoordinate { key })?;
        facts.push(LocalityException::new(
            coordinate,
            NonResident::Overlaid(RemoteBase::Absent {
                generation: root.id,
            }),
        ));
    }
    let prepared = PreparedLocality::prepare(&root, &facts)?;
    let mut bytes = vec![0_u8; usize::from(prepared.required_bytes)];
    black_box(prepared.write(&mut bytes)?);
    Ok(bytes)
}
