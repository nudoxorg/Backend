//! Defines validate frame behavior for `heart-view`, whose purpose is to validate and borrow canonical framed data without allocating.
//! This module owns the validate frame invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Compact borrowed validation witness and audited section reborrows.

use core::{iter::FusedIterator, ops::Deref};

use heart_schema::{
    FRAME_HEADER_BYTES, RowCount, SECTION_DESCRIPTOR_BYTES, SectionDescriptor, SectionKind,
};
use zerocopy::FromBytes;

use super::directory::{KnownDirectory, KnownKindCursor};
use super::{SectionReadError, ValidateError};

/// Borrowed bytes that have passed complete frame structural validation.
///
/// The canonical bytes remain the only descriptor and body authority. This
/// 24-byte witness retains their borrow plus the validated known descriptor
/// indices and resolved kinds; optional extension descriptors need no second
/// scan or compatibility dispatch during iteration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedFrame<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) known: KnownDirectory,
}

impl<'a> ValidatedFrame<'a> {
    /// Validates a frame against the fixed protocol maxima.
    ///
    /// # Errors
    ///
    /// Returns [`ValidateError`] for every malformed, unsupported, noncanonical, or over-limit
    /// structural condition; section payload bytes are not scanned.
    pub fn validate(bytes: &'a [u8]) -> Result<Self, ValidateError> {
        Self::validate_with_limits(bytes, heart_schema::DecodeLimits::default())
    }

    /// Validates a frame using policy that may lower, but never raise, wire maxima.
    ///
    /// # Errors
    ///
    /// Returns [`ValidateError`] for every malformed, unsupported, noncanonical, or policy-limit
    /// condition; section payload bytes are not scanned.
    pub fn validate_with_limits(
        bytes: &'a [u8],
        limits: heart_schema::DecodeLimits,
    ) -> Result<Self, ValidateError> {
        super::validate(bytes, limits)
    }

    /// Iterates known sections lent from the canonical frame bytes.
    ///
    /// The validation pass retained exact known descriptor indices and kinds,
    /// so iteration performs one descriptor/body reborrow per yielded section.
    #[must_use]
    pub const fn sections(&self) -> Sections<'a> {
        Sections {
            bytes: self.bytes,
            known: self.known,
            cursor: KnownKindCursor::Metadata,
            remaining: self.known.membership_count(),
        }
    }
}

impl Deref for ValidatedFrame<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.bytes
    }
}

/// One known section body borrowed from a validated frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Section<'a> {
    /// Finite validated section kind.
    pub kind: SectionKind,
    /// Bounded validated logical row count.
    pub rows: RowCount,
    /// Original body bytes, borrowed without a copy.
    pub bytes: &'a [u8],
}

/// Exact-size iterator over the known descriptors retained by validation.
pub struct Sections<'a> {
    bytes: &'a [u8],
    known: KnownDirectory,
    cursor: KnownKindCursor,
    remaining: u8,
}

impl<'a> Iterator for Sections<'a> {
    type Item = Result<Section<'a>, SectionReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        let (index, kind) = self.known.next_from(&mut self.cursor)?;
        self.remaining -= 1;
        Some(validated_section(self.bytes, index, kind))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::from(self.remaining);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Sections<'_> {
    fn len(&self) -> usize {
        usize::from(self.remaining)
    }
}

impl FusedIterator for Sections<'_> {}

/// Reborrows one selected descriptor/body after `ValidatedFrame::validate`
/// established its exact directory/body ranges, kind, and bounded row scalar.
///
/// The table gives the already-resolved known kind. Reborrowing repeats only
/// the constant-time safe bounds and typed-scalar conversions, containing any
/// implementation drift as a typed read error.
fn validated_section(
    bytes: &[u8],
    index: u16,
    kind: SectionKind,
) -> Result<Section<'_>, SectionReadError> {
    let descriptor_offset = FRAME_HEADER_BYTES + usize::from(index) * SECTION_DESCRIPTOR_BYTES;
    let descriptor_bytes = bytes
        .get(descriptor_offset..descriptor_offset + SECTION_DESCRIPTOR_BYTES)
        .ok_or(SectionReadError::DescriptorWindow {
            index,
            available: bytes.len().saturating_sub(descriptor_offset),
        })?;
    let descriptor = SectionDescriptor::ref_from_bytes(descriptor_bytes).map_err(|source| {
        SectionReadError::DescriptorWindow {
            index,
            available: source.into_src().len(),
        }
    })?;
    let raw_offset = descriptor.body_offset.get();
    let offset = usize::try_from(raw_offset).map_err(|source| SectionReadError::BodyOffset {
        index,
        raw: raw_offset,
        source,
    })?;
    let raw_length = descriptor.body_length.get();
    let length = usize::try_from(raw_length).map_err(|source| SectionReadError::BodyLength {
        index,
        raw: raw_length,
        source,
    })?;
    let end = offset
        .checked_add(length)
        .ok_or(SectionReadError::BodyWindow {
            index,
            offset,
            length,
            available: bytes.len(),
        })?;
    let body = bytes.get(offset..end).ok_or(SectionReadError::BodyWindow {
        index,
        offset,
        length,
        available: bytes.len(),
    })?;
    let rows = RowCount::try_from(descriptor.rows.get())
        .map_err(|source| SectionReadError::Rows { index, source })?;
    Ok(Section {
        kind,
        rows,
        bytes: body,
    })
}

#[cfg(test)]
mod read_tests {
    use heart_schema::{FRAME_HEADER_BYTES, LimitError, LimitKind, MAX_ROWS, SectionKind};

    use super::{SectionReadError, validated_section};

    #[test]
    fn post_validation_range_and_scalar_drift_are_contained() {
        let short = [0_u8; FRAME_HEADER_BYTES + 15];
        assert_eq!(
            validated_section(&short, 0, SectionKind::Data),
            Err(SectionReadError::DescriptorWindow {
                index: 0,
                available: 15,
            })
        );

        let mut outside_body = [0_u8; FRAME_HEADER_BYTES + 16];
        outside_body[28..32].copy_from_slice(&40_u32.to_le_bytes());
        outside_body[32..36].copy_from_slice(&1_u32.to_le_bytes());
        assert_eq!(
            validated_section(&outside_body, 0, SectionKind::Data),
            Err(SectionReadError::BodyWindow {
                index: 0,
                offset: 40,
                length: 1,
                available: 40,
            })
        );

        let mut invalid_rows = [0_u8; FRAME_HEADER_BYTES + 16];
        invalid_rows[28..32].copy_from_slice(&40_u32.to_le_bytes());
        invalid_rows[36..40].copy_from_slice(&(MAX_ROWS + 1).to_le_bytes());
        assert_eq!(
            validated_section(&invalid_rows, 0, SectionKind::Data),
            Err(SectionReadError::Rows {
                index: 0,
                source: LimitError {
                    kind: LimitKind::Rows,
                    requested: (MAX_ROWS + 1).into(),
                    maximum: MAX_ROWS.into(),
                },
            })
        );
    }
}
