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
fn compiler_unavailable_tool_preserves_the_selected_native_setup_requirement() {
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
    assert_eq!(fault.compiler_failure(), Some(&failure));
    assert!(fault.cause().sentence().contains("requires tsc"));
    assert!(fault.cause().sentence().contains("Set NUDOX_TSC"));
    assert!(
        FaultDto::new(&fault)
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
fn valid_compiler_json_and_coordinate_strings_do_not_elevate_protocol_text() {
    let failure = setup_failure("src/main.ts", None);
    let bytes = failure
        .encode_bounded_json()
        .expect("valid compiler DTO JSON");
    assert_eq!(
        PackageCompilerFailure::decode_bounded_json(&bytes),
        Ok(failure.clone())
    );
    for message in [
        String::from_utf8(bytes).expect("JSON UTF-8"),
        serde_json::to_string(&fault_value(&compiler_fault(&failure))).expect("valid fault JSON"),
        "/abs/trap::src/hidden.ts:12::Secret".to_owned(),
        "library record not found".to_owned(),
    ] {
        let fault = Fault::from_client_error(
            &backend_client::ClientError::Protocol(message),
            Operand::Argument("request".to_owned()),
        );
        assert_eq!(fault.slug(), FaultSlug::Protocol);
        assert_eq!(fault.cause().slug(), CauseSlug::Unproven);
        assert_eq!(fault.operand(), &Operand::Argument("request".to_owned()));
        assert!(fault.compiler_failure().is_none());
        let dto = FaultDto::new(&fault);
        assert!(dto.compiler_failure.is_none());
        assert!(dto.compiler_tool_requirement.is_none());
    }
    // The identical facts are elevated only through the typed command boundary.
    assert_eq!(compiler_fault(&failure).compiler_failure(), Some(&failure));
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

#[test]
fn compiler_index_receipt_uses_shared_human_sentence_and_keeps_exact_machine_facts() {
    use backend_library::{
        IndexJobOutcome, IndexJobTerminal, IndexJobTicket, PackageReference, ProductText,
        SurfaceReply,
    };
    let failure = setup_failure("src/main.ts", None);
    let terminal = IndexJobTerminal {
        ticket: IndexJobTicket::new(
            std::num::NonZeroU64::new(1).expect("nonzero job"),
            [7; 16],
            PackageReference::parse(
                std::env::temp_dir()
                    .join("tiny-ts")
                    .to_string_lossy()
                    .into_owned(),
            )
            .expect("absolute project"),
        ),
        outcome: IndexJobOutcome::RefusedWithCompilerFailure {
            detail: ProductText::new("RAW LEGACY JSON AND DIAGNOSTIC")
                .expect("bounded legacy detail"),
            failure: failure.clone(),
        },
    };
    for reply in [
        SurfaceReply::IndexTerminal(terminal.clone()),
        SurfaceReply::IndexProgress(backend_library::IndexJobObservation::Terminal(
            terminal.clone(),
        )),
    ] {
        let dto = ProductDto::new(&product_view(&reply));
        let tags = dto.records[0].tags.join(" ");
        assert!(tags.contains(compiler_fault(&failure).cause().sentence()));
        assert!(!tags.contains("RAW LEGACY"));
        assert!(!tags.contains("content:"));
        assert!(!tags.contains("compiler_failure {"));
        let projection = dto.index_job.expect("exact job projection remains");
        let value = serde_json::to_value(projection).expect("machine job facts");
        let expected = serde_json::to_value(&terminal).expect("exact terminal");
        match &reply {
            SurfaceReply::IndexTerminal(_) => assert_eq!(value["value"], expected),
            SurfaceReply::IndexProgress(_) => assert_eq!(value["value"]["detail"], expected),
            _ => unreachable!(),
        }
    }
}

#[test]
fn partial_publication_keeps_useful_languages_all_refusals_and_exact_machine_basis() {
    use backend_library::{
        IndexJobOutcome, IndexJobPartialPublication, IndexJobTerminal, IndexJobTicket,
        IndexOperationProfileRefusal, IndexOperationPublicationReceipt,
        IndexOperationSemanticCoverage, IndexOperationSemanticProfileState,
        IndexOperationSemanticUnavailableReason, IndexOperationSourceProfile,
        IndexSourceCaptureSummary, PackageReference, SemanticLanguageProfile,
    };
    let package = PackageReference::parse("/abs/mixed-docs").expect("package");
    let py = SemanticLanguageProfile::from_name("python").expect("Python");
    let ts = SemanticLanguageProfile::from_name("typescript").expect("TypeScript");
    let mut profiles = vec![
        IndexOperationSourceProfile {
            profile: py,
            source_version: [4; 32],
            input_digest: [5; 32],
            observation_sequence: 2,
            source_count: 236,
            state: IndexOperationSemanticProfileState::Published {
                generation: [6; 32],
                coverage: IndexOperationSemanticCoverage::Complete,
            },
        },
        IndexOperationSourceProfile {
            profile: ts,
            source_version: [4; 32],
            input_digest: [7; 32],
            observation_sequence: 3,
            source_count: 626,
            state: IndexOperationSemanticProfileState::Unavailable {
                reason: IndexOperationSemanticUnavailableReason::Rejected,
            },
        },
    ];
    profiles.sort_by_key(|profile| profile.profile);
    let root = backend_library::view_state_root(&[]);
    let basis =
        backend_library::Basis::new(root, backend_library::object_version(b"partial-source"));
    let view = backend_library::ViewRoot::new_incomplete(
        backend_library::view_key(b"partial-view"),
        basis,
        backend_library::Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        Vec::new(),
        Vec::new(),
    )
    .expect("view");
    let partial = IndexJobPartialPublication {
        package: package.clone(),
        receipt: IndexOperationPublicationReceipt::from_published_view(
            Some([8; 32]),
            [9; 32],
            [10; 32],
            3,
            &view,
            backend_library::Cursor::for_view_root(&view),
        )
        .expect("receipt"),
        source_capture: IndexSourceCaptureSummary {
            producer_package: package.clone(),
            request_identity: [1; 32],
            commit_identity: [2; 32],
            workspace_root: [3; 32],
            workspace_sequence: 2,
            profiles: profiles.into_boxed_slice(),
        },
        refused_profiles: vec![IndexOperationProfileRefusal {
            profile: ts,
            reason: IndexOperationSemanticUnavailableReason::Rejected,
            compiler_failure: Some(setup_failure("src/frontend/.prettierrc.js", None)),
        }]
        .into_boxed_slice(),
    };
    partial.admit().expect("checked partial fixture");
    let fault = Fault::from_command_failure(
        &CommandFailure::PartiallyPublished(partial.clone()),
        Operand::Path("/abs/mixed-docs".to_owned()),
    );
    assert_eq!(fault.slug().as_str(), "partially-published");
    let sentence = fault.cause().sentence();
    assert!(sentence.contains("python: published 236 source files with Complete coverage"));
    assert!(sentence.contains("typescript: unavailable"));
    assert!(sentence.contains("requires tsc"));
    assert!(sentence.contains("Set NUDOX_TSC"));
    assert!(sentence.contains("refused profiles do not have current semantic coverage"));
    assert!(
        fault.compiler_failure().is_none(),
        "do not substitute the first refusal for the whole partition"
    );
    let json = serde_json::to_value(FaultDto::new(&fault)).expect("CLI/MCP structured fact");
    assert_eq!(
        json["partial_publication"],
        serde_json::to_value(&partial).expect("exact basis")
    );
    assert!(
        json["partial_publication"]["source_capture"]
            .get("operation_key")
            .is_none()
    );
    let mut forged = json.clone();
    forged["partial_publication"]["refused_profiles"] = serde_json::json!([]);
    assert!(serde_json::from_value::<FaultDto>(forged).is_err());
    let terminal = IndexJobTerminal {
        ticket: IndexJobTicket::new(
            std::num::NonZeroU64::new(1).expect("ticket"),
            [11; 16],
            package,
        ),
        outcome: IndexJobOutcome::PartiallyPublished(partial),
    };
    let dto = ProductDto::new(&product_view(
        &backend_library::SurfaceReply::IndexTerminal(terminal),
    ));
    assert!(dto.records[0].tags.join(" ").contains(sentence));
    assert!(
        dto.records[0]
            .tags
            .iter()
            .any(|tag| tag == "outcome partially-published")
    );
}
