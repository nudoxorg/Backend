//! Defines document behavior for `backend-semantic::index_core`, whose purpose is to define immutable index documents, segments, and snapshot identities.
//! This module owns the document invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Cross-plane immutable entity document identities.

use core::{cmp::Ordering, mem::size_of};

use crate::ir::EntityId;
use backend_version::{
    ArtifactId, Domain, Encoding, HASH_BYTES, IrFragmentDomain, IrFragmentEncoding,
    IrSemanticImageDomain, IrSemanticImageEncoding,
};

/// Fixed encoded width of one immutable globally addressable entity document.
pub const ENTITY_DOCUMENT_ID_BYTES: usize = HASH_BYTES + size_of::<u32>();

/// Closed immutable compiler artifact that owns an indexed entity coordinate.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EntityArtifactIdentity {
    /// Legacy compact fragment authority.
    Compact(ArtifactId<IrFragmentEncoding, IrFragmentDomain>),
    /// Complete portable semantic-image authority.
    Semantic(ArtifactId<IrSemanticImageEncoding, IrSemanticImageDomain>),
}

/// Immutable globally addressable entity document identity.
///
/// This is the one entity-address concept shared by the exact and lexical planes: a complete
/// content-addressed compiler artifact plus its validated entity position. It is collision-free
/// across compact compatibility and full semantic images without an untyped hash salt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct EntityDocumentId {
    /// Complete immutable compiler artifact containing the declaration.
    pub artifact: EntityArtifactIdentity,
    /// Canonical declaration position within the selected artifact.
    pub entity: EntityId,
}

impl PartialOrd for EntityDocumentId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EntityDocumentId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.artifact
            .cmp(&other.artifact)
            .then_with(|| self.entity.raw.cmp(&other.entity.raw))
    }
}

impl From<EntityDocumentId> for [u8; ENTITY_DOCUMENT_ID_BYTES] {
    fn from(document: EntityDocumentId) -> Self {
        let mut bytes = [0_u8; ENTITY_DOCUMENT_ID_BYTES];
        match document.artifact {
            EntityArtifactIdentity::Compact(identity) => {
                bytes[..HASH_BYTES].copy_from_slice(identity.as_ref());
            }
            EntityArtifactIdentity::Semantic(identity) => {
                bytes[..HASH_BYTES].copy_from_slice(identity.as_ref());
            }
        }
        bytes[HASH_BYTES..].copy_from_slice(&document.entity.raw.to_be_bytes());
        bytes
    }
}

/// Rejection while decoding a persisted entity document identity.
#[derive(Debug, thiserror::Error)]
pub enum EntityDocumentIdError {
    /// The stored field did not have the exact fixed global document width.
    #[error("entity document has {actual} bytes, expected {ENTITY_DOCUMENT_ID_BYTES}")]
    Width {
        /// Complete observed byte width.
        actual: usize,
        /// Exact fixed-array decoding terminal.
        #[source]
        source: core::array::TryFromSliceError,
    },
    /// The embedded artifact authority cell was not an indexable compiler artifact.
    #[error(
        "entity document artifact authority ({observed_encoding}, {observed_domain}) is unknown"
    )]
    ArtifactAuthority {
        /// Complete observed encoding registry code.
        observed_encoding: u8,
        /// Complete observed semantic-domain registry code.
        observed_domain: u8,
    },
    /// The stored artifact bytes did not carry the authority selected by their tag.
    #[error("entity document artifact authority was invalid")]
    Artifact {
        /// Complete typed artifact decoding cause.
        #[source]
        source: backend_version::ArtifactIdDecodeError,
    },
    /// The verified fixed-width document could not recover its entity component.
    #[error("entity document entity component was structurally malformed")]
    Entity {
        /// Exact fixed-array decoding terminal.
        #[source]
        source: core::array::TryFromSliceError,
    },
}

impl TryFrom<&[u8]> for EntityDocumentId {
    type Error = EntityDocumentIdError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let raw = <[u8; ENTITY_DOCUMENT_ID_BYTES]>::try_from(bytes).map_err(|source| {
            EntityDocumentIdError::Width {
                actual: bytes.len(),
                source,
            }
        })?;
        let identity = &raw[..HASH_BYTES];
        let authority = (raw[0], raw[1]);
        let compact_authority = (
            u8::from(IrFragmentEncoding::CODE),
            u8::from(IrFragmentDomain::CODE),
        );
        let semantic_authority = (
            u8::from(IrSemanticImageEncoding::CODE),
            u8::from(IrSemanticImageDomain::CODE),
        );
        let artifact = match authority {
            observed if observed == compact_authority => EntityArtifactIdentity::Compact(
                ArtifactId::try_from(identity)
                    .map_err(|source| EntityDocumentIdError::Artifact { source })?,
            ),
            observed if observed == semantic_authority => EntityArtifactIdentity::Semantic(
                ArtifactId::try_from(identity)
                    .map_err(|source| EntityDocumentIdError::Artifact { source })?,
            ),
            (observed_encoding, observed_domain) => {
                return Err(EntityDocumentIdError::ArtifactAuthority {
                    observed_encoding,
                    observed_domain,
                });
            }
        };
        let entity = EntityId::new(u32::from_be_bytes(
            raw[HASH_BYTES..]
                .try_into()
                .map_err(|source| EntityDocumentIdError::Entity { source })?,
        ));
        Ok(Self { artifact, entity })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ENTITY_DOCUMENT_ID_BYTES, EntityArtifactIdentity, EntityDocumentId, EntityDocumentIdError,
    };
    use crate::ir::EntityId;
    use backend_version::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};

    #[test]
    fn global_document_wire_roundtrips_and_rejects_foreign_authority() {
        let document = EntityDocumentId {
            artifact: EntityArtifactIdentity::Compact(ArtifactId::<
                IrFragmentEncoding,
                IrFragmentDomain,
            >::from_encoded_bytes(
                b"entity-document-wire"
            )),
            entity: EntityId::new(7),
        };
        let mut bytes: [u8; ENTITY_DOCUMENT_ID_BYTES] = document.into();
        assert!(matches!(
            EntityDocumentId::try_from(bytes.as_slice()),
            Ok(observed) if observed == document
        ));
        assert!(matches!(
            EntityDocumentId::try_from(&bytes[..ENTITY_DOCUMENT_ID_BYTES - 1]),
            Err(EntityDocumentIdError::Width { .. })
        ));
        bytes[1] ^= 1;
        assert!(matches!(
            EntityDocumentId::try_from(bytes.as_slice()),
            Err(EntityDocumentIdError::ArtifactAuthority { .. })
        ));
    }
}
