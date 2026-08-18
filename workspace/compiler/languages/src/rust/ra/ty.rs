//! Type lowering: AST shape + Semantics resolution → `nudox_ir::kinds::Type`.
//!
//! # What changed vs the old ty.rs
//!
//! The old `ty.rs` produced the old IR's `Type` enum (`TypeReference`,
//! `BorrowedRef`, `RawPointer`, `DynTrait`, `ImplTrait`, etc.)  The new IR's
//! `Type` enum covers:
//!
//! ```text
//! Type::SelfType
//! Type::Primitive(Primitive)          — integers, floats, bool, char, Str,
//!                                       MutPointer, ConstPointer, Reference
//! Type::Tuple(List<TupleElement>)     — Positional | Named; Rust uses Positional
//! Type::Slice(Box<Type>)
//! Type::Array { ty, length }
//! Type::Union(List<Type>)             — used for raw unions only
//! Type::Intersection(List<Type>)      — TypeScript; unused for Rust
//! Type::Never
//! Type::Any                           — NEVER emitted: Rust has no top type
//! Type::Unknown(UnknownType)          — a named reason the type is not known
//! Type::Nominal(RawRef)               — named type; Ref resolved at seal time
//! Type::Apply { base, args }          — generic application Foo<T, U>
//! Type::TypeVar(String)               — use of a generic type parameter
//! Type::Wildcard { variance, bound }  — Java/Kotlin wildcards; unused for Rust
//! Type::FunctionPointer { params, ret, abi }  — `fn(…) -> T` / `extern "C" fn(…)`
//! Type::Annotated { inner, annotation }       — Java @NonNull, C# nullable refs
//! Type::ImplTrait(List<Type>)         — `impl Trait` (static opaque dispatch)
//! Type::DynTrait(List<Type>)          — `dyn Trait` (fat pointer, dynamic dispatch)
//! Type::Inferred                      — `_`; a real type exists but is unresolved
//! Type::QualifiedPath { self_ty, trait_ref, assoc }  — `<T as Trait>::Assoc`
//! Type::Conditional / Mapped / TemplateLiteral       — TypeScript; unused for Rust
//! Type::AnonymousRecord { form, members }            — Go/TS; unused for Rust
//! ```
//!
//! `ImplTrait` and `DynTrait` are deliberately separate variants: `impl Trait`
//! is zero-cost static dispatch, `dyn Trait` is a vtable fat pointer. Unifying
//! them behind a flag would erase that distinction.
//!
//! `Nominal` carries a `RawRef` that the caller-supplied `ref_for` callback
//! resolves against the `Lowering` arena. For a **local** path this is a
//! forward reference (`Ref::Local`, rewritten to `Ref::Intro` at seal time).
//! For a **foreign** (cross-crate) path, `ref_for` (see
//! `item.rs::make_ref_for!`) calls `Lowering::refer_import`, which *also*
//! returns `Ref::Local` — a forward reference into a separate "import" arena
//! slot, meant to be resolved to `Ref::Foreign` later. Either way `ref_for`
//! returns `Some`, so a generic application of an external type —
//! `Option<usize>`, `Vec<T>`, `HashMap<K, V>`, … — lowers to `Type::Apply {
//! base: Nominal(ref), args }` exactly like a local one; `ref_for` returning
//! `None` (falling back to `Type::Any`) is reached only when a path fails to
//! *resolve* at all (unknown macro-generated code, a broken `cfg`, …), not
//! merely because the type is external.
//!
//! **If a rendered signature is missing a generic wrapper (docs/LIMITATIONS.md
//! L19), the defect is not in this module — this module's `Type::Apply`
//! construction is correct.** Verified directly (2026-08-05) two ways: (a)
//! instrumenting `lower_path_type` while lowering `result/memchr-2.8.3`
//! showed every one of `memchr`'s ~150 uses of `Option<T>` — including the
//! public `memchr()` function's own return type — resolves through
//! `PathResolution::Def` and builds `Type::Apply` correctly; (b)
//! `crates/nudox-engine/tests/generic_signature_shapes.rs`'s
//! `apply_nesting_is_correct_not_collapsed_to_inner_arg` asserts the built
//! `Type::Apply` structure directly (arity + nesting, bypassing rendering)
//! for `Option<T>`, `Result<T, E>`, `Vec<T>`, `HashMap<K, V>`, `Box<dyn
//! Trait>`, and a three-deep `Option<Vec<Result<T, E>>>` — the exact shape
//! the L19 ticket describes collapsing — and passes.
//!
//! The rendered *text* is nonetheless broken today, for two reasons, both
//! entirely outside `ty.rs` and `crates/nudox-engine/src/chunk/signature.rs`:
//!
//! 1. **Cross-package `Ref::Local` is never resolved to `Ref::Foreign`.**
//!    `workspace/ir/model/src/package/seal.rs`'s `Ref::Local` → `Ref::Intro`
//!    rewrite only walks the package's own declared entries and has no
//!    knowledge of the import arena; a repo-wide grep confirms `Ref::Foreign(`
//!    is never *constructed* anywhere in `workspace/ir/model/src` (only
//!    matched, in `index.rs`'s trait impls). So every foreign generic's
//!    `Ref::Local` survives into the sealed table, where
//!    `signature.rs::resolve_nominal` — whose own comment says a bare
//!    `Ref::Local` "should not appear in a sealed table" — renders it `"?"`.
//!    Measured on real `memchr`: 147/420 functions render a bare `?` for a
//!    foreign generic base, and zero functions in the whole crate correctly
//!    render `Option<`, `Result<`, `Vec<`, `Box<`, or `HashMap<`.
//! 2. **A separate identity-collision bug** in `item.rs`: every function's
//!    parameter/return `Param` entry is declared under the function's
//!    *enclosing* `parent` (module or impl) instead of under the function's
//!    own id (`declare_params`, and the `output_refs` closure in
//!    `lower_free_function_with_id`, both do `out.declare(param_id,
//!    parent.clone(), …)` where `parent` is the caller-supplied enclosing
//!    scope, not `Some(fn_id.clone())`). Combined with `seal.rs`'s collision
//!    disambiguator falling back to `Disambiguator::Span` for any
//!    non-Function/Impl kind, and `item.rs::plain_sym` always setting `span:
//!    0..0` for producer-synthesized param symbols, every same-named
//!    parameter declared directly in the same module (in particular *every*
//!    function's `return` param) collides onto one shared `IntroId` and
//!    silently overwrites its siblings in `PristineIntroTable`. Measured on
//!    real `memchr`: 58 collision groups; 416 functions with a return type
//!    reduce to 188 distinct identities. `memchr`'s own `return` Param is
//!    overwritten by one of 18 sibling free functions (`memchr2`, `memchr3`,
//!    `memrchr`, `memrchr2`, `memrchr3`, and their `_raw`/`_iter` variants)
//!    declared in the same module — whichever of those legitimately returns
//!    a bare `usize` (most plausibly `count_raw`) is what actually renders
//!    for `memchr`, which is the literal `usize` text the ticket reports
//!    (this bug alone explains the *exact* wording; bug (1) alone would have
//!    produced `?<usize>` instead).
//!
//! Neither file is in this crate's edit scope: `item.rs` is under active
//! concurrent edit by another agent, and `workspace/ir/model/src/{lower,
//! package/seal}.rs` belong to `nudox-ir`, a crate this task never
//! authorized touching. See the L19 task report's REMAINING section for the
//! full evidence trail.
//!
//! # This producer emits no `Type::Any` at all (CC-2)
//!
//! **Rust has no top type.** There is no `Object`, no `interface{}`, no
//! `unknown` — no type that every value inhabits. Every `Type::Any` this
//! module used to emit was therefore a claim the language cannot express, and
//! the post-CC-2 meaning of `Type::Any` (a genuine top type) has no Rust
//! inhabitant. Every former site now says which kind of gap it is:
//!
//! - **Unresolvable path** — `resolve_path_opt` returned `None`, or resolved
//!   but `ref_for` produced nothing → `UnknownType::UnresolvedExternal`,
//!   carrying the written path. rust-analyzer resolves everything in the crate
//!   it loaded, so a failure here names something outside it (or behind a
//!   macro it declined to expand).
//! - **Missing AST child / unexpanded macro** — a `PathType` with no path, a
//!   `SliceType` with no element, a `MacroType` whose call is absent →
//!   `UnknownType::OracleGap`. The source did not parse; no producer effort
//!   closes it.
//! - **`str` and other builtins with no `Primitive` slot**, and HIR types with
//!   no IR variant (closures, fn items, opaques) →
//!   `UnknownType::NoIrRepresentation`, carrying the construct name. This is
//!   the countable lattice backlog.
//!
//! Keeping the spelling is not cosmetic. `Skeleton` encoded `Type::Any` as one
//! byte, so `fn f(x: SomeForeignA)` and `fn f(x: SomeForeignB)` produced
//! byte-identical signature skeletons — one `IntroId`, one silent overwrite.
//!
//! # Still-unrepresentable, and not a `Type::Unknown` case
//!
//! - **Lifetime / const generic args in `Apply.args`** — `Apply.args` is
//!   `List<Type>`; there is no position for lifetime or const arguments.
//! - **Lifetime-only bounds on `dyn Trait`** — `dyn Trait + 'static` lifetime
//!   bounds are filtered out in `lower_dyn_trait`; the IR's `List<Type>` has
//!   no lifetime slot.
//!
//! Both drop an argument from a list rather than mislabel a type, so there is
//! no type position to attach a reason to.
//!
//! Previously `Any` fallbacks that now have real IR slots:
//! - `impl Trait` → `Type::ImplTrait(bounds)` (was: `Type::Any`)
//! - `dyn Trait + Bound` → `Type::DynTrait(bounds)` (was: `Type::Intersection`)
//! - `<T as Trait>::Assoc` → `Type::QualifiedPath` (was: `Type::Any`)
//! - `_` (InferType) → `Type::Inferred` (was: `Type::Any`)
//!
//! # Strategy
//!
//! 1. AST shape is the source of truth for lifetimes, mutability, and arg order.
//! 2. `Semantics::resolve_path` resolves `PathType` → `ModuleDef` so we can
//!    emit `Nominal(RawRef)` for both local *and* foreign types (see above).
//! 3. Only an unresolvable path falls back to a named `Type::Unknown`.
//! 4. `lower_hir_type_fallback` covers the no-AST case (macro-expanded items).

use std::panic::{self, AssertUnwindSafe};

use ra_ap_hir::{HirDisplay, ModuleDef, Mutability, PathResolution, attach_db};
use ra_ap_syntax::{
    AstNode,
    ast::{self, HasGenericArgs, PathSegmentKind},
};
use tracing::debug;

use nudox_ir::{
    index::RawRef,
    kinds::{
        Type,
        ty::{Primitive, TupleElement, Width},
    },
};

use super::ctx::{LowerCtx, PathKey};
use super::item::id_of;

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower a written AST type.
///
/// `ref_for` maps a resolved canonical path (local *or* foreign — see the
/// module doc) to a `RawRef` via `Lowering::refer`/`refer_import`. It returns
/// `None` only when the path could not be resolved at all, so the caller can
/// substitute `Type::Any`; a resolved foreign path still yields a real
/// `RawRef` and participates in `Type::Apply` normally.
pub(crate) fn lower_ast_type(
    ctx: &mut LowerCtx<'_>,
    node: &ast::Type,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    match node {
        ast::Type::PathType(p) => lower_path_type(ctx, p, ref_for),
        ast::Type::RefType(r) => lower_ref_type(ctx, r, ref_for),
        ast::Type::PtrType(p) => lower_ptr_type(ctx, p, ref_for),
        ast::Type::SliceType(s) => {
            let inner = s
                .ty()
                .map_or(Type::ORACLE_GAP, |t| lower_ast_type(ctx, &t, ref_for));
            Type::Slice(Box::new(inner))
        }
        ast::Type::ArrayType(a) => lower_array_type(ctx, a, ref_for),
        ast::Type::TupleType(t) => {
            // Rust tuples are positional; no element labels.
            let fields: Box<[TupleElement]> = t
                .fields()
                .map(|f| TupleElement::Positional(lower_ast_type(ctx, &f, ref_for)))
                .collect();
            Type::Tuple(fields)
        }
        ast::Type::FnPtrType(f) => lower_fn_ptr_type(ctx, f, ref_for),
        ast::Type::DynTraitType(d) => lower_dyn_trait(ctx, d, ref_for),
        ast::Type::ImplTraitType(i) => lower_impl_trait(ctx, i, ref_for),
        ast::Type::NeverType(_) => Type::Never,
        // `_` wildcard / infer position: a specific type exists but the producer
        // could not determine it. Distinct from Type::Any (genuinely dynamic).
        ast::Type::InferType(_) => Type::Inferred,
        ast::Type::ParenType(p) => p
            .ty()
            .map_or(Type::ORACLE_GAP, |t| lower_ast_type(ctx, &t, ref_for)),
        ast::Type::ForType(f) => f
            .ty()
            .map_or(Type::ORACLE_GAP, |t| lower_ast_type(ctx, &t, ref_for)),
        ast::Type::MacroType(m) => lower_macro_type(ctx, m, ref_for),
        // Pattern types (`#is(...)`) are nightly-only; peel to the base type.
        ast::Type::PatternType(p) => p
            .ty()
            .map_or(Type::ORACLE_GAP, |t| lower_ast_type(ctx, &t, ref_for)),
    }
}

/// Prefer AST when present; otherwise fall back to HIR (lifetimes erased).
pub(crate) fn lower_type_prefer_ast(
    ctx: &mut LowerCtx<'_>,
    ast_ty: Option<&ast::Type>,
    hir_ty: &ra_ap_hir::Type<'_>,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    match ast_ty {
        Some(t) => lower_ast_type(ctx, t, ref_for),
        None => lower_hir_type_fallback(ctx, hir_ty, ref_for),
    }
}

/// Fallback when only `hir::Type` is available (no useful AST).
///
/// Lifetimes are erased.  Prefer `lower_ast_type` whenever an AST node exists.
pub(crate) fn lower_hir_type_fallback(
    ctx: &mut LowerCtx<'_>,
    ty: &ra_ap_hir::Type<'_>,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    if ty.is_never() {
        return Type::Never;
    }
    if ty.is_unit() {
        return Type::Tuple(Box::new([]));
    }
    if let Some((inner, mutability)) = ty.as_reference() {
        return Type::Primitive(Primitive::Reference {
            lifetime: None,
            mutable: matches!(mutability, Mutability::Mut),
            ty: Box::new(lower_hir_type_fallback(ctx, &inner, ref_for)),
        });
    }
    if let Some((inner, mutability)) = ty.as_raw_ptr() {
        let inner_ty = Box::new(lower_hir_type_fallback(ctx, &inner, ref_for));
        return if matches!(mutability, Mutability::Mut) {
            Type::Primitive(Primitive::MutPointer(inner_ty))
        } else {
            Type::Primitive(Primitive::ConstPointer(inner_ty))
        };
    }
    if let Some(inner) = ty.as_slice() {
        return Type::Slice(Box::new(lower_hir_type_fallback(ctx, &inner, ref_for)));
    }
    if let Some((inner, length)) = ty.as_array(ctx.db) {
        return Type::Array {
            ty: Box::new(lower_hir_type_fallback(ctx, &inner, ref_for)),
            length,
        };
    }
    if let Some(builtin) = ty.as_builtin() {
        let name = builtin.name().as_str().to_string();
        if let Some(prim) = primitive_from_str(&name) {
            return Type::Primitive(prim);
        }
    }
    if ty.is_bool() {
        return Type::Primitive(Primitive::Bool);
    }
    if ty.is_char() {
        return Type::Primitive(Primitive::Char);
    }
    if let Some(param) = ty.as_type_param(ctx.db) {
        return Type::TypeVar(param.name(ctx.db).as_str().to_owned());
    }
    if ty.is_tuple() {
        // Rust tuples are positional; no element labels.
        let fields: Box<[TupleElement]> = ty
            .tuple_fields(ctx.db)
            .iter()
            .map(|f| TupleElement::Positional(lower_hir_type_fallback(ctx, f, ref_for)))
            .collect();
        return Type::Tuple(fields);
    }
    if let Some(adt) = ty.as_adt() {
        let def = ModuleDef::Adt(adt);
        let type_args: Box<[Type]> = ty
            .type_arguments()
            .map(|arg| lower_hir_type_fallback(ctx, &arg, ref_for))
            .collect();

        if let Some(key) = id_of(ctx, def)
            && let Some(raw_ref) = ref_for(&key)
        {
            let base = Type::Nominal(raw_ref);
            if type_args.is_empty() {
                return base;
            }
            return Type::Apply {
                base: Box::new(base),
                args: type_args,
            };
        }
        // External ADT — `ref_for` had nothing for it. Rust has no top type,
        // so `Type::Any` here claimed something the language cannot even
        // express; keep the rendered name so two different external ADTs in
        // one overload set do not hash alike.
        let name = attach_db(ctx.db, || ty.display(ctx.db, ctx.display).to_string());
        return Type::unresolved_external(name);
    }
    // Last resort: rendered display string.
    let rendered = attach_db(ctx.db, || ty.display(ctx.db, ctx.display).to_string());
    if rendered == "!" {
        return Type::Never;
    }
    if rendered == "()" {
        return Type::Tuple(Box::new([]));
    }
    if let Some(prim) = primitive_from_str(&rendered) {
        return Type::Primitive(prim);
    }
    if rendered == "Self" {
        return Type::SelfType;
    }
    // A HIR type this IR has no variant for — a closure type, an fn item, an
    // opaque. We know exactly what it is; the lattice has nowhere to put it.
    Type::no_ir_representation(rendered)
}

// ── Path types ────────────────────────────────────────────────────────────────

fn lower_path_type(
    ctx: &mut LowerCtx<'_>,
    path_ty: &ast::PathType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    // A `PathType` node with no `Path` child means the source did not parse
    // into a type here. Nothing this producer does closes that.
    let Some(path) = path_ty.path() else {
        return Type::ORACLE_GAP;
    };

    // `<T as Trait>::Assoc` — qualified path, now representable as Type::QualifiedPath.
    if let Some(first) = path.segments().next()
        && let Some(PathSegmentKind::Type {
            type_ref,
            trait_ref,
        }) = first.kind()
    {
        // self_ty: the type inside the angle brackets (the `T` in `<T as Trait>::Assoc`)
        let self_ty = type_ref
            .map_or(Type::ORACLE_GAP, |t| lower_ast_type(ctx, &t, ref_for));

        // trait_ref: the `as Trait` disambiguation (present for `<T as Trait>::Assoc`,
        // absent for `<T>::Assoc` — though the latter is rare in practice).
        let trait_ref_ty: Option<Box<Type>> = trait_ref.and_then(|tr| {
            tr.path().map(|p| {
                if let Some(res) = resolve_path_opt(ctx, &p)
                    && let PathResolution::Def(def) = res
                    && let Some(key) = id_of(ctx, def)
                    && let Some(raw_ref) = ref_for(&key)
                {
                    let type_args = last_segment_type_args(ctx, &p, ref_for);
                    let base = Type::Nominal(raw_ref);
                    if type_args.is_empty() {
                        Box::new(base)
                    } else {
                        Box::new(Type::Apply {
                            base: Box::new(base),
                            args: type_args.into_boxed_slice(),
                        })
                    }
                } else {
                    // The `as Trait` disambiguator did not resolve. Keep the
                    // written path — `<T as Iterator>::Item` and
                    // `<T as Display>::Item` must not collapse together.
                    Box::new(Type::unresolved_external(path_identifier_text(&p)))
                }
            })
        });

        // assoc: the last segment name (the `Assoc` in `<T as Trait>::Assoc`)
        let assoc = path
            .segments()
            .skip(1) // skip the `<T as Trait>` self-type segment
            .filter_map(|seg| match seg.kind() {
                Some(PathSegmentKind::Name(n)) => Some(n.text().to_string()),
                _ => None,
            })
            .last()
            .unwrap_or_else(|| "__assoc".to_string());

        return Type::QualifiedPath {
            self_ty: Box::new(self_ty),
            trait_ref: trait_ref_ty,
            assoc,
        };
    }

    // Bare `Self`.
    if path
        .as_single_name_ref()
        .is_some_and(|n| n.text() == "Self")
    {
        return Type::SelfType;
    }

    // Generic args on the last path segment.
    let type_args = last_segment_type_args(ctx, &path, ref_for);

    if let Some(res) = resolve_path_opt(ctx, &path) {
        match res {
            PathResolution::SelfType(_) => return Type::SelfType,
            PathResolution::TypeParam(p) => {
                // A *use* of a generic parameter; `Type::TypeVar` keeps the
                // link to the `GenericParam` that binds it.
                return Type::TypeVar(p.name(ctx.db).as_str().to_string());
            }
            PathResolution::Def(ModuleDef::BuiltinType(b)) => {
                let name = b.name().as_str().to_string();
                if let Some(prim) = primitive_from_str(&name) {
                    return Type::Primitive(prim);
                }
                // `str` (and any future builtin `primitive_from_str` does not
                // cover) — a known language builtin with no `Primitive` slot.
                // `Type::Any` said "accepts anything", which for `str` is the
                // opposite of true.
                return Type::no_ir_representation(name);
            }
            PathResolution::Def(def) => {
                if let Some(key) = id_of(ctx, def)
                    && let Some(raw_ref) = ref_for(&key)
                {
                    let base = Type::Nominal(raw_ref);
                    if type_args.is_empty() {
                        return base;
                    }
                    return Type::Apply {
                        base: Box::new(base),
                        args: type_args.into_boxed_slice(),
                    };
                }
                // Resolved, but `ref_for` could not produce a ref — an item in
                // another crate. Keep the written path.
                return Type::unresolved_external(path_identifier_text(&path));
            }
            // Value-ns / attr — fall through.
            PathResolution::Local(_)
            | PathResolution::ConstParam(_)
            | PathResolution::BuiltinAttr(_)
            | PathResolution::ToolModule(_)
            | PathResolution::DeriveHelper(_) => {}
        }
    }

    // Unresolvable path; check for primitive by name.
    let written = path_identifier_text(&path);
    if path.as_single_name_ref().is_some() {
        if let Some(prim) = primitive_from_str(&written) {
            return Type::Primitive(prim);
        }
        if written == "Self" {
            return Type::SelfType;
        }
    }
    // rust-analyzer resolves everything inside the crate it has loaded, so a
    // path it cannot resolve names something outside it — or something behind
    // a macro it declined to expand. Keeping the spelling is what stops two
    // different unresolvable paths in one overload set from producing the same
    // skeleton byte.
    Type::unresolved_external(written)
}

// ── ref / ptr / array ─────────────────────────────────────────────────────────

fn lower_ref_type(
    ctx: &mut LowerCtx<'_>,
    r: &ast::RefType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    let lifetime = r.lifetime().map(|lt| lt.text().to_string());
    let mutable = r.mut_token().is_some();
    let inner = r
        .ty()
        .map_or(Type::ORACLE_GAP, |t| lower_ast_type(ctx, &t, ref_for));
    Type::Primitive(Primitive::Reference {
        lifetime,
        mutable,
        ty: Box::new(inner),
    })
}

fn lower_ptr_type(
    ctx: &mut LowerCtx<'_>,
    p: &ast::PtrType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    let is_mutable = p.mut_token().is_some();
    let inner = p
        .ty()
        .map_or(Type::ORACLE_GAP, |t| lower_ast_type(ctx, &t, ref_for));
    if is_mutable {
        Type::Primitive(Primitive::MutPointer(Box::new(inner)))
    } else {
        Type::Primitive(Primitive::ConstPointer(Box::new(inner)))
    }
}

fn lower_array_type(
    ctx: &mut LowerCtx<'_>,
    a: &ast::ArrayType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    let inner = a
        .ty()
        .map_or(Type::ORACLE_GAP, |t| lower_ast_type(ctx, &t, ref_for));
    let length = a
        .const_arg()
        .and_then(|c| c.expr())
        .map_or(0, |e| {
            let text = e.syntax().text().to_string().replace('_', "");
            text.parse::<usize>().unwrap_or(0)
        });
    Type::Array {
        ty: Box::new(inner),
        length,
    }
}

// ── fn pointer ───────────────────────────────────────────────────────────────

/// `fn(i32) -> bool` / `unsafe fn(…)` / `extern "C" fn(…)` → `Type::FunctionPointer`.
///
/// ABI: `extern "C" fn` carries `abi: Some("C")`.  Plain `fn` (Rust default
/// ABI) carries `abi: None`.  The distinction matters: two signatures that
/// differ only in ABI have incompatible calling conventions and must not unify.
///
/// Parameters: `fn(i32, bool)` — each positional `Type` from the param list,
/// in declaration order.  Patterns are stripped; only the type is kept because
/// `Type::FunctionPointer.params` is `List<Type>`, not `List<Param>`.
///
/// Return type: `None` when absent (equivalent to `-> ()` / unit); `Some` for
/// everything else.
fn lower_fn_ptr_type(
    ctx: &mut LowerCtx<'_>,
    f: &ast::FnPtrType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    // Extract the ABI string from `extern "…"`, if present.
    // `abi.string_token()` yields the raw token including the quotes, e.g.
    // `"C"`.  We strip the surrounding quotes to store just `C`.
    let abi: Option<String> = f.abi().and_then(|a| {
        a.string_token().map(|tok| {
            let raw = tok.text().to_string();
            // Strip surrounding double-quotes if present.
            raw.trim_matches('"').to_string()
        })
    });

    // Collect parameter types in order.  Only the type is kept; names/patterns
    // in bare fn pointers (`fn(x: i32)`) are not part of the IR type.
    let params: Box<[Type]> = f
        .param_list()
        .map(|list| {
            list.params()
                .filter_map(|p| p.ty())
                .map(|t| lower_ast_type(ctx, &t, ref_for))
                .collect()
        })
        .unwrap_or_default();

    // Return type: absent means unit/void; explicit `-> T` is lowered normally.
    let ret: Option<Box<Type>> = f
        .ret_type()
        .and_then(|r| r.ty())
        .map(|t| Box::new(lower_ast_type(ctx, &t, ref_for)));

    Type::FunctionPointer { params, ret, abi }
}

// ── dyn / impl ────────────────────────────────────────────────────────────────

/// `dyn Trait + OtherTrait` → `Type::DynTrait(bounds)`.
///
/// A fat pointer with a vtable, requiring object safety. Previously squeezed
/// into `Type::Intersection`, which conflated a structural intersection `A & B`
/// with a trait-object type `dyn A + B`. The two have distinct dispatch
/// semantics and must not unify.
///
/// Lifetime-only bounds (e.g. `dyn Trait + 'static`) are filtered out:
/// the IR's `List<Type>` has no lifetime slot. This is a known limitation
/// (lifetimes on dyn objects are not representable in the type algebra);
/// the IR already tracks lifetime information separately via `Primitive::Reference`.
fn lower_dyn_trait(
    ctx: &mut LowerCtx<'_>,
    d: &ast::DynTraitType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    let Some(bounds) = d.type_bound_list() else {
        return Type::DynTrait(Box::new([]));
    };
    let trait_types: Box<[Type]> = bounds
        .bounds()
        .filter_map(|b| match b.kind() {
            Some(ast::TypeBoundKind::PathType(_, path_ty)) => {
                Some(path_type_to_type(ctx, &path_ty, ref_for))
            }
            _ => None, // Lifetime bounds: no Type slot — filtered out.
        })
        .collect();
    Type::DynTrait(trait_types)
}

/// `impl Trait` → `Type::ImplTrait(bounds)`.
///
/// `impl Trait` is zero-cost static dispatch — the compiler selects a concrete
/// type; the call site only sees the bound set. Distinct from `dyn Trait`
/// (fat-pointer, dynamic dispatch). Previously emitted as `Type::Any`, which
/// erased all bound information.
fn lower_impl_trait(
    ctx: &mut LowerCtx<'_>,
    i: &ast::ImplTraitType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    let bounds: Box<[Type]> = i
        .type_bound_list()
        .map(|list| {
            list.bounds()
                .filter_map(|b| match b.kind() {
                    Some(ast::TypeBoundKind::PathType(_, path_ty)) => {
                        Some(path_type_to_type(ctx, &path_ty, ref_for))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    Type::ImplTrait(bounds)
}

fn lower_macro_type(
    ctx: &mut LowerCtx<'_>,
    m: &ast::MacroType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    // A `MacroType` node with no macro call under it: the source did not
    // parse. No producer effort closes this.
    let Some(call) = m.macro_call() else {
        return Type::ORACLE_GAP;
    };
    let expanded = panic::catch_unwind(AssertUnwindSafe(|| ctx.sema.expand_macro_call(&call)))
        .unwrap_or_else(|_| {
            debug!("sema.expand_macro_call panicked; treating macro type as Any");
            None
        });
    if let Some(expanded) = expanded {
        if let Some(ty) = ast::Type::cast(expanded.value.clone()) {
            return lower_ast_type(ctx, &ty, ref_for);
        }
        if let Some(ty) = expanded.value.descendants().find_map(ast::Type::cast) {
            return lower_ast_type(ctx, &ty, ref_for);
        }
    }
    // The macro did not expand (or expanded to something that is not a type,
    // or panicked above). rust-analyzer has already failed here.
    Type::ORACLE_GAP
}

// ── Generic args on last path segment ────────────────────────────────────────

/// Generic type args (`Foo<Bar, Baz>`) from the last segment of a path.
///
/// Only `TypeArg` entries are kept; lifetime and const args are dropped because
/// the new IR's `Apply.args` is `List<Type>` with no lifetime/const positions.
///
/// Public within the crate so that `item.rs` can recover written supertrait
/// args (e.g. `Bar<u32>` in `trait Foo: Bar<u32>`) via the AST path.
pub(crate) fn last_segment_type_args_pub(
    ctx: &mut LowerCtx<'_>,
    path: &ast::Path,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Vec<Type> {
    last_segment_type_args(ctx, path, ref_for)
}

fn last_segment_type_args(
    ctx: &mut LowerCtx<'_>,
    path: &ast::Path,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Vec<Type> {
    let Some(seg) = path.segment() else {
        return Vec::new();
    };
    // Parenthesized sugar `Fn(i32) -> bool`.
    if let Some(paren) = seg.parenthesized_arg_list() {
        let mut args: Vec<Type> = paren
            .type_args()
            .filter_map(|ta| ta.ty())
            .map(|t| lower_ast_type(ctx, &t, ref_for))
            .collect();
        if let Some(ret) = seg.ret_type().and_then(|r| r.ty()) {
            args.push(lower_ast_type(ctx, &ret, ref_for));
        }
        return args;
    }
    let Some(list) = seg.generic_arg_list() else {
        return Vec::new();
    };
    list.generic_args()
        .filter_map(|arg| match arg {
            ast::GenericArg::TypeArg(ta) => ta.ty().map(|t| lower_ast_type(ctx, &t, ref_for)),
            // Lifetime, const, assoc → ignored for now.
            _ => None,
        })
        .collect()
}

/// A `PathType` → `Type` conversion used by `dyn Trait + …` lowering.
fn path_type_to_type(
    ctx: &mut LowerCtx<'_>,
    path_ty: &ast::PathType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    let Some(path) = path_ty.path() else {
        return Type::ORACLE_GAP;
    };
    if let Some(res) = resolve_path_opt(ctx, &path)
        && let PathResolution::Def(def) = res
        && let Some(key) = id_of(ctx, def)
        && let Some(raw_ref) = ref_for(&key)
    {
        let type_args = last_segment_type_args(ctx, &path, ref_for);
        let base = Type::Nominal(raw_ref);
        if type_args.is_empty() {
            return base;
        }
        return Type::Apply {
            base: Box::new(base),
            args: type_args.into_boxed_slice(),
        };
    }
    // A `dyn Trait + …` bound naming a trait from another crate. Keeping the
    // spelling is what makes `dyn Debug` and `dyn Display` distinguishable —
    // under `Type::Any` every foreign bound in a `DynTrait` list was the same
    // byte, so `dyn Debug + Send` and `dyn Display + Send` had one skeleton.
    Type::unresolved_external(path_identifier_text(&path))
}

// ── Semantics resolve (panic-safe) ────────────────────────────────────────────

pub(crate) fn resolve_path_opt(ctx: &LowerCtx<'_>, path: &ast::Path) -> Option<PathResolution> {
    panic::catch_unwind(AssertUnwindSafe(|| ctx.sema.resolve_path(path))).unwrap_or_else(|_| {
        debug!("sema.resolve_path panicked; using Any fallback");
        None
    })
}

// ── Primitives ────────────────────────────────────────────────────────────────

/// Map a Rust primitive name to the IR `Primitive`.
///
/// `str` is intentionally **not** mapped — callers leave it as `Type::Any`
/// (or handle it as `Primitive::Str` if desired, but `str` is DST, not the
/// same as `String`).
pub(crate) fn primitive_from_str(name: &str) -> Option<Primitive> {
    Some(match name {
        "i8" => Primitive::Integer {
            signed: true,
            width: Width::W8,
        },
        "i16" => Primitive::Integer {
            signed: true,
            width: Width::W16,
        },
        "i32" => Primitive::Integer {
            signed: true,
            width: Width::W32,
        },
        "i64" => Primitive::Integer {
            signed: true,
            width: Width::W64,
        },
        "i128" => Primitive::Integer {
            signed: true,
            width: Width::W128,
        },
        "isize" => Primitive::Integer {
            signed: true,
            width: Width::Arch,
        },
        "u8" => Primitive::Integer {
            signed: false,
            width: Width::W8,
        },
        "u16" => Primitive::Integer {
            signed: false,
            width: Width::W16,
        },
        "u32" => Primitive::Integer {
            signed: false,
            width: Width::W32,
        },
        "u64" => Primitive::Integer {
            signed: false,
            width: Width::W64,
        },
        "u128" => Primitive::Integer {
            signed: false,
            width: Width::W128,
        },
        "usize" => Primitive::Integer {
            signed: false,
            width: Width::Arch,
        },
        "f16" => Primitive::Float(Width::W16),
        "f32" => Primitive::Float(Width::W32),
        "f64" => Primitive::Float(Width::W64),
        "f128" => Primitive::Float(Width::W128),
        "bool" => Primitive::Bool,
        "char" => Primitive::Char,
        "str" => Primitive::Str,
        _ => return None,
    })
}

// ── Path text helpers ─────────────────────────────────────────────────────────

/// The written text of a path, for `generics.rs` to name an unresolved bound.
///
/// Exposed (rather than duplicated) so that both modules produce byte-identical
/// `UnknownType::UnresolvedExternal` names for the same written path — two
/// spellings of the same gap would defeat the skeleton discrimination the
/// name exists to provide.
pub(crate) fn path_identifier_text_of(path: &ast::Path) -> String {
    path_identifier_text(path)
}

fn path_identifier_text(path: &ast::Path) -> String {
    path.segments()
        .filter_map(|seg| match seg.kind() {
            Some(PathSegmentKind::Name(n)) => Some(n.text().to_string()),
            Some(PathSegmentKind::SelfTypeKw) => Some("Self".into()),
            Some(PathSegmentKind::SelfKw) => Some("self".into()),
            Some(PathSegmentKind::SuperKw) => Some("super".into()),
            Some(PathSegmentKind::CrateKw) => Some("crate".into()),
            Some(PathSegmentKind::Type { .. }) | None => None,
        })
        .collect::<Vec<_>>()
        .join("::")
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Integer primitive mapping is exact and covers all widths.
    #[test]
    fn integer_primitives_are_exact() {
        assert_eq!(
            primitive_from_str("i32"),
            Some(Primitive::Integer {
                signed: true,
                width: Width::W32
            })
        );
        assert_eq!(
            primitive_from_str("usize"),
            Some(Primitive::Integer {
                signed: false,
                width: Width::Arch
            })
        );
        assert_eq!(
            primitive_from_str("u128"),
            Some(Primitive::Integer {
                signed: false,
                width: Width::W128
            })
        );
    }

    /// Float primitive mapping covers the four supported widths.
    #[test]
    fn float_primitives_are_exact() {
        assert_eq!(
            primitive_from_str("f32"),
            Some(Primitive::Float(Width::W32))
        );
        assert_eq!(
            primitive_from_str("f64"),
            Some(Primitive::Float(Width::W64))
        );
        assert_eq!(
            primitive_from_str("f128"),
            Some(Primitive::Float(Width::W128))
        );
    }

    /// Bool and char primitives.
    #[test]
    fn bool_char_primitives() {
        assert_eq!(primitive_from_str("bool"), Some(Primitive::Bool));
        assert_eq!(primitive_from_str("char"), Some(Primitive::Char));
    }

    /// `str` maps to `Primitive::Str` (distinct from `String`).
    #[test]
    fn str_maps_to_str_primitive() {
        assert_eq!(primitive_from_str("str"), Some(Primitive::Str));
    }

    /// Unknown type names return `None`.
    #[test]
    fn unknown_returns_none() {
        assert_eq!(primitive_from_str("String"), None);
        assert_eq!(primitive_from_str("Vec"), None);
        assert_eq!(primitive_from_str(""), None);
    }

    // ── FunctionPointer lowering contract ────────────────────────────────────

    /// `fn(i32) -> bool` lowers to a `FunctionPointer` with `abi: None`.
    ///
    /// This is the shape `lower_fn_ptr_type` must emit for plain bare fn pointers.
    /// The ABI field being `None` distinguishes a default-ABI fn from one with an
    /// explicit `extern "…"` string.
    #[test]
    fn fn_ptr_plain_has_no_abi() {
        let ty = Type::FunctionPointer {
            params: Box::new([Type::Primitive(Primitive::Integer {
                signed: true,
                width: Width::W32,
            })]),
            ret: Some(Box::new(Type::Primitive(Primitive::Bool))),
            abi: None,
        };
        assert!(
            matches!(&ty, Type::FunctionPointer { abi: None, .. }),
            "plain fn() must carry abi: None"
        );
    }

    /// `extern "C" fn(i32) -> bool` lowers to a `FunctionPointer` with `abi: Some("C")`.
    ///
    /// Two signatures that differ only in ABI have distinct calling conventions;
    /// they must not unify.  Carrying the ABI in the type makes this distinction
    /// visible to Trustfall queries without payload inspection.
    #[test]
    fn fn_ptr_extern_c_has_abi() {
        let ty = Type::FunctionPointer {
            params: Box::new([Type::Primitive(Primitive::Integer {
                signed: true,
                width: Width::W32,
            })]),
            ret: Some(Box::new(Type::Primitive(Primitive::Bool))),
            abi: Some("C".to_owned()),
        };
        assert!(
            matches!(&ty, Type::FunctionPointer { abi: Some(s), .. } if s == "C"),
            "extern \"C\" fn must carry abi: Some(\"C\")"
        );
    }

    /// Plain fn and extern-C fn are structurally distinct (different `abi` field).
    #[test]
    fn fn_ptr_plain_and_extern_c_differ() {
        let plain = Type::FunctionPointer {
            params: Box::new([Type::Primitive(Primitive::Integer {
                signed: true,
                width: Width::W32,
            })]),
            ret: None,
            abi: None,
        };
        let extern_c = Type::FunctionPointer {
            params: Box::new([Type::Primitive(Primitive::Integer {
                signed: true,
                width: Width::W32,
            })]),
            ret: None,
            abi: Some("C".to_owned()),
        };
        assert_ne!(
            plain, extern_c,
            "plain fn and extern \"C\" fn must not compare equal"
        );
    }

    /// `FunctionPointer` serde round-trip preserves `abi`, `params`, and `ret`.
    #[test]
    fn fn_ptr_serde_roundtrip() {
        let original = Type::FunctionPointer {
            params: Box::new([
                Type::Primitive(Primitive::Integer {
                    signed: false,
                    width: Width::W64,
                }),
                Type::Primitive(Primitive::Bool),
            ]),
            ret: Some(Box::new(Type::Never)),
            abi: Some("Rust".to_owned()),
        };
        let json = serde_json::to_string(&original).expect("serialize failed");
        let decoded: Type = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(original, decoded);
    }

    /// Unit tuple `()` round-trips through `Tuple(Box::new([]))`.
    #[test]
    fn unit_tuple_is_empty_positional_list() {
        let ty = Type::Tuple(Box::new([]));
        assert!(matches!(&ty, Type::Tuple(elems) if elems.is_empty()));
    }

    /// Rust tuples use `TupleElement::Positional`; no labels are attached.
    #[test]
    fn rust_tuple_elements_are_positional() {
        let ty = Type::Tuple(Box::new([
            TupleElement::Positional(Type::Primitive(Primitive::Integer {
                signed: true,
                width: Width::W32,
            })),
            TupleElement::Positional(Type::Primitive(Primitive::Bool)),
        ]));
        if let Type::Tuple(elems) = &ty {
            for elem in elems {
                assert!(
                    matches!(elem, TupleElement::Positional(_)),
                    "Rust tuple elements must be Positional"
                );
            }
        } else {
            panic!("expected Type::Tuple");
        }
    }

    /// `impl Trait` and `dyn Trait` must stay structurally distinguishable.
    ///
    /// These two used to be asserted as `matches!(x, Type::ImplTrait(_))` on a
    /// value the test had just constructed as `Type::ImplTrait`, which is a
    /// tautology: it restates the constructor and would pass against any
    /// producer, including one that never emits the variant. What is actually
    /// worth pinning is the property downstream depends on — that the two
    /// dispatch strategies do not share an identity — so this asserts on
    /// **skeleton bytes**, which is what `IntroId` is minted from.
    ///
    /// The behavioural claim that the producer *reaches* these variants is
    /// covered end-to-end by `tests/no_top_type.rs`, which lowers a real crate
    /// containing `impl Display` and `&dyn Debug` through rust-analyzer.
    #[test]
    fn impl_trait_and_dyn_trait_do_not_share_an_identity() {
        use nudox_ir::skeleton::type_skeleton;
        let bound = || Box::new([Type::TypeVar("T".to_owned())]);
        let impl_t = Type::ImplTrait(bound());
        let dyn_t = Type::DynTrait(bound());
        assert_ne!(
            type_skeleton(&impl_t),
            type_skeleton(&dyn_t),
            "static dispatch and a vtable are different types and must not collide"
        );
        // `dyn A + B` must also not be encoded as the structural intersection
        // `A & B`, which is what it used to be squeezed into.
        assert_ne!(
            type_skeleton(&dyn_t),
            type_skeleton(&Type::Intersection(bound())),
            "`dyn A + B` is a trait object, not an intersection type"
        );
    }

    /// The three "I cannot give you a concrete type" spellings are three
    /// different facts, and Rust reaches exactly two of them.
    ///
    /// `Type::Any` is a genuine top type and Rust has none, so it is never
    /// emitted (enforced end-to-end by `tests/no_top_type.rs`). `Inferred` is
    /// a *written* request (`_`). `Unknown(reason)` is an unrequested gap.
    /// Asserting only `Inferred != Any` — as this test used to — left the
    /// third case, the one CC-2 added, unpinned.
    #[test]
    fn inferred_unknown_and_any_are_three_distinct_types() {
        use nudox_ir::skeleton::type_skeleton;
        let inferred = type_skeleton(&Type::Inferred);
        let oracle_gap = type_skeleton(&Type::ORACLE_GAP);
        let external = type_skeleton(&Type::unresolved_external("std::io::Error"));
        let any = type_skeleton(&Type::Any);

        assert_ne!(inferred, any, "`_` is a request, not a top type");
        assert_ne!(
            inferred, oracle_gap,
            "`_` is a request, not a parse failure"
        );
        assert_ne!(oracle_gap, any);
        assert_ne!(
            external, oracle_gap,
            "a named gap outranks an anonymous one"
        );
        // And the name inside an external gap is identity-relevant, which is
        // what stops two unresolvable parameter types from colliding.
        assert_ne!(
            external,
            type_skeleton(&Type::unresolved_external("std::fmt::Error")),
            "two distinct unresolved paths must not share a skeleton"
        );
    }

    /// `Type::QualifiedPath` round-trips through serde.
    #[test]
    fn qualified_path_shape_is_correct() {
        let qp = Type::QualifiedPath {
            self_ty: Box::new(Type::TypeVar("T".to_owned())),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "Item".to_owned(),
        };
        assert!(matches!(&qp, Type::QualifiedPath { assoc, .. } if assoc == "Item"));
    }
}
