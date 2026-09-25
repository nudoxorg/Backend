//! Most-significant-digit radix order for 32-byte intro ids.
//!
//! A comparison sort memcmp's the id on every probe. This partitions on a
//! 16-bit digit. Intro ids are uniform, so 32k keys fall into 65536 buckets
//! and almost every bucket has size one after the first digit: the other 30
//! bytes are never read.
//!
//! The sort is stable. Equal ids keep ascending input index, so the last
//! index in a run is the last input occurrence.

/// Order `indices` by the 32-byte id `byte_at(index, byte)` names.
///
/// `indices` should start as `0..n`.
pub fn sort_ids(indices: &mut [u32], byte_at: impl Fn(u32, usize) -> u8) {
    if indices.len() < 2 {
        return;
    }
    let mut scratch = vec![0u32; indices.len()];
    let mut stack = Vec::with_capacity(32);
    stack.push((0usize, indices.len(), 0usize));
    while let Some((start, end, byte)) = stack.pop() {
        partition(indices, &mut scratch, &byte_at, start, end, byte, &mut stack);
    }
}

fn partition(
    indices: &mut [u32],
    scratch: &mut [u32],
    byte_at: &impl Fn(u32, usize) -> u8,
    start: usize,
    end: usize,
    byte: usize,
    stack: &mut Vec<(usize, usize, usize)>,
) {
    let len = end - start;
    if len < 2 || byte >= 32 {
        return;
    }
    if len < 16 || byte > 30 {
        indices[start..end].sort_by(|&a, &b| cmp_id(byte_at, a, b, byte).then(a.cmp(&b)));
        return;
    }
    let digit = |index: u32| -> usize {
        (usize::from(byte_at(index, byte)) << 8) | usize::from(byte_at(index, byte + 1))
    };
    let mut count = vec![0u32; 65536];
    for &index in &indices[start..end] {
        count[digit(index)] += 1;
    }
    let mut origin = vec![0u32; 65536];
    let mut sum = 0u32;
    for bucket in 0..65536 {
        origin[bucket] = sum;
        sum += count[bucket];
    }
    let mut cursor = origin.clone();
    for &index in &indices[start..end] {
        let bucket = digit(index);
        scratch[start + cursor[bucket] as usize] = index;
        cursor[bucket] += 1;
    }
    indices[start..end].copy_from_slice(&scratch[start..end]);
    let next = byte + 2;
    if next >= 32 {
        return;
    }
    for bucket in 0..65536 {
        if count[bucket] > 1 {
            let from = start + origin[bucket] as usize;
            stack.push((from, from + count[bucket] as usize, next));
        }
    }
}

fn cmp_id(byte_at: &impl Fn(u32, usize) -> u8, left: u32, right: u32, from: usize) -> std::cmp::Ordering {
    for byte in from..32 {
        match byte_at(left, byte).cmp(&byte_at(right, byte)) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}
