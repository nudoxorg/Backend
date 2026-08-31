use std::{
    env, fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use nudox_compile_driver::{NativeTool, ResolvedToolchain, ToolchainSelection};
use nudox_compile_vocab::Language;
use nudox_durable_journal::PublicationLimits;
use thiserror::Error;
use wave_application_compiler::{
    LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerOpenError,
    LocalCompilerScratch, LocalCompilerTimeout, LocalCompilerTimeoutError, LocalToolchainSet,
    LocalToolchainSetError,
};
use wave_application_core::{CorrelationId, InputText};

static FIXTURE_ORDINAL: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
pub(super) enum LocalCompilerTestError {
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
    GeneratedBody {
        observed: Box<wave_application_core::ReplyBody>,
    },
    #[error("generated reply had an unexpected terminal")]
    GeneratedTerminal {
        observed: Box<wave_application_core::Terminal>,
    },
    #[error("generated recipe did not retain the requested Rust LowerIr authority")]
    GeneratedRecipe {
        language: Language,
        stage: nudox_compile_vocab::Stage,
    },
    #[error("generated source authority had an unexpected byte length")]
    GeneratedSourceLength { observed: u32 },
    #[error("generated publication binding was zero")]
    GeneratedBindingZero,
    #[error("compiler diagnostic did not preserve the expected closed terminal")]
    CompilerDiagnostic {
        observed: Box<Option<wave_application_core::Diagnostic>>,
    },
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

impl From<wave_application_core::InputTextError> for LocalCompilerTestError {
    fn from(error: wave_application_core::InputTextError) -> Self {
        Self::Input(error)
    }
}

pub(super) struct Fixture {
    directory: PathBuf,
    pub(super) artifacts: PathBuf,
    pub(super) journal: PathBuf,
    pub(super) native_work: PathBuf,
}

impl Fixture {
    pub(super) fn create() -> Result<Self, LocalCompilerTestError> {
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

    pub(super) fn remove(self) -> Result<(), io::Error> {
        fs::remove_dir_all(self.directory)
    }
}

pub(super) fn rustc_path() -> Result<PathBuf, LocalCompilerTestError> {
    let root =
        env::var_os("NUDOX_STABLE_TOOLCHAIN").ok_or(LocalCompilerTestError::MissingToolchain)?;
    PathBuf::from(root)
        .join("bin/rustc")
        .canonicalize()
        .map_err(LocalCompilerTestError::RustcPath)
}

pub(super) fn rust_toolchains(
    rustc: &Path,
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

pub(super) fn open_local_compiler<'path, 'scratch, 'cancel>(
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
    LocalCompiler::create(config, one_slot()?, scratch).map_err(LocalCompilerTestError::CompilerOpen)
}

pub(super) fn generate(
    language: Language,
    stage: &str,
    source: &str,
) -> Result<wave_application_core::ApplicationInput, LocalCompilerTestError> {
    Ok(wave_application_core::ApplicationInput::Generate {
        correlation: CorrelationId(1),
        language: language_input(language)?,
        stage: InputText::try_from_str(stage)?,
        source: InputText::try_from_str(source)?,
    })
}

fn one_slot() -> Result<PublicationLimits, LocalCompilerTestError> {
    PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
        .map_err(LocalCompilerTestError::Limits)
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
