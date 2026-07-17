//! Non-hash identity types: package lineage, wire-stable references, channel
//! and author coordinates.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::encode::{encode_str, write_u16le};
use crate::hash::{ContentBlake3, IntroId};

/// An ecosystem/registry namespace (`"cargo"`, `"npm"`, `"pypi"`, …).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EcosystemId(SmolStr);

impl EcosystemId {
    #[inline]
    pub fn new(s: impl Into<SmolStr>) -> Self {
        Self(s.into())
    }
    #[inline]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl core::fmt::Debug for EcosystemId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A package name within its ecosystem.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PackageName(SmolStr);

impl PackageName {
    #[inline]
    pub fn new(s: impl Into<SmolStr>) -> Self {
        Self(s.into())
    }
    #[inline]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl core::fmt::Debug for PackageName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The production identity of a package *lineage* (ecosystem + name), stable
/// across all its generations. This — not the arena-local `PackageIdx` and not
/// brief-06's path-based POC id — is the key Registry/Change APIs use.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PackageLineageId {
    pub ecosystem: EcosystemId,
    pub name: PackageName,
}

impl PackageLineageId {
    #[inline]
    pub fn new(ecosystem: EcosystemId, name: PackageName) -> Self {
        Self { ecosystem, name }
    }

    /// `encode_str(ecosystem) || encode_str(name)` — the canonical lineage
    /// bytes used inside [`StableRef`] and IntroId preimages.
    pub fn encode(&self, out: &mut Vec<u8>) {
        encode_str(out, self.ecosystem.as_str());
        encode_str(out, self.name.as_str());
    }
}

impl core::fmt::Debug for PackageLineageId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.ecosystem.as_str(), self.name.as_str())
    }
}

/// Wire-stable cross-package entry reference: lineage + intro. The **only**
/// cross-package reference form allowed in `Change` payloads and archives
/// (design K27).
///
/// Canonical bytes (used as a hash-preimage fragment):
/// `encode_str(ecosystem) || encode_str(name) || intro_bytes(32)`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StableRef {
    pub package: PackageLineageId,
    pub intro: IntroId,
}

impl StableRef {
    #[inline]
    pub fn new(package: PackageLineageId, intro: IntroId) -> Self {
        Self { package, intro }
    }

    /// Canonical, endian-stable bytes (see type docs). Appends to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        self.package.encode(out);
        out.extend_from_slice(self.intro.as_bytes());
    }

    /// Canonical bytes as an owned buffer.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

impl core::fmt::Debug for StableRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}#{:?}", self.package, self.intro)
    }
}

/// Domain-separated author identity:
/// `blake3("nudox.author.v1" || pubkey_or_email_bytes)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct AuthorId(ContentBlake3);

impl AuthorId {
    pub const DOMAIN: &'static str = "nudox.author.v1";

    /// Derive an author id from opaque identity bytes (public key or email).
    #[inline]
    pub fn from_identity(identity_bytes: &[u8]) -> Self {
        Self(ContentBlake3::from_domain(Self::DOMAIN, identity_bytes))
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

impl core::fmt::Debug for AuthorId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "author:{}…", &self.0.to_hex()[..12])
    }
}

/// Unix milliseconds. A plain scalar carried in change headers; encoded as
/// `u64le` in hash preimages (never a float).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct TimestampUnixMs(pub u64);

impl TimestampUnixMs {
    #[inline]
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_le_bytes());
    }
}

/// A human-authored change message.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangeMessage(SmolStr);

impl ChangeMessage {
    #[inline]
    pub fn new(s: impl Into<SmolStr>) -> Self {
        Self(s.into())
    }
    #[inline]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl core::fmt::Debug for ChangeMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.0.as_str())
    }
}

/// The name of a channel within a package lineage (e.g. `"main"`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChannelName(SmolStr);

impl ChannelName {
    #[inline]
    pub fn new(s: impl Into<SmolStr>) -> Self {
        Self(s.into())
    }
    #[inline]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl core::fmt::Debug for ChannelName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An opaque, process-local channel handle handed out by a `ChannelStore`.
/// Not content-addressed; never sealed into archives or changes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct ChannelId(pub u128);

/// Kind-discriminant fragment used inside link-domain-key preimages. The IR
/// crate's `KindDiscriminant` is a `u16`; this crate accepts the raw `u16` so it
/// need not depend on `nudox-ir` (design forbid-list: `nudox-change` must not
/// reference IR arena types).
#[inline]
pub(crate) fn encode_kind_disc(out: &mut Vec<u8>, disc: u16) {
    write_u16le(out, disc);
}
