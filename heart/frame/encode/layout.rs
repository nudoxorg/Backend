//! Defines encode layout behavior for `heart-frame`, whose purpose is to encode canonical bounded frames into caller-owned storage.
//! This module owns the encode layout invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Compact canonical layout facts with one caller-shaped arena accumulation.

use heart_schema::{
    FRAME_HEADER_BYTES, MAX_ARENA_BYTES, MAX_FRAME_BYTES, SECTION_ALIGNMENT,
    SECTION_DESCRIPTOR_BYTES, SectionDescriptor, SectionFlags, SectionKind, SectionKindCode,
};
use zerocopy::byteorder::{U16, U32};

use super::{EncodeError, EncodedFrameLength, NativeByteCount, SectionInput};

pub(super) const MAX_SECTION_SLOTS: usize = <SectionKind as strum::EnumCount>::COUNT;
#[allow(
    clippy::as_conversions,
    reason = "compile-time u16 grammar geometry proof"
)]
const _: () = assert!(MAX_SECTION_SLOTS <= u16::MAX as usize);
#[allow(
    clippy::as_conversions,
    reason = "compile-time u16 grammar geometry proof"
)]
const _: () = assert!(FRAME_HEADER_BYTES <= u16::MAX as usize);
#[allow(
    clippy::as_conversions,
    reason = "compile-time u16 grammar geometry proof"
)]
const _: () = assert!(SECTION_DESCRIPTOR_BYTES <= u16::MAX as usize);
const _: () = assert!(SECTION_ALIGNMENT.is_power_of_two());

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "fixed u32 limits fit native slice coordinates on the supported 32/64-bit targets"
)]
const MAX_ARENA_BYTES_USIZE: usize = MAX_ARENA_BYTES as usize;

/// The largest geometry attainable after the caller-shaped arena bound.
const MAX_ALIGNMENT_GAPS: usize = MAX_SECTION_SLOTS * (SECTION_ALIGNMENT - 1);
const MAX_LAYOUT_BYTES: usize = FRAME_HEADER_BYTES
    + MAX_SECTION_SLOTS * SECTION_DESCRIPTOR_BYTES
    + MAX_ARENA_BYTES_USIZE
    + MAX_ALIGNMENT_GAPS;
#[allow(
    clippy::as_conversions,
    reason = "compile-time u32 grammar geometry proof"
)]
const _: () = assert!(MAX_LAYOUT_BYTES <= MAX_FRAME_BYTES as usize);
#[allow(
    clippy::as_conversions,
    reason = "compile-time u32 grammar geometry proof"
)]
const _: () = assert!(MAX_LAYOUT_BYTES <= u32::MAX as usize);

/// Compact wire-domain layout facts retained after preflight.
///
/// There are no retained per-section spans: all descriptor and body locations
/// are pure functions of this compact header/count fact and the immutable input
/// slices. Recomputing them performs no caller-shaped arithmetic because the
/// one preflight arena accumulation has already bounded every body length.
#[derive(Clone, Copy)]
pub(super) struct Layout {
    pub(super) header_bytes: u16,
    pub(super) section_count: u16,
    pub(super) total_bytes: EncodedFrameLength,
}

pub(super) fn preflight(sections: &[SectionInput<'_>]) -> Result<Layout, EncodeError> {
    let section_count = canonical_section_count(sections.len())?;
    let header_bytes = header_bytes(section_count);
    let mut previous = None;
    let mut arena_bytes = 0_usize;
    let mut cursor = usize::from(header_bytes);
    for section in sections {
        validate_order(previous, section.kind)?;
        previous = Some(section.kind);
        arena_bytes = reserve_arena(arena_bytes, section.bytes.len())?;
        cursor = body_end(body_offset(cursor), section.bytes.len());
    }
    Ok(Layout {
        header_bytes,
        section_count,
        total_bytes: EncodedFrameLength::from_layout(cursor),
    })
}

fn canonical_section_count(inputs: usize) -> Result<u16, EncodeError> {
    if inputs > MAX_SECTION_SLOTS {
        return Err(EncodeError::TooManySections {
            count: inputs.into(),
            maximum: canonical_section_limit().into(),
        });
    }
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "the closed EnumCount bound and compile-time u16 proof above establish this conversion"
    )]
    Ok(inputs as u16)
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "derived exclusively from the closed EnumCount and compile-time u16 proof"
)]
const fn canonical_section_limit() -> u16 {
    MAX_SECTION_SLOTS as u16
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "both schema widths and the bounded count are compile-time proven to fit u16"
)]
const fn header_bytes(section_count: u16) -> u16 {
    (FRAME_HEADER_BYTES + section_count as usize * SECTION_DESCRIPTOR_BYTES) as u16
}

fn validate_order(previous: Option<SectionKind>, next: SectionKind) -> Result<(), EncodeError> {
    if let Some(previous) = previous {
        if previous == next {
            return Err(EncodeError::DuplicateKind { kind: next });
        }
        if previous > next {
            return Err(EncodeError::OutOfOrderKind { previous, next });
        }
    }
    Ok(())
}

/// The sole caller-shaped addition: exact overflow and the arena policy share
/// the same rejection because either proves the requested arena cannot fit.
pub(super) fn reserve_arena(current: usize, added: usize) -> Result<usize, EncodeError> {
    let Some(next) = current.checked_add(added) else {
        return Err(EncodeError::ArenaTooLarge {
            current: current.into(),
            added: added.into(),
            maximum: MAX_ARENA_BYTES.into(),
        });
    };
    if next > MAX_ARENA_BYTES_USIZE {
        return Err(EncodeError::ArenaTooLarge {
            current: NativeByteCount::from(current),
            added: NativeByteCount::from(added),
            maximum: MAX_ARENA_BYTES.into(),
        });
    }
    Ok(next)
}

/// Alignment arithmetic is bounded by `MAX_LAYOUT_BYTES` before this function.
pub(super) const fn body_offset(previous_end: usize) -> usize {
    let remainder = previous_end % SECTION_ALIGNMENT;
    if remainder == 0 {
        previous_end
    } else {
        previous_end + SECTION_ALIGNMENT - remainder
    }
}

/// Body-end arithmetic is bounded by `MAX_LAYOUT_BYTES` before this function.
pub(super) const fn body_end(offset: usize, body_bytes: usize) -> usize {
    offset + body_bytes
}

/// Descriptor arithmetic is bounded by the closed `MAX_SECTION_SLOTS` grammar.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "preflight's grammar and arena bounds prove each emitted descriptor coordinate fits u32"
)]
pub(super) fn descriptor(
    index: usize,
    section: &SectionInput<'_>,
    body_offset: usize,
) -> (usize, SectionDescriptor) {
    let descriptor_offset = FRAME_HEADER_BYTES + index * SECTION_DESCRIPTOR_BYTES;
    (
        descriptor_offset,
        SectionDescriptor {
            kind: SectionKindCode::from(section.kind),
            flags: SectionFlags::from(0),
            reserved: U16::new(0),
            body_offset: U32::new(body_offset as u32),
            body_length: U32::new(section.bytes.len() as u32),
            rows: U32::new(*section.rows),
        },
    )
}
