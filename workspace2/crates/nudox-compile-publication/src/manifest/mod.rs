//! Canonical compiler package manifest construction, grammar, and validated borrowed views.

mod build;
mod validate;
mod wire;

#[cfg(test)]
mod tests;

pub use build::{
    COMPILATION_MANIFEST_ENTRY_BYTES, COMPILATION_MANIFEST_HEADER_BYTES, CompilationPrepareError,
    CompilationWriteError, PublicationFragmentError,
};
pub use validate::{
    CompilationManifestEntries, CompilationManifestError, CompilationManifestFacts,
    CompilationManifestView, StoredFragmentFacts,
};

pub(crate) use build::CanonicalCompilation;

/// Typed identity of one complete canonical compiler package manifest.
pub type CompilationManifestIdentity =
    nudox_id::ArtifactId<nudox_id::IrManifestEncoding, nudox_id::IrManifestDomain>;
