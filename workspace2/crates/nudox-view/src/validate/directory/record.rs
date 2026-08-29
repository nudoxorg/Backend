//! Per-descriptor semantic, range, and arena validation.

use nudox_schema::{
    DecodeLimits, LimitKind, RowCount, SectionDescriptor, SectionDisposition, SectionKind,
    section_compatibility,
};

use super::super::header::Header;
use super::super::{DescriptorError, ValidateError, descriptor_error};
use super::span::validate_body_span;
use super::{DirectoryState, enforce_limit};

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "validated raw fields are bounded below one mebibyte on supported 32/64-bit targets"
)]
pub(super) fn validate_descriptor<'a>(
    bytes: &'a [u8],
    header: Header<'a>,
    limits: DecodeLimits,
    index: usize,
    descriptor: &SectionDescriptor,
    state: &mut DirectoryState,
) -> Result<(), ValidateError> {
    let index = index as u16;
    if descriptor.reserved.get() != 0 {
        return Err(descriptor_error(index, DescriptorError::NonzeroReserved));
    }
    let known_kind = record_kind(index, descriptor, state)?;
    let rows_raw = descriptor.rows.get();
    RowCount::try_from(rows_raw).map_err(|error| ValidateError::LimitExceeded {
        kind: error.kind,
        observed: *error.requested,
        maximum: *error.maximum,
    })?;
    enforce_limit(LimitKind::Rows, rows_raw, *limits.rows)?;
    let offset = descriptor.body_offset.get() as usize;
    let length = descriptor.body_length.get() as usize;
    let end = validate_body_span(bytes, header, index, offset, length, state.previous_end)?;
    record_arena(length, limits, state)?;
    state.previous_end = end;
    if let Some(kind) = known_kind {
        state.known.push(index, kind);
    }
    Ok(())
}

fn record_kind(
    index: u16,
    descriptor: &SectionDescriptor,
    state: &mut DirectoryState,
) -> Result<Option<SectionKind>, ValidateError> {
    let known = match section_compatibility(descriptor.kind, descriptor.flags) {
        Ok(SectionDisposition::Supported(kind)) => Some(kind),
        Ok(SectionDisposition::SkippableOptional) => None,
        Err(source) => {
            return Err(descriptor_error(
                index,
                DescriptorError::IncompatibleKind {
                    kind: u8::from(descriptor.kind),
                    flags: u8::from(descriptor.flags),
                    source,
                },
            ));
        }
    };
    if let Some(previous) = state.previous_kind {
        if previous == descriptor.kind {
            return Err(descriptor_error(
                index,
                DescriptorError::DuplicateKind {
                    kind: u8::from(descriptor.kind),
                },
            ));
        }
        if previous > descriptor.kind {
            return Err(descriptor_error(
                index,
                DescriptorError::OutOfOrderKind {
                    previous: u8::from(previous),
                    next: u8::from(descriptor.kind),
                },
            ));
        }
    }
    state.previous_kind = Some(descriptor.kind);
    Ok(known)
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "body length was proven within the bounded frame before aggregation"
)]
fn record_arena(
    length: usize,
    limits: DecodeLimits,
    state: &mut DirectoryState,
) -> Result<(), ValidateError> {
    let length = length as u32;
    state.arena_bytes += length;
    enforce_limit(
        LimitKind::ArenaBytes,
        state.arena_bytes,
        *limits.arena_bytes,
    )
}
