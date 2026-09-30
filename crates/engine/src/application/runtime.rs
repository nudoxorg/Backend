//! Owns one durable publisher and a bounded set of native compilation lanes.
//!
//! Lane workers share immutable admitted configuration and own private native scratch/work
//! directories. Staged results cross one bounded, byte-accounted queue to the publisher owner.

use std::{
    collections::HashMap,
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{
            Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError, channel, sync_channel,
        },
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::driver::{ResolvedToolchain, ToolchainResolutionError, ToolchainSelection};
use crate::publication::{binding::CompilationBindingFacts, manifest::CompilationManifestFacts};
use arrayvec::ArrayVec;
use backend_compile::EmbeddingExecutable;
use backend_frontend_csharp::legacy::{CSharpAuthorityConfiguration, CSharpOracle};
use backend_frontend_go::legacy::oracle::GoPackageAuthorityWitness;
use backend_frontend_go::legacy::{ConfiguredGoOracle, GoOracleInvocationModeV1};
use backend_frontend_java::legacy::harness::JdkToolchain;
use backend_frontend_python::legacy::Pyrefly;
use backend_frontend_rust::legacy::{RustFeatureControl, RustToolchain, SourceByteLimit};
use backend_frontend_typescript::legacy::{ExplicitTypeScriptChecker, TypeScriptInvocationModeV1};
use backend_library::interface::{
    CompilerCapability, CompilerReadiness, CompilerRequest, CompilerRuntimeCause, CompilerTerminal,
    GeneratedArtifact, PackageCompilePhase, PackageCompileRequest, SemanticImageAccessError,
    SemanticImageAuthority, SemanticImageSnapshot,
};
use backend_semantic::ir::SemanticInputWitness;
use backend_semantic::vocabulary::{Language, LanguageProfile, NativeTool, Stage};
use backend_store::journal::PublicationLimits;
use backend_version::{
    CompilationTargetDomain, CompileRecipeDomain, ContentId, Coverage, ToolchainDomain,
};
use thiserror::Error;

const COMPILER_LANE_COUNT: usize = 2;
const MAX_ADMITTED_COMPILER_REQUESTS: usize = 16;

use crate::application::compiler::{
    EmbeddingProvisioningFailure, EmbeddingRequirement, LocalCompilerExecution,
    MAX_PACKAGE_EMBEDDING_BYTES, MAX_PACKAGE_FRAGMENT_BYTES, MAX_PACKAGE_SEMANTIC_BYTES,
    StagedCompilerArtifact, StagedPackageCompilation, StagedSemanticPackage,
    StagedVersionedPlaneSegment,
};
use crate::application::executor::{
    BoundedLaneQueue, LaneIdentity, LaneSendError, StagedOutputBudget, StagedOutputLease,
};
use crate::application::toolchain_probe::{
    ToolchainProbeError, ToolchainProbeLimits, probe_version,
};
use crate::application::{
    ActivatedSemanticPackage, CSharpPackageAuthorityConfiguration,
    JavaPackageAuthorityConfiguration, LocalCompiler, LocalCompilerConfig, LocalCompilerControl,
    LocalCompilerOpenError, LocalCompilerPath, LocalCompilerScratch, LocalCompilerScratchError,
    LocalCompilerTimeout, LocalPackageRoot, LocalPackageRootError, LocalPackageRootSet,
    LocalPackageRootSetError, LocalToolchainSet, LocalToolchainSetError, MAX_LOCAL_PACKAGE_ROOTS,
    MAX_LOCAL_TOOLCHAINS, MAX_MANIFEST_ENTRIES, PackageAuthorityConfiguration,
    PackageSemanticError, PackageSource, PackageSourceSet, PackageSourceSetError,
    PublishedSemanticPackage, RustPackageAuthorityConfiguration, StagedSemanticArtifact,
};
use crate::compiler_input_manifest_v2::{CompilationUnitKeyV2, CompilerPackageTargetV2};
use crate::publication::StagedSemanticObjectClaim;

/// One owned source member transferred to the compiler owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedPackageSource {
    relative_path: Box<str>,
    source: String,
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
            source: source.to_owned(),
        })
    }

    /// Admits and takes ownership of one normalized package-relative source without copying its
    /// already-owned source buffer.
    ///
    /// # Errors
    /// Returns the exact path rejection before the request becomes visible to a compiler lane.
    pub fn from_string(relative_path: &str, source: String) -> Result<Self, PackageSourceSetError> {
        PackageSource::new(relative_path, &source)?;
        Ok(Self {
            relative_path: relative_path.into(),
            source,
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
    package_target: CompilerPackageTargetV2,
    package_root: Box<Path>,
    sources: Box<[OwnedPackageSource]>,
    input_claim: Option<SemanticInputWitness>,
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
        let package_target = CompilerPackageTargetV2::for_package(request.as_ref().clone());
        Self::new_for_unit(request, package_target, package_root, sources)
    }

    /// Admits a complete owned source frontier for one exact package-plus-unit target.
    ///
    /// # Errors
    /// Returns a package-target mismatch, path, cardinality, or ordering rejection before
    /// making the request visible to a compiler lane.
    pub fn new_for_unit(
        request: PackageCompileRequest,
        package_target: CompilerPackageTargetV2,
        package_root: PathBuf,
        sources: Box<[OwnedPackageSource]>,
    ) -> Result<Self, PackageSourceSetError> {
        let borrowed = borrow_package_sources(&sources)?;
        PackageSourceSet::new_for_unit(&request, &package_target, &package_root, &borrowed)?;
        Ok(Self {
            request,
            package_target,
            package_root: package_root.into_boxed_path(),
            sources,
            input_claim: None,
        })
    }

    /// Attaches the opaque exact input-manifest claim admitted by the caller.
    ///
    /// The compiler records these bytes as provenance only; closure and read-completeness
    /// authority are checked by the separate input admission boundary.
    #[must_use]
    pub fn with_input_claim(mut self, input: SemanticInputWitness) -> Self {
        self.input_claim = Some(input);
        self
    }

    fn facts(&self) -> RequestFacts {
        RequestFacts {
            profile: self.request.target.profile,
            language: self.request.target.profile.language(),
            stage: self.request.target.stage,
            target: Some(self.package_target.target()),
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
    environment_identity: Option<[u8; 32]>,
    target_platform_identity: Option<[u8; 32]>,
    portable_options_digest: Option<[u8; 32]>,
    lineage: Option<CompilerSessionLineage>,
    manifest: Option<[u8; 32]>,
    state: LocalCompilerCapabilityState,
}

/// Exact runtime identity available to a remote worker after its local
/// compiler authority and toolchain have been admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalCompilerExecutionIdentity {
    target: ContentId<CompilationTargetDomain>,
    profile: LanguageProfile,
    stage: Stage,
    toolchain: NativeTool,
    toolchain_identity: [u8; 32],
    local_authority_fingerprint: [u8; 32],
    environment_identity: [u8; 32],
    target_platform_identity: [u8; 32],
    invocation_recipe: crate::application::CompilerInvocationRecipeV2,
}

/// Local-only recipe identity for versioned planes that cannot claim portable invocation parity.
///
/// This type has no conversion to `CompileRecipeDomain` or `CompilerInvocationRecipeV2`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalCompilerPlaneRecipeIdentity([u8; 32]);

impl LocalCompilerPlaneRecipeIdentity {
    /// Returns the host-scoped recipe digest committed by local plane manifests.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Exact host-local execution identity for a staged semantic plane.
///
/// It binds the opened toolchain, local authority/options, closed compiler-child environment
/// recipe, target, and exact input/read-witness claim. Its distinct type and host-scoped recipe
/// prevent promotion into the portable worker recipe used by remote admission. Input coverage is
/// copied exactly and is never upgraded by this value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalCompilerPlaneExecutionIdentity {
    target: ContentId<CompilationTargetDomain>,
    profile: LanguageProfile,
    stage: Stage,
    toolchain: NativeTool,
    toolchain_identity: [u8; 32],
    local_authority_fingerprint: [u8; 32],
    environment_identity: [u8; 32],
    target_platform_identity: [u8; 32],
    recipe_identity: LocalCompilerPlaneRecipeIdentity,
    input: SemanticInputWitness,
}

impl LocalCompilerPlaneExecutionIdentity {
    #[must_use]
    pub const fn target(self) -> ContentId<CompilationTargetDomain> {
        self.target
    }

    #[must_use]
    pub const fn profile(self) -> LanguageProfile {
        self.profile
    }

    #[must_use]
    pub const fn stage(self) -> Stage {
        self.stage
    }

    #[must_use]
    pub const fn toolchain(self) -> NativeTool {
        self.toolchain
    }

    #[must_use]
    pub const fn toolchain_identity(self) -> [u8; 32] {
        self.toolchain_identity
    }

    #[must_use]
    pub const fn local_authority_fingerprint(self) -> [u8; 32] {
        self.local_authority_fingerprint
    }

    fn with_local_authority_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.local_authority_fingerprint = fingerprint;
        self
    }

    #[must_use]
    pub const fn environment_identity(self) -> [u8; 32] {
        self.environment_identity
    }

    #[must_use]
    pub const fn target_platform_identity(self) -> [u8; 32] {
        self.target_platform_identity
    }

    #[must_use]
    pub const fn recipe_identity(self) -> LocalCompilerPlaneRecipeIdentity {
        self.recipe_identity
    }

    /// Returns the exact source/read-set witness claim retained by this local output.
    #[must_use]
    pub const fn input_witness(self) -> SemanticInputWitness {
        self.input
    }
}

/// Opened runtime facets from which one exact staged local-plane identity is sealed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LocalCompilerPlaneExecutionSeed {
    target: ContentId<CompilationTargetDomain>,
    profile: LanguageProfile,
    stage: Stage,
    toolchain: NativeTool,
    toolchain_identity: [u8; 32],
    local_authority_fingerprint: [u8; 32],
    environment_identity: [u8; 32],
    target_platform_identity: [u8; 32],
    capability_identity: [u8; 32],
}

impl LocalCompilerPlaneExecutionSeed {
    pub(crate) const fn target(self) -> ContentId<CompilationTargetDomain> {
        self.target
    }

    pub(crate) const fn profile(self) -> LanguageProfile {
        self.profile
    }

    pub(crate) const fn stage(self) -> Stage {
        self.stage
    }

    pub(crate) const fn toolchain_identity(self) -> [u8; 32] {
        self.toolchain_identity
    }

    pub(crate) const fn local_authority_fingerprint(self) -> [u8; 32] {
        self.local_authority_fingerprint
    }

    pub(crate) const fn environment_identity(self) -> [u8; 32] {
        self.environment_identity
    }

    fn with_local_authority_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.local_authority_fingerprint = fingerprint;
        self
    }

    pub(crate) fn bind_input(
        self,
        input: SemanticInputWitness,
    ) -> LocalCompilerPlaneExecutionIdentity {
        let mut recipe = blake3::Hasher::new();
        recipe.update(b"compiler.local-plane-recipe.v1\0");
        recipe.update(&self.capability_identity);
        recipe.update(self.target.as_ref());
        recipe.update(&<[u8; 2]>::from(self.profile));
        recipe.update(&[u8::from(self.stage), u8::from(self.toolchain)]);
        recipe.update(&self.toolchain_identity);
        recipe.update(&self.local_authority_fingerprint);
        recipe.update(&self.environment_identity);
        recipe.update(&self.target_platform_identity);
        recipe.update(input.input_root());
        recipe.update(input.read_manifest_root().as_bytes());
        let coverage = input.coverage();
        recipe.update(&[coverage_wire_tag(coverage.state())]);
        recipe.update(&[u8::from(coverage.is_authorized_complete())]);
        LocalCompilerPlaneExecutionIdentity {
            target: self.target,
            profile: self.profile,
            stage: self.stage,
            toolchain: self.toolchain,
            toolchain_identity: self.toolchain_identity,
            local_authority_fingerprint: self.local_authority_fingerprint,
            environment_identity: self.environment_identity,
            target_platform_identity: self.target_platform_identity,
            recipe_identity: LocalCompilerPlaneRecipeIdentity(*recipe.finalize().as_bytes()),
            input,
        }
    }
}

const fn coverage_wire_tag(coverage: Coverage) -> u8 {
    match coverage {
        Coverage::Complete => 1,
        Coverage::Partial => 2,
        Coverage::Unavailable => 3,
        Coverage::Unsupported => 4,
        Coverage::Closed => 5,
    }
}

impl LocalCompilerExecutionIdentity {
    #[must_use]
    pub const fn target(self) -> ContentId<CompilationTargetDomain> {
        self.target
    }

    #[must_use]
    pub const fn profile(self) -> LanguageProfile {
        self.profile
    }

    #[must_use]
    pub const fn stage(self) -> Stage {
        self.stage
    }

    #[must_use]
    pub const fn toolchain(self) -> NativeTool {
        self.toolchain
    }

    #[must_use]
    pub const fn toolchain_identity(self) -> [u8; 32] {
        self.toolchain_identity
    }

    #[must_use]
    pub const fn local_authority_fingerprint(self) -> [u8; 32] {
        self.local_authority_fingerprint
    }

    fn with_local_authority_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.local_authority_fingerprint = fingerprint;
        self
    }

    #[must_use]
    pub const fn environment_identity(self) -> [u8; 32] {
        self.environment_identity
    }

    #[must_use]
    pub const fn target_platform_identity(self) -> [u8; 32] {
        self.target_platform_identity
    }

    #[must_use]
    pub const fn recipe_identity(self) -> ContentId<CompileRecipeDomain> {
        self.invocation_recipe.identity()
    }

    /// Returns the exact typed portable invocation recipe attested by this runtime.
    #[must_use]
    pub const fn invocation_recipe(self) -> crate::application::CompilerInvocationRecipeV2 {
        self.invocation_recipe
    }
}

/// Stable identity for one package/profile/toolchain session lineage.
///
/// Source files and project manifests are deliberately absent. Those belong
/// to [`ExactInputWitness`], which advances while this lineage remains stable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompilerSessionLineage([u8; 32]);

impl CompilerSessionLineage {
    /// Returns the canonical lineage digest.
    #[must_use]
    pub const fn identity(self) -> [u8; 32] {
        self.0
    }
}

/// Exact, canonical witness for the inputs and recipe of one package compile.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExactInputWitness([u8; 32]);

impl ExactInputWitness {
    /// Admits a digest produced from a caller's canonical exact-input stream.
    #[must_use]
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Returns the canonical exact-input digest.
    #[must_use]
    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
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

    /// Identity of the closed compiler-child environment recipe for this profile.
    /// Ambient owner variables are not inherited by compiler children and do
    /// not participate in this portable identity.
    #[must_use]
    pub const fn environment_identity(self) -> Option<[u8; 32]> {
        self.environment_identity
    }

    /// Identity of the platform on which this native compiler runtime runs.
    #[must_use]
    pub const fn target_platform_identity(self) -> Option<[u8; 32]> {
        self.target_platform_identity
    }

    /// Stable session lineage for this exact package authority configuration.
    #[must_use]
    pub const fn session_lineage(self) -> Option<CompilerSessionLineage> {
        self.lineage
    }

    /// Host-local capability configuration identity, absent when a required authority is
    /// unavailable. This includes local authority fingerprints and is not a portable invocation
    /// recipe; use [`LocalCompilerExecutionIdentity::recipe_identity`] for that identity.
    #[must_use]
    pub const fn manifest(self) -> Option<[u8; 32]> {
        self.manifest
    }

    /// Honest lifecycle of the exact compiler capability.
    #[must_use]
    pub const fn state(self) -> LocalCompilerCapabilityState {
        self.state
    }

    /// Binds an admitted runtime row to the exact requested package target
    /// and stage. Unsupported profiles or any unattested runtime facet fail
    /// closed with `None`.
    #[must_use]
    pub fn execution_identity(
        self,
        target: ContentId<CompilationTargetDomain>,
        profile: LanguageProfile,
        stage: Stage,
    ) -> Option<LocalCompilerExecutionIdentity> {
        if self.state != LocalCompilerCapabilityState::Ready || self.profile != profile {
            return None;
        }
        Some(LocalCompilerExecutionIdentity {
            target,
            profile,
            stage,
            toolchain: self.toolchain,
            toolchain_identity: self.toolchain_identity?,
            local_authority_fingerprint: self.local_authority_fingerprint?,
            environment_identity: self.environment_identity?,
            target_platform_identity: self.target_platform_identity?,
            invocation_recipe: crate::application::CompilerInvocationRecipeV2::new(
                profile,
                stage,
                self.toolchain,
                ContentId::from_digest(self.toolchain_identity?),
                self.environment_identity?,
                self.target_platform_identity?,
                self.portable_options_digest?,
            )
            .ok()?,
        })
    }

    /// Binds opened host-local runtime evidence for staged local-plane publication.
    ///
    /// Unlike [`Self::execution_identity`], this does not require portable option attestation;
    /// it retains the path-scoped local capability digest and can never authorize remote work.
    pub(crate) fn plane_execution_seed(
        self,
        target: ContentId<CompilationTargetDomain>,
        profile: LanguageProfile,
        stage: Stage,
    ) -> Option<LocalCompilerPlaneExecutionSeed> {
        if self.state != LocalCompilerCapabilityState::Ready || self.profile != profile {
            return None;
        }
        Some(LocalCompilerPlaneExecutionSeed {
            target,
            profile,
            stage,
            toolchain: self.toolchain,
            toolchain_identity: self.toolchain_identity?,
            local_authority_fingerprint: self.local_authority_fingerprint?,
            environment_identity: self.environment_identity?,
            target_platform_identity: self.target_platform_identity?,
            capability_identity: self.manifest?,
        })
    }

    /// Binds exact runtime evidence to a canonical package-plus-unit target.
    #[must_use]
    pub fn execution_identity_for_unit(
        self,
        package_target: &CompilerPackageTargetV2,
        profile: LanguageProfile,
        stage: Stage,
    ) -> Option<LocalCompilerExecutionIdentity> {
        if !compiler_unit_supported(profile, package_target.unit_key()) {
            return None;
        }
        self.execution_identity(package_target.target(), profile, stage)
    }

    /// Returns a local-only staged-plane identity seed for the exact package/native unit.
    pub(crate) fn plane_execution_seed_for_unit(
        self,
        package_target: &CompilerPackageTargetV2,
        profile: LanguageProfile,
        stage: Stage,
    ) -> Option<LocalCompilerPlaneExecutionSeed> {
        if !compiler_unit_supported(profile, package_target.unit_key()) {
            return None;
        }
        self.plane_execution_seed(package_target.target(), profile, stage)
    }
}

fn compiler_unit_supported(profile: LanguageProfile, unit_key: &CompilationUnitKeyV2) -> bool {
    match unit_key {
        CompilationUnitKeyV2::PackageRoot => true,
        CompilationUnitKeyV2::RustCrate { .. } => profile.language() == Language::Rust,
        CompilationUnitKeyV2::CSharpProject { .. } => profile.language() == Language::CSharp,
        _ => false,
    }
}

/// Complete fixed-cardinality semantic capability snapshot owned by one compiler runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalCompilerCapabilities {
    advertised: [LocalCompilerCapability; LanguageProfile::PRODUCT_PROFILES.len()],
    rust_editions: [LocalCompilerCapability; 3],
}

impl LocalCompilerCapabilities {
    fn from_configuration(configuration: &LocalCompilerRuntimeConfiguration) -> Self {
        let target_platform_identity = runtime_target_platform_identity();
        let capability_for_profile = |profile: LanguageProfile| {
            let language = profile.language();
            let toolchain = language.native_tool();
            let runtime = configuration
                .toolchains
                .iter()
                .find(|candidate| candidate.tool == toolchain);
            let identity = runtime.and_then(|candidate| candidate.identity);
            let local_authority_fingerprint =
                package_authority_fingerprint(profile, &configuration.package_authority, runtime);
            let environment_identity = Some(compiler_environment_identity(profile));
            let portable_options_digest = portable_invocation_options_digest(
                profile,
                &configuration.package_authority,
                runtime,
            );
            let lineage = identity
                .zip(local_authority_fingerprint)
                .zip(environment_identity)
                .zip(target_platform_identity)
                .map(|(((identity, authority), environment), platform)| {
                    CompilerSessionLineage(semantic_lineage(
                        profile,
                        toolchain,
                        identity,
                        authority,
                        environment,
                        platform,
                    ))
                });
            let manifest = identity
                .zip(local_authority_fingerprint)
                .zip(environment_identity)
                .zip(target_platform_identity)
                .map(|(((identity, authority), environment), platform)| {
                    semantic_recipe(
                        profile,
                        toolchain,
                        identity,
                        authority,
                        environment,
                        platform,
                    )
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
                environment_identity,
                target_platform_identity,
                portable_options_digest,
                lineage,
                manifest,
                state,
            }
        };
        Self {
            advertised: LanguageProfile::PRODUCT_PROFILES.map(capability_for_profile),
            rust_editions: [
                backend_semantic::vocabulary::RustEdition::Rust2015,
                backend_semantic::vocabulary::RustEdition::Rust2018,
                backend_semantic::vocabulary::RustEdition::Rust2021,
            ]
            .map(|edition| capability_for_profile(LanguageProfile::Rust(edition))),
        }
    }

    /// Returns all language slots in canonical compiler order.
    #[must_use]
    pub const fn as_slice(&self) -> &[LocalCompilerCapability] {
        &self.advertised
    }

    /// Finds the capability that governs one profile without allocating or
    /// consulting ambient state.
    ///
    /// The public table advertises one current product profile per language.
    /// Earlier Rust editions have their own exact, internally attested rows:
    /// their environment, options, and recipe identities must bind the selected
    /// edition instead of borrowing the Rust 2024 row.
    #[must_use]
    pub fn for_profile(&self, profile: LanguageProfile) -> LocalCompilerCapability {
        self.advertised
            .iter()
            .copied()
            .find(|capability| capability.profile == profile)
            .or_else(|| {
                let LanguageProfile::Rust(_) = profile else {
                    return None;
                };
                self.rust_editions
                    .iter()
                    .copied()
                    .find(|capability| capability.profile == profile)
            })
            .expect("fixed compiler capability table governs every compiled profile")
    }
}

fn package_authority_fingerprint(
    profile: LanguageProfile,
    authority: &LocalRuntimePackageAuthority,
    runtime: Option<&LocalRuntimeToolchain>,
) -> Option<[u8; 32]> {
    let mut identity = blake3::Hasher::new();
    identity.update(b"compiler-application.package-authority.v1\0");
    identity.update(&<[u8; 2]>::from(profile));
    match profile.language() {
        Language::Rust => {
            let rust = authority.rust.as_ref()?;
            identity.update(
                backend_frontend_rust::legacy::RUST_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1
                    .as_bytes(),
            );
            let cargo = rust.toolchain.cargo.as_deref()?;
            let cargo_home = rust.toolchain.cargo_home.as_deref()?;
            update_path_identity(&mut identity, &rust.toolchain.tool);
            update_path_identity(&mut identity, &rust.toolchain.sysroot);
            update_path_identity(&mut identity, cargo);
            update_path_identity(&mut identity, cargo_home);
            update_optional_string_identity(
                &mut identity,
                rust.toolchain.rustup_toolchain.as_deref(),
            );
            update_optional_path_identity(&mut identity, rust.toolchain.rustup_home.as_deref());
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
            identity.update(&[u8::from(authority.python_toolchain_identity.is_some())]);
            if let Some(pyrefly_toolchain) = authority.python_toolchain_identity {
                identity.update(pyrefly_toolchain.content_id().as_ref());
            }
        }
        Language::Go => {
            identity.update(&authority.go.as_ref()?.local_configuration_fingerprint());
        }
        Language::Java => {
            let java = authority.java.as_ref()?;
            identity.update(
                backend_frontend_java::legacy::JAVA_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1
                    .as_bytes(),
            );
            update_path_identity(&mut identity, java.toolchain.root());
            identity.update(&(java.classpath.len() as u64).to_be_bytes());
            for path in &java.classpath {
                update_path_identity(&mut identity, path);
            }
        }
        Language::CSharp => {
            let csharp = authority.csharp.as_ref()?;
            identity.update(
                backend_frontend_csharp::legacy::CSHARP_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1
                    .as_bytes(),
            );
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
            let clang = authority.clang.as_ref()?;
            let driver = runtime?.executable_path()?;
            if clang.driver() != driver {
                return None;
            }
            identity.update(b"direct-libclang-package-authority-v2\0");
            identity.update(&clang_authority_fingerprint(
                clang.driver(),
                clang.resource_dir(),
                clang.sysroot(),
                clang.libclang_path(),
                clang.system_include_dirs().iter().map(PathBuf::as_path),
            ));
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

fn clang_authority_fingerprint<'path>(
    driver: &Path,
    resource_dir: &Path,
    sysroot: Option<&Path>,
    libclang: &Path,
    system_include_dirs: impl IntoIterator<Item = &'path Path>,
) -> [u8; 32] {
    let mut identity = blake3::Hasher::new();
    identity.update(b"compiler-application.clang-native-authority.v1\0");
    update_path_identity(&mut identity, driver);
    update_path_identity(&mut identity, resource_dir);
    update_optional_path_identity(&mut identity, sysroot);
    update_path_identity(&mut identity, libclang);
    let system_include_dirs: Vec<_> = system_include_dirs.into_iter().collect();
    identity.update(&(system_include_dirs.len() as u64).to_be_bytes());
    for directory in system_include_dirs {
        update_path_identity(&mut identity, directory);
    }
    *identity.finalize().as_bytes()
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

/// Identifies the closed environment recipe supplied to native compiler children.
/// Exact tools, versions, portable options, and target platform are attested in
/// their own recipe fields; adapter-specific fixed environment values are
/// versioned with the native payload.
fn compiler_environment_identity(profile: LanguageProfile) -> [u8; 32] {
    let mut environment = blake3::Hasher::new();
    environment.update(b"compiler-application.execution-environment.v2\0");
    environment.update(&<[u8; 2]>::from(profile));
    environment.update(crate::application::NATIVE_COMPILER_ENVIRONMENT_POLICY_ID);
    environment.update(&backend_compile::NATIVE_PAYLOAD_VERSION.to_be_bytes());
    *environment.finalize().as_bytes()
}

fn runtime_target_platform_identity() -> Option<[u8; 32]> {
    let os = runtime_host_os();
    let architecture = runtime_host_architecture();
    if os == 0 || architecture == 0 {
        return None;
    }
    let mut platform = blake3::Hasher::new();
    platform.update(b"compiler-application.target-platform.v1\0");
    platform.update(&[os, architecture, usize::BITS as u8]);
    platform.update(&[u8::from(cfg!(target_endian = "little"))]);
    Some(*platform.finalize().as_bytes())
}

/// Identity of the source-independent invocation options that this runtime can prove portably.
///
/// An adapter returns a digest only when its ordered options and selected executable can be bound
/// to typed toolchain evidence. External module trees and classpaths fail closed until their bytes
/// are admitted into a typed input closure. Local compilation remains available in every mode.
fn portable_invocation_options_digest(
    profile: LanguageProfile,
    authority: &LocalRuntimePackageAuthority,
    runtime: Option<&LocalRuntimeToolchain>,
) -> Option<[u8; 32]> {
    let mut options = blake3::Hasher::new();
    options.update(b"backend.compiler.application.portable-options.v1\0");
    options.update(env!("CARGO_PKG_VERSION").as_bytes());
    options.update(&backend_compile::NATIVE_PAYLOAD_VERSION.to_be_bytes());
    options.update(&<[u8; 2]>::from(profile));
    match profile.language() {
        Language::Rust => {
            let rust = authority.rust.as_ref()?;
            options.update(b"rust-cargo-features-v1\0");
            options.update(&[
                u8::from(rust.all_features),
                u8::from(rust.no_default_features),
            ]);
            update_string_list_identity(&mut options, &rust.features);
        }
        Language::CSharp => {
            let csharp = authority.csharp.as_ref()?;
            // The reference tree is a semantic input but has no portable content identity in the
            // current admission API. Do not collapse two different local paths into one recipe.
            if csharp.reference_directory.is_some() {
                return None;
            }
            options.update(b"csharp-helper-options-v1\0");
            update_optional_string_identity(&mut options, csharp.assembly_name.as_deref());
            update_string_list_identity(&mut options, &csharp.define_symbols);
            update_string_list_identity(&mut options, &csharp.extra_usings);
            options.update(&[
                u8::from(csharp.implicit_usings),
                u8::from(csharp.include_non_public),
            ]);
        }
        Language::Clang => {
            // clang-sys 1.9.1 cannot prove that its in-process runtime loader
            // opened this configured path. Keep C-family execution local until
            // the loaded library itself can be attested by the frontend.
            return None;
        }
        Language::TypeScript => {
            let checker = authority.typescript.as_ref()?;
            if checker.portable_invocation_mode() != TypeScriptInvocationModeV1::ReportProgram
                || !checker.uses_toolchain_executable(runtime?.executable_path()?)
            {
                // Node mode also depends on the external TypeScript module tree. Until that tree
                // is admitted as a typed closure, its identity cannot be stated portably.
                return None;
            }
            options.update(b"typescript-report-program-v1\0");
        }
        Language::Python => {
            let checker = authority.python.as_ref()?;
            let pyrefly_toolchain = authority.python_toolchain_identity?;
            options.update(b"python-pyrefly-v1\0");
            options.update(pyrefly_toolchain.content_id().as_ref());
            update_owned_string_identity(
                &mut options,
                checker.portable_invocation_options().arguments(),
            );
        }
        Language::Go => {
            let oracle = authority.go.as_ref()?;
            if oracle.portable_invocation_options().mode() != GoOracleInvocationModeV1::GoToolchain
                || !oracle.uses_toolchain_executable(runtime?.executable_path()?)
            {
                // Direct oracle binaries have no admitted executable-version identity.
                return None;
            }
            let portable = oracle.portable_invocation_options();
            options.update(b"go-vendored-oracle-v1\0");
            options.update(portable.helper_source_identity()?.as_ref());
        }
        Language::Java => {
            let java = authority.java.as_ref()?;
            if !java.classpath.is_empty() {
                // External classpath entries are semantic inputs, and their content is not yet
                // captured in the typed workspace closure.
                return None;
            }
            let compiler_name = if cfg!(windows) { "javac.exe" } else { "javac" };
            let configured_compiler = java.toolchain.root().join("bin").join(compiler_name);
            if runtime?.executable_path()? != configured_compiler {
                return None;
            }
            options.update(b"java-jdk-empty-classpath-v1\0");
        }
    }
    Some(*options.finalize().as_bytes())
}

fn update_owned_string_identity(identity: &mut blake3::Hasher, values: &[String]) {
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
    environment_identity: [u8; 32],
    target_platform_identity: [u8; 32],
) -> [u8; 32] {
    let mut manifest = blake3::Hasher::new();
    manifest.update(b"compiler-application.semantic-capability.v3\0");
    manifest.update(env!("CARGO_PKG_VERSION").as_bytes());
    manifest.update(&<[u8; 2]>::from(profile));
    manifest.update(&[u8::from(toolchain)]);
    manifest.update(identity.as_ref());
    manifest.update(&local_authority_fingerprint);
    manifest.update(&environment_identity);
    manifest.update(&target_platform_identity);
    manifest.update(&backend_compile::NATIVE_PAYLOAD_VERSION.to_be_bytes());
    *manifest.finalize().as_bytes()
}

fn semantic_lineage(
    profile: LanguageProfile,
    toolchain: NativeTool,
    identity: ContentId<ToolchainDomain>,
    local_authority_fingerprint: [u8; 32],
    environment_identity: [u8; 32],
    target_platform_identity: [u8; 32],
) -> [u8; 32] {
    let mut lineage = blake3::Hasher::new();
    lineage.update(b"compiler-application.semantic-lineage.v2\0");
    lineage.update(&<[u8; 2]>::from(profile));
    lineage.update(&[u8::from(toolchain)]);
    lineage.update(identity.as_ref());
    lineage.update(&local_authority_fingerprint);
    lineage.update(&environment_identity);
    lineage.update(&target_platform_identity);
    *lineage.finalize().as_bytes()
}

const fn runtime_host_os() -> u8 {
    if cfg!(target_os = "macos") {
        1
    } else if cfg!(target_os = "linux") {
        2
    } else if cfg!(target_os = "windows") {
        3
    } else {
        0
    }
}

const fn runtime_host_architecture() -> u8 {
    if cfg!(target_arch = "aarch64") {
        1
    } else if cfg!(target_arch = "x86_64") {
        2
    } else {
        0
    }
}

const INPUT_WITNESS_MAGIC: [u8; 8] = *b"NUDXIW01";
const MAX_INPUT_WITNESS_ENTRIES: usize = 4_096;

/// Small durable hint cache for deciding whether a package/profile has an
/// already published generation for its exact input witness. A missing,
/// malformed, or unreadable entry always requests a rebuild.
struct CompilerInputWitnessStore {
    directory: Box<Path>,
    lock: Mutex<()>,
    sequence: AtomicU64,
}

impl CompilerInputWitnessStore {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory: directory.into_boxed_path(),
            lock: Mutex::new(()),
            sequence: AtomicU64::new(0),
        }
    }

    fn is_current(
        &self,
        package_root: &Path,
        profile: LanguageProfile,
        lineage: CompilerSessionLineage,
        witness: ExactInputWitness,
        selected_binding: [u8; 32],
    ) -> bool {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.entry_path(package_root, profile, lineage);
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };
        use std::io::Read as _;
        let expected = INPUT_WITNESS_MAGIC.len() + witness.0.len() + selected_binding.len();
        let mut bytes = Vec::with_capacity(expected.saturating_add(1));
        if file
            .take(u64::try_from(expected.saturating_add(1)).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .is_err()
        {
            return false;
        }
        bytes.len() == INPUT_WITNESS_MAGIC.len() + witness.0.len() + selected_binding.len()
            && bytes.starts_with(&INPUT_WITNESS_MAGIC)
            && bytes[INPUT_WITNESS_MAGIC.len()..INPUT_WITNESS_MAGIC.len() + witness.0.len()]
                == witness.0
            && bytes[INPUT_WITNESS_MAGIC.len() + witness.0.len()..] == selected_binding
    }

    fn remember(
        &self,
        package_root: &Path,
        profile: LanguageProfile,
        lineage: CompilerSessionLineage,
        witness: ExactInputWitness,
        selected_binding: [u8; 32],
    ) -> std::io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::fs::create_dir_all(&self.directory)?;
        let path = self.entry_path(package_root, profile, lineage);
        if !path.exists()
            && std::fs::read_dir(&self.directory)?.count() >= MAX_INPUT_WITNESS_ENTRIES
        {
            return Err(std::io::Error::other(
                "compiler input witness cache reached its entry bound",
            ));
        }
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let temporary = self.directory.join(format!(
            ".input-witness-{}-{sequence}.tmp",
            std::process::id()
        ));
        let result = (|| {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&INPUT_WITNESS_MAGIC)?;
            file.write_all(&witness.0)?;
            file.write_all(&selected_binding)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    fn entry_path(
        &self,
        package_root: &Path,
        profile: LanguageProfile,
        lineage: CompilerSessionLineage,
    ) -> PathBuf {
        use std::fmt::Write as _;
        let mut key = blake3::Hasher::new();
        key.update(b"compiler-application.input-witness-key.v1\0");
        let root = package_root.as_os_str().as_encoded_bytes();
        key.update(&(root.len() as u64).to_be_bytes());
        key.update(root);
        key.update(&<[u8; 2]>::from(profile));
        key.update(&lineage.0);
        let mut name = String::with_capacity(64 + 8);
        for byte in key.finalize().as_bytes() {
            let _ = write!(name, "{byte:02x}");
        }
        self.directory.join(format!("{name}.witness"))
    }
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

    fn executable_path(&self) -> Option<&Path> {
        self.executable.as_deref()
    }

    pub(crate) const fn probe_failed(tool: NativeTool) -> Self {
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

/// Exact bounded version identity of the separately configured Pyrefly executable.
///
/// The NativeTool::Python row describes the interpreter and cannot stand in for this checker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PyreflyToolchainIdentity(ContentId<ToolchainDomain>);

impl PyreflyToolchainIdentity {
    pub(crate) fn from_version_output(output: &[u8]) -> Self {
        Self(ContentId::from_canonical_bytes(output))
    }

    /// Returns the admitted identity derived from the bounded Pyrefly `--version` output.
    #[must_use]
    pub const fn content_id(self) -> ContentId<ToolchainDomain> {
        self.0
    }
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
    /// Explicit Clang driver, resource/sysroot, and selected libclang paths.
    /// Missing or unverified native-library selection withholds C-family authority.
    pub clang: Option<backend_frontend_clang::ClangAuthorityEnvironment>,
    /// TypeScript checker authority.
    pub typescript: Option<ExplicitTypeScriptChecker>,
    /// Python Pyrefly authority.
    pub python: Option<Pyrefly>,
    /// Separate bounded version identity for the configured Pyrefly executable.
    /// Missing identity keeps local compilation available and withholds remote recipe admission.
    pub python_toolchain_identity: Option<PyreflyToolchainIdentity>,
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
    embedding_runtime: Option<Arc<EmbeddingExecutable>>,
    embedding_requirement: EmbeddingRequirement,
    embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
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
            embedding_runtime: None,
            embedding_requirement: EmbeddingRequirement::Optional,
            embedding_provisioning_failure: None,
            timeout,
            publication_limits,
            scratch,
        })
    }

    /// Uses an already activated bounded embedding runtime for each package source.
    #[must_use]
    pub fn with_embedding_runtime(
        mut self,
        runtime: Arc<EmbeddingExecutable>,
        requirement: EmbeddingRequirement,
    ) -> Self {
        self.embedding_runtime = Some(runtime);
        self.embedding_requirement = requirement;
        self.embedding_provisioning_failure = None;
        self
    }

    /// Records configured embedding provisioning that failed before activation.
    ///
    /// Optional compilation retains IR and exposes this closed cause in staged status. Required
    /// embedding compilation fails at the normal staging boundary.
    #[must_use]
    pub fn with_embedding_provisioning_failure(
        mut self,
        cause: EmbeddingProvisioningFailure,
        requirement: EmbeddingRequirement,
    ) -> Self {
        self.embedding_runtime = None;
        self.embedding_requirement = requirement;
        self.embedding_provisioning_failure = Some(cause);
        self
    }

    /// Selects optional or required embedding behavior independently of runtime presence.
    #[must_use]
    pub fn with_embedding_requirement(mut self, requirement: EmbeddingRequirement) -> Self {
        self.embedding_requirement = requirement;
        self
    }

    /// Returns the same compiler identity snapshot that runtime admission derives from this
    /// configuration, without starting compiler threads or touching the configured paths.
    #[must_use]
    pub(crate) fn capabilities(&self) -> LocalCompilerCapabilities {
        LocalCompilerCapabilities::from_configuration(self)
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
    /// One lane could not allocate its private reusable scratch.
    #[error("could not create compiler lane {lane} scratch")]
    LaneScratch {
        /// Bounded lane ordinal.
        lane: usize,
        /// Exact scratch allocation rejection.
        #[source]
        source: LocalCompilerScratchError,
    },
    /// The operating system could not create one bounded native compilation lane.
    #[error("could not spawn compiler lane {lane}")]
    LaneSpawn {
        /// Bounded lane ordinal.
        lane: usize,
        /// Exact thread-spawn failure.
        #[source]
        source: std::io::Error,
    },
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

/// Cloneable bounded client for one local compiler execution kernel.
pub struct LocalCompilerClient {
    shared: Arc<RuntimeShared>,
    client_id: u64,
    last_image: Mutex<Option<Arc<SemanticImageSnapshot>>>,
}

impl Clone for LocalCompilerClient {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            client_id: self.shared.next_client.fetch_add(1, Ordering::Relaxed),
            last_image: Mutex::new(None),
        }
    }
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
        let witness_parent = configuration
            .paths
            .journal_directory
            .parent()
            .unwrap_or(&configuration.paths.journal_directory);
        let input_witnesses = Arc::new(CompilerInputWitnessStore::new(
            witness_parent.join("compiler-input-witnesses"),
        ));
        let pending_probes = configuration
            .toolchains
            .iter()
            .filter_map(LocalRuntimeToolchain::probe_request)
            .collect::<Vec<_>>();
        let capabilities = Arc::new(RwLock::new(LocalCompilerCapabilities::from_configuration(
            &configuration,
        )));
        let capability_signal = Arc::new(CapabilitySignal::new());
        let (command_tx, command_rx) = sync_channel(8);
        let (probe_tx, probe_rx) = channel();
        let (startup_tx, startup_rx) = sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let alive = Arc::new(AtomicBool::new(true));
        let worker_alive = Arc::clone(&alive);
        let worker_capabilities = Arc::clone(&capabilities);
        let worker_capability_signal = Arc::clone(&capability_signal);
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
                    &worker_capability_signal,
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
                        requests: Mutex::new(HashMap::new()),
                        next_request: AtomicU64::new(1),
                        next_client: AtomicU64::new(2),
                        alive,
                        capabilities,
                        capability_signal,
                        input_witnesses,
                    }),
                    client_id: 1,
                    last_image: Mutex::new(None),
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

    /// Requests cancellation of every currently admitted attempt on this client handle.
    pub fn cancel_active(&self) {
        let requests = self
            .shared
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ((client_id, _), token) in requests.iter() {
            if *client_id == self.client_id {
                token.store(true, Ordering::Release);
            }
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

    /// Returns a typed exact execution identity only while this opened
    /// compiler owner is alive and the selected profile has every required
    /// runtime facet admitted. The caller's target is included verbatim and
    /// is never used to synthesize toolchain, environment, platform, or recipe
    /// evidence.
    #[must_use]
    pub fn execution_identity(
        &self,
        target: ContentId<CompilationTargetDomain>,
        profile: LanguageProfile,
        stage: Stage,
    ) -> Option<LocalCompilerExecutionIdentity> {
        if !self.shared.alive.load(Ordering::Acquire) {
            return None;
        }
        self.capabilities()
            .for_profile(profile)
            .execution_identity(target, profile, stage)
    }

    /// Returns runtime evidence keyed by the exact canonical package and native-unit target.
    ///
    /// The target is derived from both package URL and `CompilationUnitKeyV2`; callers cannot
    /// substitute a package-root identity for a project/crate unit identity.
    #[must_use]
    pub fn execution_identity_for_unit(
        &self,
        package_target: &CompilerPackageTargetV2,
        profile: LanguageProfile,
        stage: Stage,
    ) -> Option<LocalCompilerExecutionIdentity> {
        if !self.shared.alive.load(Ordering::Acquire) {
            return None;
        }
        self.capabilities()
            .for_profile(profile)
            .execution_identity_for_unit(package_target, profile, stage)
    }

    /// Checks whether the exact package inputs and recipe were successfully
    /// compiled before. Missing or unreadable cache entries request a compile.
    #[must_use]
    pub fn exact_input_witness_is_current(
        &self,
        package_root: &Path,
        profile: LanguageProfile,
        lineage: CompilerSessionLineage,
        witness: ExactInputWitness,
        selected_binding: [u8; 32],
    ) -> bool {
        self.shared.input_witnesses.is_current(
            package_root,
            profile,
            lineage,
            witness,
            selected_binding,
        )
    }

    /// Records an exact witness after the caller has validated a successful
    /// semantic publication. A failed write is a cache miss on the next run.
    pub fn remember_exact_input_witness(
        &self,
        package_root: &Path,
        profile: LanguageProfile,
        lineage: CompilerSessionLineage,
        witness: ExactInputWitness,
        selected_binding: [u8; 32],
    ) -> bool {
        self.shared
            .input_witnesses
            .remember(package_root, profile, lineage, witness, selected_binding)
            .is_ok()
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
        let lease = RequestLease::acquire(&self.shared, self.client_id, facts)?;
        self.wait_for_toolchain(facts, &lease.cancelled)?;
        let (response_tx, response_rx) = sync_channel(1);
        let command = RuntimeCommand::Compile {
            request,
            response: response_tx,
            request_id: lease.request_id,
            cancelled: Arc::clone(&lease.cancelled),
        };
        let sender = self
            .shared
            .command
            .as_ref()
            .ok_or_else(|| facts.terminal(CompilerRuntimeCause::RequestOwnerStopped))?;
        match sender.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(facts.terminal(CompilerRuntimeCause::QueueFull));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(facts.terminal(CompilerRuntimeCause::RequestOwnerStopped));
            }
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
        let result = result.map(|(generated, image)| {
            *self
                .last_image
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(image));
            generated
        });
        drop(lease);
        result
    }

    /// Waits for the selected profile's explicit toolchain probe to settle.
    ///
    /// Host admission intentionally launches probes after the process owner
    /// becomes reachable, so an unrelated slow tool (for example a hanging Go
    /// probe) cannot delay listener readiness. A compile for a profile whose
    /// toolchain is still being probed must therefore wait before it is sent to
    /// the compiler owner; otherwise the owner would turn a transient
    /// `Probing` state into a durable `Unavailable` semantic publication.
    fn wait_for_toolchain(
        &self,
        facts: RequestFacts,
        cancelled: &AtomicBool,
    ) -> Result<(), CompilerTerminal> {
        const PROBE_WAIT: Duration = Duration::from_secs(6);
        const SIGNAL_WAIT: Duration = Duration::from_millis(25);
        let deadline = Instant::now()
            .checked_add(PROBE_WAIT)
            .expect("fixed probe wait fits the monotonic clock");
        let mut signal = self
            .shared
            .capability_signal
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let state = self.capabilities().for_profile(facts.profile).state();
            if !matches!(state, LocalCompilerCapabilityState::Probing) {
                return Ok(());
            }
            if !self.shared.alive.load(Ordering::Acquire) {
                return Err(facts.terminal(CompilerRuntimeCause::RequestOwnerStopped));
            }
            if cancelled.load(Ordering::Acquire) {
                return Err(facts.terminal(CompilerRuntimeCause::RequestCancelled));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(facts.terminal(CompilerRuntimeCause::ToolchainProbeTimeout));
            }
            let timeout = remaining.min(SIGNAL_WAIT);
            signal = self
                .shared
                .capability_signal
                .changed
                .wait_timeout(signal, timeout)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
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
        if let Some(image) = self
            .last_image
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|image| image.authority == requested)
            .cloned()
        {
            return image.try_clone();
        }
        let lease = RequestLease::acquire_snapshot(&self.shared, self.client_id, requested)?;
        let (response, returned) = sync_channel(1);
        let sender = self
            .shared
            .command
            .as_ref()
            .ok_or(SemanticImageAccessError::OwnerStopped { requested })?;
        match sender.try_send(RuntimeCommand::SemanticImage {
            requested,
            response,
            cancelled: Arc::clone(&lease.cancelled),
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(SemanticImageAccessError::RequestInFlight { requested });
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(SemanticImageAccessError::OwnerStopped { requested });
            }
        }
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
        let lease = RequestLease::acquire(&self.shared, self.client_id, facts)
            .map_err(PackageSemanticRuntimeError::Runtime)?;
        self.wait_for_toolchain(facts, &lease.cancelled)
            .map_err(PackageSemanticRuntimeError::Runtime)?;
        let (response, returned) = sync_channel(1);
        let sender = self.shared.command.as_ref().ok_or_else(|| {
            PackageSemanticRuntimeError::Runtime(
                facts.terminal(CompilerRuntimeCause::RequestOwnerStopped),
            )
        })?;
        match sender.try_send(RuntimeCommand::CompilePackageSources {
            request,
            response,
            request_id: lease.request_id,
            cancelled: Arc::clone(&lease.cancelled),
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::QueueFull),
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::RequestOwnerStopped),
                ));
            }
        }
        let result = returned.recv().map_err(|_| {
            PackageSemanticRuntimeError::Runtime(
                facts.terminal(CompilerRuntimeCause::ResponseOwnerStopped),
            )
        })?;
        drop(lease);
        result
    }

    /// Compiles a complete package frontier into canonical output bytes without selecting the
    /// compiler owner's local journal head.
    ///
    /// # Errors
    ///
    /// Returns the same bounded runtime, authority, and compilation failures as local package
    /// compilation, plus exact staged-output preparation failures.
    pub fn compile_package_sources_staged(
        &self,
        request: OwnedPackageSourceSet,
    ) -> Result<StagedSemanticPackage, PackageSemanticRuntimeError> {
        let facts = request.facts();
        let lease = RequestLease::acquire(&self.shared, self.client_id, facts)
            .map_err(PackageSemanticRuntimeError::Runtime)?;
        self.wait_for_toolchain(facts, &lease.cancelled)
            .map_err(PackageSemanticRuntimeError::Runtime)?;
        let (response, returned) = sync_channel(1);
        let sender = self.shared.command.as_ref().ok_or_else(|| {
            PackageSemanticRuntimeError::Runtime(
                facts.terminal(CompilerRuntimeCause::RequestOwnerStopped),
            )
        })?;
        match sender.try_send(RuntimeCommand::CompilePackageSourcesStaged {
            request,
            response,
            request_id: lease.request_id,
            cancelled: Arc::clone(&lease.cancelled),
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::QueueFull),
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::RequestOwnerStopped),
                ));
            }
        }
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
            profile,
            language: profile.language(),
            stage: Stage::LowerIr,
            target: None,
        };
        let lease = RequestLease::acquire(&self.shared, self.client_id, facts)
            .map_err(PackageSemanticRuntimeError::Runtime)?;
        let (response, returned) = sync_channel(1);
        let sender = self.shared.command.as_ref().ok_or_else(|| {
            PackageSemanticRuntimeError::Runtime(
                facts.terminal(CompilerRuntimeCause::RequestOwnerStopped),
            )
        })?;
        match sender.try_send(RuntimeCommand::ActivateSemanticGeneration {
            profile,
            manifest,
            binding,
            response,
            cancelled: Arc::clone(&lease.cancelled),
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::QueueFull),
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::RequestOwnerStopped),
                ));
            }
        }
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
    requests: Mutex<HashMap<(u64, u64), Arc<AtomicBool>>>,
    next_request: AtomicU64,
    next_client: AtomicU64,
    alive: Arc<AtomicBool>,
    capabilities: Arc<RwLock<LocalCompilerCapabilities>>,
    capability_signal: Arc<CapabilitySignal>,
    input_witnesses: Arc<CompilerInputWitnessStore>,
}

struct CapabilitySignal {
    lock: Mutex<()>,
    changed: Condvar,
}

impl CapabilitySignal {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            changed: Condvar::new(),
        }
    }
}

impl Drop for RuntimeShared {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        for token in self
            .requests
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            token.store(true, Ordering::Release);
        }
        drop(self.command.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct RequestLease<'shared> {
    shared: &'shared RuntimeShared,
    client_id: u64,
    request_id: u64,
    cancelled: Arc<AtomicBool>,
}

impl<'shared> RequestLease<'shared> {
    fn acquire(
        shared: &'shared RuntimeShared,
        client_id: u64,
        facts: RequestFacts,
    ) -> Result<Self, CompilerTerminal> {
        if !shared.alive.load(Ordering::Acquire) {
            return Err(facts.terminal(CompilerRuntimeCause::RequestOwnerStopped));
        }
        let request_id = shared.next_request.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut requests = shared
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if requests.len() >= MAX_ADMITTED_COMPILER_REQUESTS {
            return Err(facts.terminal(CompilerRuntimeCause::QueueFull));
        }
        requests.insert((client_id, request_id), Arc::clone(&cancelled));
        Ok(Self {
            shared,
            client_id,
            request_id,
            cancelled,
        })
    }

    fn acquire_snapshot(
        shared: &'shared RuntimeShared,
        client_id: u64,
        requested: SemanticImageAuthority,
    ) -> Result<Self, SemanticImageAccessError> {
        if !shared.alive.load(Ordering::Acquire) {
            return Err(SemanticImageAccessError::OwnerStopped { requested });
        }
        let request_id = shared.next_request.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut requests = shared
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if requests.len() >= MAX_ADMITTED_COMPILER_REQUESTS {
            return Err(SemanticImageAccessError::RequestInFlight { requested });
        }
        requests.insert((client_id, request_id), Arc::clone(&cancelled));
        Ok(Self {
            shared,
            client_id,
            request_id,
            cancelled,
        })
    }
}

impl Drop for RequestLease<'_> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.shared
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(self.client_id, self.request_id));
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
                profile: *profile,
                language: profile.language(),
                stage: *stage,
                target: None,
            },
            Self::Package(request) => RequestFacts {
                profile: request.target.profile,
                language: request.target.profile.language(),
                stage: request.target.stage,
                target: Some(request.as_ref().identity),
            },
        }
    }

    fn source_count(&self) -> usize {
        match self {
            Self::Generate { .. } | Self::Package(_) => 1,
        }
    }

    fn source_identity(&self) -> Option<[u8; 32]> {
        match self {
            Self::Generate { source, .. } => Some(*blake3::hash(source.as_bytes()).as_bytes()),
            Self::Package(_) => None,
        }
    }
}

#[derive(Clone, Copy)]
struct RequestFacts {
    profile: LanguageProfile,
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
        request_id: u64,
        cancelled: Arc<AtomicBool>,
    },
    SemanticImage {
        requested: SemanticImageAuthority,
        response: SyncSender<Result<SemanticImageSnapshot, SemanticImageAccessError>>,
        cancelled: Arc<AtomicBool>,
    },
    CompilePackageSources {
        request: OwnedPackageSourceSet,
        response: SyncSender<Result<PublishedSemanticPackage, PackageSemanticRuntimeError>>,
        request_id: u64,
        cancelled: Arc<AtomicBool>,
    },
    CompilePackageSourcesStaged {
        request: OwnedPackageSourceSet,
        response: SyncSender<Result<StagedSemanticPackage, PackageSemanticRuntimeError>>,
        request_id: u64,
        cancelled: Arc<AtomicBool>,
    },
    ActivateSemanticGeneration {
        profile: LanguageProfile,
        manifest: CompilationManifestFacts,
        binding: CompilationBindingFacts,
        response: SyncSender<Result<ActivatedSemanticPackage, PackageSemanticRuntimeError>>,
        cancelled: Arc<AtomicBool>,
    },
}

enum RuntimeEvent {
    Phase(PackageCompilePhase),
    Complete(Result<(GeneratedArtifact, SemanticImageSnapshot), CompilerTerminal>),
}

enum LaneJob {
    Compile {
        request: OwnedCompilerRequest,
        response: SyncSender<RuntimeEvent>,
        cancelled: Arc<AtomicBool>,
        reservation: StagedOutputLease,
    },
    PackageSources {
        request: OwnedPackageSourceSet,
        response: PackageCompileResponse,
        cancelled: Arc<AtomicBool>,
        go_authority_witness: Option<GoPackageAuthorityWitness>,
        execution_identity: Option<LocalCompilerExecutionIdentity>,
        plane_execution_seed: Option<LocalCompilerPlaneExecutionSeed>,
        reservation: StagedOutputLease,
    },
}

enum PackageCompileResponse {
    Published(SyncSender<Result<PublishedSemanticPackage, PackageSemanticRuntimeError>>),
    Staged(SyncSender<Result<StagedSemanticPackage, PackageSemanticRuntimeError>>),
}

impl PackageCompileResponse {
    fn send_error(self, error: PackageSemanticRuntimeError) {
        match self {
            Self::Published(response) => {
                let _ = response.send(Err(error));
            }
            Self::Staged(response) => {
                let _ = response.send(Err(error));
            }
        }
    }
}

enum LaneCompletion {
    Compile {
        request: OwnedCompilerRequest,
        staged: Result<StagedCompilerArtifact, CompilerTerminal>,
        response: SyncSender<RuntimeEvent>,
        cancelled: Arc<AtomicBool>,
        _reservation: StagedOutputLease,
    },
    PackageSources {
        facts: RequestFacts,
        request: OwnedPackageSourceSet,
        staged: Result<StagedPackageCompilation, PackageSemanticRuntimeError>,
        response: PackageCompileResponse,
        cancelled: Arc<AtomicBool>,
        reservation: StagedOutputLease,
    },
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
    capability_signal: &CapabilitySignal,
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
                capability_signal.changed.notify_all();
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
        clang: configuration.package_authority.clang.as_ref(),
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
    let mut lane_scratches = Vec::new();
    for lane in 0..COMPILER_LANE_COUNT {
        match configuration.scratch.lane_scratch() {
            Ok(scratch) => lane_scratches.push(scratch),
            Err(source) => {
                if let Some(startup) = startup.take() {
                    let _ = startup.send(Err(LocalCompilerRuntimeOpenError::LaneScratch {
                        lane,
                        source,
                    }));
                }
                alive.store(false, Ordering::Release);
                return WorkerDisposition::Stop;
            }
        }
    }
    let capabilities = LocalCompilerCapabilities::from_configuration(configuration);
    let native_work_root = configuration.paths.native_work_directory.to_path_buf();
    let image_cap = maximum_image_bytes(configuration);
    let embedding_runtime = configuration.embedding_runtime.clone();
    let embedding_requirement = configuration.embedding_requirement;
    let embedding_provisioning_failure = configuration.embedding_provisioning_failure;
    let embedding_payload_max = embedding_runtime
        .as_ref()
        .map_or(0, |runtime| runtime.canonical_payload_max_bytes());
    let embedding_scratch_max = embedding_runtime
        .as_ref()
        .map_or(0, |runtime| runtime.maximum_inference_scratch_bytes());
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
    let disposition = run_worker_lanes(
        native_work_root,
        image_cap,
        &mut compiler,
        commands,
        probes,
        startup,
        alive,
        capabilities,
        lane_scratches,
        embedding_runtime,
        embedding_requirement,
        embedding_provisioning_failure,
        embedding_payload_max,
        embedding_scratch_max,
    );
    let _ = compiler.shutdown();
    disposition
}

fn run_worker_lanes(
    native_work_root: PathBuf,
    image_cap: usize,
    compiler: &mut LocalCompiler<'_, '_, '_>,
    commands: &Receiver<RuntimeCommand>,
    probes: &Receiver<ToolchainProbeObservation>,
    startup: &mut Option<SyncSender<Result<(), LocalCompilerRuntimeOpenError>>>,
    alive: &AtomicBool,
    capabilities: LocalCompilerCapabilities,
    lane_scratches: Vec<LocalCompilerScratch>,
    embedding_runtime: Option<Arc<EmbeddingExecutable>>,
    embedding_requirement: EmbeddingRequirement,
    embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
    embedding_payload_max: usize,
    embedding_scratch_max: usize,
) -> WorkerDisposition {
    let reservation_capacity = staged_output_reservation(
        MAX_MANIFEST_ENTRIES,
        image_cap,
        embedding_payload_max,
        embedding_scratch_max,
    )
    .unwrap_or(usize::MAX / COMPILER_LANE_COUNT);
    let budget = StagedOutputBudget::new(reservation_capacity.saturating_mul(COMPILER_LANE_COUNT));
    let base_execution = compiler.execution();
    let native_root = native_work_root;

    thread::scope(|scope| {
        let (completion_tx, completion_rx) = sync_channel(COMPILER_LANE_COUNT);
        let mut lane_senders = Vec::with_capacity(COMPILER_LANE_COUNT);
        for (lane, scratch) in lane_scratches.into_iter().enumerate() {
            let (sender, receiver) = sync_channel(1);
            let execution = base_execution
                .clone()
                .with_native_work_directory(native_root.join(format!("lane-{lane}")));
            let completions = completion_tx.clone();
            let lane_embedding_runtime = embedding_runtime.clone();
            let lane_embedding_provisioning_failure = embedding_provisioning_failure;
            let name = format!("nudox-compiler-lane-{lane}");
            if let Err(source) = thread::Builder::new()
                .name(name)
                .spawn_scoped(scope, move || {
                    run_lane(
                        execution,
                        scratch,
                        receiver,
                        completions,
                        lane_embedding_runtime,
                        embedding_requirement,
                        lane_embedding_provisioning_failure,
                    )
                })
            {
                if let Some(startup) = startup.take() {
                    let _ = startup.send(Err(LocalCompilerRuntimeOpenError::LaneSpawn {
                        lane,
                        source,
                    }));
                }
                alive.store(false, Ordering::Release);
                return WorkerDisposition::Stop;
            }
            lane_senders.push(sender);
        }
        drop(completion_tx);
        let Some(mut lane_queue) = BoundedLaneQueue::new(lane_senders) else {
            if let Some(startup) = startup.take() {
                let _ = startup.send(Err(LocalCompilerRuntimeOpenError::StartupOwnerStopped));
            }
            alive.store(false, Ordering::Release);
            return WorkerDisposition::Stop;
        };
        if let Some(startup) = startup.take()
            && startup.send(Ok(())).is_err()
        {
            alive.store(false, Ordering::Release);
            return WorkerDisposition::Stop;
        }

        let mut in_flight = 0_usize;
        let mut pending_reconfigure = None;
        let mut stopping = false;
        loop {
            if pending_reconfigure.is_none()
                && let Ok(observation) = probes.try_recv()
            {
                pending_reconfigure = Some(observation);
            }
            match completion_rx.try_recv() {
                Ok(completion) => {
                    in_flight = in_flight.saturating_sub(1);
                    if !publish_lane_completion(compiler, completion) {
                        stopping = true;
                        alive.store(false, Ordering::Release);
                    }
                    continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => stopping = true,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }

            if (stopping || pending_reconfigure.is_some()) && in_flight == 0 {
                drop(lane_queue);
                return if stopping {
                    WorkerDisposition::Stop
                } else if let Some(observation) = pending_reconfigure {
                    WorkerDisposition::Reconfigure(observation)
                } else {
                    WorkerDisposition::Stop
                };
            }
            if stopping || pending_reconfigure.is_some() {
                match completion_rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(completion) => {
                        in_flight = in_flight.saturating_sub(1);
                        if !publish_lane_completion(compiler, completion) {
                            stopping = true;
                            alive.store(false, Ordering::Release);
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => stopping = true,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                continue;
            }

            match commands.recv_timeout(Duration::from_millis(2)) {
                Ok(command) => dispatch_runtime_command(
                    command,
                    &mut lane_queue,
                    &budget,
                    &capabilities,
                    image_cap,
                    embedding_payload_max,
                    embedding_scratch_max,
                    embedding_requirement,
                    &mut in_flight,
                    compiler,
                    &mut stopping,
                    alive,
                ),
                Err(RecvTimeoutError::Disconnected) => stopping = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn dispatch_runtime_command(
    command: RuntimeCommand,
    lane_queue: &mut BoundedLaneQueue<LaneJob>,
    budget: &Arc<crate::application::executor::StagedOutputBudget>,
    capabilities: &LocalCompilerCapabilities,
    image_cap: usize,
    embedding_payload_max: usize,
    embedding_scratch_max: usize,
    embedding_requirement: EmbeddingRequirement,
    in_flight: &mut usize,
    compiler: &mut LocalCompiler<'_, '_, '_>,
    stopping: &mut bool,
    alive: &AtomicBool,
) {
    match command {
        RuntimeCommand::Compile {
            request,
            response,
            request_id: _,
            cancelled,
        } => {
            let facts = request.facts();
            let reservation_bytes =
                staged_output_reservation(request.source_count(), image_cap, 0, 0);
            let Some(reservation_bytes) = reservation_bytes else {
                let _ = response.send(RuntimeEvent::Complete(Err(
                    facts.terminal(CompilerRuntimeCause::QueueFull)
                )));
                return;
            };
            let reservation = match budget.reserve(reservation_bytes) {
                Ok(reservation) => reservation,
                Err(_) => {
                    let _ = response.send(RuntimeEvent::Complete(Err(
                        facts.terminal(CompilerRuntimeCause::QueueFull)
                    )));
                    return;
                }
            };
            let identity = lane_identity(facts, capabilities, request.source_identity());
            let job = LaneJob::Compile {
                request,
                response,
                cancelled,
                reservation,
            };
            match lane_queue.try_send(identity, job) {
                Ok(_) => *in_flight = in_flight.saturating_add(1),
                Err(LaneSendError::Full(job)) => {
                    reject_lane_job(job, CompilerRuntimeCause::QueueFull);
                }
                Err(LaneSendError::Closed(job)) => {
                    reject_lane_job(job, CompilerRuntimeCause::RequestOwnerStopped);
                }
            }
        }
        RuntimeCommand::CompilePackageSources {
            request,
            response,
            request_id: _,
            cancelled,
        } => queue_package_sources(
            request,
            PackageCompileResponse::Published(response),
            cancelled,
            lane_queue,
            budget,
            capabilities,
            image_cap,
            embedding_payload_max,
            embedding_scratch_max,
            embedding_requirement,
            in_flight,
        ),
        RuntimeCommand::CompilePackageSourcesStaged {
            request,
            response,
            request_id: _,
            cancelled,
        } => queue_package_sources(
            request,
            PackageCompileResponse::Staged(response),
            cancelled,
            lane_queue,
            budget,
            capabilities,
            image_cap,
            embedding_payload_max,
            embedding_scratch_max,
            embedding_requirement,
            in_flight,
        ),
        RuntimeCommand::SemanticImage {
            requested,
            response,
            cancelled,
        } => {
            if cancelled.load(Ordering::Acquire) {
                let _ = response.send(Err(SemanticImageAccessError::OwnerStopped { requested }));
                return;
            }
            match catch_unwind(AssertUnwindSafe(|| {
                compiler.semantic_image_snapshot(requested)
            })) {
                Ok(result) => {
                    let _ = response.send(result);
                }
                Err(payload) => {
                    let cause =
                        backend_library::interface::CompilerRuntimePanic::capture(payload.as_ref());
                    let _ = response.send(Err(SemanticImageAccessError::WorkerPanic {
                        requested,
                        cause,
                    }));
                    *stopping = true;
                    alive.store(false, Ordering::Release);
                }
            }
        }
        RuntimeCommand::ActivateSemanticGeneration {
            profile,
            manifest,
            binding,
            response,
            cancelled,
        } => {
            let facts = RequestFacts {
                profile,
                language: profile.language(),
                stage: Stage::LowerIr,
                target: None,
            };
            if cancelled.load(Ordering::Acquire) {
                let _ = response.send(Err(PackageSemanticRuntimeError::Runtime(
                    facts.terminal(CompilerRuntimeCause::RequestCancelled),
                )));
                return;
            }
            match catch_unwind(AssertUnwindSafe(|| {
                compiler
                    .activate_semantic_generation(manifest, binding)
                    .map_err(PackageSemanticRuntimeError::from)
            })) {
                Ok(result) => {
                    let _ = response.send(result);
                }
                Err(payload) => {
                    let cause =
                        backend_library::interface::CompilerRuntimePanic::capture(payload.as_ref());
                    let _ = response.send(Err(PackageSemanticRuntimeError::Runtime(
                        facts.terminal(CompilerRuntimeCause::WorkerPanic(cause)),
                    )));
                    *stopping = true;
                    alive.store(false, Ordering::Release);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn queue_package_sources(
    request: OwnedPackageSourceSet,
    response: PackageCompileResponse,
    cancelled: Arc<AtomicBool>,
    lane_queue: &mut BoundedLaneQueue<LaneJob>,
    budget: &Arc<StagedOutputBudget>,
    capabilities: &LocalCompilerCapabilities,
    image_cap: usize,
    embedding_payload_max: usize,
    embedding_scratch_max: usize,
    _embedding_requirement: EmbeddingRequirement,
    in_flight: &mut usize,
) {
    let facts = request.facts();
    let capability = capabilities.for_profile(facts.profile);
    let go_authority_witness =
        if facts.language == Language::Go && capability.local_authority_fingerprint().is_some() {
            match GoPackageAuthorityWitness::capture(&request.package_root) {
                Ok(witness) => Some(witness),
                Err(error) => {
                    response.send_error(PackageSemanticRuntimeError::Package(
                        PackageSemanticError::GoAuthorityWitness(error),
                    ));
                    return;
                }
            }
        } else {
            None
        };
    let package_authority_fingerprint = go_authority_witness.as_ref().and_then(|witness| {
        capability
            .local_authority_fingerprint()
            .map(|base| package_go_authority_fingerprint(base, witness))
    });
    let reusable_go_authority =
        go_authority_witness_allows_semantic_reuse(facts.language, go_authority_witness.as_ref());
    let reservation_bytes = staged_output_reservation(
        request.sources.len(),
        image_cap,
        embedding_payload_max,
        embedding_scratch_max,
    );
    let Some(reservation_bytes) = reservation_bytes else {
        response.send_error(PackageSemanticRuntimeError::Runtime(
            facts.terminal(CompilerRuntimeCause::QueueFull),
        ));
        return;
    };
    let reservation = match budget.reserve(reservation_bytes) {
        Ok(reservation) => reservation,
        Err(_) => {
            response.send_error(PackageSemanticRuntimeError::Runtime(
                facts.terminal(CompilerRuntimeCause::QueueFull),
            ));
            return;
        }
    };
    let portable_go_authority =
        go_authority_witness_is_portable(facts.language, go_authority_witness.as_ref());
    let execution_identity = portable_go_authority
        .then(|| {
            capability.execution_identity_for_unit(
                &request.package_target,
                facts.profile,
                facts.stage,
            )
        })
        .flatten()
        .map(|identity| match package_authority_fingerprint {
            Some(fingerprint) => identity.with_local_authority_fingerprint(fingerprint),
            None => identity,
        });
    let plane_execution_seed = reusable_go_authority
        .then(|| {
            capability.plane_execution_seed_for_unit(
                &request.package_target,
                facts.profile,
                facts.stage,
            )
        })
        .flatten()
        .map(|seed| match package_authority_fingerprint {
            Some(fingerprint) => seed.with_local_authority_fingerprint(fingerprint),
            None => seed,
        });
    let identity = lane_identity(facts, capabilities, None);
    let job = LaneJob::PackageSources {
        request,
        response,
        cancelled,
        go_authority_witness,
        execution_identity,
        plane_execution_seed,
        reservation,
    };
    match lane_queue.try_send(identity, job) {
        Ok(_) => *in_flight = in_flight.saturating_add(1),
        Err(LaneSendError::Full(job)) => {
            reject_lane_job(job, CompilerRuntimeCause::QueueFull);
        }
        Err(LaneSendError::Closed(job)) => {
            reject_lane_job(job, CompilerRuntimeCause::RequestOwnerStopped);
        }
    }
}

fn package_go_authority_fingerprint(
    base: [u8; 32],
    witness: &GoPackageAuthorityWitness,
) -> [u8; 32] {
    let mut identity = blake3::Hasher::new();
    identity.update(b"compiler-application.go-package-authority-witness.v1\0");
    identity.update(&base);
    identity.update(&witness.identity());
    *identity.finalize().as_bytes()
}

fn go_authority_witness_is_portable(
    language: Language,
    witness: Option<&GoPackageAuthorityWitness>,
) -> bool {
    language != Language::Go || witness.is_some_and(|witness| !witness.requires_local_execution())
}

fn go_authority_witness_allows_semantic_reuse(
    language: Language,
    witness: Option<&GoPackageAuthorityWitness>,
) -> bool {
    language != Language::Go || witness.is_some_and(GoPackageAuthorityWitness::is_complete)
}

fn maximum_image_bytes(configuration: &LocalCompilerRuntimeConfiguration) -> usize {
    MAX_PACKAGE_SEMANTIC_BYTES.saturating_add(
        configuration
            .package_authority
            .maximum_image_bytes
            .map_or(0, NonZeroUsize::get),
    )
}

fn staged_output_reservation(
    source_count: usize,
    image_cap: usize,
    embedding_payload_max: usize,
    embedding_scratch_max: usize,
) -> Option<usize> {
    if source_count == 0 || source_count > MAX_MANIFEST_ENTRIES {
        return None;
    }
    let semantic_and_ir = image_cap.checked_mul(2)?;
    let embedding_bytes = source_count
        .checked_mul(embedding_payload_max)?
        .min(MAX_PACKAGE_EMBEDDING_BYTES);
    let per_source = core::mem::size_of::<StagedCompilerArtifact>()
        .checked_add(core::mem::size_of::<StagedSemanticArtifact>())?
        .checked_add(core::mem::size_of::<[u8; 32]>())?
        .checked_add(core::mem::size_of::<Box<[u8]>>())?
        .checked_add(core::mem::size_of::<(&str, &str)>())?
        .checked_add(core::mem::size_of::<&str>())?
        .checked_add(core::mem::size_of::<StagedSemanticObjectClaim>().checked_mul(2)?)?
        .checked_add(core::mem::size_of::<usize>())?
        .checked_add(crate::publication::manifest::COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES)?;
    let metadata_bytes = source_count.checked_mul(per_source)?;
    let segment_count = source_count
        .checked_add(image_cap.div_ceil(backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES))?;
    let versioned_plane_bytes = segment_count
        .checked_mul(
            core::mem::size_of::<backend_semantic::ir::SemanticPlaneSegment>()
                .checked_add(core::mem::size_of::<StagedVersionedPlaneSegment<'static>>())?
                .checked_add(backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES / 1024)?,
        )?
        .checked_add(source_count.checked_mul(1024)?)?;
    semantic_and_ir
        .checked_add(embedding_bytes)?
        .checked_add(embedding_scratch_max)?
        .checked_add(MAX_PACKAGE_FRAGMENT_BYTES)?
        .checked_add(metadata_bytes)?
        .checked_add(versioned_plane_bytes)?
        .checked_add(crate::publication::manifest::COMPILATION_MANIFEST_HEADER_BYTES)?
        .checked_add(crate::publication::binding::COMPILATION_BINDING_BYTES)?
        .checked_add(core::mem::size_of::<StagedSemanticPackage>())
}

fn lane_identity(
    facts: RequestFacts,
    capabilities: &LocalCompilerCapabilities,
    source: Option<[u8; 32]>,
) -> LaneIdentity {
    let lineage = capabilities
        .for_profile(facts.profile)
        .session_lineage()
        .map_or([0; 32], CompilerSessionLineage::identity);
    LaneIdentity {
        capability_lineage: lineage,
        profile: facts.profile.into(),
        stage: facts.stage.into(),
        target: facts.target.map(|identity| *identity.as_ref()),
        source: if facts.target.is_none() { source } else { None },
    }
}

fn reject_lane_job(job: LaneJob, cause: CompilerRuntimeCause) {
    match job {
        LaneJob::Compile {
            request, response, ..
        } => {
            let _ = response.send(RuntimeEvent::Complete(Err(request.facts().terminal(cause))));
        }
        LaneJob::PackageSources {
            request, response, ..
        } => {
            response.send_error(PackageSemanticRuntimeError::Runtime(
                request.facts().terminal(cause),
            ));
        }
    }
}

fn run_lane(
    execution: LocalCompilerExecution<'_, '_>,
    mut scratch: LocalCompilerScratch,
    jobs: Receiver<LaneJob>,
    completions: SyncSender<LaneCompletion>,
    embedding_runtime: Option<Arc<EmbeddingExecutable>>,
    embedding_requirement: EmbeddingRequirement,
    embedding_provisioning_failure: Option<EmbeddingProvisioningFailure>,
) {
    while let Ok(job) = jobs.recv() {
        match job {
            LaneJob::Compile {
                request,
                response,
                cancelled,
                reservation,
            } => {
                let facts = request.facts();
                let staged = if cancelled.load(Ordering::Acquire) {
                    Err(facts.terminal(CompilerRuntimeCause::RequestCancelled))
                } else {
                    catch_unwind(AssertUnwindSafe(|| match &request {
                        OwnedCompilerRequest::Generate {
                            profile,
                            stage,
                            source,
                        } => execution.stage_generate(
                            CompilerRequest {
                                profile: *profile,
                                stage: *stage,
                                source,
                            },
                            &mut scratch,
                            &cancelled,
                            &mut |phase| {
                                if response.send(RuntimeEvent::Phase(phase)).is_err() {
                                    cancelled.store(true, Ordering::Release);
                                }
                            },
                        ),
                        OwnedCompilerRequest::Package(request) => execution.stage_package(
                            request,
                            &mut scratch,
                            &cancelled,
                            &mut |phase| {
                                if response.send(RuntimeEvent::Phase(phase)).is_err() {
                                    cancelled.store(true, Ordering::Release);
                                }
                            },
                        ),
                    }))
                    .unwrap_or_else(|payload| {
                        Err(facts.terminal(CompilerRuntimeCause::WorkerPanic(
                            backend_library::interface::CompilerRuntimePanic::capture(
                                payload.as_ref(),
                            ),
                        )))
                    })
                };
                let panicked = matches!(
                    &staged,
                    Err(CompilerTerminal::Runtime {
                        cause: CompilerRuntimeCause::WorkerPanic(_),
                        ..
                    })
                );
                let replacement = panicked.then(|| scratch.lane_scratch().ok()).flatten();
                let retire_lane = panicked && replacement.is_none();
                if let Some(replacement) = replacement {
                    scratch = replacement;
                }
                if completions
                    .send(LaneCompletion::Compile {
                        request,
                        staged,
                        response,
                        cancelled,
                        _reservation: reservation,
                    })
                    .is_err()
                {
                    return;
                }
                if retire_lane {
                    return;
                }
            }
            LaneJob::PackageSources {
                request,
                response,
                cancelled,
                go_authority_witness,
                execution_identity,
                plane_execution_seed,
                reservation,
            } => {
                let facts = request.facts();
                let staged = if cancelled.load(Ordering::Acquire) {
                    Err(PackageSemanticRuntimeError::Runtime(
                        facts.terminal(CompilerRuntimeCause::RequestCancelled),
                    ))
                } else {
                    catch_unwind(AssertUnwindSafe(|| {
                        let sources = borrow_package_sources(&request.sources)?;
                        let package = PackageSourceSet::new_for_unit(
                            &request.request,
                            &request.package_target,
                            &request.package_root,
                            &sources,
                        )?;
                        let package = match go_authority_witness.as_ref() {
                            Some(witness) => package.with_go_authority_witness(witness),
                            None => package,
                        };
                        let package = match request.input_claim {
                            Some(input) => package.with_input_claim(input),
                            None => package,
                        };
                        let package = match embedding_provisioning_failure {
                            Some(cause) => package.with_embedding_provisioning_failure(cause),
                            None => package,
                        };
                        execution
                            .stage_package_sources(
                                package,
                                execution_identity,
                                plane_execution_seed,
                                embedding_runtime.as_deref(),
                                embedding_requirement,
                                &mut scratch,
                                &cancelled,
                                &mut |_| {},
                            )
                            .map_err(PackageSemanticRuntimeError::from)
                    }))
                    .unwrap_or_else(|payload| {
                        Err(PackageSemanticRuntimeError::Runtime(facts.terminal(
                            CompilerRuntimeCause::WorkerPanic(
                                backend_library::interface::CompilerRuntimePanic::capture(
                                    payload.as_ref(),
                                ),
                            ),
                        )))
                    })
                };
                let panicked = matches!(
                    &staged,
                    Err(PackageSemanticRuntimeError::Runtime(
                        CompilerTerminal::Runtime {
                            cause: CompilerRuntimeCause::WorkerPanic(_),
                            ..
                        }
                    ))
                );
                let replacement = panicked.then(|| scratch.lane_scratch().ok()).flatten();
                let retire_lane = panicked && replacement.is_none();
                if let Some(replacement) = replacement {
                    scratch = replacement;
                }
                if completions
                    .send(LaneCompletion::PackageSources {
                        facts,
                        request,
                        staged,
                        response,
                        cancelled,
                        reservation,
                    })
                    .is_err()
                {
                    return;
                }
                if retire_lane {
                    return;
                }
            }
        }
    }
}

fn publish_lane_completion(
    compiler: &mut LocalCompiler<'_, '_, '_>,
    completion: LaneCompletion,
) -> bool {
    match completion {
        LaneCompletion::Compile {
            request,
            staged,
            response,
            cancelled,
            _reservation,
        } => {
            let facts = request.facts();
            let result = match staged {
                Err(terminal) => Err(terminal),
                Ok(staged) => match catch_unwind(AssertUnwindSafe(|| {
                    let generated =
                        compiler.publish_staged_single(staged, &cancelled, &mut |phase| {
                            if response.send(RuntimeEvent::Phase(phase)).is_err() {
                                cancelled.store(true, Ordering::Release);
                            }
                        })?;
                    let image = compiler
                        .semantic_image_snapshot(generated.semantic_image)
                        .map_err(|_| facts.terminal(CompilerRuntimeCause::ResponseOwnerStopped))?;
                    Ok((generated, image))
                })) {
                    Ok(result) => result,
                    Err(payload) => {
                        let cause = backend_library::interface::CompilerRuntimePanic::capture(
                            payload.as_ref(),
                        );
                        let _ = response.send(RuntimeEvent::Complete(Err(
                            facts.terminal(CompilerRuntimeCause::WorkerPanic(cause))
                        )));
                        return false;
                    }
                },
            };
            let _ = response.send(RuntimeEvent::Complete(result));
            true
        }
        LaneCompletion::PackageSources {
            facts,
            request: _,
            staged,
            response,
            cancelled,
            reservation,
        } => {
            match response {
                PackageCompileResponse::Published(response) => {
                    let result = match staged {
                        Err(error) => Err(error),
                        Ok(staged) => match catch_unwind(AssertUnwindSafe(|| {
                            compiler
                                .publish_staged_package(staged, &cancelled, &mut |_| {})
                                .map_err(PackageSemanticRuntimeError::from)
                        })) {
                            Ok(result) => result,
                            Err(payload) => {
                                let cause =
                                    backend_library::interface::CompilerRuntimePanic::capture(
                                        payload.as_ref(),
                                    );
                                let _ = response.send(Err(PackageSemanticRuntimeError::Runtime(
                                    facts.terminal(CompilerRuntimeCause::WorkerPanic(cause)),
                                )));
                                return false;
                            }
                        },
                    };
                    let _ = response.send(result);
                }
                PackageCompileResponse::Staged(response) => {
                    let result = match staged {
                        Err(error) => Err(error),
                        Ok(staged) => match catch_unwind(AssertUnwindSafe(|| {
                            compiler
                                .prepare_staged_package(staged, &cancelled)
                                .map_err(PackageSemanticRuntimeError::from)
                        })) {
                            Ok(result) => result,
                            Err(payload) => {
                                let cause =
                                    backend_library::interface::CompilerRuntimePanic::capture(
                                        payload.as_ref(),
                                    );
                                let _ = response.send(Err(PackageSemanticRuntimeError::Runtime(
                                    facts.terminal(CompilerRuntimeCause::WorkerPanic(cause)),
                                )));
                                return false;
                            }
                        },
                    };
                    match result {
                        Ok(mut staged) => {
                            staged.retain_budget_lease(reservation);
                            let _ = response.send(Ok(staged));
                        }
                        Err(error) => {
                            let _ = response.send(Err(error));
                        }
                    }
                }
            }
            true
        }
    }
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

#[cfg(test)]
mod input_witness_store_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn exact_witness_requires_the_same_inputs_lineage_and_selected_generation_after_restart() {
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "backend-exact-input-witness-store-{}-{sequence}",
            std::process::id()
        ));
        let package_root = directory.join("package");
        let cache_directory = directory.join("cache");
        std::fs::create_dir_all(&package_root).expect("create package root");
        let profile = LanguageProfile::PRODUCT_PROFILES[0];
        let lineage = CompilerSessionLineage([1; 32]);
        let witness = ExactInputWitness::from_digest([2; 32]);
        let binding = [3; 32];

        CompilerInputWitnessStore::new(cache_directory.clone())
            .remember(&package_root, profile, lineage, witness, binding)
            .expect("persist exact witness");

        // A fresh store instance models a compiler owner after a cold restart.
        let restarted = CompilerInputWitnessStore::new(cache_directory);
        assert!(restarted.is_current(&package_root, profile, lineage, witness, binding));
        assert!(!restarted.is_current(
            &package_root,
            profile,
            lineage,
            ExactInputWitness::from_digest([4; 32]),
            binding,
        ));
        assert!(!restarted.is_current(
            &package_root,
            profile,
            CompilerSessionLineage([5; 32]),
            witness,
            binding,
        ));
        assert!(!restarted.is_current(&package_root, profile, lineage, witness, [6; 32],));
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[cfg(test)]
mod portable_recipe_tests {
    use super::*;
    use backend_semantic::vocabulary::PythonVersion;

    fn python_recipe(
        interpreter: &Path,
        pyrefly: &Path,
        pyrefly_version: &[u8],
        arguments: Vec<String>,
    ) -> ContentId<CompileRecipeDomain> {
        let checker = Pyrefly::from_executable(pyrefly.to_path_buf())
            .expect("absolute Pyrefly executable")
            .with_arguments(arguments);
        let authority = LocalRuntimePackageAuthority {
            python: Some(checker),
            python_toolchain_identity: Some(PyreflyToolchainIdentity::from_version_output(
                pyrefly_version,
            )),
            ..LocalRuntimePackageAuthority::default()
        };
        let runtime = LocalRuntimeToolchain::resolved(
            NativeTool::Python,
            interpreter.to_path_buf(),
            b"same Python tool version",
        )
        .expect("absolute selected toolchain");
        let profile = LanguageProfile::Python(PythonVersion::Python314);
        let options = portable_invocation_options_digest(profile, &authority, Some(&runtime))
            .expect("adapter is bound to the selected toolchain");
        crate::application::CompilerInvocationRecipeV2::new(
            profile,
            Stage::LowerIr,
            NativeTool::Python,
            runtime.identity.expect("resolved toolchain identity"),
            compiler_environment_identity(profile),
            [8; 32],
            options,
        )
        .expect("portable invocation recipe")
        .identity()
    }

    #[test]
    fn python_recipe_uses_ordered_options_and_ignores_host_executable_path() {
        let first = python_recipe(
            Path::new("/host-a/bin/python"),
            Path::new("/host-a/bin/pyrefly"),
            b"pyrefly 0.1.0",
            vec!["check".to_owned(), "--strict".to_owned()],
        );
        let relocated = python_recipe(
            Path::new("/host-b/sdk/python"),
            Path::new("/host-b/sdk/pyrefly"),
            b"pyrefly 0.1.0",
            vec!["check".to_owned(), "--strict".to_owned()],
        );
        let reordered = python_recipe(
            Path::new("/host-b/sdk/python"),
            Path::new("/host-b/sdk/pyrefly"),
            b"pyrefly 0.1.0",
            vec!["--strict".to_owned(), "check".to_owned()],
        );
        let changed = python_recipe(
            Path::new("/host-b/sdk/python"),
            Path::new("/host-b/sdk/pyrefly"),
            b"pyrefly 0.1.0",
            vec![
                "check".to_owned(),
                "--strict".to_owned(),
                "--no-cache".to_owned(),
            ],
        );
        let changed_pyrefly = python_recipe(
            Path::new("/host-b/sdk/python"),
            Path::new("/host-b/sdk/pyrefly"),
            b"pyrefly 0.2.0",
            vec!["check".to_owned(), "--strict".to_owned()],
        );

        assert_eq!(first, relocated);
        assert_ne!(first, reordered);
        assert_ne!(first, changed);
        assert_ne!(first, changed_pyrefly);
    }

    #[test]
    fn portable_environment_identity_is_profile_specific_and_recipe_binds_real_inputs() {
        let profile = LanguageProfile::Python(PythonVersion::Python314);
        let baseline = compiler_environment_identity(profile);

        let baseline_recipe = crate::application::CompilerInvocationRecipeV2::new(
            profile,
            Stage::LowerIr,
            NativeTool::Python,
            ContentId::from_digest([1; 32]),
            baseline,
            [3; 32],
            [4; 32],
        )
        .expect("baseline recipe");
        let changed_toolchain = crate::application::CompilerInvocationRecipeV2::new(
            profile,
            Stage::LowerIr,
            NativeTool::Python,
            ContentId::from_digest([2; 32]),
            baseline,
            [3; 32],
            [4; 32],
        )
        .expect("toolchain-bound recipe");
        let changed_platform = crate::application::CompilerInvocationRecipeV2::new(
            profile,
            Stage::LowerIr,
            NativeTool::Python,
            ContentId::from_digest([1; 32]),
            baseline,
            [5; 32],
            [4; 32],
        )
        .expect("platform-bound recipe");
        let changed_profile =
            compiler_environment_identity(LanguageProfile::Python(PythonVersion::Python313));

        assert_ne!(baseline, changed_profile, "profile is a meaningful input");
        assert_ne!(baseline_recipe.identity(), changed_toolchain.identity());
        assert_ne!(baseline_recipe.identity(), changed_platform.identity());
    }
}

#[cfg(test)]
mod clang_native_authority_tests {
    use super::*;
    use backend_semantic::vocabulary::CStandard;

    fn authority_identity(driver: &Path, resource_dir: &Path) -> [u8; 32] {
        clang_authority_fingerprint(
            driver,
            resource_dir,
            Some(Path::new("/opt/clang/sysroot")),
            Path::new("/opt/clang/lib/libclang.so"),
            [Path::new("/opt/clang/include")],
        )
    }

    #[test]
    fn same_version_different_clang_paths_and_resources_have_distinct_local_scopes() {
        let profile = LanguageProfile::C(CStandard::C23);
        let toolchain = ContentId::from_canonical_bytes(b"clang version 20.1.0");
        let environment = compiler_environment_identity(profile);
        let platform = [9; 32];

        let first_authority = authority_identity(
            Path::new("/nix/store/aaa-clang/bin/clang"),
            Path::new("/nix/store/aaa-clang/lib/clang/20"),
        );
        let same_authority = authority_identity(
            Path::new("/nix/store/aaa-clang/bin/clang"),
            Path::new("/nix/store/aaa-clang/lib/clang/20"),
        );
        let relocated_authority = authority_identity(
            Path::new("/nix/store/bbb-clang/bin/clang"),
            Path::new("/nix/store/bbb-clang/lib/clang/20"),
        );

        assert_eq!(first_authority, same_authority);
        assert_ne!(first_authority, relocated_authority);
        assert_ne!(
            semantic_lineage(
                profile,
                NativeTool::Clang,
                toolchain,
                first_authority,
                environment,
                platform,
            ),
            semantic_lineage(
                profile,
                NativeTool::Clang,
                toolchain,
                relocated_authority,
                environment,
                platform,
            ),
            "the same version bytes cannot collapse distinct Clang closures into one scope"
        );
        assert_eq!(
            portable_invocation_options_digest(
                profile,
                &LocalRuntimePackageAuthority::default(),
                None,
            ),
            None,
            "clang-sys does not attest its in-process loaded library, so no remote grant is issued"
        );
        assert_eq!(
            package_authority_fingerprint(profile, &LocalRuntimePackageAuthority::default(), None,),
            None,
            "a missing explicit driver/libclang witness makes Clang locally unavailable"
        );
    }
}

#[cfg(test)]
mod go_workspace_identity_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_GO_WORK_TEST: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn go_package_authority_witness_changes_local_identity_and_placement() {
        let sequence = NEXT_GO_WORK_TEST.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-go-work-witness-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create package root");
        // The witness records canonical paths; the temporary directory is a
        // symlink on macOS (`/var` -> `/private/var`).
        let root = root.canonicalize().expect("canonical package root");
        assert!(!go_authority_witness_is_portable(Language::Go, None));
        assert!(!go_authority_witness_allows_semantic_reuse(
            Language::Go,
            None
        ));

        let without_workspace = GoPackageAuthorityWitness::capture(&root)
            .expect("capture package without Go manifests");
        assert!(go_authority_witness_is_portable(
            Language::Go,
            Some(&without_workspace)
        ));
        let absent_identity = package_go_authority_fingerprint([7; 32], &without_workspace);

        let module = root.join("module");
        std::fs::create_dir_all(&module).expect("create workspace module");
        std::fs::write(
            module.join("go.mod"),
            b"module example.test/module\ngo 1.24\n",
        )
        .expect("write module manifest");
        std::fs::write(module.join("module.go"), b"package module\n").expect("write module source");
        let workspace_path = root.join("go.work");
        std::fs::write(&workspace_path, b"go 1.24\nuse ./module\n").expect("write workspace file");
        let workspace =
            GoPackageAuthorityWitness::capture(&root).expect("capture workspace authority inputs");
        assert!(!go_authority_witness_is_portable(
            Language::Go,
            Some(&workspace)
        ));
        assert!(
            workspace.is_complete(),
            "all bounded workspace inputs were captured"
        );
        assert!(go_authority_witness_allows_semantic_reuse(
            Language::Go,
            Some(&workspace)
        ));
        assert_eq!(
            workspace.go_work_witness().path(),
            Some(workspace_path.as_path())
        );
        assert_ne!(
            absent_identity,
            package_go_authority_fingerprint([7; 32], &workspace),
            "a selected workspace and local module tree change package-local authority identity"
        );
        assert!(
            workspace
                .matches_current(&root)
                .expect("revalidate unchanged workspace")
        );

        std::fs::write(
            &workspace_path,
            b"go 1.24\nuse ./module\nreplace example.test/x => ./other\n",
        )
        .expect("change workspace file");
        assert!(
            !workspace
                .matches_current(&root)
                .expect("revalidate changed workspace")
        );
        let incomplete = GoPackageAuthorityWitness::capture(&root)
            .expect("capture missing local replacement target");
        assert!(!incomplete.is_complete());
        assert!(incomplete.requires_local_execution());
        assert!(
            !go_authority_witness_allows_semantic_reuse(Language::Go, Some(&incomplete)),
            "an incomplete local tree must not receive a reusable plane identity"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod local_plane_identity_tests {
    use super::*;
    use backend_semantic::ir::{
        SemanticBuildIdentity, SemanticIrPlane, SemanticPlane, SemanticPlaneKind,
        SemanticPlaneManifest, SemanticPlaneSegment,
    };
    use backend_semantic::vocabulary::CStandard;
    use backend_version::ScopeRoot;

    fn local_identity(
        environment_identity: [u8; 32],
        input_root: [u8; 32],
        read_manifest: [u8; 32],
    ) -> LocalCompilerPlaneExecutionIdentity {
        let profile = LanguageProfile::C(CStandard::C23);
        let input = SemanticInputWitness::claimed_state(
            input_root,
            ScopeRoot::from_bytes(read_manifest),
            Coverage::Partial,
        );
        LocalCompilerPlaneExecutionSeed {
            target: ContentId::from_digest([1; 32]),
            profile,
            stage: Stage::LowerIr,
            toolchain: NativeTool::Clang,
            toolchain_identity: [2; 32],
            local_authority_fingerprint: [3; 32],
            environment_identity,
            target_platform_identity: [4; 32],
            capability_identity: [5; 32],
        }
        .bind_input(input)
    }

    fn local_manifest_root(identity: LocalCompilerPlaneExecutionIdentity) -> [u8; 32] {
        let input = identity.input_witness();
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let segment = SemanticPlaneSegment::from_payload_with_witness(
            kind,
            [0; 32],
            [0; 32],
            1,
            b"same local output bytes",
            input,
        )
        .expect("exact nonempty segment");
        let plane = SemanticPlane::claimed(kind, vec![segment], Coverage::Partial)
            .expect("claimed partial plane");
        let build = SemanticBuildIdentity::new(
            [6; 32],
            *identity.target().as_ref(),
            identity.profile(),
            identity.stage(),
            identity.recipe_identity().as_bytes(),
            identity.toolchain_identity(),
            identity.environment_identity(),
            identity.target_platform_identity(),
        );
        SemanticPlaneManifest::new(
            backend_semantic::ir::GenerationId::from_canonical_bytes(b"same local output bytes"),
            build,
            input,
            vec![plane],
        )
        .expect("manifest binds the local execution and partial input claim")
        .root()
        .as_bytes()
        .to_owned()
    }

    #[test]
    fn local_plane_identity_is_host_scoped_input_bound_and_disjoint_from_portable_recipe() {
        let first = local_identity([7; 32], [8; 32], [9; 32]);
        let changed_environment = local_identity([17; 32], [8; 32], [9; 32]);
        let changed_read_set = local_identity([7; 32], [18; 32], [9; 32]);

        assert_ne!(first, changed_environment);
        assert_ne!(
            first.recipe_identity(),
            changed_environment.recipe_identity()
        );
        assert_ne!(
            local_manifest_root(first),
            local_manifest_root(changed_environment),
            "a changed child environment recipe changes the canonical local plane root"
        );
        assert_ne!(first, changed_read_set);
        assert_ne!(
            local_manifest_root(first),
            local_manifest_root(changed_read_set),
            "a changed exact input/read-set claim changes the canonical local plane root"
        );
        assert_eq!(
            first.input_witness().coverage().state(),
            Coverage::Partial,
            "local identity must preserve the input authority's partial state"
        );

        let portable = crate::application::CompilerInvocationRecipeV2::new(
            first.profile(),
            first.stage(),
            first.toolchain(),
            ContentId::from_digest(first.toolchain_identity()),
            first.environment_identity(),
            first.target_platform_identity(),
            [19; 32],
        )
        .expect("portable recipe fields are valid")
        .identity();
        assert_ne!(
            first.recipe_identity().as_bytes(),
            *portable.as_ref(),
            "host-local recipe IDs are not portable worker recipe IDs"
        );
    }
}
