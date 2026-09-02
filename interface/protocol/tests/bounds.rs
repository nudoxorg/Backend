//! Exercises the `interface-protocol` tests bounds contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::io::{self, Cursor};

use compiler_vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion,
    RustEdition, Stage, TypeScriptSource,
};
use heart_identity::ContentIdDecodeError;
use interface_core::{
    ApplicationInput, CapabilityDomain, ContentId, GenerationId, InconsistentRecovery,
    IndexSnapshotId, InputText, PORTABLE_LOCAL_SOURCE_LIMIT, Pin, RejectedSourceText,
};
use interface_protocol::{
    AdapterErrorCause, AdapterErrorCode, AdapterField, CANONICAL_CONTENT_ID_TEXT_BYTES,
    CanonicalContentIdDecodeError, MAX_HEADER_LINE_BYTES, MAX_HEADER_LINES, McpDecode, McpRequest,
    McpRequestId, decode_cli, decode_mcp, read_frame,
};
use serde::Serialize;

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
    #[serde(rename = "interface.application")]
    Application,
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
    profile: &'source str,
    stage: &'source str,
}

#[derive(Serialize)]
struct GenerateArguments<'source> {
    action: GenerateAction,
    correlation: u64,
    profile: &'source str,
    stage: &'source str,
    source: &'source str,
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
            name: ToolName::Application,
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
            name: ToolName::Application,
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

fn mcp_generate_request(
    id: JsonRpcRequestId,
    profile: &str,
    stage: &str,
    source: &str,
) -> io::Result<String> {
    shape_request(
        id,
        GenerateArguments {
            action: GenerateAction::Generate,
            correlation: 71,
            profile,
            stage,
            source,
        },
    )
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
fn cli_and_mcp_admit_every_closed_compiler_target_without_stringly_core_vocabulary()
-> Result<(), TestError> {
    let profiles = [
        ("rust-2024", LanguageProfile::Rust(RustEdition::Rust2024)),
        (
            "typescript",
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        ),
        ("tsx", LanguageProfile::TypeScript(TypeScriptSource::Tsx)),
        (
            "python-3.14",
            LanguageProfile::Python(PythonVersion::Python314),
        ),
        ("go-1.25", LanguageProfile::Go(GoVersion::Go125)),
        ("java-21", LanguageProfile::Java(JavaRelease::Java21)),
        (
            "csharp-14",
            LanguageProfile::CSharp(CSharpVersion::CSharp14),
        ),
        ("c-23", LanguageProfile::C(CStandard::C23)),
        ("cxx-23", LanguageProfile::Cxx(CxxStandard::Cxx23)),
    ];
    for (profile_text, profile) in profiles {
        let cli = decode_cli(&[
            "generate".to_owned(),
            "71".to_owned(),
            profile_text.to_owned(),
            "lower-ir".to_owned(),
            "fn all_languages() {}".to_owned(),
        ])
        .map_err(io::Error::other)?;
        let ApplicationInput::Generate(cli_request) = cli else {
            return Err(io::Error::other("CLI compiler command was not generate").into());
        };
        assert_eq!(cli_request.target.profile, profile);
        assert_eq!(cli_request.target.stage, Stage::LowerIr);

        let body = mcp_generate_request(
            JsonRpcRequestId::Text(profile_text.to_owned()),
            profile_text,
            "lower-ir",
            "fn all_languages() {}",
        )?;
        let McpDecode::Accepted(envelope) = decode_mcp(body.as_bytes()) else {
            return Err(io::Error::other("MCP compiler command was rejected").into());
        };
        let McpRequest::Application(ApplicationInput::Generate(mcp_request)) = envelope.request
        else {
            return Err(io::Error::other("MCP compiler command was not generate").into());
        };
        assert_eq!(mcp_request.target, cli_request.target);
        assert_eq!(&*mcp_request.source, &*cli_request.source);
    }
    Ok(())
}

#[test]
fn source_budget_is_preserved_for_cli_and_mcp_at_exact_and_plus_one_boundaries()
-> Result<(), TestError> {
    let exact_source = "é".repeat(PORTABLE_LOCAL_SOURCE_LIMIT.bytes / "é".len());
    let exact = decode_cli(&[
        "generate".to_owned(),
        "72".to_owned(),
        "rust-2024".to_owned(),
        "lower-ir".to_owned(),
        exact_source,
    ])
    .map_err(io::Error::other)?;
    assert!(matches!(
        exact,
        ApplicationInput::Generate(request)
            if request.source.len() == PORTABLE_LOCAL_SOURCE_LIMIT.bytes
    ));

    let mut plus_one_source = "é".repeat(PORTABLE_LOCAL_SOURCE_LIMIT.bytes / "é".len());
    plus_one_source.push('x');
    let body = mcp_generate_request(
        JsonRpcRequestId::Number(72),
        "rust-2024",
        "lower-ir",
        &plus_one_source,
    )?;
    let McpDecode::Rejected(error) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("oversize MCP source was admitted").into());
    };
    assert_eq!(error.id, Some(serde_json::Value::from(72)));
    assert!(matches!(
        &error.cause,
        Some(AdapterErrorCause::SourceLength(RejectedSourceText { source, error }))
            if source == &plus_one_source
                && error.observed == PORTABLE_LOCAL_SOURCE_LIMIT.bytes + 1
                && error.limit == PORTABLE_LOCAL_SOURCE_LIMIT
    ));
    Ok(())
}

#[test]
fn malformed_compiler_wire_is_rejected_before_dispatch_with_its_request_identity()
-> Result<(), TestError> {
    let body = mcp_generate_request(
        JsonRpcRequestId::Number(73),
        "Rust",
        "lower-ir",
        "fn malformed() {}",
    )?;
    let McpDecode::Rejected(error) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("mixed-case compiler language was admitted").into());
    };
    assert_eq!(error.id, Some(serde_json::Value::from(73)));
    assert_eq!(error.error.code, AdapterErrorCode::InvalidShape);
    assert_eq!(error.error.field, AdapterField::Request);
    assert!(matches!(error.cause, Some(AdapterErrorCause::Json(_))));

    let Err(error) = decode_cli(&[
        "generate".to_owned(),
        "73".to_owned(),
        "rusty".to_owned(),
        "lower-ir".to_owned(),
        "fn malformed() {}".to_owned(),
    ]) else {
        return Err(io::Error::other("unknown CLI compiler language was admitted").into());
    };
    assert!(matches!(
        error.cause,
        Some(AdapterErrorCause::UnknownLanguage(observed)) if &*observed == "rusty"
    ));
    Ok(())
}

#[test]
fn post_parse_mcp_rejections_retain_request_id() -> Result<(), TestError> {
    let McpDecode::Rejected(error) = decode_mcp(
        br#"{"jsonrpc":"2.0","id":91,"method":"tools/call","params":{"name":"interface.application"}}"#,
    ) else {
        return Err(io::Error::other("malformed MCP request was accepted").into());
    };
    assert_eq!(error.id, Some(serde_json::Value::from(91)));
    assert_eq!(error.error.field, AdapterField::Arguments);
    Ok(())
}

#[test]
fn mcp_missing_required_generate_field_rejects_before_dispatch_with_id() -> Result<(), TestError> {
    let body = shape_request(
        JsonRpcRequestId::Number(401),
        MissingGenerateArguments {
            action: GenerateAction::Generate,
            correlation: 401,
            profile: "rust-2024",
            stage: "parse",
        },
    )?;
    let McpDecode::Rejected(error) = decode_mcp(body.as_bytes()) else {
        return Err(io::Error::other("missing generate source was accepted").into());
    };
    assert_eq!(error.id, Some(serde_json::Value::from(401)));
    assert_eq!(error.error.code, AdapterErrorCode::InvalidShape);
    assert_eq!(error.error.field, AdapterField::Request);
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
    assert_eq!(error.error.field, AdapterField::Request);
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
    assert_eq!(error.field, AdapterField::Generation);
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
    assert_eq!(error.error.field, AdapterField::Generation);
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
    assert_eq!(error.error.field, AdapterField::RequestId);
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
    assert_eq!(error.error.field, AdapterField::RequestId);
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
            correlation: interface_core::CorrelationId(31),
            expected,
            observed,
            bundle,
            budget: interface_core::ResourceBudget {
                ram_free: interface_core::ByteCount::from(4096),
                nvme_free: interface_core::ByteCount::from(8192),
                operations: interface_core::OperationBudget::from(1),
                retries: interface_core::RetryBudget::from(1),
                memory_pressure: interface_core::Pressure::Relaxed,
                storage_pressure: interface_core::Pressure::Relaxed,
                battery: interface_core::BatteryState::Normal,
            },
        })
    );
    Ok(())
}
