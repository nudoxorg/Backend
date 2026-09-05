//! Owns one bounded compiler thread behind a cloneable application capability.
//!
//! Native authority sessions and durable publication stay on the one thread that owns their
//! borrowed configuration and reusable scratch. Clients transfer owned requests through a
//! one-slot channel; package progress is delivered with backpressure in exact phase order.

use std::{
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

use arrayvec::ArrayVec;
use compiler_driver::{ResolvedToolchain, ToolchainResolutionError, ToolchainSelection};
use compiler_languages_csharp::{CSharpAuthorityConfiguration, CSharpOracle};
use compiler_languages_go::ConfiguredGoOracle;
use compiler_languages_java::harness::JdkToolchain;
use compiler_languages_python::Pyrefly;
use compiler_languages_rust::{RustFeatureControl, RustToolchain, SourceByteLimit};
use compiler_languages_typescript::ExplicitTypeScriptChecker;
use compiler_vocabulary::{Language, LanguageProfile, NativeTool, Stage};
use heart_identity::{CompilationTargetDomain, ContentId, ToolchainDomain};
use interface_core::{
    CompilerCapability, CompilerReadiness, CompilerRequest, CompilerRuntimeCause, CompilerTerminal,
    GeneratedArtifact, PackageCompilePhase, PackageCompileRequest, SemanticImageAccessError,
    SemanticImageAuthority, SemanticImageSnapshot,
};
use server_journal::PublicationLimits;
use thiserror::Error;

use crate::{
    CSharpPackageAuthorityConfiguration, JavaPackageAuthorityConfiguration, LocalCompiler,
    LocalCompilerConfig, LocalCompilerControl, LocalCompilerOpenError, LocalCompilerPath,
    LocalCompilerScratch, LocalCompilerTimeout, LocalPackageRoot, LocalPackageRootError,
    LocalPackageRootSet, LocalPackageRootSetError, LocalToolchainSet, LocalToolchainSetError,
    MAX_LOCAL_PACKAGE_ROOTS, MAX_LOCAL_TOOLCHAINS, PackageAuthorityConfiguration,
    RustPackageAuthorityConfiguration,
};

/// Exact owned storage paths retained by the compiler worker.
#[derive(Debug)]
pub struct LocalCompilerRuntimePaths {
    /// Immutable compact and semantic artifact directory.
    pub artifact_directory: Box<Path>,
    /// Durable journal owner directory.
    pub journal_directory: Box<Path>,
    /// Dedicated native-work directory.
    pub native_work_directory: Box<Path>,
}

impl LocalCompilerRuntimePaths {
    /// Validates three explicit absolute paths without consulting ambient state.
    ///
    /// # Errors
    ///
    /// Returns the exact named relative-path rejection.
    pub fn new(
        artifact_directory: PathBuf,
        journal_directory: PathBuf,
        native_work_directory: PathBuf,
    ) -> Result<Self, LocalCompilerOpenError> {
        for (path, role) in [
            (&artifact_directory, LocalCompilerPath::Artifacts),
            (&journal_directory, LocalCompilerPath::Journal),
            (&native_work_directory, LocalCompilerPath::NativeWork),
        ] {
            if !path.is_absolute() {
                return Err(LocalCompilerOpenError::RelativePath { path: role });
            }
        }
        Ok(Self {
            artifact_directory: artifact_directory.into_boxed_path(),
            journal_directory: journal_directory.into_boxed_path(),
            native_work_directory: native_work_directory.into_boxed_path(),
        })
    }
}

/// Immutable facts of one owned runtime toolchain row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalRuntimeToolchainFacts {
    /// Closed native tool family.
    pub tool: NativeTool,
    /// Exact version-byte identity, absent only for an explicitly unavailable row.
    pub identity: Option<ContentId<ToolchainDomain>>,
}

/// One owned explicit toolchain row that can be rebound inside the compiler thread.
#[derive(Debug)]
pub struct LocalRuntimeToolchain {
    facts: LocalRuntimeToolchainFacts,
    executable: Option<Box<Path>>,
}

impl LocalRuntimeToolchain {
    /// Owns one absolute executable and exact caller-probed version identity.
    ///
    /// # Errors
    ///
    /// Rejects a relative executable before the runtime configuration becomes visible.
    pub fn resolved(
        tool: NativeTool,
        executable: PathBuf,
        version_bytes: &[u8],
    ) -> Result<Self, ToolchainResolutionError> {
        let resolved = ResolvedToolchain::from_version(tool, &executable, version_bytes)?;
        Ok(Self {
            facts: LocalRuntimeToolchainFacts {
                tool,
                identity: Some(resolved.identity),
            },
            executable: Some(executable.into_boxed_path()),
        })
    }

    /// Retains the intentional absence of one registry-selected tool.
    #[must_use]
    pub const fn unavailable(tool: NativeTool) -> Self {
        Self {
            facts: LocalRuntimeToolchainFacts {
                tool,
                identity: None,
            },
            executable: None,
        }
    }

    fn selection(&self) -> Result<ToolchainSelection<'_>, ToolchainResolutionError> {
        match (self.executable.as_deref(), self.facts.identity) {
            (Some(executable), Some(identity)) => Ok(ToolchainSelection::ResolvedNative(
                ResolvedToolchain::from_identity(self.facts.tool, executable, identity)?,
            )),
            (None, None) => Ok(ToolchainSelection::ExplicitlyUnavailable {
                tool: self.facts.tool,
            }),
            _ => unreachable!("validated runtime toolchain keeps executable and identity paired"),
        }
    }
}

impl core::ops::Deref for LocalRuntimeToolchain {
    type Target = LocalRuntimeToolchainFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

/// Immutable facts of one owned package-store root.
#[derive(Debug, Eq, PartialEq)]
pub struct LocalRuntimePackageRootFacts {
    /// Closed ecosystem owned by this root.
    pub ecosystem: interface_core::PackageEcosystem,
    /// Explicit absolute root path.
    pub path: Box<Path>,
}

/// One validated owned package-store root.
#[derive(Debug, Eq, PartialEq)]
pub struct LocalRuntimePackageRoot {
    facts: LocalRuntimePackageRootFacts,
}

impl LocalRuntimePackageRoot {
    /// Owns one explicit absolute package root.
    ///
    /// # Errors
    ///
    /// Rejects a relative path with its exact ecosystem.
    pub fn new(
        ecosystem: interface_core::PackageEcosystem,
        path: PathBuf,
    ) -> Result<Self, LocalPackageRootError> {
        LocalPackageRoot::new(ecosystem, &path)?;
        Ok(Self {
            facts: LocalRuntimePackageRootFacts {
                ecosystem,
                path: path.into_boxed_path(),
            },
        })
    }
}

impl core::ops::Deref for LocalRuntimePackageRoot {
    type Target = LocalRuntimePackageRootFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

/// Owned Rust authority configuration retained for the worker lifetime.
#[derive(Debug)]
pub struct LocalRuntimeRustAuthority {
    /// Exact compiler and sysroot authority.
    pub toolchain: RustToolchain,
    /// Maximum root-source extent admitted before Cargo graph loading.
    pub maximum_source_bytes: SourceByteLimit,
    /// Enable every package feature.
    pub all_features: bool,
    /// Suppress the default feature set.
    pub no_default_features: bool,
    /// Exact caller-selected feature names.
    pub features: Box<[Box<str>]>,
}

/// Owned Java authority configuration retained for the worker lifetime.
#[derive(Debug)]
pub struct LocalRuntimeJavaAuthority {
    /// Exact owned JDK authority.
    pub toolchain: JdkToolchain<'static>,
    /// Exact caller-selected classpath entries.
    pub classpath: Box<[Box<Path>]>,
}

/// Owned Roslyn helper policy retained for the compiler worker lifetime.
#[derive(Debug)]
pub struct LocalRuntimeCSharpAuthority {
    /// Producer bound to the exact published helper assembly.
    pub producer: CSharpOracle,
    /// Optional assembly name forwarded to Roslyn.
    pub assembly_name: Option<Box<str>>,
    /// Optional reference-assembly directory forwarded to Roslyn.
    pub reference_directory: Option<Box<Path>>,
    /// Additional preprocessor symbols, in caller order.
    pub define_symbols: Box<[Box<str>]>,
    /// Additional global usings, in caller order.
    pub extra_usings: Box<[Box<str>]>,
    /// Whether SDK-style implicit global usings are reconstructed.
    pub implicit_usings: bool,
    /// Whether non-public declarations are retained by Roslyn.
    pub include_non_public: bool,
    /// Maximum exact source bytes admitted before helper entry.
    pub maximum_source_bytes: usize,
}

/// Owned language-authority adapters for package compilation.
#[derive(Debug, Default)]
pub struct LocalRuntimePackageAuthority {
    /// TypeScript checker authority.
    pub typescript: Option<ExplicitTypeScriptChecker>,
    /// Python Pyrefly authority.
    pub python: Option<Pyrefly>,
    /// Rust Analyzer/Cargo authority.
    pub rust: Option<LocalRuntimeRustAuthority>,
    /// Go package oracle authority.
    pub go: Option<ConfiguredGoOracle>,
    /// C# Roslyn helper authority.
    pub csharp: Option<LocalRuntimeCSharpAuthority>,
    /// Java JDK/doclet authority.
    pub java: Option<LocalRuntimeJavaAuthority>,
    /// Exact maximum retained Go or Java authority image bytes.
    pub maximum_image_bytes: Option<NonZeroUsize>,
}

/// Complete owned configuration moved into the compiler worker.
pub struct LocalCompilerRuntimeConfiguration {
    paths: LocalCompilerRuntimePaths,
    toolchains: Box<[LocalRuntimeToolchain]>,
    package_roots: Box<[LocalRuntimePackageRoot]>,
    package_authority: LocalRuntimePackageAuthority,
    timeout: LocalCompilerTimeout,
    publication_limits: PublicationLimits,
    scratch: LocalCompilerScratch,
}

impl LocalCompilerRuntimeConfiguration {
    /// Validates canonical toolchain/root ordering and binds all owned runtime resources.
    ///
    /// # Errors
    ///
    /// Returns the exact bounded-table rejection before a worker is spawned.
    pub fn new(
        paths: LocalCompilerRuntimePaths,
        toolchains: Box<[LocalRuntimeToolchain]>,
        package_roots: Box<[LocalRuntimePackageRoot]>,
        package_authority: LocalRuntimePackageAuthority,
        timeout: LocalCompilerTimeout,
        publication_limits: PublicationLimits,
        scratch: LocalCompilerScratch,
    ) -> Result<Self, LocalCompilerRuntimeConfigurationError> {
        validate_toolchain_order(&toolchains)?;
        validate_package_root_order(&package_roots)?;
        Ok(Self {
            paths,
            toolchains,
            package_roots,
            package_authority,
            timeout,
            publication_limits,
            scratch,
        })
    }
}

/// Rejection while validating owned bounded runtime tables.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LocalCompilerRuntimeConfigurationError {
    /// The toolchain table violated its bounded canonical order.
    #[error(transparent)]
    Toolchains(#[from] LocalToolchainSetError),
    /// The package-root table violated its bounded canonical order.
    #[error(transparent)]
    PackageRoots(#[from] LocalPackageRootSetError),
}

/// Failure to establish the single compiler owner.
#[derive(Debug, Error)]
pub enum LocalCompilerRuntimeOpenError {
    /// The operating system could not create the named compiler thread.
    #[error("could not spawn local compiler owner")]
    Spawn(#[source] std::io::Error),
    /// An owned toolchain could not be rebound to its validated exact identity.
    #[error("runtime toolchain row {ordinal} could not be rebound")]
    Toolchain {
        /// Canonical row ordinal.
        ordinal: usize,
        /// Exact rebind rejection.
        #[source]
        source: ToolchainResolutionError,
    },
    /// The worker rejected the canonical toolchain table.
    #[error(transparent)]
    ToolchainSet(#[from] LocalToolchainSetError),
    /// One owned package root failed its second worker-side validation.
    #[error("runtime package-root row {ordinal} could not be rebound")]
    PackageRoot {
        /// Canonical row ordinal.
        ordinal: usize,
        /// Exact root rejection.
        #[source]
        source: LocalPackageRootError,
    },
    /// The worker rejected the canonical package-root table.
    #[error(transparent)]
    PackageRootSet(#[from] LocalPackageRootSetError),
    /// Durable compiler/publication construction failed.
    #[error(transparent)]
    Compiler(#[from] LocalCompilerOpenError),
    /// The worker stopped before returning its setup result.
    #[error("local compiler owner stopped during setup")]
    StartupOwnerStopped,
    /// The compiler owner panicked during setup and retained its bounded payload.
    #[error("local compiler owner panicked during setup: {0:?}")]
    WorkerPanic(interface_core::CompilerRuntimePanic),
}

/// Cloneable, bounded client for one single-owner local compiler runtime.
#[derive(Clone)]
pub struct LocalCompilerClient {
    shared: Arc<RuntimeShared>,
}

impl LocalCompilerClient {
    /// Starts one compiler owner and waits until all borrowed configuration is valid.
    ///
    /// # Errors
    ///
    /// Returns the exact spawn, configuration, publisher, or startup-panic cause.
    pub fn start(
        configuration: LocalCompilerRuntimeConfiguration,
    ) -> Result<Self, LocalCompilerRuntimeOpenError> {
        let (command_tx, command_rx) = sync_channel(1);
        let (startup_tx, startup_rx) = sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let alive = Arc::new(AtomicBool::new(true));
        let worker_alive = Arc::clone(&alive);
        let worker = thread::Builder::new()
            .name("nudox-compiler-owner".to_owned())
            .spawn(move || {
                run_worker(
                    configuration,
                    command_rx,
                    startup_tx,
                    &worker_cancelled,
                    &worker_alive,
                );
            })
            .map_err(LocalCompilerRuntimeOpenError::Spawn)?;
        match startup_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                shared: Arc::new(RuntimeShared {
                    command: Some(command_tx),
                    worker: Some(worker),
                    cancelled,
                    active: AtomicBool::new(false),
                    alive,
                }),
            }),
            Ok(Err(error)) => {
                drop(command_tx);
                join_failed_start(worker, error)
            }
            Err(_) => {
                drop(command_tx);
                match worker.join() {
                    Ok(()) => Err(LocalCompilerRuntimeOpenError::StartupOwnerStopped),
                    Err(payload) => Err(LocalCompilerRuntimeOpenError::WorkerPanic(
                        interface_core::CompilerRuntimePanic::capture(payload.as_ref()),
                    )),
                }
            }
        }
    }

    /// Requests cancellation of the currently admitted compile, if any.
    pub fn cancel_active(&self) {
        if self.shared.active.load(Ordering::Acquire) {
            self.shared.cancelled.store(true, Ordering::Release);
        }
    }

    fn execute<Progress>(
        &self,
        request: OwnedCompilerRequest,
        progress: &mut Progress,
    ) -> Result<GeneratedArtifact, CompilerTerminal>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        let facts = request.facts();
        let lease = RequestLease::acquire(&self.shared, facts)?;
        self.shared.cancelled.store(false, Ordering::Release);
        let (response_tx, response_rx) = sync_channel(1);
        let command = RuntimeCommand::Compile {
            request,
            response: response_tx,
        };
        let sender = self
            .shared
            .command
            .as_ref()
            .expect("runtime command sender lives until the final client drop");
        if sender.send(command).is_err() {
            return Err(facts.terminal(CompilerRuntimeCause::RequestOwnerStopped));
        }
        let result = loop {
            match response_rx.recv() {
                Ok(RuntimeEvent::Phase(phase)) => progress(phase),
                Ok(RuntimeEvent::Complete(result)) => break result,
                Err(_) => {
                    break Err(facts.terminal(CompilerRuntimeCause::ResponseOwnerStopped));
                }
            }
        };
        drop(lease);
        result
    }

    /// Retrieves one exact reopened semantic image from the single compiler owner.
    ///
    /// # Errors
    ///
    /// Returns exact active-request, stopped-owner, supersession, validation, allocation, or
    /// bounded panic facts. The worker never exposes its reusable scratch directly.
    pub fn semantic_image_snapshot(
        &self,
        requested: SemanticImageAuthority,
    ) -> Result<SemanticImageSnapshot, SemanticImageAccessError> {
        let lease = RequestLease::acquire_snapshot(&self.shared, requested)?;
        let (response, returned) = sync_channel(1);
        let sender = self
            .shared
            .command
            .as_ref()
            .ok_or(SemanticImageAccessError::OwnerStopped { requested })?;
        sender
            .send(RuntimeCommand::SemanticImage {
                requested,
                response,
            })
            .map_err(|_| SemanticImageAccessError::OwnerStopped { requested })?;
        let result = returned
            .recv()
            .map_err(|_| SemanticImageAccessError::OwnerStopped { requested })?;
        drop(lease);
        result
    }
}

impl CompilerCapability for LocalCompilerClient {
    fn readiness(&self) -> CompilerReadiness {
        if self.shared.alive.load(Ordering::Acquire) {
            CompilerReadiness::Ready
        } else {
            CompilerReadiness::Unavailable
        }
    }

    fn cancel_active(&self) {
        Self::cancel_active(self);
    }

    fn semantic_image_snapshot(
        &mut self,
        requested: SemanticImageAuthority,
    ) -> Result<SemanticImageSnapshot, SemanticImageAccessError> {
        Self::semantic_image_snapshot(self, requested)
    }

    fn generate(
        &mut self,
        request: CompilerRequest<'_>,
    ) -> Result<GeneratedArtifact, CompilerTerminal> {
        let owned = OwnedCompilerRequest::Generate {
            profile: request.profile,
            stage: request.stage,
            source: request.source.to_owned().into_boxed_str(),
        };
        self.execute(owned, &mut |_| {})
    }

    fn compile_package<Progress>(
        &mut self,
        request: &PackageCompileRequest,
        progress: &mut Progress,
    ) -> Result<GeneratedArtifact, CompilerTerminal>
    where
        Progress: FnMut(PackageCompilePhase),
    {
        self.execute(OwnedCompilerRequest::Package(request.clone()), progress)
    }
}

struct RuntimeShared {
    command: Option<SyncSender<RuntimeCommand>>,
    worker: Option<JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    active: AtomicBool,
    alive: Arc<AtomicBool>,
}

impl Drop for RuntimeShared {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        drop(self.command.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct RequestLease<'shared> {
    shared: &'shared RuntimeShared,
}

impl<'shared> RequestLease<'shared> {
    fn acquire(
        shared: &'shared RuntimeShared,
        facts: RequestFacts,
    ) -> Result<Self, CompilerTerminal> {
        if !shared.alive.load(Ordering::Acquire) {
            return Err(facts.terminal(CompilerRuntimeCause::RequestOwnerStopped));
        }
        shared
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| facts.terminal(CompilerRuntimeCause::RequestInFlight))?;
        Ok(Self { shared })
    }

    fn acquire_snapshot(
        shared: &'shared RuntimeShared,
        requested: SemanticImageAuthority,
    ) -> Result<Self, SemanticImageAccessError> {
        if !shared.alive.load(Ordering::Acquire) {
            return Err(SemanticImageAccessError::OwnerStopped { requested });
        }
        shared
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SemanticImageAccessError::RequestInFlight { requested })?;
        Ok(Self { shared })
    }
}

impl Drop for RequestLease<'_> {
    fn drop(&mut self) {
        self.shared.cancelled.store(false, Ordering::Release);
        self.shared.active.store(false, Ordering::Release);
    }
}

enum OwnedCompilerRequest {
    Generate {
        profile: LanguageProfile,
        stage: Stage,
        source: Box<str>,
    },
    Package(PackageCompileRequest),
}

impl OwnedCompilerRequest {
    fn facts(&self) -> RequestFacts {
        match self {
            Self::Generate { profile, stage, .. } => RequestFacts {
                language: profile.language(),
                stage: *stage,
                target: None,
            },
            Self::Package(request) => RequestFacts {
                language: request.target.profile.language(),
                stage: request.target.stage,
                target: Some(request.as_ref().identity),
            },
        }
    }
}

#[derive(Clone, Copy)]
struct RequestFacts {
    language: Language,
    stage: Stage,
    target: Option<ContentId<CompilationTargetDomain>>,
}

impl RequestFacts {
    const fn terminal(self, cause: CompilerRuntimeCause) -> CompilerTerminal {
        CompilerTerminal::Runtime {
            language: self.language,
            stage: self.stage,
            target: self.target,
            cause,
        }
    }
}

enum RuntimeCommand {
    Compile {
        request: OwnedCompilerRequest,
        response: SyncSender<RuntimeEvent>,
    },
    SemanticImage {
        requested: SemanticImageAuthority,
        response: SyncSender<Result<SemanticImageSnapshot, SemanticImageAccessError>>,
    },
}

enum RuntimeEvent {
    Phase(PackageCompilePhase),
    Complete(Result<GeneratedArtifact, CompilerTerminal>),
}

fn run_worker(
    mut configuration: LocalCompilerRuntimeConfiguration,
    commands: Receiver<RuntimeCommand>,
    startup: SyncSender<Result<(), LocalCompilerRuntimeOpenError>>,
    cancelled: &AtomicBool,
    alive: &AtomicBool,
) {
    let mut toolchains = ArrayVec::<ToolchainSelection<'_>, MAX_LOCAL_TOOLCHAINS>::new();
    for (ordinal, owned) in configuration.toolchains.iter().enumerate() {
        match owned.selection() {
            Ok(selection) => toolchains.push(selection),
            Err(source) => {
                let _ = startup.send(Err(LocalCompilerRuntimeOpenError::Toolchain {
                    ordinal,
                    source,
                }));
                alive.store(false, Ordering::Release);
                return;
            }
        }
    }
    let toolchains = match LocalToolchainSet::validate(&toolchains) {
        Ok(toolchains) => toolchains,
        Err(error) => {
            let _ = startup.send(Err(error.into()));
            alive.store(false, Ordering::Release);
            return;
        }
    };

    let mut package_roots = ArrayVec::<LocalPackageRoot<'_>, MAX_LOCAL_PACKAGE_ROOTS>::new();
    for (ordinal, owned) in configuration.package_roots.iter().enumerate() {
        match LocalPackageRoot::new(owned.ecosystem, &owned.path) {
            Ok(root) => package_roots.push(root),
            Err(source) => {
                let _ = startup.send(Err(LocalCompilerRuntimeOpenError::PackageRoot {
                    ordinal,
                    source,
                }));
                alive.store(false, Ordering::Release);
                return;
            }
        }
    }
    let package_roots = match LocalPackageRootSet::validate(&package_roots) {
        Ok(package_roots) => package_roots,
        Err(error) => {
            let _ = startup.send(Err(error.into()));
            alive.store(false, Ordering::Release);
            return;
        }
    };

    let rust_feature_storage = configuration.package_authority.rust.as_ref().map(|rust| {
        rust.features
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&str>>()
    });
    let java_classpath_storage = configuration.package_authority.java.as_ref().map(|java| {
        java.classpath
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&Path>>()
    });
    let csharp_define_storage = configuration
        .package_authority
        .csharp
        .as_ref()
        .map(|csharp| {
            csharp
                .define_symbols
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>()
        });
    let csharp_using_storage = configuration
        .package_authority
        .csharp
        .as_ref()
        .map(|csharp| {
            csharp
                .extra_usings
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>()
        });
    let rust = configuration.package_authority.rust.as_ref().map(|rust| {
        RustPackageAuthorityConfiguration {
            toolchain: &rust.toolchain,
            maximum_source_bytes: rust.maximum_source_bytes,
            features: RustFeatureControl {
                all_features: rust.all_features,
                no_default_features: rust.no_default_features,
                features: rust_feature_storage.as_deref().unwrap_or_default(),
            },
        }
    });
    let java = configuration.package_authority.java.as_ref().map(|java| {
        JavaPackageAuthorityConfiguration {
            toolchain: &java.toolchain,
            classpath: java_classpath_storage.as_deref().unwrap_or_default(),
        }
    });
    let csharp = configuration
        .package_authority
        .csharp
        .as_ref()
        .map(|csharp| CSharpPackageAuthorityConfiguration {
            producer: &csharp.producer,
            configuration: CSharpAuthorityConfiguration {
                assembly_name: csharp.assembly_name.as_deref(),
                reference_directory: csharp.reference_directory.as_deref(),
                define_symbols: csharp_define_storage.as_deref().unwrap_or_default(),
                extra_usings: csharp_using_storage.as_deref().unwrap_or_default(),
                implicit_usings: csharp.implicit_usings,
                include_non_public: csharp.include_non_public,
                maximum_source_bytes: csharp.maximum_source_bytes,
            },
        });
    let authority = PackageAuthorityConfiguration {
        typescript: configuration.package_authority.typescript.as_ref(),
        python: configuration.package_authority.python.as_ref(),
        rust,
        go: configuration.package_authority.go.as_ref(),
        csharp,
        java,
        maximum_image_bytes: configuration
            .package_authority
            .maximum_image_bytes
            .map_or(0, NonZeroUsize::get),
    };
    let config = LocalCompilerConfig {
        toolchains,
        artifact_directory: &configuration.paths.artifact_directory,
        journal_directory: &configuration.paths.journal_directory,
        native_work_directory: &configuration.paths.native_work_directory,
        control: LocalCompilerControl {
            timeout: configuration.timeout,
            cancelled,
        },
    };
    let mut compiler = match LocalCompiler::create_with_package_authority(
        config,
        package_roots,
        authority,
        configuration.publication_limits,
        &mut configuration.scratch,
    ) {
        Ok(compiler) => compiler,
        Err(error) => {
            let _ = startup.send(Err(error.into()));
            alive.store(false, Ordering::Release);
            return;
        }
    };
    if startup.send(Ok(())).is_err() {
        alive.store(false, Ordering::Release);
        let _ = compiler.shutdown();
        return;
    }

    while let Ok(command) = commands.recv() {
        cancelled.store(false, Ordering::Release);
        match command {
            RuntimeCommand::Compile { request, response } => {
                let facts = request.facts();
                let result = catch_unwind(AssertUnwindSafe(|| match &request {
                    OwnedCompilerRequest::Generate {
                        profile,
                        stage,
                        source,
                    } => compiler.generate(CompilerRequest {
                        profile: *profile,
                        stage: *stage,
                        source,
                    }),
                    OwnedCompilerRequest::Package(request) => {
                        compiler.compile_package(request, &mut |phase| {
                            if response.send(RuntimeEvent::Phase(phase)).is_err() {
                                cancelled.store(true, Ordering::Release);
                            }
                        })
                    }
                }));
                match result {
                    Ok(result) => {
                        let _ = response.send(RuntimeEvent::Complete(result));
                    }
                    Err(payload) => {
                        let cause = interface_core::CompilerRuntimePanic::capture(payload.as_ref());
                        let _ = response.send(RuntimeEvent::Complete(Err(
                            facts.terminal(CompilerRuntimeCause::WorkerPanic(cause))
                        )));
                        break;
                    }
                }
            }
            RuntimeCommand::SemanticImage {
                requested,
                response,
            } => {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    compiler.semantic_image_snapshot(requested)
                }));
                match result {
                    Ok(result) => {
                        let _ = response.send(result);
                    }
                    Err(payload) => {
                        let cause = interface_core::CompilerRuntimePanic::capture(payload.as_ref());
                        let _ = response.send(Err(SemanticImageAccessError::WorkerPanic {
                            requested,
                            cause,
                        }));
                        break;
                    }
                }
            }
        }
    }
    alive.store(false, Ordering::Release);
    let _ = compiler.shutdown();
}

fn join_failed_start(
    worker: JoinHandle<()>,
    error: LocalCompilerRuntimeOpenError,
) -> Result<LocalCompilerClient, LocalCompilerRuntimeOpenError> {
    match worker.join() {
        Ok(()) => Err(error),
        Err(payload) => Err(LocalCompilerRuntimeOpenError::WorkerPanic(
            interface_core::CompilerRuntimePanic::capture(payload.as_ref()),
        )),
    }
}

fn validate_toolchain_order(
    entries: &[LocalRuntimeToolchain],
) -> Result<(), LocalToolchainSetError> {
    if entries.len() > MAX_LOCAL_TOOLCHAINS {
        return Err(LocalToolchainSetError::Capacity {
            actual: entries.len(),
            maximum: MAX_LOCAL_TOOLCHAINS,
        });
    }
    let mut previous = None;
    for entry in entries {
        if let Some(preceding) = previous {
            match entry.tool.cmp(&preceding) {
                core::cmp::Ordering::Equal => {
                    return Err(LocalToolchainSetError::Duplicate { tool: entry.tool });
                }
                core::cmp::Ordering::Less => {
                    return Err(LocalToolchainSetError::OutOfOrder {
                        preceding,
                        observed: entry.tool,
                    });
                }
                core::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(entry.tool);
    }
    Ok(())
}

fn validate_package_root_order(
    entries: &[LocalRuntimePackageRoot],
) -> Result<(), LocalPackageRootSetError> {
    if entries.len() > MAX_LOCAL_PACKAGE_ROOTS {
        return Err(LocalPackageRootSetError::Capacity {
            actual: entries.len(),
            maximum: MAX_LOCAL_PACKAGE_ROOTS,
        });
    }
    let mut previous = None;
    for entry in entries {
        if let Some(preceding) = previous {
            match entry.ecosystem.cmp(&preceding) {
                core::cmp::Ordering::Equal => {
                    return Err(LocalPackageRootSetError::Duplicate {
                        ecosystem: entry.ecosystem,
                    });
                }
                core::cmp::Ordering::Less => {
                    return Err(LocalPackageRootSetError::OutOfOrder {
                        preceding,
                        observed: entry.ecosystem,
                    });
                }
                core::cmp::Ordering::Greater => {}
            }
        }
        previous = Some(entry.ecosystem);
    }
    Ok(())
}
