//! Validated server-boundary authority and borrowed fixed vector selection.

use core::ops::Deref;

use nudox_index_graph_vector::{
    Cancellation, GraphAuthority, MAX_PARTITIONS, PartitionId, VectorAuthority,
    VectorSegmentDescriptor,
};
use nudox_index_publish::PublishedIndexSnapshot;
use nudox_index_vocab::{IndexSnapshotId, VectorSegmentId};

/// Immutable derived projection facts visible through a validated retrieval boundary.
///
/// Constructing this descriptive view directly does not prove that its facts belong to a sealed
/// durable publication. [`RetrievalBoundary`] validates and owns that correlation, then exposes
/// this view only through immutable [`Deref`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetrievalBoundaryView<'vector> {
    /// Full graph projection authority pinned by the retrieval boundary.
    pub graph_authority: GraphAuthority,
    /// Full vector model, dimension, metric, and snapshot authority pinned by the boundary.
    pub vector_authority: VectorAuthority,
    /// Exact bounded vector descriptor selection borrowed for the boundary lifetime.
    pub vector_selection: &'vector [VectorSegmentDescriptor],
}

/// Typed rejection while validating a durable retrieval boundary's derived authorities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RetrievalBoundaryError {
    /// The pinned graph authority named another sealed snapshot.
    #[error("graph authority snapshot differs from the sealed publication")]
    GraphSnapshot {
        /// Snapshot sealed by the durable publication witness.
        expected: IndexSnapshotId,
        /// Complete supplied graph authority.
        observed: GraphAuthority,
    },
    /// The pinned vector authority named another sealed snapshot.
    #[error("vector authority snapshot differs from the sealed publication")]
    VectorSnapshot {
        /// Snapshot sealed by the durable publication witness.
        expected: IndexSnapshotId,
        /// Complete supplied vector authority.
        observed: VectorAuthority,
    },
    /// The selected vector descriptor count exceeded the fixed server boundary.
    #[error("vector selection has {observed} descriptors, limit is {maximum}")]
    VectorSelectionCapacity {
        /// Fixed descriptor capacity.
        maximum: usize,
        /// Complete caller selection width.
        observed: usize,
    },
    /// A selected descriptor belonged to another full vector authority.
    #[error("vector selection descriptor {position} differs from pinned authority")]
    VectorSelectionAuthority {
        /// Selection position.
        position: usize,
        /// Full vector authority pinned by the boundary.
        expected: VectorAuthority,
        /// Complete descriptor authority.
        observed: VectorAuthority,
    },
    /// One immutable vector descriptor was selected twice.
    #[error("vector selection repeats descriptor {id:?}")]
    DuplicateVectorDescriptor {
        /// Earlier selection position.
        first_position: usize,
        /// Repeated selection position.
        position: usize,
        /// Repeated immutable descriptor identity.
        id: VectorSegmentId,
    },
    /// Two selected descriptors named one partition, making partition absence ambiguous.
    #[error("vector selection repeats partition {partition:?}")]
    DuplicateVectorPartition {
        /// Earlier selection position.
        first_position: usize,
        /// Repeated selection position.
        position: usize,
        /// Repeated partition coordinate.
        partition: PartitionId,
    },
}

/// Runtime retrieval boundary pinned to one durable publication and derived projection facts.
///
/// The private publication witness prevents a copyable descriptive snapshot view from entering
/// this server boundary without its owning durable publication proof.
pub struct RetrievalBoundary<'boundary, 'store, 'selection, 'vector, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    view: RetrievalBoundaryView<'vector>,
    pub(super) published: &'boundary PublishedIndexSnapshot<'store, 'selection, PayloadOwner>,
    pub(super) cancellation: &'boundary Cancellation,
}

impl<'vector, PayloadOwner> Deref for RetrievalBoundary<'_, '_, '_, 'vector, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    type Target = RetrievalBoundaryView<'vector>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'boundary, 'store, 'selection, 'vector, PayloadOwner>
    RetrievalBoundary<'boundary, 'store, 'selection, 'vector, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Validates and binds derived graph/vector projection facts to a sealed durable snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`RetrievalBoundaryError`] when an authority names another sealed snapshot or the
    /// borrowed vector selection is oversized, ambiguous, or belongs to another full authority.
    #[allow(
        clippy::result_large_err,
        reason = "the closed error retains full graph/vector authorities instead of erasing or boxing evidence"
    )]
    pub fn new(
        published: &'boundary PublishedIndexSnapshot<'store, 'selection, PayloadOwner>,
        cancellation: &'boundary Cancellation,
        graph_authority: GraphAuthority,
        vector_authority: VectorAuthority,
        vector_selection: &'vector [VectorSegmentDescriptor],
    ) -> Result<Self, RetrievalBoundaryError> {
        let snapshot = published.snapshot.id;
        if graph_authority.snapshot != snapshot {
            return Err(RetrievalBoundaryError::GraphSnapshot {
                expected: snapshot,
                observed: graph_authority,
            });
        }
        if vector_authority.snapshot != snapshot {
            return Err(RetrievalBoundaryError::VectorSnapshot {
                expected: snapshot,
                observed: vector_authority,
            });
        }
        if vector_selection.len() > MAX_PARTITIONS {
            return Err(RetrievalBoundaryError::VectorSelectionCapacity {
                maximum: MAX_PARTITIONS,
                observed: vector_selection.len(),
            });
        }
        for (position, descriptor) in vector_selection.iter().copied().enumerate() {
            if descriptor.authority != vector_authority {
                return Err(RetrievalBoundaryError::VectorSelectionAuthority {
                    position,
                    expected: vector_authority,
                    observed: descriptor.authority,
                });
            }
            for (first_position, first) in
                vector_selection.iter().take(position).copied().enumerate()
            {
                if first.id == descriptor.id {
                    return Err(RetrievalBoundaryError::DuplicateVectorDescriptor {
                        first_position,
                        position,
                        id: descriptor.id,
                    });
                }
                if first.partition == descriptor.partition {
                    return Err(RetrievalBoundaryError::DuplicateVectorPartition {
                        first_position,
                        position,
                        partition: descriptor.partition,
                    });
                }
            }
        }
        Ok(Self {
            view: RetrievalBoundaryView {
                graph_authority,
                vector_authority,
                vector_selection,
            },
            published,
            cancellation,
        })
    }
}
