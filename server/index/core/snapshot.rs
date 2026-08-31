//! Defines snapshot behavior for `server-index-core`, whose purpose is to define immutable index documents, segments, and snapshot identities.
//! This module owns the snapshot invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Opaque authority for one immutable selection of exact and lexical segments.

use core::ops::Deref;

use heart_identity::{
    ContentHasher, FixedCanonicalRecord, GenerationId, HASH_BYTES, IndexSnapshotDomain,
};
use server_index_vocabulary::{ExactSegmentId, IndexSnapshotId, LexicalSegmentId};

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
        hasher.write_record(&CanonicalRecord(*b"heart.index.snapshot.v2"));
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

    /// Derives the existing snapshot identity from fixed-capacity canonical directory slots.
    ///
    /// Durable readers use this when parsed entries must remain `Option`-backed until every
    /// directory position is proved present. It shares the same hash grammar as [`Self::new`]
    /// without fabricating placeholder segment identities for unused capacity.
    pub fn canonical_identity_from_slots<const CAPACITY: usize>(
        generation: GenerationId,
        exact: &[Option<ExactSegmentId>; CAPACITY],
        exact_count: usize,
        lexical: &[Option<LexicalSegmentId>; CAPACITY],
        lexical_count: usize,
    ) -> Result<IndexSnapshotId, IndexSnapshotError> {
        if exact_count > MAX_SELECTED_SEGMENTS {
            return Err(IndexSnapshotError::ExactSegmentLimit {
                limit: MAX_SELECTED_SEGMENTS,
                observed: exact_count,
            });
        }
        if lexical_count > MAX_SELECTED_SEGMENTS {
            return Err(IndexSnapshotError::LexicalSegmentLimit {
                limit: MAX_SELECTED_SEGMENTS,
                observed: lexical_count,
            });
        }
        let exact = exact
            .get(..exact_count)
            .ok_or(IndexSnapshotError::CanonicalExactSlots {
                required: exact_count,
                available: CAPACITY,
            })?;
        let lexical =
            lexical
                .get(..lexical_count)
                .ok_or(IndexSnapshotError::CanonicalLexicalSlots {
                    required: lexical_count,
                    available: CAPACITY,
                })?;
        let mut hasher = ContentHasher::<IndexSnapshotDomain>::new();
        hasher.write_record(&CanonicalRecord(*b"heart.index.snapshot.v2"));
        hasher.write_record(&CanonicalRecord(*generation));
        hash_canonical_lane(&mut hasher, exact, IndexSnapshotLane::Exact)?;
        hash_canonical_lane(&mut hasher, lexical, IndexSnapshotLane::Lexical)?;
        Ok(hasher.finalize())
    }
}

fn hash_canonical_lane<SegmentId>(
    hasher: &mut ContentHasher<IndexSnapshotDomain>,
    slots: &[Option<SegmentId>],
    lane: IndexSnapshotLane,
) -> Result<(), IndexSnapshotError>
where
    SegmentId: CanonicalSnapshotId,
{
    hasher.write_record(&CanonicalRecord((slots.len() as u64).to_le_bytes()));
    let mut previous = None;
    for (ordinal, slot) in slots.iter().copied().enumerate() {
        let id = slot.ok_or(IndexSnapshotError::CanonicalSlotMissing { lane, ordinal })?;
        if let Some(previous) = previous
            && previous >= id
        {
            return Err(IndexSnapshotError::CanonicalOrder { lane, ordinal });
        }
        hasher.write_record(&CanonicalRecord(id.canonical_bytes()));
        previous = Some(id);
    }
    Ok(())
}

trait CanonicalSnapshotId: Copy + Ord {
    fn canonical_bytes(self) -> [u8; HASH_BYTES];
}

impl CanonicalSnapshotId for ExactSegmentId {
    fn canonical_bytes(self) -> [u8; HASH_BYTES] {
        *self
    }
}

impl CanonicalSnapshotId for LexicalSegmentId {
    fn canonical_bytes(self) -> [u8; HASH_BYTES] {
        *self
    }
}

/// One immutable snapshot directory lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexSnapshotLane {
    /// Exact-key segment identities.
    Exact,
    /// Lexical term/document segment identities.
    Lexical,
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
    /// Fixed-capacity exact slots could not cover the declared canonical prefix.
    #[error("canonical exact slots cannot cover the declared prefix")]
    CanonicalExactSlots {
        /// Declared canonical exact prefix width.
        required: usize,
        /// Available fixed exact slot capacity.
        available: usize,
    },
    /// Fixed-capacity lexical slots could not cover the declared canonical prefix.
    #[error("canonical lexical slots cannot cover the declared prefix")]
    CanonicalLexicalSlots {
        /// Declared canonical lexical prefix width.
        required: usize,
        /// Available fixed lexical slot capacity.
        available: usize,
    },
    /// A declared canonical directory position had not been proved present.
    #[error("canonical snapshot slot was not proved present")]
    CanonicalSlotMissing {
        /// Lane containing the absent fixed slot.
        lane: IndexSnapshotLane,
        /// Declared canonical position without an identity.
        ordinal: usize,
    },
    /// Canonical directory identities were not strictly ordered.
    #[error("canonical snapshot identities were not strictly ordered")]
    CanonicalOrder {
        /// Lane containing the out-of-order identity.
        lane: IndexSnapshotLane,
        /// Later noncanonical identity position.
        ordinal: usize,
    },
}
