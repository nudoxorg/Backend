//! Closed runtime operation vocabulary and retained typed causes.

use nudox_index_core::{
    ExactDegradation, ExactManifestError, ExactResolution, LexicalDegradation,
    LexicalManifestError, LexicalQueryError, LexicalSnapshotHit,
};
use nudox_index_graph_vector::{
    GraphAuthority, GraphDegradation, MissingPartitions, StreamCapacityError, VectorAuthority,
};
use nudox_index_qdrant::{QdrantError, QueryCandidateCount};
use nudox_index_tantivy::{TantivyAdapterError, TantivyTerminal};
use nudox_index_trustfall::{TrustfallGraphError, TrustfallTerminal};
use nudox_index_vocab::{ExactSegmentId, IndexSnapshotId, LexicalSegmentId};

/// Exact immutable coverage absent from an otherwise authoritative operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalAbsence<'coverage> {
    /// Exact segment identities selected by the sealed snapshot but not reachable.
    Exact(&'coverage [ExactSegmentId]),
    /// Lexical segment identities selected by the sealed snapshot but not reachable.
    Lexical(&'coverage [LexicalSegmentId]),
    /// Graph/vector partitions selected by the sealed authority but not reachable.
    Partitions(MissingPartitions),
}

/// Exact immutable coverage retained by a terminal that may have degraded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalCoverage<'coverage> {
    /// Every selected immutable fact was reachable.
    Complete,
    /// Exact selected immutable facts that were not reachable.
    Missing(RetrievalAbsence<'coverage>),
}

/// Typed provenance for an answer that retained snapshot authority through a degraded route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalDegradation {
    /// The exact route degraded while retaining the sealed snapshot.
    Exact(ExactDegradation),
    /// The lexical route degraded while retaining the sealed snapshot.
    Lexical(LexicalDegradation),
    /// The graph acquisition degraded while retaining the sealed snapshot.
    Graph(GraphDegradation),
}

/// The vector surface whose snapshot authority failed before an adapter request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorAuthoritySurface {
    /// The blocking Qdrant adapter's immutable configuration.
    QdrantAdapter,
    /// A vector segment descriptor selected for a Qdrant request.
    QdrantSelection {
        /// Position in the caller's bounded selection.
        position: usize,
    },
}

/// Concrete point at which cancellation became terminal for one retrieval operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationCause {
    /// Cancellation was observed before manifest, adapter, or transport work began.
    Preflight,
    /// The bounded asynchronous graph acquisition reached its cancelled terminal.
    GraphAcquisition,
}

/// Closed typed causes for failed runtime operations.
#[derive(Debug, thiserror::Error)]
pub enum RetrievalFailure {
    /// Exact manifest admission rejected the supplied segments before a lookup.
    #[error("exact manifest admission failed")]
    ExactManifest(ExactManifestError),
    /// Lexical manifest admission rejected the supplied segments before a query.
    #[error("lexical manifest admission failed")]
    LexicalManifest(LexicalManifestError),
    /// Lexical query admission rejected caller-owned scratch or output.
    #[error("lexical query admission failed")]
    LexicalQuery(LexicalQueryError),
    /// Tantivy rejected its immutable projection or query.
    #[error("Tantivy adapter failed")]
    Tantivy(#[source] TantivyAdapterError),
    /// Trustfall rejected its static operation or caller output.
    #[error("Trustfall adapter failed")]
    Trustfall(#[source] TrustfallGraphError),
    /// Qdrant rejected transport, collection, payload, or query facts.
    #[error("Qdrant adapter failed")]
    Qdrant(#[source] QdrantError),
    /// The bounded asynchronous graph acquisition failed before Trustfall construction.
    #[error("graph acquisition failed")]
    GraphStream(StreamCapacityError),
    /// A validated graph view belongs to another immutable snapshot.
    #[error("graph view snapshot authority differs from the sealed publication")]
    GraphAuthority {
        /// Snapshot pinned by the durable publication witness.
        expected: IndexSnapshotId,
        /// Complete graph authority observed before constructing Trustfall.
        observed: GraphAuthority,
    },
    /// The graph view does not belong to the acquisition terminal that named it.
    #[error("graph view authority differs from graph acquisition authority")]
    GraphTerminalAuthority {
        /// Authority retained by the bounded graph acquisition terminal.
        expected: GraphAuthority,
        /// Authority carried by the validated graph view passed to Trustfall.
        observed: GraphAuthority,
    },
    /// A Qdrant configuration or descriptor belongs to another immutable snapshot.
    #[error("Qdrant vector snapshot authority differs from the sealed publication")]
    VectorAuthority {
        /// Snapshot pinned by the durable publication witness.
        expected: IndexSnapshotId,
        /// Blocking adapter or selection surface rejected before transport.
        surface: VectorAuthoritySurface,
        /// Complete vector authority observed before Qdrant query admission.
        observed: VectorAuthority,
    },
}

/// Successful query result facts, with result storage remaining caller-owned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RetrievalResult<'output, 'bytes> {
    /// Exact result copied into the caller's exact output slot.
    Exact(ExactResolution<'bytes>),
    /// Caller-owned deterministic lexical hits initialized by the core query.
    Lexical(&'output [LexicalSnapshotHit<'bytes>]),
    /// Tantivy completed its disposable lexical projection.
    Tantivy(TantivyTerminal),
    /// Trustfall completed its synchronous borrowed graph projection.
    Trustfall(TrustfallTerminal),
    /// Qdrant initialized this many caller-owned candidate slots.
    Qdrant(QueryCandidateCount),
}

/// The sole server-operation terminal vocabulary for retrieval adapters.
///
/// Cancellation is reported only when it was observed before any adapter or manifest work began;
/// it never represents an attempted interruption of an already-running synchronous operation.
#[derive(Debug)]
pub enum RetrievalOperationTerminal<'coverage, 'output, 'bytes> {
    /// Every selected immutable fact was reachable through a healthy route.
    Complete {
        /// Snapshot authority sealed to the durable publication witness.
        snapshot: IndexSnapshotId,
        /// Typed result retaining caller-owned output only where needed.
        result: RetrievalResult<'output, 'bytes>,
    },
    /// Exact immutable coverage was absent but retained in original selection order.
    Partial {
        /// Snapshot authority sealed to the durable publication witness.
        snapshot: IndexSnapshotId,
        /// Result from reachable immutable facts.
        result: RetrievalResult<'output, 'bytes>,
        /// Exact selected immutable coverage that was not reached.
        absence: RetrievalAbsence<'coverage>,
    },
    /// The answer retained sealed authority through a typed degraded route.
    Degraded {
        /// Snapshot authority sealed to the durable publication witness.
        snapshot: IndexSnapshotId,
        /// Result from reachable immutable facts.
        result: RetrievalResult<'output, 'bytes>,
        /// Complete or exactly missing immutable coverage.
        coverage: RetrievalCoverage<'coverage>,
        /// Provenance of the retained-authority degradation.
        degradation: RetrievalDegradation,
    },
    /// Cancellation won before manifest construction, backend construction, or transport.
    Cancelled {
        /// Snapshot authority that would otherwise have been queried.
        snapshot: IndexSnapshotId,
        /// Concrete boundary at which cancellation became terminal.
        cause: CancellationCause,
    },
    /// No answer was produced; the concrete typed cause is retained.
    Failed {
        /// Snapshot authority requested through the durable publication witness.
        snapshot: IndexSnapshotId,
        /// Closed typed failure cause.
        cause: RetrievalFailure,
    },
}
