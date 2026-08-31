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

/// One closed application command before transport-independent typed conversion.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum RawApplicationCommand {
    /// Compiler vocabulary request.
    Generate {
        /// Request correlation.
        correlation: u64,
        /// Compiler language token.
        language: String,
        /// Compiler stage token.
        stage: String,
        /// Package name.
        package: String,
        /// Source text.
        source: String,
    },
    /// Snapshot status request.
    #[serde(rename = "status")]
    SnapshotStatus {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
    },
    /// Lexical retrieval request.
    Search {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
        /// Lexical query.
        query: String,
        /// Result limit as a native number or decimal text.
        limit: RawNumber,
    },
    /// Graph retrieval request.
    Graph {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
        /// Result limit as a native number or decimal text.
        limit: RawNumber,
    },
    /// Vector retrieval request.
    Vector {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
        /// Result limit as a native number or decimal text.
        limit: RawNumber,
    },
    /// Locality request.
    Locality {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
    },
    /// Capability health request.
    Health {
        /// Request correlation.
        correlation: u64,
    },
    /// Local recovery policy request.
    RecoverLocal {
        /// Request correlation.
        correlation: u64,
        /// Generation authority.
        generation: String,
        /// Snapshot authority.
        snapshot: String,
        /// Analyzer bundle identity.
        bundle: String,
        /// Free RAM budget.
        ram_free: RawNumber,
        /// Free `NVMe` budget.
        nvme_free: RawNumber,
        /// Operation budget.
        operations: RawNumber,
        /// Retry budget.
        retries: RawNumber,
        /// Memory pressure token.
        memory_pressure: String,
        /// Storage pressure token.
        storage_pressure: String,
        /// Battery state token.
        battery: String,
    },
    /// Inconsistent remote recovery policy request.
    RecoverInconsistent {
        /// Request correlation.
        correlation: u64,
        /// Expected generation authority.
        expected_generation: String,
        /// Expected snapshot authority.
        expected_snapshot: String,
        /// Observed generation authority.
        observed_generation: String,
        /// Observed snapshot authority.
        observed_snapshot: String,
        /// Analyzer bundle identity.
        bundle: String,
        /// Free RAM budget.
        ram_free: RawNumber,
        /// Free `NVMe` budget.
        nvme_free: RawNumber,
        /// Operation budget.
        operations: RawNumber,
        /// Retry budget.
        retries: RawNumber,
        /// Memory pressure token.
        memory_pressure: String,
        /// Storage pressure token.
        storage_pressure: String,
        /// Battery state token.
        battery: String,
    },
    /// Local release policy request.
    ReleaseLocal {
        /// Request correlation.
        correlation: u64,
        /// Generation authority.
        generation: String,
        /// Snapshot authority.
        snapshot: String,
        /// Analyzer bundle identity.
        bundle: String,
        /// Free RAM budget.
        ram_free: RawNumber,
        /// Free `NVMe` budget.
        nvme_free: RawNumber,
        /// Operation budget.
        operations: RawNumber,
        /// Retry budget.
        retries: RawNumber,
        /// Memory pressure token.
        memory_pressure: String,
        /// Storage pressure token.
        storage_pressure: String,
        /// Battery state token.
        battery: String,
    },
    /// Execution polling request.
    PollExecution {
        /// Request correlation.
        correlation: u64,
        /// Service execution operation key.
        operation: RawNumber,
    },
    /// Execution cancellation request.
    Cancel {
        /// Request correlation.
        correlation: u64,
        /// Service execution operation key.
        operation: RawNumber,
    },
}
