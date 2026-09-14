//! Defines qdrant behavior for `backend-engine retrieval`, whose purpose is to compose exact, lexical, graph, and vector retrieval under one snapshot authority.
//! This module owns the qdrant invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Qdrant authority, exact vector-coverage, and terminal classification.

use backend_semantic::graph_vector::{
    MAX_PARTITIONS, MissingPartitions, MissingPartitionsError, PartitionId, VectorAuthority,
    VectorSegmentDescriptor,
};
use backend_extension_qdrant::server::{QdrantBlockingAdapter, QdrantCandidate, QueryCandidateCount};

use crate::retrieval::{
    CancellationCause, RetrievalAbsence, RetrievalBoundary, RetrievalCoverage,
    RetrievalDegradation, RetrievalFailure, RetrievalOperationTerminal, RetrievalResult,
    VectorAuthoritySurface, VectorRoute,
};

impl<PayloadOwner> RetrievalBoundary<'_, '_, '_, '_, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Queries Qdrant only within the boundary's exact pinned vector selection.
    ///
    /// `reachable` is a validated ordered subset of that selection. Its omitted partitions become
    /// an exact partial-coverage fact even when Qdrant has no transport work for an empty subset.
    /// A pre-cancelled call leaves `output` untouched; later cancellation is not claimed to
    /// interrupt Qdrant's blocking operation.
    pub fn qdrant<'output>(
        &self,
        route: VectorRoute,
        adapter: &QdrantBlockingAdapter,
        reachable: &[VectorSegmentDescriptor],
        query_coordinates: &[i16],
        requested_limit: usize,
        output: &'output mut [Option<QdrantCandidate>],
    ) -> RetrievalOperationTerminal<'static, 'output, 'static> {
        let snapshot = self.published.snapshot;
        if self.cancellation.is_cancelled() {
            return RetrievalOperationTerminal::Cancelled {
                snapshot: snapshot.id,
                cause: CancellationCause::Preflight,
            };
        }
        let config = adapter.config();
        if config.authority != self.vector_authority {
            return RetrievalOperationTerminal::Failed {
                snapshot: snapshot.id,
                cause: RetrievalFailure::VectorAuthority {
                    expected: self.vector_authority,
                    surface: VectorAuthoritySurface::QdrantAdapter,
                    observed: config.authority,
                },
            };
        }
        let missing = match vector_missing(self.vector_authority, self.vector_selection, reachable)
        {
            VectorCoverageTransition::Covered(missing) => missing,
            VectorCoverageTransition::Rejected(cause) => {
                return RetrievalOperationTerminal::Failed {
                    snapshot: snapshot.id,
                    cause: map_selection_error(self.vector_authority, cause),
                };
            }
        };
        match adapter.query(reachable, query_coordinates, requested_limit, output) {
            Ok(result) => classify_qdrant_result(snapshot.id, route, result, missing),
            Err(cause) => RetrievalOperationTerminal::Failed {
                snapshot: snapshot.id,
                cause: RetrievalFailure::Qdrant(cause),
            },
        }
    }
}

const fn classify_qdrant_result<'output>(
    snapshot: backend_semantic::index_vocabulary::IndexSnapshotId,
    route: VectorRoute,
    result: QueryCandidateCount,
    missing: Option<MissingPartitions>,
) -> RetrievalOperationTerminal<'static, 'output, 'static> {
    let result = RetrievalResult::Qdrant(result);
    match (route, missing) {
        (VectorRoute::Healthy, None) => RetrievalOperationTerminal::Complete { snapshot, result },
        (VectorRoute::Healthy, Some(missing)) => RetrievalOperationTerminal::Partial {
            snapshot,
            result,
            absence: RetrievalAbsence::Partitions(missing),
        },
        (VectorRoute::Degraded(reason), None) => RetrievalOperationTerminal::Degraded {
            snapshot,
            result,
            coverage: RetrievalCoverage::Complete,
            degradation: RetrievalDegradation::Vector(reason),
        },
        (VectorRoute::Degraded(reason), Some(missing)) => RetrievalOperationTerminal::Degraded {
            snapshot,
            result,
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Partitions(missing)),
            degradation: RetrievalDegradation::Vector(reason),
        },
    }
}

fn vector_missing<'descriptor>(
    authority: VectorAuthority,
    pinned: &[VectorSegmentDescriptor],
    reachable: &'descriptor [VectorSegmentDescriptor],
) -> VectorCoverageTransition<'descriptor> {
    match authority_transition(authority, pinned, reachable) {
        AuthorityTransition::Admitted => {}
        AuthorityTransition::Rejected(cause) => {
            return VectorCoverageTransition::Rejected(VectorSelectionRejection::Authority(cause));
        }
    }
    match order_transition(pinned, reachable) {
        OrderTransition::Ordered => {}
        OrderTransition::Rejected(cause) => {
            return VectorCoverageTransition::Rejected(VectorSelectionRejection::Order(cause));
        }
    }
    coverage_transition(pinned, reachable)
}

enum AuthorityTransition {
    Admitted,
    Rejected(AuthorityRejection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorityRejection {
    Capacity {
        maximum: usize,
        observed: usize,
    },
    Mismatch {
        position: usize,
        observed: VectorAuthority,
    },
}

fn authority_transition(
    authority: VectorAuthority,
    pinned: &[VectorSegmentDescriptor],
    reachable: &[VectorSegmentDescriptor],
) -> AuthorityTransition {
    if pinned.len() > MAX_PARTITIONS {
        return AuthorityTransition::Rejected(AuthorityRejection::Capacity {
            maximum: MAX_PARTITIONS,
            observed: pinned.len(),
        });
    }
    if reachable.len() > MAX_PARTITIONS {
        return AuthorityTransition::Rejected(AuthorityRejection::Capacity {
            maximum: MAX_PARTITIONS,
            observed: reachable.len(),
        });
    }
    for (position, descriptor) in reachable.iter().copied().enumerate() {
        if descriptor.authority != authority {
            return AuthorityTransition::Rejected(AuthorityRejection::Mismatch {
                position,
                observed: descriptor.authority,
            });
        }
    }
    AuthorityTransition::Admitted
}

enum OrderTransition<'descriptor> {
    Ordered,
    Rejected(OrderRejection<'descriptor>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderRejection<'descriptor> {
    Unselected {
        position: usize,
        observed: &'descriptor VectorSegmentDescriptor,
    },
    Reordered {
        position: usize,
        preceding_position: usize,
        selected_position: usize,
        observed: &'descriptor VectorSegmentDescriptor,
    },
}

fn order_transition<'descriptor>(
    pinned: &[VectorSegmentDescriptor],
    reachable: &'descriptor [VectorSegmentDescriptor],
) -> OrderTransition<'descriptor> {
    let mut previous_position = None;
    for (position, descriptor) in reachable.iter().enumerate() {
        let Some(selected_position) = pinned.iter().position(|selected| selected == descriptor)
        else {
            return OrderTransition::Rejected(OrderRejection::Unselected {
                position,
                observed: descriptor,
            });
        };
        if let Some(preceding_position) = previous_position
            && selected_position <= preceding_position
        {
            return OrderTransition::Rejected(OrderRejection::Reordered {
                position,
                preceding_position,
                selected_position,
                observed: descriptor,
            });
        }
        previous_position = Some(selected_position);
    }
    OrderTransition::Ordered
}

fn coverage_transition<'descriptor>(
    pinned: &[VectorSegmentDescriptor],
    reachable: &[VectorSegmentDescriptor],
) -> VectorCoverageTransition<'descriptor> {
    let mut pinned_partitions = [PartitionId::new(0); MAX_PARTITIONS];
    for (descriptor, destination) in pinned.iter().copied().zip(&mut pinned_partitions) {
        *destination = descriptor.partition;
    }
    let mut reachable_partitions = [PartitionId::new(0); MAX_PARTITIONS];
    for (descriptor, destination) in reachable.iter().copied().zip(&mut reachable_partitions) {
        *destination = descriptor.partition;
    }
    let Some(pinned) = pinned_partitions.get(..pinned.len()) else {
        return VectorCoverageTransition::Rejected(VectorSelectionRejection::Coverage(
            MissingPartitionsError::SelectedCapacity {
                maximum: MAX_PARTITIONS,
                observed: pinned.len(),
            },
        ));
    };
    let Some(reachable) = reachable_partitions.get(..reachable.len()) else {
        return VectorCoverageTransition::Rejected(VectorSelectionRejection::Coverage(
            MissingPartitionsError::ReachableCapacity {
                maximum: MAX_PARTITIONS,
                observed: reachable.len(),
            },
        ));
    };
    match MissingPartitions::from_selected_reachable(pinned, reachable) {
        Ok(missing) => VectorCoverageTransition::Covered(missing),
        Err(cause) => VectorCoverageTransition::Rejected(VectorSelectionRejection::Coverage(cause)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VectorSelectionRejection<'descriptor> {
    Authority(AuthorityRejection),
    Order(OrderRejection<'descriptor>),
    Coverage(MissingPartitionsError),
}

enum VectorCoverageTransition<'descriptor> {
    Covered(Option<MissingPartitions>),
    Rejected(VectorSelectionRejection<'descriptor>),
}

const fn map_selection_error(
    expected: VectorAuthority,
    cause: VectorSelectionRejection<'_>,
) -> RetrievalFailure {
    match cause {
        VectorSelectionRejection::Authority(AuthorityRejection::Mismatch {
            position,
            observed,
        }) => RetrievalFailure::VectorAuthority {
            expected,
            surface: VectorAuthoritySurface::QdrantReachable { position },
            observed,
        },
        VectorSelectionRejection::Authority(AuthorityRejection::Capacity { maximum, observed }) => {
            RetrievalFailure::VectorSelectionCapacity { maximum, observed }
        }
        VectorSelectionRejection::Order(OrderRejection::Unselected { position, observed }) => {
            RetrievalFailure::UnpinnedVectorDescriptor {
                position,
                observed: *observed,
            }
        }
        VectorSelectionRejection::Order(OrderRejection::Reordered {
            position,
            preceding_position,
            selected_position,
            observed,
        }) => RetrievalFailure::VectorSelectionOrder {
            position,
            preceding_position,
            selected_position,
            observed: *observed,
        },
        VectorSelectionRejection::Coverage(cause) => RetrievalFailure::VectorCoverage(cause),
    }
}
