//! Measures `backend-store` benches capacity-planning model work with production data paths.
//! Measurements separate setup from steady-state work and retain resource counters.
//! Results support capacity decisions without changing the measured implementation.
use std::path::PathBuf;

use serde::Serialize;

/// Maximum real API corpus accepted by this small local runner.
pub(crate) const MAX_CORPUS: usize = 64;

/// A reproducible cache-state convention, rather than an unprovable claim about OS page cache.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheMode {
    /// One fresh process invocation and fresh artifact directory were used.
    ColdProcess,
    /// An unmeasured journey preceded measured samples in the same process.
    WarmProcess,
}

/// One actual public API stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    NativeCompileLowerIr,
    SemanticIrBuild,
    SemanticIrRender,
    IrVcsDiff,
    TrustfallIrQuery,
    DurableCompilerPublication,
    DeterministicIndexBuild,
    ExactCoreQuery,
    TantivyLexicalBuild,
    TantivyLexicalQuery,
    VectorIngress,
    VectorExactQuery,
    IrVectorExactQuery,
}

impl Stage {
    /// Stable CLI selector used by the per-stage external peak-RSS wrapper.
    #[must_use]
    pub(crate) const fn selector(self) -> &'static str {
        match self {
            Self::NativeCompileLowerIr => "native-compile-lower-ir",
            Self::SemanticIrBuild => "semantic-ir-build",
            Self::SemanticIrRender => "semantic-ir-render",
            Self::IrVcsDiff => "ir-vcs-diff",
            Self::TrustfallIrQuery => "trustfall-ir-query",
            Self::DeterministicIndexBuild => "deterministic-index-build",
            Self::DurableCompilerPublication => "durable-compiler-publication",
            Self::ExactCoreQuery => "exact-core-query",
            Self::TantivyLexicalBuild => "tantivy-lexical-build",
            Self::TantivyLexicalQuery => "tantivy-lexical-query",
            Self::VectorIngress => "vector-ingress",
            Self::VectorExactQuery => "vector-exact-query",
            Self::IrVectorExactQuery => "ir-vector-exact-query",
        }
    }

    /// Pipeline order used to prepare prerequisites for a selected external sample.
    #[must_use]
    pub(crate) const fn position(self) -> u8 {
        match self {
            Self::NativeCompileLowerIr => 0,
            Self::SemanticIrBuild => 1,
            Self::SemanticIrRender => 2,
            Self::IrVcsDiff => 3,
            Self::TrustfallIrQuery => 4,
            Self::DurableCompilerPublication => 5,
            Self::DeterministicIndexBuild => 6,
            Self::ExactCoreQuery => 7,
            Self::TantivyLexicalBuild => 8,
            Self::TantivyLexicalQuery => 9,
            Self::VectorIngress => 10,
            Self::VectorExactQuery => 11,
            Self::IrVectorExactQuery => 12,
        }
    }

    /// Resolves one stable CLI selector without open-ended string states.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        [
            Self::NativeCompileLowerIr,
            Self::SemanticIrBuild,
            Self::SemanticIrRender,
            Self::IrVcsDiff,
            Self::TrustfallIrQuery,
            Self::DurableCompilerPublication,
            Self::DeterministicIndexBuild,
            Self::ExactCoreQuery,
            Self::TantivyLexicalBuild,
            Self::TantivyLexicalQuery,
            Self::VectorIngress,
            Self::VectorExactQuery,
            Self::IrVectorExactQuery,
        ]
        .into_iter()
        .find(|stage| stage.selector() == value)
    }

    /// Enumerates every current local public-API stage.
    #[must_use]
    pub(crate) const fn all() -> [Self; 13] {
        [
            Self::NativeCompileLowerIr,
            Self::SemanticIrBuild,
            Self::SemanticIrRender,
            Self::IrVcsDiff,
            Self::TrustfallIrQuery,
            Self::DurableCompilerPublication,
            Self::DeterministicIndexBuild,
            Self::ExactCoreQuery,
            Self::TantivyLexicalBuild,
            Self::TantivyLexicalQuery,
            Self::VectorIngress,
            Self::VectorExactQuery,
            Self::IrVectorExactQuery,
        ]
    }
}

/// A metric that may be unavailable without lying about portability.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "availability")]
pub(crate) enum Metric {
    /// The runner captured this value directly.
    Measured { value: u64 },
    /// The standard library cannot soundly provide this value on this platform.
    Unavailable { reason: MetricUnavailable },
}

/// Closed reason for unavailable instrumentation.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricUnavailable {
    /// CPU accounting is intentionally delegated to the platform wrapper.
    ExternalProcessAccountingRequired,
    /// A peak high-water mark requires platform APIs or the external wrapper.
    ExternalPeakRssWrapperRequired,
    /// The optional `ps` probe was not available or did not return a valid value.
    ProcessProbeUnavailable,
}

/// Allocation facts supplied by the workspace's established allocator counter.
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Allocations {
    /// Total allocation calls during the measured closure.
    pub(crate) count_total: u64,
    /// Live allocation calls after the closure returns.
    pub(crate) count_current: i64,
    /// Peak simultaneously live allocation calls.
    pub(crate) count_max: u64,
    /// Total requested allocation bytes.
    pub(crate) bytes_total: u64,
    /// Requested bytes still live after the closure returns.
    pub(crate) bytes_current: i64,
    /// Peak simultaneously live requested bytes.
    pub(crate) bytes_max: u64,
}

/// One measured stage sample; byte counts are explicit logical I/O, not inferred block-device I/O.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct StageSample {
    /// Public API stage.
    pub(crate) stage: Stage,
    /// Warm/cold-process convention used for this sample.
    pub(crate) cache_mode: CacheMode,
    /// One-based measured sample ordinal for this corpus.
    pub(crate) sample: usize,
    /// Number of representative source/doc/vector input items consumed.
    pub(crate) input_items: usize,
    /// Number of output items or initialized result slots produced.
    pub(crate) output_items: usize,
    /// Logical bytes read by this stage's public operation.
    pub(crate) bytes_read: u64,
    /// Logical bytes written by this stage's public operation.
    pub(crate) bytes_written: u64,
    /// Durable regular-file bytes below this stage's artifact root after completion.
    pub(crate) durable_bytes: u64,
    /// Stage wall-clock elapsed time, excluding output serialization and RSS probing.
    pub(crate) wall_time_ns: u128,
    /// CPU accounting when a platform-specific owner supplied it.
    pub(crate) cpu_time_ns: Metric,
    /// Peak process RSS when a platform-specific owner supplied it.
    pub(crate) peak_rss_bytes: Metric,
    /// Best-effort current process RSS sampled after the timed closure.
    pub(crate) rss_after_bytes: Metric,
    /// Allocation facts for this process only; child compiler allocations are not included.
    pub(crate) allocations: Allocations,
    /// Logical input-byte throughput over `wall_time_ns`.
    pub(crate) input_throughput_bytes_per_second: f64,
}

/// Stable execution facts needed to reproduce a result.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Machine {
    /// Operating-system name reported by Rust.
    pub(crate) operating_system: &'static str,
    /// CPU architecture reported by Rust.
    pub(crate) architecture: &'static str,
    /// Rust compiler version text from the selected native toolchain.
    pub(crate) rustc_version: String,
    /// Absolute selected `rustc` executable.
    pub(crate) rustc_path: PathBuf,
    /// Harness build profile chosen by the caller.
    pub(crate) profile: &'static str,
    /// Operating-system release used for this invocation.
    pub(crate) operating_system_release: MachineText,
    /// CPU model reported by the current operating system.
    pub(crate) cpu_model: MachineText,
    /// Physical CPU core count reported by the current operating system.
    pub(crate) physical_cpu_count: MachineNumber,
    /// Physical RAM bytes reported by the current operating system.
    pub(crate) physical_memory_bytes: MachineNumber,
}

/// A textual machine fact that a platform probe can report without inventing a fallback string.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "availability")]
pub(crate) enum MachineText {
    /// The current platform reported this exact text value.
    Measured { value: String },
    /// The current platform did not provide this fact through the bounded probe.
    Unavailable { reason: MachineProbeUnavailable },
}

/// A numeric machine fact that a platform probe can report without guessing zero.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "availability")]
pub(crate) enum MachineNumber {
    /// The current platform reported this exact numeric value.
    Measured { value: u64 },
    /// The current platform did not provide this fact through the bounded probe.
    Unavailable { reason: MachineProbeUnavailable },
}

/// Closed reason that a machine-provenance probe could not provide a fact.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MachineProbeUnavailable {
    /// This compact runner has no probe for the current target operating system.
    UnsupportedPlatform,
    /// The target operating system could not start the bounded probe command.
    ProbeStartFailed,
    /// The probe command rejected the request.
    ProbeRejected,
    /// The probe output was not valid UTF-8 or did not contain one value.
    ProbeOutputInvalid,
    /// A numeric machine value was outside the portable accounting width.
    ProbeValueInvalid,
}

/// Complete machine-readable result written once per invocation.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct BenchmarkResult {
    /// Stable result schema marker.
    pub(crate) schema: &'static str,
    /// Reproducibility facts.
    pub(crate) machine: Machine,
    /// Command configuration after validation.
    pub(crate) configuration: Configuration,
    /// Measured samples in execution order.
    pub(crate) samples: Vec<StageSample>,
    /// Current capability boundaries intentionally not measured as fabricated stages.
    pub(crate) unavailable: [UnavailableCapability; 2],
}

/// Validated runner configuration persisted in every output.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Configuration {
    /// Requested number of sources/documents, bounded by `MAX_CORPUS`.
    pub(crate) corpus_size: usize,
    /// Measured repetitions after warmups.
    pub(crate) samples: usize,
    /// Unrecorded full-journey warmups.
    pub(crate) warmups: usize,
    /// Cache state convention.
    pub(crate) cache_mode: CacheMode,
    /// Optional one-stage execution used by the external RSS/CPU wrapper.
    pub(crate) only_stage: Option<Stage>,
}

/// An honest current capability gap with a closed reason.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UnavailableCapability {
    /// No public Qdrant client/transport boundary is linked by this repository slice.
    QdrantVectorTransport,
    /// No model/tokenizer runtime or local embedding inference public API is linked today.
    LocalEmbeddingInference,
}
