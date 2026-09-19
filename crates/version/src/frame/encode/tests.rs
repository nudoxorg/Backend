//! Defines encode tests behavior for `frame`, whose purpose is to encode canonical bounded frames into caller-owned storage.
//! This module owns the encode tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::size_of;

use crate::schema::{
    FRAME_HEADER_BYTES, LimitError, MAX_ARENA_BYTES, RowCount, SECTION_ALIGNMENT,
    SECTION_DESCRIPTOR_BYTES, SectionKind,
};
use rstest::rstest;

use super::{EncodeError, PreparedFrame, SectionInput, encode_into, encoded_len};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error(transparent)]
    Limit(#[from] LimitError),
    #[error("rstest capacity case exceeded its fixed backing storage")]
    FixtureRange,
}

fn rows(value: u32) -> Result<RowCount, TestError> {
    Ok(RowCount::try_from(value)?)
}

#[derive(Clone, Copy)]
enum ArenaBoundary {
    OneBelow,
    Exact,
    OneAbove,
}

impl ArenaBoundary {
    const fn candidate(self, maximum: usize) -> usize {
        match self {
            Self::OneBelow => maximum - 1,
            Self::Exact => maximum,
            Self::OneAbove => maximum + 1,
        }
    }

    const fn accepted(self) -> bool {
        matches!(self, Self::OneBelow | Self::Exact)
    }
}

const TWO_SECTION_FRAME_BYTES: usize =
    FRAME_HEADER_BYTES + 2 * SECTION_DESCRIPTOR_BYTES + SECTION_ALIGNMENT + 1;
const UNTOUCHED_TAIL_BYTES: usize = SECTION_ALIGNMENT - 1;

#[rstest]
#[case::one_below(ArenaBoundary::OneBelow)]
#[case::exact(ArenaBoundary::Exact)]
#[case::one_above(ArenaBoundary::OneAbove)]
fn preflight_arena_accumulation_has_exact_n_minus_one_n_n_plus_one_provenance(
    #[case] boundary: ArenaBoundary,
) {
    const MAX_ARENA_BYTES_USIZE: usize = 1_000_000;
    const _: () = assert!(MAX_ARENA_BYTES == 1_000_000);
    let maximum = MAX_ARENA_BYTES_USIZE;
    let value = boundary.candidate(maximum);
    if boundary.accepted() {
        assert_eq!(super::layout::reserve_arena(0, value), Ok(value));
    } else {
        assert_eq!(
            super::layout::reserve_arena(0, value),
            Err(EncodeError::ArenaTooLarge {
                current: 0.into(),
                added: value.into(),
                maximum: MAX_ARENA_BYTES.into(),
            })
        );
    }
}

#[test]
fn empty_frame_has_a_stable_golden_vector() -> Result<(), TestError> {
    let mut bytes = [0xff; FRAME_HEADER_BYTES];
    assert_eq!(
        usize::from(encode_into(&[], &mut bytes)?),
        FRAME_HEADER_BYTES
    );
    assert_eq!(
        bytes,
        [
            b'N', b'D', b'X', b'1', 1, 0, 24, 0, 24, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0,
        ]
    );
    Ok(())
}

#[test]
fn prepared_layout_is_a_reusable_exposed_length_fact() -> Result<(), TestError> {
    let sections = [SectionInput {
        kind: SectionKind::Data,
        rows: rows(0)?,
        bytes: b"payload",
    }];
    let prepared = PreparedFrame::new(&sections)?;
    let expected_len = FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES + b"payload".len();
    assert_eq!(usize::from(prepared.encoded_len), expected_len);
    assert_eq!(encoded_len(&sections)?, prepared.encoded_len);
    let mut first = [0_u8; FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES + b"payload".len()];
    let mut second = [0_u8; FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES + b"payload".len()];
    assert_eq!(usize::from(prepared.encode_into(&mut first)?), expected_len);
    assert_eq!(
        usize::from(prepared.encode_into(&mut second)?),
        expected_len
    );
    assert_eq!(first, second);
    Ok(())
}

#[test]
fn prepared_frame_has_a_fixed_inline_metadata_budget_after_deriving_layout_ends() {
    // Only the borrowed input slice plus the 8-byte wire-authoritative layout
    // survives preparation; descriptor and body coordinates are recomputed.
    assert_eq!(size_of::<PreparedFrame<'static, 'static>>(), 32);
}

#[test]
fn canonical_order_errors_are_exact() -> Result<(), TestError> {
    let duplicate = [
        SectionInput {
            kind: SectionKind::Metadata,
            rows: rows(0)?,
            bytes: &[],
        },
        SectionInput {
            kind: SectionKind::Metadata,
            rows: rows(0)?,
            bytes: &[],
        },
    ];
    assert_eq!(
        encoded_len(&duplicate),
        Err(EncodeError::DuplicateKind {
            kind: SectionKind::Metadata,
        })
    );
    let out_of_order = [
        SectionInput {
            kind: SectionKind::Rows,
            rows: rows(0)?,
            bytes: &[],
        },
        SectionInput {
            kind: SectionKind::Metadata,
            rows: rows(0)?,
            bytes: &[],
        },
    ];
    assert_eq!(
        encoded_len(&out_of_order),
        Err(EncodeError::OutOfOrderKind {
            previous: SectionKind::Rows,
            next: SectionKind::Metadata,
        })
    );
    Ok(())
}

#[rstest]
#[case::one_below(23, true)]
#[case::exact(24, false)]
#[case::one_above(25, false)]
fn empty_frame_capacity_boundary_has_exact_no_write_provenance(
    #[case] available: usize,
    #[case] rejected: bool,
) -> Result<(), TestError> {
    let mut storage = [0xa5; 25];
    let output = storage
        .get_mut(..available)
        .ok_or(TestError::FixtureRange)?;
    let actual = encode_into(&[], output);
    if rejected {
        assert_eq!(
            actual,
            Err(EncodeError::OutputTooShort {
                required: encoded_len(&[])?,
                available: available.into(),
            })
        );
        assert_eq!(storage, [0xa5; 25]);
    } else {
        assert_eq!(actual.map(usize::from), Ok(24));
    }
    Ok(())
}

#[test]
fn canonical_inputs_are_limited_to_the_known_kind_vocabulary() -> Result<(), TestError> {
    let too_many = [
        SectionInput {
            kind: SectionKind::Metadata,
            rows: rows(0)?,
            bytes: &[],
        },
        SectionInput {
            kind: SectionKind::Rows,
            rows: rows(0)?,
            bytes: &[],
        },
        SectionInput {
            kind: SectionKind::Data,
            rows: rows(0)?,
            bytes: &[],
        },
        SectionInput {
            kind: SectionKind::Data,
            rows: rows(0)?,
            bytes: &[],
        },
    ];
    assert_eq!(
        encoded_len(&too_many),
        Err(EncodeError::TooManySections {
            count: 4.into(),
            maximum: 3_u16.into(),
        })
    );
    Ok(())
}

#[test]
fn encoder_writes_exactly_the_preflight_range_and_only_clears_alignment_gaps()
-> Result<(), TestError> {
    let sections = [
        SectionInput {
            kind: SectionKind::Metadata,
            rows: rows(0)?,
            bytes: &[0x11],
        },
        SectionInput {
            kind: SectionKind::Rows,
            rows: rows(0)?,
            bytes: &[0x22],
        },
    ];
    let mut output = [0xa5; TWO_SECTION_FRAME_BYTES + UNTOUCHED_TAIL_BYTES];
    assert_eq!(
        usize::from(encode_into(&sections, &mut output)?),
        TWO_SECTION_FRAME_BYTES
    );
    assert_eq!(output.get(56), Some(&0x11));
    assert_eq!(output.get(57..64), Some(&[0; 7][..]));
    assert_eq!(output.get(64), Some(&0x22));
    assert_eq!(
        output.get(TWO_SECTION_FRAME_BYTES..),
        Some(&[0xa5; UNTOUCHED_TAIL_BYTES][..])
    );
    Ok(())
}
