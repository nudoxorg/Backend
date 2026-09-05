//! Canonical immutable index-snapshot identity grammar shared by portable clients and servers.

use crate::{
    ContentHasher, ContentId, FixedCanonicalRecord, GenerationId, IndexExactSegmentDomain,
    IndexLexicalSegmentDomain, IndexSnapshotDomain, IndexSnapshotId,
};

struct CanonicalRecord<const BYTES: usize>([u8; BYTES]);

impl<const BYTES: usize> FixedCanonicalRecord<BYTES> for CanonicalRecord<BYTES> {
    fn canonical_bytes(&self) -> &[u8; BYTES] {
        &self.0
    }
}

/// Exact failure while encoding a canonical snapshot lane count.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IndexSnapshotIdentityError {
    /// The exact segment count does not fit the fixed 64-bit wire cell.
    #[error("exact index snapshot lane count exceeds its fixed wire width")]
    ExactCountOverflow {
        /// Observed host-sized exact lane length.
        observed: usize,
    },
    /// The lexical segment count does not fit the fixed 64-bit wire cell.
    #[error("lexical index snapshot lane count exceeds its fixed wire width")]
    LexicalCountOverflow {
        /// Observed host-sized lexical lane length.
        observed: usize,
    },
}

/// Derives the immutable snapshot identity from already-validated update-ordered segment lanes.
///
/// The caller owns duplicate, limit, and publication validation. This function owns only the
/// one cross-plane canonical grammar: `heart.index.snapshot.v2`, generation, exact count and
/// exact lane, then lexical count and lexical lane. Epochs, payload lengths, residence, and
/// transport metadata are deliberately excluded.
///
/// # Errors
///
/// Returns the exact lane whose host-sized length cannot enter the fixed 64-bit wire grammar.
pub fn derive_index_snapshot(
    generation: GenerationId,
    exact: &[ContentId<IndexExactSegmentDomain>],
    lexical: &[ContentId<IndexLexicalSegmentDomain>],
) -> Result<IndexSnapshotId, IndexSnapshotIdentityError> {
    let exact_count =
        u64::try_from(exact.len()).map_err(|_| IndexSnapshotIdentityError::ExactCountOverflow {
            observed: exact.len(),
        })?;
    let lexical_count = u64::try_from(lexical.len()).map_err(|_| {
        IndexSnapshotIdentityError::LexicalCountOverflow {
            observed: lexical.len(),
        }
    })?;

    let mut hasher = ContentHasher::<IndexSnapshotDomain>::new();
    hasher.write_record(&CanonicalRecord(*b"heart.index.snapshot.v2"));
    hasher.write_record(&CanonicalRecord(*generation));
    hasher.write_record(&CanonicalRecord(exact_count.to_le_bytes()));
    for identity in exact {
        hasher.write_record(&CanonicalRecord(**identity));
    }
    hasher.write_record(&CanonicalRecord(lexical_count.to_le_bytes()));
    for identity in lexical {
        hasher.write_record(&CanonicalRecord(**identity));
    }
    Ok(hasher.finalize())
}
