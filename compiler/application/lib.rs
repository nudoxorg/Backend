//! The `compiler-application` crate exists to bind application requests to native compilation and durable publication.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Concrete, explicitly configured local compiler and durable-publication capability.

mod compiler;
mod config;
mod documentation;
mod terminal;

pub use compiler::LocalCompiler;
pub use config::{
    LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch, LocalCompilerTimeout,
    LocalCompilerTimeoutError, LocalToolchainSet, LocalToolchainSetError,
    MAX_FRAGMENT_OUTPUT_BYTES, MAX_LOCAL_COMPILER_TIMEOUT, MAX_LOCAL_TOOLCHAINS,
    MAX_LOCALITY_OUTPUT_BYTES, MAX_MANIFEST_ENTRIES, MAX_MANIFEST_OUTPUT_BYTES,
};
pub use documentation::{
    CanonicalDocumentationEntities, DocumentationEntity, DocumentationEntityView,
    DocumentationFragment, DocumentationFragments, DocumentationMembers,
    DocumentationProjectionError, DocumentationReference, DocumentationRelation,
    DocumentationRelations, DocumentationSession, DocumentationSessionView, DocumentationTarget,
    DocumentationTextPart, DocumentationType, DocumentationTypeView,
};
pub use terminal::{LocalCompilerOpenError, LocalCompilerPath};
