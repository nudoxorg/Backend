//! Defines json behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! JSON-RPC/MCP envelope decoding and lossless presentation.

use std::{error::Error, fmt, io, ops::Deref};

use interface_core::{ApplicationInput, ApplicationReply, InputText};
use request::{RequestDto, RequestIdField, RequestIdentityDto};
use serde_json::Value;

use crate::{
    AdapterError, AdapterErrorCode, AdapterField, command::RawApplicationCommand, mcp_tools,
};

mod request;
mod wire;

pub use wire::{McpError, McpLifecycle, McpReply, mcp_initialize, mcp_pong, mcp_tools_list};

/// Decoded MCP request identity plus one typed request for the application process.
#[derive(Debug, PartialEq)]
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
#[derive(Debug, PartialEq)]
pub enum McpRequest {
    /// A normal application command ready for service dispatch.
    Application(ApplicationInput),
    /// A standard JSON-RPC cancellation target that still needs operation resolution.
    Cancellation(CancellationTarget),
    /// The lifecycle handshake; the process responds with fixed protocol facts.
    Initialize(InitializeParams),
    /// Post-initialize lifecycle notification with no application side effect.
    Initialized,
    /// Deterministic registry enumeration.
    ListTools,
    /// Liveness request.
    Ping,
}

/// Bounded lifecycle handshake facts retained from an accepted request.
#[derive(Debug, Eq, PartialEq)]
pub struct InitializeParams {
    /// Client-requested protocol revision, when it fits the shared text bound.
    pub protocol_version: Option<InputText>,
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
    /// An explicit JSON `null` request identity. It is distinct from an absent id notification.
    Null,
    /// A JSON number request identity retained without narrowing to a correlation or operation.
    Number(serde_json::Number),
    /// A bounded JSON string request identity.
    String(InputText),
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

impl Error for McpDecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

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
    let request: RequestDto = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(source) => {
            let id = serde_json::from_slice::<RequestIdentityDto>(body)
                .ok()
                .and_then(|request| match request.id {
                    RequestIdField::Missing => None,
                    RequestIdField::Present(value) => Some(value),
                });
            return McpDecode::Rejected(McpDecodeError::new(
                id,
                AdapterError::invalid_json(AdapterField::Request, source),
            ));
        }
    };
    let id = match request.id {
        RequestIdField::Missing => None,
        RequestIdField::Present(value) => Some(value),
    };
    let request_id = match id.as_ref() {
        Some(value) => match parse_request_id(value, AdapterField::Id) {
            Ok(request_id) => Some(request_id),
            Err(error) => return McpDecode::Rejected(McpDecodeError::new(id, error)),
        },
        None => None,
    };
    if request.jsonrpc.as_deref() != Some("2.0") {
        return McpDecode::Rejected(McpDecodeError::new(id, malformed(AdapterField::JsonRpc)));
    }
    let Some(method) = request.method else {
        return McpDecode::Rejected(McpDecodeError::new(id, malformed(AdapterField::Method)));
    };
    let params = request.params;
    let request = match method.as_str() {
        "tools/call" => params
            .ok_or_else(|| missing(AdapterField::Params))
            .and_then(tool_input)
            .map(McpRequest::Application),
        "initialize" => initialize_input(params).map(McpRequest::Initialize),
        "notifications/initialized" => Ok(McpRequest::Initialized),
        "tools/list" => Ok(McpRequest::ListTools),
        "ping" => Ok(McpRequest::Ping),
        "$/cancelRequest" if id.is_none() => params
            .ok_or_else(|| missing(AdapterField::Params))
            .and_then(cancellation_input)
            .map(McpRequest::Cancellation),
        "$/cancelRequest" => Err(malformed(AdapterField::Id)),
        _ => Err(unknown(&method)),
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

fn tool_input(params: Value) -> Result<ApplicationInput, AdapterError> {
    let params: request::ParamsDto = serde_json::from_value(params)
        .map_err(|source| AdapterError::invalid_json(AdapterField::Params, source))?;
    if params.name.as_deref() == Some("interface.application") {
        let arguments = params
            .arguments
            .ok_or_else(|| missing(AdapterField::Arguments))?;
        let command: RawApplicationCommand = serde_json::from_value(arguments)
            .map_err(|source| AdapterError::invalid_json(AdapterField::Request, source))?;
        return command.try_into();
    }
    let name = params
        .name
        .as_deref()
        .ok_or_else(|| missing(AdapterField::Action))?;
    let tool = mcp_tools::find(name).ok_or_else(|| unknown(name))?;
    mcp_tools::inject(tool, params.arguments)?.try_into()
}

fn initialize_input(params: Option<Value>) -> Result<InitializeParams, AdapterError> {
    let params = params.unwrap_or(Value::Object(serde_json::Map::new()));
    let params: request::InitializeParams = serde_json::from_value(params)
        .map_err(|source| AdapterError::invalid_json(AdapterField::Params, source))?;
    let protocol_version = params
        .protocol_version
        .map(|value| InputText::try_from_str(&value))
        .transpose()
        .map_err(|source| AdapterError::field_too_long(AdapterField::Request, source))?;
    Ok(InitializeParams { protocol_version })
}

fn cancellation_input(params: Value) -> Result<CancellationTarget, AdapterError> {
    let params: request::ParamsDto = serde_json::from_value(params)
        .map_err(|source| AdapterError::invalid_json(AdapterField::Params, source))?;
    // MCP's cancellation notification identifies the original request, not the service's
    // internal operation key. The process resolves this typed target through its bounded active
    // request table before constructing the core cancellation input.
    let RequestIdField::Present(request_id) = params.request_id else {
        return Err(missing(AdapterField::RequestId));
    };
    Ok(CancellationTarget {
        request_id: parse_request_id(&request_id, AdapterField::RequestId)?,
    })
}

fn parse_request_id(value: &Value, field: AdapterField) -> Result<McpRequestId, AdapterError> {
    if value.is_null() {
        return Ok(McpRequestId::Null);
    }
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

fn malformed(field: AdapterField) -> AdapterError {
    AdapterError::simple(AdapterErrorCode::InvalidShape, field)
}

fn missing(field: AdapterField) -> AdapterError {
    AdapterError::simple(AdapterErrorCode::MissingField, field)
}

fn unknown(_action: &str) -> AdapterError {
    AdapterError::simple(AdapterErrorCode::UnknownAction, AdapterField::Action)
}
