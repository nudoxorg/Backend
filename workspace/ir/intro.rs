//! Deterministic [`crate::change::IntroId`] bootstrap from package + kind + path.
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

use crate::change::encode::{encode_segments, encode_str, write_u16le};
use crate::change::{ContentBlake3, IntroId, PackageLineageId};

use crate::kind::KindDiscriminant;
use crate::skeleton::{fnsig_flag_bytes, function_signature_skeleton};
use crate::wire::{FnSigFlags, ParamWire};

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

// ---------------------------------------------------------------------------
// DisambiguatorV2
// ---------------------------------------------------------------------------

/// Collision-scoped disambiguator for v2 `IntroId` preimages.
///
/// The case-selection logic (which variant to use, and the collision check) is
/// the **caller's** responsibility. This enum merely carries the bytes to include
/// in the preimage.
///
/// Byte encoding (frozen — never renumber/reorder):
/// - `None` → empty (0 bytes)
/// - `FnOverload(bytes)` → `0x01` ‖ bytes
/// - `Span { start, end }` → `0x02` ‖ `u64le(start)` ‖ `u64le(end)`
/// - `TraitImpl(bytes)` → `0x03` ‖ bytes
// frozen — never renumber/reorder variants
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DisambiguatorV2 {
    /// No disambiguation needed; the qualified path is unique within this generation.
    None,
    /// Function overload: signature skeleton bytes (from
    /// [`crate::skeleton::function_signature_skeleton`]).
    FnOverload(Box<[u8]>),
    /// Source span: byte offsets of the declaration. Last resort for same-key
    /// non-function entries (e.g. anonymous types at distinct positions).
    Span { start: u32, end: u32 },
    /// Trait impl: `(trait_ref, self_ty)` skeleton bytes (from
    /// [`crate::skeleton::trait_impl_skeleton`]).
    TraitImpl(Box<[u8]>),
}

impl DisambiguatorV2 {
    /// Serialize to bytes for inclusion in the `IntroId` preimage.
    ///
    /// - `None` → empty
    /// - `FnOverload(b)` → `0x01` ‖ `b`
    /// - `Span { s, e }` → `0x02` ‖ `u64le(s)` ‖ `u64le(e)` (16 bytes total payload)
    /// - `TraitImpl(b)` → `0x03` ‖ `b`
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            DisambiguatorV2::None => Vec::new(),
            DisambiguatorV2::FnOverload(bytes) => {
                let mut out = Vec::with_capacity(1 + bytes.len());
                out.push(0x01);
                out.extend_from_slice(bytes);
                out
            }
            DisambiguatorV2::Span { start, end } => {
                let mut out = Vec::with_capacity(17);
                out.push(0x02);
                out.extend_from_slice(&(*start as u64).to_le_bytes());
                out.extend_from_slice(&(*end as u64).to_le_bytes());
                out
            }
            DisambiguatorV2::TraitImpl(bytes) => {
                let mut out = Vec::with_capacity(1 + bytes.len());
                out.push(0x03);
                out.extend_from_slice(bytes);
                out
            }
        }
    }
}

// ---------------------------------------------------------------------------
// bootstrap_intro_id_v2
// ---------------------------------------------------------------------------

/// Derive the forever-stable [`IntroId`] for a symbol introduction (v2).
///
/// Domain: `"nudox.intro.v2"`. Preimage layout is identical to v1 except for
/// the domain tag and the [`DisambiguatorV2`] (collision-scoped, not
/// always-on — see §4.3 of the plan).
///
/// # Parameters
///
/// - `package` — the package lineage; encodes as `encode_str(ecosystem) || encode_str(name)`.
/// - `kind_disc` — the entry's kind discriminant, encoded as `u16le`.
/// - `segments` — ancestor names from the package root to (but not including) this symbol,
///   root-first. Mandatory for non-root symbols.
/// - `name` — the leaf declaration name.
/// - `disambiguator` — the collision-scoped tie-breaker; `None` is the common case.
///
/// # Note
///
/// The caller is responsible for the collision-scoped selection rule (§4.3): count
/// entries in this generation sharing `(kind_disc, segments, name)`, and choose the
/// appropriate variant. This function only hashes.
///
/// # Stability guarantee
///
/// The preimage format is frozen at `"nudox.intro.v2"`. Any change requires a new
/// domain string and a database migration.
pub fn bootstrap_intro_id_v2(
    package: &PackageLineageId,
    kind_disc: KindDiscriminant,
    segments: &[&str],
    name: &str,
    disambiguator: &DisambiguatorV2,
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

    IntroId::from_domain("nudox.intro.v2", &preimage)
}

// ---------------------------------------------------------------------------
// sig_key (§4.6)
// ---------------------------------------------------------------------------

/// Compute the `SigKey` for a function (§4.6).
///
/// ```text
/// SigKey = blake3("nudox.sigkey.v1"
///          ‖ function_signature_skeleton(inputs, outputs)
///          ‖ 0xFE
///          ‖ fnsig_flag_bytes(sig))
/// ```
///
/// Not stored in the VCS file (it is derivable); used by the surface projector
/// and the continuity matcher.
// frozen — domain string and byte layout must never change
pub fn sig_key(inputs: &[ParamWire], outputs: &[ParamWire], sig: &FnSigFlags) -> ContentBlake3 {
    let mut preimage = function_signature_skeleton(inputs, outputs);
    preimage.push(0xFE);
    preimage.extend_from_slice(&fnsig_flag_bytes(sig));
    ContentBlake3::from_domain("nudox.sigkey.v1", &preimage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{EcosystemId, PackageLineageId, PackageName};

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

    // -----------------------------------------------------------------------
    // bootstrap_intro_id_v2 tests
    // -----------------------------------------------------------------------

    #[test]
    fn v2_deterministic_for_same_inputs() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Function;
        let id1 = bootstrap_intro_id_v2(&pkg, disc, &["MyMod"], "foo", &DisambiguatorV2::None);
        let id2 = bootstrap_intro_id_v2(&pkg, disc, &["MyMod"], "foo", &DisambiguatorV2::None);
        assert_eq!(id1, id2);
    }

    #[test]
    fn v2_domain_differs_from_v1() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Function;
        let v1 = bootstrap_intro_id(&pkg, disc, &[], "foo", &Disambiguator::None);
        let v2 = bootstrap_intro_id_v2(&pkg, disc, &[], "foo", &DisambiguatorV2::None);
        assert_ne!(v1, v2, "v1 and v2 must produce different ids (different domain tags)");
    }

    #[test]
    fn v2_fn_overload_changes_id() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Function;
        let base = bootstrap_intro_id_v2(&pkg, disc, &[], "f", &DisambiguatorV2::None);
        let overload = bootstrap_intro_id_v2(
            &pkg,
            disc,
            &[],
            "f",
            &DisambiguatorV2::FnOverload(Box::new([0x01, 0xFF, 0x10])),
        );
        assert_ne!(base, overload);
    }

    #[test]
    fn v2_span_disambiguator_changes_id() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Record;
        let base = bootstrap_intro_id_v2(&pkg, disc, &[], "Anon", &DisambiguatorV2::None);
        let span = bootstrap_intro_id_v2(
            &pkg,
            disc,
            &[],
            "Anon",
            &DisambiguatorV2::Span { start: 10, end: 20 },
        );
        assert_ne!(base, span);
    }

    #[test]
    fn v2_trait_impl_changes_id() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Impl;
        let base = bootstrap_intro_id_v2(&pkg, disc, &[], "impl", &DisambiguatorV2::None);
        let with_impl = bootstrap_intro_id_v2(
            &pkg,
            disc,
            &[],
            "impl",
            &DisambiguatorV2::TraitImpl(Box::new([0x01, 0x10, 0xFF, 0x18])),
        );
        assert_ne!(base, with_impl);
    }

    #[test]
    fn v2_different_disambiguator_variants_differ() {
        let pkg = test_pkg();
        let disc = KindDiscriminant::Function;
        let bytes: Box<[u8]> = Box::new([0x01, 0xFF]);
        let overload = bootstrap_intro_id_v2(
            &pkg, disc, &[], "f", &DisambiguatorV2::FnOverload(bytes.clone()),
        );
        let trait_impl = bootstrap_intro_id_v2(
            &pkg, disc, &[], "f", &DisambiguatorV2::TraitImpl(bytes),
        );
        assert_ne!(overload, trait_impl, "FnOverload(x) and TraitImpl(x) must differ (different prefix bytes)");
    }

    #[test]
    fn v2_trait_impl_skeleton_disambiguator_is_deterministic() {
        use crate::skeleton::trait_impl_skeleton;
        use crate::wire::{TypeRefWire, TypeWire};
        use crate::change::IntroId;

        let intro = IntroId::from_raw([0xAB; 32]);
        let of = TypeRefWire::Same(intro);
        let self_ty = TypeWire::Never;

        let sk1 = trait_impl_skeleton(Some(&of), &self_ty);
        let sk2 = trait_impl_skeleton(Some(&of), &self_ty);
        assert_eq!(sk1, sk2, "trait_impl_skeleton must be deterministic");

        let pkg = test_pkg();
        let disc = KindDiscriminant::Impl;
        let id1 = bootstrap_intro_id_v2(
            &pkg, disc, &[], "impl",
            &DisambiguatorV2::TraitImpl(sk1.clone().into_boxed_slice()),
        );
        let id2 = bootstrap_intro_id_v2(
            &pkg, disc, &[], "impl",
            &DisambiguatorV2::TraitImpl(sk2.into_boxed_slice()),
        );
        assert_eq!(id1, id2);
    }

    #[test]
    fn v2_disambiguator_span_bytes_layout() {
        let d = DisambiguatorV2::Span { start: 10, end: 20 };
        let bytes = d.to_bytes();
        // prefix byte + u64le(10) + u64le(20) = 17 bytes
        assert_eq!(bytes.len(), 17);
        assert_eq!(bytes[0], 0x02);
        assert_eq!(&bytes[1..9], &10u64.to_le_bytes());
        assert_eq!(&bytes[9..17], &20u64.to_le_bytes());
    }

    #[test]
    fn v2_disambiguator_none_is_empty() {
        assert!(DisambiguatorV2::None.to_bytes().is_empty());
    }

    // -----------------------------------------------------------------------
    // sig_key tests
    // -----------------------------------------------------------------------

    #[test]
    fn sig_key_is_deterministic() {
        use crate::wire::{FnSigFlags, ParamWire, TypeRefWire};
        use crate::change::IntroId;

        let inputs = vec![ParamWire {
            name: Some("x".into()),
            ty: TypeRefWire::Same(IntroId::from_raw([0x01; 32])),
        }];
        let outputs: Vec<ParamWire> = vec![];
        let sig = FnSigFlags::default();

        let k1 = sig_key(&inputs, &outputs, &sig);
        let k2 = sig_key(&inputs, &outputs, &sig);
        assert_eq!(k1, k2);
    }

    #[test]
    fn sig_key_changes_on_param_type_change() {
        use crate::wire::{FnSigFlags, ParamWire, TypeRefWire};
        use crate::change::IntroId;

        let input_a = vec![ParamWire {
            name: None,
            ty: TypeRefWire::Same(IntroId::from_raw([0xAA; 32])),
        }];
        let input_b = vec![ParamWire {
            name: None,
            ty: TypeRefWire::Same(IntroId::from_raw([0xBB; 32])),
        }];
        let sig = FnSigFlags::default();
        let outputs: Vec<ParamWire> = vec![];

        let ka = sig_key(&input_a, &outputs, &sig);
        let kb = sig_key(&input_b, &outputs, &sig);
        assert_ne!(ka, kb);
    }

    #[test]
    fn sig_key_changes_on_sig_flag_change() {
        use crate::wire::{FnSigFlags, ParamWire};

        let inputs: Vec<ParamWire> = vec![];
        let outputs: Vec<ParamWire> = vec![];
        let normal = FnSigFlags::default();
        let async_ = FnSigFlags { is_async: true, ..Default::default() };

        let kn = sig_key(&inputs, &outputs, &normal);
        let ka = sig_key(&inputs, &outputs, &async_);
        assert_ne!(kn, ka);
    }
}
