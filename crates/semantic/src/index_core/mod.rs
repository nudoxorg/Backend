//! The `backend-semantic::index_core` module exists to define immutable index documents, segments, and snapshot identities.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Borrowed immutable exact and lexical index segments.
//!
//! The crate owns logical query semantics only.  It borrows canonical row lanes
//! and immutable manifests; storage, publication, and backend adapters remain
//! outside this portable core.

mod document;
mod exact;
mod exact_manifest;
mod lexical;
mod lexical_manifest;
mod selection;
mod snapshot;

pub use self::document::{
    ENTITY_DOCUMENT_ID_BYTES, EntityArtifactIdentity, EntityDocumentId, EntityDocumentIdError,
};

pub use self::exact::{
    ExactOperation, ExactRow, ExactSegment, ExactSegmentError, ExactSegmentVerifier,
    ExactSegmentView, MAX_EXACT_PAYLOAD_BYTES, MAX_EXACT_ROWS,
};
pub use self::exact_manifest::{
    ExactDegradation, ExactManifest, ExactManifestError, ExactManifestView, ExactResolution,
    ExactTerminal,
};
pub use self::lexical::{
    LexicalHit, LexicalMatch, LexicalOperation, LexicalOrderKey, LexicalOutputError, LexicalRow,
    LexicalRowValue, LexicalScore, LexicalSegment, LexicalSegmentError, LexicalSegmentVerifier,
    LexicalSegmentView, LexicalSnapshotHit, LexicalTopK, LexicalTopKError,
    MAX_LEXICAL_PAYLOAD_BYTES, MAX_LEXICAL_ROWS, MAX_LEXICAL_TOP_K,
};
pub use self::lexical_manifest::{
    LexicalDegradation, LexicalManifest, LexicalManifestError, LexicalManifestView,
    LexicalQueryError, LexicalTerminal,
};
pub use self::snapshot::{IndexSnapshot, IndexSnapshotError, IndexSnapshotLane, IndexSnapshotView};
pub use crate::index_vocabulary::{ExactSegmentId, IndexSnapshotId, LexicalSegmentId};
pub use backend_version::GenerationId;

/// Maximum exact or lexical segments a single borrowed manifest can select.
///
/// This is the multi-segment rollover budget: the real corpus fragments exceed
/// the old eight-segment wall, and each fragment's entities may roll over
/// across several segments. 255 is the largest count the immutable pack header
/// can encode in its one-byte lane cell, so it is the protocol ceiling rather
/// than an arbitrary product limit. The selection bound keeps the manifest
/// directory, match-range scratch, and packed directory bounded by one
/// `u8`-wide lane instead of growing without limit.
pub const MAX_SELECTED_SEGMENTS: usize = 255;
