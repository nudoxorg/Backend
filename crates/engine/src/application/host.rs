//! Explicit process-host admission for the local compiler runtime.
//!
//! This module is the sole boundary allowed to read host path variables or inspect a finite set
//! of platform locations. It converts that ambient input into absolute, canonical, probed owners
//! before the compiler thread starts. Every later compile remains independent of `PATH` and the
//! process environment.

mod authority;
mod error;
mod paths;

use std::{
    ffi::OsString,
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use authority::NativeExecutables;
use backend_semantic::vocabulary::NativeTool;
use backend_store::journal::PublicationLimits;
use paths::create_directory;

use crate::application::{
    LocalCompilerClient, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimePaths,
    LocalCompilerScratch, LocalCompilerTimeout, ToolchainProbeLimits,
};

pub use error::LocalCompilerHostError;
pub use paths::{LocalHostDirectory, LocalHostPathKind, LocalHostPathRole};

const PLATFORM_PATH_CAPACITY: usize = 12;
const NATIVE_WORK_ATTEMPTS: u64 = 64;
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const VERSION_PROBE_STREAM_BYTES: usize = 16 * 1024;
const COMPILER_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const FRAGMENT_SCRATCH_BYTES: usize = 16 * 1024 * 1024;
const AUTHORITY_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const PUBLICATION_QUEUE_CAPACITY: usize = 64;
const PUBLICATION_GROUP_CAPACITY: usize = 64;
const PACKAGE_SOURCE_BYTES: u32 = 4 * 1024 * 1024;

static NEXT_NATIVE_WORK: AtomicU64 = AtomicU64::new(0);

/// Closed process variable vocabulary consulted during local-host admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalHostVariable {
    /// Durable compiler data root.
    NudoxDataRoot,
    /// User home used only for documented platform defaults.
    Home,
    /// XDG data root used on Unix platforms other than macOS.
    XdgDataHome,
    /// Windows local application-data root.
    LocalAppData,
    /// Explicit Rust compiler.
    NudoxRustc,
    /// Explicit Rust sysroot paired with `NUDOX_RUSTC` or the admitted default compiler.
    NudoxRustSysroot,
    /// Explicit Clang compiler.
    NudoxClang,
    /// Explicit Python interpreter.
    NudoxPython,
    /// Explicit TypeScript compiler.
    NudoxTypeScriptCompiler,
    /// Explicit Go compiler.
    NudoxGo,
    /// Explicit Java compiler.
    NudoxJavaCompiler,
    /// Explicit .NET host.
    NudoxDotnet,
    /// Explicit Node runtime for the vendored TypeScript authority driver.
    NudoxTypeScriptNode,
    /// Explicit Node module root containing the TypeScript compiler API.
    NudoxTypeScriptModuleRoot,
    /// Explicit program that directly emits TypeScript authority reports.
    NudoxTypeScriptReportProgram,
    /// Explicit Pyrefly executable.
    NudoxPyrefly,
    /// Explicit binary that directly emits Go authority reports.
    NudoxGoOracle,
    /// Explicit JDK home.
    NudoxJdk,
    /// Explicit published Roslyn authority helper assembly.
    NudoxRoslynHelper,
    /// Explicit Cargo registry source root.
    NudoxCargoRoot,
    /// Explicit npm package root.
    NudoxNpmRoot,
    /// Explicit unpacked Python-distribution root.
    NudoxPypiRoot,
    /// Explicit Go module-cache root.
    NudoxGoRoot,
    /// Explicit Maven repository root.
    NudoxMavenRoot,
    /// Explicit NuGet package root.
    NudoxNugetRoot,
    /// Explicit local C/C++ package root.
    NudoxGenericRoot,
}

/// Typed read access to process-host values.
///
/// Test and embedded hosts can provide a static implementation; production uses
/// [`ProcessHostEnvironment`]. No environment map or string key enters compiler state.
pub trait LocalHostEnvironment {
    /// Returns one exact OS-native value when the named variable is present.
    fn value(&self, variable: LocalHostVariable) -> Option<OsString>;
}

/// Production environment reader.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessHostEnvironment;

impl LocalHostEnvironment for ProcessHostEnvironment {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        std::env::var_os(variable_name(variable))
    }
}

/// Process environment with one explicit durable compiler root.
#[derive(Clone, Debug)]
pub struct WorkspaceCompilerEnvironment {
    data_root: PathBuf,
}

impl LocalHostEnvironment for WorkspaceCompilerEnvironment {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        if variable == LocalHostVariable::NudoxDataRoot {
            Some(self.data_root.clone().into_os_string())
        } else {
            ProcessHostEnvironment.value(variable)
        }
    }
}

/// Whether host admission may inspect its finite documented platform locations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalHostDiscovery {
    /// Only explicitly supplied typed environment paths participate.
    ExplicitOnly,
    /// Explicit paths take precedence, followed by the finite platform table.
    PlatformDefaults,
}

/// Immutable host inputs for one local compiler owner.
#[derive(Clone, Debug)]
pub struct LocalCompilerHost<Environment> {
    /// Typed environment source.
    pub environment: Environment,
    /// Closed discovery policy.
    pub discovery: LocalHostDiscovery,
}

impl<Environment> LocalCompilerHost<Environment> {
    /// Binds one environment reader and discovery policy without inspecting either.
    #[must_use]
    pub const fn new(environment: Environment, discovery: LocalHostDiscovery) -> Self {
        Self {
            environment,
            discovery,
        }
    }
}

impl<Environment: LocalHostEnvironment> LocalCompilerHost<Environment> {
    /// Admits the host, builds a complete canonical runtime table, and starts its single owner.
    ///
    /// # Errors
    ///
    /// Returns exact path, resource-bound, authority, configuration, or worker-startup causes.
    /// Explicitly configured broken paths fail admission; they never become silent unavailable
    /// rows. A genuinely absent optional tool remains an honest unavailable row.
    pub fn open(&self) -> Result<LocalCompilerClient, LocalCompilerHostError> {
        let data_root = self.data_root()?;
        create_directory(LocalHostDirectory::DataRoot, &data_root)?;
        let artifact_directory = data_root.join("artifacts");
        let journal_directory = data_root.join("journal");
        create_directory(LocalHostDirectory::Artifacts, &artifact_directory)?;
        create_directory(LocalHostDirectory::Journal, &journal_directory)?;
        let native_work_directory = self.create_native_work(&data_root)?;

        let probe_limits =
            ToolchainProbeLimits::new(VERSION_PROBE_TIMEOUT, nonzero(VERSION_PROBE_STREAM_BYTES))?;
        let home = self.optional_absolute_path(LocalHostVariable::Home)?;
        let jdk_root = self.directory(
            LocalHostVariable::NudoxJdk,
            LocalHostPathRole::JdkRoot,
            self.jdk_candidates(home.as_deref()),
        )?;
        let executables = NativeExecutables {
            rustc: self.executable(
                LocalHostVariable::NudoxRustc,
                LocalHostPathRole::Native(NativeTool::Rustc),
                self.executable_candidates(home.as_deref(), NativeTool::Rustc),
            )?,
            clang: self.executable(
                LocalHostVariable::NudoxClang,
                LocalHostPathRole::Native(NativeTool::Clang),
                self.executable_candidates(home.as_deref(), NativeTool::Clang),
            )?,
            python: self.executable(
                LocalHostVariable::NudoxPython,
                LocalHostPathRole::Native(NativeTool::Python),
                self.executable_candidates(home.as_deref(), NativeTool::Python),
            )?,
            typescript: self.executable(
                LocalHostVariable::NudoxTypeScriptCompiler,
                LocalHostPathRole::Native(NativeTool::TypeScriptCompiler),
                self.executable_candidates(home.as_deref(), NativeTool::TypeScriptCompiler),
            )?,
            go: self.executable(
                LocalHostVariable::NudoxGo,
                LocalHostPathRole::Native(NativeTool::GoCompiler),
                self.executable_candidates(home.as_deref(), NativeTool::GoCompiler),
            )?,
            java: self.executable(
                LocalHostVariable::NudoxJavaCompiler,
                LocalHostPathRole::Native(NativeTool::JavaCompiler),
                self.java_candidates(home.as_deref(), jdk_root.as_deref()),
            )?,
            csharp: self.executable(
                LocalHostVariable::NudoxDotnet,
                LocalHostPathRole::Native(NativeTool::CSharpCompiler),
                self.executable_candidates(home.as_deref(), NativeTool::CSharpCompiler),
            )?,
        };
        let toolchains = executables.toolchain_rows();
        let package_roots = self.package_roots(home.as_deref())?;
        let package_authority =
            self.package_authority(home.as_deref(), &executables, jdk_root, probe_limits)?;
        let paths = LocalCompilerRuntimePaths::new(
            artifact_directory,
            journal_directory,
            native_work_directory,
        )?;
        let configuration = LocalCompilerRuntimeConfiguration::new(
            paths,
            toolchains,
            package_roots,
            package_authority,
            LocalCompilerTimeout::new(COMPILER_TIMEOUT)?,
            PublicationLimits::new(
                nonzero(PUBLICATION_QUEUE_CAPACITY),
                nonzero(PUBLICATION_GROUP_CAPACITY),
            )?,
            LocalCompilerScratch::with_fragment_capacity(nonzero(FRAGMENT_SCRATCH_BYTES))?,
        )?;
        LocalCompilerClient::start(configuration).map_err(Into::into)
    }

    fn data_root(&self) -> Result<PathBuf, LocalCompilerHostError> {
        if let Some(root) = self.optional_absolute_path(LocalHostVariable::NudoxDataRoot)? {
            return Ok(root);
        }
        #[cfg(target_os = "macos")]
        {
            let home = self.required_absolute_path(LocalHostVariable::Home)?;
            return Ok(home.join("Library/Application Support/Nudox"));
        }
        #[cfg(target_os = "windows")]
        {
            let local = self.required_absolute_path(LocalHostVariable::LocalAppData)?;
            return Ok(local.join("Nudox"));
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(data) = self.optional_absolute_path(LocalHostVariable::XdgDataHome)? {
                return Ok(data.join("nudox"));
            }
            let home = self.required_absolute_path(LocalHostVariable::Home)?;
            return Ok(home.join(".local/share/nudox"));
        }
        #[allow(unreachable_code)]
        Err(LocalCompilerHostError::DataRootUnavailable)
    }

    fn create_native_work(&self, data_root: &Path) -> Result<PathBuf, LocalCompilerHostError> {
        let parent = data_root.join("native-work");
        create_directory(LocalHostDirectory::NativeWorkParent, &parent)?;
        let first = NEXT_NATIVE_WORK.fetch_add(NATIVE_WORK_ATTEMPTS, Ordering::Relaxed);
        for offset in 0..NATIVE_WORK_ATTEMPTS {
            let candidate = parent.join(format!(
                "process-{}-{}",
                std::process::id(),
                first.saturating_add(offset)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(LocalCompilerHostError::CreateDirectory {
                        directory: LocalHostDirectory::NativeWork,
                        path: candidate.into_boxed_path(),
                        source,
                    });
                }
            }
        }
        Err(LocalCompilerHostError::NativeWorkCapacity {
            parent: parent.into_boxed_path(),
            attempts: NATIVE_WORK_ATTEMPTS,
        })
    }
}

impl LocalCompilerHost<ProcessHostEnvironment> {
    /// Production host admission with explicit environment overrides and finite platform defaults.
    #[must_use]
    pub const fn production() -> Self {
        Self::new(ProcessHostEnvironment, LocalHostDiscovery::PlatformDefaults)
    }
}

impl LocalCompilerHost<WorkspaceCompilerEnvironment> {
    /// Production compiler ownership with an explicit workspace-owned durable
    /// root and explicitly configured native authorities.
    ///
    /// A long-running service must bind its listener independently of ambient
    /// developer toolchains. Missing authority variables therefore enter the
    /// runtime as truthful unavailable capability rows; an operator can opt
    /// into a toolchain by supplying its absolute typed environment path.
    #[must_use]
    pub fn production_at(data_root: PathBuf) -> Self {
        Self::new(
            WorkspaceCompilerEnvironment { data_root },
            LocalHostDiscovery::ExplicitOnly,
        )
    }
}

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => unreachable!(),
    }
}

const fn variable_name(variable: LocalHostVariable) -> &'static str {
    match variable {
        LocalHostVariable::NudoxDataRoot => "NUDOX_DATA_ROOT",
        LocalHostVariable::Home => "HOME",
        LocalHostVariable::XdgDataHome => "XDG_DATA_HOME",
        LocalHostVariable::LocalAppData => "LOCALAPPDATA",
        LocalHostVariable::NudoxRustc => "NUDOX_RUSTC",
        LocalHostVariable::NudoxRustSysroot => "NUDOX_RUST_SYSROOT",
        LocalHostVariable::NudoxClang => "NUDOX_CLANG",
        LocalHostVariable::NudoxPython => "NUDOX_PYTHON",
        LocalHostVariable::NudoxTypeScriptCompiler => "NUDOX_TSC",
        LocalHostVariable::NudoxGo => "NUDOX_GO",
        LocalHostVariable::NudoxJavaCompiler => "NUDOX_JAVAC",
        LocalHostVariable::NudoxDotnet => "NUDOX_DOTNET",
        LocalHostVariable::NudoxTypeScriptNode => "NUDOX_TYPESCRIPT_NODE",
        LocalHostVariable::NudoxTypeScriptModuleRoot => "NUDOX_TYPESCRIPT_MODULE_ROOT",
        LocalHostVariable::NudoxTypeScriptReportProgram => "NUDOX_TYPESCRIPT_REPORT_PROGRAM",
        LocalHostVariable::NudoxPyrefly => "NUDOX_PYREFLY",
        LocalHostVariable::NudoxGoOracle => "NUDOX_GO_ORACLE",
        LocalHostVariable::NudoxJdk => "NUDOX_JDK",
        LocalHostVariable::NudoxRoslynHelper => "NUDOX_ROSLYN_HELPER",
        LocalHostVariable::NudoxCargoRoot => "NUDOX_CARGO_ROOT",
        LocalHostVariable::NudoxNpmRoot => "NUDOX_NPM_ROOT",
        LocalHostVariable::NudoxPypiRoot => "NUDOX_PYPI_ROOT",
        LocalHostVariable::NudoxGoRoot => "NUDOX_GO_ROOT",
        LocalHostVariable::NudoxMavenRoot => "NUDOX_MAVEN_ROOT",
        LocalHostVariable::NudoxNugetRoot => "NUDOX_NUGET_ROOT",
        LocalHostVariable::NudoxGenericRoot => "NUDOX_GENERIC_ROOT",
    }
}
