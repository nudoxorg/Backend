//! Defines json wire envelope behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire envelope invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use interface_core::{ApplicationDisposition, ApplicationOutcome, ApplicationReply, ReplyBody};
use serde::Serialize;
use serde_json::Value;

use super::application::{DiagnosticWire, ReplyBodyWire, TerminalWire};
use crate::{AdapterError, AdapterErrorCode, AdapterField, mcp_tools};

#[derive(Serialize)]
/// JSON-RPC error envelope that borrows the caller's request identifier.
pub struct McpError<'value> {
    jsonrpc: &'static str,
    id: &'value Value,
    error: JsonRpcError,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: AdapterErrorCodeWire,
    data: AdapterErrorWire,
}

#[derive(Serialize)]
/// JSON-RPC success envelope that borrows the caller's request identifier.
pub struct McpReply<'value> {
    jsonrpc: &'static str,
    id: &'value Value,
    result: McpResult,
}

/// JSON-RPC lifecycle response with a typed registry or handshake result.
#[derive(Serialize)]
pub struct McpLifecycle<'value, Result> {
    jsonrpc: &'static str,
    id: &'value Value,
    result: Result,
}

/// Empty JSON object used only by the typed ping response.
#[derive(Serialize)]
pub struct McpPong {}

#[derive(Serialize)]
struct McpResult {
    content: [McpContent; 1],
    #[serde(rename = "structuredContent")]
    structured_content: ApplicationReplyWire,
}

#[derive(Serialize)]
struct McpContent {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: &'static str,
}

#[derive(Serialize)]
pub(crate) struct ApplicationReplyWire {
    correlation: u64,
    body: ReplyBodyWire,
    terminal: TerminalWire,
    diagnostic: Option<DiagnosticWire>,
}

#[derive(Serialize)]
pub(crate) struct AdapterErrorEnvelope {
    adapter_error: AdapterErrorWire,
}

#[derive(Serialize)]
struct AdapterErrorWire {
    code: AdapterErrorCodeWire,
    field: AdapterField,
    actual: Option<usize>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum AdapterErrorCodeWire {
    UnknownAction,
    MissingField,
    #[serde(rename = "invalid_number_or_json_shape")]
    InvalidNumber,
    #[serde(rename = "invalid_json_shape")]
    InvalidShape,
    FieldTooLong,
    TooManyFields,
    InvalidPackageUrl,
    PackageProfileMismatch,
}

impl<'value> McpError<'value> {
    /// Projects one typed adapter rejection into its stable JSON-RPC error envelope.
    #[must_use]
    pub fn new(id: &'value Value, error: &AdapterError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            error: JsonRpcError {
                code: -32602,
                message: error.code.into(),
                data: error.into(),
            },
        }
    }
}

impl<'value> McpReply<'value> {
    /// Projects one application reply into the MCP structured-content envelope.
    #[must_use]
    pub fn new(id: &'value Value, reply: ApplicationReply) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: McpResult {
                content: [McpContent {
                    content_type: "text",
                    text: "application reply",
                }],
                structured_content: reply.into(),
            },
        }
    }
}

/// Projects the deterministic MCP initialize facts.
#[must_use]
pub fn mcp_initialize(id: &Value) -> McpLifecycle<'_, mcp_tools::InitializeResult> {
    McpLifecycle {
        jsonrpc: "2.0",
        id,
        result: mcp_tools::initialize_result(),
    }
}

/// Projects the single closed MCP tool registry.
#[must_use]
pub fn mcp_tools_list(id: &Value) -> McpLifecycle<'_, mcp_tools::ToolsResult> {
    McpLifecycle {
        jsonrpc: "2.0",
        id,
        result: mcp_tools::tools_result(),
    }
}

/// Projects a JSON-RPC ping result.
#[must_use]
pub const fn mcp_pong(id: &Value) -> McpLifecycle<'_, McpPong> {
    McpLifecycle {
        jsonrpc: "2.0",
        id,
        result: McpPong {},
    }
}

impl From<ApplicationReply> for ApplicationReplyWire {
    fn from(reply: ApplicationReply) -> Self {
        let correlation = reply.correlation.0;
        match reply.outcome {
            ApplicationOutcome::Resolved(body) => Self::resolved(correlation, &body),
            ApplicationOutcome::Failed { diagnostic } => Self {
                correlation,
                body: ReplyBodyWire::Rejected,
                terminal: TerminalWire::Failed,
                diagnostic: Some(diagnostic.into()),
            },
        }
    }
}

impl ApplicationReplyWire {
    fn resolved(correlation: u64, body: &ReplyBody) -> Self {
        Self {
            correlation,
            body: (*body).into(),
            terminal: ApplicationDisposition::from(body).into(),
            diagnostic: None,
        }
    }
}

impl From<ApplicationDisposition> for TerminalWire {
    fn from(disposition: ApplicationDisposition) -> Self {
        match disposition {
            ApplicationDisposition::Accepted { operation } => Self::Accepted {
                operation: operation.0,
            },
            ApplicationDisposition::Complete { emitted } => Self::Complete { emitted },
            ApplicationDisposition::Partial {
                emitted,
                unavailable,
            } => Self::Partial {
                emitted,
                unavailable: unavailable.into(),
            },
            ApplicationDisposition::Degraded {
                emitted,
                unavailable,
            } => Self::Degraded {
                emitted,
                unavailable: unavailable.into(),
            },
            ApplicationDisposition::Cancelled { emitted } => Self::Cancelled { emitted },
        }
    }
}

impl From<&AdapterError> for AdapterErrorEnvelope {
    fn from(error: &AdapterError) -> Self {
        Self {
            adapter_error: error.into(),
        }
    }
}

impl From<&AdapterError> for AdapterErrorWire {
    fn from(error: &AdapterError) -> Self {
        Self {
            code: error.code.into(),
            field: error.field,
            actual: error.actual,
        }
    }
}

impl From<AdapterErrorCode> for AdapterErrorCodeWire {
    fn from(code: AdapterErrorCode) -> Self {
        match code {
            AdapterErrorCode::UnknownAction => Self::UnknownAction,
            AdapterErrorCode::MissingField => Self::MissingField,
            AdapterErrorCode::InvalidNumber => Self::InvalidNumber,
            AdapterErrorCode::InvalidShape => Self::InvalidShape,
            AdapterErrorCode::FieldTooLong => Self::FieldTooLong,
            AdapterErrorCode::TooManyFields => Self::TooManyFields,
            AdapterErrorCode::InvalidPackageUrl => Self::InvalidPackageUrl,
            AdapterErrorCode::PackageProfileMismatch => Self::PackageProfileMismatch,
        }
    }
}
