//! Type-skeleton opcode encoder for structural type fingerprinting.
//!
//! The skeleton is a compact, deterministic byte sequence that represents the
//! *structural shape* of a type expression — deliberately excluding lifetimes,
//! mutability (except where needed for distinct pointer kinds), and other
//! semantically irrelevant attributes. Two types with identical skeletons are
//! considered structurally equivalent for the purpose of function-overload
//! disambiguation and [`crate::index::TypeFingerprintId`] caching.
//!
//! # Opcode table (stable; never renumber)
//!
//! | Opcode | Meaning                              |
//! |--------|--------------------------------------|
//! | `0x01` | Intro ref (same-package, 32 bytes)   |
//! | `0x02` | Foreign ref (StableRef canonical)    |
//! | `0x10` | SelfType                              |
//! | `0x11` | Primitive (subop byte follows)        |
//! | `0x12` | Tuple (u16le arity, elements)         |
//! | `0x13` | Slice (element skeleton follows)      |
//! | `0x14` | Array (u64le length, element)         |
//! | `0x15` | Union (u16le arity, elements)         |
//! | `0x16` | Intersection (u16le arity, elements)  |
//! | `0x17` | Never                                 |
//! | `0x18` | Any                                   |
//!
//! ## Primitive subops
//!
//! | Subop  | Meaning                              |
//! |--------|--------------------------------------|
//! | `0x01` | Integer (signed byte + width)        |
//! | `0x02` | Float (width)                        |
//! | `0x03` | Bool                                  |
//! | `0x04` | Char                                  |
//! | `0x05` | Str                                   |
//! | `0x06` | MutPointer (target skeleton)          |
//! | `0x07` | ConstPointer (target skeleton)        |
//! | `0x08` | Reference (mutable byte + target)     |
//! | `0x09` | Builtin (u32le len + UTF-8 bytes)     |
//!
//! ## Width encoding
//!
//! | Byte   | Meaning                              |
//! |--------|--------------------------------------|
//! | `0x00` | Arch (pointer-sized)                 |
//! | `0x01` | then u32le = Fixed(n) bits            |

use nudox_change::encode::encode_str;
use nudox_change::ContentBlake3;

use crate::index::TypeFingerprintId;
use crate::wire::{FnSigFlags, ParamWire, PrimitiveWire, SelfKind, TypeRefWire, TypeWire, WidthWire};

// ---------------------------------------------------------------------------
// Width encoding
// ---------------------------------------------------------------------------

/// Encode a [`WidthWire`] into `out`.
fn encode_width(w: &WidthWire, out: &mut Vec<u8>) {
    match w {
        WidthWire::Arch => out.push(0x00),
        WidthWire::Fixed(n) => {
            out.push(0x01);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

// ---------------------------------------------------------------------------
// TypeRefWire skeleton
// ---------------------------------------------------------------------------

/// Encode a [`TypeRefWire`] into the skeleton byte stream.
///
/// - `Same(intro)` → `0x01` + 32 raw intro bytes.
/// - `Foreign(stable_ref)` → `0x02` + canonical stable-ref bytes.
pub fn type_skeleton(ty: &TypeRefWire, out: &mut Vec<u8>) {
    match ty {
        TypeRefWire::Same(intro_id) => {
            out.push(0x01);
            out.extend_from_slice(intro_id.as_bytes());
        }
        TypeRefWire::Foreign(stable_ref) => {
            out.push(0x02);
            stable_ref.encode(out);
        }
    }
}

// ---------------------------------------------------------------------------
// TypeWire skeleton
// ---------------------------------------------------------------------------

/// Encode a [`TypeWire`] directly into the skeleton byte stream.
pub fn type_wire_skeleton(ty: &TypeWire, out: &mut Vec<u8>) {
    match ty {
        TypeWire::SelfType => {
            out.push(0x10);
        }
        TypeWire::Primitive(prim) => {
            out.push(0x11);
            encode_primitive(prim, out);
        }
        TypeWire::Tuple(elems) => {
            out.push(0x12);
            let arity = u16::try_from(elems.len()).expect("tuple arity exceeds u16::MAX");
            out.extend_from_slice(&arity.to_le_bytes());
            for elem in elems.iter() {
                type_skeleton(elem, out);
            }
        }
        TypeWire::Slice(elem) => {
            out.push(0x13);
            type_skeleton(elem, out);
        }
        TypeWire::Array { ty: elem, length } => {
            out.push(0x14);
            out.extend_from_slice(&length.to_le_bytes());
            type_skeleton(elem, out);
        }
        TypeWire::Union(elems) => {
            out.push(0x15);
            let arity = u16::try_from(elems.len()).expect("union arity exceeds u16::MAX");
            out.extend_from_slice(&arity.to_le_bytes());
            for elem in elems.iter() {
                type_skeleton(elem, out);
            }
        }
        TypeWire::Intersection(elems) => {
            out.push(0x16);
            let arity = u16::try_from(elems.len()).expect("intersection arity exceeds u16::MAX");
            out.extend_from_slice(&arity.to_le_bytes());
            for elem in elems.iter() {
                type_skeleton(elem, out);
            }
        }
        TypeWire::Never => {
            out.push(0x17);
        }
        TypeWire::Any => {
            out.push(0x18);
        }
    }
}

// ---------------------------------------------------------------------------
// Primitive encoding
// ---------------------------------------------------------------------------

/// Encode a [`PrimitiveWire`] as a primitive subop byte + data.
///
/// Note: `Reference.lifetime` is intentionally omitted from the skeleton —
/// structural type identity is independent of lifetime names.
fn encode_primitive(prim: &PrimitiveWire, out: &mut Vec<u8>) {
    match prim {
        PrimitiveWire::Integer { signed, width } => {
            out.push(0x01);
            out.push(*signed as u8);
            encode_width(width, out);
        }
        PrimitiveWire::Float(width) => {
            out.push(0x02);
            encode_width(width, out);
        }
        PrimitiveWire::Bool => out.push(0x03),
        PrimitiveWire::Char => out.push(0x04),
        PrimitiveWire::Str => out.push(0x05),
        PrimitiveWire::MutPointer(target) => {
            out.push(0x06);
            type_skeleton(target, out);
        }
        PrimitiveWire::ConstPointer(target) => {
            out.push(0x07);
            type_skeleton(target, out);
        }
        PrimitiveWire::Reference { lifetime: _, mutable, ty } => {
            // Lifetime is NOT in the skeleton — structural-only.
            out.push(0x08);
            out.push(*mutable as u8);
            type_skeleton(ty, out);
        }
        PrimitiveWire::Builtin(name) => {
            out.push(0x09);
            let bytes = name.as_bytes();
            let len = u32::try_from(bytes.len()).expect("builtin name exceeds u32::MAX bytes");
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(bytes);
        }
    }
}

// ---------------------------------------------------------------------------
// TypeFingerprintId
// ---------------------------------------------------------------------------

/// Compute a [`TypeFingerprintId`] from a completed skeleton byte sequence.
///
/// Uses the first 4 bytes of `blake3("nudox.tyskel.v1" || skeleton)` as a
/// `u32` little-endian.
pub fn type_fingerprint(skeleton_bytes: &[u8]) -> TypeFingerprintId {
    let hash = ContentBlake3::from_domain("nudox.tyskel.v1", skeleton_bytes);
    let bytes = hash.as_bytes();
    let v = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    TypeFingerprintId(v)
}

// ---------------------------------------------------------------------------
// function_signature_skeleton
// ---------------------------------------------------------------------------

/// Compute the signature skeleton for a function with the given input and
/// output parameters.
///
/// Each parameter's type is encoded as a `TypeRefWire` skeleton. The separator
/// byte `0xFF` appears between the end of input params and the start of output
/// params, making `f(a)->b` distinguishable from `f()->a,b`.
///
/// Parameters with unknown types (i.e. `ParamWire::ty` absent from some
/// producers) are skipped (no bytes emitted for them). A parameter's *name*
/// is never included in the skeleton.
pub fn function_signature_skeleton(inputs: &[ParamWire], outputs: &[ParamWire]) -> Vec<u8> {
    let mut out = Vec::new();
    for param in inputs {
        type_skeleton(&param.ty, &mut out);
    }
    out.push(0xFF); // separator
    for param in outputs {
        type_skeleton(&param.ty, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// trait_impl_skeleton (§4.3)
// ---------------------------------------------------------------------------

/// §4.3: 2-tuple skeleton of `(trait_ref, self_ty)` for `TraitImpl`
/// disambiguation.
///
/// Encoding (frozen — never change):
/// - If `of` is `Some(tr)`: `type_skeleton(tr)` bytes.
/// - If `of` is `None` (inherent impl): single `0x00` marker byte.
/// - `0xFF` separator.
/// - `type_wire_skeleton(self_ty)` bytes.
pub fn trait_impl_skeleton(of: Option<&TypeRefWire>, self_ty: &TypeWire) -> Vec<u8> {
    let mut out = Vec::new();
    match of {
        Some(tr) => type_skeleton(tr, &mut out),
        None => out.push(0x00),
    }
    out.push(0xFF); // separator
    type_wire_skeleton(self_ty, &mut out);
    out
}

// ---------------------------------------------------------------------------
// fnsig_flag_bytes (§4.6)
// ---------------------------------------------------------------------------

/// Canonical byte encoding of [`FnSigFlags`] for the `SigKey` preimage (§4.6).
///
/// Encoding order (frozen — never change):
/// 1. `self_kind` tag byte: `None=0`, `Value=1`, `Ref=2`, `RefMut=3`, `Arbitrary=4`
///    followed by `type_skeleton` of the arbitrary receiver type.
/// 2. One byte each for `is_async`, `is_const`, `is_unsafe` (0 or 1).
/// 3. ABI: `0x00` if `None`, else `0x01` followed by `encode_str`-style bytes.
/// 4. One byte each for `variadic`, `defaulted` (0 or 1).
pub fn fnsig_flag_bytes(sig: &FnSigFlags) -> Vec<u8> {
    let mut out = Vec::new();

    // 1. self_kind
    match &sig.self_kind {
        SelfKind::None => out.push(0),
        SelfKind::Value => out.push(1),
        SelfKind::Ref => out.push(2),
        SelfKind::RefMut => out.push(3),
        SelfKind::Arbitrary(tr) => {
            out.push(4);
            type_skeleton(tr, &mut out);
        }
    }

    // 2. bool flags
    out.push(sig.is_async as u8);
    out.push(sig.is_const as u8);
    out.push(sig.is_unsafe as u8);

    // 3. ABI
    match &sig.abi {
        None => out.push(0x00),
        Some(abi) => {
            out.push(0x01);
            encode_str(&mut out, abi);
        }
    }

    // 4. variadic / defaulted
    out.push(sig.variadic as u8);
    out.push(sig.defaulted as u8);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_change::IntroId;

    fn dummy_intro() -> IntroId {
        IntroId::from_raw([0u8; 32])
    }

    #[test]
    fn self_type_opcode() {
        let mut out = Vec::new();
        type_wire_skeleton(&TypeWire::SelfType, &mut out);
        assert_eq!(out, vec![0x10]);
    }

    #[test]
    fn never_and_any() {
        let mut out = Vec::new();
        type_wire_skeleton(&TypeWire::Never, &mut out);
        assert_eq!(out, vec![0x17]);
        out.clear();
        type_wire_skeleton(&TypeWire::Any, &mut out);
        assert_eq!(out, vec![0x18]);
    }

    #[test]
    fn bool_primitive() {
        let mut out = Vec::new();
        type_wire_skeleton(&TypeWire::Primitive(PrimitiveWire::Bool), &mut out);
        assert_eq!(out, vec![0x11, 0x03]);
    }

    #[test]
    fn intro_ref_skeleton() {
        let intro = dummy_intro();
        let mut out = Vec::new();
        type_skeleton(&TypeRefWire::Same(intro), &mut out);
        assert_eq!(out[0], 0x01);
        assert_eq!(&out[1..], intro.as_bytes());
    }

    #[test]
    fn tuple_arity_encoded() {
        let elem = TypeRefWire::Same(dummy_intro());
        let tuple = TypeWire::Tuple(Box::new([elem.clone(), elem.clone()]));
        let mut out = Vec::new();
        type_wire_skeleton(&tuple, &mut out);
        assert_eq!(out[0], 0x12);
        // u16le arity = 2
        assert_eq!(&out[1..3], &2u16.to_le_bytes());
    }

    #[test]
    fn fingerprint_is_4_bytes() {
        let skel = vec![0x10u8]; // SelfType
        let fp = type_fingerprint(&skel);
        // just verify it produces a u32 without panicking
        let _ = fp.0;
    }

    /// **Golden pins** for the opcode walk (design: "Golden vectors committed"
    /// under the type-skeleton section). Skeleton bytes and the derived
    /// fingerprint are *durable identity* — they feed overload disambiguators
    /// and therefore `IntroId` preimages. Any encoding drift breaks identity of
    /// already-published symbols, so an intentional change requires a new
    /// domain tag (`nudox.tyskel.v2`), never an edit of these vectors.
    #[test]
    fn skeleton_golden_vectors() {
        let intro = IntroId::from_raw([0xAB; 32]);

        // &mut [i32; 3] as: Array { ty: Same(intro), length: 3 }
        let mut out = Vec::new();
        type_wire_skeleton(&TypeWire::Array { ty: Box::new(TypeRefWire::Same(intro)), length: 3 }, &mut out);
        let mut expected = vec![0x14];
        expected.extend_from_slice(&3u64.to_le_bytes());
        expected.push(0x01);
        expected.extend_from_slice(&[0xAB; 32]);
        assert_eq!(out, expected, "Array opcode layout drifted");

        // Integer { signed: true, width: Fixed(32) }
        let mut out = Vec::new();
        type_wire_skeleton(
            &TypeWire::Primitive(PrimitiveWire::Integer { signed: true, width: WidthWire::Fixed(32) }),
            &mut out,
        );
        assert_eq!(out, vec![0x11, 0x01, 0x01, 0x01, 32, 0, 0, 0], "Integer opcode layout drifted");

        // Reference { lifetime ignored, mutable, ty }
        let mut out = Vec::new();
        type_wire_skeleton(
            &TypeWire::Primitive(PrimitiveWire::Reference {
                lifetime: Some("'a".to_owned()),
                mutable: true,
                ty: Box::new(TypeRefWire::Same(intro)),
            }),
            &mut out,
        );
        let mut expected = vec![0x11, 0x08, 0x01, 0x01];
        expected.extend_from_slice(&[0xAB; 32]);
        assert_eq!(out, expected, "Reference opcode layout drifted (lifetime must be excluded)");

        // Builtin("String") — name is structural, length-prefixed.
        let mut out = Vec::new();
        type_wire_skeleton(&TypeWire::Primitive(PrimitiveWire::Builtin("String".to_owned())), &mut out);
        let mut expected = vec![0x11, 0x09];
        expected.extend_from_slice(&6u32.to_le_bytes());
        expected.extend_from_slice(b"String");
        assert_eq!(out, expected, "Builtin opcode layout drifted");
    }

    /// **Golden pin** of the fingerprint derivation (`nudox.tyskel.v1` domain,
    /// first 4 LE bytes). See `skeleton_golden_vectors` for why.
    #[test]
    fn fingerprint_golden_pin() {
        let fp = type_fingerprint(&[0x10]); // SelfType
        assert_eq!(fp.0, 0x9001_4699, "tyskel fingerprint derivation drifted");
    }

    #[test]
    fn signature_separator() {
        let inputs = vec![ParamWire { name: None, ty: TypeRefWire::Same(dummy_intro()) }];
        let outputs: Vec<ParamWire> = vec![];
        let bytes = function_signature_skeleton(&inputs, &outputs);
        // Should have intro bytes + 0xFF separator
        assert!(bytes.contains(&0xFF));
    }

    // -----------------------------------------------------------------------
    // trait_impl_skeleton tests
    // -----------------------------------------------------------------------

    #[test]
    fn trait_impl_skeleton_inherent_marker() {
        let self_ty = TypeWire::SelfType;
        let bytes = trait_impl_skeleton(None, &self_ty);
        // First byte = 0x00 (inherent impl marker), then 0xFF separator, then 0x10 (SelfType)
        assert_eq!(bytes[0], 0x00, "inherent impl must start with 0x00 marker");
        assert_eq!(bytes[1], 0xFF, "separator must follow marker");
        assert_eq!(bytes[2], 0x10, "SelfType opcode must follow separator");
        assert_eq!(bytes.len(), 3);
    }

    #[test]
    fn trait_impl_skeleton_with_trait() {
        let intro = dummy_intro();
        let of = TypeRefWire::Same(intro);
        let self_ty = TypeWire::Never;
        let bytes = trait_impl_skeleton(Some(&of), &self_ty);
        // First byte = 0x01 (Same intro opcode), then 32 intro bytes, 0xFF, 0x17 (Never)
        assert_eq!(bytes[0], 0x01);
        assert_eq!(&bytes[1..33], intro.as_bytes());
        assert_eq!(bytes[33], 0xFF);
        assert_eq!(bytes[34], 0x17, "Never opcode");
    }

    #[test]
    fn trait_impl_skeleton_deterministic() {
        let intro = dummy_intro();
        let of = TypeRefWire::Same(intro);
        let self_ty = TypeWire::Any;
        let b1 = trait_impl_skeleton(Some(&of), &self_ty);
        let b2 = trait_impl_skeleton(Some(&of), &self_ty);
        assert_eq!(b1, b2);
    }

    #[test]
    fn trait_impl_skeleton_none_vs_some_differ() {
        let intro = dummy_intro();
        let of = TypeRefWire::Same(intro);
        let self_ty = TypeWire::SelfType;
        let inherent = trait_impl_skeleton(None, &self_ty);
        let trait_ = trait_impl_skeleton(Some(&of), &self_ty);
        assert_ne!(inherent, trait_);
    }

    // -----------------------------------------------------------------------
    // fnsig_flag_bytes tests
    // -----------------------------------------------------------------------

    use crate::wire::{FnSigFlags, SelfKind};

    #[test]
    fn fnsig_flag_bytes_default_is_deterministic() {
        let sig = FnSigFlags::default();
        let b1 = fnsig_flag_bytes(&sig);
        let b2 = fnsig_flag_bytes(&sig);
        assert_eq!(b1, b2);
    }

    #[test]
    fn fnsig_flag_bytes_self_kind_tags() {
        let mk = |sk: SelfKind| fnsig_flag_bytes(&FnSigFlags { self_kind: sk, ..Default::default() });
        assert_eq!(mk(SelfKind::None)[0], 0);
        assert_eq!(mk(SelfKind::Value)[0], 1);
        assert_eq!(mk(SelfKind::Ref)[0], 2);
        assert_eq!(mk(SelfKind::RefMut)[0], 3);
        let arb = mk(SelfKind::Arbitrary(TypeRefWire::Same(dummy_intro())));
        assert_eq!(arb[0], 4);
    }

    #[test]
    fn fnsig_flag_bytes_bool_flags() {
        let async_sig = FnSigFlags { is_async: true, ..Default::default() };
        let b = fnsig_flag_bytes(&async_sig);
        // byte 0 = self_kind (None=0), byte 1 = is_async
        assert_eq!(b[1], 1);
        assert_eq!(b[2], 0); // is_const
        assert_eq!(b[3], 0); // is_unsafe

        let unsafe_sig = FnSigFlags { is_unsafe: true, ..Default::default() };
        let b = fnsig_flag_bytes(&unsafe_sig);
        assert_eq!(b[3], 1);
    }

    #[test]
    fn fnsig_flag_bytes_abi_presence() {
        let no_abi = FnSigFlags::default();
        let b = fnsig_flag_bytes(&no_abi);
        // byte at offset 4 (after self_kind+3 bools) = 0x00 for no ABI
        assert_eq!(b[4], 0x00);

        let c_abi = FnSigFlags { abi: Some("C".into()), ..Default::default() };
        let b = fnsig_flag_bytes(&c_abi);
        assert_eq!(b[4], 0x01);
        // followed by encode_str("C") = u32le(1) + b"C"
        let len = u32::from_le_bytes([b[5], b[6], b[7], b[8]]);
        assert_eq!(len, 1);
        assert_eq!(b[9], b'C');
    }

    #[test]
    fn fnsig_flag_bytes_variadic_and_defaulted() {
        let sig = FnSigFlags { variadic: true, defaulted: true, ..Default::default() };
        let b = fnsig_flag_bytes(&sig);
        // last two bytes (no abi case: offset 5,6)
        assert_eq!(*b.last().unwrap(), 1, "defaulted must be last");
        assert_eq!(b[b.len() - 2], 1, "variadic must be second-to-last");
    }
}
