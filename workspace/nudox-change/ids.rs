//! Non-hash identity types: package lineage, wire-stable references, channel
//! and author coordinates.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::encode::{encode_str, write_u16le};
use crate::hash::IntroId;

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

/// Kind-discriminant fragment used inside link-domain-key preimages. The IR
/// crate's `KindDiscriminant` is a `u16`; this crate accepts the raw `u16` so it
/// need not depend on `nudox-ir` (design forbid-list: `nudox-change` must not
/// reference IR arena types).
#[inline]
pub(crate) fn encode_kind_disc(out: &mut Vec<u8>, disc: u16) {
    write_u16le(out, disc);
}
