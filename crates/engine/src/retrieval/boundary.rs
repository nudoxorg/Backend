//! Defines boundary behavior for `backend-engine retrieval`, whose purpose is to compose exact, lexical, graph, and vector retrieval under one snapshot authority.
//! This module owns the boundary invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Validated server-boundary authority and borrowed fixed vector selection.

use core::ops::Deref;

use backend_semantic::graph_vector::{
    Cancellation, GraphAuthority, MAX_PARTITIONS, PartitionId, VectorAuthority,
    VectorSegmentDescriptor,
};
use crate::index_publish::PublishedIndexSnapshot;
use backend_semantic::index_vocabulary::{IndexSnapshotId, VectorSegmentId};

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

/// Complete authority facts retained in one cold retrieval-boundary rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalBoundaryEvidenceView {
    /// A graph authority named another sealed snapshot.
    GraphSnapshot {
        /// Snapshot sealed by the durable publication witness.
        expected: IndexSnapshotId,
        /// Complete supplied graph authority.
        observed: GraphAuthority,
    },
    /// A vector authority named another sealed snapshot.
    VectorSnapshot {
        /// Snapshot sealed by the durable publication witness.
        expected: IndexSnapshotId,
        /// Complete supplied vector authority.
        observed: VectorAuthority,
    },
    /// A selected vector descriptor belonged to another full vector authority.
    VectorSelectionAuthority {
        /// Selection position.
        position: usize,
        /// Full vector authority pinned by the boundary.
        expected: VectorAuthority,
        /// Complete descriptor authority.
        observed: VectorAuthority,
    },
}

/// Cold heap-owned authority evidence behind one typed boundary-construction rejection.
///
/// Construction succeeds without allocation. This wrapper allocates exactly once only for the
/// three broad authority-rejection variants, preserving every authority field while keeping their
/// `Result` carrier compact. It dereferences to the immutable complete evidence view.
#[derive(Debug, Eq, PartialEq)]
pub struct RetrievalBoundaryEvidence(Box<RetrievalBoundaryEvidenceView>);

impl Deref for RetrievalBoundaryEvidence {
    type Target = RetrievalBoundaryEvidenceView;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl RetrievalBoundaryEvidence {
    fn graph_snapshot(expected: IndexSnapshotId, observed: GraphAuthority) -> Self {
        Self(Box::new(RetrievalBoundaryEvidenceView::GraphSnapshot {
            expected,
            observed,
        }))
    }

    fn vector_snapshot(expected: IndexSnapshotId, observed: VectorAuthority) -> Self {
        Self(Box::new(RetrievalBoundaryEvidenceView::VectorSnapshot {
            expected,
            observed,
        }))
    }

    fn vector_selection_authority(
        position: usize,
        expected: VectorAuthority,
        observed: VectorAuthority,
    ) -> Self {
        Self(Box::new(
            RetrievalBoundaryEvidenceView::VectorSelectionAuthority {
                position,
                expected,
                observed,
            },
        ))
    }
}

/// Typed rejection while validating a durable retrieval boundary's derived authorities.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum RetrievalBoundaryError {
    /// The pinned graph authority named another sealed snapshot.
    #[error("graph authority snapshot differs from the sealed publication")]
    GraphSnapshot(RetrievalBoundaryEvidence),
    /// The pinned vector authority named another sealed snapshot.
    #[error("vector authority snapshot differs from the sealed publication")]
    VectorSnapshot(RetrievalBoundaryEvidence),
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
        /// Selection position, repeated for direct error discrimination.
        position: usize,
        /// Cold complete expected/observed authority evidence.
        evidence: RetrievalBoundaryEvidence,
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
    pub fn new(
        published: &'boundary PublishedIndexSnapshot<'store, 'selection, PayloadOwner>,
        cancellation: &'boundary Cancellation,
        graph_authority: GraphAuthority,
        vector_authority: VectorAuthority,
        vector_selection: &'vector [VectorSegmentDescriptor],
    ) -> Result<Self, RetrievalBoundaryError> {
        let snapshot = published.snapshot.id;
        if graph_authority.snapshot != snapshot {
            return Err(RetrievalBoundaryError::GraphSnapshot(
                RetrievalBoundaryEvidence::graph_snapshot(snapshot, graph_authority),
            ));
        }
        if vector_authority.snapshot != snapshot {
            return Err(RetrievalBoundaryError::VectorSnapshot(
                RetrievalBoundaryEvidence::vector_snapshot(snapshot, vector_authority),
            ));
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
                    evidence: RetrievalBoundaryEvidence::vector_selection_authority(
                        position,
                        vector_authority,
                        descriptor.authority,
                    ),
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

#[cfg(test)]
mod tests {
    use core::{hint::black_box, mem::size_of};

    use allocation_counter::measure;

    use super::*;

    #[test]
    fn cold_boundary_authority_evidence_allocates_once_while_hot_result_stays_compact() {
        type Boundary = RetrievalBoundary<'static, 'static, 'static, 'static, Box<[u8]>>;

        let snapshot = IndexSnapshotId::from_canonical_bytes(b"boundary-cold-evidence");
        let authority =
            GraphAuthority::new(snapshot, backend_semantic::graph_vector::ProjectionId::new(1));
        let allocations = measure(|| {
            let evidence = black_box(RetrievalBoundaryEvidence::graph_snapshot(
                snapshot, authority,
            ));
            assert!(matches!(
                *evidence,
                RetrievalBoundaryEvidenceView::GraphSnapshot {
                    expected,
                    observed,
                } if expected == snapshot && observed == authority
            ));
        });
        assert_eq!(allocations.count_total, 1);
        assert_eq!(allocations.count_current, 0);
        assert_eq!(allocations.count_max, 1);
        assert!(matches!(
            u64::try_from(size_of::<RetrievalBoundaryEvidenceView>()),
            Ok(evidence_size) if allocations.bytes_total >= evidence_size
        ));
        assert_eq!(
            size_of::<Result<Boundary, RetrievalBoundaryError>>(),
            size_of::<Boundary>()
        );
    }
}
