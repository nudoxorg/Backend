//! Defines encode behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the encode invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::size_of;

use heart_identity::{FixedCanonicalRecord, GenerationHasher, GenerationId};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned,
    byteorder::{BigEndian, U64},
};

use crate::packed::{NO_PARENT, RootRow};

/// Canonical root header grammar, shared by streaming identity and explicit bytes.
#[derive(Immutable, IntoBytes, FromBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct RootHeaderRecord {
    pub(crate) count: U64<BigEndian>,
}

/// Canonical semantic row grammar. All fields are byte arrays so `repr(C)` has
/// alignment one and no padding; `size_of` is the authoritative wire width.
#[derive(Clone, Copy, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned)]
#[repr(u8)]
pub(crate) enum ParentWire {
    Absent = 0,
    Present = 1,
}

impl From<ParentWire> for u8 {
    #[allow(
        clippy::as_conversions,
        reason = "the closed repr(u8) wire enum has the exact canonical discriminants 0 and 1"
    )]
    fn from(parent: ParentWire) -> Self {
        parent as Self
    }
}

/// Canonical semantic row grammar. All fields are byte arrays so `repr(C)` has
/// alignment one and no padding; `size_of` is the authoritative wire width.
#[derive(Clone, Copy, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned)]
#[repr(C)]
pub(crate) struct RootWireRecord {
    pub(crate) key: U64<BigEndian>,
    pub(crate) parent_present: ParentWire,
    pub(crate) parent_key: U64<BigEndian>,
    pub(crate) descriptor: heart_object::ObjectDescriptorWireRecord,
}

const ROOT_HEADER_RECORD_BYTES: usize = size_of::<RootHeaderRecord>();
pub(crate) const ROOT_ROW_RECORD_BYTES: usize = size_of::<RootWireRecord>();
const _: [(); 63] = [(); ROOT_ROW_RECORD_BYTES];

impl RootHeaderRecord {
    const fn new(count: u64) -> Self {
        Self {
            count: U64::new(count),
        }
    }
}

impl RootWireRecord {
    #[allow(
        clippy::as_conversions,
        clippy::indexing_slicing,
        reason = "the builder resolved every non-sentinel compact parent from this exact immutable row slice"
    )]
    fn from_row<DomainTag>(rows: &[RootRow<DomainTag>], row: &RootRow<DomainTag>) -> Self {
        let (parent_present, parent_key) = match row.parent() {
            NO_PARENT => (ParentWire::Absent, U64::new(0)),
            parent => (ParentWire::Present, U64::new(*rows[parent as usize].key)),
        };
        Self {
            key: U64::new(*row.key),
            parent_present,
            parent_key,
            descriptor: heart_object::ObjectDescriptorWireRecord::from(&row.object()),
        }
    }
}

impl FixedCanonicalRecord<ROOT_HEADER_RECORD_BYTES> for RootHeaderRecord {
    fn canonical_bytes(&self) -> &[u8; ROOT_HEADER_RECORD_BYTES] {
        zerocopy::transmute_ref!(self)
    }
}

impl FixedCanonicalRecord<ROOT_ROW_RECORD_BYTES> for RootWireRecord {
    fn canonical_bytes(&self) -> &[u8; ROOT_ROW_RECORD_BYTES] {
        zerocopy::transmute_ref!(self)
    }
}

/// Streams canonical semantic rows into the foundation hasher with no retained encoding buffer.
pub(crate) fn canonical_id<DomainTag>(rows: &[RootRow<DomainTag>]) -> GenerationId {
    let header = RootHeaderRecord::new(canonical_count(rows.len()));
    let mut hasher = GenerationHasher::new();
    hasher.write_record(&header);
    for row in rows {
        hasher.write_record(&RootWireRecord::from_row(rows, row));
    }
    hasher.finalize()
}

/// Returns the exact canonical byte extent for one already validated root arena.
pub(crate) const fn canonical_len<DomainTag>(rows: &[RootRow<DomainTag>]) -> usize {
    ROOT_HEADER_RECORD_BYTES + rows.len() * ROOT_ROW_RECORD_BYTES
}

/// Writes canonical root bytes after the caller preflights the exact output prefix.
#[allow(
    clippy::indexing_slicing,
    reason = "GenerationRoot::write_canonical proved this exact output extent before delegation"
)]
pub(crate) fn write_canonical<DomainTag>(rows: &[RootRow<DomainTag>], output: &mut [u8]) {
    let header = RootHeaderRecord::new(canonical_count(rows.len()));
    output[..ROOT_HEADER_RECORD_BYTES].copy_from_slice(header.canonical_bytes());
    let mut offset = ROOT_HEADER_RECORD_BYTES;
    for row in rows {
        let record = RootWireRecord::from_row(rows, row);
        output[offset..offset + ROOT_ROW_RECORD_BYTES].copy_from_slice(record.canonical_bytes());
        offset += ROOT_ROW_RECORD_BYTES;
    }
}

#[allow(
    clippy::as_conversions,
    reason = "heart-root's explicit target gate permits only 32/64-bit usize, both losslessly representable as u64"
)]
const fn canonical_count(rows: usize) -> u64 {
    rows as u64
}
