//! Defines manifest behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the manifest invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Canonical compiler package manifest construction, grammar, and validated borrowed views.

mod build;
mod validate;
mod wire;

#[cfg(test)]
mod tests;

pub use build::{
    COMPILATION_MANIFEST_ENTRY_BYTES, COMPILATION_MANIFEST_HEADER_BYTES,
    COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES, CompilationPrepareError, CompilationWriteError,
    PublicationFragmentError, SemanticImageRegion,
};
pub use validate::{
    CompilationManifestEntries, CompilationManifestError, CompilationManifestFacts,
    CompilationManifestFormat, CompilationManifestView, StoredFragmentFacts,
};

pub(crate) use build::{CanonicalCompilation, CanonicalSemanticCompilation};

/// Typed identity of one complete canonical compiler package manifest.
pub type CompilationManifestIdentity = heart_identity::ArtifactId<
    heart_identity::IrManifestEncoding,
    heart_identity::IrManifestDomain,
>;
