use core::mem::size_of;

use nudox_id::{FixedCanonicalRecord, GenerationHasher, GenerationId, HASH_BYTES};
use zerocopy::{
    Immutable, IntoBytes,
    byteorder::{BigEndian, U16, U32, U64},
};

use crate::packed::{NO_PARENT, RootRow};

/// Canonical root header grammar, shared by streaming identity and explicit bytes.
#[derive(Immutable, IntoBytes)]
#[repr(C)]
struct RootHeaderRecord {
    count: U64<BigEndian>,
}

/// Canonical semantic row grammar. All fields are byte arrays so `repr(C)` has
/// alignment one and no padding; `size_of` is the authoritative wire width.
#[derive(Immutable, IntoBytes)]
#[repr(C)]
struct RootRowRecord {
    key: U64<BigEndian>,
    parent_present: u8,
    parent_key: U64<BigEndian>,
    content: [u8; HASH_BYTES],
    length: U64<BigEndian>,
    schema: U32<BigEndian>,
    kind: U16<BigEndian>,
}

const ROOT_HEADER_RECORD_BYTES: usize = size_of::<RootHeaderRecord>();
const ROOT_ROW_RECORD_BYTES: usize = size_of::<RootRowRecord>();

impl RootHeaderRecord {
    const fn new(count: u64) -> Self {
        Self {
            count: U64::new(count),
        }
    }
}

impl RootRowRecord {
    #[allow(
        clippy::as_conversions,
        clippy::indexing_slicing,
        reason = "the builder resolved every non-sentinel compact parent from this exact immutable row slice"
    )]
    fn from_row<DomainTag>(rows: &[RootRow<DomainTag>], row: &RootRow<DomainTag>) -> Self {
        let (parent_present, parent_key) = match row.parent {
            NO_PARENT => (0, U64::new(0)),
            parent => (1, U64::new(*rows[parent as usize].key)),
        };
        Self {
            key: U64::new(*row.key),
            parent_present,
            parent_key,
            content: *row.object.content,
            length: U64::new(*row.object.length),
            schema: U32::new(u32::from(row.object.schema)),
            kind: U16::new(*row.object.kind),
        }
    }
}

impl FixedCanonicalRecord<ROOT_HEADER_RECORD_BYTES> for RootHeaderRecord {
    fn canonical_bytes(&self) -> &[u8; ROOT_HEADER_RECORD_BYTES] {
        zerocopy::transmute_ref!(self)
    }
}

impl FixedCanonicalRecord<ROOT_ROW_RECORD_BYTES> for RootRowRecord {
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
        hasher.write_record(&RootRowRecord::from_row(rows, row));
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
        let record = RootRowRecord::from_row(rows, row);
        output[offset..offset + ROOT_ROW_RECORD_BYTES].copy_from_slice(record.canonical_bytes());
        offset += ROOT_ROW_RECORD_BYTES;
    }
}

#[allow(
    clippy::as_conversions,
    reason = "nudox-root's explicit target gate permits only 32/64-bit usize, both losslessly representable as u64"
)]
const fn canonical_count(rows: usize) -> u64 {
    rows as u64
}
