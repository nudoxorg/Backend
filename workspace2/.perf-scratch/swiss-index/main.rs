use std::{hint::black_box, mem::size_of, time::{Duration, Instant}};

const ENTRIES: usize = 100_000;
const QUERIES: usize = 2_000_000;
const RUNS: usize = 9;
const EMPTY: u8 = 0x80;

#[derive(Clone, Copy)]
struct BaselineBucket(u32);

#[repr(C, align(16))]
struct Group {
    control: [u8; 16],
    entries: [u32; 16],
}

impl Group {
    const fn empty() -> Self {
        Self { control: [EMPTY; 16], entries: [0; 16] }
    }
}

struct Baseline {
    buckets: Vec<BaselineBucket>,
    keys: Vec<u64>,
    mask: usize,
}

impl Baseline {
    fn build(keys: &[u64]) -> Self {
        let bucket_count = (keys.len() * 2).next_power_of_two();
        let mut this = Self {
            buckets: vec![BaselineBucket(0); bucket_count],
            keys: keys.to_vec(),
            mask: bucket_count - 1,
        };
        for (position, &key) in keys.iter().enumerate() {
            let mut bucket = key as usize & this.mask;
            while this.buckets[bucket].0 != 0 {
                bucket = (bucket + 1) & this.mask;
            }
            this.buckets[bucket] = BaselineBucket(position as u32 + 1);
        }
        this
    }

    #[inline]
    fn get(&self, key: u64) -> Option<u32> {
        let mut bucket = key as usize & self.mask;
        loop {
            let ordinal = self.buckets[bucket].0;
            if ordinal == 0 {
                return None;
            }
            if self.keys[ordinal as usize - 1] == key {
                return Some(ordinal - 1);
            }
            bucket = (bucket + 1) & self.mask;
        }
    }
}

struct Swiss {
    groups: Vec<Group>,
    keys: Vec<u64>,
    group_mask: usize,
}

impl Swiss {
    fn build(keys: &[u64]) -> Self {
        // At least one group vacancy and no more than 14/16 occupancy.
        let group_count = keys.len().div_ceil(14).next_power_of_two();
        let mut this = Self {
            groups: (0..group_count).map(|_| Group::empty()).collect(),
            keys: keys.to_vec(),
            group_mask: group_count - 1,
        };
        for (position, &key) in keys.iter().enumerate() {
            let fingerprint = h2(key);
            let mut group = h1(key) & this.group_mask;
            loop {
                if let Some(lane) = this.groups[group].control.iter().position(|&byte| byte == EMPTY) {
                    this.groups[group].entries[lane] = position as u32 + 1;
                    this.groups[group].control[lane] = fingerprint;
                    break;
                }
                group = (group + 1) & this.group_mask;
            }
        }
        this
    }

    #[inline]
    fn get_scalar(&self, key: u64) -> Option<u32> {
        let fingerprint = h2(key);
        let mut group = h1(key) & self.group_mask;
        loop {
            let current = &self.groups[group];
            let mut saw_empty = false;
            for lane in 0..16 {
                let control = current.control[lane];
                saw_empty |= control == EMPTY;
                if control == fingerprint {
                    let ordinal = current.entries[lane];
                    if self.keys[ordinal as usize - 1] == key {
                        return Some(ordinal - 1);
                    }
                }
            }
            if saw_empty {
                return None;
            }
            group = (group + 1) & self.group_mask;
        }
    }

    #[inline]
    fn get_simd(&self, key: u64) -> Option<u32> {
        let fingerprint = h2(key);
        let mut group = h1(key) & self.group_mask;
        loop {
            let current = &self.groups[group];
            let (mut matches, empties) = control_candidates(&current.control, fingerprint);
            while matches != 0 {
                let lane = matches.trailing_zeros() as usize;
                let ordinal = current.entries[lane];
                if self.keys[ordinal as usize - 1] == key {
                    return Some(ordinal - 1);
                }
                matches &= matches - 1;
            }
            if empties != 0 {
                return None;
            }
            group = (group + 1) & self.group_mask;
        }
    }

    #[inline]
    fn get_simd_hybrid(&self, key: u64) -> Option<u32> {
        let fingerprint = h2(key);
        let mut group = h1(key) & self.group_mask;
        loop {
            let current = &self.groups[group];
            let (has_match, has_empty) = control_any(&current.control, fingerprint);
            if has_match {
                for lane in 0..16 {
                    if current.control[lane] == fingerprint {
                        let ordinal = current.entries[lane];
                        if self.keys[ordinal as usize - 1] == key {
                            return Some(ordinal - 1);
                        }
                    }
                }
            }
            if has_empty {
                return None;
            }
            group = (group + 1) & self.group_mask;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn control_any(control: &[u8; 16], fingerprint: u8) -> (bool, bool) {
    use std::arch::aarch64::{vceqq_u8, vdupq_n_u8, vld1q_u8, vmaxvq_u8};
    // SAFETY: same proof as `control_candidates` below.
    unsafe {
        let lanes = vld1q_u8(control.as_ptr());
        (
            vmaxvq_u8(vceqq_u8(lanes, vdupq_n_u8(fingerprint))) != 0,
            vmaxvq_u8(vceqq_u8(lanes, vdupq_n_u8(EMPTY))) != 0,
        )
    }
}

#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn control_any(control: &[u8; 16], fingerprint: u8) -> (bool, bool) {
    (control.contains(&fingerprint), control.contains(&EMPTY))
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn control_candidates(control: &[u8; 16], fingerprint: u8) -> (u16, u16) {
    use std::arch::aarch64::{
        vandq_u8, vceqq_u8, vdupq_n_u8, vgetq_lane_u64, vld1q_u8,
        vpaddlq_u16, vpaddlq_u32, vpaddlq_u8,
    };
    // SAFETY: `control` proves a readable 16-byte region. AArch64 NEON supports
    // unaligned loads, though Group also supplies 16-byte alignment. These
    // operations have no target feature beyond mandatory AArch64 Advanced SIMD.
    unsafe {
        let lanes = vld1q_u8(control.as_ptr());
        let matches = vceqq_u8(lanes, vdupq_n_u8(fingerprint));
        let empties = vceqq_u8(lanes, vdupq_n_u8(EMPTY));
        let weights = vld1q_u8([1_u8, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128].as_ptr());
        let movemask = |comparison| {
            let weighted = vandq_u8(comparison, weights);
            let pairs = vpaddlq_u8(weighted);
            let quads = vpaddlq_u16(pairs);
            let octets = vpaddlq_u32(quads);
            (vgetq_lane_u64::<0>(octets) | (vgetq_lane_u64::<1>(octets) << 8)) as u16
        };
        (movemask(matches), movemask(empties))
    }
}

#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn control_candidates(control: &[u8; 16], fingerprint: u8) -> (u16, u16) {
    let mut matches = 0_u16;
    let mut empties = 0_u16;
    for (lane, &byte) in control.iter().enumerate() {
        matches |= u16::from(byte == fingerprint) << lane;
        empties |= u16::from(byte == EMPTY) << lane;
    }
    (matches, empties)
}

#[inline]
fn h1(key: u64) -> usize {
    key as usize
}

#[inline]
fn h2(key: u64) -> u8 {
    ((key >> 57) as u8) & 0x7f
}

fn random_keys(count: usize, seed: u64) -> Vec<u64> {
    let mut state = seed;
    (0..count).map(|_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // Multiplication separates low bucket bits from generator artifacts.
        state.wrapping_mul(0x9e37_79b9_7f4a_7c15)
    }).collect()
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn measure(mut operation: impl FnMut() -> u64) -> (Duration, u64) {
    let expected = operation();
    let mut observed = expected;
    let elapsed = median((0..RUNS).map(|_| {
        let started = Instant::now();
        observed = black_box(operation());
        started.elapsed()
    }).collect());
    assert_eq!(observed, expected);
    (elapsed, observed)
}

fn main() {
    assert_eq!(size_of::<Group>(), 80);
    let keys = random_keys(ENTRIES, 0x1234_5678_9abc_def0);
    let baseline = Baseline::build(&keys);
    let swiss = Swiss::build(&keys);
    let random = random_keys(QUERIES, 0x0ddc_0ffe_e15e_beef);
    let mixed: Vec<_> = random.iter().enumerate().map(|(index, &miss)| {
        if index & 1 == 0 { keys[index % keys.len()] } else { miss }
    }).collect();
    println!("format\tswiss-index-v1");
    println!("entries\tqueries\tdistribution\trepresentation\tmetadata_bytes\tmedian_ns\tchecksum");
    let hits: Vec<_> = (0..QUERIES).map(|index| keys[index % keys.len()]).collect();
    for (distribution, queries) in [("hits", &hits), ("misses", &random), ("mixed", &mixed)] {
        let (baseline_time, baseline_sum) = measure(|| queries.iter().map(|&key| u64::from(baseline.get(key).unwrap_or(u32::MAX))).sum());
        let (scalar_time, scalar_sum) = measure(|| queries.iter().map(|&key| u64::from(swiss.get_scalar(key).unwrap_or(u32::MAX))).sum());
        let (simd_time, simd_sum) = measure(|| queries.iter().map(|&key| u64::from(swiss.get_simd(key).unwrap_or(u32::MAX))).sum());
        let (hybrid_time, hybrid_sum) = measure(|| queries.iter().map(|&key| u64::from(swiss.get_simd_hybrid(key).unwrap_or(u32::MAX))).sum());
        assert_eq!(baseline_sum, scalar_sum);
        assert_eq!(baseline_sum, simd_sum);
        assert_eq!(baseline_sum, hybrid_sum);
        println!("{ENTRIES}\t{QUERIES}\t{distribution}\tbaseline-half-load\t{}\t{}\t{baseline_sum}", baseline.buckets.len() * size_of::<BaselineBucket>(), baseline_time.as_nanos());
        println!("{ENTRIES}\t{QUERIES}\t{distribution}\tswiss-scalar\t{}\t{}\t{scalar_sum}", swiss.groups.len() * size_of::<Group>(), scalar_time.as_nanos());
        println!("{ENTRIES}\t{QUERIES}\t{distribution}\tswiss-neon\t{}\t{}\t{simd_sum}", swiss.groups.len() * size_of::<Group>(), simd_time.as_nanos());
        println!("{ENTRIES}\t{QUERIES}\t{distribution}\tswiss-neon-hybrid\t{}\t{}\t{hybrid_sum}", swiss.groups.len() * size_of::<Group>(), hybrid_time.as_nanos());
    }
}
