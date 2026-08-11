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
//! # The collision fix — two layers
//!
//! **Layer 1 (was already landed):** The sibling `workspace/ir` skeletons hash
//! **only** the trait-ref + self-type (`trait_impl_skeleton`) or the param
//! types (`function_signature_skeleton`), silently dropping `generics`,
//! `where`-clauses, and impl negativity. That makes `impl<T: Copy> Foo for
//! Bar<T>` and `impl<T: Clone> Foo for Bar<T>` — and a positive vs. a negative
//! impl — collide onto one intro id. These encoders include all of that.
//!
//! **Layer 2 (P-A, this module):** `Ref::Local` nominal bases — same-package
//! types referenced before sealing is complete — previously all encoded as the
//! single placeholder byte `0x00`, so `impl Foo for Bar` and `impl Foo for Baz`
//! (bare same-package nominals, no generic arguments) produced identical
//! skeletons and therefore identical intro ids. The fix uses a one-step
//! *path-id pre-pass* in `seal`: every entry's id under `Disambiguator::None`
//! is computed up front (it depends only on kind + ancestor path + name, with
//! no refs), then passed into the skeleton encoder as a resolver. A resolved
//! `Ref::Local(idx)` encodes **byte-identically to `Ref::Intro(path_id)`** —
//! opcode `0x01` + 32 bytes — so the encoding is now one job: turn a reference
//! into its target's id bytes.
//!
//! An unresolvable ref (one whose index is not in the path-id map — imports
//! minted via `EntryBuilder::index_of_import`) still encodes as `0x00`
//! ("unknown"). An import's target lives in a different package; it has no
//! path-id within the current arena. The generic-arg layer still discriminates
//! `Bar<u32>` from `Bar<String>` even when `Bar` itself cannot be resolved, so
//! the common parameterised-type cases remain collision-free.
//!
//! # Termination argument
//!
//! The path-id pre-pass reads only `(kind, ancestor-path, leaf-name)` with
//! `Disambiguator::None`; it touches no skeleton. The skeleton pass (Pass 2)
//! reads only the path-id map produced by the pre-pass. There is no cycle.
//!
//! # Determinism rules (frozen — never change the opcodes/layout)
//!
//! - Every sequence is `u32le(count)` then each element (no magic separators —
//!   length prefixes make the encoding unambiguous without them).
//! - Generic-parameter **names are deliberately excluded**: renaming a type
//!   parameter (`impl<T>` vs `impl<U>`) is alpha-equivalent and must not change
//!   identity. Only the structural shape (kind + bounds + default) is hashed.
//! - [`Type::Nominal`] and [`Type::Apply`] carry named types and their generic
//!   arguments, so `Foo for Bar<u32>` and `Foo for Bar<String>` differ too.
//! - A `Ref::Local` that resolves through the path-id map encodes as opcode
//!   `0x01` + 32 bytes, identical to `Ref::Intro`. A `Ref::Local` that does not
//!   resolve (an import) encodes as `0x00`.

use crate::{
    change::{
        IntroId,
        encode::{encode_str, write_u16le, write_u32le, write_u64le},
    },
    index::{RawRef, Ref, UntypedEntryIndex},
    kinds::{
        GenericParam, Type, UnknownType, WherePred,
        ty::{
            AnonRecordForm, MappedModifier, Primitive, TemplatePart, TupleElement, Variance, Width,
        },
    },
};

// ---------------------------------------------------------------------------
// Skeleton writer
// ---------------------------------------------------------------------------

/// A write-once skeleton encoder that carries both the output buffer and a
/// resolver for same-package nominal references.
///
/// The resolver maps an arena-local [`UntypedEntryIndex`] to the entry's
/// *path id* — the [`IntroId`] minted with `Disambiguator::None`. It returns
/// `None` for indices that are not in the current arena (imports), which are
/// encoded as the "unknown" placeholder.
///
/// Use [`Skeleton::new`] when a resolver is available (the normal seal path)
/// and [`Skeleton::unresolved`] when no local indices are expected (unit tests,
/// foreign-only callers).
pub struct Skeleton<'a> {
    out: Vec<u8>,
    /// Stack fat pointer — no heap allocation.
    nominal: &'a dyn Fn(UntypedEntryIndex) -> Option<IntroId>,
}

/// Backing function for [`NO_RESOLVE`].
fn no_resolve_fn(_: UntypedEntryIndex) -> Option<IntroId> {
    None
}

/// A function-pointer resolver that always returns `None`, used by
/// [`Skeleton::unresolved`]. Stored as a `fn` pointer so that `&NO_RESOLVE`
/// is `&'static fn(...)` which coerces to `&'static dyn Fn(...)`.
static NO_RESOLVE: fn(UntypedEntryIndex) -> Option<IntroId> = no_resolve_fn;

impl<'a> Skeleton<'a> {
    /// Create a new encoder backed by `resolver` for local-ref lookup.
    pub fn new(resolver: &'a dyn Fn(UntypedEntryIndex) -> Option<IntroId>) -> Self {
        Self {
            out: Vec::new(),
            nominal: resolver,
        }
    }

    /// Create a new encoder that treats all local refs as unresolvable.
    ///
    /// Suitable for unit tests and callers whose types contain no
    /// `Ref::Local` variants (e.g. foreign-only nominal refs).
    pub fn unresolved() -> Self {
        Self {
            out: Vec::new(),
            nominal: &NO_RESOLVE,
        }
    }

    /// Consume the encoder and return the finished byte buffer.
    pub fn finish(self) -> Vec<u8> {
        self.out
    }

    // -----------------------------------------------------------------------
    // Public entry points
    // -----------------------------------------------------------------------

    /// Skeleton for `impl` disambiguation: `(trait-ref, self-type, generics,
    /// wheres, negative, blanket)`. Including generics + wheres + flags is
    /// what closes the `workspace/ir` collision; resolving local refs through
    /// the path-id map closes the P-A collision.
    pub fn trait_impl(
        mut self,
        of: Option<&Type>,
        self_ty: &Type,
        generics: &[GenericParam],
        wheres: &[WherePred],
        negative: bool,
        blanket: bool,
    ) -> Vec<u8> {
        match of {
            Some(t) => {
                self.out.push(0x01);
                self.ty(t);
            }
            None => self.out.push(0x00),
        }
        self.ty(self_ty);
        self.generics(generics);
        self.wheres(wheres);
        self.out.push(negative as u8);
        self.out.push(blanket as u8);
        self.out
    }

    /// Skeleton for overload disambiguation: input types, output types,
    /// generics, and where-clauses. Callers resolve `EntryIndex<Param>` →
    /// `Param.ty` before calling.
    pub fn signature(
        mut self,
        input_tys: &[Option<Type>],
        output_tys: &[Option<Type>],
        generics: &[GenericParam],
        wheres: &[WherePred],
    ) -> Vec<u8> {
        self.opt_seq(input_tys);
        self.opt_seq(output_tys);
        self.generics(generics);
        self.wheres(wheres);
        self.out
    }

    // -----------------------------------------------------------------------
    // Private recursive helpers
    // -----------------------------------------------------------------------

    fn ty(&mut self, ty: &Type) {
        match ty {
            Type::SelfType => self.out.push(0x01),
            Type::Primitive(p) => {
                self.out.push(0x02);
                self.primitive(p);
            }
            Type::Tuple(elems) => {
                self.out.push(0x03);
                self.tuple_elements(elems);
            }
            Type::Slice(t) => {
                self.out.push(0x04);
                self.ty(t);
            }
            Type::Array { ty, length } => {
                self.out.push(0x05);
                self.ty(ty);
                write_u64le(&mut self.out, *length as u64);
            }
            Type::Union(ts) => {
                self.out.push(0x06);
                self.seq(ts);
            }
            Type::Intersection(ts) => {
                self.out.push(0x07);
                self.seq(ts);
            }
            Type::Never => self.out.push(0x08),
            Type::Any => self.out.push(0x09),
            // The *reason* is identity-relevant, and so is any name it
            // carries.
            //
            // Two overloads whose parameters differ only in which external
            // type they name — `f(Context)` and `f(Command)`, both unresolved
            // — must not collide. That is the exact failure `Ref::Foreign`
            // was given canonical key bytes to fix (see `ref_`), and encoding
            // every `Unknown` as one opcode would reintroduce it one layer up.
            //
            // A later link pass that rewrites `Unknown(UnresolvedExternal)`
            // into `Nominal(Ref::Foreign)` *does* move the skeleton — but it
            // moves it by changing the opcode (0x18 → 0x0a) regardless of
            // whether the name is hashed, so carrying the name costs no extra
            // stability and buys collision-freedom in the meantime.
            Type::Unknown(reason) => {
                self.out.push(0x18);
                self.unknown(reason);
            }
            Type::Nominal(r) => {
                self.out.push(0x0a);
                self.ref_(r);
            }
            Type::Apply { base, args } => {
                self.out.push(0x0b);
                self.ty(base);
                self.seq(args);
            }
            // The *name* is deliberately excluded, exactly as it is for
            // `GenericParam`: `fn f<T>(x: T)` and `fn f<U>(x: U)` are
            // alpha-equivalent and must not get different identities.
            Type::TypeVar(_) => self.out.push(0x0c),
            Type::Wildcard { variance, bound } => {
                self.out.push(0x0d);
                self.variance(variance);
                match bound {
                    Some(t) => {
                        self.out.push(0x01);
                        self.ty(t);
                    }
                    None => self.out.push(0x00),
                }
            }
            // Params, return type, and ABI are all identity-relevant:
            // fn(i32)->bool and fn(i64)->bool are distinct types; and
            // delegate*<> and delegate* unmanaged[Cdecl]<> differ by ABI.
            Type::FunctionPointer { params, ret, abi } => {
                self.out.push(0x0e);
                self.seq(params);
                match ret {
                    Some(t) => {
                        self.out.push(0x01);
                        self.ty(t);
                    }
                    None => self.out.push(0x00),
                }
                match abi {
                    Some(s) => {
                        self.out.push(0x01);
                        encode_str(&mut self.out, s);
                    }
                    None => self.out.push(0x00),
                }
            }
            // Annotation is identity-relevant: @NonNull String and String
            // (unannotated) are distinct types with different contracts.
            Type::Annotated { inner, annotation } => {
                self.out.push(0x0f);
                self.ty(inner);
                encode_str(&mut self.out, &annotation.token);
                match &annotation.arg {
                    Some(s) => {
                        self.out.push(0x01);
                        encode_str(&mut self.out, s);
                    }
                    None => self.out.push(0x00),
                }
            }
            // All four arms are identity-relevant.
            Type::Conditional {
                check,
                extends_ty,
                then_ty,
                else_ty,
            } => {
                self.out.push(0x10);
                self.ty(check);
                self.ty(extends_ty);
                self.ty(then_ty);
                self.ty(else_ty);
            }
            // source, value, and modifiers are identity-relevant.
            // key_var is alpha-equivalent (excluded, like TypeVar names).
            Type::Mapped {
                key_var: _,
                source,
                value,
                readonly,
                optional,
            } => {
                self.out.push(0x11);
                self.ty(source);
                self.ty(value);
                self.mapped_modifier(readonly);
                self.mapped_modifier(optional);
            }
            // Literal spans ARE identity-relevant.
            Type::TemplateLiteral(parts) => {
                self.out.push(0x12);
                write_u32le(&mut self.out, parts.len() as u32);
                for part in parts.iter() {
                    match part {
                        TemplatePart::Literal(s) => {
                            self.out.push(0x01);
                            encode_str(&mut self.out, s);
                        }
                        TemplatePart::Interpolated(t) => {
                            self.out.push(0x02);
                            self.ty(t);
                        }
                    }
                }
            }
            // Form, member names, types, optional, and readonly are all
            // identity-relevant.
            Type::AnonymousRecord { form, members } => {
                self.out.push(0x13);
                self.anon_record_form(form);
                write_u32le(&mut self.out, members.len() as u32);
                for m in members.iter() {
                    encode_str(&mut self.out, &m.name);
                    self.ty(&m.ty);
                    self.out.push(m.optional as u8);
                    self.out.push(m.readonly as u8);
                }
            }
            Type::ImplTrait(bounds) => {
                self.out.push(0x14);
                self.seq(bounds);
            }
            Type::DynTrait(bounds) => {
                self.out.push(0x15);
                self.seq(bounds);
            }
            // Inferred encodes as its opcode only — every `_` is structurally
            // identical to every other.
            Type::Inferred => self.out.push(0x16),
            Type::QualifiedPath {
                self_ty,
                trait_ref,
                assoc,
            } => {
                self.out.push(0x17);
                self.ty(self_ty);
                match trait_ref {
                    Some(t) => {
                        self.out.push(0x01);
                        self.ty(t);
                    }
                    None => self.out.push(0x00),
                }
                encode_str(&mut self.out, assoc);
            }
        }
    }

    /// Encode a nominal reference.
    ///
    /// Opcodes:
    /// - `0x00` — unknown / unresolvable (import, or no resolver provided).
    /// - `0x01` + 32 bytes — a resolved entry: either `Ref::Intro(id)`
    ///   directly, or `Ref::Local(idx)` resolved through the path-id map to its
    ///   id. The two cases are **byte-identical** by construction.
    /// - `0x02` + canonical bytes — a cross-package `Ref::Foreign`.
    ///
    /// Because a resolved local and an intro produce identical bytes, the
    /// skeleton of any entry is **stable across sealing**: it encodes the same
    /// bytes whether computed pre-seal (with the path-id resolver) or post-seal
    /// (where every `Local` has already been rewritten to `Intro` by
    /// `visit_mut`).
    fn ref_(&mut self, r: &RawRef) {
        match r {
            Ref::Local(idx) => {
                // Rebind into a `let` so the shared borrow of `self.nominal`
                // ends before we mutably borrow `self.out`.
                let resolved = (self.nominal)(*idx);
                match resolved {
                    Some(id) => {
                        self.out.push(0x01);
                        self.out.extend_from_slice(id.as_bytes());
                    }
                    None => self.out.push(0x00),
                }
            }
            Ref::Intro(i) => {
                self.out.push(0x01);
                self.out.extend_from_slice(i.as_bytes());
            }
            // The cross-package KEY, never the resolved target. Linking a
            // reference must not change any skeleton, or an entry's `IntroId`
            // would depend on which *other* packages happened to be sealed
            // alongside it.
            //
            // This is also what structurally separates `impl Clone for Memchr`
            // from `impl Debug for Memchr`: both used to encode their foreign
            // trait as the single placeholder byte `0x00`, so their `TraitImpl`
            // skeletons were byte-identical. Rust survived that only because
            // `impl_display_name` bakes the trait name into `Symbol::name` —
            // a naming convention, not an invariant, and one that Java and C#
            // do not share.
            Ref::Foreign { key, .. } => {
                self.out.push(0x02);
                self.out.extend_from_slice(&key.canonical_bytes());
            }
        }
    }

    /// Encode an [`UnknownType`] reason. No `_` arm — a new reason must get an
    /// opcode here, or identity silently merges two different gaps.
    fn unknown(&mut self, r: &UnknownType) {
        match r {
            UnknownType::Unannotated => self.out.push(0x01),
            UnknownType::DynamicallyTyped => self.out.push(0x02),
            UnknownType::UnresolvedLocalName { name } => {
                self.out.push(0x03);
                encode_str(&mut self.out, name);
            }
            UnknownType::UnresolvedExternal { name } => {
                self.out.push(0x04);
                encode_str(&mut self.out, name);
            }
            UnknownType::TruncatedAtDepthLimit => self.out.push(0x05),
            UnknownType::OracleGap => self.out.push(0x06),
            UnknownType::NoIrRepresentation { construct } => {
                self.out.push(0x07);
                encode_str(&mut self.out, construct);
            }
        }
    }

    fn primitive(&mut self, p: &Primitive) {
        match p {
            Primitive::Integer { signed, width } => {
                self.out.push(0x01);
                self.out.push(*signed as u8);
                self.width(width);
            }
            Primitive::Float(w) => {
                self.out.push(0x02);
                self.width(w);
            }
            Primitive::Bool => self.out.push(0x03),
            Primitive::Char => self.out.push(0x04),
            Primitive::Str => self.out.push(0x05),
            Primitive::MutPointer(t) => {
                self.out.push(0x06);
                self.ty(t);
            }
            Primitive::ConstPointer(t) => {
                self.out.push(0x07);
                self.ty(t);
            }
            // The lifetime *name* is excluded (not identity-relevant); mutability
            // and the referent shape are.
            Primitive::Reference {
                lifetime: _,
                mutable,
                ty,
            } => {
                self.out.push(0x08);
                self.out.push(*mutable as u8);
                self.ty(ty);
            }
            Primitive::Builtin(s) => {
                self.out.push(0x09);
                encode_str(&mut self.out, s);
            }
        }
    }

    fn width(&mut self, w: &Width) {
        match w {
            Width::Fixed(n) => {
                self.out.push(0x01);
                write_u16le(&mut self.out, n.get());
            }
            Width::Arch => self.out.push(0x02),
        }
    }

    fn seq(&mut self, ts: &[Type]) {
        write_u32le(&mut self.out, ts.len() as u32);
        for t in ts {
            self.ty(t);
        }
    }

    /// Encode a sequence of tuple elements.
    ///
    /// **Identity decision:** the label IS included — `(int start, int end)` and
    /// `(int, int)` are distinct C# types with different member-access semantics,
    /// and two overloads differing only in tuple-element labelling must not
    /// collide.
    fn tuple_elements(&mut self, elems: &[TupleElement]) {
        write_u32le(&mut self.out, elems.len() as u32);
        for elem in elems {
            match elem {
                TupleElement::Positional(t) => {
                    self.out.push(0x01);
                    self.ty(t);
                }
                TupleElement::Named { label, ty } => {
                    self.out.push(0x02);
                    encode_str(&mut self.out, label);
                    self.ty(ty);
                }
            }
        }
    }

    fn variance(&mut self, v: &Variance) {
        self.out.push(match v {
            Variance::Invariant => 0x01,
            Variance::Covariant => 0x02,
            Variance::Contravariant => 0x03,
        });
    }

    fn mapped_modifier(&mut self, m: &MappedModifier) {
        self.out.push(match m {
            MappedModifier::Add => 0x01,
            MappedModifier::Remove => 0x02,
            MappedModifier::Absent => 0x03,
        });
    }

    fn anon_record_form(&mut self, f: &AnonRecordForm) {
        self.out.push(match f {
            AnonRecordForm::Struct => 0x01,
            AnonRecordForm::Interface => 0x02,
        });
    }

    fn opt_seq(&mut self, tys: &[Option<Type>]) {
        write_u32le(&mut self.out, tys.len() as u32);
        for t in tys {
            match t {
                Some(ty) => {
                    self.out.push(0x01);
                    self.ty(ty);
                }
                None => self.out.push(0x00),
            }
        }
    }

    /// Structural fingerprint of a generic-parameter list. Names are excluded
    /// (alpha-equivalence); kind + bounds + default + variance are hashed.
    ///
    /// **Variance in identity:** declaration-site variance (`in`/`out` in C#)
    /// IS included. `interface IFoo<in T>` and `interface IFoo<out T>` are
    /// structurally distinct — a consumer seeing only the skeleton must be able
    /// to tell them apart. Variance is not alpha-equivalent detail.
    fn generics(&mut self, params: &[GenericParam]) {
        write_u32le(&mut self.out, params.len() as u32);
        for p in params {
            match p {
                GenericParam::Lifetime { name: _ } => self.out.push(0x01),
                GenericParam::Type {
                    name: _,
                    bounds,
                    default,
                    variance,
                } => {
                    self.out.push(0x02);
                    self.seq(bounds);
                    match default {
                        Some(t) => {
                            self.out.push(0x01);
                            self.ty(t);
                        }
                        None => self.out.push(0x00),
                    }
                    // Variance: 0x00 = None (unspecified); otherwise 0x01 + opcode.
                    match variance {
                        None => self.out.push(0x00),
                        Some(v) => {
                            self.out.push(0x01);
                            self.variance(v);
                        }
                    }
                }
                GenericParam::Const { name: _, ty } => {
                    self.out.push(0x03);
                    self.ty(ty);
                }
            }
        }
    }

    /// Structural fingerprint of a `where`-clause list.
    fn wheres(&mut self, preds: &[WherePred]) {
        write_u32le(&mut self.out, preds.len() as u32);
        for pred in preds {
            self.ty(&pred.target);
            self.seq(&pred.bounds);
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience free functions (thin wrappers — the public API surface)
// ---------------------------------------------------------------------------

/// Type fingerprint for a single [`Type`] expression.
///
/// Used by the archive's type-fingerprint index to group entries that share the
/// same declared type (e.g. all `Field`s with type `u32`). The bytes are a
/// deterministic, length-prefixed encoding of the type's structural shape;
/// two types produce the same bytes iff they are structurally identical (no
/// local-ref resolution — treat every `Ref::Local` as unknown).
///
/// Uses no local-ref resolver; call [`Skeleton::new`] directly if you need one.
pub fn type_skeleton(ty: &crate::kinds::Type) -> Vec<u8> {
    let mut s = Skeleton::unresolved();
    s.ty(ty);
    s.finish()
}

/// Signature skeleton for overload disambiguation: input types, output types,
/// generics, and where-clauses. Callers resolve `EntryIndex<Param>` →
/// `Param.ty` before calling.
///
/// Uses no local-ref resolver; call [`Skeleton::new`] directly if you need one.
pub fn function_signature_skeleton(
    input_tys: &[Option<Type>],
    output_tys: &[Option<Type>],
    generics: &[GenericParam],
    wheres: &[WherePred],
) -> Vec<u8> {
    Skeleton::unresolved().signature(input_tys, output_tys, generics, wheres)
}

/// Skeleton for `impl` disambiguation: `(trait-ref, self-type, generics,
/// wheres, negative, blanket)`.
///
/// Uses no local-ref resolver; call [`Skeleton::new`] directly if you need one.
pub fn trait_impl_skeleton(
    of: Option<&Type>,
    self_ty: &Type,
    generics: &[GenericParam],
    wheres: &[WherePred],
    negative: bool,
    blanket: bool,
) -> Vec<u8> {
    Skeleton::unresolved().trait_impl(of, self_ty, generics, wheres, negative, blanket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::IntroId;

    fn ty_param(bound: Type) -> [GenericParam; 1] {
        [GenericParam::Type {
            name: "T".to_owned(),
            bounds: [bound].into(),
            default: None,
            variance: None,
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
            variance: None,
        }];
        let u = [GenericParam::Type {
            name: "U".to_owned(),
            bounds: [Type::Any].into(),
            default: None,
            variance: None,
        }];
        let sk_t = trait_impl_skeleton(None, &Type::Any, &t, &[], false, false);
        let sk_u = trait_impl_skeleton(None, &Type::Any, &u, &[], false, false);
        assert_eq!(
            sk_t, sk_u,
            "renaming a type parameter must not change identity"
        );
    }

    /// The deferred half of the collision fix, now closed: `impl Foo for
    /// Bar<u32>` vs `impl Foo for Bar<String>` — same nominal base,
    /// different generic argument — must not collide.
    #[test]
    fn generic_application_args_discriminate() {
        let bar = |arg: Type| Type::Apply {
            base: Box::new(Type::Nominal(Ref::Intro(IntroId::from_raw([0xba; 32])))),
            args: [arg].into(),
        };
        let u32_impl = trait_impl_skeleton(None, &bar(Type::U32), &[], &[], false, false);
        let str_impl = trait_impl_skeleton(
            None,
            &bar(Type::Primitive(Primitive::Str)),
            &[],
            &[],
            false,
            false,
        );
        assert_ne!(
            u32_impl, str_impl,
            "Foo for Bar<u32> and Foo for Bar<String> must not collide"
        );
    }

    /// Distinct *sealed* nominal types discriminate (Intro targets hash by
    /// bytes).
    #[test]
    fn distinct_sealed_nominals_discriminate() {
        let a = Type::Nominal(Ref::Intro(IntroId::from_raw([0xaa; 32])));
        let b = Type::Nominal(Ref::Intro(IntroId::from_raw([0xbb; 32])));
        let mut sa = Skeleton::unresolved();
        let mut sb = Skeleton::unresolved();
        sa.ty(&a);
        sb.ty(&b);
        assert_ne!(
            sa.finish(),
            sb.finish(),
            "different nominal targets must differ"
        );
    }

    /// Arity matters: `Bar<u32>` ≠ `Bar<u32, u32>`.
    #[test]
    fn generic_arity_discriminates() {
        let base = || Box::new(Type::Nominal(Ref::Intro(IntroId::from_raw([0xba; 32]))));
        let one = Type::Apply {
            base: base(),
            args: [Type::U32].into(),
        };
        let two = Type::Apply {
            base: base(),
            args: [Type::U32, Type::U32].into(),
        };
        let mut s1 = Skeleton::unresolved();
        let mut s2 = Skeleton::unresolved();
        s1.ty(&one);
        s2.ty(&two);
        assert_ne!(s1.finish(), s2.finish());
    }

    /// Determinism: the same input always yields the same bytes.
    #[test]
    fn type_skeleton_is_deterministic() {
        let tup = Type::Tuple(
            [
                TupleElement::Positional(Type::I32),
                TupleElement::Positional(Type::Any),
            ]
            .into(),
        );
        let mut a = Skeleton::unresolved();
        let mut b = Skeleton::unresolved();
        a.ty(&tup);
        b.ty(&tup.clone());
        let (fa, fb) = (a.finish(), b.finish());
        assert_eq!(fa, fb);
        assert!(!fa.is_empty());
    }

    /// P-A fix: a `Ref::Local` resolved through the path-id map encodes
    /// byte-identically to a `Ref::Intro` carrying the same `IntroId`.
    #[test]
    fn resolved_local_is_byte_identical_to_intro() {
        let id = IntroId::from_raw([0x42; 32]);
        let fake_idx = UntypedEntryIndex::export(1);

        // Resolver that always maps to `id`.
        let resolver = |_: UntypedEntryIndex| Some(id);

        let local_ty = Type::Nominal(Ref::Local(fake_idx));
        let intro_ty = Type::Nominal(Ref::Intro(id));

        let mut with_local = Skeleton::new(&resolver);
        with_local.ty(&local_ty);
        let bytes_local = with_local.finish();

        let mut with_intro = Skeleton::unresolved();
        with_intro.ty(&intro_ty);
        let bytes_intro = with_intro.finish();

        assert_eq!(
            bytes_local, bytes_intro,
            "Ref::Local resolved to path_id must encode identically to Ref::Intro(path_id)"
        );
    }

    /// `Type::Unknown` opcodes are frozen: `0x18` then the reason opcode.
    ///
    /// Pinning the bytes here is what makes the CC-2 domain bump a *one-time*
    /// event — a later edit that reorders the reason opcodes would silently
    /// move every affected `IntroId` again.
    #[test]
    fn unknown_reason_opcodes_are_frozen() {
        use crate::kinds::UnknownType;
        let bytes = |r: UnknownType| type_skeleton(&Type::Unknown(r));
        assert_eq!(bytes(UnknownType::Unannotated), &[0x18, 0x01]);
        assert_eq!(bytes(UnknownType::DynamicallyTyped), &[0x18, 0x02]);
        assert_eq!(bytes(UnknownType::TruncatedAtDepthLimit), &[0x18, 0x05]);
        assert_eq!(bytes(UnknownType::OracleGap), &[0x18, 0x06]);
        // Name-carrying reasons: opcode, reason, then the encoded string.
        let local = bytes(UnknownType::UnresolvedLocalName {
            name: "Ctx".to_owned(),
        });
        assert_eq!(&local[..2], &[0x18, 0x03]);
        assert!(local.ends_with(b"Ctx"), "the name must be in the skeleton");
        let external = bytes(UnknownType::UnresolvedExternal {
            name: "Ctx".to_owned(),
        });
        assert_eq!(&external[..2], &[0x18, 0x04]);
        assert_ne!(local, external, "the reason must discriminate, not just the name");
        assert_eq!(
            &bytes(UnknownType::NoIrRepresentation {
                construct: "complex128".to_owned()
            })[..2],
            &[0x18, 0x07]
        );
    }

    /// `Type::Any` keeps opcode `0x09` — the top type's encoding is unchanged
    /// by CC-2, so an entry whose signature genuinely mentions `Object` /
    /// `interface{}` / `unknown` keeps a byte-identical preimage across the
    /// v4 → v5 boundary.
    ///
    /// Its *digest* still moves, because `INTRO_DOMAIN` is hashed as a prefix
    /// of that preimage. Preimage stability is the property worth pinning here:
    /// it is what says the `Unknown` split did not quietly redefine the top
    /// type as well.
    #[test]
    fn any_opcode_is_unchanged_by_the_unknown_split() {
        assert_eq!(type_skeleton(&Type::Any), &[0x09]);
    }

    /// Two overloads whose only difference is an unresolved parameter type
    /// must not share a signature skeleton.
    ///
    /// This is the Gson / Click failure mode one layer up from `Ref::Foreign`:
    /// under `Type::Any` both signatures encoded the same single byte, minted
    /// the same `IntroId`, and one method silently overwrote the other.
    #[test]
    fn overloads_over_distinct_unresolved_types_do_not_collide() {
        let a = function_signature_skeleton(
            &[Some(Type::unresolved_external("java.lang.Class"))],
            &[],
            &[],
            &[],
        );
        let b = function_signature_skeleton(
            &[Some(Type::unresolved_external("java.lang.reflect.Type"))],
            &[],
            &[],
            &[],
        );
        assert_ne!(a, b, "fromJson(String, Class) and (String, Type) must differ");
    }

    /// An unresolvable local ref (import) encodes as the 0x00 placeholder.
    #[test]
    fn unresolvable_local_encodes_as_placeholder() {
        let fake_idx = UntypedEntryIndex::export(1);
        let ty = Type::Nominal(Ref::Local(fake_idx));
        let mut sk = Skeleton::unresolved();
        sk.ty(&ty);
        let bytes = sk.finish();
        // opcode 0x0a (Nominal) then 0x00 (unknown ref).
        assert_eq!(bytes, &[0x0a, 0x00]);
    }
}
