//! VCS-layer identity vocabulary deferred from `nudox-ir`.
//!
//! `ChangeSetFingerprint` and `LinkDomainKey` live here because they are
//! owned by the VCS layer, not the IR data model.  `nudox-ir` explicitly
//! deferred them (see `workspace/ir/model/src/change/mod.rs` §VCS-layer types).
//!
//! `LinkRecord` is the format-level undirected link representation used by the
//! archive binary format and the structural diff engine.  It is distinct from
//! the richer directed `nudox_ir::relation::Relation` — the archive seals
//! undirected cross-entry edges, while Relation carries semantic direction and
//! fidelity.
//!
//! `ArenaIdx`, `TypeFingerprintId`, `StrId` are archive-specific index handles.

use serde::{Deserialize, Serialize};

use ir::change::{ContentBlake3, StableRef};
use ir::kind::KindDiscriminant;
use ir::manifest::ChangeId;

// ---------------------------------------------------------------------------
// Utility macros (local copies of workspace/ir's blake3_newtype! pattern)
// ---------------------------------------------------------------------------

macro_rules! blake3_newtype {
    ($(#[$meta:meta])* $name:ident, $dbg:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[repr(transparent)]
        pub struct $name(ContentBlake3);

        impl $name {
            #[inline]
            pub const fn from_content_blake3(inner: ContentBlake3) -> Self { Self(inner) }
            #[inline]
            pub const fn from_raw(bytes: [u8; 32]) -> Self {
                Self(ContentBlake3::from_raw(bytes))
            }
            #[inline]
            pub fn from_domain(domain: &str, preimage: &[u8]) -> Self {
                Self(ContentBlake3::from_domain(domain, preimage))
            }
            #[inline]
            pub const fn content_blake3(&self) -> ContentBlake3 { self.0 }
            #[inline]
            pub const fn as_bytes(&self) -> &[u8; 32] { self.0.as_bytes() }
            #[inline]
            pub fn to_hex(&self) -> String { self.0.to_hex() }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}:{}…", $dbg, &self.to_hex()[..12])
            }
        }
    };
}

// ---------------------------------------------------------------------------
// ChangeSetFingerprint
// ---------------------------------------------------------------------------

blake3_newtype!(
    /// Equality-only fingerprint of an applied change set (v1: BLAKE3 of the
    /// sorted `ChangeId` bytes). Sufficient for tip *equality*, NOT for set
    /// reconciliation on its own (design Issue 17).
    ChangeSetFingerprint, "cset"
);

impl ChangeSetFingerprint {
    /// Domain tag for the v1 change-set fingerprint.
    pub const DOMAIN: &'static str = "nudox.cset.v1";

    /// Fingerprint of an applied change set: `blake3(DOMAIN || sort(id bytes))`.
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

// ---------------------------------------------------------------------------
// LinkDomainKey
// ---------------------------------------------------------------------------

/// Partition key for an undirected link, order-independent in its endpoints.
///
/// `blake3("nudox.link.v1" || lo.canonical || hi.canonical || u16le(lo_kind) ||
/// u16le(hi_kind))` where `(lo, hi)` is the pair of [`StableRef`] endpoints
/// sorted by canonical bytes, each paired with its own kind discriminant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct LinkDomainKey(ContentBlake3);

impl LinkDomainKey {
    pub const DOMAIN: &'static str = "nudox.link.v1";

    /// Compute the undirected key for `(a, kind_a)` — `(b, kind_b)`.
    pub fn from_link(a: &StableRef, b: &StableRef, kind_a: u16, kind_b: u16) -> Self {
        let a_bytes = a.canonical_bytes();
        let b_bytes = b.canonical_bytes();
        let (lo, lo_k, hi, hi_k) = if a_bytes <= b_bytes {
            (&a_bytes, kind_a, &b_bytes, kind_b)
        } else {
            (&b_bytes, kind_b, &a_bytes, kind_a)
        };
        let mut preimage = Vec::with_capacity(lo.len() + hi.len() + 4);
        preimage.extend_from_slice(lo);
        preimage.extend_from_slice(hi);
        preimage.extend_from_slice(&lo_k.to_le_bytes());
        preimage.extend_from_slice(&hi_k.to_le_bytes());
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

// ---------------------------------------------------------------------------
// LinkRecord — format-level undirected link for archive + diff engine
// ---------------------------------------------------------------------------

/// An undirected link between two [`StableRef`] endpoints, as stored in the
/// archive binary format and used by the structural diff engine.
///
/// Distinct from [`ir::relation::Relation`]: `LinkRecord` is the frozen
/// format-layer representation (undirected, keyed by `KindDiscriminant`);
/// `Relation` is the semantic layer (directed, keyed by `ReferenceKind`).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LinkRecord {
    /// The first endpoint.
    pub a: StableRef,
    /// The second endpoint.
    pub b: StableRef,
    /// The kind discriminant of the `a` endpoint.
    pub kind_a: KindDiscriminant,
    /// The kind discriminant of the `b` endpoint.
    pub kind_b: KindDiscriminant,
}

impl LinkRecord {
    /// Compute the [`LinkDomainKey`] for this record.
    pub fn domain_key(&self) -> LinkDomainKey {
        LinkDomainKey::from_link(&self.a, &self.b, self.kind_a.as_u16(), self.kind_b.as_u16())
    }
}

// ---------------------------------------------------------------------------
// Archive-specific index handles
// ---------------------------------------------------------------------------

/// An index into a package's dense entry arena, assigned during sealing.
/// Not stable across seal operations; used within one archive only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct ArenaIdx(pub u32);

/// A handle into the per-arena string interner (compact, 4-byte, Copy).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct StrId(pub u32);

/// The first 4 bytes (LE u32) of `blake3("nudox.tyskel.v1" || skeleton_bytes)`.
///
/// Used as a cheap structural type-equality filter before comparing full
/// skeletons.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct TypeFingerprintId(pub u32);

// ---------------------------------------------------------------------------
// sig_key — function signature key for continuity matching
// ---------------------------------------------------------------------------

/// Compute a stable, content-addressed key for a function signature from
/// the local wire types (`ParamWire`, `FnSigFlags`).
///
/// Used by the diff engine and the old continuity matcher (the new
/// `nudox_ir::continuity::resolve` uses `function_sig_key` internally on
/// `Kind::Function`). The domain is shared with workspace/ir's `sig_key` so
/// keys computed here are compatible with keys computed there.
pub fn sig_key(
    inputs: &[crate::wire::ParamWire],
    outputs: &[crate::wire::ParamWire],
    sig: &crate::wire::FnSigFlags,
) -> ContentBlake3 {
    let bytes = postcard::to_allocvec(&(inputs, outputs, sig))
        .expect("sig_key postcard serialization is infallible");
    ContentBlake3::from_domain("nudox.sigkey.v1", &bytes)
}

// ---------------------------------------------------------------------------
// type_wire_skeleton / type_fingerprint — archive index helpers
// ---------------------------------------------------------------------------

/// Compute a deterministic byte skeleton for a [`crate::wire::TypeWire`] for
/// use in the `TypeSkeletonIndex` archive section.
pub fn type_wire_skeleton(ty: &crate::wire::TypeWire) -> Vec<u8> {
    postcard::to_allocvec(ty).expect("type_wire_skeleton postcard serialization is infallible")
}

/// Derive a [`TypeFingerprintId`] from the first 4 bytes of
/// `blake3("nudox.tyskel.v1" || skeleton_bytes)`.
pub fn type_fingerprint(ty: &crate::wire::TypeWire) -> TypeFingerprintId {
    let skel = type_wire_skeleton(ty);
    let hash = ContentBlake3::from_domain("nudox.tyskel.v1", &skel);
    let first4 = u32::from_le_bytes(hash.as_bytes()[..4].try_into().unwrap());
    TypeFingerprintId(first4)
}
