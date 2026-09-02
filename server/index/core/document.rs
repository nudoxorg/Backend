//! Defines document behavior for `server-index-core`, whose purpose is to define immutable index documents, segments, and snapshot identities.
//! This module owns the document invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Cross-plane immutable entity document identities.

use core::{cmp::Ordering, mem::size_of};

use compiler_ir_vocabulary::EntityId;
use heart_identity::{ArtifactId, HASH_BYTES, IrFragmentDomain, IrFragmentEncoding};

/// Fixed encoded width of one immutable globally addressable entity document.
pub const ENTITY_DOCUMENT_ID_BYTES: usize = HASH_BYTES + size_of::<u32>();

/// Immutable globally addressable entity document identity.
///
/// This is the one entity-address concept shared by the exact and lexical planes: a complete
/// content-addressed compact-IR fragment plus its canonical entity position. It is collision-free
/// across a snapshot without a lossy hash salt. A raw fragment change intentionally creates a new
/// address even when its declaration text is unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct EntityDocumentId {
    /// Complete immutable compact-IR artifact containing the declaration.
    pub fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    /// Canonical declaration position within `fragment` after compiler-index normalization.
    pub entity: EntityId,
}

const _: () = assert!(size_of::<EntityDocumentId>() == ENTITY_DOCUMENT_ID_BYTES);

impl PartialOrd for EntityDocumentId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EntityDocumentId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.fragment
            .cmp(&other.fragment)
            .then_with(|| self.entity.raw.cmp(&other.entity.raw))
    }
}

impl From<EntityDocumentId> for [u8; ENTITY_DOCUMENT_ID_BYTES] {
    fn from(document: EntityDocumentId) -> Self {
        let mut bytes = [0_u8; ENTITY_DOCUMENT_ID_BYTES];
        bytes[..HASH_BYTES].copy_from_slice(document.fragment.as_ref());
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
    /// The stored fragment bytes did not carry compact-IR artifact authority.
    #[error("entity document fragment authority was invalid")]
    Fragment {
        /// Complete typed artifact decoding cause.
        #[source]
        source: heart_identity::ArtifactIdDecodeError,
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
        let fragment = ArtifactId::try_from(&raw[..HASH_BYTES])
            .map_err(|source| EntityDocumentIdError::Fragment { source })?;
        let entity = EntityId::new(u32::from_be_bytes(
            raw[HASH_BYTES..]
                .try_into()
                .map_err(|source| EntityDocumentIdError::Entity { source })?,
        ));
        Ok(Self { fragment, entity })
    }
}

#[cfg(test)]
mod tests {
    use super::{ENTITY_DOCUMENT_ID_BYTES, EntityDocumentId, EntityDocumentIdError};
    use compiler_ir_vocabulary::EntityId;
    use heart_identity::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};

    #[test]
    fn global_document_wire_roundtrips_and_rejects_foreign_authority() {
        let document = EntityDocumentId {
            fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"entity-document-wire",
            ),
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
        bytes[0] ^= 1;
        assert!(matches!(
            EntityDocumentId::try_from(bytes.as_slice()),
            Err(EntityDocumentIdError::Fragment { .. })
        ));
    }
}
