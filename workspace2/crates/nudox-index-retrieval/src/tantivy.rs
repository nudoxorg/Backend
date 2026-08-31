//! Tantivy preflight and terminal classification.

use nudox_index_tantivy::{TantivyHit, TantivyLexical};

use crate::{
    CancellationCause, RetrievalBoundary, RetrievalFailure, RetrievalOperationTerminal,
    RetrievalResult,
};

impl<PayloadOwner> RetrievalBoundary<'_, '_, '_, '_, PayloadOwner>
where
    PayloadOwner: AsRef<[u8]>,
{
    /// Searches a disposable Tantivy projection after sealed-snapshot preflight.
    ///
    /// A pre-cancelled call leaves `output` untouched. A cancellation observed after the single
    /// preflight sample does not relabel this blocking call as interrupted.
    pub fn tantivy<'output>(
        &self,
        adapter: &TantivyLexical,
        query: &str,
        requested_limit: usize,
        output: &'output mut [Option<TantivyHit>],
    ) -> RetrievalOperationTerminal<'static, 'output, 'static> {
        let snapshot = self.published.snapshot;
        if self.cancellation.is_cancelled() {
            return RetrievalOperationTerminal::Cancelled {
                snapshot: snapshot.id,
                cause: CancellationCause::Preflight,
            };
        }
        match adapter.search(snapshot.id, query, requested_limit, output) {
            Ok(result) => RetrievalOperationTerminal::Complete {
                snapshot: result.snapshot,
                result: RetrievalResult::Tantivy(result),
            },
            Err(cause) => RetrievalOperationTerminal::Failed {
                snapshot: snapshot.id,
                cause: RetrievalFailure::Tantivy(cause),
            },
        }
    }
}
