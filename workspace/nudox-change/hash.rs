//! Domain-separated BLAKE3 identity newtypes.
//!
//! Every identity in the system is a 32-byte BLAKE3 digest of a **domain-tagged**
//! canonical preimage: `blake3(domain_string_bytes || preimage)`. The domain
//! string (e.g. `"nudox-change/v1"`) makes digests from different planes
//! non-colliding even when their preimages would otherwise be equal. This is a
//! *prefix* tag, not BLAKE3 keyed mode (whose key must be exactly 32 bytes).
//!
//! The newtypes are intentionally **not** interconvertible: a [`CasKey`] cannot
//! be passed where a [`GenerationStamp`] is required. This is the type-level
//! backbone of the Hash ①/② discipline (design K12): the outbox stores only a
//! `GenerationStamp`; CAS addresses are `CasKey`; channel membership is
//! `ChangeId`; tip equality is `ChangeSetFingerprint`.

use serde::{Deserialize, Serialize};

/// Compute `blake3(domain || preimage)` and return the raw 32 bytes.
#[inline]
pub fn hash_domain(domain: &str, preimage: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(preimage);
    *hasher.finalize().as_bytes()
}

/// Lowercase hex of 32 bytes.
#[inline]
pub fn to_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

/// BLAKE3-256 of a **domain-tagged** canonical preimage.
///
/// This is the shared representation under every identity newtype below; it is
/// never a `CasKey`/`ChangeId`/etc. by itself — those are distinct types.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct ContentBlake3([u8; 32]);

impl ContentBlake3 {
    /// Wrap raw digest bytes (already computed elsewhere).
    #[inline]
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// `blake3(domain || preimage)`.
    #[inline]
    pub fn from_domain(domain: &str, preimage: &[u8]) -> Self {
        Self(hash_domain(domain, preimage))
    }

    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[inline]
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    #[inline]
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }
}

impl core::fmt::Debug for ContentBlake3 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Short prefix keeps logs readable while staying unambiguous.
        write!(f, "b3:{}…", &self.to_hex()[..12])
    }
}

/// Declare a `#[repr(transparent)]` newtype over [`ContentBlake3`] with the
/// standard constructor/accessor surface and a domain-tagged `Debug`.
macro_rules! blake3_newtype {
    ($(#[$meta:meta])* $name:ident, $dbg:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[repr(transparent)]
        pub struct $name(ContentBlake3);

        impl $name {
            #[inline]
            pub const fn from_content_blake3(inner: ContentBlake3) -> Self {
                Self(inner)
            }
            #[inline]
            pub const fn from_raw(bytes: [u8; 32]) -> Self {
                Self(ContentBlake3::from_raw(bytes))
            }
            /// `blake3(domain || preimage)` wrapped in this newtype.
            #[inline]
            pub fn from_domain(domain: &str, preimage: &[u8]) -> Self {
                Self(ContentBlake3::from_domain(domain, preimage))
            }
            #[inline]
            pub const fn content_blake3(&self) -> ContentBlake3 {
                self.0
            }
            #[inline]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                self.0.as_bytes()
            }
            #[inline]
            pub fn to_hex(&self) -> String {
                self.0.to_hex()
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}:{}…", $dbg, &self.to_hex()[..12])
            }
        }
    };
}

blake3_newtype!(
    /// Content address of exact archive/blob bytes in the CAS (Hash ② domain).
    CasKey, "cas"
);
blake3_newtype!(
    /// Logical generation-snapshot identity (Hash ① domain). Stored by the
    /// outbox; derived from versioned generation `identity_bytes`, never from
    /// a manifest's own CAS bytes.
    GenerationStamp, "gen"
);
blake3_newtype!(
    /// Forever-stable identity of one change object; channel membership key.
    ChangeId, "chg"
);
blake3_newtype!(
    /// Equality-only fingerprint of an applied change set (v1: BLAKE3 of the
    /// sorted `ChangeId` bytes). Sufficient for tip *equality*, NOT for set
    /// reconciliation on its own (design Issue 17).
    ChangeSetFingerprint, "cset"
);
blake3_newtype!(
    /// Forever-stable identity of a symbol *introduction*. Assigned once on the
    /// first `Insert`; renames never change it (design K18). The primary key of
    /// the whole change algebra for IR.
    IntroId, "intro"
);

/// Alias used at the `ChannelStore` tip API — same bits as
/// [`ChangeSetFingerprint`] in v1, kept as an alias so a future homomorphic
/// Merkle state can replace it without churning call sites.
pub type MerkleState = ChangeSetFingerprint;

impl ChangeSetFingerprint {
    /// Domain tag for the v1 change-set fingerprint.
    pub const DOMAIN: &'static str = "nudox.cset.v1";

    /// Fingerprint of an applied change set: `blake3(DOMAIN || sort(id bytes))`.
    ///
    /// The input is copied and sorted, so call-site order is irrelevant — this
    /// is what makes the fingerprint a set identity (design Issue 17).
    pub fn from_change_ids(ids: &[ChangeId]) -> Self {
        let mut sorted: Vec<[u8; 32]> = ids.iter().map(|c| *c.as_bytes()).collect();
        sorted.sort_unstable();
        let mut preimage = Vec::with_capacity(sorted.len() * 32);
        for id in &sorted {
            preimage.extend_from_slice(id);
        }
        Self::from_domain(Self::DOMAIN, &preimage)
    }

    /// Fingerprint of the empty change set (a fresh channel tip).
    pub fn empty() -> Self {
        Self::from_change_ids(&[])
    }
}
