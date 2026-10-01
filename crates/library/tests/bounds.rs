//! Exercises the `backend-library` tests bounds contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::io::{self, Cursor};

use backend_semantic::vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion,
    RustEdition, Stage, TypeScriptSource,
};
use backend_library::interface::{
    ApplicationInput, CapabilityDomain, ContentId, GenerationId, InconsistentRecovery,
    IndexSnapshotId, PORTABLE_LOCAL_SOURCE_LIMIT, Pin,
};
use backend_library::protocol::{
    AdapterErrorCause, AdapterField, CANONICAL_CONTENT_ID_TEXT_BYTES,
    CanonicalContentIdDecodeError, MAX_HEADER_LINE_BYTES, MAX_HEADER_LINES, decode_cli, read_frame,
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
    Ok(())
}

#[test]
fn cli_admits_every_closed_compiler_target_without_stringly_core_vocabulary()
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
    }
    Ok(())
}

#[test]
fn source_budget_is_preserved_for_the_cli_at_its_exact_boundary() -> Result<(), TestError> {
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

    Ok(())
}

#[test]
fn malformed_compiler_wire_is_rejected_before_dispatch() -> Result<(), TestError> {
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
            correlation: backend_library::interface::CorrelationId(31),
            expected,
            observed,
            bundle,
            budget: backend_library::interface::ResourceBudget {
                ram_free: backend_library::interface::ByteCount::from(4096),
                nvme_free: backend_library::interface::ByteCount::from(8192),
                operations: backend_library::interface::OperationBudget::from(1),
                retries: backend_library::interface::RetryBudget::from(1),
                memory_pressure: backend_library::interface::Pressure::Relaxed,
                storage_pressure: backend_library::interface::Pressure::Relaxed,
                battery: backend_library::interface::BatteryState::Normal,
            },
        })
    );
    Ok(())
}
