//! Public TypeScript authority boundary proofs.
//!
//! The fixtures stand in for the checker with an executable shell script and
//! probe non-UTF-8 native paths, both of which are Unix-specific.
#![cfg(unix)]

use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fs,
    io,
    os::unix::ffi::OsStringExt,
    path::Path,
    sync::atomic::AtomicBool,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    AuthorityFailure, CompileControl, CompileFailure, CompileOutput, CompileRequest,
    CompileScratch, NativeTool, ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection,
    compile, compile_ir,
};
use backend_frontend_typescript::legacy::{
    AuthorityError, Checker, CheckerError, Origin, Report, TypeTree, source_digest,
};
use backend_semantic::vocabulary::{LanguageProfile, PythonVersion, Stage, TypeScriptSource};

const SIMPLE_SOURCE: &[u8] = b"export const n: number = 1;";
const GOLDEN_SOURCE: &[u8] =
    include_bytes!("../../../frontends/typescript/tests/fixtures/source.ts");
const GOLDEN_TRANSCRIPT: &[u8] =
    include_bytes!("../../../frontends/typescript/tests/transcripts/golden.json");

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

fn empty_report(source: &[u8]) -> Report {
    let digest = source_digest(source);
    let source_digest = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Report {
        schema_version: 1,
        source_digest,
        declaration_file: false,
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
        declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
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
    #[allow(
        unsafe_code,
        reason = "`std::env::set_var` is `unsafe` in edition 2024; this test-only checker override is serialized through the `ENVIRONMENT` mutex."
    )]
    fn set(value: &OsStr) -> Self {
        let previous = std::env::var_os("NUDOX_TYPESCRIPT_CHECKER_BIN");
        unsafe { std::env::set_var("NUDOX_TYPESCRIPT_CHECKER_BIN", value) };
        Self { previous }
    }
}

impl Drop for CheckerEnvironment {
    #[allow(
        unsafe_code,
        reason = "`std::env::set_var`/`remove_var` are `unsafe` in edition 2024; this test-only checker override is serialized through the `ENVIRONMENT` mutex."
    )]
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("NUDOX_TYPESCRIPT_CHECKER_BIN", value) },
            None => unsafe { std::env::remove_var("NUDOX_TYPESCRIPT_CHECKER_BIN") },
        }
    }
}

fn assert_checker_terminal(value: &OsStr, expected: &str) -> TestResult<()> {
    let _serial = ENVIRONMENT
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| io::Error::other(format!("checker environment lock poisoned: {error}")))?;
    let _environment = CheckerEnvironment::set(value);
    let request = request(
        SIMPLE_SOURCE,
        SemanticAuthorityInput::None,
        tool(Path::new("/bin/true"), NativeTool::TypeScriptCompiler)?,
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
    Ok(())
}

fn tool<'path>(path: &'path Path, native: NativeTool) -> TestResult<ResolvedToolchain<'path>> {
    Ok(ResolvedToolchain::from_version(
        native,
        path,
        b"typescript-authority-test",
    )?)
}

#[test]
fn injected_report_reaches_public_ir() -> TestResult<()> {
    let report = empty_report(SIMPLE_SOURCE);
    let mut diagnostic = [0; 1024];
    let result = compile_ir(
        request(
            SIMPLE_SOURCE,
            SemanticAuthorityInput::TypeScript { report: &report },
            tool(Path::new("/bin/true"), NativeTool::TypeScriptCompiler)?,
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
    )
    .map_err(|error| {
        io::Error::other(format!("injected TypeScript authority did not compile: {error:?}"))
    })?;
    assert!(result.ir.entity_count() > 0);
    Ok(())
}

#[test]
fn report_for_mutated_source_is_a_source_binding_failure() -> TestResult<()> {
    let report = empty_report(SIMPLE_SOURCE);
    let mut source = SIMPLE_SOURCE.to_vec();
    source[0] = b' ';
    let mut diagnostic = [0; 1024];
    let failure = match compile_ir(
        request(
            &source,
            SemanticAuthorityInput::TypeScript { report: &report },
            tool(Path::new("/bin/true"), NativeTool::TypeScriptCompiler)?,
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
    ) {
        Ok(_) => {
            assert!(false, "stale report must be rejected");
            return Ok(());
        }
        Err(failure) => failure,
    };
    let CompileFailure::Authority { failure, .. } = failure else {
        assert!(false, "binding rejection must be an authority failure");
        return Ok(());
    };
    assert!(matches!(
        failure,
        AuthorityFailure::TypeScript {
            cause: backend_frontend_typescript::legacy::AuthorityError::Checker {
                cause: backend_frontend_typescript::legacy::CheckerError::SourceBinding { .. }
            },
            ..
        }
    ));
    Ok(())
}

#[test]
fn typescript_authority_rejects_a_non_typescript_profile() -> TestResult<()> {
    let report = empty_report(SIMPLE_SOURCE);
    let mut diagnostic = [0; 1024];
    let failure = match compile_ir(
        request(
            SIMPLE_SOURCE,
            SemanticAuthorityInput::TypeScript { report: &report },
            tool(Path::new("/bin/true"), NativeTool::Python)?,
            LanguageProfile::Python(PythonVersion::Python314),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/tmp"),
        },
    ) {
        Ok(_) => {
            assert!(false, "authority must bind to TypeScript");
            return Ok(());
        }
        Err(failure) => failure,
    };
    assert!(matches!(
        failure,
        CompileFailure::AuthorityInputProfileMismatch { .. }
    ));
    Ok(())
}

const fn assert_copy<T: Copy>() {}

#[test]
fn public_request_and_authority_input_are_copy() -> TestResult<()> {
    assert_copy::<backend_engine::driver::CompileRequest<'static, 'static, 'static>>();
    assert_copy::<SemanticAuthorityInput<'static>>();
    Ok(())
}

#[test]
fn checker_spawn_failure_is_a_public_authority_terminal() -> TestResult<()> {
    assert_checker_terminal(
        OsStr::new("/nonexistent/nudox-missing-checker"),
        "TypeScript checker could not be started",
    )?;
    Ok(())
}

#[test]
fn checker_module_failure_is_a_public_authority_terminal() -> TestResult<()> {
    let path =
        std::env::temp_dir().join(format!("nudox-typescript-checker-{}", std::process::id()));
    fs::write(&path, b"#!/bin/sh\nexit 3\n")?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    assert_checker_terminal(path.as_os_str(), "module unavailable")?;
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn checker_invalid_unicode_configuration_is_a_public_authority_terminal() -> TestResult<()> {
    assert_checker_terminal(
        OsString::from_vec(b"/invalid/\xff/checker".to_vec()).as_os_str(),
        "NUDOX_TYPESCRIPT_CHECKER_BIN",
    )?;
    Ok(())
}

fn compile_report<'diagnostic>(
    source: &'static [u8],
    report: &Report,
    diagnostic: &'diagnostic mut [u8],
) -> TestResult<backend_engine::driver::CompiledIr> {
    compile_ir(
        request(
            source,
            SemanticAuthorityInput::TypeScript { report },
            tool(Path::new("/bin/true"), NativeTool::TypeScriptCompiler)?,
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        ),
        CompileScratch {
            diagnostic_output: diagnostic,
            native_work: Path::new("/tmp"),
        },
    )
    .map_err(|error| io::Error::other(format!("TypeScript report compilation failed: {error:?}")))
    .map_err(Into::into)
}

#[test]
fn build_ir_populates_the_typescript_extension_plane() -> TestResult<()> {
    let report = Checker::default().decode(GOLDEN_TRANSCRIPT)?;
    let mut diagnostic = [0; 1024];
    let ir = compile_report(GOLDEN_SOURCE, &report, &mut diagnostic)?;
    let plane = ir.ir.storage_columns().language_extensions.typescript;
    assert!(!plane.facts.is_empty());
    assert!(plane.facts.iter().any(|facts| facts.observed.is_some()));
    assert!(plane.ids.row_count() > 0);
    Ok(())
}

#[test]
fn mutated_computed_report_changes_the_ir_extension_plane() -> TestResult<()> {
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
    let original = compile_report(GOLDEN_SOURCE, &report, &mut original_diagnostic)?;
    let mut changed_diagnostic = [0; 1024];
    let changed = compile_report(GOLDEN_SOURCE, &mutated, &mut changed_diagnostic)?;
    let original_plane = original.ir.storage_columns().language_extensions.typescript;
    let changed_plane = changed.ir.storage_columns().language_extensions.typescript;
    assert_eq!(
        original_plane.ids.row_count(),
        changed_plane.ids.row_count()
    );
    let differing_rows: Vec<_> = (0..original_plane.ids.row_count())
        .filter_map(|row| {
            (original_plane.get(backend_semantic::ir::EntityId::new(row as u32))
                != changed_plane.get(backend_semantic::ir::EntityId::new(row as u32)))
            .then_some(row)
        })
        .collect();
    assert!(!differing_rows.is_empty());
    Ok(())
}
