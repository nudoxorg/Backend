//! The `compiler-application` crate exists to bind application requests to native compilation and durable publication.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Concrete, explicitly configured local compiler and durable-publication capability.

mod compiler;
mod config;
mod documentation;
mod package_authority;
mod package_source;
mod runtime;
mod terminal;

pub use compiler::LocalCompiler;
pub use config::{
    LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch, LocalCompilerScratchError,
    LocalCompilerTimeout, LocalCompilerTimeoutError, LocalPackageRoot, LocalPackageRootError, LocalPackageRootFacts,
    LocalPackageRootSet, LocalPackageRootSetError, LocalToolchainSet, LocalToolchainSetError,
    MAX_FRAGMENT_OUTPUT_BYTES, MAX_LOCAL_COMPILER_TIMEOUT, MAX_LOCAL_PACKAGE_ROOTS,
    MAX_LOCAL_TOOLCHAINS, MAX_LOCALITY_OUTPUT_BYTES, MAX_MANIFEST_ENTRIES,
    MAX_MANIFEST_OUTPUT_BYTES,
};
pub use documentation::{
    CanonicalDocumentationEntities, DocumentationEntity, DocumentationEntityView,
    DocumentationFragment, DocumentationFragments, DocumentationMembers,
    DocumentationProjectionError, DocumentationReference, DocumentationRelation,
    DocumentationRelations, DocumentationSession, DocumentationSessionView, DocumentationTarget,
    DocumentationTextPart, DocumentationType, DocumentationTypeView,
};
pub use package_authority::{
    JavaPackageAuthorityConfiguration, PackageAuthorityConfiguration, PackageAuthorityError,
    PackageAuthorityOwner, PackageAuthorityRequest, PackageAuthorityStage,
    RustPackageAuthorityConfiguration, enter_package_authority,
};
pub use package_source::MAX_LOCAL_PACKAGE_SOURCE_BYTES;
pub use runtime::{
    LocalCompilerClient, LocalCompilerRuntimeConfiguration,
    LocalCompilerRuntimeConfigurationError, LocalCompilerRuntimeOpenError,
    LocalCompilerRuntimePaths, LocalRuntimeJavaAuthority, LocalRuntimePackageAuthority,
    LocalRuntimePackageRoot, LocalRuntimePackageRootFacts, LocalRuntimeRustAuthority,
    LocalRuntimeToolchain, LocalRuntimeToolchainFacts,
};
pub use terminal::{LocalCompilerOpenError, LocalCompilerPath};
