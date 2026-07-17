//! Typed conflict-partition keys.
//!
//! Keys are **typed** (never a bare BLAKE3), so a link key can never be
//! confused with any other digest domain (design Issue 12).

use serde::{Deserialize, Serialize};

use crate::hash::ContentBlake3;
use crate::ids::{encode_kind_disc, StableRef};

/// Partition key for an undirected link, order-independent in its endpoints.
///
/// `blake3("nudox.link.v1" || lo.canonical || hi.canonical || u16le(lo_kind) ||
/// u16le(hi_kind))` where `(lo, hi)` is the pair of [`StableRef`] endpoints
/// sorted by canonical bytes, each paired with its own kind discriminant. Sorting
/// the `(ref, kind)` pairs together makes `link_key(a,b) == link_key(b,a)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct LinkDomainKey(ContentBlake3);

impl LinkDomainKey {
    pub const DOMAIN: &'static str = "nudox.link.v1";

    /// Compute the undirected key for `(a, kind_a)` — `(b, kind_b)`.
    pub fn from_link(a: &StableRef, b: &StableRef, kind_a: u16, kind_b: u16) -> Self {
        let a_bytes = a.canonical_bytes();
        let b_bytes = b.canonical_bytes();
        // Order the (endpoint, kind) pairs together so the key is stable under
        // endpoint swap.
        let (lo, lo_k, hi, hi_k) = if a_bytes <= b_bytes {
            (&a_bytes, kind_a, &b_bytes, kind_b)
        } else {
            (&b_bytes, kind_b, &a_bytes, kind_a)
        };
        let mut preimage = Vec::with_capacity(lo.len() + hi.len() + 4);
        preimage.extend_from_slice(lo);
        preimage.extend_from_slice(hi);
        encode_kind_disc(&mut preimage, lo_k);
        encode_kind_disc(&mut preimage, hi_k);
        Self(ContentBlake3::from_domain(Self::DOMAIN, &preimage))
    }

    #[inline]
    pub const fn from_content_blake3(inner: ContentBlake3) -> Self {
        Self(inner)
    }

    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl core::fmt::Debug for LinkDomainKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "link:{}…", &self.0.to_hex()[..12])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{EcosystemId, PackageLineageId, PackageName};
    use crate::IntroId;

    fn sref(pkg: &str, n: u8) -> StableRef {
        StableRef::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(pkg)),
            IntroId::from_raw([n; 32]),
        )
    }

    #[test]
    fn link_key_is_endpoint_order_independent() {
        let a = sref("liba", 1);
        let b = sref("libb", 2);
        assert_eq!(
            LinkDomainKey::from_link(&a, &b, 4, 1),
            LinkDomainKey::from_link(&b, &a, 1, 4),
            "swapping endpoints (with their kinds) must not change the key"
        );
    }

    #[test]
    fn link_key_distinguishes_kinds() {
        let a = sref("liba", 1);
        let b = sref("libb", 2);
        assert_ne!(
            LinkDomainKey::from_link(&a, &b, 4, 1),
            LinkDomainKey::from_link(&a, &b, 4, 2),
            "different endpoint kinds must produce different keys"
        );
    }
}
