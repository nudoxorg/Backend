//! Defines binding behavior for the `backend-engine` publication, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the binding invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Non-circular immutable bindings from a verified generation to an IR manifest.

use core::ops::Deref;

use backend_store::hydration::VerifiedGenerationFacts;
use backend_version::{
    ArtifactId, ArtifactIdDecodeError, CompilePublicationDomain, CompilePublicationEncoding,
    ContentId, ContentIdDecodeError, DependencySetDomain, GenerationId,
};
use thiserror::Error;

use crate::publication::manifest::CompilationManifestIdentity;

/// Typed identity of an immutable compiler generation-to-manifest binding artifact.
pub type CompilationBindingIdentity =
    ArtifactId<CompilePublicationEncoding, CompilePublicationDomain>;

/// Exact fixed byte width of a generation-to-manifest binding artifact.
pub const COMPILATION_BINDING_BYTES: usize = 108;

const MAGIC: [u8; 8] = *b"NUDXCPB\0";
const VERSION: u16 = 1;
const ROOT_OFFSET: usize = 12;
const DEP_SET_OFFSET: usize = ROOT_OFFSET + 32;
const MANIFEST_OFFSET: usize = DEP_SET_OFFSET + 32;

/// Immutable facts of a validated generation-to-manifest binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilationBindingFacts {
    /// Typed identity of the complete canonical binding bytes.
    pub identity: CompilationBindingIdentity,
    /// Exact verified generation facts selected by the durable journal.
    pub generation: VerifiedGenerationFacts,
    /// Typed complete compiler package manifest identity referenced by that generation.
    pub manifest: CompilationManifestIdentity,
}

/// Borrowed validated immutable binding bytes.
pub struct CompilationBindingView<'binding> {
    bytes: &'binding [u8],
    facts: CompilationBindingFacts,
}

impl<'binding> CompilationBindingView<'binding> {
    /// Writes and validates one canonical non-circular binding into caller-owned output.
    pub fn write_into(
        generation: VerifiedGenerationFacts,
        manifest: CompilationManifestIdentity,
        output: &'binding mut [u8],
    ) -> Result<Self, CompilationBindingWriteError> {
        if output.len() < COMPILATION_BINDING_BYTES {
            return Err(CompilationBindingWriteError::OutputTooSmall {
                required: COMPILATION_BINDING_BYTES,
                available: output.len(),
            });
        }
        let output = &mut output[..COMPILATION_BINDING_BYTES];
        output[..8].copy_from_slice(&MAGIC);
        output[8..10].copy_from_slice(&VERSION.to_le_bytes());
        output[10..12].copy_from_slice(&0_u16.to_le_bytes());
        output[ROOT_OFFSET..DEP_SET_OFFSET].copy_from_slice(generation.pinned_root.as_ref());
        output[DEP_SET_OFFSET..MANIFEST_OFFSET].copy_from_slice(generation.dep_set.as_ref());
        output[MANIFEST_OFFSET..].copy_from_slice(manifest.as_ref());
        Self::validate(output).map_err(CompilationBindingWriteError::FreshValidation)
    }

    /// Validates a complete non-circular generation-to-manifest binding artifact.
    pub fn validate(bytes: &'binding [u8]) -> Result<Self, CompilationBindingError> {
        if bytes.len() != COMPILATION_BINDING_BYTES {
            return Err(CompilationBindingError::Length {
                expected: COMPILATION_BINDING_BYTES,
                observed: bytes.len(),
            });
        }
        let observed_magic = fixed::<8>(bytes, 0);
        if observed_magic != MAGIC {
            return Err(CompilationBindingError::Magic {
                observed: observed_magic,
            });
        }
        let observed_version = u16::from_le_bytes(fixed::<2>(bytes, 8));
        if observed_version != VERSION {
            return Err(CompilationBindingError::Version {
                observed: observed_version,
            });
        }
        let reserved = u16::from_le_bytes(fixed::<2>(bytes, 10));
        if reserved != 0 {
            return Err(CompilationBindingError::Reserved { observed: reserved });
        }
        let pinned_root = GenerationId::try_from(&bytes[ROOT_OFFSET..DEP_SET_OFFSET])
            .map_err(CompilationBindingError::PinnedRoot)?;
        let dep_set =
            ContentId::<DependencySetDomain>::try_from(&bytes[DEP_SET_OFFSET..MANIFEST_OFFSET])
                .map_err(CompilationBindingError::DependencySet)?;
        let manifest = CompilationManifestIdentity::try_from(&bytes[MANIFEST_OFFSET..])
            .map_err(CompilationBindingError::Manifest)?;
        Ok(Self {
            bytes,
            facts: CompilationBindingFacts {
                identity: CompilationBindingIdentity::from_encoded_bytes(bytes),
                generation: VerifiedGenerationFacts {
                    pinned_root,
                    dep_set,
                },
                manifest,
            },
        })
    }
}

impl Deref for CompilationBindingView<'_> {
    type Target = CompilationBindingFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl AsRef<[u8]> for CompilationBindingView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.bytes
    }
}

/// Failure while writing a canonical generation-to-manifest binding artifact.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CompilationBindingWriteError {
    /// Caller output cannot hold the complete fixed binding artifact.
    #[error("binding output has {available} bytes, requires {required}")]
    OutputTooSmall {
        /// Exact fixed binding width.
        required: usize,
        /// Caller-provided output width.
        available: usize,
    },
    /// Fresh canonical bytes did not pass the public persisted-byte validator.
    #[error("fresh compiler generation binding failed independent validation")]
    FreshValidation(#[source] CompilationBindingError),
}

/// Rejection while decoding a persisted generation-to-manifest binding artifact.
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(
    missing_docs,
    reason = "each field repeats the exact fact documented by its enclosing terminal"
)]
pub enum CompilationBindingError {
    /// Binding bytes did not have the complete fixed width.
    #[error("compiler generation binding has {observed} bytes, expected {expected}")]
    Length { expected: usize, observed: usize },
    /// Fixed binding magic differed from this format.
    #[error("compiler generation binding magic is invalid")]
    Magic { observed: [u8; 8] },
    /// Fixed binding version is unsupported.
    #[error("compiler generation binding version {observed} is unsupported")]
    Version { observed: u16 },
    /// Fixed binding reserved bits were nonzero.
    #[error("compiler generation binding reserved bits are nonzero: {observed}")]
    Reserved { observed: u16 },
    /// Persisted root bytes lacked generation-root authority.
    #[error("compiler generation binding has an invalid pinned root")]
    PinnedRoot(#[source] ContentIdDecodeError),
    /// Persisted dependency-set bytes lacked dependency-set authority.
    #[error("compiler generation binding has an invalid dependency set")]
    DependencySet(#[source] ContentIdDecodeError),
    /// Persisted manifest bytes lacked the IR-manifest artifact authority.
    #[error("compiler generation binding has an invalid compiler manifest identity")]
    Manifest(#[source] ArtifactIdDecodeError),
}

fn fixed<const WIDTH: usize>(input: &[u8], offset: usize) -> [u8; WIDTH] {
    let mut output = [0; WIDTH];
    output.copy_from_slice(&input[offset..offset + WIDTH]);
    output
}

#[cfg(test)]
mod tests {
    use backend_store::hydration::VerifiedGenerationFacts;
    use backend_version::{
        ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, ContentId,
        DependencySetDomain, GenerationId, IrManifestDomain, IrManifestEncoding,
    };
    use thiserror::Error;

    use super::{COMPILATION_BINDING_BYTES, CompilationBindingError, CompilationBindingView};

    #[derive(Debug, Error)]
    enum TestError {
        #[error(transparent)]
        Write(#[from] super::CompilationBindingWriteError),
    }

    #[test]
    fn binding_is_canonical_and_uses_the_compilation_publication_identity() -> Result<(), TestError>
    {
        let generation = VerifiedGenerationFacts {
            pinned_root: GenerationId::from_canonical_bytes(b"publication-root"),
            dep_set: ContentId::<DependencySetDomain>::from_canonical_bytes(b"publication-deps"),
        };
        let manifest = ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
            b"publication-manifest",
        );
        let mut first = [0_u8; COMPILATION_BINDING_BYTES];
        let first = CompilationBindingView::write_into(generation, manifest, &mut first)?;
        requires_compilation_binding_identity(first.identity);
        let mut second = [0_u8; COMPILATION_BINDING_BYTES];
        let second = CompilationBindingView::write_into(generation, manifest, &mut second)?;
        assert_eq!(first.as_ref(), second.as_ref());
        assert_eq!(first.identity, second.identity);
        assert_eq!(first.generation, generation);
        assert_eq!(first.manifest, manifest);
        Ok(())
    }

    #[test]
    fn binding_rejects_an_ir_manifest_identity_with_the_wrong_authority() -> Result<(), TestError> {
        let generation = VerifiedGenerationFacts {
            pinned_root: GenerationId::from_canonical_bytes(b"publication-root"),
            dep_set: ContentId::<DependencySetDomain>::from_canonical_bytes(b"publication-deps"),
        };
        let manifest = ArtifactId::<IrManifestEncoding, IrManifestDomain>::from_encoded_bytes(
            b"publication-manifest",
        );
        let mut bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let binding = CompilationBindingView::write_into(generation, manifest, &mut bytes)?;
        let mut mutant = [0_u8; COMPILATION_BINDING_BYTES];
        mutant.copy_from_slice(binding.as_ref());
        mutant[76] = 0;
        assert!(matches!(
            CompilationBindingView::validate(&mutant),
            Err(CompilationBindingError::Manifest(_))
        ));
        Ok(())
    }

    fn requires_compilation_binding_identity(
        _: ArtifactId<CompilePublicationEncoding, CompilePublicationDomain>,
    ) {
    }
}
