//! Defines vector behavior for `server-index-retrieval`, whose purpose is to compose exact, lexical, graph, and vector retrieval under one snapshot authority.
//! This module owns the vector-route invariants and typed terminal transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Local exact-scan retrieval retains caller-owned output and typed vector provenance.

use server_index_graph_vector::{
    MAX_PARTITIONS, PartitionId, ValidatedVectorSegment, VectorHit, VectorQueryOutcome,
    VectorQueryTerminal, exact_vector_query,
};

use crate::{
    CancellationCause, RetrievalAbsence, RetrievalBoundary, RetrievalCoverage,
    RetrievalDegradation, RetrievalFailure, RetrievalOperationTerminal, RetrievalResult,
    VectorRoute,
};

impl<PayloadOwner> RetrievalBoundary<'_, '_, '_, '_, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Executes the bounded scalar local vector oracle against the boundary's pinned selection.
    ///
    /// Analytic work is O(selected partitions × points × query dimension). Selection is copied to
    /// fixed stack storage only to adapt the borrowed boundary descriptors to the core query API.
    pub fn vector<'output>(
        &self,
        route: VectorRoute,
        segments: &[ValidatedVectorSegment<'_>],
        query: &[i16],
        requested_limit: usize,
        output: &'output mut [Option<VectorHit>],
    ) -> RetrievalOperationTerminal<'static, 'output, 'static> {
        let snapshot = self.published.snapshot.id;
        if self.cancellation.is_cancelled() {
            return RetrievalOperationTerminal::Cancelled {
                snapshot,
                cause: CancellationCause::Preflight,
            };
        }

        let mut selected = [PartitionId::new(0); MAX_PARTITIONS];
        for (slot, descriptor) in selected
            .iter_mut()
            .zip(self.vector_selection.iter().copied())
        {
            *slot = descriptor.partition;
        }
        let selected_len = self.vector_selection.len();
        let Some(selected) = selected.get(..selected_len) else {
            return RetrievalOperationTerminal::Failed {
                snapshot,
                cause: RetrievalFailure::VectorQuery(
                    server_index_graph_vector::VectorQueryError::TooManySelected {
                        maximum: MAX_PARTITIONS,
                        observed: selected_len,
                    },
                ),
            };
        };
        for (position, segment) in segments.iter().enumerate() {
            if !selected.contains(&segment.partition) {
                return RetrievalOperationTerminal::Failed {
                    snapshot,
                    cause: RetrievalFailure::UnpinnedVectorSegment {
                        position,
                        partition: segment.partition,
                    },
                };
            }
        }

        let outcome = match exact_vector_query(
            self.vector_authority,
            selected,
            segments,
            query,
            requested_limit,
            output,
        ) {
            Ok(outcome) => outcome,
            Err(cause) => {
                return RetrievalOperationTerminal::Failed {
                    snapshot,
                    cause: RetrievalFailure::VectorQuery(cause),
                };
            }
        };
        classify_vector(route, snapshot, outcome, output)
    }
}

const fn classify_vector(
    route: VectorRoute,
    snapshot: server_index_vocabulary::IndexSnapshotId,
    outcome: VectorQueryOutcome,
    output: &[Option<VectorHit>],
) -> RetrievalOperationTerminal<'static, '_, 'static> {
    let result = RetrievalResult::Vector {
        hits: output,
        written: outcome.written,
    };
    match (outcome.terminal, route) {
        (VectorQueryTerminal::Complete { .. }, VectorRoute::Healthy) => {
            RetrievalOperationTerminal::Complete { snapshot, result }
        }
        (VectorQueryTerminal::Complete { .. }, VectorRoute::Degraded(reason)) => {
            RetrievalOperationTerminal::Degraded {
                snapshot,
                result,
                coverage: RetrievalCoverage::Complete,
                degradation: RetrievalDegradation::Vector(reason),
            }
        }
        (VectorQueryTerminal::Partial { missing, .. }, VectorRoute::Healthy) => {
            RetrievalOperationTerminal::Partial {
                snapshot,
                result,
                absence: RetrievalAbsence::Partitions(missing),
            }
        }
        (VectorQueryTerminal::Partial { missing, .. }, VectorRoute::Degraded(reason)) => {
            RetrievalOperationTerminal::Degraded {
                snapshot,
                result,
                coverage: RetrievalCoverage::Missing(RetrievalAbsence::Partitions(missing)),
                degradation: RetrievalDegradation::Vector(reason),
            }
        }
    }
}
