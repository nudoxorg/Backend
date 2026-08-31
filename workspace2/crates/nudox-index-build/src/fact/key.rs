use core::{
    mem::{align_of, size_of},
    ops::Deref,
};

use nudox_id::{ContentId, HASH_BYTES, IndexExactSegmentDomain};
use nudox_ir_vocab::EntityId;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

const EXACT_ENTITY_KEY_SCHEMA: u8 = 1;

/// Exact serialized width of one fragment-global entity key.
pub const EXACT_ENTITY_KEY_BYTES: usize = HASH_BYTES + 1 + size_of::<u32>();

/// A canonical exact-plane identity for one entity in a prepared fragment.
///
/// The canonical entity ordinal is encoded big-endian after the schema byte, so the byte order
/// is exactly the builder's canonical entity order for every admitted row. The typed canonical
/// fragment namespace prefix makes the key globally unique across all indexed fragments without
/// a second per-entity hash. It is deliberately distinct from the borrowed source name:
/// declarations may overload one name, while name lookup remains the lexical plane.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ExactEntityKey([u8; EXACT_ENTITY_KEY_BYTES]);

impl ExactEntityKey {
    pub(crate) fn new(
        fragment_namespace: ContentId<IndexExactSegmentDomain>,
        entity: EntityId,
    ) -> Self {
        let wire = ExactEntityKeyWire {
            namespace: *fragment_namespace,
            schema: EXACT_ENTITY_KEY_SCHEMA,
            ordinal: entity.raw.to_be_bytes(),
        };
        Self(zerocopy::transmute!(wire))
    }
}

/// Declarative, padding-free global key grammar used by exact-core rows.
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
#[repr(C)]
struct ExactEntityKeyWire {
    namespace: [u8; HASH_BYTES],
    schema: u8,
    ordinal: [u8; size_of::<u32>()],
}

const _: () = assert!(size_of::<ExactEntityKeyWire>() == EXACT_ENTITY_KEY_BYTES);
const _: () = assert!(align_of::<ExactEntityKeyWire>() == 1);

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
    use nudox_id::{ContentId, IndexExactSegmentDomain};
    use nudox_index_core::MAX_EXACT_ROWS;
    use nudox_ir_vocab::EntityId;
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
        let fragment =
            ContentId::<IndexExactSegmentDomain>::from_canonical_bytes(b"exact-entity-key-order");
        let mut previous = ExactEntityKey::new(fragment, EntityId::new(0));
        for ordinal in 1..MAX_EXACT_ROWS {
            let ordinal = u32::try_from(ordinal)
                .map_err(|source| ExactEntityKeyTestError::Ordinal { source })?;
            let observed = ExactEntityKey::new(fragment, EntityId::new(ordinal));
            if previous >= observed {
                return Err(ExactEntityKeyTestError::NotOrdered);
            }
            previous = observed;
        }
        Ok(())
    }
}
