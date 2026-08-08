//! Deterministic `IntroId` minting — the *bootstrap* that assigns a symbol its
//! forever-stable identity from producer-visible facts alone (no prior state).
//!
//! An [`IntroId`] is `blake3(INTRO_DOMAIN ‖ preimage)` where the preimage is
//! the canonical encoding of `(package, kind, ancestor-path, leaf-name,
//! disambiguator)`. The [`Disambiguator`] is what keeps sibling declarations
//! that share `(kind, path, name)` — overloads, multiple impls — distinct; its
//! payload is a structural [`skeleton`](crate::skeleton) that (unlike
//! `workspace/ir`) includes generics, where-clauses, and impl negativity,
//! closing the collision bug.

use crate::{
    change::{
        IntroId, PackageLineageId,
        encode::{encode_segments, encode_str, write_u16le, write_u32le, write_u64le},
    },
    kind::KindDiscriminant,
};

/// Domain tag for the IntroId preimage.
///
/// `v4` — the cross-package-reference boundary.
///
/// Two changes move digests relative to `v3`, and the bump exists so that
/// divergence is an explicit version boundary rather than a silent one:
///
/// 1. [`skeleton::ref_`](crate::skeleton) used to encode *every* cross-package
///    reference as the single placeholder byte `0x00` (it had no way to name
///    one). It now encodes `0x02` plus the reference's canonical key bytes. The
///    `TraitImpl` disambiguator is applied to **every** impl unconditionally, so
///    this moves the `IntroId` of every impl whose trait or self type mentions a
///    foreign type — in practice, most trait impls in the corpus.
/// 2. [`Disambiguator::Ordinal`] is a new tag. Existing tags `0..=3` are
///    untouched, so an entry that neither collides nor names a foreign type
///    keeps a v3-shaped preimage — but it is hashed under a new domain and its
///    digest still moves.
///
/// **Every persisted `IntroId` in the corpus is invalidated by this bump.**
/// Manifest stamps, archives and any stored table must be re-derived once.
pub const INTRO_DOMAIN: &str = "nudox.intro.v4";

/// Collision-scoped disambiguator selected per §4.3: `None` for the common
/// unique case (so a unique name's id is signature-stable), a signature
/// skeleton for overload sets, a trait-impl skeleton for impls, or a source
/// span as a last resort for other same-key collisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disambiguator {
    /// The unique, signature-stable case.
    None,
    /// One of ≥2 functions sharing `(kind, path, name)`: the signature
    /// skeleton.
    FnOverload(Box<[u8]>),
    /// An impl block: the `(trait, self, generics, wheres, negative, blanket)`
    /// skeleton.
    TraitImpl(Box<[u8]>),
    /// A non-function collision with no better key: the declaration's byte
    /// span.
    Span { start: usize, end: usize },

    /// Last resort: the entry's ordinal among declarations that minted the
    /// *same* id under every structural rule above, plus its span.
    ///
    /// # Why this exists
    ///
    /// Every other disambiguator is a function of the declaration's own
    /// content, and this one is not — so its use is *reported*
    /// (`SealReport::forced`) rather than silent, and it is only ever reached
    /// after `Span` has already failed.
    ///
    /// It exists because losing the declaration is strictly worse. Java's
    /// oracle reports `span: 0..line` (a line number, not byte offsets), so two
    /// overloads declared on one line — routine in C# and TypeScript, and
    /// possible in Java — are identical under `Span`. Without a terminal tier
    /// the escalation ladder ends in either a silent overwrite (what shipped) or
    /// a hard failure that takes the whole package offline.
    ///
    /// The cost, stated plainly: an `Ordinal` id is not stable across a
    /// producer reordering its output. A report is the mitigation; it is not
    /// stability.
    Ordinal {
        span_start: usize,
        span_end: usize,
        index: u32,
    },
}

impl Disambiguator {
    /// Canonical bytes: a leading tag (`0..=3`) then the variant payload
    /// (length-prefixed skeleton, or two `u64le` for a span).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Disambiguator::None => out.push(0),
            Disambiguator::FnOverload(sk) => {
                out.push(1);
                write_u32le(&mut out, sk.len() as u32);
                out.extend_from_slice(sk);
            }
            Disambiguator::TraitImpl(sk) => {
                out.push(2);
                write_u32le(&mut out, sk.len() as u32);
                out.extend_from_slice(sk);
            }
            Disambiguator::Span { start, end } => {
                out.push(3);
                write_u64le(&mut out, *start as u64);
                write_u64le(&mut out, *end as u64);
            }
            Disambiguator::Ordinal {
                span_start,
                span_end,
                index,
            } => {
                out.push(4);
                write_u64le(&mut out, *span_start as u64);
                write_u64le(&mut out, *span_end as u64);
                write_u32le(&mut out, *index);
            }
        }
        out
    }
}

/// Mint the forever-stable [`IntroId`] for a declaration.
///
/// `segments` is the ancestor name path, root-first (e.g. `["mymod", "MyType"]`
/// for a method on `MyType`). The preimage layout is frozen (see
/// [`INTRO_DOMAIN`]).
pub fn bootstrap_intro_id(
    package: &PackageLineageId,
    kind_disc: KindDiscriminant,
    segments: &[&str],
    name: &str,
    disambiguator: &Disambiguator,
) -> IntroId {
    let mut preimage = Vec::new();
    package.encode(&mut preimage); // encode_str(ecosystem) ‖ encode_str(name)
    write_u16le(&mut preimage, kind_disc.as_u16());
    encode_segments(&mut preimage, segments);
    encode_str(&mut preimage, name);
    preimage.extend_from_slice(&disambiguator.to_bytes());
    IntroId::from_domain(INTRO_DOMAIN, &preimage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{EcosystemId, PackageName};

    fn pkg() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo"))
    }

    /// Determinism + the collision-scoped contract: same inputs ⇒ same id;
    /// a different disambiguator ⇒ different id.
    #[test]
    fn deterministic_and_disambiguator_sensitive() {
        let a = bootstrap_intro_id(
            &pkg(),
            KindDiscriminant::Function,
            &["m"],
            "f",
            &Disambiguator::None,
        );
        let b = bootstrap_intro_id(
            &pkg(),
            KindDiscriminant::Function,
            &["m"],
            "f",
            &Disambiguator::None,
        );
        assert_eq!(a, b, "same inputs must mint the same IntroId");

        let c = bootstrap_intro_id(
            &pkg(),
            KindDiscriminant::Function,
            &["m"],
            "f",
            &Disambiguator::FnOverload(Box::new([1, 2, 3])),
        );
        assert_ne!(a, c, "a different disambiguator must change the IntroId");
    }

    /// A unique-named function (None disambiguator) is signature-stable: its id
    /// does not depend on any skeleton. Distinct names differ.
    #[test]
    fn distinct_names_and_kinds_differ() {
        let f = bootstrap_intro_id(
            &pkg(),
            KindDiscriminant::Function,
            &[],
            "f",
            &Disambiguator::None,
        );
        let g = bootstrap_intro_id(
            &pkg(),
            KindDiscriminant::Function,
            &[],
            "g",
            &Disambiguator::None,
        );
        let r = bootstrap_intro_id(
            &pkg(),
            KindDiscriminant::Record,
            &[],
            "f",
            &Disambiguator::None,
        );
        assert_ne!(f, g);
        assert_ne!(f, r, "same name, different kind must differ");
    }

    /// GOLDEN: freeze the exact preimage layout. Independently reproducible as
    /// blake3("nudox.intro.v4" ‖ encode_str("cargo") ‖ encode_str("demo") ‖
    /// u16le(4) ‖ u32le(1) ‖ encode_str("m") ‖ encode_str("f") ‖ 0x00).
    #[test]
    fn bootstrap_golden() {
        let id = bootstrap_intro_id(
            &pkg(),
            KindDiscriminant::Function,
            &["m"],
            "f",
            &Disambiguator::None,
        );
        assert_eq!(
            id.to_hex(),
            "ebb5afd66f97c25b9dbf60c3deade5fd27a67438a5981ba482f8c153b47a0618",
            "IntroId preimage layout regression (update only with a FORMAT_VERSION bump)"
        );
    }
}
