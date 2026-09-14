//! The `backend-engine` application module binds application requests to native compilation and durable publication.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Concrete, explicitly configured local compiler and durable-publication capability.

mod compiler;
mod config;
mod documentation;
mod host;
mod package_authority;
mod package_source;
mod runtime;
mod terminal;
mod toolchain_probe;

pub use self::compiler::{
    ActivatedSemanticPackage, LocalCompiler, PackageSemanticError, PackageSource, PackageSourceSet,
    PackageSourceSetError, PublishedSemanticPackage,
};
pub use self::config::{
    LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch, LocalCompilerScratchError,
    LocalCompilerTimeout, LocalCompilerTimeoutError, LocalPackageRoot, LocalPackageRootError,
    LocalPackageRootFacts, LocalPackageRootSet, LocalPackageRootSetError, LocalToolchainSet,
    LocalToolchainSetError, MAX_FRAGMENT_OUTPUT_BYTES, MAX_LOCAL_COMPILER_TIMEOUT,
    MAX_LOCAL_PACKAGE_ROOTS, MAX_LOCAL_TOOLCHAINS, MAX_LOCALITY_OUTPUT_BYTES, MAX_MANIFEST_ENTRIES,
    MAX_MANIFEST_OUTPUT_BYTES,
};
pub use self::documentation::{
    CanonicalDocumentationEntities, DocumentationEntity, DocumentationEntityView,
    DocumentationFragment, DocumentationFragments, DocumentationMembers,
    DocumentationProjectionError, DocumentationReference, DocumentationRelation,
    DocumentationRelations, DocumentationSession, DocumentationSessionView, DocumentationTarget,
    DocumentationTextPart, DocumentationType, DocumentationTypeView,
};
pub use self::host::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostDirectory, LocalHostDiscovery,
    LocalHostEnvironment, LocalHostPathKind, LocalHostPathRole, LocalHostVariable,
    ProcessHostEnvironment, WorkspaceCompilerEnvironment,
};
pub use self::package_authority::{
    CSharpPackageAuthorityConfiguration, JavaPackageAuthorityConfiguration,
    PackageAuthorityConfiguration, PackageAuthorityError, PackageAuthorityOwner,
    PackageAuthorityRequest, PackageAuthorityStage, RustPackageAuthorityConfiguration,
    enter_package_authority,
};
pub use self::package_source::MAX_LOCAL_PACKAGE_SOURCE_BYTES;
pub use self::runtime::{
    LocalCompilerCapabilities, LocalCompilerCapability, LocalCompilerCapabilityState,
    LocalCompilerClient, LocalCompilerRuntimeConfiguration, LocalCompilerRuntimeConfigurationError,
    LocalCompilerRuntimeOpenError, LocalCompilerRuntimePaths, LocalRuntimeCSharpAuthority,
    LocalRuntimeJavaAuthority, LocalRuntimePackageAuthority, LocalRuntimePackageRoot,
    LocalRuntimePackageRootFacts, LocalRuntimeRustAuthority, LocalRuntimeToolchain,
    LocalRuntimeToolchainFacts, LocalRuntimeToolchainState, OwnedPackageSource,
    OwnedPackageSourceSet, PackageSemanticRuntimeError,
};
pub use self::terminal::{LocalCompilerOpenError, LocalCompilerPath};
pub use self::toolchain_probe::{
    ToolchainProbeCleanupAction, ToolchainProbeError, ToolchainProbeLimitError,
    ToolchainProbeLimits, ToolchainProbeLimitsView, ToolchainProbePrimary,
    ToolchainProbeStreamError,
};
