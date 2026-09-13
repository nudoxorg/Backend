//! The `compiler-publication` crate exists to publish verified compiler fragments as immutable generations.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Filesystem adapters for durable compiler publication artifacts.

/// Canonical generation-to-manifest binding artifacts.
pub mod binding;
mod binding_store;
pub mod immutable;
/// Canonical recipe-bearing package manifest construction and validation.
pub mod manifest;
/// Immutable storage for validated canonical package manifests.
pub mod manifest_store;
/// Durable publication from compiler-driver outputs only.
pub mod publication;
/// Immutable storage for complete validated semantic images.
pub mod semantic_immutable;

mod generation;
mod storage;

/// Exact immutable generation-binding storage failures surfaced by publication and reopen.
pub use binding_store::{BindingIoPhase, BindingStoreError};
/// Exact construction failures while rebuilding one complete compiler generation closure.
pub use generation::GenerationBuildError;
/// Durable compiler publication and verified reopen public boundary.
pub use publication::{
    OpenPublicationScratch, OpenPublishedError, OpenSemanticPublicationScratch, OpenedCompilation,
    OpenedFragment, OpenedFragmentCursor, OpenedFragmentError, OpenedFragmentFactMismatch,
    OpenedFragmentView, OpenedSemanticArtifact, OpenedSemanticArtifactCursor,
    OpenedSemanticArtifactError, OpenedSemanticCompilation, OpenedSemanticGeneration,
    PublicationScratch, PublishCompiledError, PublishControl, PublishSemanticError,
    PublishedCompilation, SemanticGenerationRequirements, SemanticPublicationScratch,
    UncommittedPublication, UncommittedPublicationFacts, open_published, open_published_semantic,
    open_semantic_generation, publish_compiled, publish_semantic, semantic_generation_requirements,
};
pub use semantic_immutable::{
    ImmutableSemanticImageError, ImmutableSemanticImageStore, SemanticImageArtifactFacts,
    StoredSemanticImage,
};
pub use storage::ImmutableFileError;
