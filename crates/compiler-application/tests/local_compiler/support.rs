//! Exercises the `compiler-application` tests local-compiler support contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    env, fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use compiler_application::{
    LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerOpenError,
    LocalCompilerScratch, LocalCompilerTimeout, LocalCompilerTimeoutError, LocalToolchainSet,
    LocalToolchainSetError,
};
use compiler_driver::{NativeTool, ResolvedToolchain, ToolchainSelection};
use backend_semantic::vocabulary::{LanguageProfile, Stage};
use backend_library::interface::{
    CorrelationId, GenerateRequest, GenerateTarget, RejectedSourceText, SourceText,
};
use backend_store::journal::PublicationLimits;
use thiserror::Error;

static FIXTURE_ORDINAL: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
pub(super) enum LocalCompilerTestError {
    #[error("fixture filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("test did not receive a configured Nix Python executable")]
    MissingToolchain,
    #[error("configured Python executable path could not be canonicalized")]
    PythonPath(#[source] io::Error),
    #[error("configured Python executable could not report its exact version")]
    PythonVersion(#[source] io::Error),
    #[error("configured Python executable rejected its version probe")]
    PythonVersionRejected { status: ExitStatus },
    #[error("configured Python executable reported no version identity bytes")]
    PythonVersionEmpty,
    #[error("resolved Python toolchain was rejected")]
    Toolchain(#[from] compiler_driver::ToolchainResolutionError),
    #[error("local compiler toolchain table was rejected")]
    ToolchainSet(#[from] LocalToolchainSetError),
    #[error("local compiler timeout was rejected")]
    Timeout(#[from] LocalCompilerTimeoutError),
    #[error("durable publication limits were rejected")]
    Limits(#[from] backend_store::journal::PublicationLimitError),
    #[error("the required two-slot publication capacity was not representable")]
    PublicationCapacity,
    #[error("local compiler publication owner could not open")]
    CompilerOpen(#[from] LocalCompilerOpenError),
    #[error("local compiler publication owner could not shut down")]
    Shutdown(#[from] backend_store::journal::ShutdownError),
    #[error("compiler fixture source was rejected: {0:?}")]
    Source(RejectedSourceText),
    #[error("generated reply did not retain complete generated facts")]
    GeneratedOutcome {
        observed: Box<backend_library::interface::ApplicationOutcome>,
    },
    #[error("generated recipe did not retain the requested Rust LowerIr authority")]
    GeneratedRecipe {
        profile: LanguageProfile,
        stage: Stage,
    },
    #[error("generated source authority had an unexpected byte length")]
    GeneratedSourceLength { observed: u32 },
    #[error("generated publication binding was zero")]
    GeneratedBindingZero,
    #[error("generated semantic image authority was absent or zero-width")]
    GeneratedSemanticImageAbsent,
    #[error("compiler diagnostic did not preserve the expected closed terminal")]
    CompilerDiagnostic {
        observed: Box<backend_library::interface::ApplicationOutcome>,
    },
    #[error("toolchain table order did not produce the required typed rejection")]
    ToolchainOrder {
        observed: Option<LocalToolchainSetError>,
    },
    #[error("toolchain table duplication did not produce the required typed rejection")]
    ToolchainDuplicate {
        observed: Option<LocalToolchainSetError>,
    },
    #[error("test source exceeded the compact source authority width")]
    SourceLength {
        actual: usize,
        #[source]
        source: std::num::TryFromIntError,
    },
}

impl From<RejectedSourceText> for LocalCompilerTestError {
    fn from(error: RejectedSourceText) -> Self {
        Self::Source(error)
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
            "compiler-application-{}-{ordinal}",
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

pub(super) fn python_path() -> Result<PathBuf, LocalCompilerTestError> {
    let configured = env::var_os("COMPILER_PYTHON_COMPILER").map(PathBuf::from);
    let candidate = match configured {
        Some(path) if path.is_file() => path,
        Some(_) => return Err(LocalCompilerTestError::MissingToolchain),
        None => {
            let Some(paths) = env::var_os("PATH") else {
                return Err(LocalCompilerTestError::MissingToolchain);
            };
            let mut found = None;
            for directory in env::split_paths(&paths) {
                let candidate = directory.join("python3");
                if candidate.is_file() {
                    let canonical = candidate
                        .canonicalize()
                        .map_err(LocalCompilerTestError::PythonPath)?;
                    if canonical.starts_with("/nix/store/") {
                        found = Some(canonical);
                        break;
                    }
                }
            }
            found.ok_or(LocalCompilerTestError::MissingToolchain)?
        }
    };
    candidate
        .canonicalize()
        .map_err(LocalCompilerTestError::PythonPath)
}

pub(super) fn python_toolchains(
    python: &Path,
) -> Result<[ToolchainSelection<'_>; 3], LocalCompilerTestError> {
    let output = Command::new(python)
        .arg("--version")
        .output()
        .map_err(LocalCompilerTestError::PythonVersion)?;
    if !output.status.success() {
        return Err(LocalCompilerTestError::PythonVersionRejected {
            status: output.status,
        });
    }
    let version = if output.stdout.is_empty() {
        output.stderr.as_slice()
    } else {
        output.stdout.as_slice()
    };
    if version.is_empty() {
        return Err(LocalCompilerTestError::PythonVersionEmpty);
    }
    Ok([
        ToolchainSelection::ExplicitlyUnavailable {
            tool: NativeTool::Rustc,
        },
        ToolchainSelection::ResolvedNative(ResolvedToolchain::from_version(
            NativeTool::Python,
            python,
            version,
        )?),
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
    LocalCompiler::create(config, two_slots()?, scratch)
        .map_err(LocalCompilerTestError::CompilerOpen)
}

pub(super) fn generate(
    profile: LanguageProfile,
    stage: Stage,
    source: &str,
) -> Result<backend_library::interface::ApplicationInput, LocalCompilerTestError> {
    Ok(backend_library::interface::ApplicationInput::Generate(
        GenerateRequest {
            target: GenerateTarget {
                correlation: CorrelationId(1),
                profile,
                stage,
            },
            source: SourceText::try_from(source.to_owned())?,
        },
    ))
}

fn two_slots() -> Result<PublicationLimits, LocalCompilerTestError> {
    let two = NonZeroUsize::MIN
        .checked_add(1)
        .ok_or(LocalCompilerTestError::PublicationCapacity)?;
    PublicationLimits::new(two, two).map_err(LocalCompilerTestError::Limits)
}
