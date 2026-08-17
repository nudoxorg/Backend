//! Function / method lowering → `nudox_ir::kinds::Function` + `Param` entries.
//!
//! # What changed vs the old function.rs
//!
//! The new `Function` kind carries:
//! - `receiver: Option<Receiver>` — enum, same variants.
//! - `input_params: List<Ref<Param>>` — Param entries are separate IR entries.
//! - `output_params: List<Ref<Param>>` — ditto.
//! - `modifiers: List<FnModifier>` — `Async`, `Const`, `Unsafe`, `Generator`.
//! - `generics: List<GenericParam>` and `wheres: List<WherePred>`.
//! - `abi: Option<String>` — `extern "C"` etc.
//! - `is_defaulted: bool` — trait default bodies.
//!
//! Parameters are now **separate IR entries** (kind = `Param`).  The item
//! lowerer must declare them into `Lowering` before calling this module, then
//! pass the resulting `Ref<Param>` slices here.  This module returns the
//! built `Function` body + the param data so the caller can declare them.
//!
//! **Memory:** Parameter types are cloned from the HIR/AST types, which are
//! small and largely stack-allocated (the `Type` enum is `Clone`).  `Box<[T]>`
//! is used for all finished lists.

use nudox_ir::{
    index::RawRef,
    kinds::{FnModifier, Function, GenericParam, ParamAttribute, Receiver, Type, WherePred},
};
use ra_ap_hir::{Access, Function as HirFunction, HasSource, SelfParam};
use ra_ap_syntax::{AstToken, ast::HasGenericParams};

use super::{
    ctx::{LowerCtx, PathKey},
    generics, ty,
};

// ── Output type ───────────────────────────────────────────────────────────────

/// Everything the item lowerer needs to emit a function into `Lowering`.
pub(crate) struct FunctionData {
    /// The `Function` kind body.
    pub(crate) body: Function,
    /// Input parameters in declaration order.
    /// Each entry is `(name, type, attributes)` for the caller to declare as a
    /// `Param` entry and then hand the `Ref<Param>` back to `Function.input_params`.
    pub(crate) input_params: Vec<ParamData>,
    /// Output parameter (the return type), if any.
    pub(crate) output_param: Option<ParamData>,
}

/// Data for a single parameter that the caller will declare as a `Param` entry.
pub(crate) struct ParamData {
    pub(crate) name: String,
    pub(crate) ty: Option<Type>,
    pub(crate) attributes: Vec<ParamAttribute>,
}

// ── Entry points ──────────────────────────────────────────────────────────────

/// Lower a free or inherent/trait-impl function into `FunctionData`.
///
/// `ref_for` supplies a `RawRef` for a local canonical path key.
pub(crate) fn lower_function(
    ctx: &mut LowerCtx<'_>,
    f: HirFunction,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Option<FunctionData> {
    let receiver = receiver_kind(ctx, f);
    let (generics, wheres) = fn_generics(ctx, f, ref_for);
    let modifiers = fn_modifiers(ctx, f);
    let abi = fn_abi(ctx, f);
    let is_defaulted = f.has_body(ctx.db) && f.module(ctx.db).krate(ctx.db) == ctx.krate;

    let input_params = lower_input_params(ctx, f, ref_for);
    let output_param = lower_output_param(ctx, f, ref_for);

    // Placeholder Refs: the caller fills these after declaring Param entries.
    // We return raw data; the caller assembles the Refs.
    let body = Function::builder()
        .maybe_receiver(receiver)
        // input_params and output_params are filled by the caller after declaring
        // Param entries; we leave them empty here and the caller uses the returned
        // `input_params` / `output_param` vectors.
        .modifiers(modifiers)
        .generics(generics)
        .wheres(wheres)
        .maybe_abi(abi)
        .is_defaulted(is_defaulted)
        .build();

    Some(FunctionData {
        body,
        input_params,
        output_param,
    })
}

// ── Receiver ─────────────────────────────────────────────────────────────────

fn receiver_kind(ctx: &LowerCtx<'_>, f: HirFunction) -> Option<Receiver> {
    let sp = f.self_param(ctx.db)?;
    Some(map_self_param(ctx, sp))
}

fn map_self_param(ctx: &LowerCtx<'_>, sp: SelfParam) -> Receiver {
    // Explicit type annotation on the self param → Arbitrary.
    if let Some(src) = ctx.sema.source(sp).or_else(|| sp.source(ctx.db))
        && src.value.ty().is_some()
    {
        return Receiver::Arbitrary;
    }
    match sp.access(ctx.db) {
        Access::Shared => Receiver::SharedRef,
        Access::Exclusive => Receiver::MutRef,
        Access::Owned => Receiver::Owned,
    }
}

// ── Modifiers / ABI ──────────────────────────────────────────────────────────

fn fn_modifiers(ctx: &LowerCtx<'_>, f: HirFunction) -> Vec<FnModifier> {
    let mut mods = Vec::new();
    if f.is_const(ctx.db) {
        mods.push(FnModifier::Const);
    }
    if f.is_async(ctx.db) {
        mods.push(FnModifier::Async);
    }
    if f.is_unsafe(ctx.db) {
        mods.push(FnModifier::Unsafe);
    }
    mods
}

/// Extract the ABI string from the function's AST, if any.
///
/// `extern "C" fn` → `Some("C")`.  Default ABI (`fn`) → `None`.
fn fn_abi(ctx: &LowerCtx<'_>, f: HirFunction) -> Option<String> {
    let src = ctx.sema.source(f).or_else(|| f.source(ctx.db))?;
    let abi = src.value.abi()?;
    abi.abi_string().map(|s| {
        // Strip the surrounding quotes from the ABI string literal.
        let text = s.text();
        let trimmed = text.trim_matches('"');
        trimmed.to_owned()
    })
}

// ── Generics ─────────────────────────────────────────────────────────────────

fn fn_generics(
    ctx: &mut LowerCtx<'_>,
    f: HirFunction,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> (Vec<GenericParam>, Vec<WherePred>) {
    let src = match ctx.sema.source(f).or_else(|| f.source(ctx.db)) {
        Some(s) => s,
        None => return (Vec::new(), Vec::new()),
    };
    generics::lower_generics(
        ctx,
        src.value.generic_param_list(),
        src.value.where_clause(),
        ref_for,
    )
}

// ── Parameters ───────────────────────────────────────────────────────────────

fn lower_input_params(
    ctx: &mut LowerCtx<'_>,
    f: HirFunction,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Vec<ParamData> {
    let hir_params = f.params_without_self(ctx.db);
    let ast_tys = ast_param_types(ctx, f, ref_for);

    hir_params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = p
                .name(ctx.db)
                .map(|n| n.as_str().to_owned())
                .unwrap_or_else(|| format!("_{i}"));
            // The HIR reports more parameters than the AST produced types for,
            // which only happens on source that did not parse cleanly.
            let param_ty = ast_tys.get(i).cloned().unwrap_or(Type::ORACLE_GAP);
            let mut attributes = Vec::new();
            // `f.is_varargs` covers variadic C-style params.
            if i + 1 == hir_params.len() && f.is_varargs(ctx.db) {
                attributes.push(ParamAttribute::Variadic);
            }
            ParamData {
                name,
                ty: Some(param_ty),
                attributes,
            }
        })
        .collect()
}

fn lower_output_param(
    ctx: &mut LowerCtx<'_>,
    f: HirFunction,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Option<ParamData> {
    let src = ctx.sema.source(f).or_else(|| f.source(ctx.db));
    let output_ty = if let Some(src) = &src
        && let Some(ret) = src.value.ret_type()
        && let Some(ty_node) = ret.ty()
    {
        let hir_ty = f.ret_type(ctx.db);
        ty::lower_type_prefer_ast(ctx, Some(&ty_node), &hir_ty, ref_for)
    } else if src.is_some() {
        // Written as `fn foo()` — implicit `()`.
        Type::Tuple(Box::new([]))
    } else {
        ty::lower_hir_type_fallback(ctx, &f.ret_type(ctx.db), ref_for)
    };

    // Unit return → no output param (matches old producer behaviour).
    match &output_ty {
        Type::Tuple(t) if t.is_empty() => None,
        _ => Some(ParamData {
            name: "return".to_owned(),
            ty: Some(output_ty),
            attributes: Vec::new(),
        }),
    }
}

/// AST types for non-self params, in declaration order.
fn ast_param_types(
    ctx: &mut LowerCtx<'_>,
    f: HirFunction,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Vec<Type> {
    let Some(src) = ctx.sema.source(f).or_else(|| f.source(ctx.db)) else {
        return f
            .params_without_self(ctx.db)
            .iter()
            .map(|p| {
                let hir_ty = p.ty();
                ty::lower_hir_type_fallback(ctx, hir_ty, ref_for)
            })
            .collect();
    };
    let Some(list) = src.value.param_list() else {
        return Vec::new();
    };
    let hir_params = f.params_without_self(ctx.db);
    list.params()
        .enumerate()
        .map(|(i, p)| match p.ty() {
            Some(t) => {
                if let Some(hp) = hir_params.get(i) {
                    let hir_ty = hp.ty();
                    ty::lower_type_prefer_ast(ctx, Some(&t), hir_ty, ref_for)
                } else {
                    ty::lower_ast_type(ctx, &t, ref_for)
                }
            }
            None => hir_params
                .get(i)
                .map(|hp| {
                    let hir_ty = hp.ty();
                    ty::lower_hir_type_fallback(ctx, hir_ty, ref_for)
                })
                // Neither AST nor HIR has a type for this parameter position.
                .unwrap_or(Type::ORACLE_GAP),
        })
        .collect()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use nudox_ir::kinds::{FnModifier, Receiver};

    /// `FnModifier` variants are well-defined.
    #[test]
    fn fn_modifier_variants() {
        let mods = [FnModifier::Async, FnModifier::Const, FnModifier::Unsafe];
        assert_eq!(mods.len(), 3);
    }

    /// `Receiver` variants are exhaustive.
    #[test]
    fn receiver_variants() {
        let _ = [
            Receiver::Owned,
            Receiver::SharedRef,
            Receiver::MutRef,
            Receiver::Arbitrary,
        ];
    }
}
