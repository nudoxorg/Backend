//! Deterministic [`nudox_change::IntroId`] bootstrap from package + kind + path.
//!
//! # Bootstrap contract
//!
//! An `IntroId` is assigned **once**, when a symbol is first introduced, and
//! never changes even if the symbol is renamed. The deterministic derivation
//! ensures that two producers independently indexing the same package version
//! produce identical `IntroId`s for the same symbol.
//!
//! ## Preimage construction
//!
//! ```text
//! domain = "nudox.intro.v1"
//!
//! preimage =
//!   package.encode()              // encode_str(ecosystem) || encode_str(name)
//!   || u16le(kind_disc as u16)    // frozen KindDiscriminant wire value
//!   || encode_segments(segments)  // u32le(count) || each encode_str(seg)  root→leaf
//!   || encode_str(name)           // the leaf declaration name
//!   || disambiguator_bytes        // empty | sig skeleton | span u64le*2
//! ```
//!
//! # Disambiguator
//!
//! When two symbols share the same package, kind, path segments, and name, the
//! `Disambiguator` breaks the tie:
//! - [`Disambiguator::None`] — no tie-breaking needed (the common case).
//! - [`Disambiguator::Overload`] — function-signature skeleton bytes (from
//!   [`crate::skeleton::function_signature_skeleton`]).
//! - [`Disambiguator::Span`] — source byte offsets (last resort for e.g. two
//!   anonymous types at different positions).

use nudox_change::encode::{encode_segments, encode_str, write_u16le};
use nudox_change::{IntroId, PackageLineageId};

use crate::kind::KindDiscriminant;

// ---------------------------------------------------------------------------
// Disambiguator
// ---------------------------------------------------------------------------

/// How to break ties when two symbols share the same qualified path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Disambiguator {
    /// No disambiguation needed; the qualified path is already unique.
    None,
    /// Function overload: the type-skeleton bytes of the function signature
    /// (from [`crate::skeleton::function_signature_skeleton`]).
    Overload(Box<[u8]>),
    /// Source span: byte offsets of the declaration. Used as last resort when
    /// the skeleton is identical (e.g. two anonymous types at distinct positions).
    Span { start: u32, end: u32 },
}

impl Disambiguator {
    /// Serialize the disambiguator to bytes for inclusion in the `IntroId`
    /// preimage.
    ///
    /// - `None` → empty (0 bytes)
    /// - `Overload(bytes)` → the raw skeleton bytes
    /// - `Span { start, end }` → `start as u64 le || end as u64 le` (16 bytes)
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Disambiguator::None => Vec::new(),
            Disambiguator::Overload(bytes) => bytes.to_vec(),
            Disambiguator::Span { start, end } => {
                let mut out = Vec::with_capacity(16);
                out.extend_from_slice(&(*start as u64).to_le_bytes());
                out.extend_from_slice(&(*end as u64).to_le_bytes());
                out
            }
        }
    }
}

// ---------------------------------------------------------------------------
// bootstrap_intro_id
// ---------------------------------------------------------------------------

/// Derive the forever-stable [`IntroId`] for a symbol introduction.
///
/// # Parameters
///
/// - `package` — the package lineage (ecosystem + name). Encodes as
///   `encode_str(ecosystem) || encode_str(name)`.
/// - `kind_disc` — the entry's kind discriminant, encoded as `u16le`.
/// - `segments` — ancestor qualified names from the package root to (but not
///   including) the symbol itself, root-first. **The parent chain is mandatory**
///   for non-root symbols; omitting it will produce a different (wrong) `IntroId`.
/// - `name` — the leaf declaration name.
/// - `disambiguator` — tie-breaker for overloaded symbols or same-name types.
///
/// # Stability guarantee
///
/// The preimage format is frozen at `"nudox.intro.v1"`. Any change to the
/// encoding requires a new domain string and a database migration.
pub fn bootstrap_intro_id(
    package: &PackageLineageId,
    kind_disc: KindDiscriminant,
    segments: &[&str],
    name: &str,
    disambiguator: &Disambiguator,
) -> IntroId {
    let mut preimage = Vec::new();

    // 1. Package lineage bytes.
    package.encode(&mut preimage);

    // 2. Kind discriminant as u16le.
    write_u16le(&mut preimage, kind_disc.as_u16());

    // 3. Ancestor segments (root → parent), count-prefixed.
    encode_segments(&mut preimage, segments);

    // 4. Leaf name.
    encode_str(&mut preimage, name);

    // 5. Disambiguator bytes (may be empty).
    preimage.extend_from_slice(&disambiguator.to_bytes());

    IntroId::from_domain("nudox.intro.v1", &preimage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_change::{EcosystemId, PackageLineageId, PackageName};

    fn test_pkg() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Function;
        let segments = &["MyModule"];
        let name = "do_thing";
        let disamb = Disambiguator::None;

        let id1 = bootstrap_intro_id(&pkg, disc, segments, name, &disamb);
        let id2 = bootstrap_intro_id(&pkg, disc, segments, name, &disamb);
        assert_eq!(id1, id2);
    }

    #[test]
    fn different_names_produce_different_ids() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Function;
        let segments: &[&str] = &[];
        let id_a = bootstrap_intro_id(&pkg, disc, segments, "alpha", &Disambiguator::None);
        let id_b = bootstrap_intro_id(&pkg, disc, segments, "beta", &Disambiguator::None);
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn different_kinds_produce_different_ids() {
        let pkg = test_pkg();
        let segments: &[&str] = &[];
        let name = "Foo";
        let id_module =
            bootstrap_intro_id(&pkg, KindDiscriminant::Module, segments, name, &Disambiguator::None);
        let id_record =
            bootstrap_intro_id(&pkg, KindDiscriminant::Record, segments, name, &Disambiguator::None);
        assert_ne!(id_module, id_record);
    }

    #[test]
    fn overload_disambiguator_changes_id() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Function;
        let segments: &[&str] = &[];
        let name = "overloaded";
        let id_none =
            bootstrap_intro_id(&pkg, disc, segments, name, &Disambiguator::None);
        let id_overload = bootstrap_intro_id(
            &pkg,
            disc,
            segments,
            name,
            &Disambiguator::Overload(Box::new([0x01, 0xFF, 0x10])),
        );
        assert_ne!(id_none, id_overload);
    }

    #[test]
    fn span_disambiguator_to_bytes() {
        let d = Disambiguator::Span { start: 10, end: 20 };
        let bytes = d.to_bytes();
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[0..8], &10u64.to_le_bytes());
        assert_eq!(&bytes[8..16], &20u64.to_le_bytes());
    }

    #[test]
    fn none_disambiguator_is_empty() {
        assert!(Disambiguator::None.to_bytes().is_empty());
    }

    #[test]
    fn parent_segments_affect_id() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Function;
        let name = "do_thing";
        let id_root =
            bootstrap_intro_id(&pkg, disc, &[], name, &Disambiguator::None);
        let id_nested =
            bootstrap_intro_id(&pkg, disc, &["Parent"], name, &Disambiguator::None);
        assert_ne!(id_root, id_nested);
    }
}
