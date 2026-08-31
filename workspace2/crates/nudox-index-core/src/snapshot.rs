//! Opaque authority for one immutable selection of exact and lexical segments.

use core::ops::Deref;

use nudox_id::{ContentHasher, FixedCanonicalRecord, GenerationId, IndexSnapshotDomain};
use nudox_index_vocab::{ExactSegmentId, IndexSnapshotId, LexicalSegmentId};

use crate::MAX_SELECTED_SEGMENTS;

struct CanonicalRecord<const BYTES: usize>([u8; BYTES]);

impl<const BYTES: usize> FixedCanonicalRecord<BYTES> for CanonicalRecord<BYTES> {
    fn canonical_bytes(&self) -> &[u8; BYTES] {
        &self.0
    }
}

// The admitted width is eight: this performs at most 28 comparisons and retains no scratch owner.
fn duplicate_positions<SegmentId: Eq>(segments: &[SegmentId]) -> Option<(usize, usize)> {
    segments
        .iter()
        .enumerate()
        .find_map(|(left_position, left)| {
            segments[left_position + 1..]
                .iter()
                .position(|right| left == right)
                .map(|right_offset| (left_position, left_position + right_offset + 1))
        })
}

/// Immutable public facts of one validated borrowed segment selection.
///
/// A view constructed directly is descriptive data, not a snapshot authority.  Only
/// [`IndexSnapshot::new`] creates the validated authority wrapper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexSnapshotView<'selection> {
    /// Canonical generation whose published facts these projections describe.
    pub generation: GenerationId,
    /// Content identity derived from the generation and both selected segment lanes.
    pub id: IndexSnapshotId,
    /// Selected exact segments in their immutable update order.
    pub exact: &'selection [ExactSegmentId],
    /// Selected lexical segments in their immutable update order.
    pub lexical: &'selection [LexicalSegmentId],
}

/// A validated borrowed selection whose identity binds its generation and every selected segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct IndexSnapshot<'selection>(IndexSnapshotView<'selection>);

impl<'selection> Deref for IndexSnapshot<'selection> {
    type Target = IndexSnapshotView<'selection>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'selection> IndexSnapshot<'selection> {
    /// Validates bounded, unique segment selections before deriving the snapshot identity.
    pub fn new(
        generation: GenerationId,
        exact: &'selection [ExactSegmentId],
        lexical: &'selection [LexicalSegmentId],
    ) -> Result<Self, IndexSnapshotError> {
        if exact.len() > MAX_SELECTED_SEGMENTS {
            return Err(IndexSnapshotError::ExactSegmentLimit {
                limit: MAX_SELECTED_SEGMENTS,
                observed: exact.len(),
            });
        }
        if lexical.len() > MAX_SELECTED_SEGMENTS {
            return Err(IndexSnapshotError::LexicalSegmentLimit {
                limit: MAX_SELECTED_SEGMENTS,
                observed: lexical.len(),
            });
        }
        if let Some((left_position, right_position)) = duplicate_positions(exact) {
            return Err(IndexSnapshotError::DuplicateExactSegment {
                left_position,
                right_position,
                id: exact[left_position],
            });
        }
        if let Some((left_position, right_position)) = duplicate_positions(lexical) {
            return Err(IndexSnapshotError::DuplicateLexicalSegment {
                left_position,
                right_position,
                id: lexical[left_position],
            });
        }

        let mut hasher = ContentHasher::<IndexSnapshotDomain>::new();
        hasher.write_record(&CanonicalRecord(*b"nudox.index.snapshot.v2"));
        hasher.write_record(&CanonicalRecord(*generation));
        hasher.write_record(&CanonicalRecord((exact.len() as u64).to_le_bytes()));
        for id in exact {
            hasher.write_record(&CanonicalRecord(**id));
        }
        hasher.write_record(&CanonicalRecord((lexical.len() as u64).to_le_bytes()));
        for id in lexical {
            hasher.write_record(&CanonicalRecord(**id));
        }

        Ok(Self(IndexSnapshotView {
            generation,
            id: hasher.finalize(),
            exact,
            lexical,
        }))
    }
}

/// Rejection while deriving an immutable snapshot selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IndexSnapshotError {
    /// Too many exact segments were selected.
    #[error("exact segment selection exceeds the immutable snapshot limit")]
    ExactSegmentLimit {
        /// Maximum admitted exact segments.
        limit: usize,
        /// Complete observed selection width.
        observed: usize,
    },
    /// Too many lexical segments were selected.
    #[error("lexical segment selection exceeds the immutable snapshot limit")]
    LexicalSegmentLimit {
        /// Maximum admitted lexical segments.
        limit: usize,
        /// Complete observed selection width.
        observed: usize,
    },
    /// One exact identity occupied two selected positions.
    #[error("exact segment identity is duplicated in the immutable snapshot")]
    DuplicateExactSegment {
        /// Earlier selected position.
        left_position: usize,
        /// Later selected position.
        right_position: usize,
        /// Repeated exact segment identity.
        id: ExactSegmentId,
    },
    /// One lexical identity occupied two selected positions.
    #[error("lexical segment identity is duplicated in the immutable snapshot")]
    DuplicateLexicalSegment {
        /// Earlier selected position.
        left_position: usize,
        /// Later selected position.
        right_position: usize,
        /// Repeated lexical segment identity.
        id: LexicalSegmentId,
    },
}
