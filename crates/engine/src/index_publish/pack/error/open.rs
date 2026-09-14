//! Defines pack error open behavior for `backend-engine index_publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack error open invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Owner-generic structural and semantic validation failures.

use backend_version::{ContentIdDecodeError, HASH_BYTES};
use backend_semantic::index_vocabulary::{ExactSegmentId, IndexPackId, IndexSnapshotId, LexicalSegmentId};

use super::{IndexPackLane, IndexPackRegion, IndexPackRowInvariant};
use crate::index_publish::pack::grammar::INDEX_PACK_MAGIC_BYTES;

/// Failure while validating a pack owner into a borrowed immutable view.
#[derive(Debug, thiserror::Error)]
pub enum IndexPackOpenError {
    /// The requested physical pack identity differs from the final byte stream.
    #[error("index pack physical identity differs from its complete bytes")]
    Identity {
        /// Identity requested by the receipt or content-addressed path.
        expected: IndexPackId,
        /// Identity derived from all supplied pack bytes.
        observed: IndexPackId,
    },
    /// The owner ended before a fixed grammar region could be read.
    #[error("index pack is truncated in {region:?}")]
    Truncated {
        /// Region containing the incomplete record.
        region: IndexPackRegion,
        /// First required byte offset.
        offset: usize,
        /// Full record width required at `offset`.
        required: usize,
        /// Complete owner byte length.
        available: usize,
    },
    /// The fixed magic cell did not name this grammar.
    #[error("index pack magic differs from the registered grammar")]
    Magic {
        /// Complete observed fixed magic record.
        observed: [u8; INDEX_PACK_MAGIC_BYTES],
    },
    /// The pack grammar version is not supported by this reader.
    #[error("index pack grammar version is unsupported")]
    Version {
        /// Complete observed version.
        observed: u16,
    },
    /// The header declared a different fixed header geometry.
    #[error("index pack header geometry is noncanonical")]
    HeaderBytes {
        /// Complete observed header width.
        observed: u16,
    },
    /// The header total did not equal the complete owner length.
    #[error("index pack declared length differs from its owner length")]
    TotalBytes {
        /// Complete declared pack width.
        declared: usize,
        /// Complete actual owner width.
        actual: usize,
    },
    /// A fixed 32-bit wire address cannot be represented by this target's native index width.
    #[error("index pack wire address does not fit the target address width")]
    AddressWidth {
        /// Exact little-endian wire address that could not become a native offset.
        observed: u32,
    },
    /// A reserved grammar byte was nonzero.
    #[error("index pack reserved grammar bytes were nonzero")]
    Reserved {
        /// First nonzero reserved-byte position in the fixed header.
        offset: usize,
        /// Complete observed reserved byte.
        observed: u8,
    },
    /// A selected lane count exceeded the existing snapshot bound.
    #[error("index pack selected lane count exceeds the snapshot bound")]
    SegmentCount {
        /// Segment lane carrying the rejected count.
        lane: IndexPackLane,
        /// Complete observed count.
        observed: usize,
        /// Existing shared selection limit.
        maximum: usize,
    },
    /// The packed generation identity did not carry generation authority.
    #[error("index pack generation identity authority is invalid")]
    GenerationAuthority {
        /// Exact fixed identity decoding cause.
        #[source]
        source: ContentIdDecodeError,
    },
    /// The packed snapshot identity did not carry index-snapshot authority.
    #[error("index pack snapshot identity authority is invalid")]
    SnapshotAuthority {
        /// Exact fixed identity decoding cause.
        #[source]
        source: ContentIdDecodeError,
    },
    /// A directory identity did not carry the lane's segment authority.
    #[error("index pack directory identity authority is invalid")]
    DirectoryAuthority {
        /// Segment lane carrying the malformed identity.
        lane: IndexPackLane,
        /// Canonical selected-segment ordinal.
        ordinal: usize,
        /// Exact typed identity decoding cause.
        #[source]
        source: ContentIdDecodeError,
    },
    /// A fixed directory range did not follow the canonical contiguous layout.
    #[error("index pack segment range is noncanonical")]
    SegmentRange {
        /// Segment lane carrying the malformed range.
        lane: IndexPackLane,
        /// Canonical selected-segment ordinal.
        ordinal: usize,
        /// Offset derived from prior fixed grammar records.
        expected: usize,
        /// Offset declared by this directory record.
        observed: usize,
    },
    /// A segment body extended beyond the complete pack width.
    #[error("index pack segment body exceeds the complete pack")]
    SegmentEnd {
        /// Segment lane carrying the malformed range.
        lane: IndexPackLane,
        /// Canonical selected-segment ordinal.
        ordinal: usize,
        /// Declared segment end offset.
        end: usize,
        /// Complete pack byte length.
        total: usize,
    },
    /// Directory identities were not strictly ordered in their canonical lane.
    #[error("index pack directory identities are not strictly ordered")]
    DirectoryOrder {
        /// Segment lane carrying the ordering violation.
        lane: IndexPackLane,
        /// Later canonical selected-segment ordinal.
        ordinal: usize,
        /// Complete preceding identity bytes.
        previous: [u8; HASH_BYTES],
        /// Complete observed identity bytes.
        observed: [u8; HASH_BYTES],
    },
    /// A variable-width row offset did not begin exactly after the preceding row.
    #[error("index pack row offset is noncanonical")]
    RowOffset {
        /// Segment lane carrying the malformed row.
        lane: IndexPackLane,
        /// Canonical selected-segment ordinal.
        segment: usize,
        /// Row ordinal inside the selected segment.
        row: usize,
        /// Offset derived from prior row records.
        expected: usize,
        /// Offset declared by this row directory cell.
        observed: usize,
    },
    /// A closed row-state discriminant was unknown.
    #[error("index pack row state is unknown")]
    RowState {
        /// Segment lane carrying the malformed row.
        lane: IndexPackLane,
        /// Canonical selected-segment ordinal.
        segment: usize,
        /// Row ordinal inside the selected segment.
        row: usize,
        /// Complete raw discriminant byte.
        observed: u8,
    },
    /// A tombstone retained a nonempty value body.
    #[error("index pack tombstone retained a value body")]
    TombstoneValue {
        /// Segment lane carrying the malformed row.
        lane: IndexPackLane,
        /// Canonical selected-segment ordinal.
        segment: usize,
        /// Row ordinal inside the selected segment.
        row: usize,
        /// Complete declared value byte count.
        observed: usize,
    },
    /// A lexical tombstone retained a nonzero score wire value.
    #[error("index pack lexical tombstone retained a score")]
    TombstoneScore {
        /// Canonical selected lexical-segment ordinal.
        segment: usize,
        /// Row ordinal inside the selected segment.
        row: usize,
        /// Exact encoded score that must have been zero.
        observed: u32,
    },
    /// A caller selected a row outside its validated segment's declared row lane.
    #[error("index pack row ordinal exceeds the validated segment lane")]
    RowOrdinal {
        /// Segment lane containing the query target.
        lane: IndexPackLane,
        /// Canonical selected segment ordinal.
        segment: usize,
        /// Requested row ordinal.
        observed: usize,
        /// Validated row count.
        count: usize,
    },
    /// A validated-layout slot was absent despite its declared directory count.
    #[error("index pack validated layout omitted a declared directory slot")]
    LayoutSlot {
        /// Segment lane owning the impossible missing slot.
        lane: IndexPackLane,
        /// Declared selected-directory ordinal.
        ordinal: usize,
    },
    /// A lexical document address was structurally or authoritatively invalid.
    #[error("index pack lexical document address is invalid")]
    LexicalDocument {
        /// Canonical selected lexical-segment ordinal.
        segment: usize,
        /// Row ordinal inside the selected segment.
        row: usize,
        /// Exact typed document decoding cause.
        #[source]
        source: backend_semantic::index_core::EntityDocumentIdError,
    },
    /// The existing core segment semantic invariant rejected decoded rows.
    #[error("index pack decoded rows violate their existing core segment invariant")]
    SegmentInvariant {
        /// Segment lane carrying the rejected row set.
        lane: IndexPackLane,
        /// Canonical selected-segment ordinal.
        segment: usize,
        /// Closed core-invariant class.
        invariant: IndexPackRowInvariant,
    },
    /// Decoded rows did not reproduce the directory's typed semantic segment identity.
    #[error("index pack segment bytes differ from the directory semantic identity")]
    ExactSegmentIdentity {
        /// Canonical selected exact-segment ordinal.
        ordinal: usize,
        /// Directory identity.
        expected: ExactSegmentId,
        /// Identity recomputed through the existing exact row codec.
        observed: ExactSegmentId,
    },
    /// Decoded rows did not reproduce the directory's typed semantic segment identity.
    #[error("index pack segment bytes differ from the directory semantic identity")]
    LexicalSegmentIdentity {
        /// Canonical selected lexical-segment ordinal.
        ordinal: usize,
        /// Directory identity.
        expected: LexicalSegmentId,
        /// Identity recomputed through the existing lexical row codec.
        observed: LexicalSegmentId,
    },
    /// Directory lanes did not reproduce the header's canonical snapshot identity.
    #[error("index pack directories differ from the header snapshot identity")]
    SnapshotIdentity {
        /// Header identity.
        expected: IndexSnapshotId,
        /// Identity recomputed from generation and both selected directory lanes.
        observed: IndexSnapshotId,
    },
    /// Reconstructed directory lanes violated the existing immutable snapshot selection law.
    #[error("index pack directories violate the immutable snapshot selection law")]
    SnapshotInvariant {
        /// Exact existing snapshot validation cause.
        #[source]
        source: backend_semantic::index_core::IndexSnapshotError,
    },
    /// A selected body did not consume exactly its declared range, or all bodies did not consume the pack.
    #[error("index pack directory geometry does not consume the declared range")]
    TrailingBytes {
        /// First unclaimed or overclaimed byte offset.
        offset: usize,
        /// Complete declared pack width.
        total: usize,
    },
}

/// Returned unchanged after owner-generic pack validation fails.
pub struct RejectedIndexPack<Owner>
where
    Owner: AsRef<[u8]>,
{
    /// Exact validation failure.
    pub error: IndexPackOpenError,
    /// Original immutable owner, retained without copying its bytes.
    pub owner: Owner,
}
