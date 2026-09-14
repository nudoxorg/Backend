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
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use arrayvec::ArrayVec;
use compiler_driver::{ResolvedToolchain, ToolchainResolutionError, ToolchainSelection};
use backend_frontend_csharp::legacy::{CSharpAuthorityConfiguration, CSharpOracle};
use backend_frontend_go::legacy::ConfiguredGoOracle;
use backend_frontend_java::legacy::harness::JdkToolchain;
use backend_frontend_python::legacy::Pyrefly;
use backend_frontend_rust::legacy::{RustFeatureControl, RustToolchain, SourceByteLimit};
use backend_frontend_typescript::legacy::ExplicitTypeScriptChecker;
use compiler_publication::{binding::CompilationBindingFacts, manifest::CompilationManifestFacts};
use backend_semantic::vocabulary::{Language, LanguageProfile, NativeTool, Stage};
use backend_version::{CompilationTargetDomain, ContentId, ToolchainDomain};
use backend_library::interface::{
    CompilerCapability, CompilerReadiness, CompilerRequest, CompilerRuntimeCause, CompilerTerminal,
    GeneratedArtifact, PackageCompilePhase, PackageCompileRequest, SemanticImageAccessError,
    SemanticImageAuthority, SemanticImageSnapshot,
};
use backend_store::journal::PublicationLimits;
use thiserror::Error;

use crate::toolchain_probe::{ToolchainProbeError, ToolchainProbeLimits, probe_version};
use crate::{
    ActivatedSemanticPackage, CSharpPackageAuthorityConfiguration,
    JavaPackageAuthorityConfiguration, LocalCompiler, LocalCompilerConfig, LocalCompilerControl,
    LocalCompilerOpenError, LocalCompilerPath, LocalCompilerScratch, LocalCompilerTimeout,
    LocalPackageRoot, LocalPackageRootError, LocalPackageRootSet, LocalPackageRootSetError,
    LocalToolchainSet, LocalToolchainSetError, MAX_LOCAL_PACKAGE_ROOTS, MAX_LOCAL_TOOLCHAINS,
    PackageAuthorityConfiguration, PackageSemanticError, PackageSource, PackageSourceSet,
    PackageSourceSetError, PublishedSemanticPackage, RustPackageAuthorityConfiguration,
};

/// One owned source member transferred to the compiler owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedPackageSource {
    relative_path: Box<str>,
    source: Box<str>,
}

impl OwnedPackageSource {
    /// Admits and owns one normalized package-relative source.
    ///
    /// # Errors
    ///
    /// Returns the exact package-source path rejection before allocating owned storage.
    pub fn new(relative_path: &str, source: &str) -> Result<Self, PackageSourceSetError> {
        PackageSource::new(relative_path, source)?;
        Ok(Self {
            relative_path: relative_path.into(),
            source: source.into(),
        })
    }

    fn borrow(&self) -> Result<PackageSource<'_>, PackageSourceSetError> {
        PackageSource::new(&self.relative_path, &self.source)
    }
}

/// Complete owned package frontier transferred through the bounded runtime channel.
#[derive(Clone, Debug)]
pub struct OwnedPackageSourceSet {
    request: PackageCompileRequest,
    package_root: Box<Path>,
    sources: Box<[OwnedPackageSource]>,
}

impl OwnedPackageSourceSet {
    /// Admits one package compilation request and its complete ordered source frontier.
    ///
    /// # Errors
    ///
    /// Returns a path, cardinality, or ordering rejection before the request becomes visible.
    pub fn new(
        request: PackageCompileRequest,
        package_root: PathBuf,
        sources: Box<[OwnedPackageSource]>,
    ) -> Result<Self, PackageSourceSetError> {
        let borrowed = borrow_package_sources(&sources)?;
        PackageSourceSet::new(&request, &package_root, &borrowed)?;
        Ok(Self {
            request,
            package_root: package_root.into_boxed_path(),
            sources,
        })
    }

    fn facts(&self) -> RequestFacts {
        RequestFacts {
            language: self.request.target.profile.language(),
            stage: self.request.target.stage,
            target: Some(self.request.as_ref().identity),
        }
    }
}

fn borrow_package_sources(
    sources: &[OwnedPackageSource],
) -> Result<Vec<PackageSource<'_>>, PackageSourceSetError> {
    sources.iter().map(OwnedPackageSource::borrow).collect()
}

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
    /// Exact admission state for the selected executable and its version identity.
    pub state: LocalRuntimeToolchainState,
}

/// Closed state of one explicitly selected native toolchain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalRuntimeToolchainState {
    /// No executable was configured for this tool.
    Unavailable,
    /// An absolute executable is undergoing a bounded version probe.
    Probing,
    /// The executable and exact version-byte identity were both admitted.
    Ready,
    /// The bounded version probe reached a typed terminal.
    ProbeFailed,
}

/// Honest readiness of one language semantic capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCompilerCapabilityState {
    /// A required executable or package authority is absent.
    Unavailable,
    /// The selected executable is undergoing bounded identity admission.
    Probing,
    /// The selected executable failed bounded identity admission.
    ProbeFailed,
    /// Exact toolchain and package authority are ready.
    Ready,
}

/// One compiler-owned semantic capability bound to the exact runtime configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalCompilerCapability {
    profile: LanguageProfile,
    toolchain: NativeTool,
    toolchain_identity: Option<[u8; 32]>,
    local_authority_fingerprint: Option<[u8; 32]>,
    manifest: Option<[u8; 32]>,
    state: LocalCompilerCapabilityState,
}

impl LocalCompilerCapability {
    /// Exact language profile served by this capability slot.
    #[must_use]
    pub const fn profile(self) -> LanguageProfile {
        self.profile
    }

    /// Language family proved by the exact profile.
    #[must_use]
    pub const fn language(self) -> Language {
        self.profile.language()
    }

    /// Native tool selected by the compiler registry for this language.
    #[must_use]
    pub const fn toolchain(self) -> NativeTool {
        self.toolchain
    }

    /// Exact bounded version observation for the selected native tool.
    #[must_use]
    pub const fn toolchain_identity(self) -> Option<[u8; 32]> {
        self.toolchain_identity
    }

    /// Host-local package-authority configuration fingerprint.
    ///
    /// Paths participate in this drift detector, so it must not be used as a
    /// cross-host semantic-equivalence witness.
    #[must_use]
    pub const fn local_authority_fingerprint(self) -> Option<[u8; 32]> {
        self.local_authority_fingerprint
    }

    /// Exact semantic recipe identity, absent when a required authority is unavailable.
    #[must_use]
    pub const fn manifest(self) -> Option<[u8; 32]> {
        self.manifest
    }

    /// Honest lifecycle of the exact compiler capability.
    #[must_use]
    pub const fn state(self) -> LocalCompilerCapabilityState {
        self.state
    }
}

/// Complete fixed-cardinality semantic capability snapshot owned by one compiler runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalCompilerCapabilities(
    [LocalCompilerCapability; LanguageProfile::PRODUCT_PROFILES.len()],
);

impl LocalCompilerCapabilities {
    fn from_configuration(configuration: &LocalCompilerRuntimeConfiguration) -> Self {
        Self(LanguageProfile::PRODUCT_PROFILES.map(|profile| {
            let language = profile.language();
            let toolchain = language.native_tool();
            let runtime = configuration
                .toolchains
                .iter()
                .find(|candidate| candidate.tool == toolchain);
            let identity = runtime.and_then(|candidate| candidate.identity);
            let local_authority_fingerprint =
                package_authority_fingerprint(profile, &configuration.package_authority);
            let manifest =
                identity
                    .zip(local_authority_fingerprint)
                    .map(|(identity, authority)| {
                        semantic_recipe(profile, toolchain, identity, authority)
                    });
            let state = if local_authority_fingerprint.is_none() {
                LocalCompilerCapabilityState::Unavailable
            } else {
                match runtime.map(|candidate| candidate.state) {
                    Some(LocalRuntimeToolchainState::Probing) => {
                        LocalCompilerCapabilityState::Probing
                    }
                    Some(LocalRuntimeToolchainState::ProbeFailed) => {
                        LocalCompilerCapabilityState::ProbeFailed
                    }
                    Some(LocalRuntimeToolchainState::Ready) if manifest.is_some() => {
                        LocalCompilerCapabilityState::Ready
                    }
                    Some(
                        LocalRuntimeToolchainState::Unavailable | LocalRuntimeToolchainState::Ready,
                    )
                    | None => LocalCompilerCapabilityState::Unavailable,
                }
            };
            LocalCompilerCapability {
                profile,
                toolchain,
                toolchain_identity: identity.map(|identity| *identity.as_ref()),
                local_authority_fingerprint,
                manifest,
                state,
            }
        }))
    }

    /// Returns all language slots in canonical compiler order.
    #[must_use]
    pub const fn as_slice(&self) -> &[LocalCompilerCapability] {
        &self.0
    }

    /// Finds the exact profile capability without allocating or consulting ambient state.
    #[must_use]
    pub fn for_profile(&self, profile: LanguageProfile) -> LocalCompilerCapability {
        self.0
            .iter()
            .copied()
            .find(|capability| capability.profile == profile)
            .expect("fixed compiler capability table contains every product profile")
    }
}

fn package_authority_fingerprint(
    profile: LanguageProfile,
    authority: &LocalRuntimePackageAuthority,
) -> Option<[u8; 32]> {
    let mut identity = blake3::Hasher::new();
    identity.update(b"compiler-application.package-authority.v1\0");
    identity.update(&<[u8; 2]>::from(profile));
    match profile.language() {
        Language::Rust => {
            let rust = authority.rust.as_ref()?;
            update_path_identity(&mut identity, &rust.toolchain.tool);
            update_path_identity(&mut identity, &rust.toolchain.sysroot);
            identity.update(&rust.maximum_source_bytes.0.to_be_bytes());
            identity.update(&[
                u8::from(rust.all_features),
                u8::from(rust.no_default_features),
            ]);
            update_string_list_identity(&mut identity, &rust.features);
        }
        Language::TypeScript => {
            identity.update(
                &authority
                    .typescript
                    .as_ref()?
                    .local_configuration_fingerprint(),
            );
        }
        Language::Python => {
            identity.update(&authority.python.as_ref()?.local_configuration_fingerprint());
        }
        Language::Go => {
            identity.update(&authority.go.as_ref()?.local_configuration_fingerprint());
        }
        Language::Java => {
            let java = authority.java.as_ref()?;
            update_path_identity(&mut identity, java.toolchain.root());
            identity.update(&(java.classpath.len() as u64).to_be_bytes());
            for path in &java.classpath {
                update_path_identity(&mut identity, path);
            }
        }
        Language::CSharp => {
            let csharp = authority.csharp.as_ref()?;
            update_path_identity(&mut identity, csharp.producer.oracle_path());
            identity.update(&(csharp.producer.output_limit as u64).to_be_bytes());
            identity.update(&(csharp.producer.image_limit as u64).to_be_bytes());
            update_optional_string_identity(&mut identity, csharp.assembly_name.as_deref());
            update_optional_path_identity(&mut identity, csharp.reference_directory.as_deref());
            update_string_list_identity(&mut identity, &csharp.define_symbols);
            update_string_list_identity(&mut identity, &csharp.extra_usings);
            identity.update(&[
                u8::from(csharp.implicit_usings),
                u8::from(csharp.include_non_public),
            ]);
            identity.update(&(csharp.maximum_source_bytes as u64).to_be_bytes());
        }
        Language::Clang => {
            // Clang package semantics are owned directly by the selected
            // libclang toolchain; there is no second helper or resolver.
            identity.update(b"direct-libclang-package-authority");
        }
    }
    match authority.maximum_image_bytes {
        Some(bound) => {
            identity.update(&[1]);
            identity.update(&(bound.get() as u64).to_be_bytes());
        }
        None => {
            identity.update(&[0]);
        }
    }
    Some(*identity.finalize().as_bytes())
}

fn update_path_identity(identity: &mut blake3::Hasher, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    identity.update(&(bytes.len() as u64).to_be_bytes());
    identity.update(bytes);
}

fn update_optional_path_identity(identity: &mut blake3::Hasher, path: Option<&Path>) {
    match path {
        Some(path) => {
            identity.update(&[1]);
            update_path_identity(identity, path);
        }
        None => {
            identity.update(&[0]);
        }
    }
}

fn update_optional_string_identity(identity: &mut blake3::Hasher, value: Option<&str>) {
    match value {
        Some(value) => {
            identity.update(&[1]);
            identity.update(&(value.len() as u64).to_be_bytes());
            identity.update(value.as_bytes());
        }
        None => {
            identity.update(&[0]);
        }
    }
}

fn update_string_list_identity(identity: &mut blake3::Hasher, values: &[Box<str>]) {
    identity.update(&(values.len() as u64).to_be_bytes());
    for value in values {
        identity.update(&(value.len() as u64).to_be_bytes());
        identity.update(value.as_bytes());
    }
}

fn semantic_recipe(
    profile: LanguageProfile,
    toolchain: NativeTool,
    identity: ContentId<ToolchainDomain>,
    local_authority_fingerprint: [u8; 32],
) -> [u8; 32] {
    let mut manifest = blake3::Hasher::new();
    manifest.update(b"compiler-application.semantic-capability.v1\0");
    manifest.update(env!("CARGO_PKG_VERSION").as_bytes());
    manifest.update(&<[u8; 2]>::from(profile));
    manifest.update(&[u8::from(toolchain)]);
    manifest.update(identity.as_ref());
    manifest.update(&local_authority_fingerprint);
    *manifest.finalize().as_bytes()
}

/// One owned explicit toolchain row that can be rebound inside the compiler thread.
#[derive(Debug)]
pub struct LocalRuntimeToolchain {
    facts: LocalRuntimeToolchainFacts,
    executable: Option<Box<Path>>,
}

impl LocalRuntimeToolchain {
    /// Probes one absolute native executable under explicit output and deadline bounds, then owns
    /// the executable together with the identity of the exact returned version bytes.
    ///
    /// # Errors
    ///
    /// Returns exact admission, process, bounded-stream, deadline, exit, or identity causes. No
    /// ambient executable search or unbounded child output participates.
    pub fn probe(
        tool: NativeTool,
        executable: PathBuf,
        limits: ToolchainProbeLimits,
    ) -> Result<Self, ToolchainProbeError> {
        let version = probe_version(tool, &executable, limits)?;
        Self::resolved(tool, executable, &version)
            .map_err(|source| ToolchainProbeError::Resolution { tool, source })
    }

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
                state: LocalRuntimeToolchainState::Ready,
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
                state: LocalRuntimeToolchainState::Unavailable,
            },
            executable: None,
        }
    }

    pub(crate) fn probing(tool: NativeTool, executable: PathBuf) -> Self {
        debug_assert!(executable.is_absolute());
        Self {
            facts: LocalRuntimeToolchainFacts {
                tool,
                identity: None,
                state: LocalRuntimeToolchainState::Probing,
            },
            executable: Some(executable.into_boxed_path()),
        }
    }

    const fn probe_failed(tool: NativeTool) -> Self {
        Self {
            facts: LocalRuntimeToolchainFacts {
                tool,
                identity: None,
                state: LocalRuntimeToolchainState::ProbeFailed,
            },
            executable: None,
        }
    }

    fn probe_request(&self) -> Option<(NativeTool, PathBuf)> {
        (self.facts.state == LocalRuntimeToolchainState::Probing).then(|| {
            (
                self.facts.tool,
                self.executable
                    .as_deref()
                    .expect("probing toolchain retains its admitted executable")
                    .to_path_buf(),
            )
        })
    }

    fn selection(&self) -> Result<ToolchainSelection<'_>, ToolchainResolutionError> {
        match (
            self.facts.state,
            self.executable.as_deref(),
            self.facts.identity,
        ) {
            (LocalRuntimeToolchainState::Ready, Some(executable), Some(identity)) => {
                Ok(ToolchainSelection::ResolvedNative(
                    ResolvedToolchain::from_identity(self.facts.tool, executable, identity)?,
                ))
            }
            (
                LocalRuntimeToolchainState::Unavailable
                | LocalRuntimeToolchainState::Probing
                | LocalRuntimeToolchainState::ProbeFailed,
                _,
                None,
            ) => Ok(ToolchainSelection::ExplicitlyUnavailable {
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
    pub ecosystem: backend_library::interface::PackageEcosystem,
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
        ecosystem: backend_library::interface::PackageEcosystem,
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
    WorkerPanic(backend_library::interface::CompilerRuntimePanic),
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
        let pending_probes = configuration
            .toolchains
            .iter()
            .filter_map(LocalRuntimeToolchain::probe_request)
            .collect::<Vec<_>>();
        let capabilities = Arc::new(RwLock::new(LocalCompilerCapabilities::from_configuration(
            &configuration,
        )));
        let (command_tx, command_rx) = sync_channel(1);
        let (probe_tx, probe_rx) = channel();
        let (startup_tx, startup_rx) = sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let alive = Arc::new(AtomicBool::new(true));
        let worker_alive = Arc::clone(&alive);
        let worker_capabilities = Arc::clone(&capabilities);
        let worker = thread::Builder::new()
            .name("nudox-compiler-owner".to_owned())
            .spawn(move || {
                run_worker(
                    configuration,
                    command_rx,
                    probe_rx,
                    startup_tx,
                    &worker_cancelled,
                    &worker_alive,
                    &worker_capabilities,
                );
            })
            .map_err(LocalCompilerRuntimeOpenError::Spawn)?;
        match startup_rx.recv() {
            Ok(Ok(())) => {
                start_toolchain_probes(pending_probes, probe_tx);
                Ok(Self {
                    shared: Arc::new(RuntimeShared {
                        command: Some(command_tx),
                        worker: Some(worker),
                        cancelled,
                        active: AtomicBool::new(false),
                        alive,
                        capabilities,
                    }),
                })
            }
            Ok(Err(error)) => {
                drop(command_tx);
                join_failed_start(worker, error)
            }
            Err(_) => {
                drop(command_tx);
                match worker.join() {
                    Ok(()) => Err(LocalCompilerRuntimeOpenError::StartupOwnerStopped),
                    Err(payload) => Err(LocalCompilerRuntimeOpenError::WorkerPanic(
                        backend_library::interface::CompilerRuntimePanic::capture(payload.as_ref()),
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

    /// Returns immutable capability facts admitted by this exact compiler owner.
    #[must_use]
    pub fn capabilities(&self) -> LocalCompilerCapabilities {
        *self
            .shared
            .capabilities
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reports whether the compiler owner that proved these capabilities is still alive.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.shared.alive.load(Ordering::Acquire)
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

    /// Compiles and atomically publishes a complete package source frontier.
    ///
    /// # Errors
    ///
    /// Returns exact runtime ownership, package authority, lowering, publication, or reopen
    /// failures. The request is re-admitted inside the compiler owner before native execution.
    pub fn compile_package_sources(
        &self,
        request: OwnedPackageSourceSet,
    ) -> Result<PublishedSemanticPackage, PackageSemanticRuntimeError> {
        let facts = request.facts();
        let lease = RequestLease::acquire(&self.shared, facts)
            .map_err(PackageSemanticRuntimeError::Runtime)?;
        self.shared.cancelled.store(false, Ordering::Release);
        let (response, returned) = sync_channel(1);
        let sender = self
            .shared
            .command
            .as_ref()
            .expect("runtime command sender lives until the final client drop");
        sender
            .send(RuntimeCommand::CompilePackageSources { request, response })
            .map_err(|_| {
                PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::RequestOwnerStopped),
                )
            })?;
        let result = returned.recv().map_err(|_| {
            PackageSemanticRuntimeError::Runtime(
                facts.terminal(CompilerRuntimeCause::ResponseOwnerStopped),
            )
        })?;
        drop(lease);
        result
    }

    /// Reopens and verifies one exact immutable semantic generation in the compiler owner.
    ///
    /// # Errors
    ///
    /// Returns runtime ownership, panic, manifest, binding, artifact, or semantic-image failures.
    pub fn activate_semantic_generation(
        &self,
        profile: LanguageProfile,
        manifest: CompilationManifestFacts,
        binding: CompilationBindingFacts,
    ) -> Result<ActivatedSemanticPackage, PackageSemanticRuntimeError> {
        let facts = RequestFacts {
            language: profile.language(),
            stage: Stage::LowerIr,
            target: None,
        };
        let lease = RequestLease::acquire(&self.shared, facts)
            .map_err(PackageSemanticRuntimeError::Runtime)?;
        let (response, returned) = sync_channel(1);
        let sender = self
            .shared
            .command
            .as_ref()
            .expect("runtime command sender lives until the final client drop");
        sender
            .send(RuntimeCommand::ActivateSemanticGeneration {
                profile,
                manifest,
                binding,
                response,
            })
            .map_err(|_| {
                PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::RequestOwnerStopped),
                )
            })?;
        let result = returned.recv().map_err(|_| {
            PackageSemanticRuntimeError::Runtime(
                facts.terminal(CompilerRuntimeCause::ResponseOwnerStopped),
            )
        })?;
        drop(lease);
        result
    }
}

/// Failure of one owned package-wide semantic runtime request.
#[derive(Debug, Error)]
pub enum PackageSemanticRuntimeError {
    /// The owned request failed its worker-side re-admission.
    #[error("package source frontier failed worker-side admission")]
    Admission(#[from] PackageSourceSetError),
    /// Runtime ownership or panic terminal.
    #[error("package compiler runtime failed: {0:?}")]
    Runtime(CompilerTerminal),
    /// Package authority, lowering, publication, or reopen terminal.
    #[error(transparent)]
    Package(#[from] PackageSemanticError),
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
    capabilities: Arc<RwLock<LocalCompilerCapabilities>>,
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
    CompilePackageSources {
        request: OwnedPackageSourceSet,
        response: SyncSender<Result<PublishedSemanticPackage, PackageSemanticRuntimeError>>,
    },
    ActivateSemanticGeneration {
        profile: LanguageProfile,
        manifest: CompilationManifestFacts,
        binding: CompilationBindingFacts,
        response: SyncSender<Result<ActivatedSemanticPackage, PackageSemanticRuntimeError>>,
    },
}

enum RuntimeEvent {
    Phase(PackageCompilePhase),
    Complete(Result<GeneratedArtifact, CompilerTerminal>),
}

struct ToolchainProbeObservation {
    tool: NativeTool,
    result: Result<LocalRuntimeToolchain, ToolchainProbeError>,
}

fn start_toolchain_probes(
    pending: Vec<(NativeTool, PathBuf)>,
    observations: Sender<ToolchainProbeObservation>,
) {
    let limits = ToolchainProbeLimits::new(
        Duration::from_secs(5),
        NonZeroUsize::new(16 * 1024).expect("fixed probe stream bound is nonzero"),
    )
    .expect("fixed probe deadline is nonzero");
    for (tool, executable) in pending {
        let probe_observations = observations.clone();
        let name = format!("nudox-toolchain-{}-probe", u8::from(tool));
        let spawn = thread::Builder::new().name(name).spawn(move || {
            let result = LocalRuntimeToolchain::probe(tool, executable, limits);
            let _ = probe_observations.send(ToolchainProbeObservation { tool, result });
        });
        if spawn.is_err() {
            let _ = observations.send(ToolchainProbeObservation {
                tool,
                result: Ok(LocalRuntimeToolchain::probe_failed(tool)),
            });
        }
    }
}

fn run_worker(
    mut configuration: LocalCompilerRuntimeConfiguration,
    commands: Receiver<RuntimeCommand>,
    probes: Receiver<ToolchainProbeObservation>,
    startup: SyncSender<Result<(), LocalCompilerRuntimeOpenError>>,
    cancelled: &AtomicBool,
    alive: &AtomicBool,
    capabilities: &RwLock<LocalCompilerCapabilities>,
) {
    let mut startup = Some(startup);
    loop {
        match run_worker_generation(
            &mut configuration,
            &commands,
            &probes,
            &mut startup,
            cancelled,
            alive,
        ) {
            WorkerDisposition::Stop => break,
            WorkerDisposition::Reconfigure(observation) => {
                let replacement = observation
                    .result
                    .unwrap_or_else(|_| LocalRuntimeToolchain::probe_failed(observation.tool));
                let Some(slot) = configuration
                    .toolchains
                    .iter_mut()
                    .find(|candidate| candidate.tool == observation.tool)
                else {
                    alive.store(false, Ordering::Release);
                    break;
                };
                *slot = replacement;
                *capabilities
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    LocalCompilerCapabilities::from_configuration(&configuration);
            }
        }
    }
    alive.store(false, Ordering::Release);
}

enum WorkerDisposition {
    Stop,
    Reconfigure(ToolchainProbeObservation),
}

fn run_worker_generation(
    configuration: &mut LocalCompilerRuntimeConfiguration,
    commands: &Receiver<RuntimeCommand>,
    probes: &Receiver<ToolchainProbeObservation>,
    startup: &mut Option<SyncSender<Result<(), LocalCompilerRuntimeOpenError>>>,
    cancelled: &AtomicBool,
    alive: &AtomicBool,
) -> WorkerDisposition {
    let mut toolchains = ArrayVec::<ToolchainSelection<'_>, MAX_LOCAL_TOOLCHAINS>::new();
    for (ordinal, owned) in configuration.toolchains.iter().enumerate() {
        match owned.selection() {
            Ok(selection) => toolchains.push(selection),
            Err(source) => {
                if let Some(startup) = startup.take() {
                    let _ = startup.send(Err(LocalCompilerRuntimeOpenError::Toolchain {
                        ordinal,
                        source,
                    }));
                }
                alive.store(false, Ordering::Release);
                return WorkerDisposition::Stop;
            }
        }
    }
    let toolchains = match LocalToolchainSet::validate(&toolchains) {
        Ok(toolchains) => toolchains,
        Err(error) => {
            if let Some(startup) = startup.take() {
                let _ = startup.send(Err(error.into()));
            }
            alive.store(false, Ordering::Release);
            return WorkerDisposition::Stop;
        }
    };

    let mut package_roots = ArrayVec::<LocalPackageRoot<'_>, MAX_LOCAL_PACKAGE_ROOTS>::new();
    for (ordinal, owned) in configuration.package_roots.iter().enumerate() {
        match LocalPackageRoot::new(owned.ecosystem, &owned.path) {
            Ok(root) => package_roots.push(root),
            Err(source) => {
                if let Some(startup) = startup.take() {
                    let _ = startup.send(Err(LocalCompilerRuntimeOpenError::PackageRoot {
                        ordinal,
                        source,
                    }));
                }
                alive.store(false, Ordering::Release);
                return WorkerDisposition::Stop;
            }
        }
    }
    let package_roots = match LocalPackageRootSet::validate(&package_roots) {
        Ok(package_roots) => package_roots,
        Err(error) => {
            if let Some(startup) = startup.take() {
                let _ = startup.send(Err(error.into()));
            }
            alive.store(false, Ordering::Release);
            return WorkerDisposition::Stop;
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
            if let Some(startup) = startup.take() {
                let _ = startup.send(Err(error.into()));
            }
            alive.store(false, Ordering::Release);
            return WorkerDisposition::Stop;
        }
    };
    if let Some(startup) = startup.take()
        && startup.send(Ok(())).is_err()
    {
        alive.store(false, Ordering::Release);
        let _ = compiler.shutdown();
        return WorkerDisposition::Stop;
    }

    loop {
        if let Ok(observation) = probes.try_recv() {
            let _ = compiler.shutdown();
            return WorkerDisposition::Reconfigure(observation);
        }
        let command = match commands.recv_timeout(Duration::from_millis(5)) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
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
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(payload.as_ref());
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
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(payload.as_ref());
                        let _ = response.send(Err(SemanticImageAccessError::WorkerPanic {
                            requested,
                            cause,
                        }));
                        break;
                    }
                }
            }
            RuntimeCommand::CompilePackageSources { request, response } => {
                let facts = request.facts();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let sources = borrow_package_sources(&request.sources)?;
                    let borrowed =
                        PackageSourceSet::new(&request.request, &request.package_root, &sources)?;
                    compiler
                        .compile_package_sources(borrowed, &mut |_| {})
                        .map_err(PackageSemanticRuntimeError::from)
                }));
                match result {
                    Ok(result) => {
                        let _ = response.send(result);
                    }
                    Err(payload) => {
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(payload.as_ref());
                        let _ = response.send(Err(PackageSemanticRuntimeError::Runtime(
                            facts.terminal(CompilerRuntimeCause::WorkerPanic(cause)),
                        )));
                        break;
                    }
                }
            }
            RuntimeCommand::ActivateSemanticGeneration {
                profile,
                manifest,
                binding,
                response,
            } => {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    compiler
                        .activate_semantic_generation(manifest, binding)
                        .map_err(PackageSemanticRuntimeError::from)
                }));
                match result {
                    Ok(result) => {
                        let _ = response.send(result);
                    }
                    Err(payload) => {
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(payload.as_ref());
                        let terminal = RequestFacts {
                            language: profile.language(),
                            stage: Stage::LowerIr,
                            target: None,
                        }
                        .terminal(CompilerRuntimeCause::WorkerPanic(cause));
                        let _ = response.send(Err(PackageSemanticRuntimeError::Runtime(terminal)));
                        break;
                    }
                }
            }
        }
    }
    let _ = compiler.shutdown();
    WorkerDisposition::Stop
}

fn join_failed_start(
    worker: JoinHandle<()>,
    error: LocalCompilerRuntimeOpenError,
) -> Result<LocalCompilerClient, LocalCompilerRuntimeOpenError> {
    match worker.join() {
        Ok(()) => Err(error),
        Err(payload) => Err(LocalCompilerRuntimeOpenError::WorkerPanic(
            backend_library::interface::CompilerRuntimePanic::capture(payload.as_ref()),
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
