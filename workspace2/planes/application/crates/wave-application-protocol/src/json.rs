//! JSON-RPC/MCP envelope decoding and lossless presentation.

use std::{error::Error, fmt, io, ops::Deref};

use nudox_adaptive::{
    BudgetAmount, CapabilityKind, DuplicateInput, ExecutionPhase, InputClass, Overload,
    OverloadSubject, PolicyError, RecoveryCause, ResourceClass, StorageTier,
};
use serde_json::{Value, json};
use wave_application_core::{
    APPLICATION_OPERATION, AdaptiveDisposition, ApplicationInput, ApplicationReply, Capability,
    CapabilityHealth, CapabilityTransition, CorrelationId, Diagnostic, DiagnosticDetail,
    ExecutionState, InputText, ReplyBody, Terminal,
};

use crate::{AdapterError, AdapterErrorCode, cli::input_from_json};

/// Decoded MCP request identity plus the single service input it carries.
#[derive(Clone, Debug, PartialEq)]
pub struct McpEnvelope {
    /// JSON-RPC request identifier preserved for the response.
    /// `None` denotes a JSON-RPC notification, which must not receive a response.
    pub id: Option<Value>,
    /// Closed typed service input.
    pub input: ApplicationInput,
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
    let input = match method {
        "tools/call" => tool_input(params),
        "$/cancelRequest" => cancellation_input(params),
        _ => Err(unknown(method)),
    };
    match input {
        Ok(input) => McpDecode::Accepted(McpEnvelope { id, input }),
        Err(error) => McpDecode::Rejected(McpDecodeError::new(id, error)),
    }
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
    input_from_json(&action, correlation, |name| {
        required_argument(arguments, name)
    })
}

fn cancellation_input(
    params: &serde_json::Map<String, Value>,
) -> Result<ApplicationInput, AdapterError> {
    // MCP's cancellation notification identifies the original request, not the service's
    // internal operation key. This application currently exposes one bounded operation, so the
    // request id maps to its correlation while the typed operation identity remains service-owned.
    let request_id = params
        .get("requestId")
        .ok_or_else(|| missing("requestId"))?;
    let correlation = request_id_number(request_id, "requestId")?;
    Ok(ApplicationInput::Cancel {
        correlation: CorrelationId(correlation),
        operation: APPLICATION_OPERATION,
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

fn request_id_number(value: &Value, field: &'static str) -> Result<u64, AdapterError> {
    if let Some(number) = value.as_u64() {
        return Ok(number);
    }
    let Some(text) = value.as_str() else {
        return Err(malformed(field));
    };
    text.parse::<u64>()
        .map_err(|source| AdapterError::invalid_number(field, source))
}

fn reply_body(body: ReplyBody) -> Value {
    match body {
        ReplyBody::CompilerPassthrough {
            package,
            language,
            stage,
            source,
        } => json!({
            "kind": "compiler_passthrough",
            "package": text(package),
            "language": language_name(language),
            "stage": stage_name(stage),
            "source": text(source),
        }),
        ReplyBody::DependencyUnavailable { capability } => json!({
            "kind": "dependency_unavailable",
            "capability": capability_name(capability),
        }),
        ReplyBody::Health(facts) => json!({
            "kind": "health",
            "facts": facts.map(health),
        }),
        ReplyBody::Adaptive(disposition) => json!({
            "kind": "adaptive",
            "disposition": adaptive_disposition(disposition),
        }),
        ReplyBody::ExecutionStarted {
            operation,
            transition,
        } => json!({
            "kind": "execution_started",
            "operation": operation.0,
            "transition": capability_transition(transition),
        }),
        ReplyBody::Execution(state) => json!({
            "kind": "execution",
            "state": execution_state(state),
        }),
        ReplyBody::Rejected => json!({"kind": "rejected"}),
    }
}

fn terminal(terminal: Terminal) -> Value {
    match terminal {
        Terminal::Accepted { operation } => {
            json!({"kind": "accepted", "operation": operation.0})
        }
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
        wave_application_core::DiagnosticCode::OperationUnavailable => "operation_unavailable",
        wave_application_core::DiagnosticCode::AdaptivePolicyRejected => "adaptive_policy_rejected",
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
        DiagnosticDetail::Operation(operation) => {
            json!({"kind": "operation", "value": operation.0})
        }
        DiagnosticDetail::Capability(capability) => {
            json!({"kind": "capability", "value": capability_name(capability)})
        }
        DiagnosticDetail::Policy(policy) => json!({
            "kind": "policy",
            "error": policy_error(policy),
        }),
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
        Capability::CompilerRegistry => "compiler_registry",
        Capability::CompilerOutput => "compiler_output",
        Capability::Index => "index",
        Capability::Graph => "graph",
        Capability::Vector => "vector",
        Capability::LocalAnalyzer => "local_analyzer",
        Capability::Remote => "remote",
    }
}

fn adaptive_disposition(disposition: AdaptiveDisposition) -> Value {
    match disposition {
        AdaptiveDisposition::NoAction => json!({"kind": "no_action"}),
        AdaptiveDisposition::RetryRemote {
            pin,
            cause,
            retries_remaining,
        } => json!({
            "kind": "retry_remote",
            "pin": pin_value(pin),
            "cause": recovery_cause(cause),
            "retries_remaining": retries_remaining.get(),
        }),
        AdaptiveDisposition::RecoveryExhausted { pin, cause } => json!({
            "kind": "recovery_exhausted",
            "pin": pin_value(pin),
            "cause": recovery_cause(cause),
        }),
        AdaptiveDisposition::Overloaded(overload) => json!({
            "kind": "overloaded",
            "overload": overload_value(overload),
        }),
        AdaptiveDisposition::Rejected(policy) => json!({
            "kind": "rejected",
            "error": policy_error(policy),
        }),
    }
}

fn capability_transition(transition: CapabilityTransition) -> Value {
    match transition {
        CapabilityTransition::Acquire { capability, bundle } => json!({
            "kind": "acquire",
            "capability": capability_kind_name(capability),
            "bundle": content_id(bundle),
        }),
        CapabilityTransition::Release { capability, bundle } => json!({
            "kind": "release",
            "capability": capability_kind_name(capability),
            "bundle": content_id(bundle),
        }),
    }
}

fn execution_state(state: ExecutionState) -> Value {
    match state {
        ExecutionState::Pending {
            operation,
            transition,
        } => json!({
            "kind": "pending",
            "operation": operation.0,
            "transition": capability_transition(transition),
        }),
        ExecutionState::Completed {
            operation,
            transition,
        } => json!({
            "kind": "completed",
            "operation": operation.0,
            "transition": capability_transition(transition),
        }),
        ExecutionState::Cancelled {
            operation,
            transition,
        } => json!({
            "kind": "cancelled",
            "operation": operation.0,
            "transition": capability_transition(transition),
        }),
        ExecutionState::Failed {
            operation,
            transition,
            phase,
        } => json!({
            "kind": "failed",
            "operation": operation.0,
            "transition": capability_transition(transition),
            "phase": execution_phase_name(phase),
        }),
    }
}

fn pin_value(pin: nudox_adaptive::Pin) -> Value {
    json!({
        "generation": content_id(pin.generation),
        "snapshot": content_id(pin.snapshot),
    })
}

fn content_id<DomainTag>(value: nudox_adaptive::ContentId<DomainTag>) -> String {
    value.to_string()
}

fn recovery_cause(cause: RecoveryCause) -> Value {
    match cause {
        RecoveryCause::Outage => json!({"kind": "outage"}),
        RecoveryCause::Inconsistent { observed } => json!({
            "kind": "inconsistent",
            "observed": pin_value(observed),
        }),
    }
}

fn overload_value(overload: Overload) -> Value {
    json!({
        "subject": overload_subject(overload.subject),
        "resource": resource_class_name(overload.resource),
        "needed": budget_amount(overload.needed),
        "available": budget_amount(overload.available),
    })
}

fn overload_subject(subject: OverloadSubject) -> Value {
    match subject {
        OverloadSubject::Fact(key) => json!({"kind": "fact", "key": fact_key(key)}),
        OverloadSubject::Bundle { capability, bundle } => json!({
            "kind": "bundle",
            "capability": capability_kind_name(capability),
            "bundle": content_id(bundle),
        }),
        OverloadSubject::Remote(pin) => json!({"kind": "remote", "pin": pin_value(pin)}),
    }
}

fn budget_amount(amount: BudgetAmount) -> Value {
    match amount {
        BudgetAmount::Bytes(bytes) => json!({"kind": "bytes", "value": bytes.get()}),
        BudgetAmount::Operations(operations) => {
            json!({"kind": "operations", "value": operations.get()})
        }
        BudgetAmount::Retries(retries) => {
            json!({"kind": "retries", "value": retries.get()})
        }
    }
}

fn fact_key(key: nudox_adaptive::FactKey) -> Value {
    json!({
        "pin": pin_value(key.pin),
        "object": content_id(key.object),
    })
}

fn policy_error(error: PolicyError) -> Value {
    match error {
        PolicyError::TooManyFacts {
            class,
            limit,
            observed,
        } => json!({
            "kind": "too_many_facts",
            "class": input_class_name(class),
            "limit": limit,
            "observed": observed,
        }),
        PolicyError::PinMismatch {
            class,
            expected,
            observed,
        } => json!({
            "kind": "pin_mismatch",
            "class": input_class_name(class),
            "expected": pin_value(expected),
            "observed": fact_key(observed),
        }),
        PolicyError::Duplicate(duplicate) => {
            json!({"kind": "duplicate", "value": duplicate_input(duplicate)})
        }
    }
}

fn duplicate_input(duplicate: DuplicateInput) -> Value {
    match duplicate {
        DuplicateInput::Local { key, tier } => {
            json!({
                "kind": "local",
                "key": fact_key(key),
                "tier": storage_tier_name(tier),
            })
        }
        DuplicateInput::Remote { key } => json!({"kind": "remote", "key": fact_key(key)}),
        DuplicateInput::Demand { key } => json!({"kind": "demand", "key": fact_key(key)}),
        DuplicateInput::Bundle { capability, bundle } => json!({
            "kind": "bundle",
            "capability": capability_kind_name(capability),
            "bundle": content_id(bundle),
        }),
    }
}

fn input_class_name(class: InputClass) -> &'static str {
    match class {
        InputClass::Local => "local",
        InputClass::Remote => "remote",
        InputClass::Demand => "demand",
        InputClass::Bundle => "bundle",
    }
}

fn resource_class_name(class: ResourceClass) -> &'static str {
    match class {
        ResourceClass::Ram => "ram",
        ResourceClass::Nvme => "nvme",
        ResourceClass::Operations => "operations",
        ResourceClass::Retries => "retries",
    }
}

fn storage_tier_name(tier: StorageTier) -> &'static str {
    match tier {
        StorageTier::Ram => "ram",
        StorageTier::Nvme => "nvme",
    }
}

fn capability_kind_name(capability: CapabilityKind) -> &'static str {
    match capability {
        CapabilityKind::Analyzer => "analyzer",
        CapabilityKind::Compiler => "compiler",
        CapabilityKind::Codec => "codec",
        CapabilityKind::Model => "model",
    }
}

fn execution_phase_name(phase: ExecutionPhase) -> &'static str {
    match phase {
        ExecutionPhase::LocalResidence => "local_residence",
        ExecutionPhase::CapabilityBundle => "capability_bundle",
        ExecutionPhase::Remote => "remote",
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
