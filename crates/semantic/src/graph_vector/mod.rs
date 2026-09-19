//! The `backend-semantic::graph_vector` module exists to execute typed graph and vector work through bounded leased storage.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Immutable graph/vector projections and their bounded leased edge.

mod authority;
mod graph;
#[allow(
    unsafe_code,
    reason = "the private lease cells publish Copy graph edges through reviewed atomic state transitions"
)]
mod lease;
mod trace;
mod vector;

pub use self::authority::{
    GraphAuthority, Metric, MissingPartitions, MissingPartitionsError, ModelId, PartitionId,
    ProjectionId, VectorAuthority,
};
pub use self::graph::{
    AdmissionError, GraphEdge, GraphHit, GraphQueryError, GraphQueryOutcome, GraphQueryTerminal,
    GraphRow, GraphView, ValidatedGraphView,
};
pub use self::lease::{
    Cancellation, EdgeBatchProducer, EdgeBatchStream, GraphDegradation, GraphLease,
    GraphStreamEvent, GraphTerminal, LeaseCapacity, LeaseLoad, LeaseStateCell, LeasedGraphBatch,
    StreamCapacityError,
};
pub use self::trace::{GraphTraceEvent, TraceProbe, TraceRecorder};
pub use self::vector::{
    MAX_VECTOR_DIMENSION, ValidatedVectorSegment, VectorFact, VectorHit, VectorPoint,
    VectorQueryError, VectorQueryOutcome, VectorQueryTerminal, VectorSegmentDescriptor,
    VectorSegmentError, VectorSegmentSelection, VectorSegmentView, VectorTerminal,
    compact_vector_facts, exact_vector_query,
};

/// Maximum admitted graph or vector partitions for one request.
pub const MAX_PARTITIONS: usize = 4;
