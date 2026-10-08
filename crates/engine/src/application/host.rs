//! Explicit process-host admission for the local compiler runtime.
//!
//! This module is the sole boundary allowed to read host path variables or inspect a finite set
//! of platform locations. It converts that ambient input into absolute, canonical, probed owners
//! before the compiler thread starts. Every later compile remains independent of `PATH` and the
//! process environment.

mod authority;
mod capture;
mod error;
mod paths;
mod rust_selection;
mod snapshot;

use std::{
    ffi::OsString,
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use arrayvec::ArrayVec;
use authority::NativeExecutables;
use backend_compile::EmbeddingExecutable;
use backend_frontend_rust::legacy::RustCargoMetadataPolicy;
use backend_platform::DirectoryCapability;
use backend_semantic::vocabulary::NativeTool;
use backend_store::journal::PublicationLimits;
use paths::create_directory;

use crate::application::{
    DEFAULT_RUST_CARGO_METADATA_POLICY, EmbeddingProvisioningFailure, EmbeddingRequirement,
    LocalCompilerCapabilities, LocalCompilerClient, LocalCompilerRuntimeConfiguration,
    LocalCompilerRuntimePaths, LocalCompilerScratch, LocalCompilerTimeout,
    LocalRuntimeGoAuthorityFailure, ToolchainProbeLimits,
};

pub use capture::CapturedLocalHostEnvironment;
pub use error::LocalCompilerHostError;
pub(crate) use paths::{
    BundleTypeScriptResourceProof, bundled_typescript_runtime, bundled_typescript_sdk,
};
pub use paths::{LocalHostDirectory, LocalHostPathKind, LocalHostPathRole};
pub use rust_selection::{InstalledRustInputs, InstalledRustSelectionSource, InstalledRustToolchain, InstalledToolPlace, find_installed_rust, installed_rust_system_locations,
};
pub use snapshot::{
    ClosedLocalHostEnvironmentSnapshot, ClosedLocalHostEnvironmentSnapshotError,
    LocalCompilerHostSelection, LocalCompilerHostSelectionIssue, LocalCompilerHostSelectionSource,
    LocalHostCargoHomeSelection,
    MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES,
};

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
    /// Explicit Cargo executable paired with the selected Rust compiler.
    NudoxCargo,
    /// Explicit Cargo home containing the admitted registry/cache state.
    NudoxCargoHome,
    /// Explicit Clang compiler.
    NudoxClang,
    /// Explicit libclang shared library or containing directory.
    LibclangPath,
    /// Explicit Python interpreter.
    NudoxPython,
    /// Explicit TypeScript compiler.
    NudoxTypeScriptCompiler,
    /// TypeScript compiler selected from the installed host toolchain rather than NUDOX_TSC.
    /// This internal snapshot role preserves default-versus-explicit precedence across locald.
    NudoxTypeScriptDefaultCompiler,
    /// Explicit Go compiler.
    NudoxGo,
    /// Explicit Java compiler.
    NudoxJavaCompiler,
    /// Explicit .NET host.
    NudoxDotnet,
    /// Explicit Node runtime for the vendored TypeScript authority driver.
    NudoxTypeScriptNode,
    /// Internal frozen installed Node selection; does not claim an explicit NUDOX override.
    NudoxTypeScriptDefaultNode,
    /// Internal Node selection validated against an application bundle's byte inventory.
    NudoxTypeScriptBundledNode,
    /// Frozen application executable locating a relocatable SDK; validated only if selected.
    NudoxTypeScriptBundledApplication,
    /// Explicit Node module root containing the TypeScript compiler API.
    NudoxTypeScriptModuleRoot,
    /// Frozen installed module root; preserves project precedence across closed handoff.
    NudoxTypeScriptDefaultModuleRoot,
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

impl LocalHostVariable {
    /// Every process-host variable understood by compiler admission.
    ///
    /// The order is the stable order used by closed host-environment snapshots. Keep this list
    /// exhaustive when adding a new variant. Renaming an existing variable's process spelling or
    /// changing its role meaning requires a snapshot protocol version change.
    pub const ALL: [Self; 34] = [
        Self::NudoxDataRoot,
        Self::Home,
        Self::XdgDataHome,
        Self::LocalAppData,
        Self::NudoxRustc,
        Self::NudoxRustSysroot,
        Self::NudoxCargo,
        Self::NudoxCargoHome,
        Self::NudoxClang,
        Self::LibclangPath,
        Self::NudoxPython,
        Self::NudoxTypeScriptCompiler,
        Self::NudoxTypeScriptDefaultCompiler,
        Self::NudoxGo,
        Self::NudoxJavaCompiler,
        Self::NudoxDotnet,
        Self::NudoxTypeScriptNode,
        Self::NudoxTypeScriptModuleRoot,
        Self::NudoxTypeScriptReportProgram,
        Self::NudoxPyrefly,
        Self::NudoxGoOracle,
        Self::NudoxJdk,
        Self::NudoxRoslynHelper,
        Self::NudoxCargoRoot,
        Self::NudoxNpmRoot,
        Self::NudoxPypiRoot,
        Self::NudoxGoRoot,
        Self::NudoxMavenRoot,
        Self::NudoxNugetRoot,
        Self::NudoxGenericRoot,
        Self::NudoxTypeScriptDefaultNode,
        Self::NudoxTypeScriptBundledNode,
        Self::NudoxTypeScriptBundledApplication,
        Self::NudoxTypeScriptDefaultModuleRoot,
    ];

    /// Number of closed snapshot roles after excluding the workspace-owned data root.
    pub const CLOSED_ENVIRONMENT_SNAPSHOT_ROLE_COUNT: usize = Self::ALL.len() - 1;

    /// Returns the closed snapshot roles, excluding the workspace-owned data root.
    #[must_use]
    pub fn closed_environment_snapshot_roles() -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(|variable| *variable != Self::NudoxDataRoot)
    }

    /// Returns the exact process variable name associated with this role.
    #[must_use]
    pub const fn environment_name(self) -> &'static str {
        variable_name(self)
    }
}

/// Typed read access to process-host values.
///
/// Test and embedded hosts can provide a static implementation; production uses
/// [`ProcessHostEnvironment`]. No environment map or string key enters compiler state.
pub trait LocalHostEnvironment {
    /// Returns one exact OS-native value when the named variable is present.
    fn value(&self, variable: LocalHostVariable) -> Option<OsString>;

    /// Returns the process search path for selecting a TypeScript host runtime/compiler.
    ///
    /// Host admission resolves the selected `tsc` and `node` to canonical executables and pairs
    /// them with a TypeScript module root. The search path itself is never passed to compiler
    /// children.
    fn search_path(&self) -> Option<OsString> {
        None
    }

    /// Go's configured module cache, read only while an ambient host selection is captured.
    /// A selected cache is copied to NUDOX_GO_ROOT before the environment is closed.
    fn go_module_cache(&self) -> Option<OsString> {
        None
    }

    /// Go's configured workspace roots, read only while an ambient host selection is captured.
    /// A selected cache is copied to NUDOX_GO_ROOT before the environment is closed.
    fn go_path(&self) -> Option<OsString> {
        None
    }

    /// Cargo's configured user cache, captured only during installed-tool selection.
    fn cargo_home(&self) -> Option<OsString> { None }

    /// Typed request-time Cargo-home selection carried by a closed v3 snapshot.
    fn cargo_home_selection(&self) -> LocalHostCargoHomeSelection {
        LocalHostCargoHomeSelection::Strict
    }

    /// Windows user home when HOME is absent.
    fn user_profile(&self) -> Option<OsString> { None }
}

/// Production environment reader.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessHostEnvironment;

impl LocalHostEnvironment for ProcessHostEnvironment {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        std::env::var_os(variable.environment_name())
    }

    fn search_path(&self) -> Option<OsString> {
        std::env::var_os("PATH")
    }

    fn go_module_cache(&self) -> Option<OsString> {
        std::env::var_os("GOMODCACHE")
    }

    fn go_path(&self) -> Option<OsString> {
        std::env::var_os("GOPATH")
    }

    fn cargo_home(&self) -> Option<OsString> { std::env::var_os("CARGO_HOME") }
    fn user_profile(&self) -> Option<OsString> { std::env::var_os("USERPROFILE") }
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

    fn search_path(&self) -> Option<OsString> {
        ProcessHostEnvironment.search_path()
    }

    fn go_module_cache(&self) -> Option<OsString> {
        ProcessHostEnvironment.go_module_cache()
    }

    fn go_path(&self) -> Option<OsString> {
        ProcessHostEnvironment.go_path()
    }

    fn cargo_home(&self) -> Option<OsString> { ProcessHostEnvironment.cargo_home() }
    fn user_profile(&self) -> Option<OsString> { ProcessHostEnvironment.user_profile() }
}

/// Whether host admission may inspect its finite documented platform locations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalHostDiscovery {
    /// Only explicitly supplied typed paths participate for native compilers other than the
    /// TypeScript host. TypeScript can use bounded PATH and platform discovery for its paired
    /// Node/compiler fallback.
    ExplicitOnly,
    /// No host discovery is permitted. Every compiler and auxiliary path must come from
    /// the supplied closed snapshot; this mode never consults PATH, a bundle, or platform paths.
    ClosedSnapshot,
    /// Capture installed TypeScript, Python, and Go tools once from explicit overrides and the
    /// process PATH, then close their exact paths before a long-lived owner starts serving.
    InstalledTools,
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
    /// Cargo registry policy used while resolving Rust package metadata.
    pub rust_cargo_metadata_policy: RustCargoMetadataPolicy,
    go_authority_failure: Option<LocalRuntimeGoAuthorityFailure>,
    embedding_cache_directory: Option<DirectoryCapability>,
}

impl<Environment> LocalCompilerHost<Environment> {
    /// Binds one environment reader and discovery policy without inspecting either.
    #[must_use]
    pub const fn new(environment: Environment, discovery: LocalHostDiscovery) -> Self {
        Self {
            environment,
            discovery,
            rust_cargo_metadata_policy: DEFAULT_RUST_CARGO_METADATA_POLICY,
            go_authority_failure: None,
            embedding_cache_directory: None,
        }
    }

    /// Binds the held directory capability minted by the active workspace owner.
    #[must_use]
    pub fn with_embedding_cache_directory(mut self, directory: DirectoryCapability) -> Self {
        self.embedding_cache_directory = Some(directory);
        self
    }

    /// Sets the explicit Cargo metadata acquisition policy for Rust authority.
    #[must_use]
    pub const fn with_rust_cargo_metadata_policy(
        mut self,
        policy: RustCargoMetadataPolicy,
    ) -> Self {
        self.rust_cargo_metadata_policy = policy;
        self
    }

    /// Retains a captured Go-only admission failure until a Go request arrives.
    #[must_use]
    pub const fn with_go_authority_failure(
        mut self,
        failure: Option<LocalRuntimeGoAuthorityFailure>,
    ) -> Self {
        self.go_authority_failure = failure;
        self
    }
}

impl<Environment: LocalHostEnvironment> LocalCompilerHost<Environment> {
    /// Captures installed Rust, TypeScript, Python, and Go authorities into one immutable snapshot.
    /// Pass that snapshot to a host using LocalHostDiscovery::ClosedSnapshot before opening
    /// a long-lived owner. PATH and Go cache variables are consulted only during this call.
    ///
    /// Explicit NUDOX_* paths take precedence and are validated as their declared object
    /// kind. An invalid explicit Go path is retained as a Go-only admission failure so it
    /// cannot prevent other languages from starting; other invalid explicit paths return their
    /// typed error instead of choosing a PATH alternative. The returned selection contains no
    /// ambient search inputs. An absent default Cargo home is recorded as typed deferred
    /// authority and is not created during capture.
    ///
    /// # Errors
    ///
    /// Returns an exact typed path error, invalid snapshot, or policy error when this host is not
    /// configured for installed-tool capture.
    pub fn capture_installed_selection(
        &self,
    ) -> Result<LocalCompilerHostSelection, LocalCompilerHostError> {
        if self.discovery != LocalHostDiscovery::InstalledTools {
            return Err(LocalCompilerHostError::InstalledSelectionRequiresCapturePolicy);
        }

        // Read ambient inputs exactly once. Discovery and the closed receipt must describe
        // the same launch, even when an embedding host supplies a mutable environment reader.
        LocalCompilerHost::new(
            CapturedLocalHostEnvironment::capture(&self.environment),
            LocalHostDiscovery::InstalledTools,
        )
        .with_go_authority_failure(self.go_authority_failure)
        .capture_frozen_installed_selection()
    }

    fn capture_frozen_installed_selection(
        &self,
    ) -> Result<LocalCompilerHostSelection, LocalCompilerHostError> {
        let home = self.optional_absolute_path(LocalHostVariable::Home)?;
        #[cfg(windows)]
        let home = match home {
            Some(home) => Some(home),
            None => self.environment.user_profile().map(PathBuf::from)
                .map(|path| {
                    if path.is_absolute() { Ok(path) } else {
                    Err(LocalCompilerHostError::RelativeEnvironmentPath {
                        variable: LocalHostVariable::Home, path: path.into_boxed_path(),
                    })
                    }
                }).transpose()?,
        };
        let mut paths =
            Vec::with_capacity(LocalHostVariable::CLOSED_ENVIRONMENT_SNAPSHOT_ROLE_COUNT);
        let mut go_failure = self.go_authority_failure;
        for variable in LocalHostVariable::closed_environment_snapshot_roles() {
            if matches!(
                variable,
                LocalHostVariable::Home
                    | LocalHostVariable::NudoxPython
                    | LocalHostVariable::NudoxTypeScriptCompiler
                    | LocalHostVariable::NudoxTypeScriptDefaultCompiler
                    | LocalHostVariable::NudoxTypeScriptNode
                    | LocalHostVariable::NudoxTypeScriptDefaultNode
                    | LocalHostVariable::NudoxTypeScriptBundledNode
                    | LocalHostVariable::NudoxTypeScriptBundledApplication
                    | LocalHostVariable::NudoxTypeScriptModuleRoot
                    | LocalHostVariable::NudoxTypeScriptDefaultModuleRoot
                    | LocalHostVariable::NudoxTypeScriptReportProgram
                    | LocalHostVariable::NudoxPyrefly
                    | LocalHostVariable::NudoxGo
                    | LocalHostVariable::NudoxGoRoot
                    | LocalHostVariable::NudoxGoOracle
            ) {
                continue;
            }
            let Some(value) = self.environment.value(variable) else {
                continue;
            };
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(LocalCompilerHostError::RelativeEnvironmentPath {
                    variable,
                    path: path.into_boxed_path(),
                });
            }
            paths.push((variable, self.canonical_configured_selection_path(variable, path)?,
            ));
        }
        if let Some(home) = home.as_ref() {
            paths.push((LocalHostVariable::Home, home.clone()));
        }

        let mut deferred_default_cargo_home = false;
        self.capture_installed_rust_paths(
            &mut paths,
            home.as_deref(),
            &mut deferred_default_cargo_home,
        )?;

        let typescript = self.typescript_host_selection(home.as_deref())?;
        if let Some(compiler) = typescript.compiler {
            let variable = if typescript.compiler_explicit {
                LocalHostVariable::NudoxTypeScriptCompiler
            } else {
                LocalHostVariable::NudoxTypeScriptDefaultCompiler
            };
            paths.push((variable, compiler));
        }
        if let Some(node) = typescript.node {
            let variable = match node.origin {
                crate::application::typescript_host::TypeScriptSelectionOrigin::ExplicitConfiguration => LocalHostVariable::NudoxTypeScriptNode,
                crate::application::typescript_host::TypeScriptSelectionOrigin::ValidatedApplicationBundle => LocalHostVariable::NudoxTypeScriptBundledNode,
                _ => LocalHostVariable::NudoxTypeScriptDefaultNode,
            };
            paths.push((variable, node.path));
        }
        if let Some(module_root) = typescript.module_root {
            let variable = if typescript.module_root_explicit {
                LocalHostVariable::NudoxTypeScriptModuleRoot
            } else {
                LocalHostVariable::NudoxTypeScriptDefaultModuleRoot
            };
            paths.push((variable, module_root));
        }
        if let Some(application) = typescript.bundled_application {
            paths.push((LocalHostVariable::NudoxTypeScriptBundledApplication,
                application,
            ));
        }
        if let Some(report_program) = typescript.report_program {
            paths.push((
                LocalHostVariable::NudoxTypeScriptReportProgram,
                report_program,
            ));
        }

        for (variable, role, tool) in [
            (
                LocalHostVariable::NudoxPython,
                LocalHostPathRole::Native(NativeTool::Python),
                NativeTool::Python,
            ),
            (
                LocalHostVariable::NudoxGo,
                LocalHostPathRole::Native(NativeTool::GoCompiler),
                NativeTool::GoCompiler,
            ),
        ] {
            match self.executable(
                variable,
                role,
                self.executable_candidates(home.as_deref(), tool),
            ) {
                Ok(Some(path)) => paths.push((variable, path)),
                Ok(None) => {}
                Err(error) if tool == NativeTool::GoCompiler => {
                    if go_failure.is_none() {
                        go_failure = Some(authority::go_authority_failure(&error));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(pyrefly) = self.executable(
            LocalHostVariable::NudoxPyrefly,
            LocalHostPathRole::Pyrefly,
            self.auxiliary_candidates(home.as_deref(), "pyrefly"),
        )? {
            paths.push((LocalHostVariable::NudoxPyrefly, pyrefly));
        }
        let go_selected = paths
            .iter()
            .any(|(variable, _)| *variable == LocalHostVariable::NudoxGo);
        match self.go_module_cache_directory(home.as_deref(), go_selected) {
            Ok(Some(go_root)) => paths.push((LocalHostVariable::NudoxGoRoot, go_root)),
            Ok(None) => {}
            Err(error) => {
                if go_failure.is_none() {
                    go_failure = Some(authority::go_authority_failure(&error));
                }
            }
        }

        if let Some(value) = self.environment.value(LocalHostVariable::NudoxGoOracle) {
            let path = PathBuf::from(value);
            let admitted = if path.is_absolute() {
                self.canonical_configured_selection_path(LocalHostVariable::NudoxGoOracle, path)
            } else {
                Err(LocalCompilerHostError::RelativeEnvironmentPath {
                    variable: LocalHostVariable::NudoxGoOracle, path: path.into_boxed_path(),
                })
            };
            match admitted {
                Ok(path) => paths.push((LocalHostVariable::NudoxGoOracle, path)),
                Err(_) if go_failure.is_none() => {
                    go_failure = Some(LocalRuntimeGoAuthorityFailure::OracleUnavailable);
                }
                Err(_) => {}
            }
        }

        let mut snapshot = ClosedLocalHostEnvironmentSnapshot::from_paths(paths.clone())
            .map_err(LocalCompilerHostError::HostSnapshot)?;
        if deferred_default_cargo_home {
            snapshot = snapshot
                .with_deferred_default_cargo_home()
                .map_err(LocalCompilerHostError::HostSnapshot)?;
        }
        let selection = LocalCompilerHostSelection::captured_installed_tools(snapshot, go_failure)
            .map_err(LocalCompilerHostError::HostSnapshot)?;
        Ok(selection)
    }

    /// Admits the host, builds a complete canonical runtime table, and starts its single owner.
    ///
    /// # Errors
    ///
    /// Returns exact path, resource-bound, authority, configuration, or worker-startup causes.
    /// Broken Go configuration is retained as a Go-only unavailable state and fails when a Go
    /// package is requested; it does not block unrelated language owners. Other explicitly
    /// configured broken paths fail admission. A genuinely absent optional tool remains an
    /// honest unavailable row.
    pub fn open(&self) -> Result<LocalCompilerClient, LocalCompilerHostError> {
        self.open_with_embedding_state(None, None, EmbeddingRequirement::Optional)
    }

    /// Opens the compiler with one already activated shared embedding runtime.
    ///
    /// Passing `None` means embeddings were not configured; if configured provisioning failed,
    /// use [`Self::open_with_embedding_provisioning_failure`] so staged output can report that
    /// state distinctly.
    pub fn open_with_embedding_runtime(
        &self,
        runtime: Option<Arc<EmbeddingExecutable>>,
        requirement: EmbeddingRequirement,
    ) -> Result<LocalCompilerClient, LocalCompilerHostError> {
        self.open_with_embedding_state(runtime, None, requirement)
    }

    /// Opens the compiler with a closed status for configured embedding provisioning failure.
    ///
    /// Optional compilation keeps IR available and reports this status; required compilation
    /// fails when the first package is staged.
    pub fn open_with_embedding_provisioning_failure(
        &self,
        cause: EmbeddingProvisioningFailure,
        requirement: EmbeddingRequirement,
    ) -> Result<LocalCompilerClient, LocalCompilerHostError> {
        self.open_with_embedding_state(None, Some(cause), requirement)
    }

    /// Inspects the exact host compiler capability table without creating runtime directories or
    /// starting a compiler owner.
    ///
    /// This performs the same bounded executable and authority probes used by runtime admission.
    /// Its result matches a locald process only when both processes admit the same compiler and
    /// authority configuration for the same target platform.
    ///
    /// # Errors
    ///
    /// Returns exact path, resource-bound, authority, or configuration causes.
    pub fn inspect_capabilities(
        &self,
    ) -> Result<LocalCompilerCapabilities, LocalCompilerHostError> {
        let data_root = self.data_root()?;
        let paths = LocalCompilerRuntimePaths::new(
            data_root.join("artifacts"),
            data_root.join("journal"),
            data_root.join("native-work").join("scope-inspection"),
        )?;
        let configuration =
            self.runtime_configuration(paths, None, None, EmbeddingRequirement::Optional, true)?;
        Ok(configuration.capabilities())
    }

    fn open_with_embedding_state(
        &self,
        embedding_runtime: Option<Arc<EmbeddingExecutable>>,
        embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
        embedding_requirement: EmbeddingRequirement,
    ) -> Result<LocalCompilerClient, LocalCompilerHostError> {
        let data_root = self.data_root()?;
        create_directory(LocalHostDirectory::DataRoot, &data_root)?;
        let artifact_directory = data_root.join("artifacts");
        let journal_directory = data_root.join("journal");
        create_directory(LocalHostDirectory::Artifacts, &artifact_directory)?;
        create_directory(LocalHostDirectory::Journal, &journal_directory)?;
        let native_work_directory = self.create_native_work(&data_root)?;
        let paths = LocalCompilerRuntimePaths::new(
            artifact_directory,
            journal_directory,
            native_work_directory,
        )?;
        let configuration = self.runtime_configuration(
            paths,
            embedding_runtime,
            embedding_provisioning_failure,
            embedding_requirement,
            false,
        )?;
        LocalCompilerClient::start(configuration).map_err(Into::into)
    }

    fn runtime_configuration(
        &self,
        paths: LocalCompilerRuntimePaths,
        embedding_runtime: Option<Arc<EmbeddingExecutable>>,
        embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
        embedding_requirement: EmbeddingRequirement,
        admit_toolchains_now: bool,
    ) -> Result<LocalCompilerRuntimeConfiguration, LocalCompilerHostError> {
        let probe_limits =
            ToolchainProbeLimits::new(VERSION_PROBE_TIMEOUT, nonzero(VERSION_PROBE_STREAM_BYTES))?;
        let home = self.optional_absolute_path(LocalHostVariable::Home)?;
        let jdk_root = self.directory(
            LocalHostVariable::NudoxJdk,
            LocalHostPathRole::JdkRoot,
            self.jdk_candidates(home.as_deref()),
        )?;
        let typescript_host = self.typescript_host_selection(home.as_deref())?;
        let use_external_python_checker = self
            .environment
            .value(LocalHostVariable::NudoxPyrefly)
            .is_some();
        let (go, mut go_discovery_failure) = match self.executable(
            LocalHostVariable::NudoxGo,
            LocalHostPathRole::Native(NativeTool::GoCompiler),
            self.executable_candidates(home.as_deref(), NativeTool::GoCompiler),
        ) {
            Ok(go) => (go, None),
            Err(_) => (
                None,
                Some(LocalRuntimeGoAuthorityFailure::ExecutableUnavailable),
            ),
        };
        if self.go_authority_failure.is_some() {
            go_discovery_failure = self.go_authority_failure;
        }
        let cargo_home_deferred = self.environment.cargo_home_selection()
            == LocalHostCargoHomeSelection::DeferredDefault;
        let (cargo_home, cargo_home_deferred) = if cargo_home_deferred {
            let selected = self.required_absolute_path(LocalHostVariable::NudoxCargoHome)?;
            let home = self.required_absolute_path(LocalHostVariable::Home)?;
            if selected != home.join(".cargo") {
                return Err(LocalCompilerHostError::HostSnapshot(
                    ClosedLocalHostEnvironmentSnapshotError::InvalidDeferredCargoHome,
                ));
            }
            snapshot::validate_deferred_cargo_home_parent(&home)
                .map_err(LocalCompilerHostError::HostSnapshot)?;
            (Some(selected), true)
        } else {
            (
                self.directory(
                    LocalHostVariable::NudoxCargoHome,
                    LocalHostPathRole::CargoHome,
                    ArrayVec::new(),
                )?,
                false,
            )
        };
        let executables = NativeExecutables {
            rustc: self.executable(
                LocalHostVariable::NudoxRustc,
                LocalHostPathRole::Native(NativeTool::Rustc),
                self.executable_candidates(home.as_deref(), NativeTool::Rustc),
            )?,
            cargo: self.executable(
                LocalHostVariable::NudoxCargo,
                LocalHostPathRole::Cargo,
                self.auxiliary_candidates(home.as_deref(), "cargo"),
            )?,
            cargo_home,
            cargo_home_deferred,
            clang: self.executable(
                LocalHostVariable::NudoxClang,
                LocalHostPathRole::Native(NativeTool::Clang),
                self.executable_candidates(home.as_deref(), NativeTool::Clang),
            )?,
            python: if use_external_python_checker {
                self.executable(
                    LocalHostVariable::NudoxPython,
                    LocalHostPathRole::Native(NativeTool::Python),
                    self.executable_candidates(home.as_deref(), NativeTool::Python),
                )?
            } else {
                None
            },
            typescript: typescript_host.compiler.clone(),
            go,
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
        let mut toolchains = if admit_toolchains_now {
            executables.admitted_toolchain_rows(
                typescript_host
                    .node
                    .as_ref()
                    .map(|node| node.path.as_path()),
                typescript_host.module_root.as_deref(),
                probe_limits,
            )
        } else {
            executables.toolchain_rows(
                typescript_host
                    .node
                    .as_ref()
                    .map(|node| node.path.as_path()),
                typescript_host.module_root.as_deref(),
            )
        };
        let (package_roots, go_root_failure) = self.package_roots(home.as_deref())?;
        if go_discovery_failure.is_none() {
            go_discovery_failure = go_root_failure;
        }
        let go_module_cache = package_roots
            .iter()
            .find(|root| root.ecosystem == backend_library::interface::PackageEcosystem::Golang)
            .map(|root| root.path.to_path_buf());
        let (package_authority, typescript_project_host) = self.package_authority(
            home.as_deref(),
            &executables,
            typescript_host,
            jdk_root,
            go_module_cache.as_deref(),
            &paths.native_work_directory,
            probe_limits,
            go_discovery_failure,
        )?;
        if let crate::application::LocalRuntimePythonCheckerAdmission::Native { authority } =
            &package_authority.python_checker
        {
            if let Some(row) = toolchains
                .iter_mut()
                .find(|row| row.tool == NativeTool::Python)
            {
                *row =
                    crate::application::LocalRuntimeToolchain::compiled_native_python(*authority)?;
            }
        }
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
        )?
        .with_typescript_project_host(typescript_project_host)
        .with_embedding_requirement(embedding_requirement);
        let configuration = match embedding_runtime.as_ref() {
            Some(runtime) => {
                configuration.with_embedding_runtime(Arc::clone(runtime), embedding_requirement)
            }
            None => configuration,
        };
        let mut configuration = match embedding_provisioning_failure {
            Some(cause) => {
                configuration.with_embedding_provisioning_failure(cause, embedding_requirement)
            }
            None => configuration,
        };
        if let (Some(runtime), Some(directory)) = (
            embedding_runtime.as_ref(),
            self.embedding_cache_directory.as_ref(),
        ) && let Some(session) = runtime.open_durable_cache_session(directory.clone())
        {
            configuration = configuration.with_embedding_cache_session(session);
        }
        Ok(configuration)
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
    /// root and explicitly configured native authorities. TypeScript remains the narrow
    /// exception: it may use a paired host `tsc`/Node installation so projects work without
    /// manually duplicating the desktop's compiler-discovery policy.
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

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{
        GoVersion, LanguageProfile, PythonVersion, RustEdition, Stage, TypeScriptSource,
    };
    use std::ffi::OsString;

    struct InspectionEnvironment(PathBuf);

    impl LocalHostEnvironment for InspectionEnvironment {
        fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
            (variable == LocalHostVariable::NudoxDataRoot).then(|| self.0.clone().into_os_string())
        }
    }

    struct StartupEnvironment(Vec<(LocalHostVariable, OsString)>);

    impl LocalHostEnvironment for StartupEnvironment {
        fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
            self.0
                .iter()
                .find(|(candidate, _)| *candidate == variable)
                .map(|(_, value)| value.clone())
        }
    }

    struct PrivateTestDirectory(PathBuf);

    impl PrivateTestDirectory {
        fn create(label: &str) -> io::Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "{label}-{}-{}",
                std::process::id(),
                NEXT_NATIVE_WORK.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            }
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for PrivateTestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn inspected_environment_identity() -> [u8; 32] {
        let executable = std::env::current_exe().expect("test executable");
        let data_root = executable
            .parent()
            .expect("test executable directory")
            .join("unused-scope-inspection-root");
        let host = LocalCompilerHost::new(
            InspectionEnvironment(data_root),
            LocalHostDiscovery::ExplicitOnly,
        );
        host.inspect_capabilities()
            .expect("capability inspection")
            .for_profile(LanguageProfile::Rust(RustEdition::Rust2024))
            .environment_identity()
            .expect("closed compiler-child environment identity")
    }

    #[test]
    fn emit_scope_identity_for_environment_regression() {
        if std::env::var_os("BACKEND_SCOPE_ENV_REGRESSION").is_none() {
            return;
        }
        let identity = inspected_environment_identity();
        let mut hex = String::with_capacity(identity.len().saturating_mul(2));
        for byte in identity {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        println!("scope-environment-identity={hex}");
    }

    #[test]
    fn unrelated_owner_environment_does_not_churn_scope_identity() {
        fn inspect_with_owner_environment(
            working_directory: &str,
            terminal: &str,
            secret: &str,
        ) -> String {
            let output =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "application::host::tests::emit_scope_identity_for_environment_regression",
                        "--nocapture",
                    ])
                    .env_clear()
                    .env("BACKEND_SCOPE_ENV_REGRESSION", "1")
                    .env("PWD", working_directory)
                    .env("TERM", terminal)
                    .env("AWS_ACCESS_KEY_ID", "access-key")
                    .env("AWS_SECRET_ACCESS_KEY", secret)
                    .output()
                    .expect("spawn isolated scope identity probe");
            assert!(
                output.status.success(),
                "identity probe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .expect("identity probe output is UTF-8")
                .lines()
                .find_map(|line| line.strip_prefix("scope-environment-identity="))
                .expect("identity probe emitted its result")
                .to_owned()
        }

        let first = inspect_with_owner_environment("/work/one", "dumb", "secret-one");
        let changed = inspect_with_owner_environment("/work/two", "xterm-256color", "secret-two");
        assert_eq!(first, changed);
    }

    #[test]
    fn capability_inspection_does_not_create_runtime_directories() {
        let data_root = std::env::temp_dir().join(format!(
            "backend-compiler-inspection-{}-{}",
            std::process::id(),
            NEXT_NATIVE_WORK.fetch_add(1, Ordering::Relaxed),
        ));
        assert!(!data_root.exists());

        let host = LocalCompilerHost::new(
            InspectionEnvironment(data_root.clone()),
            LocalHostDiscovery::ExplicitOnly,
        );
        assert_eq!(
            host.rust_cargo_metadata_policy,
            DEFAULT_RUST_CARGO_METADATA_POLICY
        );
        let capabilities = host
            .inspect_capabilities()
            .expect("capabilities can be inspected without an installed toolchain");

        for capability in capabilities.as_slice() {
            if capability.profile().language() == backend_semantic::vocabulary::Language::Python {
                // Python's producer is compiled into this host. Its admitted identity is
                // available without an interpreter, cache, or runtime directory.
                assert_eq!(capability.state(), crate::application::LocalCompilerCapabilityState::Ready);
                assert!(capability.toolchain_identity().is_some());
                assert!(capability.local_authority_fingerprint().is_some());
            } else {
                assert_eq!(capability.state(), crate::application::LocalCompilerCapabilityState::Unavailable,
                    "an unconfigured external authority cannot be advertised ready: {capability:?}");
            }
        }
        assert!(!data_root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn closed_go_refusal_keeps_the_actual_native_python_owner_usable() {
        use backend_library::interface::{CompilerCapability, CompilerRequest};
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("nudox-go-python-{}-{}", std::process::id(),
            NEXT_NATIVE_WORK.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshot = ClosedLocalHostEnvironmentSnapshot::from_paths([]).unwrap()
            .with_go_failure(Some(LocalRuntimeGoAuthorityFailure::OracleUnavailable)).unwrap();
        let decoded = ClosedLocalHostEnvironmentSnapshot::parse(&snapshot.encode().unwrap()).unwrap();
        let selection = LocalCompilerHostSelection::from_closed_snapshot(decoded).unwrap();
        let environment = StartupEnvironment(vec![(LocalHostVariable::NudoxDataRoot,
            root.join("owner").into_os_string(),
        )]);
        let mut client = LocalCompilerHost::new(environment, LocalHostDiscovery::ClosedSnapshot)
            .with_go_authority_failure(selection.go_authority_failure()).open()
            .expect("closed Go refusal cannot block native Python startup");
        assert_eq!(client.capabilities().for_profile(LanguageProfile::Go(GoVersion::Go125)).state(),
            crate::application::LocalCompilerCapabilityState::ProbeFailed);
        client.generate(CompilerRequest {
            profile: LanguageProfile::Python(PythonVersion::Python314), stage: Stage::LowerIr,
            source: "answer = 42\n",
        }).expect("actual compiled Python producer remains usable");
        assert_eq!(client.capabilities().for_profile(LanguageProfile::Python(PythonVersion::Python314)).state(),
            crate::application::LocalCompilerCapabilityState::Ready);
        assert_eq!(client.capabilities().for_profile(LanguageProfile::TypeScript(TypeScriptSource::TypeScript)).state(),
            crate::application::LocalCompilerCapabilityState::Unavailable);
        drop(client);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires privately admitted Go, Node, and TypeScript installations"]
    fn failed_optional_go_admission_does_not_block_typescript_or_native_python_owner_startup()
    -> Result<(), Box<dyn std::error::Error>> {
        use backend_library::interface::{CompilerCapability, CompilerRequest};

        fn required_path(variable: &str) -> Result<PathBuf, io::Error> {
            let path = std::env::var_os(variable)
                .ok_or_else(|| io::Error::other(format!("{variable} is required")))?;
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(io::Error::other(format!("{variable} must be absolute")));
            }
            Ok(path)
        }

        let fixture_root = required_path("NUDOX_GO_DEBIAN_FIXTURE_ROOT")?;
        let go = fixture_root.join("usr/lib/go-1.24/bin/go");
        let typescript = required_path("NUDOX_GO_STARTUP_TSC")?;
        let node = required_path("NUDOX_GO_STARTUP_NODE")?;
        let module_root = required_path("NUDOX_GO_STARTUP_TYPESCRIPT_MODULE_ROOT")?;

        let state = PrivateTestDirectory::create("go-owner-startup")?;
        let data_root = state.path().join("owner-data");
        let invalid_go_module_cache = state.path().join("invalid-go-module-cache");
        fs::write(&invalid_go_module_cache, b"not a directory")?;
        let environment = StartupEnvironment(vec![
            (LocalHostVariable::NudoxDataRoot, data_root.into_os_string()),
            (
                LocalHostVariable::NudoxTypeScriptCompiler,
                typescript.into_os_string(),
            ),
            (
                LocalHostVariable::NudoxTypeScriptNode,
                node.into_os_string(),
            ),
            (
                LocalHostVariable::NudoxTypeScriptModuleRoot,
                module_root.into_os_string(),
            ),
            (LocalHostVariable::NudoxGo, go.into_os_string()),
            (
                LocalHostVariable::NudoxGoRoot,
                invalid_go_module_cache.into_os_string(),
            ),
        ]);
        let mut client = LocalCompilerHost::new(environment, LocalHostDiscovery::ExplicitOnly)
            .open()
            .expect("optional Go authority failure must not stop owner startup");

        assert_eq!(
            client
                .capabilities()
                .for_profile(LanguageProfile::Go(GoVersion::Go125))
                .state(),
            crate::application::LocalCompilerCapabilityState::ProbeFailed,
            "the explicit bad Go module-cache root remains a Go-only failure",
        );
        client
            .generate(CompilerRequest {
                profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                stage: Stage::LowerIr,
                source: "export const answer: number = 42;\n",
            })
            .expect("the configured TypeScript lane must remain usable");
        client
            .generate(CompilerRequest {
                profile: LanguageProfile::Python(PythonVersion::Python314),
                stage: Stage::LowerIr,
                source: "answer = 42\n",
            })
            .expect("the native Python lane must remain usable");
        assert_eq!(
            client
                .capabilities()
                .for_profile(LanguageProfile::TypeScript(TypeScriptSource::TypeScript))
                .state(),
            crate::application::LocalCompilerCapabilityState::Ready,
        );
        assert_eq!(
            client
                .capabilities()
                .for_profile(LanguageProfile::Python(PythonVersion::Python314))
                .state(),
            crate::application::LocalCompilerCapabilityState::Ready,
        );
        drop(client);
        Ok(())
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
        LocalHostVariable::NudoxCargo => "NUDOX_CARGO",
        LocalHostVariable::NudoxCargoHome => "NUDOX_CARGO_HOME",
        LocalHostVariable::NudoxClang => "NUDOX_CLANG",
        LocalHostVariable::LibclangPath => "LIBCLANG_PATH",
        LocalHostVariable::NudoxPython => "NUDOX_PYTHON",
        LocalHostVariable::NudoxTypeScriptCompiler => "NUDOX_TSC",
        LocalHostVariable::NudoxTypeScriptDefaultCompiler => "BACKEND_LOCALD_DEFAULT_TSC",
        LocalHostVariable::NudoxGo => "NUDOX_GO",
        LocalHostVariable::NudoxJavaCompiler => "NUDOX_JAVAC",
        LocalHostVariable::NudoxDotnet => "NUDOX_DOTNET",
        LocalHostVariable::NudoxTypeScriptNode => "NUDOX_TYPESCRIPT_NODE",
        LocalHostVariable::NudoxTypeScriptDefaultNode => "BACKEND_LOCALD_DEFAULT_TYPESCRIPT_NODE",
        LocalHostVariable::NudoxTypeScriptBundledNode => "BACKEND_LOCALD_BUNDLED_TYPESCRIPT_NODE",
        LocalHostVariable::NudoxTypeScriptBundledApplication => {
            "BACKEND_LOCALD_TYPESCRIPT_BUNDLED_APPLICATION"
        }
        LocalHostVariable::NudoxTypeScriptModuleRoot => "NUDOX_TYPESCRIPT_MODULE_ROOT",
        LocalHostVariable::NudoxTypeScriptDefaultModuleRoot => {
            "BACKEND_LOCALD_DEFAULT_TYPESCRIPT_MODULE_ROOT"
        }
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
