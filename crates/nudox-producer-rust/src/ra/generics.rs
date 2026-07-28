//! Generic params + where-clause lowering → `nudox_ir::kinds::{GenericParam, WherePred}`.
//!
//! # What changed vs the old generics.rs
//!
//! The new IR `GenericParam` has three variants: `Lifetime { name }`,
//! `Type { name, bounds, default }`, and `Const { name, ty }`.  Bounds are
//! `List<Type>` (type-expression references to traits) rather than the old
//! `Constraint` enum.
//!
//! The new `WherePred` is `{ target: Type, bounds: List<Type> }`.  Both
//! `target` and `bounds` use the `Type` enum, with trait references lowered as
//! `Type::Nominal` or `Type::Any` for external bounds.
//!
//! `TraitRef`, `TypeExpr`, `Constraint::AssociatedItem`, etc. from the old IR
//! are all gone.  The conversion is lossy for associated-type bindings in
//! bounds (`T: Iterator<Item = u8>`) — the `Item = u8` constraint is dropped
//! and only the trait reference is kept.

use nudox_ir::{
    index::RawRef,
    kinds::{GenericParam, WherePred},
};
use ra_ap_syntax::ast::{self, HasGenericArgs, HasName, HasTypeBounds};

use super::{ctx::{LowerCtx, PathKey}, item::id_of, ty};

// ── Public entry ──────────────────────────────────────────────────────────────

/// Lower `ast::GenericParamList` + `ast::WhereClause` into IR params and
/// where-predicates.
///
/// `ref_for` maps a canonical path key to a `RawRef` for `Type::Nominal`
/// construction.
pub(crate) fn lower_generics(
    ctx: &mut LowerCtx<'_>,
    params: Option<ast::GenericParamList>,
    where_clause: Option<ast::WhereClause>,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> (Vec<GenericParam>, Vec<WherePred>) {
    let mut ir_params: Vec<GenericParam> = Vec::new();
    let mut ir_wheres: Vec<WherePred> = Vec::new();

    // ── Generic params ────────────────────────────────────────────────────────

    if let Some(list) = params {
        for param in list.generic_params() {
            match param {
                ast::GenericParam::TypeParam(tp) => {
                    let name = tp.name().map(|n| n.text().to_string());
                    if name.as_deref() == Some("Self") {
                        continue;
                    }
                    let name = name.unwrap_or_default();

                    // Inline bounds `T: Clone + Debug` → where-predicates.
                    // The new IR represents them on the param as `bounds: List<Type>`
                    // AND emits them as `WherePred` for backward compat with the
                    // where clause lowering.  We put them only in `bounds` on the
                    // GenericParam to avoid duplication.
                    let bounds: Box<[nudox_ir::kinds::Type]> = tp
                        .type_bound_list()
                        .map(|list| {
                            bound_list_to_types(ctx, list, ref_for)
                        })
                        .unwrap_or_default();

                    let default = tp
                        .default_type()
                        .map(|t| ty::lower_ast_type(ctx, &t, ref_for));

                    ir_params.push(GenericParam::Type {
                        name,
                        bounds,
                        default,
                        // Rust has no declaration-site variance annotation; the
                        // compiler infers it from usage.  None = unspecified.
                        variance: None,
                    });
                }
                ast::GenericParam::ConstParam(cp) => {
                    let name = cp
                        .name()
                        .map(|n| n.text().to_string())
                        .unwrap_or_default();
                    let param_ty = cp
                        .ty()
                        .map(|t| ty::lower_ast_type(ctx, &t, ref_for))
                        .unwrap_or(nudox_ir::kinds::Type::Any);
                    ir_params.push(GenericParam::Const { name, ty: param_ty });
                }
                ast::GenericParam::LifetimeParam(lp) => {
                    let name = lp
                        .lifetime()
                        .map(|lt| lt.text().to_string())
                        .unwrap_or_default();
                    // Lifetime bounds (`'a: 'b`) are not representable in the
                    // new IR's WherePred (target must be a Type, not a lifetime).
                    // They are dropped here.
                    ir_params.push(GenericParam::Lifetime { name });
                }
            }
        }
    }

    // ── Where clause ─────────────────────────────────────────────────────────

    if let Some(wc) = where_clause {
        for pred in wc.predicates() {
            // Lifetime predicate (`'a: 'b`) → not representable; skip.
            if pred.lifetime().is_some() {
                continue;
            }
            let Some(ty_node) = pred.ty() else { continue };
            let target = ty::lower_ast_type(ctx, &ty_node, ref_for);

            let bounds = pred
                .type_bound_list()
                .map(|list| bound_list_to_types(ctx, list, ref_for))
                .unwrap_or_default();

            if !bounds.is_empty() {
                ir_wheres.push(WherePred { target, bounds });
            }
        }
    }

    (ir_params, ir_wheres)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Lower a type-bound list into a `Box<[Type]>`.
///
/// Only `PathType` bounds are lowered; lifetime bounds are dropped.
/// Associated-type bindings in trait args (`Iterator<Item = u8>`) are currently
/// dropped — only the trait ref itself is kept.
fn bound_list_to_types(
    ctx: &mut LowerCtx<'_>,
    list: ast::TypeBoundList,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> Box<[nudox_ir::kinds::Type]> {
    list.bounds()
        .filter_map(|bound| match bound.kind() {
            Some(ast::TypeBoundKind::PathType(_, path_ty)) => {
                Some(path_type_to_type(ctx, &path_ty, ref_for))
            }
            // Lifetime bounds and `use<…>` precision capturing → dropped.
            _ => None,
        })
        .collect()
}

/// A `PathType` bound → `Type`.
///
/// Tries to resolve to a `Nominal` ref; falls back to `Any` for external or
/// unresolvable bounds.
fn path_type_to_type(
    ctx: &mut LowerCtx<'_>,
    path_ty: &ast::PathType,
    ref_for: &mut impl FnMut(&PathKey) -> Option<RawRef>,
) -> nudox_ir::kinds::Type {
    let Some(path) = path_ty.path() else {
        return nudox_ir::kinds::Type::Any;
    };
    use ra_ap_hir::PathResolution;
    if let Some(res) = ty::resolve_path_opt(ctx, &path)
        && let PathResolution::Def(def) = res
        && let Some(key) = id_of(ctx, def)
        && let Some(raw_ref) = ref_for(&key)
    {
        // Generic args on the trait ref; only type args kept.
        let type_args: Box<[nudox_ir::kinds::Type]> = path
            .segment()
            .and_then(|s| s.generic_arg_list())
            .map(|list| {
                list.generic_args()
                    .filter_map(|arg| match arg {
                        ast::GenericArg::TypeArg(ta) => ta
                            .ty()
                            .map(|t| ty::lower_ast_type(ctx, &t, ref_for)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let base = nudox_ir::kinds::Type::Nominal(raw_ref);
        if type_args.is_empty() {
            return base;
        }
        return nudox_ir::kinds::Type::Apply {
            base: Box::new(base),
            args: type_args,
        };
    }
    nudox_ir::kinds::Type::Any
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use nudox_ir::kinds::GenericParam;

    /// A `Lifetime` generic param round-trips through the builder correctly.
    #[test]
    fn generic_param_lifetime_fields() {
        let gp = GenericParam::Lifetime { name: "'a".to_owned() };
        assert!(matches!(gp, GenericParam::Lifetime { name } if name == "'a"));
    }

    /// A `Type` generic param stores name, bounds, and default.
    #[test]
    fn generic_param_type_fields() {
        let gp = GenericParam::Type {
            name: "T".to_owned(),
            bounds: Box::new([]),
            default: None,
            // Rust has no declaration-site variance annotation; None = unspecified.
            variance: None,
        };
        assert!(matches!(gp, GenericParam::Type { name, bounds, default, variance: _ }
            if name == "T" && bounds.is_empty() && default.is_none()
        ));
    }

    /// A `Const` generic param stores name and type.
    #[test]
    fn generic_param_const_fields() {
        let gp = GenericParam::Const {
            name: "N".to_owned(),
            ty: nudox_ir::kinds::Type::I32,
        };
        assert!(matches!(gp, GenericParam::Const { name, ty: nudox_ir::kinds::Type::I32 }
            if name == "N"
        ));
    }
}
