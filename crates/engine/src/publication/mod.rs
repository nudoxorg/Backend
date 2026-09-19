//! The `backend-engine` publication module publishes verified compiler fragments as immutable generations.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Filesystem adapters for durable compiler publication artifacts.
#![deny(unsafe_code)]
#![warn(missing_docs)]

/// Canonical generation-to-manifest binding artifacts.
pub mod binding;
mod binding_store;
pub mod immutable;
/// Canonical recipe-bearing package manifest construction and validation.
pub mod manifest;
/// Immutable storage for validated canonical package manifests.
pub mod manifest_store;
/// Durable publication from `driver` outputs only.
pub mod publication;
/// Immutable storage for complete validated semantic images.
pub mod semantic_immutable;

mod generation;
mod storage;

/// Exact immutable generation-binding storage failures surfaced by publication and reopen.
pub use self::binding_store::{BindingIoPhase, BindingStoreError};
/// Exact construction failures while rebuilding one complete compiler generation closure.
pub use self::generation::GenerationBuildError;
/// Durable compiler publication and verified reopen public boundary.
pub use self::publication::{
    OpenPublicationScratch, OpenPublishedError, OpenSemanticPublicationScratch, OpenedCompilation,
    OpenedFragment, OpenedFragmentCursor, OpenedFragmentError, OpenedFragmentFactMismatch,
    OpenedFragmentView, OpenedSemanticArtifact, OpenedSemanticArtifactCursor,
    OpenedSemanticArtifactError, OpenedSemanticCompilation, OpenedSemanticGeneration,
    PublicationScratch, PublishCompiledError, PublishControl, PublishSemanticError,
    PublishedCompilation, SemanticGenerationRequirements, SemanticPublicationScratch,
    UncommittedPublication, UncommittedPublicationFacts, open_published, open_published_semantic,
    open_semantic_generation, publish_compiled, publish_semantic, semantic_generation_requirements,
};
pub use self::semantic_immutable::{
    ImmutableSemanticImageError, ImmutableSemanticImageStore, SemanticImageArtifactFacts,
    StoredSemanticImage,
};
pub use self::storage::ImmutableFileError;
