//! JSON-RPC/MCP envelope decoding and lossless presentation.

use std::{error::Error, fmt, io, ops::Deref};

use serde_json::Value;
use wave_application_core::{ApplicationInput, ApplicationReply, InputText};

use crate::{AdapterError, AdapterErrorCode, cli::input_from_json};

mod wire;

pub use wire::{McpError, McpReply};

/// Decoded MCP request identity plus one typed request for the application process.
#[derive(Clone, Debug, PartialEq)]
pub struct McpEnvelope {
    /// JSON-RPC request identifier preserved for the response.
    /// `None` denotes a JSON-RPC notification, which must not receive a response.
    pub id: Option<Value>,
    /// Parsed request identity used to resolve later cancellation notifications.
    pub request_id: Option<McpRequestId>,
    /// Closed typed application request or cancellation target.
    pub request: McpRequest,
}

/// One typed MCP request, kept distinct from the core application input until cancellation is
/// resolved against the process-owned active-operation table.
#[derive(Clone, Debug, PartialEq)]
pub enum McpRequest {
    /// A normal application command ready for service dispatch.
    Application(ApplicationInput),
    /// A standard JSON-RPC cancellation target that still needs operation resolution.
    Cancellation(CancellationTarget),
}

/// Standard `$/cancelRequest` target before the MCP process resolves it to a service operation.
#[derive(Clone, Debug, PartialEq)]
pub struct CancellationTarget {
    /// Original JSON-RPC request identity being cancelled.
    pub request_id: McpRequestId,
}

/// Bounded JSON-RPC request identity retained for exact cancellation matching.
#[derive(Clone, Debug, PartialEq)]
pub enum McpRequestId {
    /// A JSON number request identity retained without narrowing to a correlation or operation.
    Number(serde_json::Number),
    /// A bounded JSON string request identity.
    String(InputText),
}

impl McpRequestId {
    /// Projects the typed request identity back to its exact JSON-RPC representation.
    #[must_use]
    pub fn as_value(&self) -> Value {
        match self {
            Self::Number(number) => Value::Number(number.clone()),
            Self::String(text) => {
                Value::String(String::from_utf8_lossy(text.as_bytes()).into_owned())
            }
        }
    }
}

/// A bounded MCP decode failure with the already parsed JSON-RPC request id retained.
///
/// Parse failures have no trustworthy id and therefore carry `None`. Once the JSON object has
/// been parsed, every subsequent shape/dispatch failure retains its `id` so the process adapter can
/// return a JSON-RPC error to the originating request instead of manufacturing `null`.
#[derive(Debug)]
pub struct McpDecodeError {
    /// Request id, when the JSON object supplied one.
    pub id: Option<Value>,
    /// Structured adapter cause, including its original parser source where applicable.
    pub error: AdapterError,
}

impl McpDecodeError {
    fn new(id: Option<Value>, error: AdapterError) -> Self {
        Self { id, error }
    }
}

impl Deref for McpDecodeError {
    type Target = AdapterError;

    fn deref(&self) -> &Self::Target {
        &self.error
    }
}

impl fmt::Display for McpDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for McpDecodeError {}

/// Closed result of one bounded MCP frame decode.
#[derive(Debug)]
pub enum McpDecode {
    /// A valid request or notification ready for the application service.
    Accepted(McpEnvelope),
    /// A transport rejection with its parsed request identity when available.
    Rejected(McpDecodeError),
}

/// Decodes a bounded MCP JSON-RPC request without performing business validation.
#[must_use]
pub fn decode_mcp(body: &[u8]) -> McpDecode {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(source) => {
            return McpDecode::Rejected(McpDecodeError::new(
                None,
                AdapterError::invalid_json("frame", source),
            ));
        }
    };
    let id = value
        .as_object()
        .and_then(|object| object.get("id"))
        .cloned();
    let request_id = match id.as_ref() {
        Some(value) => match parse_request_id(value, "id") {
            Ok(request_id) => Some(request_id),
            Err(error) => return McpDecode::Rejected(McpDecodeError::new(id, error)),
        },
        None => None,
    };
    let Some(object) = value.as_object() else {
        return McpDecode::Rejected(McpDecodeError::new(id, malformed("request")));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return McpDecode::Rejected(McpDecodeError::new(id, malformed("jsonrpc")));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return McpDecode::Rejected(McpDecodeError::new(id, malformed("method")));
    };
    let Some(params) = object.get("params").and_then(Value::as_object) else {
        return McpDecode::Rejected(McpDecodeError::new(id, missing("params")));
    };
    let request = match method {
        "tools/call" => tool_input(params).map(McpRequest::Application),
        "$/cancelRequest" if id.is_none() => {
            cancellation_input(params).map(McpRequest::Cancellation)
        }
        "$/cancelRequest" => Err(malformed("id")),
        _ => Err(unknown(method)),
    };
    match request {
        Ok(request) => McpDecode::Accepted(McpEnvelope {
            id,
            request_id,
            request,
        }),
        Err(error) => McpDecode::Rejected(McpDecodeError::new(id, error)),
    }
}

/// Builds a standard JSON-RPC error response for malformed adapter input.
#[must_use]
pub fn mcp_error<'value>(id: &'value Value, error: &AdapterError) -> McpError<'value> {
    McpError::new(id, error)
}

/// Builds an MCP tool response whose structured content exactly projects one service reply.
#[must_use]
pub fn mcp_reply(id: &Value, reply: ApplicationReply) -> McpReply<'_> {
    McpReply::new(id, reply)
}

/// Encodes one CLI semantic result without giving the CLI a JSON dependency edge.
///
/// # Errors
///
/// Returns the serializer's I/O-compatible error if bounded reply presentation cannot serialize.
pub fn encode_cli_reply(reply: ApplicationReply) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&wire::ApplicationReplyWire::from(reply)).map_err(io::Error::other)
}

/// Encodes one CLI transport diagnostic without giving the CLI a JSON dependency edge.
///
/// # Errors
///
/// Returns the serializer's I/O-compatible error if the diagnostic cannot serialize.
pub fn encode_cli_adapter_error(error: &AdapterError) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&wire::AdapterErrorEnvelope::from(error)).map_err(io::Error::other)
}

fn tool_input(params: &serde_json::Map<String, Value>) -> Result<ApplicationInput, AdapterError> {
    if params.get("name").and_then(Value::as_str) != Some("nudox.application") {
        return Err(unknown("tool"));
    }
    let arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| missing("arguments"))?;
    let action = required_text(arguments, "action")?;
    let correlation = required_number(arguments, "correlation")?;
    input_from_json(&action, correlation, |name| {
        required_argument(arguments, name)
    })
}

fn cancellation_input(
    params: &serde_json::Map<String, Value>,
) -> Result<CancellationTarget, AdapterError> {
    // MCP's cancellation notification identifies the original request, not the service's
    // internal operation key. The process resolves this typed target through its bounded active
    // request table before constructing the core cancellation input.
    let request_id = params
        .get("requestId")
        .ok_or_else(|| missing("requestId"))?;
    Ok(CancellationTarget {
        request_id: parse_request_id(request_id, "requestId")?,
    })
}

fn required_text(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
) -> Result<String, AdapterError> {
    let value = object.get(name).ok_or_else(|| missing(name))?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| malformed(name))
}

fn required_argument(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
) -> Result<String, AdapterError> {
    let value = object.get(name).ok_or_else(|| missing(name))?;
    if let Some(text) = value.as_str() {
        return Ok(text.to_owned());
    }
    if is_numeric_argument(name) {
        return value
            .as_u64()
            .map(|number| number.to_string())
            .ok_or_else(|| malformed(name));
    }
    Err(malformed(name))
}

fn is_numeric_argument(name: &str) -> bool {
    matches!(
        name,
        "limit" | "operation" | "ram_free" | "nvme_free" | "operations" | "retries"
    )
}

fn required_number(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
) -> Result<u64, AdapterError> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed(name))
}

fn parse_request_id(value: &Value, field: &'static str) -> Result<McpRequestId, AdapterError> {
    if let Some(number) = value.as_number() {
        if number.is_i64() || number.is_u64() {
            return Ok(McpRequestId::Number(number.clone()));
        }
        return Err(malformed(field));
    }
    if let Some(text) = value.as_str() {
        return InputText::try_from_str(text)
            .map(McpRequestId::String)
            .map_err(|source| AdapterError::field_too_long(field, source));
    }
    Err(malformed(field))
}

fn malformed(field: &'static str) -> AdapterError {
    AdapterError::simple(AdapterErrorCode::InvalidShape, field)
}

fn missing(field: &'static str) -> AdapterError {
    AdapterError::simple(AdapterErrorCode::MissingField, field)
}

fn unknown(_action: &str) -> AdapterError {
    AdapterError::simple(AdapterErrorCode::UnknownAction, "action")
}
