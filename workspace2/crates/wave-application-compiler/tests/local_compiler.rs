//! Public local-compilation journey through real Rust lowering and durable publication.

use std::{
    env, fs, io,
    num::NonZeroUsize,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use nudox_compile_driver::{NativeTool, ResolvedToolchain};
use nudox_compile_vocab::{Language, Stage};
use nudox_durable_journal::PublicationLimits;
use nudox_id::{ContentId, SourceFactDomain};
use thiserror::Error;
use wave_application_compiler::{
    LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerOpenError,
    LocalCompilerScratch,
};
use wave_application_core::{
    ApplicationInput, ApplicationReply, ApplicationService, CompilerTerminal, CorrelationId,
    DiagnosticCode, DiagnosticDetail, InputText, ReplyBody, Terminal,
};

static FIXTURE_ORDINAL: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
enum LocalCompilerTestError {
    #[error("fixture filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("test did not receive an absolute stable rustc toolchain root")]
    MissingToolchain,
    #[error("stable rustc executable path could not be canonicalized")]
    RustcPath(#[source] io::Error),
    #[error("resolved rustc toolchain was rejected")]
    Toolchain(#[from] nudox_compile_driver::ToolchainResolutionError),
    #[error("durable publication limits were rejected")]
    Limits(#[from] nudox_durable_journal::PublicationLimitError),
    #[error("local compiler publication owner could not open")]
    CompilerOpen(#[from] LocalCompilerOpenError),
    #[error("local compiler publication owner could not shut down")]
    Shutdown(#[from] nudox_durable_journal::ShutdownError),
    #[error("transport fixture text was rejected")]
    Input(wave_application_core::InputTextError),
    #[error("monotonic fixture deadline overflowed")]
    DeadlineOverflow,
    #[error("expected {expected}, observed {observed}")]
    Assertion {
        expected: &'static str,
        observed: &'static str,
    },
}

impl From<wave_application_core::InputTextError> for LocalCompilerTestError {
    fn from(error: wave_application_core::InputTextError) -> Self {
        Self::Input(error)
    }
}

struct Fixture {
    directory: PathBuf,
    artifacts: PathBuf,
    journal: PathBuf,
    native_work: PathBuf,
}

impl Fixture {
    fn create() -> Result<Self, LocalCompilerTestError> {
        let ordinal = FIXTURE_ORDINAL.fetch_add(1, Ordering::Relaxed);
        let directory = env::temp_dir().join(format!(
            "wave-application-compiler-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        let artifacts = directory.join("artifacts");
        let journal = directory.join("journal");
        let native_work = directory.join("native-work");
        fs::create_dir(&native_work)?;
        Ok(Self {
            directory,
            artifacts,
            journal,
            native_work,
        })
    }

    fn remove(self) -> Result<(), io::Error> {
        fs::remove_dir_all(self.directory)
    }
}

#[test]
fn configured_rust_compiler_lowers_publishes_and_preserves_exact_terminals()
-> Result<(), LocalCompilerTestError> {
    let fixture = Fixture::create()?;
    let cancelled = AtomicBool::new(false);
    let mut scratch = LocalCompilerScratch::default();
    let rustc = rustc_path()?;
    let config = LocalCompilerConfig {
        toolchain: ResolvedToolchain::from_version(
            NativeTool::Rustc,
            &rustc,
            b"wave-application-local-compiler-test-v1",
        )?,
        artifact_directory: &fixture.artifacts,
        journal_directory: &fixture.journal,
        native_work_directory: &fixture.native_work,
        control: LocalCompilerControl {
            deadline: future_deadline()?,
            cancelled: &cancelled,
        },
    };
    let compiler = LocalCompiler::create(config, one_slot()?, &mut scratch)?;
    let mut service = ApplicationService::with_compiler(compiler);

    assert_generated(&service.execute(&generate(
        Language::Rust,
        "lower-ir",
        "pub const READY: i32 = 1;",
    )?))?;

    assert_toolchain_mismatch(&service.execute(&generate(
        Language::Clang,
        "lower-ir",
        "const char *ready = \"yes\";",
    )?))?;

    assert_unsupported_stage(&service.execute(&generate(
        Language::Rust,
        "parse",
        "pub const READY: i32 = 1;",
    )?))?;

    cancelled.store(true, Ordering::Release);
    assert_cancelled(
        &service.execute(&generate(
            Language::Rust,
            "lower-ir",
            "pub const STOP: i32 = 1;",
        )?),
        b"pub const STOP: i32 = 1;",
    )?;

    service.compiler.shutdown()?;
    fixture.remove()?;
    Ok(())
}

fn assert_generated(reply: &ApplicationReply) -> Result<(), LocalCompilerTestError> {
    let ReplyBody::Generated(facts) = &reply.body else {
        return Err(observed(
            "a generated durable compilation",
            "another reply body",
        ));
    };
    if reply.terminal != (Terminal::Complete { emitted: 1 }) {
        return Err(observed(
            "one complete generated terminal",
            "another terminal",
        ));
    }
    if facts.recipe.language != Language::Rust || facts.recipe.stage != Stage::LowerIr {
        return Err(observed(
            "Rust LowerIr recipe authority",
            "another recipe authority",
        ));
    }
    if facts.source.byte_len != 25 {
        return Err(observed(
            "exact Rust source length",
            "different source length",
        ));
    }
    if facts
        .publication
        .binding
        .as_ref()
        .iter()
        .all(|byte| *byte == 0)
    {
        return Err(observed(
            "a nonzero immutable publication binding authority",
            "all-zero binding authority",
        ));
    }
    Ok(())
}

const fn assert_toolchain_mismatch(reply: &ApplicationReply) -> Result<(), LocalCompilerTestError> {
    match reply.diagnostic {
        Some(wave_application_core::Diagnostic {
            code: DiagnosticCode::CompilerTerminal,
            detail:
                DiagnosticDetail::Compiler(CompilerTerminal::Toolchain {
                    language: Language::Clang,
                    stage: Stage::LowerIr,
                    selected: NativeTool::Clang,
                    configured: Some(NativeTool::Rustc),
                    ..
                }),
        }) => Ok(()),
        _ => Err(observed(
            "typed selected/configured toolchain mismatch",
            "another diagnostic",
        )),
    }
}

fn assert_unsupported_stage(reply: &ApplicationReply) -> Result<(), LocalCompilerTestError> {
    (reply.diagnostic.map(|diagnostic| diagnostic.code)
        == Some(DiagnosticCode::UnsupportedCompilerStage))
    .then_some(())
    .ok_or_else(|| observed("typed unsupported compiler stage", "another diagnostic"))
}

fn assert_cancelled(reply: &ApplicationReply, source: &[u8]) -> Result<(), LocalCompilerTestError> {
    let Ok(byte_len) = u32::try_from(source.len()) else {
        return Err(observed(
            "u32 source authority width",
            "larger source input",
        ));
    };
    let expected = ContentId::<SourceFactDomain>::from_canonical_bytes(source);
    match reply.diagnostic {
        Some(wave_application_core::Diagnostic {
            code: DiagnosticCode::CompilerTerminal,
            detail:
                DiagnosticDetail::Compiler(CompilerTerminal::Cancelled {
                    attempted,
                    diagnostic,
                }),
        }) if attempted.source.identity == expected
            && attempted.source.byte_len == byte_len
            && diagnostic.byte_len == 0
            && !diagnostic.truncated
            && diagnostic.bytes == [0; wave_application_core::MAX_COMPILER_DIAGNOSTIC_BYTES] =>
        {
            Ok(())
        }
        _ => Err(observed(
            "typed compiler cancellation",
            "another diagnostic",
        )),
    }
}

fn rustc_path() -> Result<PathBuf, LocalCompilerTestError> {
    let root =
        env::var_os("NUDOX_STABLE_TOOLCHAIN").ok_or(LocalCompilerTestError::MissingToolchain)?;
    let executable = PathBuf::from(root).join("bin/rustc");
    let executable = executable
        .canonicalize()
        .map_err(LocalCompilerTestError::RustcPath)?;
    Ok(executable)
}

fn future_deadline() -> Result<Instant, LocalCompilerTestError> {
    Instant::now()
        .checked_add(Duration::from_secs(10))
        .ok_or(LocalCompilerTestError::DeadlineOverflow)
}

fn one_slot() -> Result<PublicationLimits, LocalCompilerTestError> {
    PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
        .map_err(LocalCompilerTestError::Limits)
}

fn generate(
    language: Language,
    stage: &str,
    source: &str,
) -> Result<ApplicationInput, LocalCompilerTestError> {
    Ok(ApplicationInput::Generate {
        correlation: CorrelationId(1),
        language: language_input(language)?,
        stage: InputText::try_from_str(stage)?,
        source: InputText::try_from_str(source)?,
    })
}

fn language_input(language: Language) -> Result<InputText, LocalCompilerTestError> {
    let text = match language {
        Language::Rust => "rust",
        Language::TypeScript => "typescript",
        Language::Python => "python",
        Language::Go => "go",
        Language::Java => "java",
        Language::CSharp => "csharp",
        Language::Clang => "clang",
    };
    InputText::try_from_str(text).map_err(LocalCompilerTestError::Input)
}

const fn observed(expected: &'static str, observed: &'static str) -> LocalCompilerTestError {
    LocalCompilerTestError::Assertion { expected, observed }
}
