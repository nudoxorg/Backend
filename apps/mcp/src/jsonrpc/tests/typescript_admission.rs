use super::*;
use backend_library::interface::{
    AuthorityDiagnosticClass, AuthorityPhase, CompilerDiagnostic, CompilerTerminal,
    SourceAuthority, TypeScriptCompilerAdmissionCause as C, TypeScriptCompilerAdmissionDetail as D,
    TypeScriptCompilerApiExport as E, TypeScriptCompilerPackageRole as R,
    TypeScriptCompilerVersion as V,
};
use backend_semantic::vocabulary::{Language, Stage};
use backend_version::ContentId;

#[test]
fn installed_typescript_details_survive_mcp_routes_with_actual_named_token_admission() {
    let version = format!("5.9.3-{}", "a".repeat(122));
    for cause in [
        C::RuntimeFiles {
            observed: u64::MAX,
            maximum: u64::MAX - 1,
        },
        C::FilesystemEntries {
            observed: 4097,
            maximum: 4096,
        },
        C::DirectoryDepth {
            observed: 9,
            maximum: 8,
        },
        C::InputBytes {
            observed: 100,
            maximum: 99,
        },
        C::MissingApiEntry,
        C::MissingApiExport {
            export: E::GetAllowSyntheticDefaultImports,
        },
    ] {
        let detail = D {
            schema_version: 1,
            role: R::SelectedCompiler,
            version: V::new(&version),
            cause,
        };
        let failure = backend_library::PackageCompilerFailure::from_package_terminal(
            "main.ts",
            &CompilerTerminal::AuthorityAdmission {
                source: SourceAuthority {
                    identity: ContentId::from_canonical_bytes(b"source"),
                    byte_len: 6,
                },
                language: Language::TypeScript,
                stage: Stage::LowerIr,
                phase: AuthorityPhase::Resolve,
                class: AuthorityDiagnosticClass::Authority,
                diagnostic: CompilerDiagnostic::from_native(
                    b"/private/sdk authority.secret",
                    29,
                    false,
                )
                .map(|diagnostic| diagnostic.with_typescript_admission(detail.clone())),
            },
        )
        .unwrap()
        .unwrap();
        for (tool, arguments, _) in compiler_error_routes() {
            let mut server = ready(Fake {
                surface_error: Some(compiler_client_error(&failure)),
                ..Fake::default()
            });
            let response = request(
                &mut server,
                "tools/call",
                &json!({"name":tool, "arguments":arguments}),
            );
            let structured = tool_failure(&response, "compiler-refused");
            assert_eq!(
                structured["compiler_failure"],
                serde_json::to_value(&failure).unwrap()
            );
            assert!(structured.get("compiler_tool_requirement").is_none());
            let text = text_of(&response["result"]);
            assert!(text.contains(&detail.detail()));
            for private in ["/private", "authority.secret", "RAW SOURCE", "NUDOX_TSC"] {
                assert!(!text.contains(private));
            }
            assert!(wire_fits(&response, ResponseTransport::HttpBody));
            assert!(wire_tokens(&response).unwrap() <= MCP_CL100K_TOKEN_LIMIT);
            assert_context_bounded(&response);
        }
    }
}
