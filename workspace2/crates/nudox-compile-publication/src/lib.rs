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

mod generation;
mod storage;

/// Exact immutable generation-binding storage failures surfaced by publication and reopen.
pub use binding_store::{BindingIoPhase, BindingStoreError};
/// Exact construction failures while rebuilding one complete compiler generation closure.
pub use generation::GenerationBuildError;
/// Durable compiler publication and verified reopen public boundary.
pub use publication::{
    OpenPublicationScratch, OpenPublishedError, OpenedCompilation, OpenedFragment,
    OpenedFragmentCursor, OpenedFragmentError, OpenedFragmentFactMismatch, OpenedFragmentView,
    PublicationScratch, PublishCompiledError, PublishControl, PublishedCompilation,
    UncommittedPublication, UncommittedPublicationFacts, open_published, publish_compiled,
};
pub use storage::ImmutableFileError;
