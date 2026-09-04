//! Sealed marker traits and fixed-width tags for every canonical identity namespace.
//! The registry below is the sole authority for durable domain and encoding discriminants.
//! Deref exposes exact tag bytes without adding getters or permitting caller-defined namespaces.
use core::ops::Deref;

use crate::TAG_BYTES;

/// Exact fixed-width label that names one content-identity namespace.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DomainTag([u8; TAG_BYTES]);

impl DomainTag {
    /// Builds a protocol label from its exact 16-byte durable representation.
    #[must_use]
    pub(crate) const fn new(bytes: [u8; TAG_BYTES]) -> Self {
        Self(bytes)
    }
}

impl Deref for DomainTag {
    type Target = [u8; TAG_BYTES];

    /// Borrows the complete fixed-width domain label.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<[u8; TAG_BYTES]> for DomainTag {
    /// Borrows the domain label for hashing and canonical encoding.
    fn as_ref(&self) -> &[u8; TAG_BYTES] {
        self
    }
}

/// Exact fixed-width label that names one artifact encoding namespace.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EncodingTag([u8; TAG_BYTES]);

impl EncodingTag {
    /// Builds a protocol label from its exact 16-byte durable representation.
    #[must_use]
    pub(crate) const fn new(bytes: [u8; TAG_BYTES]) -> Self {
        Self(bytes)
    }
}

impl Deref for EncodingTag {
    type Target = [u8; TAG_BYTES];

    /// Borrows the complete fixed-width encoding label.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<[u8; TAG_BYTES]> for EncodingTag {
    /// Borrows the encoding label for hashing and canonical encoding.
    fn as_ref(&self) -> &[u8; TAG_BYTES] {
        self
    }
}

mod sealed {
    /// Prevents downstream crates from creating unregistered identity domains.
    pub trait Domain {}
    /// Prevents downstream crates from creating unregistered artifact encodings.
    pub trait Encoding {}
}

/// Supplies one precise immutable label from the finite foundation registry.
///
/// This trait is sealed: application crates select a centrally registered marker rather than
/// defining raw application labels or an accidental colliding identity namespace.
pub trait Domain: sealed::Domain + core::fmt::Debug {
    /// Durable identity-domain label included in each content preimage.
    const TAG: DomainTag;
    /// Closed one-byte authority cell stored in serialized content identities.
    const CODE: DomainCode;
}

/// Supplies one precise immutable encoded-artifact label from the finite registry.
pub trait Encoding: sealed::Encoding {
    /// Durable representation label included in each artifact preimage.
    const TAG: EncodingTag;
    /// Closed one-byte authority cell stored in serialized artifact identities.
    const CODE: EncodingCode;
}

include!("marker_registry.rs");

protocol_registry!(
    domains {
        (ObjectDomain, Object, 1, b"heart.object.v1\0"),
        (RootDomain, Root, 2, b"heart.root.v1\0\0\0"),
        (OperationDomain, Operation, 3, b"heart.op.v1\0\0\0\0\0"),
        (DependencySetDomain, DependencySet, 4, b"heart.depset.v1\0"),
        (CapabilityDomain, Capability, 5, b"heart.cap.v1\0\0\0\0"),
        (ConfigurationDomain, Configuration, 6, b"heart.config.v1\0"),
        (StageKeyDomain, StageKey, 7, b"heart.stage.v1\0\0"),
        (IndexSnapshotDomain, IndexSnapshot, 8, b"heart.idx.snap.1"),
        (IndexExactSegmentDomain, IndexExactSegment, 9, b"heart.idx.exact1"),
        (IndexLexicalSegmentDomain, IndexLexicalSegment, 10, b"heart.idx.lexic1"),
        (IndexVectorSegmentDomain, IndexVectorSegment, 11, b"heart.idx.vectr1"),
        (IrFragmentDomain, IrFragment, 12, b"heart.irfrag.v1\0"),
        (IrManifestDomain, IrManifest, 13, b"heart.irmani.v1\0"),
        (SourceFactDomain, SourceFact, 14, b"heart.source.v1\0"),
        (ToolchainDomain, Toolchain, 15, b"heart.tool.v1\0\0\0"),
        (CompilationTargetDomain, CompilationTarget, 16, b"heart.target.v1\0"),
        (CompileRecipeDomain, CompileRecipe, 17, b"heart.recipe.v1\0"),
        (CompilePublicationDomain, CompilePublication, 18, b"heart.publish.v1"),
        (IndexPackDomain, IndexPack, 19, b"heart.idx.pack.1"),
        (DeclarationKeyDomain, DeclarationKey, 20, b"heart.declkey.v1"),
        (DeclarationFamilyDomain, DeclarationFamily, 21, b"heart.declfam.v1"),
        (DeclarationVariantDomain, DeclarationVariant, 22, b"heart.declvar.v1"),
        (ForeignDeclarationDomain, ForeignDeclaration, 23, b"heart.foreign.v1"),
        (SemanticScopeDomain, SemanticScope, 24, b"heart.scopeid.v1")
    }
    encodings {
        (FrameEncoding, Frame, 1, b"heart.frame.v1\0\0"),
        (LocalitySortedEncoding, LocalitySorted, 2, b"heart.locsort.v1"),
        (ObjectPackEncoding, ObjectPack, 3, b"heart.objpack.v1"),
        (IrFragmentEncoding, IrFragment, 4, b"heart.irfrag.w1\0"),
        (IrManifestEncoding, IrManifest, 5, b"heart.irmani.w1\0"),
        (CompilePublicationEncoding, CompilePublication, 6, b"heart.publish.w1"),
        (IrFragmentRangeEncoding, IrFragmentRange, 7, b"heart.irrange.w1"),
        (IndexPackEncoding, IndexPack, 8, b"heart.idxpack.w1")
    }
);

#[cfg(test)]
mod tests {
    use super::{
        CapabilityDomain, CompilationTargetDomain, CompilePublicationDomain,
        CompilePublicationEncoding, CompileRecipeDomain, ConfigurationDomain, DependencySetDomain,
        Domain, Encoding, FrameEncoding, IndexExactSegmentDomain, IndexLexicalSegmentDomain,
        IndexPackDomain, IndexPackEncoding, IndexSnapshotDomain, IndexVectorSegmentDomain,
        IrFragmentDomain, IrFragmentEncoding, IrFragmentRangeEncoding, IrManifestDomain,
        IrManifestEncoding, LocalitySortedEncoding, ObjectDomain, ObjectPackEncoding,
        OperationDomain, RootDomain, SourceFactDomain, StageKeyDomain, ToolchainDomain,
        DeclarationKeyDomain, DeclarationFamilyDomain, DeclarationVariantDomain,
        ForeignDeclarationDomain, SemanticScopeDomain,
    };

    #[test]
    /// Proves that no registered tag can alias another domain or encoding namespace.
    fn built_in_domain_and_encoding_labels_are_pairwise_distinct() {
        let domains = [
            ObjectDomain::TAG,
            RootDomain::TAG,
            OperationDomain::TAG,
            DependencySetDomain::TAG,
            CapabilityDomain::TAG,
            ConfigurationDomain::TAG,
            StageKeyDomain::TAG,
            IndexSnapshotDomain::TAG,
            IndexExactSegmentDomain::TAG,
            IndexLexicalSegmentDomain::TAG,
            IndexVectorSegmentDomain::TAG,
            IrFragmentDomain::TAG,
            IrManifestDomain::TAG,
            SourceFactDomain::TAG,
            ToolchainDomain::TAG,
            CompilationTargetDomain::TAG,
            CompileRecipeDomain::TAG,
            CompilePublicationDomain::TAG,
            IndexPackDomain::TAG,
            DeclarationKeyDomain::TAG,
            DeclarationFamilyDomain::TAG,
            DeclarationVariantDomain::TAG,
            ForeignDeclarationDomain::TAG,
            SemanticScopeDomain::TAG,
        ];
        for (index, domain) in domains.iter().enumerate() {
            for other in domains.iter().skip(index + 1) {
                assert_ne!(domain, other);
            }
        }
        let encodings = [
            FrameEncoding::TAG,
            LocalitySortedEncoding::TAG,
            ObjectPackEncoding::TAG,
            IrFragmentEncoding::TAG,
            IrManifestEncoding::TAG,
            CompilePublicationEncoding::TAG,
            IrFragmentRangeEncoding::TAG,
            IndexPackEncoding::TAG,
        ];
        for (index, encoding) in encodings.iter().enumerate() {
            for other in encodings.iter().skip(index + 1) {
                assert_ne!(encoding, other);
            }
            for domain in domains {
                assert_ne!(encoding.as_ref(), domain.as_ref());
            }
        }
    }
}
