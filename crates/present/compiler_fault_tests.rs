//! Closed compiler refusals at the shared CLI/MCP presentation boundary.

use super::*;
use backend_library::interface::{
    CompilerAttempt, CompilerFragmentFailure, CompilerTerminal, SourceAuthority,
};
use backend_library::{CommandFailure, PackageCompilerFailure};
use backend_semantic::ir::FragmentError;
use backend_semantic::vocabulary::{Language, NativeTool, Stage};
use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain};

fn source() -> SourceAuthority {
    let bytes = b"export function welcome(): string { return 'hello'; }";
    SourceAuthority {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(bytes),
        byte_len: u32::try_from(bytes.len()).expect("small fixture"),
    }
}

fn setup_failure(path: &str, configured: Option<NativeTool>) -> PackageCompilerFailure {
    PackageCompilerFailure::from_package_terminal(
        path,
        &CompilerTerminal::Toolchain {
            source: source(),
            language: Language::TypeScript,
            stage: Stage::LowerIr,
            selected: NativeTool::TypeScriptCompiler,
            configured,
        },
    )
    .expect("valid typed setup terminal")
    .expect("setup failure is projected")
}

fn compiler_fault(failure: &PackageCompilerFailure) -> Fault {
    Fault::from_client_error(
        &backend_client::ClientError::CommandFailed(CommandFailure::CompilerRefused {
            // Legacy text is not evidence: do not copy it into either human
            // detail or the producer-supplied machine facts.
            detail: "RAW SOURCE DIAGNOSTIC AND LEGACY JSON".repeat(2_000),
            failure: failure.clone(),
        }),
        Operand::Text("/abs/tiny-ts".to_owned()),
    )
}

#[test]
fn compiler_setup_fault_retains_exact_facts_and_actionable_tool_requirement() {
    for configured in [None, Some(NativeTool::GoCompiler)] {
        let failure = setup_failure("src/main.ts", configured);
        let fault = compiler_fault(&failure);
        assert_eq!(fault.slug(), FaultSlug::CompilerRefused);
        assert_eq!(fault.cause().slug(), CauseSlug::Refused);
        assert_eq!(fault.compiler_failure(), Some(&failure));
        let text = fault.render(None);
        assert!(text.contains("src/main.ts: setup/toolchain_configuration_mismatch"));
        assert!(text.contains("Set NUDOX_TSC to an absolute path"));
        assert!(!text.contains("RAW SOURCE DIAGNOSTIC"));
        assert!(!text.contains("content:"));
        assert!(!text.contains("facts="));
        assert!(text.len() < 400);

        let dto = FaultDto::new(&fault);
        assert_eq!(dto.compiler_failure, Some(failure.clone()));
        let requirement = dto
            .compiler_tool_requirement
            .as_ref()
            .expect("tool setup facts");
        assert_eq!(requirement.executable, "tsc");
        assert_eq!(requirement.configuration_variable, "NUDOX_TSC");
        assert_eq!(
            requirement.configured_tool,
            failure.configured_native_tool()
        );
        assert!(requirement.configuration_required);
        let value = fault_value(&fault);
        assert_eq!(
            value["compiler_failure"],
            serde_json::to_value(&failure).expect("exact DTO")
        );
        assert_eq!(value["compiler_failure"]["phase"], "setup");
        assert_eq!(
            value["compiler_failure"]["recipe_identity"],
            serde_json::Value::Null
        );
        assert_eq!(
            serde_json::from_value::<FaultDto>(value).expect("shared CLI/MCP DTO"),
            dto
        );
        let retargeted = fault
            .clone()
            .about(Operand::Text("another operand".to_owned()));
        assert_eq!(retargeted.compiler_failure(), Some(&failure));
    }
}

#[test]
fn compiler_unavailable_tool_does_not_promise_configuration_will_add_capability() {
    let failure = PackageCompilerFailure::from_package_terminal(
        "src/main.ts",
        &CompilerTerminal::ToolingUnavailable {
            source: source(),
            language: Language::TypeScript,
            stage: Stage::LowerIr,
            tool: NativeTool::TypeScriptCompiler,
        },
    )
    .expect("valid terminal")
    .expect("unavailable tool projected");
    let fault = compiler_fault(&failure);
    assert!(
        fault
            .cause()
            .sentence()
            .contains("unavailable to this deployment")
    );
    assert!(!fault.cause().sentence().contains("Set NUDOX_TSC"));
    assert!(
        !FaultDto::new(&fault)
            .compiler_tool_requirement
            .expect("required tool remains visible")
            .configuration_required
    );
}

#[test]
fn compiler_fragment_fault_keeps_phase_and_kind_without_an_invented_tool_requirement() {
    let failure = PackageCompilerFailure::from_fragment_failure(
        "src/main.ts",
        CompilerAttempt {
            source: source(),
            recipe: ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"recipe"),
        },
        &CompilerFragmentFailure::validate(FragmentError::TruncatedHeader {
            required: 32,
            actual: 7,
        }),
    )
    .expect("typed fragment refusal");
    let fault = compiler_fault(&failure);
    assert!(
        fault
            .cause()
            .sentence()
            .contains("validate/validate_truncated_header")
    );
    assert_eq!(fault.compiler_failure(), Some(&failure));
    assert!(FaultDto::new(&fault).compiler_tool_requirement.is_none());
    assert!(!fault.render(None).contains("content:"));
}

#[test]
fn compiler_like_protocol_messages_remain_unproven_protocol_errors() {
    let failure = setup_failure("src/main.ts", None);
    let flattened = CommandFailure::CompilerRefused {
        detail: "setup refused".to_owned(),
        failure,
    }
    .to_string();
    for message in [
        flattened,
        "the frame length prefix was truncated".to_owned(),
    ] {
        let fault = Fault::from_client_error(
            &backend_client::ClientError::Protocol(message),
            Operand::Text("/abs/tiny-ts".to_owned()),
        );
        assert_eq!(fault.slug(), FaultSlug::Protocol);
        assert_eq!(fault.cause().slug(), CauseSlug::Unproven);
        assert!(fault.compiler_failure().is_none());
        let value = fault_value(&fault);
        assert!(value.get("compiler_failure").is_none());
        assert!(value.get("compiler_tool_requirement").is_none());
    }
}

#[test]
fn compiler_fault_packet_is_bounded_and_inconsistent_typed_facts_are_refused() {
    let path = "\"".repeat(PackageCompilerFailure::MAX_RELATIVE_PATH_BYTES);
    let failure = setup_failure(&path, None);
    let value = fault_value(&compiler_fault(&failure));
    let encoded =
        encode_value(&value, DEFAULT_RESPONSE_BUDGET_BYTES).expect("complete bounded fault");
    assert!(encoded.bytes.len() < DEFAULT_RESPONSE_BUDGET_BYTES);
    assert!(encode_value(&value, 1_024).is_err());
    let mut invalid = value;
    invalid["compiler_failure"]["kind_tag"] = serde_json::json!("tooling_unavailable");
    assert!(serde_json::from_value::<FaultDto>(invalid).is_err());
}
