//! Defines root-view parse behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the root-view parse invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Canonical borrowed-root parsing and transient hierarchy validation.

use alloc::{collections::TryReserveError, vec::Vec};
use core::mem::size_of;

use heart_identity::{ContentAuthority, ContentAuthorityError, Domain, GenerationId};
use heart_object::ObjectDescriptorDecodeError;
use thiserror::Error;
use zerocopy::{FromBytes, TryFromBytes};

use super::{BorrowedRootFacts, BorrowedRootRows, ValidatedRoot};
use crate::{
    EntryKey,
    encode::{ParentWire, ROOT_ROW_RECORD_BYTES, RootHeaderRecord, RootWireRecord},
    packed::NO_PARENT,
};

pub(super) const ROOT_HEADER_BYTES: usize = size_of::<RootHeaderRecord>();

/// Canonical-root byte validation failure.
#[derive(Debug, Error)]
pub enum RootReadError {
    /// The complete fixed header was not present.
    #[error("root header needs {required} bytes but only {available} are available")]
    HeaderTruncated {
        /// Fixed header width.
        required: usize,
        /// Complete supplied byte length.
        available: usize,
    },
    /// The declared row count does not fit the root's compact coordinate space.
    #[error("root declares {declared} rows, exceeding the compact row limit")]
    CountOutOfRange {
        /// Complete observed wide count.
        declared: u64,
        /// Narrowing failure.
        #[source]
        source: core::num::TryFromIntError,
    },
    /// The fixed-width grammar could not fit the host address space.
    #[error("root byte layout overflows for {count} rows")]
    LayoutOverflow {
        /// Declared compact row count.
        count: u32,
    },
    /// The artifact ended before its declared row extent.
    #[error("root declares an exact {required}-byte extent but only {available} are available")]
    Truncated {
        /// Exact declared grammar extent.
        required: usize,
        /// Complete supplied byte length.
        available: usize,
    },
    /// The artifact contains bytes after its declared rows.
    #[error("root has {actual} bytes but its exact grammar requires {expected}")]
    TrailingBytes {
        /// Exact declared grammar extent.
        expected: usize,
        /// Complete supplied byte length.
        actual: usize,
    },
    /// One row carried a parent-presence value outside the closed wire grammar.
    #[error("root row {ordinal} has invalid parent tag {observed}")]
    ParentTag {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Complete observed tag byte.
        observed: u8,
    },
    /// One row carried an invalid nested object descriptor.
    #[error("root row {ordinal} has an invalid object descriptor")]
    Descriptor {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Exact nested descriptor rejection.
        #[source]
        source: ObjectDescriptorDecodeError,
    },
    /// Exact geometry passed, but the bytes could not inhabit the typed row grammar.
    #[error("root rows violated their typed wire representation")]
    TypedRows,
    /// One row's content identity belongs to a different closed domain.
    #[error("root row {ordinal} content authority failed checked decode")]
    ContentAuthority {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Checked authority rejection with complete operands.
        #[source]
        source: ContentAuthorityError,
    },
    /// Canonical keys were duplicated or not strictly increasing.
    #[error("root row {ordinal} key {current:?} does not strictly follow {previous:?}")]
    KeyOrder {
        /// Canonical row ordinal of `current`.
        ordinal: u32,
        /// Previous key.
        previous: EntryKey,
        /// Current key.
        current: EntryKey,
    },
    /// An absent parent marker carried a nonzero key.
    #[error("root row {ordinal} key {key:?} has absent parent marker with key {observed:?}")]
    AbsentParentKey {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Child key.
        key: EntryKey,
        /// Noncanonical parent-key cell.
        observed: EntryKey,
    },
    /// A row names itself as its parent.
    #[error("root row {ordinal} key {key:?} names itself as parent")]
    SelfParent {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Self-parented key.
        key: EntryKey,
    },
    /// A present parent key was absent from the same canonical root.
    #[error(
        "root row {ordinal} key {key:?} names missing parent {parent:?}, whose canonical insertion coordinate is {insertion}"
    )]
    MissingParent {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Child key.
        key: EntryKey,
        /// Missing parent key.
        parent: EntryKey,
        /// Exact insertion coordinate returned by canonical-key lookup.
        insertion: usize,
    },
    /// The exact compact hierarchy lane could not be reserved.
    #[error("root hierarchy validation could not reserve {requested_bytes} transient bytes")]
    HierarchyScratch {
        /// Exact logical `u32` lane width.
        requested_bytes: usize,
        /// Allocator reservation cause.
        #[source]
        source: TryReserveError,
    },
    /// The parent relation contains a cycle.
    #[error("root hierarchy contains a cycle reachable from {key:?}")]
    HierarchyCycle {
        /// First canonical start whose unresolved path exposed the cycle.
        key: EntryKey,
    },
}

#[derive(Default)]
pub(super) struct RootValidationWork {
    pub(super) authority_rows: usize,
    pub(super) order_rows: usize,
    pub(super) parent_rows: usize,
    pub(super) hierarchy_steps: usize,
    pub(super) scratch_high_water_bytes: usize,
}

impl<'bytes, DomainTag: Domain> TryFrom<&'bytes [u8]> for ValidatedRoot<'bytes, DomainTag> {
    type Error = RootReadError;

    fn try_from(bytes: &'bytes [u8]) -> Result<Self, Self::Error> {
        parse_root(bytes, &mut RootValidationWork::default())
    }
}

pub(super) fn parse_root<'bytes, DomainTag: Domain>(
    bytes: &'bytes [u8],
    work: &mut RootValidationWork,
) -> Result<ValidatedRoot<'bytes, DomainTag>, RootReadError> {
    let header_bytes = bytes
        .get(..ROOT_HEADER_BYTES)
        .ok_or(RootReadError::HeaderTruncated {
            required: ROOT_HEADER_BYTES,
            available: bytes.len(),
        })?;
    let header = RootHeaderRecord::ref_from_bytes(header_bytes).map_err(|source| {
        RootReadError::HeaderTruncated {
            required: ROOT_HEADER_BYTES,
            available: source.into_src().len(),
        }
    })?;
    let declared = header.count.get();
    let count = u32::try_from(declared)
        .map_err(|source| RootReadError::CountOutOfRange { declared, source })?;
    let native_count = native(count);
    let row_bytes = native_count
        .checked_mul(ROOT_ROW_RECORD_BYTES)
        .ok_or(RootReadError::LayoutOverflow { count })?;
    let expected = ROOT_HEADER_BYTES
        .checked_add(row_bytes)
        .ok_or(RootReadError::LayoutOverflow { count })?;
    if bytes.len() < expected {
        return Err(RootReadError::Truncated {
            required: expected,
            available: bytes.len(),
        });
    }
    if bytes.len() > expected {
        return Err(RootReadError::TrailingBytes {
            expected,
            actual: bytes.len(),
        });
    }
    #[allow(
        clippy::indexing_slicing,
        reason = "the exact-extent checks immediately above prove this complete declared row region"
    )]
    let row_region = &bytes[ROOT_HEADER_BYTES..expected];
    let rows = <[RootWireRecord]>::try_ref_from_bytes_with_elems(row_region, native_count)
        .map_err(|_| diagnose_typed_rows(row_region))?;
    let rows = validate_authority::<DomainTag>(rows, work)?;
    validate_key_order(rows.rows(), work)?;
    validate_hierarchy(rows.rows(), work)?;
    Ok(ValidatedRoot {
        facts: BorrowedRootFacts {
            bytes,
            id: GenerationId::from_canonical_bytes(bytes),
            entry_count: count.into(),
        },
        rows,
    })
}

fn diagnose_typed_rows(bytes: &[u8]) -> RootReadError {
    const PARENT_OFFSET: usize = core::mem::offset_of!(RootWireRecord, parent_present);
    const DESCRIPTOR_OFFSET: usize = core::mem::offset_of!(RootWireRecord, descriptor);
    const SCHEMA_OFFSET: usize =
        DESCRIPTOR_OFFSET + core::mem::offset_of!(heart_object::ObjectDescriptorWireRecord, schema);
    for (ordinal, row) in (0_u32..).zip(bytes.chunks_exact(ROOT_ROW_RECORD_BYTES)) {
        if let Some(&observed) = row.get(PARENT_OFFSET)
            && observed > u8::from(ParentWire::Present)
        {
            return RootReadError::ParentTag { ordinal, observed };
        }
        let schema = row
            .get(SCHEMA_OFFSET..SCHEMA_OFFSET + size_of::<u32>())
            .and_then(|bytes| <&[u8; 4]>::try_from(bytes).ok())
            .map(|bytes| u32::from_be_bytes(*bytes));
        if let Some(Err(source)) = schema.map(heart_schema::SchemaId::try_from) {
            return RootReadError::Descriptor {
                ordinal,
                source: ObjectDescriptorDecodeError::Schema(source),
            };
        }
    }
    RootReadError::TypedRows
}

fn validate_authority<'bytes, DomainTag: Domain>(
    rows: &'bytes [RootWireRecord],
    work: &mut RootValidationWork,
) -> Result<BorrowedRootRows<'bytes, DomainTag>, RootReadError> {
    let Some((first, remaining)) = rows.split_first() else {
        return Ok(BorrowedRootRows::Empty(rows));
    };
    work.authority_rows += 1;
    let authority = ContentAuthority::try_from(first.descriptor.content[0])
        .map_err(|source| RootReadError::ContentAuthority { ordinal: 0, source })?;
    for (ordinal, row) in (1_u32..).zip(remaining) {
        work.authority_rows += 1;
        ContentAuthority::<DomainTag>::try_from(row.descriptor.content[0])
            .map_err(|source| RootReadError::ContentAuthority { ordinal, source })?;
    }
    Ok(BorrowedRootRows::Populated { rows, authority })
}

fn validate_key_order(
    rows: &[RootWireRecord],
    work: &mut RootValidationWork,
) -> Result<(), RootReadError> {
    for (ordinal, pair) in (1_u32..).zip(rows.windows(2)) {
        work.order_rows += 1;
        let [previous_row, current_row] = pair else {
            continue;
        };
        let previous = EntryKey::from(previous_row.key.get());
        let current = EntryKey::from(current_row.key.get());
        if previous >= current {
            return Err(RootReadError::KeyOrder {
                ordinal,
                previous,
                current,
            });
        }
    }
    Ok(())
}

#[allow(
    clippy::indexing_slicing,
    reason = "the parent lane is allocated to the exact typed row count and every ordinal comes from that row iterator"
)]
fn validate_hierarchy(
    rows: &[RootWireRecord],
    work: &mut RootValidationWork,
) -> Result<(), RootReadError> {
    validate_parent_forms(rows, work)?;
    validate_parent_membership(rows)?;

    let requested_bytes = rows
        .len()
        .checked_mul(size_of::<u32>())
        .ok_or(RootReadError::LayoutOverflow { count: u32::MAX })?;
    let mut parents = Vec::new();
    parents
        .try_reserve_exact(rows.len())
        .map_err(|source| RootReadError::HierarchyScratch {
            requested_bytes,
            source,
        })?;
    parents.resize(rows.len(), NO_PARENT);
    work.scratch_high_water_bytes = requested_bytes;

    for (ordinal, row) in (0_u32..).zip(rows) {
        if matches!(row.parent_present, ParentWire::Present) {
            let key = EntryKey::from(row.key.get());
            let parent = EntryKey::from(row.parent_key.get());
            let position = rows
                .binary_search_by_key(&*parent, |candidate| candidate.key.get())
                .map_err(|insertion| RootReadError::MissingParent {
                    ordinal,
                    key,
                    parent,
                    insertion,
                })?;
            parents[native(ordinal)] = compact(position);
        }
    }
    validate_acyclic(rows, &mut parents, work)
}

fn validate_parent_forms(
    rows: &[RootWireRecord],
    work: &mut RootValidationWork,
) -> Result<(), RootReadError> {
    for (ordinal, row) in (0_u32..).zip(rows) {
        work.parent_rows += 1;
        let key = EntryKey::from(row.key.get());
        let parent = EntryKey::from(row.parent_key.get());
        match row.parent_present {
            ParentWire::Absent if *parent != 0 => {
                return Err(RootReadError::AbsentParentKey {
                    ordinal,
                    key,
                    observed: parent,
                });
            }
            ParentWire::Present if parent == key => {
                return Err(RootReadError::SelfParent { ordinal, key });
            }
            ParentWire::Absent | ParentWire::Present => {}
        }
    }
    Ok(())
}

fn validate_parent_membership(rows: &[RootWireRecord]) -> Result<(), RootReadError> {
    for (ordinal, row) in (0_u32..).zip(rows) {
        if matches!(row.parent_present, ParentWire::Present) {
            let key = EntryKey::from(row.key.get());
            let parent = EntryKey::from(row.parent_key.get());
            rows.binary_search_by_key(&*parent, |candidate| candidate.key.get())
                .map_err(|insertion| RootReadError::MissingParent {
                    ordinal,
                    key,
                    parent,
                    insertion,
                })?;
        }
    }
    Ok(())
}

#[allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "every compact coordinate was binary-resolved within this exact lane; the numeric start is also the diagnostic root-row coordinate"
)]
fn validate_acyclic(
    rows: &[RootWireRecord],
    parents: &mut [u32],
    work: &mut RootValidationWork,
) -> Result<(), RootReadError> {
    for start in 0..parents.len() {
        let mut slow = compact(start);
        let mut fast = slow;
        loop {
            work.hierarchy_steps += 1;
            slow = parents[native(slow)];
            if slow == NO_PARENT {
                break;
            }
            fast = parents[native(fast)];
            if fast == NO_PARENT {
                break;
            }
            fast = parents[native(fast)];
            if fast == NO_PARENT {
                break;
            }
            if slow == fast {
                return Err(RootReadError::HierarchyCycle {
                    key: EntryKey::from(rows[start].key.get()),
                });
            }
        }
        let mut cursor = compact(start);
        while cursor != NO_PARENT {
            work.hierarchy_steps += 1;
            let next = parents[native(cursor)];
            parents[native(cursor)] = NO_PARENT;
            cursor = next;
        }
    }
    Ok(())
}

#[allow(
    clippy::as_conversions,
    reason = "the root crate supports 32/64-bit usize and every compact count fits both"
)]
const fn native(value: u32) -> usize {
    value as usize
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "positions originate in a slice whose declared length already fit u32"
)]
const fn compact(value: usize) -> u32 {
    value as u32
}
