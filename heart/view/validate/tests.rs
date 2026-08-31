//! Defines validate tests behavior for `heart-view`, whose purpose is to validate and borrow canonical framed data without allocating.
//! This module owns the validate tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::size_of;

use heart_frame::{EncodeError, SectionInput, encode_into};
use heart_schema::{
    ArenaBytesLimit, DESCRIPTOR_BODY_LENGTH_OFFSET, DESCRIPTOR_BODY_OFFSET,
    DESCRIPTOR_FLAGS_OFFSET, DESCRIPTOR_KIND_OFFSET, DESCRIPTOR_ROWS_OFFSET, DecodeLimits,
    FORMAT_MAJOR, FORMAT_MINOR, FRAME_HEADER_BYTES, FRAME_MAGIC, FrameBytesLimit,
    HEADER_LENGTH_OFFSET, HEADER_MAJOR_OFFSET, HEADER_MINOR_OFFSET, HEADER_SCHEMA_OFFSET,
    HEADER_SECTION_COUNT_OFFSET, HEADER_TOTAL_LENGTH_OFFSET, LimitError, LimitKind, RowCount,
    RowCountLimit, SECTION_ALIGNMENT, SECTION_DESCRIPTOR_BYTES, SchemaId,
    SectionCompatibilityError, SectionCountLimit, SectionKind, SectionKindCode,
};
use rstest::rstest;

use super::{DescriptorError, Section, SectionReadError, Sections, ValidateError, ValidatedFrame};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error(transparent)]
    Limit(#[from] LimitError),
    #[error(transparent)]
    Validate(#[from] ValidateError),
    #[error(transparent)]
    SectionRead(#[from] SectionReadError),
    #[error("fixture range was outside its frame")]
    FixtureRange,
}

fn rows(value: u32) -> Result<RowCount, TestError> {
    Ok(RowCount::try_from(value)?)
}

fn one_section_frame() -> Result<[u8; 41], TestError> {
    let sections = [SectionInput {
        kind: SectionKind::Data,
        rows: rows(1)?,
        bytes: &[0xa5],
    }];
    let mut frame = [0_u8; 41];
    assert_eq!(usize::from(encode_into(&sections, &mut frame)?), 41);
    Ok(frame)
}

fn two_section_frame() -> Result<[u8; 65], TestError> {
    let sections = [
        SectionInput {
            kind: SectionKind::Metadata,
            rows: rows(0)?,
            bytes: &[0x11],
        },
        SectionInput {
            kind: SectionKind::Rows,
            rows: rows(1)?,
            bytes: &[0x22],
        },
    ];
    let mut frame = [0_u8; 65];
    assert_eq!(usize::from(encode_into(&sections, &mut frame)?), 65);
    Ok(frame)
}

fn put_byte(bytes: &mut [u8], offset: usize, value: u8) -> Result<(), TestError> {
    let target = bytes.get_mut(offset).ok_or(TestError::FixtureRange)?;
    *target = value;
    Ok(())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), TestError> {
    let end = offset
        .checked_add(size_of::<u32>())
        .ok_or(TestError::FixtureRange)?;
    let target = bytes.get_mut(offset..end).ok_or(TestError::FixtureRange)?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<(), TestError> {
    let end = offset
        .checked_add(size_of::<u16>())
        .ok_or(TestError::FixtureRange)?;
    let target = bytes.get_mut(offset..end).ok_or(TestError::FixtureRange)?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn optional_descriptors_around(kind: SectionKind) -> Result<[u8; 72], TestError> {
    let mut frame = [0_u8; 72];
    frame
        .get_mut(..FRAME_MAGIC.len())
        .ok_or(TestError::FixtureRange)?
        .copy_from_slice(&FRAME_MAGIC);
    put_byte(&mut frame, HEADER_MAJOR_OFFSET, u8::from(FORMAT_MAJOR))?;
    put_byte(&mut frame, HEADER_MINOR_OFFSET, u8::from(FORMAT_MINOR))?;
    put_u16(&mut frame, HEADER_LENGTH_OFFSET, 72)?;
    put_u32(&mut frame, HEADER_TOTAL_LENGTH_OFFSET, 72)?;
    put_u16(&mut frame, HEADER_SECTION_COUNT_OFFSET, 3)?;
    put_u32(&mut frame, HEADER_SCHEMA_OFFSET, u32::from(SchemaId::Frame))?;
    for (index, (code, flags)) in [
        (0_u8, 1_u8),
        (u8::from(SectionKindCode::from(kind)), 0_u8),
        (0x40_u8, 1_u8),
    ]
    .iter()
    .enumerate()
    {
        let descriptor = FRAME_HEADER_BYTES + index * SECTION_DESCRIPTOR_BYTES;
        put_byte(&mut frame, descriptor + DESCRIPTOR_KIND_OFFSET, *code)?;
        put_byte(&mut frame, descriptor + DESCRIPTOR_FLAGS_OFFSET, *flags)?;
        put_u32(&mut frame, descriptor + DESCRIPTOR_BODY_OFFSET, 72)?;
        put_u32(&mut frame, descriptor + DESCRIPTOR_BODY_LENGTH_OFFSET, 0)?;
        put_u32(&mut frame, descriptor + DESCRIPTOR_ROWS_OFFSET, 0)?;
    }
    Ok(frame)
}

fn limits_with(kind: LimitKind, maximum: u32) -> Result<DecodeLimits, TestError> {
    Ok(DecodeLimits {
        frame_bytes: FrameBytesLimit::try_from(if kind == LimitKind::FrameBytes {
            maximum
        } else {
            41
        })?,
        sections: SectionCountLimit::try_from(if kind == LimitKind::Sections {
            match maximum {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => return Err(TestError::FixtureRange),
            }
        } else {
            1
        })?,
        rows: RowCountLimit::try_from(if kind == LimitKind::Rows { maximum } else { 1 })?,
        arena_bytes: ArenaBytesLimit::try_from(if kind == LimitKind::ArenaBytes {
            maximum
        } else {
            1
        })?,
    })
}

const fn observed(kind: LimitKind) -> u32 {
    match kind {
        LimitKind::FrameBytes => 41,
        LimitKind::Sections | LimitKind::Rows | LimitKind::ArenaBytes => 1,
    }
}

#[test]
fn validation_returns_a_borrowed_compact_witness() -> Result<(), TestError> {
    let bytes = two_section_frame()?;
    let validated = ValidatedFrame::validate(&bytes)?;
    assert_borrowed_witness_header(&validated, &bytes);
    assert_borrowed_two_section_iteration(&validated, &bytes)
}

fn assert_borrowed_witness_header(validated: &ValidatedFrame<'_>, bytes: &[u8]) {
    assert_eq!(validated.as_ptr(), bytes.as_ptr());
    assert_eq!(validated.known.membership_count(), 2);
}

fn assert_borrowed_two_section_iteration(
    validated: &ValidatedFrame<'_>,
    bytes: &[u8],
) -> Result<(), TestError> {
    let mut sections = validated.sections();
    assert_eq!(sections.size_hint(), (2, Some(2)));
    let first = next_section(&mut sections)?;
    assert_eq!(sections.len(), 1);
    assert_section_at(first, SectionKind::Metadata, bytes, 56);
    let second = next_section(&mut sections)?;
    assert_section_at(second, SectionKind::Rows, bytes, 64);
    assert_eq!(sections.len(), 0);
    assert_eq!(sections.next(), None);
    Ok(())
}

fn next_section<'bytes>(sections: &mut Sections<'bytes>) -> Result<Section<'bytes>, TestError> {
    Ok(sections.next().ok_or(TestError::FixtureRange)??)
}

fn assert_section_at(section: Section<'_>, kind: SectionKind, bytes: &[u8], offset: usize) {
    assert_eq!(section.kind, kind);
    assert_eq!(section.bytes.as_ptr(), bytes.as_ptr().wrapping_add(offset));
}

#[test]
fn validated_frame_has_a_fixed_inline_metadata_budget() {
    assert_eq!(size_of::<ValidatedFrame<'static>>(), 24);
    assert_eq!(size_of::<Sections<'static>>(), 24);
}

#[rstest]
#[case::empty(0)]
#[case::metadata_only(1)]
#[case::metadata_and_rows(2)]
#[case::all_known_kinds(3)]
fn encoder_and_reborrowed_view_are_differentially_identical_for_each_canonical_prefix(
    #[case] count: usize,
) -> Result<(), TestError> {
    let all = [
        SectionInput {
            kind: SectionKind::Metadata,
            rows: rows(0)?,
            bytes: &[0x10],
        },
        SectionInput {
            kind: SectionKind::Rows,
            rows: rows(1)?,
            bytes: &[0x20],
        },
        SectionInput {
            kind: SectionKind::Data,
            rows: rows(2)?,
            bytes: &[0x30],
        },
    ];
    let inputs = all.get(..count).ok_or(TestError::FixtureRange)?;
    let mut storage =
        [0_u8; FRAME_HEADER_BYTES + 3 * SECTION_DESCRIPTOR_BYTES + 3 * SECTION_ALIGNMENT];
    let written = usize::from(encode_into(inputs, &mut storage)?);
    let frame = storage.get(..written).ok_or(TestError::FixtureRange)?;
    let validated = ValidatedFrame::validate(frame)?;
    assert_eq!(usize::from(validated.known.membership_count()), count);
    let mut sections = validated.sections();
    assert_eq!(sections.len(), count);
    for (position, source) in inputs.iter().enumerate() {
        assert_eq!(sections.len(), count - position);
        let observed = sections.next().ok_or(TestError::FixtureRange)??;
        assert_eq!(sections.len(), count - position - 1);
        assert_eq!(observed.kind, source.kind);
        assert_eq!(*observed.rows, *source.rows);
        assert_eq!(observed.bytes, source.bytes);
    }
    assert_eq!(sections.len(), 0);
    assert_eq!(sections.next(), None);
    Ok(())
}

#[rstest]
#[case::empty(0)]
#[case::one_byte(1)]
#[case::last_missing_header_byte(23)]
#[case::header_only(24)]
#[case::last_missing_body_byte(40)]
fn every_frame_boundary_has_the_exact_truncation_provenance(
    #[case] length: usize,
) -> Result<(), TestError> {
    let bytes = one_section_frame()?;
    let input = bytes.get(..length).ok_or(TestError::FixtureRange)?;
    let expected = if length < 24 {
        Err(ValidateError::Truncated {
            offset: 0,
            needed: 24,
            available: length,
        })
    } else {
        Err(ValidateError::TotalLengthMismatch {
            declared: 41,
            actual: length,
        })
    };
    assert_eq!(ValidatedFrame::validate(input), expected);
    Ok(())
}

#[test]
fn header_mutations_report_exact_records() -> Result<(), TestError> {
    let bytes = one_section_frame()?;

    let mut magic = bytes;
    put_byte(&mut magic, 0, b'X')?;
    assert_eq!(
        ValidatedFrame::validate(&magic),
        Err(ValidateError::BadMagic { observed: *b"XDX1" })
    );
    let mut version = bytes;
    put_byte(&mut version, 4, 2)?;
    assert_eq!(
        ValidatedFrame::validate(&version),
        Err(ValidateError::UnsupportedVersion { major: 2, minor: 0 })
    );
    let mut schema = bytes;
    put_u32(&mut schema, 16, 0xfeed_beef)?;
    assert_eq!(
        ValidatedFrame::validate(&schema),
        Err(ValidateError::UnsupportedSchema { wire: 0xfeed_beef })
    );
    Ok(())
}

#[test]
fn descriptor_attack_matrix_preserves_every_rejected_field() -> Result<(), TestError> {
    assert_exact_overlap_error()?;
    assert_exact_kind_order_errors()?;
    assert_exact_alignment_and_reserved_errors()?;
    Ok(())
}

fn assert_exact_overlap_error() -> Result<(), TestError> {
    let mut overlap = two_section_frame()?;
    put_u32(&mut overlap, 44, 56)?;
    assert_eq!(
        ValidatedFrame::validate(&overlap),
        Err(ValidateError::Descriptor {
            index: 1,
            error: DescriptorError::OverlappingBody {
                offset: 56,
                previous_end: 57,
            },
        })
    );
    Ok(())
}

fn assert_exact_kind_order_errors() -> Result<(), TestError> {
    let mut duplicate = two_section_frame()?;
    put_byte(
        &mut duplicate,
        40,
        u8::from(SectionKindCode::from(SectionKind::Metadata)),
    )?;
    assert_eq!(
        ValidatedFrame::validate(&duplicate),
        Err(ValidateError::Descriptor {
            index: 1,
            error: DescriptorError::DuplicateKind { kind: 1 },
        })
    );
    let mut out_of_order = two_section_frame()?;
    put_byte(
        &mut out_of_order,
        24,
        u8::from(SectionKindCode::from(SectionKind::Rows)),
    )?;
    put_byte(
        &mut out_of_order,
        40,
        u8::from(SectionKindCode::from(SectionKind::Metadata)),
    )?;
    assert_eq!(
        ValidatedFrame::validate(&out_of_order),
        Err(ValidateError::Descriptor {
            index: 1,
            error: DescriptorError::OutOfOrderKind {
                previous: 2,
                next: 1,
            },
        })
    );
    Ok(())
}

fn assert_exact_alignment_and_reserved_errors() -> Result<(), TestError> {
    let mut misaligned = one_section_frame()?;
    put_u32(&mut misaligned, 28, 41)?;
    assert_eq!(
        ValidatedFrame::validate(&misaligned),
        Err(ValidateError::Descriptor {
            index: 0,
            error: DescriptorError::MisalignedOffset {
                offset: 41,
                alignment: 8,
            },
        })
    );
    let mut reserved = one_section_frame()?;
    put_byte(&mut reserved, 26, 1)?;
    assert_eq!(
        ValidatedFrame::validate(&reserved),
        Err(ValidateError::Descriptor {
            index: 0,
            error: DescriptorError::NonzeroReserved,
        })
    );
    Ok(())
}

#[test]
fn compatibility_and_lowered_limits_have_exact_boundaries() -> Result<(), TestError> {
    let mut optional = one_section_frame()?;
    put_byte(&mut optional, 24, 0x80)?;
    put_byte(&mut optional, 25, 1)?;
    let optional_view = ValidatedFrame::validate(&optional)?;
    assert_eq!(optional_view.known.membership_count(), 0);
    assert_eq!(optional_view.known.absent_count(1), 1);
    assert_eq!(optional_view.sections().len(), 0);
    put_byte(&mut optional, 25, 0)?;
    assert_eq!(
        ValidatedFrame::validate(&optional),
        Err(ValidateError::Descriptor {
            index: 0,
            error: DescriptorError::IncompatibleKind {
                kind: 0x80,
                flags: 0,
                source: SectionCompatibilityError::UnknownRequired { kind: 0x80.into() },
            },
        })
    );
    Ok(())
}

#[rstest]
#[case::metadata(SectionKind::Metadata)]
#[case::rows(SectionKind::Rows)]
#[case::data(SectionKind::Data)]
fn optional_descriptors_bracket_each_known_kind_without_affecting_exact_iteration(
    #[case] kind: SectionKind,
) -> Result<(), TestError> {
    let frame = optional_descriptors_around(kind)?;
    let validated = ValidatedFrame::validate(&frame)?;
    assert_eq!(validated.known.membership_count(), 1);
    assert_eq!(validated.known.absent_count(3), 2);
    let mut sections = validated.sections();
    assert_eq!(sections.len(), 1);
    let observed = sections.next().ok_or(TestError::FixtureRange)??;
    assert_eq!(observed.kind, kind);
    assert_eq!(sections.len(), 0);
    assert_eq!(sections.size_hint(), (0, Some(0)));
    assert_eq!(sections.next(), None);
    assert_eq!(sections.len(), 0);
    Ok(())
}

#[rstest]
#[case::frame_n_minus_one(LimitKind::FrameBytes, 40, false)]
#[case::frame_n(LimitKind::FrameBytes, 41, true)]
#[case::frame_n_plus_one(LimitKind::FrameBytes, 42, true)]
#[case::sections_n_minus_one(LimitKind::Sections, 0, false)]
#[case::sections_n(LimitKind::Sections, 1, true)]
#[case::sections_n_plus_one(LimitKind::Sections, 2, true)]
#[case::rows_n_minus_one(LimitKind::Rows, 0, false)]
#[case::rows_n(LimitKind::Rows, 1, true)]
#[case::rows_n_plus_one(LimitKind::Rows, 2, true)]
#[case::arena_n_minus_one(LimitKind::ArenaBytes, 0, false)]
#[case::arena_n(LimitKind::ArenaBytes, 1, true)]
#[case::arena_n_plus_one(LimitKind::ArenaBytes, 2, true)]
fn decoder_policy_limits_have_n_minus_one_n_n_plus_one_provenance(
    #[case] kind: LimitKind,
    #[case] maximum: u32,
    #[case] accepted: bool,
) -> Result<(), TestError> {
    let frame = one_section_frame()?;
    let result = ValidatedFrame::validate_with_limits(&frame, limits_with(kind, maximum)?);
    if accepted {
        let validated = result?;
        assert_eq!(&*validated, frame.as_slice());
        assert_eq!(validated.known.membership_count(), 1);
        let mut sections = validated.sections();
        assert_eq!(
            sections.next(),
            Some(Ok(Section {
                kind: SectionKind::Data,
                rows: rows(1)?,
                bytes: &[0xa5],
            }))
        );
        assert_eq!(sections.next(), None);
    } else {
        assert_eq!(
            result,
            Err(ValidateError::LimitExceeded {
                kind,
                observed: observed(kind),
                maximum,
            })
        );
    }
    Ok(())
}

#[test]
fn padding_is_checked_but_payload_is_not_scanned() -> Result<(), TestError> {
    let bytes = two_section_frame()?;
    let mut padding = bytes;
    put_byte(&mut padding, 57, 1)?;
    assert_eq!(
        ValidatedFrame::validate(&padding),
        Err(ValidateError::Descriptor {
            index: 1,
            error: DescriptorError::NonzeroPadding { offset: 57 },
        })
    );
    let mut payload = bytes;
    put_byte(&mut payload, 56, 0xff)?;
    put_byte(&mut payload, 64, 0xee)?;
    assert_eq!(ValidatedFrame::validate(&payload)?.sections().len(), 2);
    Ok(())
}
