//! Defines fact key behavior for `server-index-build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the fact key invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;

use server_index_core::{ENTITY_DOCUMENT_ID_BYTES, EntityDocumentId};

/// Exact serialized width of one compiler-artifact-global entity key.
pub const EXACT_ENTITY_KEY_BYTES: usize = ENTITY_DOCUMENT_ID_BYTES;

/// A canonical exact-plane identity for one entity in a prepared compiler artifact.
///
/// Its bytes are the shared [`EntityDocumentId`] grammar: complete immutable fragment authority
/// followed by a canonical entity ordinal. Exact and lexical lanes therefore agree on one global
/// entity address without an additional namespace hash. It remains distinct from the borrowed
/// name because declarations may overload names while lexical lookup remains name-oriented.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ExactEntityKey([u8; EXACT_ENTITY_KEY_BYTES]);

impl From<EntityDocumentId> for ExactEntityKey {
    fn from(document: EntityDocumentId) -> Self {
        Self(document.into())
    }
}

impl AsRef<[u8]> for ExactEntityKey {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for ExactEntityKey {
    type Target = [u8; EXACT_ENTITY_KEY_BYTES];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::ExactEntityKey;
    use backend_semantic::ir::EntityId;
    use backend_version::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
    use server_index_core::{EntityArtifactIdentity, EntityDocumentId, MAX_EXACT_ROWS};
    use thiserror::Error;

    #[derive(Debug, Error)]
    enum ExactEntityKeyTestError {
        #[error("canonical exact entity key bytes were not ordered by ordinal")]
        NotOrdered,
        #[error("shared exact row bound did not fit compiler entity ordinal")]
        Ordinal {
            #[source]
            source: core::num::TryFromIntError,
        },
    }

    #[test]
    fn canonical_key_byte_order_matches_every_admitted_ordinal()
    -> Result<(), ExactEntityKeyTestError> {
        let fragment = ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            b"exact-key-order",
        );
        let mut previous = ExactEntityKey::from(EntityDocumentId {
            artifact: EntityArtifactIdentity::Compact(fragment),
            entity: EntityId::new(0),
        });
        for ordinal in 1..MAX_EXACT_ROWS {
            let ordinal = u32::try_from(ordinal)
                .map_err(|source| ExactEntityKeyTestError::Ordinal { source })?;
            let observed = ExactEntityKey::from(EntityDocumentId {
                artifact: EntityArtifactIdentity::Compact(fragment),
                entity: EntityId::new(ordinal),
            });
            if previous >= observed {
                return Err(ExactEntityKeyTestError::NotOrdered);
            }
            previous = observed;
        }
        Ok(())
    }
}
