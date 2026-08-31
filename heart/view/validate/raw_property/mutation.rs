//! Defines validate raw-property mutation behavior for `heart-view`, whose purpose is to validate and borrow canonical framed data without allocating.
//! This module owns the validate raw-property mutation invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use heart_schema::{
    DESCRIPTOR_BODY_LENGTH_OFFSET, DESCRIPTOR_BODY_OFFSET, DESCRIPTOR_FLAGS_OFFSET,
    DESCRIPTOR_KIND_OFFSET, DESCRIPTOR_ROWS_OFFSET, FORMAT_MAJOR, FORMAT_MINOR, FRAME_HEADER_BYTES,
    HEADER_LENGTH_OFFSET, HEADER_MAGIC_OFFSET, HEADER_MAJOR_OFFSET, HEADER_MINOR_OFFSET,
    HEADER_RESERVED_A_OFFSET, HEADER_RESERVED_B_OFFSET, HEADER_SCHEMA_OFFSET,
    HEADER_SECTION_COUNT_OFFSET, HEADER_TOTAL_LENGTH_OFFSET, LimitKind, MAX_FRAME_BYTES, MAX_ROWS,
    MAX_SECTIONS, SECTION_ALIGNMENT, SECTION_DESCRIPTOR_BYTES, SectionCompatibilityError,
    SectionFlags, SectionKind, SectionKindCode,
};

use super::super::{DescriptorError, ValidateError, ValidatedFrame};
use super::{
    CorpusError,
    fields::{
        DESCRIPTOR_MUTATION_FIELDS, DescriptorField, ExpectedValidation, HEADER_MUTATION_FIELDS,
        HeaderField,
    },
    fixture::{
        ONE_SECTION_BODY, ONE_SECTION_CORPUS, input_byte, raw_usize, read_byte, read_u16, read_u32,
        replace_byte,
    },
};

pub(super) fn structural_mutation_laws(input: &[u8]) -> Result<(), CorpusError> {
    mutation_laws(|offset| input_byte(input, offset))
}

pub(super) fn exhaustive_single_byte_mutation_laws(raw: u8) -> Result<(), CorpusError> {
    mutation_laws(|_| raw)
}

fn mutation_laws<ByteAt>(byte_at: ByteAt) -> Result<(), CorpusError>
where
    ByteAt: Fn(usize) -> u8,
{
    header_mutation_laws(&byte_at)?;
    descriptor_mutation_laws(&byte_at)?;
    payload_mutation_law(&byte_at)?;
    Ok(())
}

fn header_mutation_laws<ByteAt>(byte_at: &ByteAt) -> Result<(), CorpusError>
where
    ByteAt: Fn(usize) -> u8,
{
    for field in HEADER_MUTATION_FIELDS {
        for offset in field.offset..field.offset + field.bytes {
            let mut frame = ONE_SECTION_CORPUS;
            replace_byte(&mut frame, offset, byte_at(offset))?;
            assert_expected_error(&frame, expected_header_error(&frame, field.kind)?);
        }
    }
    Ok(())
}

fn expected_header_error(frame: &[u8], field: HeaderField) -> Result<ValidateError, CorpusError> {
    let error = match field {
        HeaderField::Magic => ValidateError::BadMagic {
            observed: read_u32(frame, HEADER_MAGIC_OFFSET)?.to_le_bytes(),
        },
        HeaderField::Major => ValidateError::UnsupportedVersion {
            major: read_byte(frame, HEADER_MAJOR_OFFSET)?,
            minor: u8::from(FORMAT_MINOR),
        },
        HeaderField::Minor => ValidateError::UnsupportedVersion {
            major: u8::from(FORMAT_MAJOR),
            minor: read_byte(frame, HEADER_MINOR_OFFSET)?,
        },
        HeaderField::HeaderLength => ValidateError::HeaderLengthMismatch {
            declared: read_u16(frame, HEADER_LENGTH_OFFSET)?,
            expected: FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES,
        },
        HeaderField::TotalLength => total_length_error(frame)?,
        HeaderField::SectionCount => section_count_error(frame)?,
        HeaderField::ReservedA => ValidateError::NonzeroHeaderReserved {
            offset: HEADER_RESERVED_A_OFFSET,
        },
        HeaderField::Schema => ValidateError::UnsupportedSchema {
            wire: read_u32(frame, HEADER_SCHEMA_OFFSET)?,
        },
        HeaderField::ReservedB => ValidateError::NonzeroHeaderReserved {
            offset: HEADER_RESERVED_B_OFFSET,
        },
    };
    Ok(error)
}

fn total_length_error(frame: &[u8]) -> Result<ValidateError, CorpusError> {
    let declared = read_u32(frame, HEADER_TOTAL_LENGTH_OFFSET)?;
    Ok(if declared > MAX_FRAME_BYTES {
        ValidateError::LimitExceeded {
            kind: LimitKind::FrameBytes,
            observed: declared,
            maximum: MAX_FRAME_BYTES,
        }
    } else {
        ValidateError::TotalLengthMismatch {
            declared,
            actual: frame.len(),
        }
    })
}

fn section_count_error(frame: &[u8]) -> Result<ValidateError, CorpusError> {
    let count = read_u16(frame, HEADER_SECTION_COUNT_OFFSET)?;
    Ok(if count > MAX_SECTIONS {
        ValidateError::LimitExceeded {
            kind: LimitKind::Sections,
            observed: u32::from(count),
            maximum: u32::from(MAX_SECTIONS),
        }
    } else {
        ValidateError::HeaderLengthMismatch {
            declared: read_u16(frame, HEADER_LENGTH_OFFSET)?,
            expected: FRAME_HEADER_BYTES + usize::from(count) * SECTION_DESCRIPTOR_BYTES,
        }
    })
}

fn descriptor_mutation_laws<ByteAt>(byte_at: &ByteAt) -> Result<(), CorpusError>
where
    ByteAt: Fn(usize) -> u8,
{
    for field in DESCRIPTOR_MUTATION_FIELDS {
        for field_offset in field.offset..field.offset + field.bytes {
            let mut frame = ONE_SECTION_CORPUS;
            replace_byte(
                &mut frame,
                FRAME_HEADER_BYTES + field_offset,
                byte_at(FRAME_HEADER_BYTES + field_offset),
            )?;
            assert_expected_validation(&frame, expected_descriptor_validation(&frame, field.kind)?);
        }
    }
    Ok(())
}

fn expected_descriptor_validation(
    frame: &[u8],
    field: DescriptorField,
) -> Result<ExpectedValidation<'_>, CorpusError> {
    let expected = match field {
        DescriptorField::Kind => descriptor_kind_expectation(frame)?,
        DescriptorField::Flags => descriptor_flags_expectation(frame)?,
        DescriptorField::Reserved => ExpectedValidation::Error(ValidateError::Descriptor {
            index: 0,
            error: DescriptorError::NonzeroReserved,
        }),
        DescriptorField::BodyOffset => ExpectedValidation::Error(ValidateError::Descriptor {
            index: 0,
            error: descriptor_offset_error(frame)?,
        }),
        DescriptorField::BodyLength => ExpectedValidation::Error(descriptor_length_error(frame)?),
        DescriptorField::Rows => descriptor_rows_expectation(frame)?,
    };
    Ok(expected)
}

fn descriptor_kind_expectation(frame: &[u8]) -> Result<ExpectedValidation<'_>, CorpusError> {
    let kind = read_byte(frame, FRAME_HEADER_BYTES + DESCRIPTOR_KIND_OFFSET)?;
    Ok(match SectionKind::try_from(SectionKindCode::from(kind)) {
        Ok(kind) => ExpectedValidation::KnownSection {
            kind,
            rows: 1,
            body: ONE_SECTION_BODY,
        },
        Err(_) => ExpectedValidation::Error(ValidateError::Descriptor {
            index: 0,
            error: DescriptorError::IncompatibleKind {
                kind,
                flags: 0,
                source: SectionCompatibilityError::UnknownRequired {
                    kind: SectionKindCode::from(kind),
                },
            },
        }),
    })
}

fn descriptor_flags_expectation(frame: &[u8]) -> Result<ExpectedValidation<'_>, CorpusError> {
    let flags = read_byte(frame, FRAME_HEADER_BYTES + DESCRIPTOR_FLAGS_OFFSET)?;
    Ok(ExpectedValidation::Error(ValidateError::Descriptor {
        index: 0,
        error: DescriptorError::IncompatibleKind {
            kind: u8::from(SectionKindCode::from(SectionKind::Data)),
            flags,
            source: SectionCompatibilityError::KnownFlags {
                kind: SectionKind::Data,
                flags: SectionFlags::from(flags),
            },
        },
    }))
}

fn descriptor_offset_error(frame: &[u8]) -> Result<DescriptorError, CorpusError> {
    let offset = raw_usize(read_u32(
        frame,
        FRAME_HEADER_BYTES + DESCRIPTOR_BODY_OFFSET,
    )?);
    Ok(if offset < FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES {
        DescriptorError::OffsetBeforeBodies {
            offset,
            minimum: FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES,
        }
    } else if !offset.is_multiple_of(SECTION_ALIGNMENT) {
        DescriptorError::MisalignedOffset {
            offset,
            alignment: SECTION_ALIGNMENT,
        }
    } else {
        DescriptorError::NoncanonicalOffset {
            expected: FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES,
            actual: offset,
        }
    })
}

fn descriptor_length_error(frame: &[u8]) -> Result<ValidateError, CorpusError> {
    let length = raw_usize(read_u32(
        frame,
        FRAME_HEADER_BYTES + DESCRIPTOR_BODY_LENGTH_OFFSET,
    )?);
    Ok(if length == 0 {
        ValidateError::TrailingBytes {
            expected_end: FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES,
            declared_total: ONE_SECTION_CORPUS.len(),
        }
    } else {
        ValidateError::Descriptor {
            index: 0,
            error: DescriptorError::BodyOutsideFrame {
                offset: FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES,
                length,
                total: ONE_SECTION_CORPUS.len(),
            },
        }
    })
}

fn descriptor_rows_expectation(frame: &[u8]) -> Result<ExpectedValidation<'_>, CorpusError> {
    let rows = read_u32(frame, FRAME_HEADER_BYTES + DESCRIPTOR_ROWS_OFFSET)?;
    Ok(if rows > MAX_ROWS {
        ExpectedValidation::Error(ValidateError::LimitExceeded {
            kind: LimitKind::Rows,
            observed: rows,
            maximum: MAX_ROWS,
        })
    } else {
        ExpectedValidation::KnownSection {
            kind: SectionKind::Data,
            rows,
            body: ONE_SECTION_BODY,
        }
    })
}

fn assert_expected_error(frame: &[u8], expected: ValidateError) {
    assert_eq!(ValidatedFrame::validate(frame), Err(expected));
}

fn assert_expected_validation(frame: &[u8], expected: ExpectedValidation<'_>) {
    match expected {
        ExpectedValidation::Error(error) => assert_expected_error(frame, error),
        ExpectedValidation::KnownSection { kind, rows, body } => {
            assert_valid_known_section(frame, kind, rows, body);
        }
    }
}

fn payload_mutation_law<ByteAt>(byte_at: &ByteAt) -> Result<(), CorpusError>
where
    ByteAt: Fn(usize) -> u8,
{
    let mut frame = ONE_SECTION_CORPUS;
    let offset = FRAME_HEADER_BYTES + SECTION_DESCRIPTOR_BYTES;
    replace_byte(&mut frame, offset, byte_at(offset))?;
    let expected = [read_byte(&frame, offset)?];
    assert_valid_known_section(&frame, SectionKind::Data, 1, &expected);
    Ok(())
}

fn assert_valid_known_section(frame: &[u8], kind: SectionKind, rows: u32, body: &[u8]) {
    let observed = ValidatedFrame::validate(frame).map(|validated| {
        let mut sections = validated.sections();
        let first = sections
            .next()
            .transpose()
            .map(|section| section.map(|section| (section.kind, *section.rows, section.bytes)));
        (first, sections.next())
    });
    assert_eq!(observed, Ok((Ok(Some((kind, rows, body))), None)));
}
