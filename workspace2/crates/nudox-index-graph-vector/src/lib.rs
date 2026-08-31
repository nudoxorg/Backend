#![forbid(unsafe_code)]
//! Immutable graph/vector projections and their bounded leased edge.

mod authority;
mod graph;
mod lease;
mod trace;
mod vector;

pub use authority::{
    GraphAuthority, Metric, MissingPartitions, ModelId, PartitionId, ProjectionId, VectorAuthority,
};
pub use graph::{
    AdmissionError, GraphEdge, GraphHit, GraphQueryError, GraphQueryOutcome, GraphQueryTerminal,
    GraphRow, ValidatedGraphView,
};
pub use lease::{
    Cancellation, EdgeBatchProducer, EdgeBatchStream, GraphStreamEvent, GraphTerminal,
    InsufficientOutput, LeasedGraphBatch, StreamCapacityError,
};
pub use trace::{GraphTraceEvent, TraceProbe, TraceRecorder};
pub use vector::{
    ValidatedVectorSegment, VectorFact, VectorHit, VectorPoint, VectorQueryError,
    VectorQueryOutcome, VectorQueryTerminal, VectorSegmentError, VectorTerminal,
    compact_vector_facts, exact_vector_query,
};

/// Maximum admitted graph or vector partitions for one request.
pub const MAX_PARTITIONS: usize = 4;
