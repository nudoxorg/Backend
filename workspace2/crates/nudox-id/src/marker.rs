use core::ops::Deref;

use crate::TAG_BYTES;

/// Closed protocol registry code for a content identity domain.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DomainCode {
    /// Immutable object content.
    Object = 1,
    /// Published generation root.
    Root = 2,
    /// Workflow operation.
    Operation = 3,
    /// Dependency-set content.
    DependencySet = 4,
    /// Capability content.
    Capability = 5,
    /// Configuration content.
    Configuration = 6,
    /// Workflow stage key.
    StageKey = 7,
    /// Index snapshot content.
    IndexSnapshot = 8,
    /// Exact index segment content.
    IndexExactSegment = 9,
    /// Lexical index segment content.
    IndexLexicalSegment = 10,
}

impl From<DomainCode> for u8 {
    #[allow(
        clippy::as_conversions,
        reason = "closed repr(u8) registry discriminant"
    )]
    fn from(code: DomainCode) -> Self {
        code as u8
    }
}

/// Closed protocol registry code for an encoded artifact representation.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EncodingCode {
    /// Frame encoding.
    Frame = 1,
    /// Locality-sorted encoding.
    LocalitySorted = 2,
    /// Object-pack encoding.
    ObjectPack = 3,
}

impl From<EncodingCode> for u8 {
    #[allow(
        clippy::as_conversions,
        reason = "closed repr(u8) registry discriminant"
    )]
    fn from(code: EncodingCode) -> Self {
        code as u8
    }
}

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

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<[u8; TAG_BYTES]> for DomainTag {
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

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<[u8; TAG_BYTES]> for EncodingTag {
    fn as_ref(&self) -> &[u8; TAG_BYTES] {
        self
    }
}

mod sealed {
    pub trait Domain {}
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

macro_rules! protocol_markers {
    ($(($marker:ident, $trait_name:ident, $tag_type:ident, $code_type:ident, $label:literal, $code:ident)),+ $(,)?) => {
        $(
            #[doc = concat!("Protocol-owned marker for `", stringify!($marker), "`.")]
            #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
            pub enum $marker {}

            impl sealed::$trait_name for $marker {}

            impl $trait_name for $marker {
                const TAG: $tag_type = $tag_type::new(*$label);
                const CODE: $code_type = $code_type::$code;
            }
        )+
    };
}

protocol_markers!(
    (
        ObjectDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.object.v1\0",
        Object
    ),
    (
        RootDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.root.v1\0\0\0",
        Root
    ),
    (
        OperationDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.op.v1\0\0\0\0\0",
        Operation
    ),
    (
        DependencySetDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.depset.v1\0",
        DependencySet
    ),
    (
        CapabilityDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.cap.v1\0\0\0\0",
        Capability
    ),
    (
        ConfigurationDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.config.v1\0",
        Configuration
    ),
    (
        StageKeyDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.stage.v1\0\0",
        StageKey
    ),
    (
        IndexSnapshotDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.idx.snap.1",
        IndexSnapshot
    ),
    (
        IndexExactSegmentDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.idx.exact1",
        IndexExactSegment
    ),
    (
        IndexLexicalSegmentDomain,
        Domain,
        DomainTag,
        DomainCode,
        b"nudox.idx.lexic1",
        IndexLexicalSegment
    ),
    (
        FrameEncoding,
        Encoding,
        EncodingTag,
        EncodingCode,
        b"nudox.frame.v1\0\0",
        Frame
    ),
    (
        LocalitySortedEncoding,
        Encoding,
        EncodingTag,
        EncodingCode,
        b"nudox.locsort.v1",
        LocalitySorted
    ),
    (
        ObjectPackEncoding,
        Encoding,
        EncodingTag,
        EncodingCode,
        b"nudox.objpack.v1",
        ObjectPack
    ),
);

#[cfg(test)]
mod tests {
    use super::{
        CapabilityDomain, ConfigurationDomain, DependencySetDomain, Domain, Encoding,
        FrameEncoding, IndexExactSegmentDomain, IndexLexicalSegmentDomain, IndexSnapshotDomain,
        LocalitySortedEncoding, ObjectDomain, ObjectPackEncoding, OperationDomain, RootDomain,
        StageKeyDomain,
    };

    #[test]
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
