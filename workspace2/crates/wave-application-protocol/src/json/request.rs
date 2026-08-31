//! Closed serde DTOs for the MCP request grammar.
//!
//! JSON-RPC request identities are intentionally kept as the one dynamic edge: the protocol
//! allows string, number, and null identities, and the process must echo that value exactly. The
//! application tool arguments themselves are a closed record, so they never travel through a
//! string-keyed map or an untyped JSON value.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::{AdapterError, AdapterErrorCode};

/// Presence-aware JSON-RPC identity field used for both requests and cancellation parameters.
///
/// A missing identity and an explicit `null` identity have different JSON-RPC meaning: the former
/// is a notification, while the latter is a request whose response identity is `null`.
#[derive(Debug, Default)]
pub(super) enum RequestIdField {
    /// The field was not present in its containing object.
    #[default]
    Missing,
    /// The field was present, including when its value is JSON `null`.
    Present(Value),
}

impl<'de> Deserialize<'de> for RequestIdField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Self::Present)
    }
}

/// Typed top-level MCP request shape. Business validation is deliberately deferred to the shared
/// CLI/MCP input decoder after this shape has been accepted.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestDto {
    /// JSON-RPC protocol marker, checked by the adapter after decoding.
    #[serde(default)]
    pub(super) jsonrpc: Option<String>,
    /// Optional request identity; `RequestIdField::Missing` denotes a notification.
    #[serde(default)]
    pub(super) id: RequestIdField,
    /// Method token, mapped to the closed method vocabulary by the adapter.
    #[serde(default)]
    pub(super) method: Option<String>,
    /// Closed method parameter record.
    #[serde(default)]
    pub(super) params: Option<ParamsDto>,
}

/// Identity-only fallback used when a later typed field rejects. It lets the process retain the
/// original response id without making the whole request grammar dynamic.
#[derive(Debug, Deserialize)]
pub(super) struct RequestIdentityDto {
    /// Optional request identity.
    #[serde(default)]
    pub(super) id: RequestIdField,
}

/// MCP method parameters. The method determines which subset is meaningful; unknown keys are
/// rejected by serde before any application operation is constructed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParamsDto {
    /// Fixed application tool name for `tools/call`.
    #[serde(default)]
    pub(super) name: Option<String>,
    /// Closed application argument record for `tools/call`.
    #[serde(default)]
    pub(super) arguments: Option<ApplicationArgumentsDto>,
    /// Original request identity for `$/cancelRequest`.
    #[serde(default, rename = "requestId")]
    pub(super) request_id: RequestIdField,
}

/// Closed argument fields shared by every application action. Optional fields are transport
/// presence markers only; `input_from_json` remains the single semantic/required-field validator.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApplicationArgumentsDto {
    /// Closed action token.
    #[serde(default)]
    pub(super) action: Option<String>,
    /// Required numeric application correlation.
    #[serde(default)]
    pub(super) correlation: Option<u64>,
    /// Compiler language token.
    #[serde(default)]
    language: Option<String>,
    /// Compiler stage token.
    #[serde(default)]
    stage: Option<String>,
    /// Package name.
    #[serde(default)]
    package: Option<String>,
    /// Source text.
    #[serde(default)]
    source: Option<String>,
    /// Snapshot selector.
    #[serde(default)]
    snapshot: Option<String>,
    /// Lexical query.
    #[serde(default)]
    query: Option<String>,
    /// Result limit, accepted as a JSON number or a numeric string for CLI parity.
    #[serde(default)]
    limit: Option<JsonArgument>,
    /// Expected generation identity.
    #[serde(default)]
    expected_generation: Option<String>,
    /// Expected snapshot identity.
    #[serde(default)]
    expected_snapshot: Option<String>,
    /// Observed generation identity.
    #[serde(default)]
    observed_generation: Option<String>,
    /// Observed snapshot identity.
    #[serde(default)]
    observed_snapshot: Option<String>,
    /// Generation identity.
    #[serde(default)]
    generation: Option<String>,
    /// Analyzer bundle identity.
    #[serde(default)]
    bundle: Option<String>,
    /// Free RAM budget.
    #[serde(default)]
    ram_free: Option<JsonArgument>,
    /// Free `NVMe` budget.
    #[serde(default)]
    nvme_free: Option<JsonArgument>,
    /// Operation budget.
    #[serde(default)]
    operations: Option<JsonArgument>,
    /// Retry budget.
    #[serde(default)]
    retries: Option<JsonArgument>,
    /// Memory pressure token.
    #[serde(default)]
    memory_pressure: Option<String>,
    /// Storage pressure token.
    #[serde(default)]
    storage_pressure: Option<String>,
    /// Battery state token.
    #[serde(default)]
    battery: Option<String>,
    /// Execution operation key.
    #[serde(default)]
    operation: Option<JsonArgument>,
}

impl ApplicationArgumentsDto {
    /// Returns one known argument as owned text for the shared application input decoder.
    pub(super) fn argument(&self, name: &'static str) -> Result<String, AdapterError> {
        match name {
            "language" => text(self.language.as_ref(), name),
            "stage" => text(self.stage.as_ref(), name),
            "package" => text(self.package.as_ref(), name),
            "source" => text(self.source.as_ref(), name),
            "snapshot" => text(self.snapshot.as_ref(), name),
            "query" => text(self.query.as_ref(), name),
            "limit" => number(self.limit.as_ref(), name),
            "generation" => text(self.generation.as_ref(), name),
            "bundle" => text(self.bundle.as_ref(), name),
            "ram_free" => number(self.ram_free.as_ref(), name),
            "nvme_free" => number(self.nvme_free.as_ref(), name),
            "operations" => number(self.operations.as_ref(), name),
            "retries" => number(self.retries.as_ref(), name),
            "memory_pressure" => text(self.memory_pressure.as_ref(), name),
            "storage_pressure" => text(self.storage_pressure.as_ref(), name),
            "battery" => text(self.battery.as_ref(), name),
            "expected_generation" => text(self.expected_generation.as_ref(), name),
            "expected_snapshot" => text(self.expected_snapshot.as_ref(), name),
            "observed_generation" => text(self.observed_generation.as_ref(), name),
            "observed_snapshot" => text(self.observed_snapshot.as_ref(), name),
            "operation" => number(self.operation.as_ref(), name),
            _ => Err(AdapterError::simple(AdapterErrorCode::InvalidShape, name)),
        }
    }
}

/// A numeric transport field that accepts the two representations already supported by MCP: a
/// JSON unsigned integer or a decimal string. Semantic width and range remain in `input_from_json`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum JsonArgument {
    /// Native JSON unsigned integer.
    Number(u64),
    /// Decimal text retained for the common CLI/MCP vocabulary.
    Text(String),
}

impl JsonArgument {
    fn as_text(&self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            Self::Text(value) => value.clone(),
        }
    }
}

fn text(value: Option<&String>, name: &'static str) -> Result<String, AdapterError> {
    value
        .cloned()
        .ok_or(AdapterError::simple(AdapterErrorCode::MissingField, name))
}

fn number(value: Option<&JsonArgument>, name: &'static str) -> Result<String, AdapterError> {
    value
        .map(JsonArgument::as_text)
        .ok_or(AdapterError::simple(AdapterErrorCode::MissingField, name))
}
