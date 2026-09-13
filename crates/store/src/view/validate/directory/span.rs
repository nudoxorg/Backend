//! Defines validate directory span behavior for `backend_store::view`, whose purpose is to validate and borrow canonical framed data without allocating.
//! This module owns the validate directory span invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Geometry and canonical-padding checks for one declared section body.

use backend_version::schema::SECTION_ALIGNMENT;

use super::super::header::Header;
use super::super::{DescriptorError, ValidateError, align_up, descriptor_error, take};

pub(super) fn validate_body_span(
    bytes: &[u8],
    header: Header<'_>,
    index: u16,
    offset: usize,
    length: usize,
    previous_end: usize,
) -> Result<usize, ValidateError> {
    validate_body_geometry(header, index, offset, length, previous_end)?;
    validate_padding(bytes, index, previous_end, offset)?;
    Ok(offset + length)
}

const fn validate_body_geometry(
    header: Header<'_>,
    index: u16,
    offset: usize,
    length: usize,
    previous_end: usize,
) -> Result<(), ValidateError> {
    if offset < header.directory_end {
        return Err(descriptor_error(
            index,
            DescriptorError::OffsetBeforeBodies {
                offset,
                minimum: header.directory_end,
            },
        ));
    }
    if offset < previous_end {
        return Err(descriptor_error(
            index,
            DescriptorError::OverlappingBody {
                offset,
                previous_end,
            },
        ));
    }
    if !offset.is_multiple_of(SECTION_ALIGNMENT) {
        return Err(descriptor_error(
            index,
            DescriptorError::MisalignedOffset {
                offset,
                alignment: SECTION_ALIGNMENT,
            },
        ));
    }
    let expected = align_up(previous_end);
    if offset != expected {
        return Err(descriptor_error(
            index,
            DescriptorError::NoncanonicalOffset {
                expected,
                actual: offset,
            },
        ));
    }
    if offset > header.bytes || length > header.bytes - offset {
        return Err(descriptor_error(
            index,
            DescriptorError::BodyOutsideFrame {
                offset,
                length,
                total: header.bytes,
            },
        ));
    }
    Ok(())
}

fn validate_padding(
    bytes: &[u8],
    index: u16,
    previous_end: usize,
    offset: usize,
) -> Result<(), ValidateError> {
    for (padding_index, byte) in take(bytes, previous_end, offset - previous_end)?
        .iter()
        .enumerate()
    {
        if *byte != 0 {
            let bad_offset = previous_end + padding_index;
            return Err(descriptor_error(
                index,
                DescriptorError::NonzeroPadding { offset: bad_offset },
            ));
        }
    }
    Ok(())
}
