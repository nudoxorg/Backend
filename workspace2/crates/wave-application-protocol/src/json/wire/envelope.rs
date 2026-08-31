use serde::Serialize;
use serde_json::Value;
use wave_application_core::{
    ApplicationDisposition, ApplicationOutcome, ApplicationReply, ReplyBody,
};

use super::application::{DiagnosticWire, ReplyBodyWire, TerminalWire};
use crate::{AdapterError, AdapterErrorCode};

#[derive(Serialize)]
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
pub struct McpReply<'value> {
    jsonrpc: &'static str,
    id: &'value Value,
    result: McpResult,
}

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
pub struct ApplicationReplyWire {
    correlation: u64,
    body: ReplyBodyWire,
    terminal: TerminalWire,
    diagnostic: Option<DiagnosticWire>,
}

#[derive(Serialize)]
pub struct AdapterErrorEnvelope {
    adapter_error: AdapterErrorWire,
}

#[derive(Serialize)]
struct AdapterErrorWire {
    code: AdapterErrorCodeWire,
    field: &'static str,
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
}

impl<'value> McpError<'value> {
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
    #[must_use]
    pub fn new(id: &'value Value, reply: ApplicationReply) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: McpResult {
                content: [McpContent {
                    content_type: "text",
                    text: "nudox application reply",
                }],
                structured_content: reply.into(),
            },
        }
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
            terminal: ApplicationDisposition::from(*body).into(),
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
        }
    }
}
