//! Demonstrates `heart-root` examples capacity-worksheet capacity output composition through public APIs.
//! The example keeps all authority and resource inputs visible to its caller.
//! It doubles as executable documentation for the smallest complete journey.
//! Derived deployment-plan facts separated from workload input facts and arithmetic.

use serde::Serialize;

use super::input::{Bytes, Count, EmbeddingProvider, Tier};

/// The derived worker and memory counts that a deployment planner can act on.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct CapacityPlan {
    /// Input tier echoed for machine-readable recommendation grouping.
    pub(crate) tier: Tier,
    /// CPU cores admitted by the compile target before worker packing.
    pub(crate) compiler_cores: Count,
    /// Vertically scaled compiler workers required for the workload.
    pub(crate) compiler_workers: Count,
    /// Replicated immutable durable bytes across all index copies.
    pub(crate) replicated_index_bytes: Bytes,
    /// Index nodes required by durable capacity alone.
    pub(crate) index_nodes_by_storage: Count,
    /// Index nodes required to keep the hot working set resident.
    pub(crate) index_nodes_by_memory: Count,
    /// Index nodes required by query slot occupancy alone.
    pub(crate) index_nodes_by_query: Count,
    /// Index nodes required to distribute replicas over fault domains.
    pub(crate) index_nodes_by_failure_domain: Count,
    /// Horizontal search-node count: maximum of each distinct constraint.
    pub(crate) index_nodes: Count,
    /// Quantized model-weight bytes; tokenizer/model-sidecar bytes are intentionally separate.
    pub(crate) embedding_weight_bytes: Bytes,
    /// One raw output-vector footprint before index/graph/payload overhead.
    pub(crate) embedding_vector_bytes_per_item: Bytes,
    /// Output embedding dimensionality retained in the index projection.
    pub(crate) embedding_output_dimensions: Count,
    /// One worker's planned model + workspace + maximum-batch memory footprint.
    pub(crate) embedding_worker_gpu_bytes: Bytes,
    /// Whether a single configured accelerator can admit one model and configured batch.
    pub(crate) embedding_worker_fits_gpu: bool,
    /// GPU/accelerator workers required by measured embedding throughput.
    pub(crate) embedding_workers: Count,
    /// Execution provider whose exact model/batch measurement sized the workers.
    pub(crate) embedding_provider: EmbeddingProvider,
}
