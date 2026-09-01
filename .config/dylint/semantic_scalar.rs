//! Enforces semantic field types at exported data boundaries.
//! Uses resolved field types, not spelling, so aliases cannot evade it.
//! Leaves private representation and explicit FFI layout alone.
use rustc_hir::{Item, ItemKind, VariantData};
use rustc_lint::LateContext;
use rustc_middle::ty::TyKind;

use crate::SEMANTIC_SCALAR;

/// Reports exported, non-FFI struct and union fields with primitive types.
pub(crate) fn check_item(context: &LateContext<'_>, item: &Item<'_>) {
    let (ItemKind::Struct(_, _, fields) | ItemKind::Union(_, _, fields)) = &item.kind else {
        return;
    };
    if !context
        .effective_visibilities
        .is_exported(item.owner_id.def_id)
        || is_c_layout(context, item)
    {
        return;
    }
    for field in named_fields(fields) {
        if context.tcx.visibility(field.def_id).is_public()
            && is_primitive(
                context
                    .tcx
                    .type_of(field.def_id)
                    .instantiate_identity()
                    .skip_norm_wip(),
            )
        {
            crate::emit(
                context,
                SEMANTIC_SCALAR,
                field.span,
                "exported field uses a primitive scalar; introduce a domain newtype",
            );
        }
    }
}

/// Returns named fields because tuple members lack a stable semantic field name.
fn named_fields<'hir>(fields: &'hir VariantData<'hir>) -> &'hir [rustc_hir::FieldDef<'hir>] {
    match fields {
        VariantData::Struct { fields, .. } => fields,
        VariantData::Tuple(..) | VariantData::Unit(..) => &[],
    }
}

/// Returns whether the resolved type is a language primitive scalar.
fn is_primitive(ty: rustc_middle::ty::Ty<'_>) -> bool {
    matches!(
        ty.kind(),
        TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Uint(_) | TyKind::Float(_)
    )
}

/// Returns whether explicit C layout makes a scalar field an ABI obligation.
fn is_c_layout(context: &LateContext<'_>, item: &Item<'_>) -> bool {
    context.tcx.adt_def(item.owner_id).repr().c()
}
