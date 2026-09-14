//! Defines pack grammar behavior for `backend-engine index_publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack grammar invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Fixed versioned wire records for immutable exact-and-lexical index packs.

use core::mem::size_of;

use backend_version::{GenerationId, HASH_BYTES};
use backend_semantic::index_core::{
    ENTITY_DOCUMENT_ID_BYTES, MAX_EXACT_PAYLOAD_BYTES, MAX_EXACT_ROWS, MAX_LEXICAL_PAYLOAD_BYTES,
    MAX_LEXICAL_ROWS,
};
use backend_semantic::index_vocabulary::{ExactSegmentId, IndexSnapshotId, LexicalSegmentId};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    byteorder::{LittleEndian, U16, U32},
};

use crate::index_publish::pack::MAX_PACK_SEGMENTS;
use crate::index_publish::pack::error::{IndexPackLane, IndexPackOpenError, IndexPackRegion};

/// Fixed wire width of the grammar signature.
pub(super) const INDEX_PACK_MAGIC_BYTES: usize = 8;
/// Reserved fixed-header bytes that must remain zero in grammar version one.
pub(super) const HEADER_RESERVED_BYTES: usize = 6;
/// Exact magic bytes for the first index-pack grammar.
pub(super) const INDEX_PACK_MAGIC: [u8; INDEX_PACK_MAGIC_BYTES] = *b"NUDXIPK\0";
/// Closed grammar version for [`INDEX_PACK_MAGIC`].
pub(super) const INDEX_PACK_VERSION: u16 = 1;

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(super) struct PackHeaderWire {
    magic: [u8; INDEX_PACK_MAGIC_BYTES],
    version: U16<LittleEndian>,
    header_bytes: U16<LittleEndian>,
    total_bytes: U32<LittleEndian>,
    generation: [u8; HASH_BYTES],
    snapshot: [u8; HASH_BYTES],
    exact_count: u8,
    lexical_count: u8,
    reserved: [u8; HEADER_RESERVED_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(super) struct SegmentDirectoryWire {
    identity: [u8; HASH_BYTES],
    body_start: U32<LittleEndian>,
    body_bytes: U32<LittleEndian>,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(super) struct SegmentRowsWire {
    row_count: U16<LittleEndian>,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(super) struct ExactRowWire {
    state: RowStateWire,
    key_bytes: U32<LittleEndian>,
    value_bytes: U32<LittleEndian>,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
pub(super) struct LexicalRowWire {
    term_bytes: U32<LittleEndian>,
    document: [u8; ENTITY_DOCUMENT_ID_BYTES],
    state: RowStateWire,
    score: U32<LittleEndian>,
}

/// Complete fixed header width; all later geometry derives from this record once.
pub(super) const INDEX_PACK_HEADER_BYTES: usize = size_of::<PackHeaderWire>();
/// Complete fixed selected-segment directory record width.
pub(super) const INDEX_PACK_DIRECTORY_BYTES: usize = size_of::<SegmentDirectoryWire>();
pub(super) const SEGMENT_ROW_COUNT_BYTES: usize = size_of::<SegmentRowsWire>();
pub(super) const ROW_OFFSET_BYTES: usize = size_of::<U32<LittleEndian>>();
pub(super) const EXACT_ROW_PREFIX_BYTES: usize = size_of::<ExactRowWire>();
pub(super) const LEXICAL_ROW_PREFIX_BYTES: usize = size_of::<LexicalRowWire>();
/// One closed canonical row state encoded in both row record variants.
#[repr(transparent)]
#[derive(Clone, Copy, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub(super) struct RowStateWire(pub(super) u8);

pub(super) const ROW_TOMBSTONE: RowStateWire = RowStateWire(0);
pub(super) const ROW_PRESENT: RowStateWire = RowStateWire(1);
/// A tombstone has no value bytes in either lane grammar.
pub(super) const TOMBSTONE_VALUE_BYTES: usize = 0;
/// A lexical tombstone carries no score payload.
pub(super) const TOMBSTONE_SCORE: u32 = 0;
/// This grammar has exactly exact and lexical segment lanes.
const INDEX_PACK_LANE_COUNT: usize = 2;

/// Largest exact body admitted by the existing row count and payload laws.
pub(super) const MAX_EXACT_BODY_BYTES: usize = SEGMENT_ROW_COUNT_BYTES
    + MAX_EXACT_ROWS * ROW_OFFSET_BYTES
    + MAX_EXACT_ROWS * EXACT_ROW_PREFIX_BYTES
    + MAX_EXACT_PAYLOAD_BYTES;
/// Largest lexical body admitted by the existing row count and payload laws.
pub(super) const MAX_LEXICAL_BODY_BYTES: usize = SEGMENT_ROW_COUNT_BYTES
    + MAX_LEXICAL_ROWS * ROW_OFFSET_BYTES
    + MAX_LEXICAL_ROWS * LEXICAL_ROW_PREFIX_BYTES
    + MAX_LEXICAL_PAYLOAD_BYTES;
/// Bounded full owner size for the fixed selected-lane capacity.
pub(super) const MAX_INDEX_PACK_BYTES: usize = INDEX_PACK_HEADER_BYTES
    + (MAX_PACK_SEGMENTS * INDEX_PACK_LANE_COUNT) * INDEX_PACK_DIRECTORY_BYTES
    + MAX_PACK_SEGMENTS * (MAX_EXACT_BODY_BYTES + MAX_LEXICAL_BODY_BYTES);

/// Fixed header facts admitted before any variable body is traversed.
#[derive(Clone, Copy)]
pub(super) struct PackHeader {
    pub(super) generation: GenerationId,
    pub(super) snapshot: IndexSnapshotId,
    pub(super) exact_count: usize,
    pub(super) lexical_count: usize,
    pub(super) total_bytes: usize,
}

/// One proven contiguous selected-segment body range.
#[derive(Clone, Copy)]
pub(super) struct PackRange {
    pub(super) start: usize,
    pub(super) end: usize,
}

impl PackRange {
    pub(super) const fn bytes(self) -> usize {
        self.end - self.start
    }
}

/// Typed directory record for one exact segment body.
#[derive(Clone, Copy)]
pub(super) struct ExactDirectory {
    pub(super) id: ExactSegmentId,
    pub(super) range: PackRange,
}

/// Typed directory record for one lexical segment body.
#[derive(Clone, Copy)]
pub(super) struct LexicalDirectory {
    pub(super) id: LexicalSegmentId,
    pub(super) range: PackRange,
}

/// Fixed-capacity proved directory layout retained beside an arbitrary byte owner.
#[derive(Clone, Copy)]
pub(super) struct PackLayout {
    pub(super) header: PackHeader,
    pub(super) exact: [Option<ExactDirectory>; MAX_PACK_SEGMENTS],
    pub(super) lexical: [Option<LexicalDirectory>; MAX_PACK_SEGMENTS],
}

pub(super) fn canonical_header(
    total_bytes: u32,
    generation: GenerationId,
    snapshot: IndexSnapshotId,
    exact_count: u8,
    lexical_count: u8,
) -> Result<PackHeaderWire, crate::index_publish::pack::error::IndexPackEncodeError> {
    Ok(PackHeaderWire {
        magic: INDEX_PACK_MAGIC,
        version: U16::new(INDEX_PACK_VERSION),
        header_bytes: U16::new(u16::try_from(INDEX_PACK_HEADER_BYTES).map_err(|_| {
            crate::index_publish::pack::error::IndexPackEncodeError::AddressSpace {
                observed: INDEX_PACK_HEADER_BYTES,
            }
        })?),
        total_bytes: U32::new(total_bytes),
        generation: *generation,
        snapshot: *snapshot,
        exact_count,
        lexical_count,
        reserved: [0; HEADER_RESERVED_BYTES],
    })
}

pub(super) const fn canonical_directory(
    identity: [u8; HASH_BYTES],
    body_start: u32,
    body_bytes: u32,
) -> SegmentDirectoryWire {
    SegmentDirectoryWire {
        identity,
        body_start: U32::new(body_start),
        body_bytes: U32::new(body_bytes),
    }
}

pub(super) const fn canonical_rows(row_count: u16) -> SegmentRowsWire {
    SegmentRowsWire {
        row_count: U16::new(row_count),
    }
}

pub(super) const fn canonical_exact_row(
    state: RowStateWire,
    key_bytes: u32,
    value_bytes: u32,
) -> ExactRowWire {
    ExactRowWire {
        state,
        key_bytes: U32::new(key_bytes),
        value_bytes: U32::new(value_bytes),
    }
}

pub(super) const fn canonical_lexical_row(
    term_bytes: u32,
    document: [u8; ENTITY_DOCUMENT_ID_BYTES],
    state: RowStateWire,
    score: u32,
) -> LexicalRowWire {
    LexicalRowWire {
        term_bytes: U32::new(term_bytes),
        document,
        state,
        score: U32::new(score),
    }
}

pub(super) fn parse_header(bytes: &[u8]) -> Result<PackHeader, IndexPackOpenError> {
    let header = parse_record::<PackHeaderWire>(bytes, 0, IndexPackRegion::Header)?;
    if header.magic != INDEX_PACK_MAGIC {
        return Err(IndexPackOpenError::Magic {
            observed: header.magic,
        });
    }
    let version = header.version.get();
    if version != INDEX_PACK_VERSION {
        return Err(IndexPackOpenError::Version { observed: version });
    }
    let header_bytes = header.header_bytes.get();
    if usize::from(header_bytes) != INDEX_PACK_HEADER_BYTES {
        return Err(IndexPackOpenError::HeaderBytes {
            observed: header_bytes,
        });
    }
    let total_bytes = native_address(header.total_bytes.get())?;
    if total_bytes != bytes.len() {
        return Err(IndexPackOpenError::TotalBytes {
            declared: total_bytes,
            actual: bytes.len(),
        });
    }
    let generation = GenerationId::try_from(header.generation)
        .map_err(|source| IndexPackOpenError::GenerationAuthority { source })?;
    let snapshot = IndexSnapshotId::try_from(header.snapshot)
        .map_err(|source| IndexPackOpenError::SnapshotAuthority { source })?;
    let exact_count = usize::from(header.exact_count);
    let lexical_count = usize::from(header.lexical_count);
    check_count(IndexPackLane::Exact, exact_count)?;
    check_count(IndexPackLane::Lexical, lexical_count)?;
    if let Some((offset, observed)) = header
        .reserved
        .iter()
        .copied()
        .enumerate()
        .find(|(_, value)| *value != 0)
    {
        return Err(IndexPackOpenError::Reserved {
            offset: INDEX_PACK_HEADER_BYTES - header.reserved.len() + offset,
            observed,
        });
    }
    Ok(PackHeader {
        generation,
        snapshot,
        exact_count,
        lexical_count,
        total_bytes,
    })
}

pub(super) fn parse_directory(
    bytes: &[u8],
    offset: usize,
    lane: IndexPackLane,
) -> Result<SegmentDirectoryWire, IndexPackOpenError> {
    parse_record(bytes, offset, IndexPackRegion::Directory(lane))
}

pub(super) fn parse_rows(
    bytes: &[u8],
    offset: usize,
    lane: IndexPackLane,
) -> Result<SegmentRowsWire, IndexPackOpenError> {
    parse_record(bytes, offset, IndexPackRegion::Segment(lane))
}

pub(super) fn parse_exact_row(
    bytes: &[u8],
    offset: usize,
) -> Result<ExactRowWire, IndexPackOpenError> {
    parse_record(bytes, offset, IndexPackRegion::Row(IndexPackLane::Exact))
}

pub(super) fn parse_lexical_row(
    bytes: &[u8],
    offset: usize,
) -> Result<LexicalRowWire, IndexPackOpenError> {
    parse_record(bytes, offset, IndexPackRegion::Row(IndexPackLane::Lexical))
}

pub(super) const fn directory_identity(directory: SegmentDirectoryWire) -> [u8; HASH_BYTES] {
    directory.identity
}

pub(super) fn directory_start(
    directory: SegmentDirectoryWire,
) -> Result<usize, IndexPackOpenError> {
    native_address(directory.body_start.get())
}

pub(super) fn directory_bytes(
    directory: SegmentDirectoryWire,
) -> Result<usize, IndexPackOpenError> {
    native_address(directory.body_bytes.get())
}

pub(super) fn row_count(rows: SegmentRowsWire) -> usize {
    usize::from(rows.row_count.get())
}

pub(super) const fn exact_state(row: ExactRowWire) -> RowStateWire {
    row.state
}

pub(super) fn exact_key_bytes(row: ExactRowWire) -> Result<usize, IndexPackOpenError> {
    native_address(row.key_bytes.get())
}

pub(super) fn exact_value_bytes(row: ExactRowWire) -> Result<usize, IndexPackOpenError> {
    native_address(row.value_bytes.get())
}

pub(super) fn lexical_term_bytes(row: LexicalRowWire) -> Result<usize, IndexPackOpenError> {
    native_address(row.term_bytes.get())
}

pub(super) const fn lexical_document(row: LexicalRowWire) -> [u8; ENTITY_DOCUMENT_ID_BYTES] {
    row.document
}

pub(super) const fn lexical_state(row: LexicalRowWire) -> RowStateWire {
    row.state
}

pub(super) const fn lexical_score(row: LexicalRowWire) -> u32 {
    row.score.get()
}

pub(super) fn parse_offset(
    bytes: &[u8],
    offset: usize,
    lane: IndexPackLane,
) -> Result<usize, IndexPackOpenError> {
    let raw = parse_record::<U32<LittleEndian>>(bytes, offset, IndexPackRegion::Segment(lane))?;
    native_address(raw.get())
}

fn native_address(value: u32) -> Result<usize, IndexPackOpenError> {
    usize::try_from(value).map_err(|_| IndexPackOpenError::AddressWidth { observed: value })
}

pub(super) fn bytes_at(
    bytes: &[u8],
    range: PackRange,
    region: IndexPackRegion,
) -> Result<&[u8], IndexPackOpenError> {
    bytes
        .get(range.start..range.end)
        .ok_or(IndexPackOpenError::Truncated {
            region,
            offset: range.start,
            required: range.bytes(),
            available: bytes.len(),
        })
}

pub(super) fn directory_end(header: PackHeader) -> Result<usize, IndexPackOpenError> {
    let entries = header
        .exact_count
        .checked_add(header.lexical_count)
        .and_then(|count| count.checked_mul(INDEX_PACK_DIRECTORY_BYTES))
        .ok_or(IndexPackOpenError::TrailingBytes {
            offset: INDEX_PACK_HEADER_BYTES,
            total: header.total_bytes,
        })?;
    let end =
        INDEX_PACK_HEADER_BYTES
            .checked_add(entries)
            .ok_or(IndexPackOpenError::TrailingBytes {
                offset: INDEX_PACK_HEADER_BYTES,
                total: header.total_bytes,
            })?;
    if end > header.total_bytes {
        return Err(IndexPackOpenError::Truncated {
            region: IndexPackRegion::Directory(IndexPackLane::Exact),
            offset: INDEX_PACK_HEADER_BYTES,
            required: entries,
            available: header.total_bytes,
        });
    }
    Ok(end)
}

pub(super) fn checked_end(
    start: usize,
    length: usize,
    lane: IndexPackLane,
    ordinal: usize,
    total: usize,
) -> Result<usize, IndexPackOpenError> {
    let end = start
        .checked_add(length)
        .ok_or(IndexPackOpenError::SegmentEnd {
            lane,
            ordinal,
            end: usize::MAX,
            total,
        })?;
    if end > total {
        return Err(IndexPackOpenError::SegmentEnd {
            lane,
            ordinal,
            end,
            total,
        });
    }
    Ok(end)
}

fn parse_record<Record>(
    bytes: &[u8],
    offset: usize,
    region: IndexPackRegion,
) -> Result<Record, IndexPackOpenError>
where
    Record: FromBytes + Immutable + KnownLayout,
{
    let width = size_of::<Record>();
    let end = offset
        .checked_add(width)
        .ok_or(IndexPackOpenError::Truncated {
            region,
            offset,
            required: width,
            available: bytes.len(),
        })?;
    let record_bytes = bytes
        .get(offset..end)
        .ok_or(IndexPackOpenError::Truncated {
            region,
            offset,
            required: width,
            available: bytes.len(),
        })?;
    Record::read_from_bytes(record_bytes).map_err(|source| IndexPackOpenError::Truncated {
        region,
        offset,
        required: width,
        available: source.into_src().len(),
    })
}

const fn check_count(lane: IndexPackLane, observed: usize) -> Result<(), IndexPackOpenError> {
    if observed > MAX_PACK_SEGMENTS {
        return Err(IndexPackOpenError::SegmentCount {
            lane,
            observed,
            maximum: MAX_PACK_SEGMENTS,
        });
    }
    Ok(())
}
