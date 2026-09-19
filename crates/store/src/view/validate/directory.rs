//! Defines validate directory behavior for `backend_store::view`, whose purpose is to validate and borrow canonical framed data without allocating.
//! This module owns the validate directory invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Directory coordinator retaining only running structural validation facts.

mod record;
mod span;

use backend_version::schema::{DecodeLimits, LimitKind, SectionKind};

use super::ValidateError;
use super::header::Header;
use record::validate_descriptor;

pub(crate) struct DirectoryState {
    pub(crate) previous_end: usize,
    pub(super) previous_kind: Option<backend_version::schema::SectionKindCode>,
    pub(super) arena_bytes: u32,
    pub(crate) known: KnownDirectory,
}

/// Compact proof of the known descriptors retained by validation. Each typed
/// slot is either a descriptor ordinal or the private absence sentinel. Array
/// position is therefore the resolved kind, without dummy records, a
/// correlated count, or an iteration-time compatibility dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KnownDirectory {
    metadata: u16,
    rows: u16,
    data: u16,
}

const ABSENT_DESCRIPTOR: u16 = u16::MAX;

/// Exhaustive iterator progress through the closed known-kind vocabulary.
/// It is a one-byte cursor, not a wire value or a retained descriptor kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KnownKindCursor {
    Metadata,
    Rows,
    Data,
    Exhausted,
}

impl KnownDirectory {
    pub(crate) const EMPTY: Self = Self {
        metadata: ABSENT_DESCRIPTOR,
        rows: ABSENT_DESCRIPTOR,
        data: ABSENT_DESCRIPTOR,
    };

    /// Stores one fully validated unique known descriptor coordinate.
    pub(crate) const fn push(&mut self, index: u16, kind: SectionKind) {
        match kind {
            SectionKind::Metadata => self.metadata = index,
            SectionKind::Rows => self.rows = index,
            SectionKind::Data => self.data = index,
        }
    }

    /// Computes memberships once for a newly-created iterator; no correlated
    /// count is retained in this compact proof table.
    pub(crate) const fn membership_count(self) -> u8 {
        (if self.metadata == ABSENT_DESCRIPTOR {
            0
        } else {
            1
        }) + (if self.rows == ABSENT_DESCRIPTOR { 0 } else { 1 })
            + (if self.data == ABSENT_DESCRIPTOR { 0 } else { 1 })
    }

    pub(crate) const fn next_from(
        self,
        cursor: &mut KnownKindCursor,
    ) -> Option<(u16, SectionKind)> {
        loop {
            let (index, kind, next) = match *cursor {
                KnownKindCursor::Metadata => {
                    (self.metadata, SectionKind::Metadata, KnownKindCursor::Rows)
                }
                KnownKindCursor::Rows => (self.rows, SectionKind::Rows, KnownKindCursor::Data),
                KnownKindCursor::Data => (self.data, SectionKind::Data, KnownKindCursor::Exhausted),
                KnownKindCursor::Exhausted => return None,
            };
            *cursor = next;
            if index != ABSENT_DESCRIPTOR {
                return Some((index, kind));
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn absent_count(self, descriptor_count: usize) -> usize {
        descriptor_count - usize::from(self.membership_count())
    }
}

pub(crate) fn validate_directory<'a>(
    bytes: &'a [u8],
    header: Header<'a>,
    limits: DecodeLimits,
) -> Result<DirectoryState, ValidateError> {
    let mut state = DirectoryState {
        previous_kind: None,
        previous_end: header.directory_end,
        arena_bytes: 0,
        known: KnownDirectory::EMPTY,
    };
    for (index, descriptor) in header.descriptors.iter().enumerate() {
        validate_descriptor(bytes, header, limits, index, descriptor, &mut state)?;
    }
    Ok(state)
}

pub(crate) const fn enforce_limit(
    kind: LimitKind,
    observed: u32,
    maximum: u32,
) -> Result<(), ValidateError> {
    if observed > maximum {
        return Err(ValidateError::LimitExceeded {
            kind,
            observed,
            maximum,
        });
    }
    Ok(())
}
