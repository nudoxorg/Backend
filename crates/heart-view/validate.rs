//! Defines validate behavior for `heart-view`, whose purpose is to validate and borrow canonical framed data without allocating.
//! This module owns the validate invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Validation coordinator and small shared checked-slice helpers.

use heart_schema::{DecodeLimits, SECTION_ALIGNMENT};
mod directory;
mod error;
mod frame;
mod header;
#[cfg(test)]
mod raw_property;
#[cfg(test)]
mod tests;

use directory::validate_directory;
pub use error::{DescriptorError, SectionReadError, ValidateError};
pub use frame::{Section, Sections, ValidatedFrame};
use header::parse_header;

fn validate(bytes: &[u8], limits: DecodeLimits) -> Result<ValidatedFrame<'_>, ValidateError> {
    let header = parse_header(bytes, limits)?;
    let state = validate_directory(bytes, header, limits)?;
    if state.previous_end != header.bytes {
        return Err(ValidateError::TrailingBytes {
            expected_end: state.previous_end,
            declared_total: header.bytes,
        });
    }
    Ok(ValidatedFrame {
        bytes,
        known: state.known,
    })
}

pub(crate) const fn descriptor_error(index: u16, error: DescriptorError) -> ValidateError {
    ValidateError::Descriptor { index, error }
}

pub(crate) const fn align_up(value: usize) -> usize {
    let remainder = value % SECTION_ALIGNMENT;
    let padding = if remainder == 0 {
        0
    } else {
        SECTION_ALIGNMENT - remainder
    };
    value + padding
}

pub(crate) fn take(bytes: &[u8], offset: usize, needed: usize) -> Result<&[u8], ValidateError> {
    let end = offset + needed;
    bytes.get(offset..end).ok_or(ValidateError::Truncated {
        offset,
        needed,
        available: if offset > bytes.len() {
            0
        } else {
            bytes.len() - offset
        },
    })
}
