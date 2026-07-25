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
        GenericParam, Type, WherePred,
        ty::{Primitive, Width},
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
            Type::Tuple(ts) => {
                self.out.push(0x03);
                self.seq(ts);
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
            Type::Nominal(r) => {
                self.out.push(0x0a);
                self.ref_(r);
            }
            Type::Apply { base, args } => {
                self.out.push(0x0b);
                self.ty(base);
                self.seq(args);
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
            Ref::Foreign(s) => {
                self.out.push(0x02);
                self.out.extend_from_slice(&s.canonical_bytes());
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
    /// (alpha-equivalence); kind + bounds + default are hashed.
    fn generics(&mut self, params: &[GenericParam]) {
        write_u32le(&mut self.out, params.len() as u32);
        for p in params {
            match p {
                GenericParam::Lifetime { name: _ } => self.out.push(0x01),
                GenericParam::Type {
                    name: _,
                    bounds,
                    default,
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
        let mut a = Skeleton::unresolved();
        let mut b = Skeleton::unresolved();
        a.ty(&Type::Tuple([Type::I32, Type::Any].into()));
        b.ty(&Type::Tuple([Type::I32, Type::Any].into()));
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
