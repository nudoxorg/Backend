//! Public TypeScript authority boundary proofs.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    AuthorityFailure, CompileControl, CompileFailure, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use compiler_languages_typescript::{Report, source_digest};
use compiler_vocabulary::{LanguageProfile, PythonVersion, Stage, TypeScriptSource};

const SIMPLE_SOURCE: &[u8] = b"export const n: number = 1;";

fn empty_report(source: &[u8]) -> Report {
    let digest = source_digest(source);
    let source_digest = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Report {
        schema_version: 1,
        source_digest,
        diagnostics: Box::new([]),
        declarations: Box::new([]),
        references: Box::new([]),
        narrowings: Box::new([]),
    }
}

fn request<'source, 'toolchain>(
    source: &'source [u8],
    authority: SemanticAuthorityInput<'source>,
    toolchain: ResolvedToolchain<'toolchain>,
    profile: LanguageProfile,
) -> CompileRequest<'source, 'toolchain, 'static> {
    CompileRequest {
        profile,
        stage: Stage::LowerIr,
        source,
        toolchain: ToolchainSelection::ResolvedNative(toolchain),
        authority,
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: &CANCELLED,
        },
    }
}

static CANCELLED: AtomicBool = AtomicBool::new(false);

fn tool<'path>(path: &'path Path, native: NativeTool) -> ResolvedToolchain<'path> {
    ResolvedToolchain::from_version(native, path, b"typescript-authority-test")
        .expect("absolute test tool path")
}

#[test]
fn injected_report_reaches_public_ir() {
    let report = empty_report(SIMPLE_SOURCE);
    let mut diagnostic = [0; 1024];
    let result = compile_ir(
        request(
            SIMPLE_SOURCE,
            SemanticAuthorityInput::TypeScript { report: &report },
            tool(Path::new("/bin/true"), NativeTool::TypeScriptCompiler),
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        ),
        CompileScratch { diagnostic_output: &mut diagnostic, native_work: Path::new("/tmp") },
    )
    .expect("injected checker authority must compile");
    assert!(result.ir.entity_count() > 0);
}

#[test]
fn report_for_mutated_source_is_a_source_binding_failure() {
    let report = empty_report(SIMPLE_SOURCE);
    let mut source = SIMPLE_SOURCE.to_vec();
    source[0] = b' ';
    let mut diagnostic = [0; 1024];
    let failure = match compile_ir(
        request(
            &source,
            SemanticAuthorityInput::TypeScript { report: &report },
            tool(Path::new("/bin/true"), NativeTool::TypeScriptCompiler),
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        ),
        CompileScratch { diagnostic_output: &mut diagnostic, native_work: Path::new("/tmp") },
    ) {
        Ok(_) => {
            assert!(false, "stale report must be rejected");
            return;
        }
        Err(failure) => failure,
    };
    let CompileFailure::Authority { failure, .. } = failure else {
        assert!(false, "binding rejection must be an authority failure");
        return;
    };
    assert!(matches!(
        failure,
        AuthorityFailure::TypeScript {
            cause: compiler_languages_typescript::AuthorityError::Checker {
                cause: compiler_languages_typescript::CheckerError::SourceBinding { .. }
            },
            ..
        }
    ));
}

#[test]
fn typescript_authority_rejects_a_non_typescript_profile() {
    let report = empty_report(SIMPLE_SOURCE);
    let mut diagnostic = [0; 1024];
    let failure = match compile_ir(
        request(
            SIMPLE_SOURCE,
            SemanticAuthorityInput::TypeScript { report: &report },
            tool(Path::new("/bin/true"), NativeTool::Python),
            LanguageProfile::Python(PythonVersion::Python314),
        ),
        CompileScratch { diagnostic_output: &mut diagnostic, native_work: Path::new("/tmp") },
    ) {
        Ok(_) => {
            assert!(false, "authority must bind to TypeScript");
            return;
        }
        Err(failure) => failure,
    };
    assert!(matches!(failure, CompileFailure::AuthorityInputProfileMismatch { .. }));
}

const fn assert_copy<T: Copy>() {}

#[test]
fn public_request_and_authority_input_are_copy() {
    assert_copy::<compiler_driver::CompileRequest<'static, 'static, 'static>>();
    assert_copy::<SemanticAuthorityInput<'static>>();
}
