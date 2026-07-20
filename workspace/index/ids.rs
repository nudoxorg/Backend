//! Typed identifiers for catalog rows.
//!
//! Schema v4 (INDEX-PLAN §8) uses two binary key widths:
//! - `BLOB16` — 16-byte UUIDs (package stems, version ids, store ids, advisory
//!   ids). These reuse heart's [`PackageId`] where they name a package/version,
//!   and get dedicated newtypes otherwise.
//! - `BLOB32` — 32-byte BLAKE3 hashes (generation stamps, channel tips, job
//!   keys, ObjectPack ids, IntroIds).
//!
//! Every id here has a total, tested codec to/from its blob form (see
//! [`crate::codec`]); a wrong-length blob is a typed error, never a panic.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use heart::PackageId;

/// Why a blob could not be decoded into a typed id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdDecodeError {
    /// The blob had the wrong length for its target id width.
    #[error("expected a {expected}-byte id, got {actual} bytes")]
    WrongLength { expected: usize, actual: usize },
}

// ─────────────────────────────────────────────────────────────────────────────
// BLOB16 — 16-byte UUID ids
// ─────────────────────────────────────────────────────────────────────────────

/// Generate a `BLOB16` UUID newtype with a total blob codec.
macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Wrap a raw UUID.
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The raw UUID.
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// The 16 big-endian bytes stored as `BLOB16`.
            pub fn to_blob(&self) -> [u8; 16] {
                *self.0.as_bytes()
            }

            /// Decode from a stored `BLOB16`.
            pub fn from_blob(bytes: &[u8]) -> Result<Self, IdDecodeError> {
                let array: [u8; 16] =
                    bytes.try_into().map_err(|_| IdDecodeError::WrongLength {
                        expected: 16,
                        actual: bytes.len(),
                    })?;
                Ok(Self(Uuid::from_bytes(array)))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

uuid_id! {
    /// The stem (name-level) identity of a package: `packages.stem_id`
    /// (INDEX-PLAN §8). Distinct from [`PackageId`] (a *version* instance).
    PackageStemId
}
uuid_id! {
    /// An IR/ObjectPack store: `stores.store_id`.
    StoreId
}
uuid_id! {
    /// An advisory record: `advisories.id`.
    AdvisoryId
}

/// A version instance id (`versions.id`) is heart's [`PackageId`]; give it the
/// same total blob codec the other BLOB16 ids have, as free functions so we do
/// not orphan-impl on a foreign type.
pub mod version_id {
    use super::{IdDecodeError, PackageId};
    use uuid::Uuid;

    /// The 16 bytes stored as `versions.id`.
    pub fn to_blob(id: &PackageId) -> [u8; 16] {
        *id.as_uuid().as_bytes()
    }

    /// Decode a `versions.id` blob.
    pub fn from_blob(bytes: &[u8]) -> Result<PackageId, IdDecodeError> {
        let array: [u8; 16] = bytes.try_into().map_err(|_| IdDecodeError::WrongLength {
            expected: 16,
            actual: bytes.len(),
        })?;
        Ok(PackageId::from_uuid(Uuid::from_bytes(array)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BLOB32 — 32-byte hash ids
// ─────────────────────────────────────────────────────────────────────────────

/// Generate a `BLOB32` hash newtype with a total blob codec + hex rendering.
macro_rules! hash_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub [u8; 32]);

        impl $name {
            /// Wrap 32 raw bytes.
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// The raw bytes stored as `BLOB32`.
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// The 32 bytes as an owned blob.
            pub fn to_blob(&self) -> [u8; 32] {
                self.0
            }

            /// Decode from a stored `BLOB32`.
            pub fn from_blob(bytes: &[u8]) -> Result<Self, IdDecodeError> {
                let array: [u8; 32] =
                    bytes.try_into().map_err(|_| IdDecodeError::WrongLength {
                        expected: 32,
                        actual: bytes.len(),
                    })?;
                Ok(Self(array))
            }

            /// Lower-hex rendering for logs and debug.
            pub fn to_hex(&self) -> String {
                let mut out = String::with_capacity(64);
                for byte in self.0 {
                    out.push_str(&format!("{byte:02x}"));
                }
                out
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_hex())
            }
        }
    };
}

hash_id! {
    /// A generation stamp: `generations.gen_stamp` (INDEX-PLAN §8, ID-15).
    /// The commit-gated `BLAKE3(project‖repo‖commit_oid[‖submodule_pins])`.
    GenerationStamp
}
hash_id! {
    /// An IR channel tip hash (`generations.channel_tip`, nullable until seal).
    ChannelTip
}
hash_id! {
    /// A producer job key (`generations.job_key`, `outbox`/`compile_cache`),
    /// mirroring heart's `JobKey` digest as a stored `BLOB32`.
    JobKeyHash
}
hash_id! {
    /// An ObjectPack root hash (`versions.source_pack`, `object_locations`,
    /// `compile_cache.object_id`) — INDEX-PLAN §6.2 `ObjectPackId`.
    ObjectPackHash
}
hash_id! {
    /// An IntroId (`symbols_proj.intro_id`) — IR-plane symbol identity projected
    /// into the search shape only (INDEX-PLAN §8: identity SoT is the IR).
    IntroIdHash
}
hash_id! {
    /// The edgepack recipe key digest (`edgepack_artifacts.edgepack_key_digest`).
    EdgepackKeyDigest
}

/// Convert a heart [`ObjectPackId`](heart::ObjectPackId) into the catalog's
/// stored [`ObjectPackHash`].
pub fn object_pack_hash(id: &heart::ObjectPackId) -> ObjectPackHash {
    ObjectPackHash::from_bytes(*id.0.as_bytes())
}
