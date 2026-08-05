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
/// `v3` — nudox-ir's own, bug-fixed successor to `workspace/ir`'s
/// `nudox.intro.v2`. The disambiguator now folds in generics/wheres/negativity,
/// so the preimage (and therefore the digest) differs from v2 for overloads and
/// impls; a distinct domain makes that a clean, explicit version boundary
/// rather than a silent divergence.
pub const INTRO_DOMAIN: &str = "nudox.intro.v3";

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
    /// blake3("nudox.intro.v3" ‖ encode_str("cargo") ‖ encode_str("demo") ‖
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
            "973eee7597d063c75231ee760a2239d58a761660098a2b589f524ab6a994f962",
            "IntroId preimage layout regression (update only with a FORMAT_VERSION bump)"
        );
    }
}
