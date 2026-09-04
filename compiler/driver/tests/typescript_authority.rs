//! Public TypeScript authority boundary proofs.

use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::OsStringExt,
    path::Path,
    sync::atomic::AtomicBool,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use compiler_driver::{
    AuthorityFailure, CompileControl, CompileFailure, CompileOutput, CompileRequest,
    CompileScratch, NativeTool, ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection,
    compile, compile_ir,
};
use compiler_languages_typescript::{
    AuthorityError, Checker, CheckerError, Origin, Report, TypeTree, source_digest,
};
use compiler_vocabulary::{LanguageProfile, PythonVersion, Stage, TypeScriptSource};

const SIMPLE_SOURCE: &[u8] = b"export const n: number = 1;";
const GOLDEN_SOURCE: &[u8] = include_bytes!("../../languages/typescript/tests/fixtures/source.ts");
const GOLDEN_TRANSCRIPT: &[u8] =
    include_bytes!("../../languages/typescript/tests/transcripts/golden.json");

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
static ENVIRONMENT: OnceLock<Mutex<()>> = OnceLock::new();

struct CheckerEnvironment {
    previous: Option<OsString>,
}

impl CheckerEnvironment {
    fn set(value: &OsStr) -> Self {
        let previous = std::env::var_os("NUDOX_TYPESCRIPT_CHECKER_BIN");
        unsafe { std::env::set_var("NUDOX_TYPESCRIPT_CHECKER_BIN", value) };
        Self { previous }
    }
}

impl Drop for CheckerEnvironment {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("NUDOX_TYPESCRIPT_CHECKER_BIN", value) },
            None => unsafe { std::env::remove_var("NUDOX_TYPESCRIPT_CHECKER_BIN") },
        }
    }
}

fn assert_checker_terminal(value: &OsStr, expected: &str) {
    let _serial = ENVIRONMENT.get_or_init(|| Mutex::new(())).lock().unwrap();
    let _environment = CheckerEnvironment::set(value);
    let request = request(
        SIMPLE_SOURCE,
        SemanticAuthorityInput::None,
        tool(Path::new("/bin/true"), NativeTool::TypeScriptCompiler),
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
    );
    let mut diagnostic = [0; 1024];
    let ir_result = compile_ir(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
    );
    let failure = match ir_result {
        Ok(_) => panic!("unavailable checker must emit no IR"),
        Err(failure) => failure,
    };
    let CompileFailure::Authority { failure, .. } = failure else {
        panic!("checker unavailability must be an authority failure");
    };
    let AuthorityFailure::TypeScript { cause, .. } = failure else {
        panic!("checker unavailability must retain the TypeScript cause");
    };
    let AuthorityError::Checker { cause } = cause else {
        panic!("checker unavailability must retain the checker cause");
    };
    assert!(cause.to_string().contains(expected), "{cause}");

    let mut fragment = [0; 4096];
    let mut diagnostic = [0; 1024];
    let failure = match compile(
        request,
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
        CompileOutput {
            fragment_output: &mut fragment,
        },
    ) {
        Ok(_) => panic!("unavailable checker must emit no fragment"),
        Err(failure) => failure,
    };
    assert!(matches!(failure, CompileFailure::Authority { .. }));
}

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
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
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
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
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
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
    ) {
        Ok(_) => {
            assert!(false, "authority must bind to TypeScript");
            return;
        }
        Err(failure) => failure,
    };
    assert!(matches!(
        failure,
        CompileFailure::AuthorityInputProfileMismatch { .. }
    ));
}

const fn assert_copy<T: Copy>() {}

#[test]
fn public_request_and_authority_input_are_copy() {
    assert_copy::<compiler_driver::CompileRequest<'static, 'static, 'static>>();
    assert_copy::<SemanticAuthorityInput<'static>>();
}

#[test]
fn checker_spawn_failure_is_a_public_authority_terminal() {
    assert_checker_terminal(
        OsStr::new("/nonexistent/nudox-missing-checker"),
        "TypeScript checker could not be started",
    );
}

#[test]
fn checker_module_failure_is_a_public_authority_terminal() {
    let path =
        std::env::temp_dir().join(format!("nudox-typescript-checker-{}", std::process::id()));
    fs::write(&path, b"#!/bin/sh\nexit 3\n").expect("checker script");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("checker permissions");
    assert_checker_terminal(path.as_os_str(), "module unavailable");
    fs::remove_file(path).expect("checker script cleanup");
}

#[test]
fn checker_invalid_unicode_configuration_is_a_public_authority_terminal() {
    assert_checker_terminal(
        OsString::from_vec(b"/invalid/\xff/checker".to_vec()).as_os_str(),
        "NUDOX_TYPESCRIPT_CHECKER_BIN",
    );
}

fn compile_report<'diagnostic>(
    source: &'static [u8],
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
    report: &Report,
    diagnostic: &'diagnostic mut [u8],
) -> Result<compiler_driver::CompiledIr, CompileFailure<'diagnostic>> {
    compile_ir(
        request(
            source,
            SemanticAuthorityInput::TypeScript { report },
            tool(Path::new("/bin/true"), NativeTool::TypeScriptCompiler),
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        ),
        CompileScratch {
            diagnostic_output: diagnostic,
            native_work: Path::new("/tmp"),
        },
    )
}

#[test]
fn build_ir_populates_the_typescript_extension_plane() -> Result<(), CheckerError> {
    let report = Checker::default().decode(GOLDEN_TRANSCRIPT)?;
    let mut diagnostic = [0; 1024];
    let ir = compile_report(GOLDEN_SOURCE, &report, &mut diagnostic).map_err(|error| {
        CheckerError::Decode {
            message: format!("golden report did not lower: {error:?}"),
            transcript: String::new(),
        }
    })?;
    let plane = ir.ir.storage_columns().language_extensions.typescript;
    assert!(!plane.facts.is_empty());
    assert!(plane.facts.iter().any(|facts| facts.observed.is_some()));
    assert!(plane.ids.row_count() > 0);
    Ok(())
}

#[test]
fn mutated_computed_report_changes_the_ir_extension_plane() -> Result<(), CheckerError> {
    let report = Checker::default().decode(GOLDEN_TRANSCRIPT)?;
    let mut mutated = report.clone();
    let declaration = mutated
        .declarations
        .iter_mut()
        .find(|declaration| declaration.origin == Origin::Computed && declaration.name_start == 13)
        .ok_or_else(|| CheckerError::Decode {
            message: "computed n declaration missing".to_owned(),
            transcript: String::new(),
        })?;
    declaration.r#type = Some(TypeTree::Primitive {
        name: "string".to_owned(),
    });
    let mut original_diagnostic = [0; 1024];
    let original =
        compile_report(GOLDEN_SOURCE, &report, &mut original_diagnostic).map_err(|error| {
            CheckerError::Decode {
                message: format!("golden report did not lower: {error:?}"),
                transcript: String::new(),
            }
        })?;
    let mut changed_diagnostic = [0; 1024];
    let changed =
        compile_report(GOLDEN_SOURCE, &mutated, &mut changed_diagnostic).map_err(|error| {
            CheckerError::Decode {
                message: format!("mutated report did not lower: {error:?}"),
                transcript: String::new(),
            }
        })?;
    let original_plane = original.ir.storage_columns().language_extensions.typescript;
    let changed_plane = changed.ir.storage_columns().language_extensions.typescript;
    assert_eq!(
        original_plane.ids.row_count(),
        changed_plane.ids.row_count()
    );
    let differing_rows: Vec<_> = (0..original_plane.ids.row_count())
        .filter_map(|row| {
            (original_plane.get(compiler_ir::EntityId::new(row as u32))
                != changed_plane.get(compiler_ir::EntityId::new(row as u32)))
            .then_some(row)
        })
        .collect();
    assert!(!differing_rows.is_empty());
    Ok(())
}
