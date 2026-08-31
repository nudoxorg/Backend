use std::io::{self, Cursor};

use nudox_id::ContentIdDecodeError;
use serde::Serialize;
use wave_application_core::{
    ApplicationInput, CapabilityDomain, ContentId, GenerationId, InconsistentRecovery,
    IndexSnapshotId, InputText, Pin,
};
use wave_application_protocol::{
    AdapterErrorCause, AdapterErrorCode, CANONICAL_CONTENT_ID_TEXT_BYTES,
    CanonicalContentIdDecodeError, MAX_HEADER_LINE_BYTES, MAX_HEADER_LINES, McpDecode, McpRequest,
    McpRequestId, decode_cli, decode_mcp, read_frame,
};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Serialize)]
enum JsonRpcVersion {
    #[serde(rename = "2.0")]
    Version2,
}

#[derive(Serialize)]
enum RpcMethod {
    #[serde(rename = "tools/call")]
    ToolsCall,
    #[serde(rename = "$/cancelRequest")]
    CancelRequest,
}

#[derive(Serialize)]
enum ToolName {
    #[serde(rename = "nudox.application")]
    NudoxApplication,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum PolicyAction {
    RecoverLocal,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Pressure {
    Relaxed,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum BatteryState {
    Normal,
}

#[derive(Serialize)]
#[serde(untagged)]
enum JsonRpcRequestId {
    Null,
    Number(u64),
    Text(String),
}

#[derive(Serialize)]
struct McpPolicyRequest<'generation> {
    jsonrpc: JsonRpcVersion,
    id: JsonRpcRequestId,
    method: RpcMethod,
    params: McpToolCall<'generation>,
}

#[derive(Serialize)]
struct McpShapeRequest<Arguments> {
    jsonrpc: JsonRpcVersion,
    id: JsonRpcRequestId,
    method: RpcMethod,
    params: McpShapeToolCall<Arguments>,
}

#[derive(Serialize)]
struct McpShapeToolCall<Arguments> {
    name: ToolName,
    arguments: Arguments,
}

#[derive(Serialize)]
struct McpToolCall<'generation> {
    name: ToolName,
    arguments: McpPolicyArguments<'generation>,
}

#[derive(Serialize)]
struct McpPolicyArguments<'generation> {
    action: PolicyAction,
    correlation: u64,
    generation: &'generation str,
    snapshot: String,
    bundle: String,
    ram_free: u64,
    nvme_free: u64,
    operations: u64,
    retries: u64,
    memory_pressure: Pressure,
    storage_pressure: Pressure,
    battery: BatteryState,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum GenerateAction {
    Generate,
}

#[derive(Serialize)]
struct MissingGenerateArguments<'source> {
    action: GenerateAction,
    correlation: u64,
    language: &'source str,
    stage: &'source str,
    package: &'source str,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum SearchAction {
    Search,
}

#[derive(Serialize)]
struct CrossActionSearchArguments<'snapshot, 'query, 'generation> {
    action: SearchAction,
    correlation: u64,
    snapshot: &'snapshot str,
    query: &'query str,
    limit: u64,
    generation: &'generation str,
}

fn shape_request<Arguments: Serialize>(
    id: JsonRpcRequestId,
    arguments: Arguments,
) -> io::Result<String> {
    serde_json::to_string(&McpShapeRequest {
        jsonrpc: JsonRpcVersion::Version2,
        id,
        method: RpcMethod::ToolsCall,
        params: McpShapeToolCall {
            name: ToolName::NudoxApplication,
            arguments,
        },
    })
    .map_err(io::Error::other)
}

#[derive(Serialize)]
struct McpCancellationNotification<'request_id> {
    jsonrpc: JsonRpcVersion,
    method: RpcMethod,
    params: McpCancellationParams<'request_id>,
}

#[derive(Serialize)]
struct McpCancellationParams<'request_id> {
    #[serde(rename = "requestId")]
    request_id: &'request_id str,
}

fn policy_arguments() -> Vec<String> {
    [
        "recover-local".to_owned(),
        "7".to_owned(),
        GenerationId::from_digest([11; 32]).to_string(),
        IndexSnapshotId::from_digest([13; 32]).to_string(),
        ContentId::<CapabilityDomain>::from_digest([17; 32]).to_string(),
        "4096".to_owned(),
        "8192".to_owned(),
        "1".to_owned(),
        "1".to_owned(),
        "relaxed".to_owned(),
        "relaxed".to_owned(),
        "normal".to_owned(),
    ]
    .into_iter()
    .collect()
}

fn mcp_policy_request(generation: &str) -> io::Result<String> {
    mcp_policy_request_with_id(generation, JsonRpcRequestId::Number(9))
}

fn mcp_policy_request_with_id(generation: &str, id: JsonRpcRequestId) -> io::Result<String> {
    serde_json::to_string(&McpPolicyRequest {
        jsonrpc: JsonRpcVersion::Version2,
        id,
        method: RpcMethod::ToolsCall,
        params: McpToolCall {
            name: ToolName::NudoxApplication,
            arguments: McpPolicyArguments {
                action: PolicyAction::RecoverLocal,
                correlation: 9,
                generation,
                snapshot: IndexSnapshotId::from_digest([13; 32]).to_string(),
                bundle: ContentId::<CapabilityDomain>::from_digest([17; 32]).to_string(),
                ram_free: 4096,
                nvme_free: 8192,
                operations: 1,
                retries: 1,
                memory_pressure: Pressure::Relaxed,
                storage_pressure: Pressure::Relaxed,
                battery: BatteryState::Normal,
            },
        },
    })
    .map_err(io::Error::other)
}

#[test]
fn mcp_preserves_null_identity_as_a_request_not_a_notification() -> Result<(), TestError> {
    let generation = GenerationId::from_digest([11; 32]).to_string();
    let body = mcp_policy_request_with_id(&generation, JsonRpcRequestId::Null)?;
    let McpDecode::Accepted(envelope) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("null-id MCP request was rejected").into());
    };
    assert_eq!(envelope.id, Some(serde_json::Value::Null));
    assert_eq!(envelope.request_id, Some(McpRequestId::Null));
    Ok(())
}

#[test]
fn mcp_preserves_string_identity_for_cancellation_matching() -> Result<(), TestError> {
    let generation = GenerationId::from_digest([11; 32]).to_string();
    let body = mcp_policy_request_with_id(
        &generation,
        JsonRpcRequestId::Text("request-string".to_owned()),
    )?;
    let McpDecode::Accepted(envelope) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("string-id MCP request was rejected").into());
    };
    let expected = InputText::try_from_str("request-string")
        .map_err(|error| io::Error::other(format!("test identity is too long: {error:?}")))?;
    assert_eq!(envelope.request_id, Some(McpRequestId::String(expected)));
    Ok(())
}

#[test]
fn fixed_header_bound_rejects_before_earned_body_allocation() -> io::Result<()> {
    let input = vec![b'x'; MAX_HEADER_LINE_BYTES];
    let Err(error) = read_frame(&mut Cursor::new(input)) else {
        return Err(io::Error::other("overlong header was accepted"));
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
}

#[test]
fn fixed_header_count_rejects_before_earned_body_allocation() -> io::Result<()> {
    let mut input = Vec::new();
    for _ in 0..=MAX_HEADER_LINES {
        input.extend_from_slice(b"x: y\r\n");
    }
    input.extend_from_slice(b"\r\n");
    let Err(error) = read_frame(&mut Cursor::new(input)) else {
        return Err(io::Error::other("excess headers were accepted"));
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
}

#[test]
fn parser_errors_preserve_primitive_and_json_causes() -> Result<(), TestError> {
    let arguments = vec!["health".to_owned(), "not-a-number".to_owned()];
    let Err(number_error) = decode_cli(&arguments) else {
        return Err(io::Error::other("invalid correlation was accepted").into());
    };
    assert!(matches!(
        number_error.cause,
        Some(AdapterErrorCause::Number(_))
    ));

    let McpDecode::Rejected(json_error) = decode_mcp(b"{") else {
        return Err(io::Error::other("broken JSON was accepted").into());
    };
    assert!(matches!(json_error.cause, Some(AdapterErrorCause::Json(_))));
    Ok(())
}

#[test]
fn post_parse_mcp_rejections_retain_request_id() -> Result<(), TestError> {
    let McpDecode::Rejected(error) = decode_mcp(
        br#"{"jsonrpc":"2.0","id":91,"method":"tools/call","params":{"name":"nudox.application"}}"#,
    ) else {
        return Err(io::Error::other("malformed MCP request was accepted").into());
    };
    assert_eq!(error.id, Some(serde_json::Value::from(91)));
    assert_eq!(error.error.field, "arguments");
    Ok(())
}

#[test]
fn mcp_missing_required_generate_field_rejects_before_dispatch_with_id() -> Result<(), TestError> {
    let body = shape_request(
        JsonRpcRequestId::Number(401),
        MissingGenerateArguments {
            action: GenerateAction::Generate,
            correlation: 401,
            language: "rust",
            stage: "parse",
            package: "missing-source",
        },
    )?;
    let McpDecode::Rejected(error) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("missing generate source was accepted").into());
    };
    assert_eq!(error.id, Some(serde_json::Value::from(401)));
    assert_eq!(error.error.code, AdapterErrorCode::InvalidShape);
    assert_eq!(error.error.field, "request");
    assert!(matches!(error.cause, Some(AdapterErrorCause::Json(_))));
    Ok(())
}

#[test]
fn mcp_cross_action_field_rejects_before_dispatch_with_string_id() -> Result<(), TestError> {
    let generation = GenerationId::from_digest([11; 32]).to_string();
    let body = shape_request(
        JsonRpcRequestId::Text("cross-action".to_owned()),
        CrossActionSearchArguments {
            action: SearchAction::Search,
            correlation: 402,
            snapshot: "snapshot",
            query: "query",
            limit: 2,
            generation: &generation,
        },
    )?;
    let McpDecode::Rejected(error) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("cross-action search field was accepted").into());
    };
    assert_eq!(error.id, Some(serde_json::Value::from("cross-action")));
    assert_eq!(error.error.code, AdapterErrorCode::InvalidShape);
    assert_eq!(error.error.field, "request");
    assert!(matches!(error.cause, Some(AdapterErrorCause::Json(_))));
    Ok(())
}

#[test]
fn cli_decodes_canonical_policy_identities_without_rehashing_labels() -> Result<(), TestError> {
    let arguments = policy_arguments();
    let expected_pin = Pin {
        generation: GenerationId::from_digest([11; 32]),
        snapshot: IndexSnapshotId::from_digest([13; 32]),
    };
    let expected_bundle = ContentId::<CapabilityDomain>::from_digest([17; 32]);
    let decoded = decode_cli(&arguments).map_err(|error| {
        io::Error::other(format!(
            "valid canonical policy identities were rejected: {error}"
        ))
    })?;
    let ApplicationInput::RecoverLocal { pin, bundle, .. } = decoded else {
        return Err(io::Error::other("canonical policy command decoded to another input").into());
    };
    assert_eq!(pin, expected_pin);
    assert_eq!(bundle, expected_bundle);
    Ok(())
}

#[test]
fn cli_rejects_malformed_canonical_identity_with_exact_width_cause() -> Result<(), TestError> {
    let mut arguments = policy_arguments();
    arguments[2] = "content:00".to_owned();
    let Err(error) = decode_cli(&arguments) else {
        return Err(io::Error::other("malformed canonical generation was accepted").into());
    };
    assert_eq!(error.field, "generation");
    assert!(matches!(
        error.cause,
        Some(AdapterErrorCause::CanonicalContentId(
            CanonicalContentIdDecodeError::Width {
                actual: 10,
                expected: CANONICAL_CONTENT_ID_TEXT_BYTES,
            }
        ))
    ));
    Ok(())
}

#[test]
fn mcp_rejects_wrong_canonical_identity_domain_with_typed_cause() -> Result<(), TestError> {
    let wrong_domain = ContentId::<CapabilityDomain>::from_digest([19; 32]).to_string();
    let body = mcp_policy_request(&wrong_domain)?;
    let McpDecode::Rejected(error) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("wrong identity domain was accepted").into());
    };
    assert_eq!(error.id, Some(serde_json::Value::from(9)));
    assert_eq!(error.error.field, "generation");
    assert!(matches!(
        error.cause,
        Some(AdapterErrorCause::CanonicalContentId(
            CanonicalContentIdDecodeError::ContentId(ContentIdDecodeError::Domain { .. })
        ))
    ));
    Ok(())
}

#[test]
fn mcp_cancellation_remains_typed_until_process_resolution() -> Result<(), TestError> {
    let body = serde_json::to_string(&McpCancellationNotification {
        jsonrpc: JsonRpcVersion::Version2,
        method: RpcMethod::CancelRequest,
        params: McpCancellationParams {
            request_id: "operation-two",
        },
    })
    .map_err(io::Error::other)?;
    let McpDecode::Accepted(envelope) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("typed cancellation notification was rejected").into());
    };
    let expected_id = InputText::try_from_str("operation-two").map_err(|error| {
        io::Error::other(format!("test request identity is too long: {error:?}"))
    })?;
    assert_eq!(envelope.request_id, None);
    assert!(matches!(
        envelope.request,
        McpRequest::Cancellation(target)
            if target.request_id == McpRequestId::String(expected_id)
    ));
    Ok(())
}

#[test]
fn mcp_rejects_fractional_cancellation_request_id() -> Result<(), TestError> {
    let body = br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"requestId":1.5}}"#;
    let McpDecode::Rejected(error) = decode_mcp(body) else {
        return Err(io::Error::other("fractional cancellation request id was accepted").into());
    };
    assert_eq!(error.id, None);
    assert_eq!(error.error.code, AdapterErrorCode::InvalidShape);
    assert_eq!(error.error.field, "requestId");
    Ok(())
}

#[test]
fn mcp_rejects_exponent_cancellation_request_id() -> Result<(), TestError> {
    let body = br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"requestId":1e3}}"#;
    let McpDecode::Rejected(error) = decode_mcp(body) else {
        return Err(io::Error::other("exponent cancellation request id was accepted").into());
    };
    assert_eq!(error.id, None);
    assert_eq!(error.error.code, AdapterErrorCode::InvalidShape);
    assert_eq!(error.error.field, "requestId");
    Ok(())
}

#[test]
fn cli_decodes_inconsistent_recovery_with_both_typed_pins() -> Result<(), TestError> {
    let expected = Pin {
        generation: GenerationId::from_digest([21; 32]),
        snapshot: IndexSnapshotId::from_digest([23; 32]),
    };
    let observed = Pin {
        generation: GenerationId::from_digest([25; 32]),
        snapshot: IndexSnapshotId::from_digest([27; 32]),
    };
    let bundle = ContentId::<CapabilityDomain>::from_digest([29; 32]);
    let arguments = vec![
        "recover-inconsistent".to_owned(),
        "31".to_owned(),
        expected.generation.to_string(),
        expected.snapshot.to_string(),
        observed.generation.to_string(),
        observed.snapshot.to_string(),
        bundle.to_string(),
        "4096".to_owned(),
        "8192".to_owned(),
        "1".to_owned(),
        "1".to_owned(),
        "relaxed".to_owned(),
        "relaxed".to_owned(),
        "normal".to_owned(),
    ];
    let decoded = decode_cli(&arguments).map_err(|error| {
        io::Error::other(format!("valid inconsistent recovery was rejected: {error}"))
    })?;
    assert_eq!(
        decoded,
        ApplicationInput::RecoverInconsistent(InconsistentRecovery {
            correlation: wave_application_core::CorrelationId(31),
            expected,
            observed,
            bundle,
            budget: wave_application_core::ResourceBudget {
                ram_free: wave_application_core::ByteCount::from(4096),
                nvme_free: wave_application_core::ByteCount::from(8192),
                operations: wave_application_core::OperationBudget::from(1),
                retries: wave_application_core::RetryBudget::from(1),
                memory_pressure: wave_application_core::Pressure::Relaxed,
                storage_pressure: wave_application_core::Pressure::Relaxed,
                battery: wave_application_core::BatteryState::Normal,
            },
        })
    );
    Ok(())
}
