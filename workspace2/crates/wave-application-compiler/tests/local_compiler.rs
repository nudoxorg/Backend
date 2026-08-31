//! Public local-compilation journey through real Rust lowering and durable publication.

use std::{
    env, fs, io,
    num::NonZeroUsize,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use nudox_compile_driver::{NativeTool, ResolvedToolchain, ToolchainSelection};
use nudox_compile_vocab::{Language, Stage};
use nudox_durable_journal::PublicationLimits;
use nudox_id::{ContentId, SourceFactDomain};
use thiserror::Error;
use wave_application_compiler::{
    LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerOpenError,
    LocalCompilerScratch, LocalCompilerTimeout, LocalCompilerTimeoutError, LocalToolchainSet,
    LocalToolchainSetError,
};
use wave_application_core::{
    ApplicationInput, ApplicationReply, ApplicationService, CompilerTerminal, CorrelationId,
    Diagnostic, DiagnosticCode, DiagnosticDetail, InputText, ReplyBody, Terminal,
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
    #[error("local compiler toolchain table was rejected")]
    ToolchainSet(#[from] LocalToolchainSetError),
    #[error("local compiler timeout was rejected")]
    Timeout(#[from] LocalCompilerTimeoutError),
    #[error("durable publication limits were rejected")]
    Limits(#[from] nudox_durable_journal::PublicationLimitError),
    #[error("local compiler publication owner could not open")]
    CompilerOpen(#[from] LocalCompilerOpenError),
    #[error("local compiler publication owner could not shut down")]
    Shutdown(#[from] nudox_durable_journal::ShutdownError),
    #[error("transport fixture text was rejected")]
    Input(wave_application_core::InputTextError),
    #[error("generated reply facts did not satisfy the public journey")]
    GeneratedBody { observed: ReplyBodyClass },
    #[error("generated reply had an unexpected terminal")]
    GeneratedTerminal { observed: Terminal },
    #[error("generated recipe did not retain the requested Rust LowerIr authority")]
    GeneratedRecipe { language: Language, stage: Stage },
    #[error("generated source authority had an unexpected byte length")]
    GeneratedSourceLength { observed: u32 },
    #[error("generated publication binding was zero")]
    GeneratedBindingZero,
    #[error("compiler diagnostic did not preserve the expected closed terminal")]
    CompilerDiagnostic { observed: Option<DiagnosticCode> },
    #[error("toolchain table order did not produce the required typed rejection")]
    ToolchainOrder {
        observed: Option<LocalToolchainSetError>,
    },
    #[error("toolchain table duplication did not produce the required typed rejection")]
    ToolchainDuplicate {
        observed: Option<LocalToolchainSetError>,
    },
    #[error("cancelled test source exceeded the compact source authority width")]
    CancelledSourceLength { actual: usize },
}

/// Closed reply classification retained by the journey test without carrying a large payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplyBodyClass {
    Generated,
    DependencyUnavailable,
    Health,
    Adaptive,
    ExecutionStarted,
    Execution,
    Rejected,
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
    let toolchains = rust_toolchains(&rustc)?;
    let compiler = open_local_compiler(&fixture, &toolchains, &cancelled, &mut scratch)?;
    let mut service = ApplicationService::with_compiler(compiler);

    assert_generated(&service.execute(&generate(
        Language::Rust,
        "lower-ir",
        "pub const READY: i32 = 1;",
    )?))?;

    assert_missing_native_toolchain(&service.execute(&generate(
        Language::Python,
        "lower-ir",
        "ready = 1",
    )?))?;

    assert_explicitly_unavailable_tool(&service.execute(&generate(
        Language::TypeScript,
        "lower-ir",
        "export const READY = 1;",
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

fn rust_toolchains(
    rustc: &std::path::Path,
) -> Result<[ToolchainSelection<'_>; 3], LocalCompilerTestError> {
    Ok([
        ToolchainSelection::ResolvedNative(ResolvedToolchain::from_version(
            NativeTool::Rustc,
            rustc,
            b"wave-application-local-compiler-test-v1",
        )?),
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Python,
        },
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::TypeScriptCompiler,
        },
    ])
}

fn open_local_compiler<'path, 'scratch, 'cancel>(
    fixture: &'path Fixture,
    toolchains: &'path [ToolchainSelection<'path>],
    cancelled: &'cancel AtomicBool,
    scratch: &'scratch mut LocalCompilerScratch,
) -> Result<LocalCompiler<'path, 'scratch, 'cancel>, LocalCompilerTestError> {
    let config = LocalCompilerConfig {
        toolchains: LocalToolchainSet::validate(toolchains)?,
        artifact_directory: &fixture.artifacts,
        journal_directory: &fixture.journal,
        native_work_directory: &fixture.native_work,
        control: LocalCompilerControl {
            timeout: LocalCompilerTimeout::new(Duration::from_secs(10))?,
            cancelled,
        },
    };
    LocalCompiler::create(config, one_slot()?, scratch)
        .map_err(LocalCompilerTestError::CompilerOpen)
}

#[test]
fn toolchain_table_requires_bounded_unique_canonical_tool_order()
-> Result<(), LocalCompilerTestError> {
    let unordered = [
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Python,
        },
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Rustc,
        },
    ];
    match LocalToolchainSet::validate(&unordered) {
        Err(LocalToolchainSetError::OutOfOrder {
            preceding: NativeTool::Python,
            observed: NativeTool::Rustc,
        }) => {}
        Err(error) => {
            return Err(LocalCompilerTestError::ToolchainOrder {
                observed: Some(error),
            });
        }
        Ok(_) => {
            return Err(LocalCompilerTestError::ToolchainOrder { observed: None });
        }
    }

    let duplicate = [
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Python,
        },
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Python,
        },
    ];
    match LocalToolchainSet::validate(&duplicate) {
        Err(LocalToolchainSetError::Duplicate {
            tool: NativeTool::Python,
        }) => Ok(()),
        Err(error) => Err(LocalCompilerTestError::ToolchainDuplicate {
            observed: Some(error),
        }),
        Ok(_) => Err(LocalCompilerTestError::ToolchainDuplicate { observed: None }),
    }
}

fn assert_generated(reply: &ApplicationReply) -> Result<(), LocalCompilerTestError> {
    let ReplyBody::Generated(facts) = &reply.body else {
        return Err(LocalCompilerTestError::GeneratedBody {
            observed: body_class(&reply.body),
        });
    };
    if reply.terminal != (Terminal::Complete { emitted: 1 }) {
        return Err(LocalCompilerTestError::GeneratedTerminal {
            observed: reply.terminal,
        });
    }
    if facts.recipe.language != Language::Rust || facts.recipe.stage != Stage::LowerIr {
        return Err(LocalCompilerTestError::GeneratedRecipe {
            language: facts.recipe.language,
            stage: facts.recipe.stage,
        });
    }
    if facts.source.byte_len != 25 {
        return Err(LocalCompilerTestError::GeneratedSourceLength {
            observed: facts.source.byte_len,
        });
    }
    if facts
        .publication
        .binding
        .as_ref()
        .iter()
        .all(|byte| *byte == 0)
    {
        return Err(LocalCompilerTestError::GeneratedBindingZero);
    }
    Ok(())
}

fn assert_missing_native_toolchain(reply: &ApplicationReply) -> Result<(), LocalCompilerTestError> {
    match reply.diagnostic {
        Some(Diagnostic {
            code: DiagnosticCode::CompilerTerminal,
            detail:
                DiagnosticDetail::Compiler(CompilerTerminal::Toolchain {
                    language: Language::Python,
                    stage: Stage::LowerIr,
                    selected: NativeTool::Python,
                    configured: None,
                    ..
                }),
        }) => Ok(()),
        _ => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: reply.diagnostic.map(|diagnostic| diagnostic.code),
        }),
    }
}

fn assert_explicitly_unavailable_tool(
    reply: &ApplicationReply,
) -> Result<(), LocalCompilerTestError> {
    match reply.diagnostic {
        Some(Diagnostic {
            code: DiagnosticCode::CompilerTerminal,
            detail:
                DiagnosticDetail::Compiler(CompilerTerminal::Toolchain {
                    language: Language::TypeScript,
                    stage: Stage::LowerIr,
                    selected: NativeTool::TypeScriptCompiler,
                    configured: None,
                    ..
                }),
        }) => Ok(()),
        _ => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: reply.diagnostic.map(|diagnostic| diagnostic.code),
        }),
    }
}

fn assert_unsupported_stage(reply: &ApplicationReply) -> Result<(), LocalCompilerTestError> {
    match reply.diagnostic {
        Some(Diagnostic {
            code: DiagnosticCode::UnsupportedCompilerStage,
            ..
        }) => Ok(()),
        _ => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: reply.diagnostic.map(|diagnostic| diagnostic.code),
        }),
    }
}

fn assert_cancelled(reply: &ApplicationReply, source: &[u8]) -> Result<(), LocalCompilerTestError> {
    let Ok(byte_len) = u32::try_from(source.len()) else {
        return Err(LocalCompilerTestError::CancelledSourceLength {
            actual: source.len(),
        });
    };
    let expected = ContentId::<SourceFactDomain>::from_canonical_bytes(source);
    match reply.diagnostic {
        Some(Diagnostic {
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
        _ => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: reply.diagnostic.map(|diagnostic| diagnostic.code),
        }),
    }
}

const fn body_class(body: &ReplyBody) -> ReplyBodyClass {
    match body {
        ReplyBody::Generated(_) => ReplyBodyClass::Generated,
        ReplyBody::DependencyUnavailable { .. } => ReplyBodyClass::DependencyUnavailable,
        ReplyBody::Health(_) => ReplyBodyClass::Health,
        ReplyBody::Adaptive(_) => ReplyBodyClass::Adaptive,
        ReplyBody::ExecutionStarted { .. } => ReplyBodyClass::ExecutionStarted,
        ReplyBody::Execution(_) => ReplyBodyClass::Execution,
        ReplyBody::Rejected => ReplyBodyClass::Rejected,
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
