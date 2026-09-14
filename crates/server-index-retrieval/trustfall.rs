//! Defines trustfall behavior for `server-index-retrieval`, whose purpose is to compose exact, lexical, graph, and vector retrieval under one snapshot authority.
//! This module owns the trustfall invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Bounded graph-acquisition composition and synchronous Trustfall classification.

use backend_semantic::ir::EntityId;
use server_index_graph_vector::{
    GraphAuthority, GraphDegradation, GraphTerminal, MissingPartitions, ValidatedGraphView,
};
use backend_extension_trustfall::server::{TrustfallGraph, TrustfallHit, TrustfallTerminal};

use crate::{
    CancellationCause, RetrievalAbsence, RetrievalBoundary, RetrievalCoverage,
    RetrievalDegradation, RetrievalFailure, RetrievalOperationTerminal, RetrievalResult,
};

impl<PayloadOwner> RetrievalBoundary<'_, '_, '_, '_, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Runs Trustfall only after a bounded graph acquisition terminal and borrowed view agree.
    ///
    /// Cancellation sampled before this boundary leaves `output` untouched. A graph-acquisition
    /// cancellation or failure also leaves it untouched and never constructs Trustfall. Complete,
    /// partial, and degraded acquisition facts are instead classified around one synchronous
    /// Trustfall execution over the already validated borrowed view.
    pub fn trustfall<'output>(
        &self,
        acquisition: GraphTerminal,
        view: &ValidatedGraphView<'_>,
        source: EntityId,
        output: &'output mut [Option<TrustfallHit>],
    ) -> RetrievalOperationTerminal<'static, 'output, 'static> {
        let snapshot = self.published.snapshot;
        if self.cancellation.is_cancelled() {
            return RetrievalOperationTerminal::Cancelled {
                snapshot: snapshot.id,
                cause: CancellationCause::Preflight,
            };
        }
        let (authority, classification) = match graph_admission(self.graph_authority, acquisition) {
            GraphAdmission::Ready {
                authority,
                classification,
            } => (authority, classification),
            GraphAdmission::Cancelled => {
                return RetrievalOperationTerminal::Cancelled {
                    snapshot: snapshot.id,
                    cause: CancellationCause::GraphAcquisition,
                };
            }
            GraphAdmission::Failed(cause) => {
                return RetrievalOperationTerminal::Failed {
                    snapshot: snapshot.id,
                    cause,
                };
            }
        };
        if view.authority != authority {
            return RetrievalOperationTerminal::Failed {
                snapshot: snapshot.id,
                cause: RetrievalFailure::GraphTerminalAuthority {
                    expected: authority,
                    observed: view.authority,
                },
            };
        }
        let graph = TrustfallGraph::new(view);
        let result = match graph.neighbors(source, output) {
            Ok(result) => result,
            Err(cause) => {
                return RetrievalOperationTerminal::Failed {
                    snapshot: snapshot.id,
                    cause: RetrievalFailure::Trustfall(cause),
                };
            }
        };
        classify_graph_result(result, classification)
    }
}

fn graph_admission(expected: GraphAuthority, terminal: GraphTerminal) -> GraphAdmission {
    let authority = graph_terminal_authority(terminal);
    if authority != expected {
        return GraphAdmission::Failed(RetrievalFailure::PinnedGraphAuthority {
            expected,
            observed: authority,
        });
    }
    match terminal {
        GraphTerminal::Cancelled { .. } => GraphAdmission::Cancelled,
        GraphTerminal::Failed { cause, .. } => {
            GraphAdmission::Failed(RetrievalFailure::GraphStream(cause))
        }
        GraphTerminal::Complete { .. } => GraphAdmission::Ready {
            authority,
            classification: GraphSuccess::Complete,
        },
        GraphTerminal::Partial { missing, .. } => GraphAdmission::Ready {
            authority,
            classification: GraphSuccess::Partial(missing),
        },
        GraphTerminal::Degraded { reason, .. } => GraphAdmission::Ready {
            authority,
            classification: GraphSuccess::Degraded(reason),
        },
        GraphTerminal::DegradedPartial {
            missing, reason, ..
        } => GraphAdmission::Ready {
            authority,
            classification: GraphSuccess::DegradedPartial { missing, reason },
        },
    }
}

const fn classify_graph_result<'output>(
    result: TrustfallTerminal,
    classification: GraphSuccess,
) -> RetrievalOperationTerminal<'static, 'output, 'static> {
    match classification {
        GraphSuccess::Complete => RetrievalOperationTerminal::Complete {
            snapshot: result.authority.snapshot,
            result: RetrievalResult::Trustfall(result),
        },
        GraphSuccess::Partial(missing) => RetrievalOperationTerminal::Partial {
            snapshot: result.authority.snapshot,
            result: RetrievalResult::Trustfall(result),
            absence: RetrievalAbsence::Partitions(missing),
        },
        GraphSuccess::Degraded(reason) => RetrievalOperationTerminal::Degraded {
            snapshot: result.authority.snapshot,
            result: RetrievalResult::Trustfall(result),
            coverage: RetrievalCoverage::Complete,
            degradation: RetrievalDegradation::Graph(reason),
        },
        GraphSuccess::DegradedPartial { missing, reason } => RetrievalOperationTerminal::Degraded {
            snapshot: result.authority.snapshot,
            result: RetrievalResult::Trustfall(result),
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Partitions(missing)),
            degradation: RetrievalDegradation::Graph(reason),
        },
    }
}

/// Graph acquisition classifications that admit one synchronous Trustfall execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GraphSuccess {
    Complete,
    Partial(MissingPartitions),
    Degraded(GraphDegradation),
    DegradedPartial {
        missing: MissingPartitions,
        reason: GraphDegradation,
    },
}

/// Graph acquisition terminal classification before synchronous adapter construction.
#[derive(Debug)]
enum GraphAdmission {
    Ready {
        authority: GraphAuthority,
        classification: GraphSuccess,
    },
    Cancelled,
    Failed(RetrievalFailure),
}

const fn graph_terminal_authority(terminal: GraphTerminal) -> GraphAuthority {
    match terminal {
        GraphTerminal::Complete { authority }
        | GraphTerminal::Cancelled { authority }
        | GraphTerminal::Partial { authority, .. }
        | GraphTerminal::Degraded { authority, .. }
        | GraphTerminal::DegradedPartial { authority, .. }
        | GraphTerminal::Failed { authority, .. } => authority,
    }
}
