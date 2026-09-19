//! Demonstrates `backend-store` examples capacity-worksheet capacity input composition through public APIs.
//! The example keeps all authority and resource inputs visible to its caller.
//! It doubles as executable documentation for the smallest complete journey.
//! Typed, workload-driven capacity arithmetic for compiler, index, and embedding workers.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One explicit capacity worksheet; all headroom and workload assumptions are input facts.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CapacityInput {
    /// Named recommendation tier with a closed operational scope.
    pub(crate) tier: Tier,
    /// Vertically scaled native compiler workload.
    pub(crate) compiler: CompilerInput,
    /// Horizontally scaled immutable index workload.
    pub(crate) index: IndexInput,
    /// Optional local or service embedding workload.
    pub(crate) embedding: EmbeddingInput,
    /// Replica and topology constraints independent of throughput.
    pub(crate) failure_domains: FailureDomainInput,
}

/// Tier identity selected by the worksheet author.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Tier {
    /// One production footprint with a separate recovery copy.
    Starter,
    /// Multiple availability zones with horizontal index replicas.
    Growth,
    /// Multi-zone and multi-worker operational footprint.
    HorizontallyScaled,
}

/// Input facts for native compile admission.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CompilerInput {
    /// Sustained native compile requests per second at the selected percentile.
    pub(crate) requests_per_second: Count,
    /// Measured CPU milliseconds required by one representative request.
    pub(crate) cpu_millis_per_request: Millis,
    /// Physical CPU cores supplied by one compiler worker.
    pub(crate) cores_per_worker: Count,
    /// Maximum planned average CPU occupancy, in thousandths of one core.
    pub(crate) target_utilization_per_mille: PerMille,
}

/// Input facts for immutable exact, lexical, and vector index serving.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct IndexInput {
    /// Total immutable projection bytes before replication, including payload and segments.
    pub(crate) primary_durable_bytes: Bytes,
    /// Replicas that must exist as independent immutable copies.
    pub(crate) replica_count: Count,
    /// Durable bytes safely usable on one search node after OS and local-cache reservation.
    pub(crate) usable_durable_bytes_per_node: Bytes,
    /// Measured hot mmap/vector/postings working set that must be resident across the fleet.
    pub(crate) hot_working_set_bytes: Bytes,
    /// RAM safely usable for that working set on one node after OS and runtime reservation.
    pub(crate) usable_memory_bytes_per_node: Bytes,
    /// Sustained query requests per second.
    pub(crate) query_requests_per_second: Count,
    /// Measured query CPU milliseconds at the target percentile.
    pub(crate) cpu_millis_per_query: Millis,
    /// CPU-equivalent query slots intentionally admitted by one node.
    pub(crate) query_slots_per_node: Count,
    /// Maximum planned query-slot occupancy, in thousandths.
    pub(crate) target_utilization_per_mille: PerMille,
}

/// Input facts for measured embedding inference capacity and GPU memory admission.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct EmbeddingInput {
    /// Closed execution provider used to produce the measured throughput fact.
    pub(crate) provider: EmbeddingProvider,
    /// Text items requiring embedding per second, after deduplication/cache admission.
    pub(crate) items_per_second: Count,
    /// Sustained measured items per second from one GPU/accelerator worker at the chosen batch.
    pub(crate) measured_items_per_second_per_worker: Count,
    /// Model parameter count excluding tokenizer and runtime sidecars.
    pub(crate) model_parameters: Count,
    /// Output embedding dimension selected by the model contract.
    pub(crate) output_dimensions: Count,
    /// Quantized bits retained for one output-vector element in the index projection.
    pub(crate) output_element_bits: Count,
    /// Quantized weight bits per model parameter.
    pub(crate) weight_bits: Count,
    /// GPU memory usable by one accelerator worker after display/driver reservation.
    pub(crate) usable_gpu_memory: Bytes,
    /// Runtime workspace/reserved memory for one loaded execution provider.
    pub(crate) runtime_workspace: Bytes,
    /// Activation/scratch bytes for one batched item at the selected model shape.
    pub(crate) activation_bytes_per_item: Bytes,
    /// Maximum input batch admitted by this worker.
    pub(crate) batch_items: Count,
}

/// Execution provider selected by a measured embedding fixture.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EmbeddingProvider {
    /// Portable local CPU with runtime-selected SIMD kernels.
    CpuSimd,
    /// Apple OS-native Core ML/Metal path.
    AppleCoreMl,
    /// NVIDIA CUDA execution-provider sidecar.
    Cuda,
    /// Windows `DirectML` execution-provider sidecar.
    DirectMl,
    /// Portable ONNX Runtime CPU execution-provider sidecar.
    OnnxRuntimeCpu,
}

/// Placement rules that make a replica count meaningful.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FailureDomainInput {
    /// Independent zones/racks that must receive index replicas.
    pub(crate) independent_domains: Count,
    /// Maximum independent domain failures the service must tolerate.
    pub(crate) tolerated_domain_failures: Count,
}

/// A transparent non-negative count fact.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(crate) struct Count(pub(crate) u64);

/// A transparent byte quantity.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(crate) struct Bytes(pub(crate) u64);

/// A transparent millisecond quantity.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(crate) struct Millis(pub(crate) u64);

/// Thousandths of full capacity; `1000` means saturated and is rejected.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(crate) struct PerMille(pub(crate) u64);

/// Capacity-input rejection with exact typed field ownership.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum CapacityModelError {
    /// A divisor or physical worker capacity was zero.
    #[error("{field:?} must be nonzero")]
    Zero { field: CapacityField },
    /// Target occupancy must reserve positive queueing headroom.
    #[error("{field:?} must be in 1..1000 per mille, observed {observed}")]
    Utilization { field: CapacityField, observed: u64 },
    /// Arithmetic overflowed the portable planner width.
    #[error("capacity arithmetic overflowed while deriving {field:?}")]
    Overflow { field: CapacityField },
    /// Replicas cannot satisfy the requested number of independent failures.
    #[error("replica count {replicas} cannot tolerate {failures} independent domain failures")]
    InsufficientReplicas { replicas: u64, failures: u64 },
    /// Independent replicas cannot be placed in the supplied domain count.
    #[error(
        "{replicas} independent replicas require at least {replicas} domains, observed {domains}"
    )]
    InsufficientDomains { domains: u64, replicas: u64 },
}

/// Exact worksheet field that owns one validation or arithmetic failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CapacityField {
    /// Compiler worker core capacity.
    CompilerCoresPerWorker,
    /// Compiler operating occupancy.
    CompilerUtilization,
    /// Index node durable capacity.
    IndexUsableDurableBytes,
    /// Independent immutable copies requested for the index.
    IndexReplicaCount,
    /// Index node query-slot capacity.
    IndexQuerySlots,
    /// Index node working-set memory capacity.
    IndexUsableMemoryBytes,
    /// Index query operating occupancy.
    IndexUtilization,
    /// Number of independent topology domains.
    IndependentDomains,
    /// Embedding worker measured throughput.
    EmbeddingMeasuredThroughput,
    /// Model quantization width.
    EmbeddingWeightBits,
    /// Output embedding dimensionality.
    EmbeddingOutputDimensions,
    /// Output vector quantization width.
    EmbeddingOutputElementBits,
    /// Embedding input batch size.
    EmbeddingBatchItems,
    /// Compiler CPU demand product.
    CompilerDemand,
    /// Index replicated storage product.
    IndexReplication,
    /// Index query demand product.
    IndexQueryDemand,
    /// Embedding weight-byte conversion.
    EmbeddingWeights,
    /// Embedding output-vector byte conversion.
    EmbeddingVectorBytes,
    /// Embedding batch activation reservation.
    EmbeddingBatchActivation,
    /// Embedding worker memory sum.
    EmbeddingWorkerMemory,
}
