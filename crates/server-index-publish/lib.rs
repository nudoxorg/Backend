//! The `server-index-publish` crate exists to seal index segments into durable, reopenable snapshot packs.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Typed handoff from durable canonical publication to an immutable index snapshot.

use core::ops::Deref;

use backend_version::ObjectDomain;
use backend_semantic::index_core::IndexSnapshot;
use backend_store::journal::{PublicationFacts, PublishedGeneration};

mod compiler;
mod pack;

pub use compiler::{
    CompilationIndexError, CompilationIndexScratch, OpenedCompilationSnapshot,
    RejectedCompilationIndex, seal_compilation_index,
};
pub use pack::{
    ExactPackRow, ExactPackSegment, ExactPackValue, IndexPack, IndexPackCleanup, IndexPackConflict,
    IndexPackEncodeError, IndexPackFacts, IndexPackLane, IndexPackOpenError, IndexPackPathRole,
    IndexPackPlan, IndexPackPlanFacts, IndexPackRegion, IndexPackRowInvariant, IndexPackStore,
    IndexPackStoreError, IndexPackStorePhase, IndexPackView, LexicalPackRow, LexicalPackSegment,
    LexicalPackValue, RejectedIndexPack, StoredIndexPack, encode_index_pack, plan_index_pack,
};

/// Immutable public facts of a snapshot tied to one durable canonical publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedIndexSnapshotView<'selection> {
    /// Stable journal, immutable fact, and compact head identities.
    pub publication: PublicationFacts,
    /// Generation-bound exact and lexical snapshot authority.
    pub snapshot: IndexSnapshot<'selection>,
}

/// A durable publication witness sealed to the exact generation named by an index snapshot.
pub struct PublishedIndexSnapshot<'store, 'selection, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    view: PublishedIndexSnapshotView<'selection>,
    _published: PublishedGeneration<'store, ObjectDomain, PayloadOwner>,
}

impl<'selection, PayloadOwner> Deref for PublishedIndexSnapshot<'_, 'selection, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    type Target = PublishedIndexSnapshotView<'selection>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'store, 'selection, PayloadOwner> PublishedIndexSnapshot<'store, 'selection, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Consumes a durable publication witness only when the snapshot names its generation.
    ///
    /// A mismatch returns both owners unchanged so the caller can diagnose or retry without
    /// reconstructing publication or projection state.
    ///
    /// # Errors
    ///
    /// Returns [`RejectedPublishedIndexSnapshot`] when the snapshot names a generation other than
    /// the durable publication. The rejection retains both consumed values without allocation.
    #[allow(
        clippy::result_large_err,
        reason = "linear rejection returns both large consumed owners unchanged without a heap box"
    )]
    pub fn seal(
        published: PublishedGeneration<'store, ObjectDomain, PayloadOwner>,
        snapshot: IndexSnapshot<'selection>,
    ) -> Result<Self, RejectedPublishedIndexSnapshot<'store, 'selection, PayloadOwner>> {
        if published.pinned_root != snapshot.generation {
            return Err(RejectedPublishedIndexSnapshot {
                error: PublishedIndexSnapshotError::GenerationMismatch {
                    published: published.pinned_root,
                    snapshot: snapshot.generation,
                },
                published,
                snapshot,
            });
        }
        Ok(Self {
            view: PublishedIndexSnapshotView {
                publication: published.publication,
                snapshot,
            },
            _published: published,
        })
    }
}

/// Typed reason a durable publication and immutable snapshot could not be sealed together.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PublishedIndexSnapshotError {
    /// The snapshot was derived from a different canonical generation.
    #[error("index snapshot generation differs from the durable publication")]
    GenerationMismatch {
        /// Generation retained by the durable published witness.
        published: backend_version::GenerationId,
        /// Generation encoded into the rejected snapshot identity.
        snapshot: backend_version::GenerationId,
    },
}

/// Failed seal returning the exact durable witness and snapshot unchanged.
pub struct RejectedPublishedIndexSnapshot<'store, 'selection, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Exact typed mismatch.
    pub error: PublishedIndexSnapshotError,
    /// Original durable publication witness.
    pub published: PublishedGeneration<'store, ObjectDomain, PayloadOwner>,
    /// Original immutable snapshot.
    pub snapshot: IndexSnapshot<'selection>,
}
