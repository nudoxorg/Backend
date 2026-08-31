//! Closed raw application commands shared by positional and JSON-RPC adapters.

use serde::Deserialize;

/// A bounded numeric transport value accepted in either native JSON or CLI text form.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawNumber {
    /// Native JSON unsigned integer retained without formatting or reparsing.
    Number(u64),
    /// Decimal text retained for positional CLI parity.
    Text(String),
}

/// Compiler vocabulary and source fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGenerate {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Compiler language token.
    pub(crate) language: String,
    /// Compiler stage token.
    pub(crate) stage: String,
    /// Package name.
    pub(crate) package: String,
    /// Source text.
    pub(crate) source: String,
}

/// Shared immutable snapshot selector.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSnapshot {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Snapshot selector.
    pub(crate) snapshot: String,
}

/// Lexical query fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSearch {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Snapshot selector.
    pub(crate) snapshot: String,
    /// Lexical query.
    pub(crate) query: String,
    /// Result limit in native JSON or CLI text form.
    pub(crate) limit: RawNumber,
}

/// Shared graph and vector retrieval fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRetrieval {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Snapshot selector.
    pub(crate) snapshot: String,
    /// Result limit in native JSON or CLI text form.
    pub(crate) limit: RawNumber,
}

/// Argument-free capability health fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHealth {
    /// Request correlation.
    pub(crate) correlation: u64,
}

/// Shared local recovery and release policy.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPolicy {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Generation authority.
    pub(crate) generation: String,
    /// Snapshot authority.
    pub(crate) snapshot: String,
    /// Analyzer bundle identity.
    pub(crate) bundle: String,
    /// Free RAM budget.
    pub(crate) ram_free: RawNumber,
    /// Free `NVMe` budget.
    pub(crate) nvme_free: RawNumber,
    /// Operation budget.
    pub(crate) operations: RawNumber,
    /// Retry budget.
    pub(crate) retries: RawNumber,
    /// Memory pressure token.
    pub(crate) memory_pressure: String,
    /// Storage pressure token.
    pub(crate) storage_pressure: String,
    /// Battery state token.
    pub(crate) battery: String,
}

/// Inconsistent recovery authorities and policy budget.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawInconsistentPolicy {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Expected generation authority.
    pub(crate) expected_generation: String,
    /// Expected snapshot authority.
    pub(crate) expected_snapshot: String,
    /// Observed generation authority.
    pub(crate) observed_generation: String,
    /// Observed snapshot authority.
    pub(crate) observed_snapshot: String,
    /// Analyzer bundle identity.
    pub(crate) bundle: String,
    /// Free RAM budget.
    pub(crate) ram_free: RawNumber,
    /// Free `NVMe` budget.
    pub(crate) nvme_free: RawNumber,
    /// Operation budget.
    pub(crate) operations: RawNumber,
    /// Retry budget.
    pub(crate) retries: RawNumber,
    /// Memory pressure token.
    pub(crate) memory_pressure: String,
    /// Storage pressure token.
    pub(crate) storage_pressure: String,
    /// Battery state token.
    pub(crate) battery: String,
}

/// Shared execution operation selector.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawOperation {
    /// Request correlation.
    pub(crate) correlation: u64,
    /// Service execution operation key.
    pub(crate) operation: RawNumber,
}

/// One closed application command before transport-independent typed conversion.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub(crate) enum RawApplicationCommand {
    /// Compiler vocabulary request.
    Generate(RawGenerate),
    /// Snapshot status request.
    #[serde(rename = "status")]
    SnapshotStatus(RawSnapshot),
    /// Lexical retrieval request.
    Search(RawSearch),
    /// Graph retrieval request.
    Graph(RawRetrieval),
    /// Vector retrieval request.
    Vector(RawRetrieval),
    /// Locality request.
    Locality(RawSnapshot),
    /// Capability health request.
    Health(RawHealth),
    /// Local recovery policy request.
    RecoverLocal(RawPolicy),
    /// Inconsistent remote recovery policy request.
    RecoverInconsistent(RawInconsistentPolicy),
    /// Local release policy request.
    ReleaseLocal(RawPolicy),
    /// Execution polling request.
    PollExecution(RawOperation),
    /// Execution cancellation request.
    Cancel(RawOperation),
}
