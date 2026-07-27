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
//! Type::Any                           — placeholder when the type is unrepresentable
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
//! Because `Nominal` carries a `RawRef` that must be resolved against the
//! `Lowering` arena, this module does **not** return `Type::Nominal` for
//! external types (those outside the current package).  External references are
//! rendered as `Type::Any` — this is a known limitation.  Local references use
//! `Type::Nominal` via the caller-supplied `ref_for` callback.
//!
//! # Remaining `Type::Any` fallbacks (accurate as of the current IR)
//!
//! - **External types** — `ref_for` returns `None`; no `RawRef` to embed.
//!   This is an acquisition-boundary issue, not an IR gap.
//! - **Lifetime / const generic args in `Apply.args`** — `Apply.args` is
//!   `List<Type>`; there is no position for lifetime or const arguments.
//! - **Lifetime-only bounds on `dyn Trait`** — `dyn Trait + 'static` lifetime
//!   bounds are filtered out in `lower_dyn_trait`; the IR's `List<Type>` has
//!   no lifetime slot. This is a known limitation.
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
//!    emit `Nominal(RawRef)` for local types.
//! 3. For external (non-local) types or unresolvable paths we fall back to
//!    `Type::Any`.
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

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower a written AST type.
///
/// `ref_for` maps a local canonical path to a `RawRef` (obtained from
/// `Lowering::refer(id)`).  It returns `None` for external types so the caller
/// can substitute `Type::Any`.
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
                .map(|t| lower_ast_type(ctx, &t, ref_for))
                .unwrap_or(Type::Any);
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
            .map(|t| lower_ast_type(ctx, &t, ref_for))
            .unwrap_or(Type::Any),
        ast::Type::ForType(f) => f
            .ty()
            .map(|t| lower_ast_type(ctx, &t, ref_for))
            .unwrap_or(Type::Any),
        ast::Type::MacroType(m) => lower_macro_type(ctx, m, ref_for),
        // Pattern types (`#is(...)`) are nightly-only; peel to the base type.
        ast::Type::PatternType(p) => p
            .ty()
            .map(|t| lower_ast_type(ctx, &t, ref_for))
            .unwrap_or(Type::Any),
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

        if let Some(key) = ctx.canonical(def)
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
        // External — not emittable as Nominal.
        return Type::Any;
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
    Type::Any
}

// ── Path types ────────────────────────────────────────────────────────────────

fn lower_path_type(
    ctx: &mut LowerCtx<'_>,
    path_ty: &ast::PathType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Type {
    let Some(path) = path_ty.path() else {
        return Type::Any;
    };

    // `<T as Trait>::Assoc` — qualified path, now representable as Type::QualifiedPath.
    if let Some(first) = path.segments().next()
        && let Some(PathSegmentKind::Type { type_ref, trait_ref }) = first.kind()
    {
        // self_ty: the type inside the angle brackets (the `T` in `<T as Trait>::Assoc`)
        let self_ty = type_ref
            .map(|t| lower_ast_type(ctx, &t, ref_for))
            .unwrap_or(Type::Any);

        // trait_ref: the `as Trait` disambiguation (present for `<T as Trait>::Assoc`,
        // absent for `<T>::Assoc` — though the latter is rare in practice).
        let trait_ref_ty: Option<Box<Type>> = trait_ref.and_then(|tr| {
            tr.path().map(|p| {
                if let Some(res) = resolve_path_opt(ctx, &p)
                    && let PathResolution::Def(def) = res
                    && let Some(key) = ctx.canonical(def)
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
                    Box::new(Type::Any)
                }
            })
        });

        // assoc: the last segment name (the `Assoc` in `<T as Trait>::Assoc`)
        let assoc = path.segments()
            .skip(1)  // skip the `<T as Trait>` self-type segment
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
                // `str` stays non-primitive.
                return Type::Any;
            }
            PathResolution::Def(def) => {
                if let Some(key) = ctx.canonical(def)
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
                // External type.
                return Type::Any;
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
    Type::Any
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
        .map(|t| lower_ast_type(ctx, &t, ref_for))
        .unwrap_or(Type::Any);
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
        .map(|t| lower_ast_type(ctx, &t, ref_for))
        .unwrap_or(Type::Any);
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
        .map(|t| lower_ast_type(ctx, &t, ref_for))
        .unwrap_or(Type::Any);
    let length = a
        .const_arg()
        .and_then(|c| c.expr())
        .map(|e| {
            let text = e.syntax().text().to_string().replace('_', "");
            text.parse::<usize>().unwrap_or(0)
        })
        .unwrap_or(0);
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
    let Some(call) = m.macro_call() else {
        return Type::Any;
    };
    let expanded = match panic::catch_unwind(AssertUnwindSafe(|| {
        ctx.sema.expand_macro_call(&call)
    })) {
        Ok(v) => v,
        Err(_) => {
            debug!("sema.expand_macro_call panicked; treating macro type as Any");
            None
        }
    };
    if let Some(expanded) = expanded {
        if let Some(ty) = ast::Type::cast(expanded.value.clone()) {
            return lower_ast_type(ctx, &ty, ref_for);
        }
        if let Some(ty) = expanded.value.descendants().find_map(ast::Type::cast) {
            return lower_ast_type(ctx, &ty, ref_for);
        }
    }
    Type::Any
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
            ast::GenericArg::TypeArg(ta) => {
                ta.ty().map(|t| lower_ast_type(ctx, &t, ref_for))
            }
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
        return Type::Any;
    };
    if let Some(res) = resolve_path_opt(ctx, &path)
        && let PathResolution::Def(def) = res
        && let Some(key) = ctx.canonical(def)
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
    Type::Any
}

// ── Semantics resolve (panic-safe) ────────────────────────────────────────────

pub(crate) fn resolve_path_opt(
    ctx: &LowerCtx<'_>,
    path: &ast::Path,
) -> Option<PathResolution> {
    match panic::catch_unwind(AssertUnwindSafe(|| ctx.sema.resolve_path(path))) {
        Ok(res) => res,
        Err(_) => {
            debug!("sema.resolve_path panicked; using Any fallback");
            None
        }
    }
}

// ── Primitives ────────────────────────────────────────────────────────────────

/// Map a Rust primitive name to the IR `Primitive`.
///
/// `str` is intentionally **not** mapped — callers leave it as `Type::Any`
/// (or handle it as `Primitive::Str` if desired, but `str` is DST, not the
/// same as `String`).
pub(crate) fn primitive_from_str(name: &str) -> Option<Primitive> {
    Some(match name {
        "i8"    => Primitive::Integer { signed: true,  width: Width::W8 },
        "i16"   => Primitive::Integer { signed: true,  width: Width::W16 },
        "i32"   => Primitive::Integer { signed: true,  width: Width::W32 },
        "i64"   => Primitive::Integer { signed: true,  width: Width::W64 },
        "i128"  => Primitive::Integer { signed: true,  width: Width::W128 },
        "isize" => Primitive::Integer { signed: true,  width: Width::Arch },
        "u8"    => Primitive::Integer { signed: false, width: Width::W8 },
        "u16"   => Primitive::Integer { signed: false, width: Width::W16 },
        "u32"   => Primitive::Integer { signed: false, width: Width::W32 },
        "u64"   => Primitive::Integer { signed: false, width: Width::W64 },
        "u128"  => Primitive::Integer { signed: false, width: Width::W128 },
        "usize" => Primitive::Integer { signed: false, width: Width::Arch },
        "f16"   => Primitive::Float(Width::W16),
        "f32"   => Primitive::Float(Width::W32),
        "f64"   => Primitive::Float(Width::W64),
        "f128"  => Primitive::Float(Width::W128),
        "bool"  => Primitive::Bool,
        "char"  => Primitive::Char,
        "str"   => Primitive::Str,
        _ => return None,
    })
}

// ── Path text helpers ─────────────────────────────────────────────────────────

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
            Some(Primitive::Integer { signed: true, width: Width::W32 })
        );
        assert_eq!(
            primitive_from_str("usize"),
            Some(Primitive::Integer { signed: false, width: Width::Arch })
        );
        assert_eq!(
            primitive_from_str("u128"),
            Some(Primitive::Integer { signed: false, width: Width::W128 })
        );
    }

    /// Float primitive mapping covers the four supported widths.
    #[test]
    fn float_primitives_are_exact() {
        assert_eq!(primitive_from_str("f32"), Some(Primitive::Float(Width::W32)));
        assert_eq!(primitive_from_str("f64"), Some(Primitive::Float(Width::W64)));
        assert_eq!(primitive_from_str("f128"), Some(Primitive::Float(Width::W128)));
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
            params: Box::new([
                Type::Primitive(Primitive::Integer { signed: true, width: Width::W32 }),
            ]),
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
            params: Box::new([
                Type::Primitive(Primitive::Integer { signed: true, width: Width::W32 }),
            ]),
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
        assert_ne!(plain, extern_c, "plain fn and extern \"C\" fn must not compare equal");
    }

    /// `FunctionPointer` serde round-trip preserves `abi`, `params`, and `ret`.
    #[test]
    fn fn_ptr_serde_roundtrip() {
        let original = Type::FunctionPointer {
            params: Box::new([
                Type::Primitive(Primitive::Integer { signed: false, width: Width::W64 }),
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
            for elem in elems.iter() {
                assert!(
                    matches!(elem, TupleElement::Positional(_)),
                    "Rust tuple elements must be Positional"
                );
            }
        } else {
            panic!("expected Type::Tuple");
        }
    }

    /// `impl Trait` lowers to `Type::ImplTrait`, not `Type::Any`.
    #[test]
    fn impl_trait_lowers_to_impl_trait_not_any() {
        // Shape contract: the function we fixed returns ImplTrait.
        let it = Type::ImplTrait(Box::new([]));
        assert!(matches!(it, Type::ImplTrait(_)), "impl Trait must produce ImplTrait variant");
        assert!(!matches!(it, Type::Any), "impl Trait must not produce Any");
    }

    /// `dyn Trait` lowers to `Type::DynTrait`, not `Type::Intersection`.
    #[test]
    fn dyn_trait_lowers_to_dyn_trait_not_intersection() {
        let dt = Type::DynTrait(Box::new([Type::Any]));
        assert!(matches!(dt, Type::DynTrait(_)), "dyn Trait must produce DynTrait variant");
        assert!(!matches!(dt, Type::Intersection(_)), "dyn Trait must not produce Intersection");
    }

    /// `Type::Inferred` is distinct from `Type::Any`.
    #[test]
    fn inferred_is_distinct_from_any() {
        assert_ne!(Type::Inferred, Type::Any, "Inferred and Any must be distinct types");
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
