use std::{hint::black_box, mem::size_of, time::{Duration, Instant}};

const ROWS: usize = 100_000;
const RUNS: usize = 11;
const NO_PARENT: u32 = u32::MAX;

#[derive(Clone)]
#[repr(C)]
struct InputEntry {
    parent: Option<u64>,
    object: [u8; 48],
    key: u64,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct PublishedRow {
    object: [u8; 48],
    key: u64,
    parent: u32,
    depth: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct PhaseRow {
    object: [u8; 48],
    key: u64,
    parent_key_then_coordinates: u64,
}

fn shuffled_keys() -> Vec<u64> {
    let mut keys: Vec<_> = (0..ROWS as u64).collect();
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    for end in (1..keys.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let other = state as usize % (end + 1);
        keys.swap(end, other);
    }
    keys
}

fn is_root(key: u64, root_every: usize) -> bool {
    key as usize % root_every == 0
}

fn current(keys: &[u64], root_every: usize) -> (Vec<PublishedRow>, usize) {
    let mut entries = Vec::with_capacity(keys.len());
    for &key in keys {
        let parent = (!is_root(key, root_every)).then_some(key - 1);
        entries.push(InputEntry { parent, object: [key as u8; 48], key });
    }
    entries.sort_unstable_by_key(|entry| entry.key);
    let mut rows = Vec::with_capacity(entries.len());
    for entry in &entries {
        let parent = entry.parent.map_or(NO_PARENT, |parent| {
            entries.binary_search_by_key(&parent, |candidate| candidate.key).unwrap() as u32
        });
        rows.push(PublishedRow { object: entry.object, key: entry.key, parent, depth: 0 });
    }
    let peak = entries.capacity() * size_of::<InputEntry>()
        + rows.capacity() * size_of::<PublishedRow>();
    black_box(&rows);
    (rows, peak)
}

fn phase_reused(keys: &[u64], root_every: usize) -> (Vec<PhaseRow>, usize) {
    let root_count = keys.iter().filter(|&&key| is_root(key, root_every)).count();
    let mut rows = Vec::with_capacity(keys.len());
    let mut roots = Vec::with_capacity(root_count);
    for &key in keys {
        let root = is_root(key, root_every);
        if root { roots.push(key); }
        rows.push(PhaseRow {
            object: [key as u8; 48],
            key,
            parent_key_then_coordinates: if root { 0 } else { key - 1 },
        });
    }
    rows.sort_unstable_by_key(|row| row.key);
    roots.sort_unstable();
    let peak = rows.capacity() * size_of::<PhaseRow>() + roots.capacity() * size_of::<u64>();
    let mut next_root = 0;
    for position in 0..rows.len() {
        let key = rows[position].key;
        let root = roots.get(next_root).is_some_and(|root| *root == key);
        let parent = if root {
            next_root += 1;
            NO_PARENT
        } else {
            let parent_key = rows[position].parent_key_then_coordinates;
            rows.binary_search_by_key(&parent_key, |candidate| candidate.key).unwrap() as u32
        };
        rows[position].parent_key_then_coordinates = u64::from(parent);
    }
    black_box(&rows);
    (rows, peak)
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn measure<Result>(mut operation: impl FnMut() -> Result) -> (Duration, Result) {
    let result = operation();
    black_box(&result);
    let mut last = None;
    let elapsed = median((0..RUNS).map(|_| {
        let start = Instant::now();
        last = Some(operation());
        start.elapsed()
    }).collect());
    (elapsed, last.unwrap())
}

fn main() {
    assert_eq!(size_of::<InputEntry>(), 72);
    assert_eq!(size_of::<PublishedRow>(), 64);
    assert_eq!(size_of::<PhaseRow>(), 64);
    let keys = shuffled_keys();
    println!("format\troot-builder-phase-reuse-v1");
    println!("columns\troot_percent\trepresentation\trows\tpeak_payload_bytes\tmedian_ns\tchecksum");
    for root_every in [100, 10, 2, 1] {
        let root_percent = 100 / root_every;
        let (current_time, (current_rows, current_peak)) = measure(|| current(&keys, root_every));
        let (phase_time, (phase_rows, phase_peak)) = measure(|| phase_reused(&keys, root_every));
        let current_checksum: u64 = current_rows.iter().map(|row| u64::from(row.parent)).sum();
        let phase_checksum: u64 = phase_rows
            .iter()
            .map(|row| u64::from(row.parent_key_then_coordinates as u32))
            .sum();
        assert_eq!(current_checksum, phase_checksum);
        println!("result\t{root_percent}\tcurrent\t{}\t{current_peak}\t{}\t{current_checksum}", keys.len(), current_time.as_nanos());
        println!("result\t{root_percent}\tphase_reused\t{}\t{phase_peak}\t{}\t{phase_checksum}", keys.len(), phase_time.as_nanos());
    }
}
