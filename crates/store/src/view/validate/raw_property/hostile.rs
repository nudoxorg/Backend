//! Defines validate raw-property hostile behavior for `backend_store::view`, whose purpose is to validate and borrow canonical framed data without allocating.
//! This module owns the validate raw-property hostile invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use backend_version::schema::{
    DESCRIPTOR_KIND_OFFSET, DESCRIPTOR_ROWS_OFFSET, FRAME_HEADER_BYTES, HEADER_LENGTH_OFFSET,
    HEADER_SECTION_COUNT_OFFSET, HEADER_TOTAL_LENGTH_OFFSET, LimitKind, MAX_FRAME_BYTES, MAX_ROWS,
    MAX_SECTIONS, SECTION_DESCRIPTOR_BYTES, SectionKind, SectionKindCode,
};

use super::super::{DescriptorError, ValidateError, ValidatedFrame};
use super::{
    CorpusError,
    fixture::{
        ONE_SECTION_CORPUS, TWO_SECTION_CORPUS, read_u16, read_u32, replace_byte, write_u16,
        write_u32,
    },
};

pub(super) fn ordering_laws() -> Result<(), CorpusError> {
    let second_kind = FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES + DESCRIPTOR_KIND_OFFSET;
    let mut duplicate = TWO_SECTION_CORPUS;
    replace_byte(
        &mut duplicate,
        second_kind,
        u8::from(SectionKindCode::from(SectionKind::Metadata)),
    )?;
    assert_eq!(
        ValidatedFrame::validate(&duplicate),
        Err(ValidateError::Descriptor {
            index: 1,
            error: DescriptorError::DuplicateKind {
                kind: u8::from(SectionKindCode::from(SectionKind::Metadata)),
            },
        })
    );

    let mut out_of_order = TWO_SECTION_CORPUS;
    replace_byte(
        &mut out_of_order,
        FRAME_HEADER_BYTES + DESCRIPTOR_KIND_OFFSET,
        u8::from(SectionKindCode::from(SectionKind::Rows)),
    )?;
    replace_byte(
        &mut out_of_order,
        second_kind,
        u8::from(SectionKindCode::from(SectionKind::Metadata)),
    )?;
    assert_eq!(
        ValidatedFrame::validate(&out_of_order),
        Err(ValidateError::Descriptor {
            index: 1,
            error: DescriptorError::OutOfOrderKind {
                previous: u8::from(SectionKindCode::from(SectionKind::Rows)),
                next: u8::from(SectionKindCode::from(SectionKind::Metadata)),
            },
        })
    );
    Ok(())
}

pub(super) fn truncation_laws() -> Result<(), CorpusError> {
    for length in 0..ONE_SECTION_CORPUS.len() {
        let truncated = ONE_SECTION_CORPUS
            .get(..length)
            .ok_or(CorpusError::MissingBytes {
                offset: 0,
                needed: length,
            })?;
        let expected = if length < FRAME_HEADER_BYTES {
            Err(ValidateError::Truncated {
                offset: 0,
                needed: FRAME_HEADER_BYTES,
                available: length,
            })
        } else {
            Err(ValidateError::TotalLengthMismatch {
                declared: read_u32(&ONE_SECTION_CORPUS, HEADER_TOTAL_LENGTH_OFFSET)?,
                actual: length,
            })
        };
        assert_eq!(ValidatedFrame::validate(truncated), expected);
    }
    Ok(())
}

pub(super) fn correlated_boundary_laws() -> Result<(), CorpusError> {
    total_length_boundaries()?;
    section_count_boundaries()?;
    row_count_boundaries()
}

fn total_length_boundaries() -> Result<(), CorpusError> {
    let mut at_limit = ONE_SECTION_CORPUS;
    write_u32(&mut at_limit, HEADER_TOTAL_LENGTH_OFFSET, MAX_FRAME_BYTES)?;
    assert_eq!(
        ValidatedFrame::validate(&at_limit),
        Err(ValidateError::TotalLengthMismatch {
            declared: MAX_FRAME_BYTES,
            actual: at_limit.len(),
        })
    );

    let mut above_limit = ONE_SECTION_CORPUS;
    write_u32(
        &mut above_limit,
        HEADER_TOTAL_LENGTH_OFFSET,
        MAX_FRAME_BYTES + 1,
    )?;
    assert_eq!(
        ValidatedFrame::validate(&above_limit),
        Err(ValidateError::LimitExceeded {
            kind: LimitKind::FrameBytes,
            observed: MAX_FRAME_BYTES + 1,
            maximum: MAX_FRAME_BYTES,
        })
    );
    Ok(())
}

fn section_count_boundaries() -> Result<(), CorpusError> {
    let mut at_limit = ONE_SECTION_CORPUS;
    write_u16(&mut at_limit, HEADER_SECTION_COUNT_OFFSET, MAX_SECTIONS)?;
    assert_eq!(
        ValidatedFrame::validate(&at_limit),
        Err(ValidateError::HeaderLengthMismatch {
            declared: read_u16(&ONE_SECTION_CORPUS, HEADER_LENGTH_OFFSET)?,
            expected: FRAME_HEADER_BYTES + usize::from(MAX_SECTIONS) * SECTION_DESCRIPTOR_BYTES,
        })
    );

    let mut above_limit = ONE_SECTION_CORPUS;
    write_u16(
        &mut above_limit,
        HEADER_SECTION_COUNT_OFFSET,
        MAX_SECTIONS + 1,
    )?;
    assert_eq!(
        ValidatedFrame::validate(&above_limit),
        Err(ValidateError::LimitExceeded {
            kind: LimitKind::Sections,
            observed: u32::from(MAX_SECTIONS + 1),
            maximum: u32::from(MAX_SECTIONS),
        })
    );
    Ok(())
}

fn row_count_boundaries() -> Result<(), CorpusError> {
    let offset = FRAME_HEADER_BYTES + DESCRIPTOR_ROWS_OFFSET;
    let mut at_limit = ONE_SECTION_CORPUS;
    write_u32(&mut at_limit, offset, MAX_ROWS)?;
    assert_eq!(
        ValidatedFrame::validate(&at_limit).map(|frame| {
            frame
                .sections()
                .next()
                .transpose()
                .map(|section| section.map(|section| *section.rows))
        }),
        Ok(Ok(Some(MAX_ROWS)))
    );

    let mut above_limit = ONE_SECTION_CORPUS;
    write_u32(&mut above_limit, offset, MAX_ROWS + 1)?;
    assert_eq!(
        ValidatedFrame::validate(&above_limit),
        Err(ValidateError::LimitExceeded {
            kind: LimitKind::Rows,
            observed: MAX_ROWS + 1,
            maximum: MAX_ROWS,
        })
    );
    Ok(())
}
