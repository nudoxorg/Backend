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
/// `v5` — the type-lattice boundary.
///
/// `v4` was the cross-package-reference boundary, and both of its changes
/// remain in force: [`skeleton::ref_`](crate::skeleton) encodes a cross-package
/// reference as `0x02` plus the reference's canonical key bytes rather than the
/// single placeholder `0x00`, and [`Disambiguator::Ordinal`] exists as tag `4`.
/// What follows is what moved *since* v4.
///
/// Three changes landed independently, each of which invalidates persisted ids
/// on its own. They deliberately share **one** bump, so the corpus is
/// re-derived once rather than three times:
///
/// 1. **The type lattice split.** [`Type::Any`](crate::kinds::Type) was
///    narrowed to a genuine top type, and `Type::Unknown` was added carrying
///    one of seven [`UnknownType`](crate::kinds::UnknownType) reasons. In
///    skeleton bytes an absent type moved from opcode `0x09` to `0x18` plus a
///    reason opcode — plus the source spelling, for the three reasons that
///    carry one. Separately, a `Param`/`Field` slot that was `None` under
///    `opt_seq` is now `Some(..)`, moving its tag from `0x00` to `0x01`
///    followed by the encoded type. Java's `TypeMirror::Null` now lowers to
///    `Type::Never`, and Go's `byte` to `Type::U8`. Any entry whose signature
///    mentions one of these moves.
/// 2. **clang's unresolved named types.** `OracleType::Named` lowered to
///    `Type::TypeVar(name)`, which the skeleton encodes as the bare byte `0x0c`
///    with the name deliberately dropped for alpha-equivalence. It now lowers
///    to `Type::Unknown(UnresolvedExternal { name })`, which keeps the
///    spelling — so two overloads differing only in which external type they
///    name stop colliding.
/// 3. **Python's `TypeData::Unsupported`.** Now
///    `Type::Unknown(NoIrRepresentation { construct })` rather than the
///    dynamic-typing fallback.
///
/// `skeleton::tests::unknown_reason_opcodes_are_frozen` pins the reason
/// opcodes, which is what makes (1) a one-time boundary rather than recurring
/// drift: a later edit that reordered them would silently move every affected
/// id again.
///
/// **Every persisted `IntroId` in the corpus is invalidated by this bump.**
/// Manifest stamps, archives and any stored table must be re-derived once.
///
/// # This is a domain change, not a preimage-layout change
///
/// The byte layout [`bootstrap_intro_id`] writes is identical to v4's; what
/// moved is the *content* producers put into it, plus the domain prefix. So
/// [`FORMAT_VERSION`](crate::change::FORMAT_VERSION) — which versions the serde
/// representation of an `Entry` — is deliberately **not** bumped alongside it.
/// The two numbers answer different questions, and coupling them would force a
/// wire-incompatibility claim that is not true.
pub const INTRO_DOMAIN: &str = "nudox.intro.v5";

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
    /// blake3("nudox.intro.v5" ‖ encode_str("cargo") ‖ encode_str("demo") ‖
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
            "15933b42e3a94e1923644866f9c51fc37e20bd99d2e1c26c4fa514e3bfdb0095",
            "IntroId digest regression. Update only alongside a deliberate change to \
             INTRO_DOMAIN or to the preimage layout above — NOT with a FORMAT_VERSION \
             bump, which versions the serde shape of an `Entry` and does not reach this \
             digest (see INTRO_DOMAIN's docs)."
        );
    }
}
