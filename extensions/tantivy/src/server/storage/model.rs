//! Durable projection data model and typed boundary errors.
use backend_semantic::index_core::{EntityDocumentId, IndexSnapshotId, LexicalScore, LexicalSegmentId};
use std::{io, path::PathBuf};
use tantivy::{IndexReader, schema::Field};

use super::codec;

pub(crate) const MAX_DURABLE_QUERY_BYTES: usize = 4_096;
pub(crate) const MAX_DURABLE_OPERATIONS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Immutable provenance carried by a durable Tantivy hit.
pub struct TantivyProvenance {
    pub(crate) snapshot: IndexSnapshotId,
    pub(crate) segment: LexicalSegmentId,
    pub(crate) document: EntityDocumentId,
    pub(crate) row: TantivyRowOrdinal,
}

impl TantivyProvenance {
    /// Returns the pinned snapshot identity.
    #[must_use]
    pub const fn snapshot(self) -> IndexSnapshotId {
        self.snapshot
    }
    /// Returns the contributing lexical segment identity.
    #[must_use]
    pub const fn segment(self) -> LexicalSegmentId {
        self.segment
    }
    /// Returns the canonical entity document identity.
    #[must_use]
    pub const fn document(self) -> EntityDocumentId {
        self.document
    }
    /// Returns the canonical row ordinal within the segment.
    #[must_use]
    pub const fn row(self) -> TantivyRowOrdinal {
        self.row
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A canonical lexical hit selected through a validated Tantivy projection.
pub struct TantivySegmentHit<'segment> {
    pub(crate) provenance: TantivyProvenance,
    pub(crate) term: &'segment [u8],
    pub(crate) score: LexicalScore,
}

impl<'segment> TantivySegmentHit<'segment> {
    /// Returns the hit's immutable provenance.
    #[must_use]
    pub const fn provenance(self) -> TantivyProvenance {
        self.provenance
    }
    /// Returns the borrowed canonical term bytes.
    #[must_use]
    pub const fn term(self) -> &'segment [u8] {
        self.term
    }
    /// Returns the fixed-point canonical score.
    #[must_use]
    pub const fn score(self) -> LexicalScore {
        self.score
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Caller-owned candidate slot used by snapshot query composition.
pub struct TantivyCandidate<'segment> {
    pub(crate) segment: LexicalSegmentId,
    pub(crate) segment_index: u8,
    pub(crate) row: TantivyRowOrdinal,
    pub(crate) term: &'segment [u8],
    pub(crate) document: EntityDocumentId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CandidateInsertion {
    Inserted,
    KeptExisting,
    ReplacedSameSegment,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// Checked ordinal of a canonical row in a Tantivy projection.
pub struct TantivyRowOrdinal(pub(crate) u16);
impl TantivyRowOrdinal {
    /// Returns the ordinal as a platform index after bounded validation.
    #[must_use]
    pub fn as_usize(self) -> usize {
        usize::from(self.0)
    }
}
impl TryFrom<usize> for TantivyRowOrdinal {
    type Error = core::num::TryFromIntError;
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        u16::try_from(value).map(Self)
    }
}

/// An opened and sidecar-validated Tantivy segment projection.
pub struct TantivySegment {
    pub(crate) id: LexicalSegmentId,
    pub(crate) path: PathBuf,
    pub(crate) reader: IndexReader,
    pub(crate) body_field: Field,
    pub(crate) ordinal_field: Field,
    pub(crate) rows: Vec<codec::StoredRow>,
}
impl TantivySegment {
    /// Returns the content-addressed lexical segment identity.
    #[must_use]
    pub const fn id(&self) -> LexicalSegmentId {
        self.id
    }
}

#[derive(Clone, Debug)]
/// Durable projection store rooted at an explicit caller-owned directory.
pub struct TantivySegmentStore {
    pub(crate) root: PathBuf,
}
/// Immutable newest-first composition proof over opened segment projections.
pub struct TantivySnapshot<'selection, 'segment> {
    pub(crate) snapshot: IndexSnapshotId,
    pub(crate) segments: &'selection [&'segment TantivySegment],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Filesystem/backend phase attached to a durable projection failure.
pub enum StorePhase {
    /// Create the caller-selected store root.
    CreateRoot,
    /// Create a disposable publication directory.
    CreateTemporary,
    /// Move a corrupt publication aside.
    Quarantine,
    /// Publish or synchronize a final projection.
    Publish,
    /// Write the canonical sidecar.
    WriteSidecar,
    /// Synchronize the canonical sidecar.
    SyncSidecar,
    /// Write the versioned projection recipe.
    WriteRecipe,
    /// Read the versioned projection recipe.
    ReadRecipe,
    /// Read and validate the canonical sidecar.
    ReadSidecar,
    /// Create the Tantivy index directory.
    CreateIndex,
    /// Allocate a Tantivy writer.
    Writer,
    /// Add a canonical row to Tantivy.
    AddDocument,
    /// Commit the Tantivy index.
    Commit,
    /// Reopen a committed Tantivy index.
    Reopen,
    /// Create a Tantivy reader.
    Reader,
    /// Execute a Tantivy membership query.
    Search,
    /// Read a matched Tantivy document.
    ReadDocument,
}

#[derive(Debug, thiserror::Error)]
/// Typed failures at the durable projection and query boundary.
pub enum TantivySegmentStoreError {
    /// Filesystem failure with its phase and concrete path.
    #[error("segment store {phase:?} failed at {path}")]
    Io {
        /// Operation phase.
        phase: StorePhase,
        /// Concrete path involved.
        path: PathBuf,
        #[source]
        /// Underlying filesystem cause.
        source: io::Error,
    },
    /// Tantivy backend failure with its phase and concrete path.
    #[error("Tantivy {phase:?} failed at {path}")]
    Tantivy {
        /// Operation phase.
        phase: StorePhase,
        /// Concrete index path involved.
        path: PathBuf,
        #[source]
        /// Underlying Tantivy cause.
        source: tantivy::TantivyError,
    },
    /// No final projection exists for the requested identity.
    #[error("segment {id:?} is not published")]
    Missing {
        /// Requested segment identity.
        id: LexicalSegmentId,
    },
    /// The derived projection failed canonical or recipe validation.
    #[error("corrupt segment projection at {path}: {detail}")]
    Corrupt {
        /// Invalid projection path.
        path: PathBuf,
        /// Stable validation detail.
        detail: &'static str,
    },
    /// The canonical segment identity disagreed with its projection.
    #[error("segment identity mismatch: expected {expected:?}, observed {observed:?}")]
    IdentityMismatch {
        /// Expected canonical identity.
        expected: LexicalSegmentId,
        /// Observed identity.
        observed: LexicalSegmentId,
    },
    /// A stored entity document identity could not be decoded.
    #[error("malformed entity document identity")]
    MalformedDocument {
        #[source]
        /// Underlying identity decoding cause.
        source: backend_semantic::index_core::EntityDocumentIdError,
    },
    /// The caller's hit output is too short.
    #[error("segment query output has {available} slots, requested {required}")]
    OutputCapacity {
        /// Required slots.
        required: usize,
        /// Available slots.
        available: usize,
    },
    /// The caller's candidate scratch is too short.
    #[error("segment candidate scratch has {available} slots, requested {required}")]
    CandidateCapacity {
        /// Required slots.
        required: usize,
        /// Available slots.
        available: usize,
    },
    /// A backend result referred to an unselected segment.
    #[error("candidate names unselected segment {segment:?}")]
    CandidateSegment {
        /// Unexpected segment identity.
        segment: LexicalSegmentId,
    },
    /// A backend result referred to an out-of-range row.
    #[error("candidate row {row:?} is outside segment {segment:?}")]
    CandidateRow {
        /// Segment containing the invalid row.
        segment: LexicalSegmentId,
        /// Invalid row ordinal.
        row: TantivyRowOrdinal,
    },
    /// The selected segment count exceeds the fixed bound.
    #[error("too many segments: {observed}, limit {limit}")]
    SegmentLimit {
        /// Fixed segment bound.
        limit: usize,
        /// Observed segment count.
        observed: usize,
    },
    /// The opened selection count differs from the pinned snapshot.
    #[error("snapshot selected {expected} segments but {observed} were opened")]
    SnapshotSelection {
        /// Pinned count.
        expected: usize,
        /// Opened count.
        observed: usize,
    },
    /// The opened segments are not in the pinned newest-first order.
    #[error("snapshot segment order mismatch at position {position}")]
    SnapshotSegmentOrder {
        /// Selection position.
        position: usize,
        /// Pinned identity at that position.
        expected: Option<LexicalSegmentId>,
        /// Opened identity at that position.
        observed: LexicalSegmentId,
    },
    /// Caller scratch or internal bounded composition capacity was exceeded.
    #[error("segment query composition exceeded bounded capacity")]
    CompositionCapacity,
    /// The query bytes exceed the fixed bound.
    #[error("query has {observed} bytes, limit is {limit}")]
    QueryBytesLimit {
        /// Fixed byte bound.
        limit: usize,
        /// Observed bytes.
        observed: usize,
    },
    /// The operation count exceeds the fixed bound.
    #[error("query has {observed} terms, limit is {limit}")]
    QueryTermLimit {
        /// Fixed operation bound.
        limit: usize,
        /// Observed operation count.
        observed: usize,
    },
    /// Query cancellation was observed.
    #[error("Tantivy query cancelled")]
    Cancelled,
    /// Canonical fixed-point score discount was not representable.
    #[error("score discount could not represent term width {term_bytes} for prefix {prefix_bytes}")]
    ScoreDiscount {
        /// Matched term width.
        term_bytes: usize,
        /// Prefix width.
        prefix_bytes: usize,
    },
    /// Temporary directory name allocation was exhausted.
    #[error("temporary segment directory names exhausted")]
    TemporaryNameExhausted,
    /// The sidecar ended before its declared fields.
    #[error("segment sidecar is truncated")]
    Truncated,
    /// A bounded count or conversion overflowed.
    #[error("segment document count overflowed")]
    CountOverflow,
    /// Sentinel-plus-hex encoding would exceed Tantivy's term bound.
    #[error(
        "canonical term has {raw_bytes} raw bytes and expands to {encoded_bytes}, limit is {max_encoded_bytes}"
    )]
    TermTooLong {
        /// Raw canonical term width.
        raw_bytes: usize,
        /// Expanded encoded width.
        encoded_bytes: usize,
        /// Maximum encoded width.
        max_encoded_bytes: usize,
    },
}

impl TantivySegmentStoreError {
    pub(crate) const fn rebuildable(&self) -> bool {
        matches!(
            self,
            Self::Corrupt { .. }
                | Self::IdentityMismatch { .. }
                | Self::MalformedDocument { .. }
                | Self::Truncated
        )
    }
}
