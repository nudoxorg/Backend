//! Qdrant authority, exact vector-coverage, and terminal classification.

use nudox_index_graph_vector::{
    MAX_PARTITIONS, MissingPartitions, MissingPartitionsError, PartitionId, VectorAuthority,
    VectorSegmentDescriptor,
};
use nudox_index_qdrant::{QdrantBlockingAdapter, QdrantCandidate, QueryCandidateCount};

use crate::{
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
            Ok(missing) => missing,
            Err(cause) => {
                return RetrievalOperationTerminal::Failed {
                    snapshot: snapshot.id,
                    cause: map_selection_error(cause),
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
    snapshot: nudox_index_vocab::IndexSnapshotId,
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

#[allow(
    clippy::result_large_err,
    clippy::too_many_lines,
    reason = "validating the exact bounded reachable subset must retain full typed authorities and descriptors"
)]
fn vector_missing(
    authority: VectorAuthority,
    pinned: &[VectorSegmentDescriptor],
    reachable: &[VectorSegmentDescriptor],
) -> Result<Option<MissingPartitions>, ReachableVectorError> {
    if reachable.len() > MAX_PARTITIONS {
        return Err(ReachableVectorError::Capacity {
            maximum: MAX_PARTITIONS,
            observed: reachable.len(),
        });
    }
    let mut reachable_partitions = [PartitionId::new(0); MAX_PARTITIONS];
    let mut previous_position = None;
    for (position, descriptor) in reachable.iter().copied().enumerate() {
        if descriptor.authority != authority {
            return Err(ReachableVectorError::Authority {
                position,
                expected: authority,
                observed: descriptor.authority,
            });
        }
        let Some(selected_position) = pinned.iter().position(|selected| *selected == descriptor)
        else {
            return Err(ReachableVectorError::Unselected {
                position,
                descriptor,
            });
        };
        if let Some(preceding_position) = previous_position
            && selected_position <= preceding_position
        {
            return Err(ReachableVectorError::Order {
                position,
                preceding_position,
                selected_position,
                descriptor,
            });
        }
        previous_position = Some(selected_position);
        let Some(destination) = reachable_partitions.get_mut(position) else {
            return Err(ReachableVectorError::Capacity {
                maximum: MAX_PARTITIONS,
                observed: reachable.len(),
            });
        };
        *destination = descriptor.partition;
    }
    let mut pinned_partitions = [PartitionId::new(0); MAX_PARTITIONS];
    if pinned.len() > MAX_PARTITIONS {
        return Err(ReachableVectorError::Capacity {
            maximum: MAX_PARTITIONS,
            observed: pinned.len(),
        });
    }
    for (descriptor, destination) in pinned.iter().copied().zip(&mut pinned_partitions) {
        *destination = descriptor.partition;
    }
    let Some(pinned) = pinned_partitions.get(..pinned.len()) else {
        return Err(ReachableVectorError::Capacity {
            maximum: MAX_PARTITIONS,
            observed: pinned.len(),
        });
    };
    let Some(reachable) = reachable_partitions.get(..reachable.len()) else {
        return Err(ReachableVectorError::Capacity {
            maximum: MAX_PARTITIONS,
            observed: reachable.len(),
        });
    };
    MissingPartitions::from_selected_reachable(pinned, reachable)
        .map_err(ReachableVectorError::Coverage)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReachableVectorError {
    Authority {
        position: usize,
        expected: VectorAuthority,
        observed: VectorAuthority,
    },
    Unselected {
        position: usize,
        descriptor: VectorSegmentDescriptor,
    },
    Order {
        position: usize,
        preceding_position: usize,
        selected_position: usize,
        descriptor: VectorSegmentDescriptor,
    },
    Capacity {
        maximum: usize,
        observed: usize,
    },
    Coverage(MissingPartitionsError),
}

const fn map_selection_error(cause: ReachableVectorError) -> RetrievalFailure {
    match cause {
        ReachableVectorError::Authority {
            position,
            expected,
            observed,
        } => RetrievalFailure::VectorAuthority {
            expected,
            surface: VectorAuthoritySurface::QdrantReachable { position },
            observed,
        },
        ReachableVectorError::Unselected {
            position,
            descriptor,
        } => RetrievalFailure::UnpinnedVectorDescriptor {
            position,
            observed: descriptor,
        },
        ReachableVectorError::Order {
            position,
            preceding_position,
            selected_position,
            descriptor,
        } => RetrievalFailure::VectorSelectionOrder {
            position,
            preceding_position,
            selected_position,
            observed: descriptor,
        },
        ReachableVectorError::Capacity { maximum, observed } => {
            RetrievalFailure::VectorSelectionCapacity { maximum, observed }
        }
        ReachableVectorError::Coverage(cause) => RetrievalFailure::VectorCoverage(cause),
    }
}
