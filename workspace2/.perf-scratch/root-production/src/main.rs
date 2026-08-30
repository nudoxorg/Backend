use std::{hint::black_box, time::{Duration, Instant}};

use nudox_id::{ContentId, ObjectDomain};
use nudox_object::ObjectRef;
use nudox_root::{EntryKey, GenerationRoot, GenerationRootBuilder, RootEntry};
use nudox_schema::SchemaId;

const ROWS: usize = 100_000;
const RUNS: usize = 11;

fn entry(key: u64) -> RootEntry<ObjectDomain> {
    RootEntry {
        key: EntryKey::from(key),
        parent: (key != 0).then(|| EntryKey::from((key - 1) / 2)),
        object: ObjectRef {
            content: ContentId::from({
                let mut bytes = [0_u8; 32];
                bytes[..8].copy_from_slice(&key.to_le_bytes());
                bytes
            }),
            length: (key & 4095).into(),
            schema: SchemaId::Object,
            kind: (key as u16).into(),
        },
    }
}

fn fixture() -> Vec<RootEntry<ObjectDomain>> {
    let mut rows: Vec<_> = (0..ROWS as u64).map(entry).collect();
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    for end in (1..rows.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        rows.swap(end, state as usize % (end + 1));
    }
    rows
}

fn ordinary(source: &[RootEntry<ObjectDomain>]) -> GenerationRoot<ObjectDomain> {
    GenerationRoot::new(source.to_vec()).unwrap()
}

fn streaming(source: &[RootEntry<ObjectDomain>]) -> GenerationRoot<ObjectDomain> {
    let mut builder = GenerationRootBuilder::with_capacity(source.len()).unwrap();
    for &row in source {
        assert!(builder.try_push(row).is_ok());
    }
    builder.finish().unwrap()
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

fn measure(mut operation: impl FnMut() -> GenerationRoot<ObjectDomain>) -> (Duration, GenerationRoot<ObjectDomain>) {
    black_box(operation());
    let mut last = None;
    let elapsed = median((0..RUNS).map(|_| {
        let started = Instant::now();
        last = Some(black_box(operation()));
        started.elapsed()
    }).collect());
    (elapsed, last.unwrap())
}

fn main() {
    let source = fixture();
    let (ordinary_time, ordinary_root) = measure(|| ordinary(&source));
    let (streaming_time, streaming_root) = measure(|| streaming(&source));
    assert_eq!(ordinary_root.id, streaming_root.id);
    println!("format\troot-production-v1");
    println!("rows\trepresentation\tconstruction_peak_bytes\tretained_bytes\tmedian_ns\tid");
    println!("{ROWS}\towned-vec\t{}\t{}\t{}\t{:?}", *ordinary_root.construction_peak_bytes, *ordinary_root.metadata_bytes(), ordinary_time.as_nanos(), ordinary_root.id);
    println!("{ROWS}\tstreaming-phase-niche\t{}\t{}\t{}\t{:?}", *streaming_root.construction_peak_bytes, *streaming_root.metadata_bytes(), streaming_time.as_nanos(), streaming_root.id);
}
