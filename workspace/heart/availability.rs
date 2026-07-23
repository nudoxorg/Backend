//! Dual-plane availability enums (INDEX-PLAN §7.1).
//!
//! When the catalog knows a generation or an ObjectPack exists, the *content*
//! may nonetheless live anywhere along a spectrum: applied/installed locally,
//! reachable on a remote store, known-but-not-yet-fetchable, or entirely
//! absent. The serving path answers "can I serve this right now, and from
//! where?" with one of these enums.
//!
//! Two enums of the same shape describe the two bulk planes:
//! - [`IrAvailability`] — a libpijul IR generation (channel tip applied here?).
//! - [`ObjectAvailability`] — a sealed [`crate::object_pack::ObjectPackId`].
//!
//! # Store identity
//!
//! A location points at a store row (`stores.store_id`, INDEX-PLAN §8). `heart`
//! is the shared-vocabulary crate and must not depend on `index`, so it carries
//! a heart-local [`AvailabilityStoreId`] newtype that mirrors `index`'s
//! `StoreId` shape (a 16-byte UUID) by value. The catalog boundary converts
//! between the two.

use serde::{Deserialize, Serialize};

/// The store a piece of content is available from: a `BLOB16` UUID mirroring
/// `index`'s `StoreId` (`stores.store_id`, INDEX-PLAN §8).
///
/// `heart` cannot depend on `index`, so this is an independent newtype with the
/// same 16-byte-UUID contract; the catalog boundary maps between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AvailabilityStoreId(pub uuid::Uuid);

impl AvailabilityStoreId {
    /// Wrap a raw UUID.
    pub const fn from_uuid(uuid: uuid::Uuid) -> Self {
        Self(uuid)
    }

    /// The raw UUID.
    pub const fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }

    /// The 16 big-endian bytes stored as `BLOB16`.
    pub fn to_blob(&self) -> [u8; 16] {
        *self.0.as_bytes()
    }
}

impl std::fmt::Display for AvailabilityStoreId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

/// Where an IR generation's content can be served from right now (§7.1).
///
/// The catalog always knows a generation exists once it is registered
/// (INDEX-PLAN ID-15); this enum reports whether its *IR change closure* is
/// actually reachable, and if remote, from which store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IrAvailability {
    /// The channel tip is applied in the local `IrRepository`; serve directly.
    Local,
    /// Not local, but a remote store holds it; fetch over iroh before serving.
    Remote {
        /// The store row to fetch from.
        store_id: AvailabilityStoreId,
    },
    /// The catalog knows the generation, but its IR is not fetchable yet
    /// (no `present` location; producer may still be sealing / providing).
    Pending,
    /// No location, anywhere, knows this generation's IR.
    Missing,
}

impl IrAvailability {
    /// Whether the content can be served without a fetch (only [`Self::Local`]).
    pub fn is_local(&self) -> bool {
        matches!(self, IrAvailability::Local)
    }

    /// The remote store to fetch from, if this is a [`Self::Remote`] location.
    pub fn remote_store(&self) -> Option<AvailabilityStoreId> {
        match self {
            IrAvailability::Remote { store_id } => Some(*store_id),
            _ => None,
        }
    }
}

/// Where an ObjectPack's bytes can be served from right now (§7.1).
///
/// The same four-state shape as [`IrAvailability`], for a sealed
/// [`crate::object_pack::ObjectPackId`] rather than an IR generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectAvailability {
    /// The pack is present in the local ObjectPack store; range-get directly.
    Local,
    /// Not local, but a remote store holds it; fetch over iroh before serving.
    Remote {
        /// The store row to fetch from.
        store_id: AvailabilityStoreId,
    },
    /// The catalog references this ObjectPackId, but no `present` location
    /// holds its bytes yet (still being provided).
    Pending,
    /// No location, anywhere, holds this pack.
    Missing,
}

impl ObjectAvailability {
    /// Whether the pack can be served without a fetch (only [`Self::Local`]).
    pub fn is_local(&self) -> bool {
        matches!(self, ObjectAvailability::Local)
    }

    /// The remote store to fetch from, if this is a [`Self::Remote`] location.
    pub fn remote_store(&self) -> Option<AvailabilityStoreId> {
        match self {
            ObjectAvailability::Remote { store_id } => Some(*store_id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_store_id() -> AvailabilityStoreId {
        AvailabilityStoreId::from_uuid(uuid::Uuid::from_bytes([
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x00,
        ]))
    }

    #[test]
    fn ir_availability_accessors() {
        assert!(IrAvailability::Local.is_local());
        assert!(!IrAvailability::Pending.is_local());
        let remote = IrAvailability::Remote { store_id: sample_store_id() };
        assert_eq!(remote.remote_store(), Some(sample_store_id()));
        assert_eq!(IrAvailability::Missing.remote_store(), None);
    }

    #[test]
    fn object_availability_accessors() {
        assert!(ObjectAvailability::Local.is_local());
        let remote = ObjectAvailability::Remote { store_id: sample_store_id() };
        assert_eq!(remote.remote_store(), Some(sample_store_id()));
        assert_eq!(ObjectAvailability::Pending.remote_store(), None);
    }

    /// Serde golden: the postcard wire encoding of each variant is stable.
    /// Changing it is a wire-break — fail loudly.
    #[test]
    fn ir_availability_postcard_goldens() {
        let local = postcard::to_allocvec(&IrAvailability::Local).unwrap();
        assert_eq!(local, vec![0x00]);

        let remote = postcard::to_allocvec(&IrAvailability::Remote {
            store_id: sample_store_id(),
        })
        .unwrap();
        // variant tag 0x01, then the UUID as a length-prefixed byte sequence
        // (postcard prefixes the 16 bytes with the length 0x10).
        assert_eq!(
            remote,
            vec![
                0x01, 0x10, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
                0xcc, 0xdd, 0xee, 0xff, 0x00,
            ]
        );

        let pending = postcard::to_allocvec(&IrAvailability::Pending).unwrap();
        assert_eq!(pending, vec![0x02]);

        let missing = postcard::to_allocvec(&IrAvailability::Missing).unwrap();
        assert_eq!(missing, vec![0x03]);
    }

    #[test]
    fn object_availability_postcard_goldens() {
        assert_eq!(
            postcard::to_allocvec(&ObjectAvailability::Local).unwrap(),
            vec![0x00]
        );
        assert_eq!(
            postcard::to_allocvec(&ObjectAvailability::Pending).unwrap(),
            vec![0x02]
        );
        assert_eq!(
            postcard::to_allocvec(&ObjectAvailability::Missing).unwrap(),
            vec![0x03]
        );
    }

    #[test]
    fn ir_availability_roundtrips() {
        for value in [
            IrAvailability::Local,
            IrAvailability::Remote { store_id: sample_store_id() },
            IrAvailability::Pending,
            IrAvailability::Missing,
        ] {
            let bytes = postcard::to_allocvec(&value).unwrap();
            let back: IrAvailability = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(value, back);
        }
    }
}
