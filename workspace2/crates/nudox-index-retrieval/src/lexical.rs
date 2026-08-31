//! Lexical manifest construction and terminal classification.

use nudox_index_core::{
    LexicalDegradation, LexicalDocumentId, LexicalManifest, LexicalOperation, LexicalSegment,
    LexicalSnapshotHit, LexicalTerminal, LexicalTopK,
};
use nudox_index_vocab::LexicalSegmentId;

use crate::{
    CancellationCause, RetrievalAbsence, RetrievalBoundary, RetrievalCoverage,
    RetrievalDegradation, RetrievalFailure, RetrievalOperationTerminal, RetrievalResult,
};

/// Health provenance supplied by the runtime lexical acquisition route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexicalRoute {
    /// Every reachable lexical segment arrived through a healthy route.
    Healthy,
    /// The route degraded without changing the sealed snapshot.
    Degraded(LexicalDegradation),
}

impl<PayloadOwner> RetrievalBoundary<'_, '_, '_, '_, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Constructs and executes a lexical manifest from the sealed snapshot selection.
    ///
    /// Cancellation is sampled before manifest construction; a pre-cancelled call leaves both
    /// caller-owned `seen_documents` scratch and `output` untouched.
    pub fn lexical<'coverage, 'output, 'bytes>(
        &self,
        route: LexicalRoute,
        segments: &'coverage [LexicalSegment<'bytes>],
        missing: &'coverage [LexicalSegmentId],
        operation: LexicalOperation<'_>,
        top_k: LexicalTopK,
        seen_documents: &mut [Option<LexicalDocumentId>],
        output: &'output mut [LexicalSnapshotHit<'bytes>],
    ) -> RetrievalOperationTerminal<'coverage, 'output, 'bytes> {
        let snapshot = self.published.snapshot;
        if self.cancellation.is_cancelled() {
            return RetrievalOperationTerminal::Cancelled {
                snapshot: snapshot.id,
                cause: CancellationCause::Preflight,
            };
        }
        let manifest = match route {
            LexicalRoute::Healthy => LexicalManifest::new(snapshot, segments, missing),
            LexicalRoute::Degraded(reason) => {
                LexicalManifest::new_degraded(snapshot, segments, missing, reason)
            }
        };
        let manifest = match manifest {
            Ok(manifest) => manifest,
            Err(cause) => {
                return RetrievalOperationTerminal::Failed {
                    snapshot: snapshot.id,
                    cause: RetrievalFailure::LexicalManifest(cause),
                };
            }
        };
        match manifest.execute(operation, top_k, seen_documents, output) {
            Ok(LexicalTerminal::Complete { snapshot, hits }) => {
                RetrievalOperationTerminal::Complete {
                    snapshot,
                    result: RetrievalResult::Lexical(hits),
                }
            }
            Ok(LexicalTerminal::Partial {
                snapshot,
                hits,
                missing,
            }) => RetrievalOperationTerminal::Partial {
                snapshot,
                result: RetrievalResult::Lexical(hits),
                absence: RetrievalAbsence::Lexical(missing),
            },
            Ok(LexicalTerminal::Degraded {
                snapshot,
                hits,
                missing,
                reason,
            }) => RetrievalOperationTerminal::Degraded {
                snapshot,
                result: RetrievalResult::Lexical(hits),
                coverage: if missing.is_empty() {
                    RetrievalCoverage::Complete
                } else {
                    RetrievalCoverage::Missing(RetrievalAbsence::Lexical(missing))
                },
                degradation: RetrievalDegradation::Lexical(reason),
            },
            Err(cause) => RetrievalOperationTerminal::Failed {
                snapshot: snapshot.id,
                cause: RetrievalFailure::LexicalQuery(cause),
            },
        }
    }
}
