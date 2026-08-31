//! Defines locality artifact rows behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the locality artifact rows invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Strict big-endian row validation with scalar and caller-dispatched SIMD paths.

use core::mem::size_of;

use fearless_simd::{Level, dispatch, prelude::*, u32x4};

const ROW_BYTES: usize = size_of::<u32>();

pub(super) const SIMD_MIN_ROWS: u32 = 32;

#[cfg(test)]
const RANDOM_LONG_CASES: u64 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowError {
    OutOfRange {
        ordinal: u32,
        row: u32,
    },
    NotStrict {
        ordinal: u32,
        previous: u32,
        current: u32,
    },
}

pub(super) fn validate_scalar(rows: &[u8], count: u32, root_count: u32) -> Result<(), RowError> {
    validate_scalar_from(rows, count, root_count, 0)
}

pub(super) fn validate_accelerated(
    rows: &[u8],
    count: u32,
    root_count: u32,
    level: Level,
) -> Result<(), RowError> {
    if count < SIMD_MIN_ROWS {
        return validate_scalar(rows, count, root_count);
    }
    validate_level(rows, count, root_count, level)
}

fn validate_level(rows: &[u8], count: u32, root_count: u32, level: Level) -> Result<(), RowError> {
    dispatch!(level, simd => validate_vectors(simd, rows, count, root_count))
}

#[allow(
    clippy::inline_always,
    reason = "fearless SIMD dispatch must inline the generic kernel into its selected target implementation"
)]
#[inline(always)]
fn validate_vectors<SimdBackend: Simd>(
    simd: SimdBackend,
    rows: &[u8],
    count: u32,
    root_count: u32,
) -> Result<(), RowError> {
    if count == 0 {
        return Ok(());
    }
    validate_one(rows, root_count, 0)?;
    let mut ordinal = 1;
    ordinal =
        scan_vectors::<SimdBackend, SimdBackend::u32s>(simd, rows, count, root_count, ordinal);
    if ordinal == count {
        return Ok(());
    }
    if <SimdBackend::u32s as SimdBase<SimdBackend>>::N
        > <u32x4<SimdBackend> as SimdBase<SimdBackend>>::N
    {
        ordinal =
            scan_vectors::<SimdBackend, u32x4<SimdBackend>>(simd, rows, count, root_count, ordinal);
    }
    validate_scalar_from(rows, count, root_count, ordinal)
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::inline_always,
    clippy::indexing_slicing,
    reason = "validated row windows make lane conversions exact, and the dispatched generic kernel must specialize for its fixed vector width"
)]
#[inline(always)]
fn scan_vectors<SimdBackend, Vector>(
    simd: SimdBackend,
    rows: &[u8],
    count: u32,
    root_count: u32,
    start: u32,
) -> u32
where
    SimdBackend: Simd,
    Vector: SimdInt<SimdBackend, Element = u32>,
{
    let lanes = Vector::N as u32;
    let vector_bytes = Vector::ByteVector::N;
    let available = count - start;
    let complete = available / lanes;
    let previous_start = (start as usize - 1) * ROW_BYTES;
    let current_start = start as usize * ROW_BYTES;
    let previous = &rows[previous_start..];
    let current = &rows[current_start..];
    let mut ordinal = start;
    for (before, values) in previous
        .chunks_exact(vector_bytes)
        .zip(current.chunks_exact(vector_bytes))
        .take(complete as usize)
    {
        let before = decode_be(Vector::from_bytes(Vector::ByteVector::from_slice(
            simd, before,
        )));
        let values = decode_be(Vector::from_bytes(Vector::ByteVector::from_slice(
            simd, values,
        )));
        let bad = values.simd_ge(root_count) | values.simd_le(before);
        let mask = bad.to_bitmask();
        if mask != 0 {
            return ordinal + mask.trailing_zeros();
        }
        ordinal += lanes;
    }
    ordinal
}

#[allow(
    clippy::inline_always,
    reason = "the endian shuffle is a leaf SIMD operation that must specialize into the dispatched vector kernel"
)]
#[inline(always)]
fn decode_be<SimdBackend, Vector>(value: Vector) -> Vector
where
    SimdBackend: Simd,
    Vector: SimdInt<SimdBackend, Element = u32>,
{
    #[cfg(target_endian = "little")]
    {
        ((value & 0x0000_00ff) << 24)
            | ((value & 0x0000_ff00) << 8)
            | ((value & 0x00ff_0000) >> 8)
            | ((value & 0xff00_0000) >> 24)
    }
    #[cfg(target_endian = "big")]
    {
        value
    }
}

fn validate_scalar_from(
    rows: &[u8],
    count: u32,
    root_count: u32,
    mut ordinal: u32,
) -> Result<(), RowError> {
    while ordinal < count {
        validate_one(rows, root_count, ordinal)?;
        ordinal += 1;
    }
    Ok(())
}

fn validate_one(rows: &[u8], root_count: u32, ordinal: u32) -> Result<(), RowError> {
    let row = read(rows, ordinal);
    if row >= root_count {
        return Err(RowError::OutOfRange { ordinal, row });
    }
    if ordinal != 0 {
        let previous = read(rows, ordinal - 1);
        if row <= previous {
            return Err(RowError::NotStrict {
                ordinal,
                previous,
                current: row,
            });
        }
    }
    Ok(())
}

#[allow(
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "the exact validated row lane contains one big-endian u32 for every compact ordinal"
)]
fn read(rows: &[u8], ordinal: u32) -> u32 {
    let start = ordinal as usize * ROW_BYTES;
    u32::from_be_bytes([
        rows[start],
        rows[start + 1],
        rows[start + 2],
        rows[start + 3],
    ])
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use fearless_simd::Level;

    use super::{
        RANDOM_LONG_CASES, RowError, validate_accelerated, validate_level, validate_scalar,
    };

    fn backing(values: &[u32], alignment: usize) -> Vec<u8> {
        let mut bytes = alloc::vec![0xa5; alignment];
        for value in values {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes
    }

    #[test]
    #[allow(
        clippy::as_conversions,
        clippy::indexing_slicing,
        reason = "the exhaustive test indexes each generated short-row lane by its originating bounded u32 ordinal"
    )]
    fn every_short_tail_and_bad_lane_matches_scalar() {
        for length in 0..=33_u32 {
            for alignment in 0..=15 {
                let valid: Vec<_> = (0..length).collect();
                assert_case(&valid, length, alignment, length + 1);
                for bad in 0..length {
                    let mut malformed = valid.clone();
                    malformed[bad as usize] = if bad == 0 { length + 1 } else { bad - 1 };
                    assert_case(&malformed, length, alignment, length + 1);
                }
            }
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "the test owner deliberately retains the exact requested alignment prefix"
    )]
    fn assert_case(values: &[u32], count: u32, alignment: usize, root_count: u32) {
        let bytes = backing(values, alignment);
        assert_levels(&bytes[alignment..], count, root_count);
    }

    fn assert_levels(bytes: &[u8], count: u32, root_count: u32) {
        let expected = validate_scalar(bytes, count, root_count);
        assert_eq!(
            validate_level(bytes, count, root_count, Level::fallback()),
            expected
        );
        assert_eq!(
            validate_level(bytes, count, root_count, Level::baseline()),
            expected
        );
        assert_eq!(
            validate_level(bytes, count, root_count, Level::new()),
            expected
        );
        assert_eq!(
            validate_accelerated(bytes, count, root_count, Level::new()),
            expected
        );
    }

    #[test]
    fn error_priority_and_ordinals_are_exact() {
        let bytes = backing(&[0, 2, 2, 9], 0);
        assert_eq!(
            validate_accelerated(&bytes, 4, 4, Level::baseline()),
            Err(RowError::NotStrict {
                ordinal: 2,
                previous: 2,
                current: 2,
            })
        );
        let bytes = backing(&[0, 1, 4, 4], 0);
        assert_eq!(
            validate_accelerated(&bytes, 4, 4, Level::baseline()),
            Err(RowError::OutOfRange { ordinal: 2, row: 4 })
        );
    }

    #[test]
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::indexing_slicing,
        reason = "the deterministic test intentionally projects pseudo-random low bits into bounded row, lane, and alignment domains"
    )]
    fn random_long_rows_match_every_level() {
        let mut seed = 0x6d5a_56da_3f21_948b_u64;
        // Sixteen complete alignment rotations, each with varied vector tails
        // and invalidity placement, complement the exhaustive short corpus.
        for corpus_case in 0..RANDOM_LONG_CASES {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(corpus_case | 1);
            let count = 64 + (seed as u32 % 2_048);
            let mut values: Vec<_> = (0..count).map(|row| row * 2).collect();
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let bad = seed as u32 % count;
            if seed & 1 == 0 {
                values[bad as usize] = count * 2 + 1;
            } else if bad != 0 {
                values[bad as usize] = values[bad as usize - 1];
            }
            assert_case(&values, count, corpus_case as usize & 15, count * 2 + 1);
        }
    }
}
