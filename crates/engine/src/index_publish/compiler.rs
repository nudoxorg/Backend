//! Defines compiler behavior for `backend-engine index_publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the compiler invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Compiler-owned publication handoff into immutable retrieval authority.

use core::ops::Deref;

use crate::publication::OpenedCompilation;
use crate::index_build::PreparedIndex;
use backend_semantic::index_core::{ExactSegmentId, IndexSnapshot, IndexSnapshotError, LexicalSegmentId};

use crate::index_publish::PublishedIndexSnapshotView;

/// Caller-owned segment-identity regions for one compiler-index seal.
pub struct CompilationIndexScratch<'selection> {
    /// Exact segment identities, overwritten and canonically ordered by [`seal_compilation_index`].
    pub exact: &'selection mut [ExactSegmentId],
    /// Lexical segment identities, overwritten and canonically ordered by [`seal_compilation_index`].
    pub lexical: &'selection mut [LexicalSegmentId],
}

/// A reopened compiler publication sealed to an immutable snapshot naming its generation.
pub struct OpenedCompilationSnapshot<'manifest, 'facts, 'indexes, 'bytes, 'selection> {
    view: PublishedIndexSnapshotView<'selection>,
    /// Fully verified compiler publication proof retained by this snapshot.
    pub opened: OpenedCompilation<'manifest, 'facts>,
    /// Complete prepared fragment indexes from which the snapshot was derived.
    pub indexes: &'indexes [PreparedIndex<'bytes>],
}

impl<'selection> Deref for OpenedCompilationSnapshot<'_, '_, '_, '_, 'selection> {
    type Target = PublishedIndexSnapshotView<'selection>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

/// Seals every fragment of one reopened compiler publication into an immutable snapshot.
///
/// The prepared indexes must follow canonical manifest order and cover every selected fragment.
/// Exact and lexical identities are copied into caller-owned scratch, independently sorted, and
/// validated by [`IndexSnapshot`]. No caller can inject an unrelated segment identity.
///
/// # Errors
///
/// Returns the consumed opened publication with the exact admission, authority, or snapshot cause.
#[allow(
    clippy::result_large_err,
    reason = "linear rejection returns the consumed opened compiler proof without allocation"
)]
pub fn seal_compilation_index<'manifest, 'facts, 'indexes, 'bytes, 'selection>(
    opened: OpenedCompilation<'manifest, 'facts>,
    indexes: &'indexes [PreparedIndex<'bytes>],
    scratch: CompilationIndexScratch<'selection>,
) -> Result<
    OpenedCompilationSnapshot<'manifest, 'facts, 'indexes, 'bytes, 'selection>,
    RejectedCompilationIndex<'manifest, 'facts>,
> {
    let SelectedIdentityScratch { exact, lexical } = match admit(&opened, indexes, scratch) {
        Ok(selected) => selected,
        Err(error) => return Err(rejected(opened, error)),
    };
    for ((exact, lexical), prepared) in exact.iter_mut().zip(lexical.iter_mut()).zip(indexes) {
        *exact = prepared.exact.id;
        *lexical = prepared.lexical.id;
    }
    exact.sort_unstable();
    lexical.sort_unstable();
    let snapshot =
        match IndexSnapshot::new(opened.publication.generation.pinned_root, exact, lexical) {
            Ok(snapshot) => snapshot,
            Err(source) => {
                return Err(rejected(opened, CompilationIndexError::Snapshot { source }));
            }
        };
    Ok(OpenedCompilationSnapshot {
        view: PublishedIndexSnapshotView {
            publication: opened.publication,
            snapshot,
        },
        opened,
        indexes,
    })
}

const fn rejected<'manifest, 'facts>(
    opened: OpenedCompilation<'manifest, 'facts>,
    error: CompilationIndexError,
) -> RejectedCompilationIndex<'manifest, 'facts> {
    RejectedCompilationIndex { error, opened }
}

struct SelectedIdentityScratch<'selection> {
    exact: &'selection mut [ExactSegmentId],
    lexical: &'selection mut [LexicalSegmentId],
}

fn admit<'selection>(
    opened: &OpenedCompilation<'_, '_>,
    indexes: &[PreparedIndex<'_>],
    scratch: CompilationIndexScratch<'selection>,
) -> Result<SelectedIdentityScratch<'selection>, CompilationIndexError> {
    let fragment_count = opened.manifest.fragment_count;
    let expected = usize::try_from(fragment_count).map_err(|source| {
        CompilationIndexError::FragmentCountAddressSpace {
            observed: fragment_count,
            source,
        }
    })?;
    if indexes.len() != expected {
        return Err(CompilationIndexError::PreparedCount {
            expected,
            observed: indexes.len(),
        });
    }
    let CompilationIndexScratch { exact, lexical } = scratch;
    let exact_available = exact.len();
    let exact = exact
        .get_mut(..expected)
        .ok_or(CompilationIndexError::ExactScratch {
            required: expected,
            available: exact_available,
        })?;
    let lexical_available = lexical.len();
    let lexical = lexical
        .get_mut(..expected)
        .ok_or(CompilationIndexError::LexicalScratch {
            required: expected,
            available: lexical_available,
        })?;
    for (ordinal, (manifest, prepared)) in opened.manifest.fragments().zip(indexes).enumerate() {
        if manifest.fragment != prepared.fragment.fragment {
            return Err(CompilationIndexError::FragmentAuthority {
                ordinal,
                expected: manifest.fragment,
                observed: prepared.fragment.fragment,
            });
        }
    }
    Ok(SelectedIdentityScratch { exact, lexical })
}

/// Exact compiler-index seal rejection.
#[derive(Debug, thiserror::Error)]
pub enum CompilationIndexError {
    /// The persisted fragment count cannot address this target.
    #[error("compiler fragment count cannot address this target")]
    FragmentCountAddressSpace {
        /// Complete persisted fragment count.
        observed: u32,
        /// Exact integer conversion cause.
        #[source]
        source: core::num::TryFromIntError,
    },
    /// Prepared indexes do not cover exactly the manifest-selected fragments.
    #[error("prepared index count differs from compiler manifest")]
    PreparedCount {
        /// Required manifest fragment count.
        expected: usize,
        /// Complete supplied prepared-index count.
        observed: usize,
    },
    /// Caller exact-identity scratch is too small.
    #[error("exact segment identity scratch is too small")]
    ExactScratch {
        /// Required identity slots.
        required: usize,
        /// Supplied identity slots.
        available: usize,
    },
    /// Caller lexical-identity scratch is too small.
    #[error("lexical segment identity scratch is too small")]
    LexicalScratch {
        /// Required identity slots.
        required: usize,
        /// Supplied identity slots.
        available: usize,
    },
    /// A prepared index belongs to a different immutable fragment.
    #[error("prepared index fragment authority differs from compiler manifest")]
    FragmentAuthority {
        /// Canonical manifest position.
        ordinal: usize,
        /// Manifest-selected immutable fragment.
        expected: crate::publication::immutable::FragmentIdentity,
        /// Immutable fragment actually indexed.
        observed: crate::publication::immutable::FragmentIdentity,
    },
    /// Canonical segment identities did not form an immutable snapshot.
    #[error("compiler-derived segment identities did not form an index snapshot")]
    Snapshot {
        /// Exact snapshot invariant cause.
        #[source]
        source: IndexSnapshotError,
    },
}

/// Failed compiler-index seal retaining the consumed opened compiler proof.
pub struct RejectedCompilationIndex<'manifest, 'facts> {
    /// Exact typed rejection.
    pub error: CompilationIndexError,
    /// Original reopened compiler publication.
    pub opened: OpenedCompilation<'manifest, 'facts>,
}
