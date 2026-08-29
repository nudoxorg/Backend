//! Fixed-word rank primitives for the two sparse locality bit lanes.

use core::mem::size_of;

use super::layout::{RANK_BLOCK_ROWS, WORD_BITS, WORDS_PER_RANK_BLOCK};

const WORD_BYTES: usize = size_of::<u64>();

/// Returns the membership bit for one already validated ordinal.
pub(super) fn member(bits: &[u8], ordinal: u32) -> bool {
    let word = read_word(bits, ordinal / WORD_BITS);
    word & (1_u64 << (ordinal % WORD_BITS)) != 0
}

/// Returns the exclusive population rank for one already validated ordinal.
pub(super) fn rank(bits: &[u8], prefixes: &[u8], ordinal: u32) -> u32 {
    rank_words(bits, prefixes, ordinal).0
}

/// Returns exclusive rank and exact word-popcount work for diagnostics.
pub(super) fn measured_rank(bits: &[u8], prefixes: &[u8], ordinal: u32) -> (u32, u32) {
    rank_words(bits, prefixes, ordinal)
}

fn rank_words(bits: &[u8], prefixes: &[u8], ordinal: u32) -> (u32, u32) {
    let block = ordinal / RANK_BLOCK_ROWS;
    let mut population = if block == 0 {
        0
    } else {
        read_prefix(prefixes, block - 1)
    };
    let within_block = ordinal % RANK_BLOCK_ROWS;
    let whole_words = within_block / WORD_BITS;
    let first_word = block * WORDS_PER_RANK_BLOCK;
    let mut word = 0;
    while word < whole_words {
        population += read_word(bits, first_word + word).count_ones();
        word += 1;
    }
    let partial = read_word(bits, first_word + whole_words);
    population += (partial & low_mask(within_block % WORD_BITS)).count_ones();
    (population, whole_words + 1)
}

/// Visits each retained directory entry and returns total bit population.
/// The first rank block is intentionally omitted because its prefix is always
/// zero. Bits are scanned exactly once in increasing fixed-word order.
pub(super) fn visit_prefixes(bits: &[u8], count: u32, mut visit: impl FnMut(u32, u32)) -> u32 {
    let blocks = count.div_ceil(RANK_BLOCK_ROWS);
    let mut population = 0;
    let mut block = 0;
    while block < blocks {
        if block != 0 {
            visit(block - 1, population);
        }
        population += block_population(bits, count, block);
        block += 1;
    }
    population
}

/// Validates every retained prefix while calculating the lane population in
/// the same monotone word pass.
pub(super) fn validate_prefixes(
    bits: &[u8],
    prefixes: &[u8],
    count: u32,
) -> Result<u32, (u32, u32, u32)> {
    let blocks = count.div_ceil(RANK_BLOCK_ROWS);
    let mut population = 0;
    let mut block = 0;
    while block < blocks {
        if block != 0 {
            let prefix = read_prefix(prefixes, block - 1);
            if prefix != population {
                return Err((block - 1, prefix, population));
            }
        }
        population += block_population(bits, count, block);
        block += 1;
    }
    Ok(population)
}

fn block_population(bits: &[u8], count: u32, block: u32) -> u32 {
    let first_word = block * WORDS_PER_RANK_BLOCK;
    let block_end = ((block + 1) * RANK_BLOCK_ROWS).min(count);
    let words = (block_end - block * RANK_BLOCK_ROWS).div_ceil(WORD_BITS);
    let mut population = 0;
    let mut word = 0;
    while word < words {
        let mut value = read_word(bits, first_word + word);
        if word + 1 == words && !block_end.is_multiple_of(WORD_BITS) {
            value &= low_mask(block_end % WORD_BITS);
        }
        population += value.count_ones();
        word += 1;
    }
    population
}

/// High bits that must be zero in the final partial membership byte.
pub(super) const fn unused_tail_mask(count: u32) -> u8 {
    let used = count % u8::BITS;
    if used == 0 { 0 } else { !((1_u8 << used) - 1) }
}

const fn low_mask(bits: u32) -> u64 {
    if bits == 0 {
        0
    } else {
        u64::MAX >> (u64::BITS - bits)
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "a validated lane has ceil(row_count / 8) bytes, so this fixed-word window is within the lane or its zero-filled tail"
)]
fn read_word(bits: &[u8], word: u32) -> u64 {
    let start = native(word) * WORD_BYTES;
    let remaining = (bits.len() - start).min(WORD_BYTES);
    let mut bytes = [0_u8; WORD_BYTES];
    bytes[..remaining].copy_from_slice(&bits[start..start + remaining]);
    u64::from_le_bytes(bytes)
}

#[allow(
    clippy::indexing_slicing,
    reason = "the validated omitted-zero rank lane contains exactly four bytes for every retained prefix ordinal"
)]
fn read_prefix(prefixes: &[u8], ordinal: u32) -> u32 {
    let start = native(ordinal) * size_of::<u32>();
    u32::from_be_bytes([
        prefixes[start],
        prefixes[start + 1],
        prefixes[start + 2],
        prefixes[start + 3],
    ])
}

#[allow(
    clippy::as_conversions,
    reason = "rank blocks and membership words use compact u32 coordinates bounded by validated lane counts"
)]
const fn native(value: u32) -> usize {
    value as usize
}
