//! Qdrant authority preflight and terminal classification.

use nudox_index_graph_vector::VectorSegmentDescriptor;
use nudox_index_qdrant::{QdrantBlockingAdapter, QdrantCandidate};

use crate::{
    CancellationCause, RetrievalBoundary, RetrievalFailure, RetrievalOperationTerminal,
    RetrievalResult, VectorAuthoritySurface,
};

impl<PayloadOwner> RetrievalBoundary<'_, '_, '_, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Queries Qdrant only after its adapter and every descriptor match the sealed snapshot.
    ///
    /// These authority checks run before Qdrant query admission or transport. A pre-cancelled
    /// call leaves `output` untouched. This is a synchronous boundary and therefore never reports
    /// cancellation that arrives after the sole preflight sample.
    pub fn qdrant<'output>(
        &self,
        adapter: &QdrantBlockingAdapter,
        selected: &[VectorSegmentDescriptor],
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
        if config.authority.snapshot != snapshot.id {
            return RetrievalOperationTerminal::Failed {
                snapshot: snapshot.id,
                cause: RetrievalFailure::VectorAuthority {
                    expected: snapshot.id,
                    surface: VectorAuthoritySurface::QdrantAdapter,
                    observed: config.authority,
                },
            };
        }
        for (position, descriptor) in selected.iter().enumerate() {
            if descriptor.authority.snapshot != snapshot.id {
                return RetrievalOperationTerminal::Failed {
                    snapshot: snapshot.id,
                    cause: RetrievalFailure::VectorAuthority {
                        expected: snapshot.id,
                        surface: VectorAuthoritySurface::QdrantSelection { position },
                        observed: descriptor.authority,
                    },
                };
            }
        }
        match adapter.query(selected, query_coordinates, requested_limit, output) {
            Ok(result) => RetrievalOperationTerminal::Complete {
                snapshot: snapshot.id,
                result: RetrievalResult::Qdrant(result),
            },
            Err(cause) => RetrievalOperationTerminal::Failed {
                snapshot: snapshot.id,
                cause: RetrievalFailure::Qdrant(cause),
            },
        }
    }
}
