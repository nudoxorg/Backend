//! The **one** byte-framing law for deriving deterministic package ids from
//! their logical parts.
//!
//! Multiple producers derive a package's id independently — the direct-git
//! enumerator ([`ingestor`]), the Homebrew follower, the system-model seeder
//! ([`index`]), and registry [`crate::package::Coordinates`] all mint the *same*
//! logical identity from *different call sites*. If any of them frames the seed
//! bytes differently, the same slug hashes to a different UUID and its catalog
//! rows orphan. This module is the single framing every one of them routes
//! through, so "the same thing" always lands on the same id.
//!
//! # The frozen framing law
//!
//! A package id is `UUIDv5(namespace::PACKAGE, framed_parts)` where
//! `framed_parts` is the concatenation, in order, of each logical part encoded
//! as:
//!
//! ```text
//! len(part) as u64 little-endian  ‖  part bytes  ‖  0x00
//! ```
//!
//! Length-prefixing every part makes the encoding **injective**: no crafted
//! part boundary can collide with a different tuple's byte stream (a bare
//! separator like `\0` alone does not — `"a\0b"` and `"a\0" + "b"` would tie).
//! The trailing `0x00` is a readability seam, not the collision mechanism.
//!
//! This is the exact framing [`crate::package::Coordinates::identity_bytes`]
//! uses; do **not** introduce a second one.

use crate::identity::{Id, Package, PackageId, namespace};

/// Frame an ordered sequence of logical parts into the canonical injective seed
/// byte stream (see the module law). This is the only place the framing is
/// spelled; all id derivations build their part list and hand it here.
pub fn frame_identity_parts<'a, Parts>(parts: Parts) -> Vec<u8>
where
    Parts: IntoIterator<Item = &'a [u8]>,
{
    parts.into_iter().fold(Vec::new(), |mut bytes, part| {
        bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
        bytes.extend_from_slice(part);
        bytes.push(0);
        bytes
    })
}

/// Derive a [`PackageId`] from an ordered sequence of logical parts under the
/// frozen framing law, in the package namespace. The single UUIDv5 primitive
/// every package-id derivation is built on.
pub fn package_id_from_parts<'a, Parts>(parts: Parts) -> PackageId
where
    Parts: IntoIterator<Item = &'a [u8]>,
{
    let seed = frame_identity_parts(parts);
    Id::<Package>::from_name(&namespace::PACKAGE, &seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_is_injective_across_part_boundaries() {
        // `["a", "bc"]` and `["ab", "c"]` must NOT collide even though a naive
        // concatenation would tie them.
        let one = frame_identity_parts([b"a".as_slice(), b"bc".as_slice()]);
        let two = frame_identity_parts([b"ab".as_slice(), b"c".as_slice()]);
        assert_ne!(one, two);
    }

    #[test]
    fn derivation_is_deterministic() {
        let a = package_id_from_parts([b"cpp".as_slice(), b"github.com/madler/zlib".as_slice()]);
        let b = package_id_from_parts([b"cpp".as_slice(), b"github.com/madler/zlib".as_slice()]);
        assert_eq!(a, b);
    }
}
