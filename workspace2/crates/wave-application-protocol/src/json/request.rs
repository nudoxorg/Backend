//! Closed serde DTOs for the MCP request grammar.
//!
//! JSON-RPC request identities are intentionally kept as the one dynamic edge: the protocol
//! allows string, number, and null identities, and the process must echo that value exactly. The
//! application tool arguments decode directly into the shared raw command vocabulary.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::command::RawApplicationCommand;

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

/// Typed top-level MCP request shape. Raw commands convert through the shared typed application
/// command boundary after this shape has been accepted.
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
    /// Shared closed raw application command for `tools/call`.
    #[serde(default)]
    pub(super) arguments: Option<RawApplicationCommand>,
    /// Original request identity for `$/cancelRequest`.
    #[serde(default, rename = "requestId")]
    pub(super) request_id: RequestIdField,
}
