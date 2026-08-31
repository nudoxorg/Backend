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

include!("marker_registry.rs");

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
        (IndexVectorSegmentDomain, IndexVectorSegment, 11, b"nudox.idx.vectr1")
    }
    encodings {
        (FrameEncoding, Frame, 1, b"nudox.frame.v1\0\0"),
        (LocalitySortedEncoding, LocalitySorted, 2, b"nudox.locsort.v1"),
        (ObjectPackEncoding, ObjectPack, 3, b"nudox.objpack.v1")
    }
);

#[cfg(test)]
mod tests {
    use super::{
        CapabilityDomain, ConfigurationDomain, DependencySetDomain, Domain, Encoding,
        FrameEncoding, IndexExactSegmentDomain, IndexLexicalSegmentDomain, IndexSnapshotDomain,
        IndexVectorSegmentDomain, LocalitySortedEncoding, ObjectDomain, ObjectPackEncoding,
        OperationDomain, RootDomain, StageKeyDomain,
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
            IndexVectorSegmentDomain::TAG,
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
