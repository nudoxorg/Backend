//! Opaque authority for one immutable selection of exact and lexical segments.

use nudox_id::{ContentHasher, FixedCanonicalRecord, IndexSnapshotDomain};
use nudox_index_vocab::{ExactSegmentId, IndexSnapshotId, LexicalSegmentId};

use crate::MAX_SELECTED_SEGMENTS;

struct CanonicalRecord<const BYTES: usize>([u8; BYTES]);

impl<const BYTES: usize> FixedCanonicalRecord<BYTES> for CanonicalRecord<BYTES> {
    fn canonical_bytes(&self) -> &[u8; BYTES] {
        &self.0
    }
}

/// A validated borrowed selection whose identity is derived from every selected segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexSnapshot<'selection> {
    id: IndexSnapshotId,
    exact: &'selection [ExactSegmentId],
    lexical: &'selection [LexicalSegmentId],
}

impl<'selection> IndexSnapshot<'selection> {
    /// Validates bounded, unique segment selections before deriving the snapshot identity.
    pub fn new(
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
        for (left_position, left) in exact.iter().enumerate() {
            if let Some((offset, _)) = exact
                .iter()
                .enumerate()
                .skip(left_position + 1)
                .find(|(_, right)| *left == **right)
            {
                return Err(IndexSnapshotError::DuplicateExactSegment {
                    left_position,
                    right_position: offset,
                    id: *left,
                });
            }
        }
        for (left_position, left) in lexical.iter().enumerate() {
            if let Some((offset, _)) = lexical
                .iter()
                .enumerate()
                .skip(left_position + 1)
                .find(|(_, right)| *left == **right)
            {
                return Err(IndexSnapshotError::DuplicateLexicalSegment {
                    left_position,
                    right_position: offset,
                    id: *left,
                });
            }
        }

        let mut hasher = ContentHasher::<IndexSnapshotDomain>::new();
        hasher.write_record(&CanonicalRecord(*b"nudox.index.snapshot.v1"));
        hasher.write_record(&CanonicalRecord((exact.len() as u64).to_le_bytes()));
        for id in exact {
            hasher.write_record(&CanonicalRecord(**id));
        }
        hasher.write_record(&CanonicalRecord((lexical.len() as u64).to_le_bytes()));
        for id in lexical {
            hasher.write_record(&CanonicalRecord(**id));
        }

        Ok(Self {
            id: hasher.finalize(),
            exact,
            lexical,
        })
    }

    /// Returns the content-derived immutable snapshot identity.
    #[must_use]
    pub const fn id(self) -> IndexSnapshotId {
        self.id
    }

    pub(crate) const fn exact(self) -> &'selection [ExactSegmentId] {
        self.exact
    }

    pub(crate) const fn lexical(self) -> &'selection [LexicalSegmentId] {
        self.lexical
    }
}

/// Rejection while deriving an immutable snapshot selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexSnapshotError {
    /// Too many exact segments were selected.
    ExactSegmentLimit {
        /// Maximum admitted exact segments.
        limit: usize,
        /// Complete observed selection width.
        observed: usize,
    },
    /// Too many lexical segments were selected.
    LexicalSegmentLimit {
        /// Maximum admitted lexical segments.
        limit: usize,
        /// Complete observed selection width.
        observed: usize,
    },
    /// One exact identity occupied two selected positions.
    DuplicateExactSegment {
        /// Earlier selected position.
        left_position: usize,
        /// Later selected position.
        right_position: usize,
        /// Repeated exact segment identity.
        id: ExactSegmentId,
    },
    /// One lexical identity occupied two selected positions.
    DuplicateLexicalSegment {
        /// Earlier selected position.
        left_position: usize,
        /// Later selected position.
        right_position: usize,
        /// Repeated lexical segment identity.
        id: LexicalSegmentId,
    },
}
