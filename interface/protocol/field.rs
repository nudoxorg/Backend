//! Defines field behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the field invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed names for every field in the application transport grammar.

use core::fmt;

use serde::Serialize;

/// One field owned by the closed CLI and MCP request vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterField {
    /// Requested application action.
    Action,
    /// Action-specific argument object.
    Arguments,
    /// Local battery state.
    Battery,
    /// Requested portable capability bundle.
    Bundle,
    /// Batched application commands.
    Commands,
    /// Request correlation identity.
    Correlation,
    /// Generation authority expected by recovery.
    ExpectedGeneration,
    /// Index snapshot authority expected by recovery.
    ExpectedSnapshot,
    /// Generation authority supplied by the caller.
    Generation,
    /// JSON-RPC request identity.
    Id,
    /// JSON-RPC protocol version.
    #[serde(rename = "jsonrpc")]
    JsonRpc,
    /// Compiler language selection.
    Language,
    /// Requested result bound.
    Limit,
    /// Local memory-pressure observation.
    MemoryPressure,
    /// JSON-RPC method name.
    Method,
    /// Locally available NVMe bytes.
    NvmeFree,
    /// Generation authority observed during recovery.
    ObservedGeneration,
    /// Index snapshot authority observed during recovery.
    ObservedSnapshot,
    /// Operation identity.
    Operation,
    /// Available operation credits.
    Operations,
    /// JSON-RPC parameter object.
    Params,
    /// Local filesystem path.
    Path,
    /// Search or graph query text.
    Query,
    /// Locally available RAM bytes.
    RamFree,
    /// Embedded application request.
    Request,
    /// Cancellation target request identity.
    #[serde(rename = "requestId")]
    RequestId,
    /// Available retry credits.
    Retries,
    /// Immutable index snapshot identity.
    Snapshot,
    /// Compiler source text.
    Source,
    /// Compiler pipeline stage.
    Stage,
    /// Local storage-pressure observation.
    StoragePressure,
}

impl fmt::Display for AdapterField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Action => "action",
            Self::Arguments => "arguments",
            Self::Battery => "battery",
            Self::Bundle => "bundle",
            Self::Commands => "commands",
            Self::Correlation => "correlation",
            Self::ExpectedGeneration => "expected_generation",
            Self::ExpectedSnapshot => "expected_snapshot",
            Self::Generation => "generation",
            Self::Id => "id",
            Self::JsonRpc => "jsonrpc",
            Self::Language => "language",
            Self::Limit => "limit",
            Self::MemoryPressure => "memory_pressure",
            Self::Method => "method",
            Self::NvmeFree => "nvme_free",
            Self::ObservedGeneration => "observed_generation",
            Self::ObservedSnapshot => "observed_snapshot",
            Self::Operation => "operation",
            Self::Operations => "operations",
            Self::Params => "params",
            Self::Path => "path",
            Self::Query => "query",
            Self::RamFree => "ram_free",
            Self::Request => "request",
            Self::RequestId => "requestId",
            Self::Retries => "retries",
            Self::Snapshot => "snapshot",
            Self::Source => "source",
            Self::Stage => "stage",
            Self::StoragePressure => "storage_pressure",
        })
    }
}
