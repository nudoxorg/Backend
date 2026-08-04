//! Domain-separated BLAKE3 identity newtypes.
//!
//! Every identity in the system is a 32-byte BLAKE3 digest of a
//! **domain-tagged** canonical preimage: `blake3(domain_string_bytes ||
//! preimage)`. The domain string (e.g. `"nudox.intro.v2"`) makes digests from
//! different planes non-colliding even when their preimages would otherwise be
//! equal. This is a *prefix* tag, not BLAKE3 keyed mode (whose key must be
//! exactly 32 bytes).

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
/// This is the shared representation under every identity newtype; it is never
/// a `CasKey`/`IntroId`/etc. by itself — those are distinct types.
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

impl core::fmt::Display for ContentBlake3 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl core::fmt::Debug for ContentBlake3 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Short prefix keeps logs readable while staying unambiguous.
        write!(f, "b3:{}…", &self.to_hex()[..12])
    }
}

/// Forever-stable identity of a symbol *introduction*. Assigned once on the
/// first `Insert`; renames never change it (design K18). The primary key of
/// the whole change algebra for IR.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct IntroId(ContentBlake3);

impl IntroId {
    /// Wrap raw digest bytes (already computed elsewhere).
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

impl core::fmt::Display for IntroId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl core::fmt::Debug for IntroId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "intro:{}…", &self.to_hex()[..12])
    }
}
