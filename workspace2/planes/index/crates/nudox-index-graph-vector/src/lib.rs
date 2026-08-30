#![forbid(unsafe_code)]
//! Immutable graph/vector projections and their bounded leased edge.

mod authority;
mod graph;
mod lease;
mod trace;
mod vector;

pub use authority::{GraphAuthority, Metric, ModelId, PartitionId, ProjectionId, VectorAuthority};
pub use graph::{
    AdmissionError, GraphEdge, GraphHit, GraphQueryError, GraphQueryOutcome, GraphQueryTerminal,
    GraphRow, TrustfallGraph,
};
pub use lease::{
    Cancellation, EdgeBatchStream, GraphStreamEvent, GraphTerminal, InsufficientOutput,
    LeasedGraphBatch, StreamCapacityError,
};
pub use trace::{GraphTraceEvent, TraceProbe, TraceRecorder};
pub use vector::{
    VectorFact, VectorHit, VectorQueryError, VectorQueryOutcome, VectorQueryTerminal, VectorRow,
    VectorTerminal, exact_vector_query,
};

/// Maximum admitted graph or vector partitions for one request.
pub const MAX_PARTITIONS: usize = 4;
