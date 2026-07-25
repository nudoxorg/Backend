//! Structural *skeletons* — deterministic, length-prefixed byte fingerprints of
//! types and signatures, used as `Disambiguator` payloads when minting an
//! [`IntroId`](crate::change::IntroId).
//!
//! # Why these exist
//!
//! Two declarations that share `(kind, ancestor-path, leaf-name)` — overloaded
//! functions, or several `impl` blocks — must still receive **distinct** intro
//! ids. The skeleton is the structural evidence that tells them apart.
//!
//! # The collision fix
//!
//! The sibling `workspace/ir` skeletons hash **only** the trait-ref + self-type
//! (`trait_impl_skeleton`) or the param types (`function_signature_skeleton`),
//! silently dropping `generics`, `where`-clauses, and impl negativity. That
//! makes `impl<T: Copy> Foo for Bar<T>` and `impl<T: Clone> Foo for Bar<T>` —
//! and a positive vs. a negative impl — collide onto one intro id. These
//! encoders include all of that, closing the collision.
//!
//! # Determinism rules (frozen — never change the opcodes/layout)
//!
//! - Every sequence is `u32le(count)` then each element (no magic separators —
//!   length prefixes make the encoding unambiguous without them).
//! - Generic-parameter **names are deliberately excluded**: renaming a type
//!   parameter (`impl<T>` vs `impl<U>`) is alpha-equivalent and must not change
//!   identity. Only the structural shape (kind + bounds + default) is hashed.
//!
//! FIXME: nudox-ir's [`Type`] is purely structural — it has no nominal type
//! reference or generic *application* node yet, so `Foo for Bar<u32>` vs
//! `Foo for Bar<String>` cannot be distinguished by the self-type (both reduce
//! to the same structural shape). That awaits the Type-nominal-ref enrichment;
//! bound-differentiated impls/overloads and negativity are fixed here.

use crate::{
    change::encode::{encode_str, write_u16le, write_u32le, write_u64le},
    kinds::{
        GenericParam, Type, WherePred,
        ty::{Primitive, Width},
    },
};

/// Opcode-per-variant structural encoding of a [`Type`].
pub fn type_skeleton(ty: &Type, out: &mut Vec<u8>) {
    match ty {
        Type::SelfType => out.push(0x01),
        Type::Primitive(p) => {
            out.push(0x02);
            primitive_skeleton(p, out);
        }
        Type::Tuple(ts) => {
            out.push(0x03);
            seq_skeleton(ts, out);
        }
        Type::Slice(t) => {
            out.push(0x04);
            type_skeleton(t, out);
        }
        Type::Array { ty, length } => {
            out.push(0x05);
            type_skeleton(ty, out);
            write_u64le(out, *length as u64);
        }
        Type::Union(ts) => {
            out.push(0x06);
            seq_skeleton(ts, out);
        }
        Type::Intersection(ts) => {
            out.push(0x07);
            seq_skeleton(ts, out);
        }
        Type::Never => out.push(0x08),
        Type::Any => out.push(0x09),
    }
}

fn primitive_skeleton(p: &Primitive, out: &mut Vec<u8>) {
    match p {
        Primitive::Integer { signed, width } => {
            out.push(0x01);
            out.push(*signed as u8);
            width_skeleton(width, out);
        }
        Primitive::Float(w) => {
            out.push(0x02);
            width_skeleton(w, out);
        }
        Primitive::Bool => out.push(0x03),
        Primitive::Char => out.push(0x04),
        Primitive::Str => out.push(0x05),
        Primitive::MutPointer(t) => {
            out.push(0x06);
            type_skeleton(t, out);
        }
        Primitive::ConstPointer(t) => {
            out.push(0x07);
            type_skeleton(t, out);
        }
        // The lifetime *name* is excluded (not identity-relevant); mutability and
        // the referent shape are.
        Primitive::Reference {
            lifetime: _,
            mutable,
            ty,
        } => {
            out.push(0x08);
            out.push(*mutable as u8);
            type_skeleton(ty, out);
        }
        Primitive::Builtin(s) => {
            out.push(0x09);
            encode_str(out, s);
        }
    }
}

fn width_skeleton(w: &Width, out: &mut Vec<u8>) {
    match w {
        Width::Fixed(n) => {
            out.push(0x01);
            write_u16le(out, n.get());
        }
        Width::Arch => out.push(0x02),
    }
}

fn seq_skeleton(ts: &[Type], out: &mut Vec<u8>) {
    write_u32le(out, ts.len() as u32);
    for t in ts {
        type_skeleton(t, out);
    }
}

/// Structural fingerprint of a generic-parameter list. Names are excluded
/// (alpha-equivalence); kind + bounds + default are hashed. **Part of the
/// fix.**
pub fn generics_skeleton(params: &[GenericParam], out: &mut Vec<u8>) {
    write_u32le(out, params.len() as u32);
    for p in params {
        match p {
            GenericParam::Lifetime { name: _ } => out.push(0x01),
            GenericParam::Type {
                name: _,
                bounds,
                default,
            } => {
                out.push(0x02);
                seq_skeleton(bounds, out);
                match default {
                    Some(t) => {
                        out.push(0x01);
                        type_skeleton(t, out);
                    }
                    None => out.push(0x00),
                }
            }
            GenericParam::Const { name: _, ty } => {
                out.push(0x03);
                type_skeleton(ty, out);
            }
        }
    }
}

/// Structural fingerprint of a `where`-clause list. **Part of the fix.**
pub fn wheres_skeleton(preds: &[WherePred], out: &mut Vec<u8>) {
    write_u32le(out, preds.len() as u32);
    for pred in preds {
        type_skeleton(&pred.target, out);
        seq_skeleton(&pred.bounds, out);
    }
}

fn opt_seq(tys: &[Option<Type>], out: &mut Vec<u8>) {
    write_u32le(out, tys.len() as u32);
    for t in tys {
        match t {
            Some(ty) => {
                out.push(0x01);
                type_skeleton(ty, out);
            }
            None => out.push(0x00),
        }
    }
}

/// Signature skeleton for overload disambiguation: input types, output types,
/// generics, and where-clauses. Callers resolve `EntryIndex<Param>` →
/// `Param.ty` before calling. Including generics + wheres is the overload half
/// of the fix.
pub fn function_signature_skeleton(
    input_tys: &[Option<Type>],
    output_tys: &[Option<Type>],
    generics: &[GenericParam],
    wheres: &[WherePred],
) -> Vec<u8> {
    let mut out = Vec::new();
    opt_seq(input_tys, &mut out);
    opt_seq(output_tys, &mut out);
    generics_skeleton(generics, &mut out);
    wheres_skeleton(wheres, &mut out);
    out
}

/// Skeleton for `impl` disambiguation: `(trait-ref, self-type, generics,
/// wheres, negative, blanket)`. The trailing four are exactly what
/// `workspace/ir` dropped, so distinct impls no longer collide.
pub fn trait_impl_skeleton(
    of: Option<&Type>,
    self_ty: &Type,
    generics: &[GenericParam],
    wheres: &[WherePred],
    negative: bool,
    blanket: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    match of {
        Some(t) => {
            out.push(0x01);
            type_skeleton(t, &mut out);
        }
        None => out.push(0x00),
    }
    type_skeleton(self_ty, &mut out);
    generics_skeleton(generics, &mut out);
    wheres_skeleton(wheres, &mut out);
    out.push(negative as u8);
    out.push(blanket as u8);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty_param(bound: Type) -> [GenericParam; 1] {
        [GenericParam::Type {
            name: "T".to_owned(),
            bounds: [bound].into(),
            default: None,
        }]
    }

    /// The exact collision Agent B found: two impls that differ ONLY by a
    /// generic bound must get different skeletons (else same IntroId →
    /// silent overwrite).
    #[test]
    fn bound_differentiated_impls_do_not_collide() {
        let copy = trait_impl_skeleton(None, &Type::Any, &ty_param(Type::Any), &[], false, false);
        let clone =
            trait_impl_skeleton(None, &Type::Any, &ty_param(Type::Never), &[], false, false);
        assert_ne!(
            copy, clone,
            "impl<T: Copy> and impl<T: Clone> must not collide"
        );
    }

    /// A negative impl must be distinguishable from the positive one.
    #[test]
    fn negativity_flips_the_skeleton() {
        let pos = trait_impl_skeleton(None, &Type::Any, &[], &[], false, false);
        let neg = trait_impl_skeleton(None, &Type::Any, &[], &[], true, false);
        assert_ne!(pos, neg, "impl Foo and impl !Foo must not collide");
    }

    /// Blanket status must be distinguishable.
    #[test]
    fn blanket_flips_the_skeleton() {
        let plain = trait_impl_skeleton(None, &Type::Any, &[], &[], false, false);
        let blanket = trait_impl_skeleton(None, &Type::Any, &[], &[], false, true);
        assert_ne!(plain, blanket);
    }

    /// A `where`-clause must change the skeleton.
    #[test]
    fn where_clause_changes_the_skeleton() {
        let w = [WherePred {
            target: Type::SelfType,
            bounds: [Type::Any].into(),
        }];
        let with = trait_impl_skeleton(None, &Type::Any, &[], &w, false, false);
        let without = trait_impl_skeleton(None, &Type::Any, &[], &[], false, false);
        assert_ne!(with, without);
    }

    /// Overloads differing only by a generic bound must not collide either.
    #[test]
    fn bound_differentiated_overloads_do_not_collide() {
        let a = function_signature_skeleton(&[Some(Type::I32)], &[], &ty_param(Type::Any), &[]);
        let b = function_signature_skeleton(&[Some(Type::I32)], &[], &ty_param(Type::Never), &[]);
        assert_ne!(a, b, "fn<T: Copy> and fn<T: Clone> must not collide");
    }

    /// Alpha-equivalence: renaming a generic parameter must NOT change the id
    /// (names are excluded). Same bounds + different name ⇒ identical skeleton.
    #[test]
    fn param_rename_is_alpha_equivalent() {
        let t = [GenericParam::Type {
            name: "T".to_owned(),
            bounds: [Type::Any].into(),
            default: None,
        }];
        let u = [GenericParam::Type {
            name: "U".to_owned(),
            bounds: [Type::Any].into(),
            default: None,
        }];
        let sk_t = trait_impl_skeleton(None, &Type::Any, &t, &[], false, false);
        let sk_u = trait_impl_skeleton(None, &Type::Any, &u, &[], false, false);
        assert_eq!(
            sk_t, sk_u,
            "renaming a type parameter must not change identity"
        );
    }

    /// Determinism: the same input always yields the same bytes.
    #[test]
    fn type_skeleton_is_deterministic() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        type_skeleton(&Type::Tuple([Type::I32, Type::Any].into()), &mut a);
        type_skeleton(&Type::Tuple([Type::I32, Type::Any].into()), &mut b);
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }
}
