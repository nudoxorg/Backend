//! Defines exact behavior for `server-index-retrieval`, whose purpose is to compose exact, lexical, graph, and vector retrieval under one snapshot authority.
//! This module owns the exact invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Exact manifest construction and terminal classification.

use server_index_core::{
    ExactDegradation, ExactManifest, ExactOperation, ExactResolution, ExactSegment, ExactTerminal,
};
use server_index_vocabulary::ExactSegmentId;

use crate::{
    CancellationCause, RetrievalAbsence, RetrievalBoundary, RetrievalCoverage,
    RetrievalDegradation, RetrievalFailure, RetrievalOperationTerminal, RetrievalResult,
};

/// Health provenance supplied by the runtime exact acquisition route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactRoute {
    /// Every reachable exact segment arrived through a healthy route.
    Healthy,
    /// The route degraded without changing the sealed snapshot.
    Degraded(ExactDegradation),
}

impl<PayloadOwner> RetrievalBoundary<'_, '_, '_, '_, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Constructs and executes an exact manifest from the sealed snapshot selection.
    ///
    /// Cancellation is sampled before manifest construction; a pre-cancelled call leaves
    /// `output` unchanged. All other terminals retain the concrete core manifest failure or the
    /// exact selected identities absent from the operation.
    pub fn exact<'coverage, 'output, 'bytes>(
        &self,
        route: ExactRoute,
        segments: &'coverage [ExactSegment<'bytes>],
        missing: &'coverage [ExactSegmentId],
        operation: ExactOperation<'_>,
        output: &'output mut Option<ExactResolution<'bytes>>,
    ) -> RetrievalOperationTerminal<'coverage, 'output, 'bytes> {
        let snapshot = self.published.snapshot;
        if self.cancellation.is_cancelled() {
            return RetrievalOperationTerminal::Cancelled {
                snapshot: snapshot.id,
                cause: CancellationCause::Preflight,
            };
        }
        let manifest = match route {
            ExactRoute::Healthy => ExactManifest::new(snapshot, segments, missing),
            ExactRoute::Degraded(reason) => {
                ExactManifest::new_degraded(snapshot, segments, missing, reason)
            }
        };
        let manifest = match manifest {
            Ok(manifest) => manifest,
            Err(cause) => {
                return RetrievalOperationTerminal::Failed {
                    snapshot: snapshot.id,
                    cause: RetrievalFailure::ExactManifest(cause),
                };
            }
        };
        classify_exact(manifest.execute(operation), output)
    }
}

const fn classify_exact<'coverage, 'output, 'bytes>(
    terminal: ExactTerminal<'coverage, 'bytes>,
    output: &'output mut Option<ExactResolution<'bytes>>,
) -> RetrievalOperationTerminal<'coverage, 'output, 'bytes> {
    match terminal {
        ExactTerminal::Complete {
            snapshot,
            resolution,
        } => {
            *output = Some(resolution);
            RetrievalOperationTerminal::Complete {
                snapshot,
                result: RetrievalResult::Exact(resolution),
            }
        }
        ExactTerminal::Partial {
            snapshot,
            resolution,
            missing,
        } => {
            *output = Some(resolution);
            RetrievalOperationTerminal::Partial {
                snapshot,
                result: RetrievalResult::Exact(resolution),
                absence: RetrievalAbsence::Exact(missing),
            }
        }
        ExactTerminal::Degraded {
            snapshot,
            resolution,
            missing,
            reason,
        } => {
            *output = Some(resolution);
            RetrievalOperationTerminal::Degraded {
                snapshot,
                result: RetrievalResult::Exact(resolution),
                coverage: if missing.is_empty() {
                    RetrievalCoverage::Complete
                } else {
                    RetrievalCoverage::Missing(RetrievalAbsence::Exact(missing))
                },
                degradation: RetrievalDegradation::Exact(reason),
            }
        }
    }
}
