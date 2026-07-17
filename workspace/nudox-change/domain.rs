//! Typed conflict-partition keys and validated graph identities.
//!
//! An atom declares the [`DomainKey`]s it touches; two atoms *conflict* iff they
//! share a domain key. Keys are **typed** (never a bare BLAKE3), so an intro
//! key can never be confused with a link key or a graph key (design Issue 12).

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;

use crate::hash::{ContentBlake3, IntroId};
use crate::ids::{encode_kind_disc, PackageLineageId, StableRef};

/// A typed conflict-partition key. Two atoms are free to commute unless they
/// share a `DomainKey`.
///
/// Not `Copy`: the graph variants carry a validated `SmolStr` IRI. The intro and
/// link variants are cheap to clone (32 bytes), so cloning a `DomainKey` is
/// cheap in the common IR path.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum DomainKey {
    /// An entry introduction (Insert/Update/Delete/Retarget/Reparent target).
    Intro(IntroId),
    /// An undirected link between two entries.
    Link(LinkDomainKey),
    /// A projected graph document.
    GraphDoc(GraphDocId),
    /// A projected graph relation.
    GraphRelation(GraphRelationId),
}

impl DomainKey {
    /// Domain keys conflict iff they are equal.
    #[inline]
    pub fn conflicts_with_key(&self, other: &Self) -> bool {
        self == other
    }
}

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

/// Why a graph identity string was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GraphIdError {
    #[error("graph id is empty")]
    Empty,
    #[error("graph id {0:?} has no scheme or known prefix")]
    NoSchemeOrPrefix(String),
}

/// A validated linked-data identity (an IRI). Private; only constructed via the
/// fallible [`GraphDocId`]/[`GraphRelationId`] constructors so an unvalidated
/// `SmolStr` can never masquerade as a graph id.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
struct GraphIri(SmolStr);

impl GraphIri {
    /// Require a scheme (`foo:`) or a `/`-style known prefix. Kept intentionally
    /// permissive for v1 — the point is to reject bare, un-namespaced strings.
    fn parse(s: &str) -> Result<Self, GraphIdError> {
        if s.is_empty() {
            return Err(GraphIdError::Empty);
        }
        let has_scheme = s
            .split_once(':')
            .is_some_and(|(scheme, _)| !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-'));
        if has_scheme || s.contains('/') {
            Ok(Self(SmolStr::new(s)))
        } else {
            Err(GraphIdError::NoSchemeOrPrefix(s.to_owned()))
        }
    }

    #[inline]
    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// IRI minting policy for [`GraphDocId::from_intro`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct IriPolicy {
    /// e.g. `"https://nudox.dev/id/"` — must parse as a valid IRI prefix.
    pub base: SmolStr,
    /// Bump on any minting-scheme change.
    pub version: u16,
}

impl IriPolicy {
    pub fn new(base: impl Into<SmolStr>, version: u16) -> Self {
        Self { base: base.into(), version }
    }

    /// `base || ecosystem || "/" || name || "/" || hex(suffix)`.
    fn mint(&self, package: &PackageLineageId, suffix_hex: &str, tail: &str) -> String {
        format!(
            "{}{}/{}/{}{}",
            self.base,
            package.ecosystem.as_str(),
            package.name.as_str(),
            suffix_hex,
            tail
        )
    }
}

/// Validated Terminus / linked-data document identity.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphDocId {
    repr: GraphIri,
}

impl GraphDocId {
    pub fn parse(s: &str) -> Result<Self, GraphIdError> {
        Ok(Self { repr: GraphIri::parse(s)? })
    }

    /// Mint a document id for an intro under a package lineage.
    pub fn from_intro(policy: &IriPolicy, package: &PackageLineageId, intro: IntroId) -> Self {
        let iri = policy.mint(package, &intro.to_hex(), "");
        // Minting always includes the base + '/', so parse cannot fail; if a
        // caller supplies a degenerate base we surface that as a doc id that
        // still round-trips through parse on the same string.
        Self { repr: GraphIri::parse(&iri).unwrap_or(GraphIri(SmolStr::new(iri))) }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        self.repr.as_str()
    }
}

impl core::fmt::Debug for GraphDocId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "doc:{}", self.repr.as_str())
    }
}

/// Validated linked-data relation identity.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphRelationId {
    repr: GraphIri,
}

impl GraphRelationId {
    pub fn parse(s: &str) -> Result<Self, GraphIdError> {
        Ok(Self { repr: GraphIri::parse(s)? })
    }

    /// Mint a relation id for a link/parent edge under a package lineage.
    pub fn from_intro(
        policy: &IriPolicy,
        package: &PackageLineageId,
        intro: IntroId,
        relation: &str,
    ) -> Self {
        let iri = policy.mint(package, &intro.to_hex(), &format!("/{relation}"));
        Self { repr: GraphIri::parse(&iri).unwrap_or(GraphIri(SmolStr::new(iri))) }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        self.repr.as_str()
    }
}

impl core::fmt::Debug for GraphRelationId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "rel:{}", self.repr.as_str())
    }
}
