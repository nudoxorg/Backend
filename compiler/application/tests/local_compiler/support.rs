//! Exercises the `compiler-application` tests local-compiler support contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    env, fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use compiler_application::{
    LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerOpenError,
    LocalCompilerScratch, LocalCompilerTimeout, LocalCompilerTimeoutError, LocalToolchainSet,
    LocalToolchainSetError,
};
use compiler_driver::{NativeTool, ResolvedToolchain, ToolchainSelection};
use compiler_vocabulary::{LanguageProfile, Stage};
use heart_identity::{ContentId, SourceFactDomain};
use interface_core::{
    CorrelationId, GenerateRequest, GenerateTarget, RejectedSourceText, SourceText,
};
use server_journal::PublicationLimits;
use thiserror::Error;

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
    Toolchain(#[from] compiler_driver::ToolchainResolutionError),
    #[error("local compiler toolchain table was rejected")]
    ToolchainSet(#[from] LocalToolchainSetError),
    #[error("local compiler timeout was rejected")]
    Timeout(#[from] LocalCompilerTimeoutError),
    #[error("durable publication limits were rejected")]
    Limits(#[from] server_journal::PublicationLimitError),
    #[error("local compiler publication owner could not open")]
    CompilerOpen(#[from] LocalCompilerOpenError),
    #[error("local compiler publication owner could not shut down")]
    Shutdown(#[from] server_journal::ShutdownError),
    #[error("compiler fixture source was rejected: {0:?}")]
    Source(RejectedSourceText),
    #[error("generated reply did not retain complete generated facts")]
    GeneratedOutcome {
        observed: Box<interface_core::ApplicationOutcome>,
    },
    #[error("generated recipe did not retain the requested Rust LowerIr authority")]
    GeneratedRecipe {
        profile: LanguageProfile,
        stage: Stage,
    },
    #[error("generated source authority had an unexpected byte length")]
    GeneratedSourceLength { observed: u32 },
    #[error("distinct compiler sources produced the same typed source identity")]
    SourceIdentityCollision {
        first: ContentId<SourceFactDomain>,
        second: ContentId<SourceFactDomain>,
    },
    #[error("generated publication binding was zero")]
    GeneratedBindingZero,
    #[error("compiler diagnostic did not preserve the expected closed terminal")]
    CompilerDiagnostic {
        observed: Box<interface_core::ApplicationOutcome>,
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

pub(super) fn rustc_path() -> Result<PathBuf, LocalCompilerTestError> {
    let root =
        env::var_os("COMPILER_STABLE_TOOLCHAIN").ok_or(LocalCompilerTestError::MissingToolchain)?;
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
            b"interface-local-compiler-test-v1",
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
    LocalCompiler::create(config, one_slot()?, scratch)
        .map_err(LocalCompilerTestError::CompilerOpen)
}

pub(super) fn generate(
    profile: LanguageProfile,
    stage: Stage,
    source: &str,
) -> Result<interface_core::ApplicationInput, LocalCompilerTestError> {
    Ok(interface_core::ApplicationInput::Generate(
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

fn one_slot() -> Result<PublicationLimits, LocalCompilerTestError> {
    PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
        .map_err(LocalCompilerTestError::Limits)
}
