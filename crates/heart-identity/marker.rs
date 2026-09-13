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

// These durable wire-domain tags retain their historical spellings for persisted-ID
// compatibility; they are not product naming.
protocol_registry!(
    domains {
        (ObjectDomain, Object, 1, b"nudox.object.v1\0"),
        (RootDomain, Root, 2, b"nudox.root.v1\0\0\0"),
        (OperationDomain, Operation, 3, b"nudox.op.v1\0\0\0\0\0"),
        (DependencySetDomain, DependencySet, 4, b"nudox.depset.v1\0"),
        (CapabilityDomain, Capability, 5, b"nudox.cap.v1\0\0\0\0"),
        (ConfigurationDomain, Configuration, 6, b"nudox.config.v1\0"),
        (StageKeyDomain, StageKey, 7, b"nudox.stage.v1\0\0"),
        (IndexSnapshotDomain, IndexSnapshot, 8, b"nudox.idx.snap.1"),
        (IndexExactSegmentDomain, IndexExactSegment, 9, b"nudox.idx.exact1"),
        (IndexLexicalSegmentDomain, IndexLexicalSegment, 10, b"nudox.idx.lexic1"),
        (IndexVectorSegmentDomain, IndexVectorSegment, 11, b"nudox.idx.vectr1"),
        (IrFragmentDomain, IrFragment, 12, b"nudox.irfrag.v1\0"),
        (IrManifestDomain, IrManifest, 13, b"nudox.irmani.v1\0"),
        (SourceFactDomain, SourceFact, 14, b"nudox.source.v1\0"),
        (ToolchainDomain, Toolchain, 15, b"nudox.tool.v1\0\0\0"),
        (CompilationTargetDomain, CompilationTarget, 16, b"nudox.target.v1\0"),
        (CompileRecipeDomain, CompileRecipe, 17, b"nudox.recipe.v1\0"),
        (CompilePublicationDomain, CompilePublication, 18, b"nudox.publish.v1"),
        (IndexPackDomain, IndexPack, 19, b"nudox.idx.pack.1"),
        (DeclarationKeyDomain, DeclarationKey, 20, b"nudox.declkey.v1"),
        (DeclarationFamilyDomain, DeclarationFamily, 21, b"nudox.declfam.v1"),
        (DeclarationVariantDomain, DeclarationVariant, 22, b"nudox.declvar.v1"),
        (ForeignDeclarationDomain, ForeignDeclaration, 23, b"nudox.foreign.v1"),
        (SemanticScopeDomain, SemanticScope, 24, b"nudox.scopeid.v1"),
        (IrSemanticImageDomain, IrSemanticImage, 25, b"nudox.semir.v1\0\0")
    }
    encodings {
        (FrameEncoding, Frame, 1, b"nudox.frame.v1\0\0"),
        (LocalitySortedEncoding, LocalitySorted, 2, b"nudox.locsort.v1"),
        (ObjectPackEncoding, ObjectPack, 3, b"nudox.objpack.v1"),
        (IrFragmentEncoding, IrFragment, 4, b"nudox.irfrag.w1\0"),
        (IrManifestEncoding, IrManifest, 5, b"nudox.irmani.w1\0"),
        (CompilePublicationEncoding, CompilePublication, 6, b"nudox.publish.w1"),
        (IrFragmentRangeEncoding, IrFragmentRange, 7, b"nudox.irrange.w1"),
        (IndexPackEncoding, IndexPack, 8, b"nudox.idxpack.w1"),
        (IrSemanticImageEncoding, IrSemanticImage, 9, b"nudox.semir.w1\0\0")
    }
);

#[cfg(test)]
mod tests {
    use super::{
        CapabilityDomain, CompilationTargetDomain, CompilePublicationDomain,
        CompilePublicationEncoding, CompileRecipeDomain, ConfigurationDomain,
        DeclarationFamilyDomain, DeclarationKeyDomain, DeclarationVariantDomain,
        DependencySetDomain, Domain, Encoding, ForeignDeclarationDomain, FrameEncoding,
        IndexExactSegmentDomain, IndexLexicalSegmentDomain, IndexPackDomain, IndexPackEncoding,
        IndexSnapshotDomain, IndexVectorSegmentDomain, IrFragmentDomain, IrFragmentEncoding,
        IrFragmentRangeEncoding, IrManifestDomain, IrManifestEncoding, IrSemanticImageDomain,
        IrSemanticImageEncoding, LocalitySortedEncoding, ObjectDomain, ObjectPackEncoding,
        OperationDomain, RootDomain, SemanticScopeDomain, SourceFactDomain, StageKeyDomain,
        ToolchainDomain,
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
            IrSemanticImageDomain::TAG,
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
            IrSemanticImageEncoding::TAG,
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
