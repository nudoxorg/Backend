//! JSON-RPC/MCP envelope decoding and lossless presentation.

use std::io;

use serde_json::{Value, json};
use wave_application_core::{
    ApplicationInput, ApplicationReply, Capability, CapabilityHealth, Diagnostic, DiagnosticDetail,
    InputText, ProgressCursor, ProgressEvents, ProgressPage, ReplyBody, Terminal,
};

use crate::{AdapterError, AdapterErrorCode, cli::input_from_json};

/// Decoded MCP request identity plus the single service input it carries.
#[derive(Clone, Debug, PartialEq)]
pub struct McpEnvelope {
    /// JSON-RPC request identifier preserved for the response.
    pub id: Value,
    /// Closed typed service input.
    pub input: ApplicationInput,
}

/// Decodes a bounded MCP JSON-RPC request without performing business validation.
///
/// # Errors
///
/// Returns [`AdapterError`] when the JSON-RPC envelope or its transport fields are malformed.
pub fn decode_mcp(body: &[u8]) -> Result<McpEnvelope, AdapterError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|source| AdapterError::invalid_json("frame", source))?;
    let object = value.as_object().ok_or_else(|| malformed("request"))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(malformed("jsonrpc"));
    }
    let id = object.get("id").cloned().ok_or_else(|| missing("id"))?;
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("method"))?;
    let params = object
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| missing("params"))?;
    let input = match method {
        "tools/call" => tool_input(params)?,
        "$/cancelRequest" => cancellation_input(params)?,
        _ => return Err(unknown(method)),
    };
    Ok(McpEnvelope { id, input })
}

/// Builds a standard JSON-RPC error response for malformed adapter input.
#[must_use]
pub fn mcp_error(id: &Value, error: &AdapterError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32602,
            "message": adapter_code(error.code),
            "data": adapter_error_json(error),
        }
    })
}

/// Builds an MCP tool response whose structured content exactly projects one service reply.
#[must_use]
pub fn mcp_reply(id: &Value, reply: ApplicationReply) -> Value {
    let structured = reply_json(reply);
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": "nudox application reply"}],
            "structuredContent": structured,
        }
    })
}

/// Converts a typed semantic reply into adapter-owned structured JSON without changing its meaning.
#[must_use]
pub fn reply_json(reply: ApplicationReply) -> Value {
    json!({
        "correlation": reply.correlation.0,
        "body": reply_body(reply.body),
        "terminal": terminal(reply.terminal),
        "diagnostic": reply.diagnostic.map(diagnostic),
    })
}

/// Encodes one CLI semantic result without giving the CLI a JSON dependency edge.
///
/// # Errors
///
/// Returns the serializer's I/O-compatible error if bounded reply presentation cannot serialize.
pub fn encode_cli_reply(reply: ApplicationReply) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&reply_json(reply)).map_err(io::Error::other)
}

/// Encodes one CLI transport diagnostic without giving the CLI a JSON dependency edge.
///
/// # Errors
///
/// Returns the serializer's I/O-compatible error if the diagnostic cannot serialize.
pub fn encode_cli_adapter_error(error: &AdapterError) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&json!({"adapter_error": adapter_error_json(error)}))
        .map_err(io::Error::other)
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
    input_from_json(&action, correlation, |name| required_text(arguments, name))
}

fn cancellation_input(
    params: &serde_json::Map<String, Value>,
) -> Result<ApplicationInput, AdapterError> {
    let correlation = required_number(params, "correlation")?;
    input_from_json("cancel", correlation, |name| required_text(params, name))
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

fn required_number(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
) -> Result<u64, AdapterError> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed(name))
}

fn reply_body(body: ReplyBody) -> Value {
    match body {
        ReplyBody::Generated {
            package,
            language,
            stage,
            output,
        } => json!({
            "kind": "generated",
            "package": text(package),
            "language": language_name(language),
            "stage": stage_name(stage),
            "output": text(output),
        }),
        ReplyBody::DependencyUnavailable { capability } => json!({
            "kind": "dependency_unavailable",
            "capability": capability_name(capability),
        }),
        ReplyBody::Health(facts) => json!({
            "kind": "health",
            "facts": facts.map(health),
        }),
        ReplyBody::Progress(page) => json!({
            "kind": "progress",
            "page": progress(page),
        }),
        ReplyBody::ProgressStarted { operation } => json!({
            "kind": "progress_started",
            "operation": operation.0,
        }),
        ReplyBody::Cancelled { operation } => json!({
            "kind": "cancelled",
            "operation": operation.0,
        }),
        ReplyBody::Rejected => json!({"kind": "rejected"}),
    }
}

fn progress(page: ProgressPage) -> Value {
    match page {
        ProgressPage::Events { events, next } => json!({
            "kind": "events",
            "events": progress_events(events),
            "next": cursor(next),
        }),
        ProgressPage::Pending {
            cursor: pending_cursor,
        } => json!({
            "kind": "pending",
            "cursor": cursor(pending_cursor),
        }),
        ProgressPage::Terminal {
            terminal: operation_terminal,
            next,
        } => json!({
            "kind": "terminal",
            "terminal": terminal(operation_terminal),
            "next": cursor(next),
        }),
        ProgressPage::Finished => json!({"kind": "finished"}),
    }
}

fn progress_events(events: ProgressEvents) -> Vec<Value> {
    let mut output = Vec::with_capacity(usize::from(events.len()));
    for ordinal in 0..events.len() {
        if let Some(event) = events.get(ordinal) {
            output.push(json!({
                "sequence": event.sequence,
                "operation": event.operation.0,
                "completed_units": event.completed_units,
            }));
        }
    }
    output
}

fn cursor(cursor: ProgressCursor) -> Value {
    match cursor {
        ProgressCursor::Start => json!("start"),
        ProgressCursor::Offset(offset) => json!(offset),
        ProgressCursor::Finished => json!("finished"),
    }
}

fn terminal(terminal: Terminal) -> Value {
    match terminal {
        Terminal::Complete { emitted } => json!({"kind": "complete", "emitted": emitted}),
        Terminal::Partial {
            emitted,
            unavailable,
        } => json!({
            "kind": "partial",
            "emitted": emitted,
            "unavailable": capability_name(unavailable),
        }),
        Terminal::Degraded {
            emitted,
            unavailable,
        } => json!({
            "kind": "degraded",
            "emitted": emitted,
            "unavailable": capability_name(unavailable),
        }),
        Terminal::Cancelled { emitted } => json!({"kind": "cancelled", "emitted": emitted}),
        Terminal::Failed => json!({"kind": "failed"}),
    }
}

fn diagnostic(diagnostic: Diagnostic) -> Value {
    json!({
        "code": diagnostic_code(diagnostic.code),
        "detail": diagnostic_detail(diagnostic.detail),
    })
}

fn diagnostic_code(code: wave_application_core::DiagnosticCode) -> &'static str {
    match code {
        wave_application_core::DiagnosticCode::UnknownLanguage => "unknown_language",
        wave_application_core::DiagnosticCode::UnknownStage => "unknown_stage",
        wave_application_core::DiagnosticCode::SemanticTextTooLong => "semantic_text_too_long",
        wave_application_core::DiagnosticCode::ResultLimitExceeded => "result_limit_exceeded",
        wave_application_core::DiagnosticCode::DependencyUnavailable => "dependency_unavailable",
        wave_application_core::DiagnosticCode::UnsupportedCompilerStage => {
            "unsupported_compiler_stage"
        }
        wave_application_core::DiagnosticCode::CompilerOutputUnrepresentable => {
            "compiler_output_unrepresentable"
        }
        wave_application_core::DiagnosticCode::ProgressCursorOutOfRange => {
            "progress_cursor_out_of_range"
        }
        wave_application_core::DiagnosticCode::OperationUnavailable => "operation_unavailable",
    }
}

fn diagnostic_detail(detail: DiagnosticDetail) -> Value {
    match detail {
        DiagnosticDetail::Text(value) => json!({"kind": "text", "value": text(value)}),
        DiagnosticDetail::Limit { requested, maximum } => {
            json!({"kind": "limit", "requested": requested, "maximum": maximum})
        }
        DiagnosticDetail::TextLength {
            actual,
            maximum,
            rejected,
        } => json!({
            "kind": "text_length",
            "actual": actual,
            "maximum": maximum,
            "rejected": text(rejected),
        }),
        DiagnosticDetail::Frontend(source) => frontend(source),
        DiagnosticDetail::Cursor { observed, maximum } => {
            json!({"kind": "cursor", "observed": observed, "maximum": maximum})
        }
        DiagnosticDetail::Operation(operation) => {
            json!({"kind": "operation", "value": operation.0})
        }
        DiagnosticDetail::Capability(capability) => {
            json!({"kind": "capability", "value": capability_name(capability)})
        }
    }
}

fn health(fact: CapabilityHealth) -> Value {
    match fact {
        CapabilityHealth::LocalReady(capability) => {
            json!({"capability": capability_name(capability), "state": "local_ready"})
        }
        CapabilityHealth::Unavailable(capability) => {
            json!({"capability": capability_name(capability), "state": "unavailable"})
        }
    }
}

fn frontend(source: nudox_compile_vocab::FrontendError) -> Value {
    match source {
        nudox_compile_vocab::FrontendError::UnsupportedStage { language, stage } => json!({
            "kind": "unsupported_stage",
            "language": language_name(language),
            "stage": stage_name(stage),
        }),
    }
}

fn language_name(language: nudox_compile_vocab::Language) -> &'static str {
    match language {
        nudox_compile_vocab::Language::RustSubset => "rust",
        nudox_compile_vocab::Language::TypeScriptSubset => "typescript",
    }
}

fn stage_name(stage: nudox_compile_vocab::Stage) -> &'static str {
    match stage {
        nudox_compile_vocab::Stage::Parse => "parse",
        nudox_compile_vocab::Stage::LowerIr => "lower-ir",
    }
}

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Compiler => "compiler",
        Capability::Index => "index",
        Capability::Graph => "graph",
        Capability::Vector => "vector",
    }
}

fn text(value: InputText) -> String {
    String::from_utf8_lossy(value.as_bytes()).into_owned()
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

fn adapter_error_json(error: &AdapterError) -> Value {
    json!({
        "code": adapter_code(error.code),
        "field": error.field,
        "actual": error.actual,
    })
}

fn adapter_code(code: AdapterErrorCode) -> &'static str {
    match code {
        AdapterErrorCode::UnknownAction => "unknown_action",
        AdapterErrorCode::MissingField => "missing_field",
        AdapterErrorCode::InvalidNumber => "invalid_number_or_json_shape",
        AdapterErrorCode::InvalidShape => "invalid_json_shape",
        AdapterErrorCode::FieldTooLong => "field_too_long",
        AdapterErrorCode::TooManyFields => "too_many_fields",
    }
}
