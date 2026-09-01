//! Enforces visible coordination at exported data boundaries.
//! Resolves standard-library container definitions instead of matching tokens.
//! Accepts atomics because their synchronization protocol is explicit at use sites.
use rustc_hir::{Item, ItemKind, VariantData};
use rustc_lint::LateContext;

use crate::HIDDEN_MUTABILITY;

/// Reports exported fields whose resolved type is a standard mutable container.
pub(crate) fn check_item(context: &LateContext<'_>, item: &Item<'_>) {
    let (ItemKind::Struct(_, _, fields) | ItemKind::Union(_, _, fields)) = &item.kind else {
        return;
    };
    if !context
        .effective_visibilities
        .is_exported(item.owner_id.def_id)
    {
        return;
    }
    for field in named_fields(fields) {
        if context.tcx.visibility(field.def_id).is_public()
            && let Some(message) = standard_mutable_container(context, field.def_id)
        {
            crate::emit(context, HIDDEN_MUTABILITY, field.span, message);
        }
    }
}

/// Returns named fields because tuple members do not carry an API field name.
fn named_fields<'hir>(fields: &'hir VariantData<'hir>) -> &'hir [rustc_hir::FieldDef<'hir>] {
    match fields {
        VariantData::Struct { fields, .. } => fields,
        VariantData::Tuple(..) | VariantData::Unit(..) => &[],
    }
}

/// Returns the concrete standard container name when the field resolves to one.
fn standard_mutable_container(
    context: &LateContext<'_>,
    field: rustc_hir::def_id::LocalDefId,
) -> Option<&'static str> {
    let ty = context
        .tcx
        .type_of(field)
        .instantiate_identity()
        .skip_norm_wip();
    let rustc_middle::ty::TyKind::Adt(definition, _) = ty.kind() else {
        return None;
    };
    match context.tcx.def_path_str(definition.did()).as_str() {
        "std::cell::Cell" | "core::cell::Cell" => {
            Some("exported field hides coordination in `Cell`; expose a proved protocol instead")
        }
        "std::cell::RefCell" | "core::cell::RefCell" => {
            Some("exported field hides coordination in `RefCell`; expose a proved protocol instead")
        }
        "std::sync::Mutex" => {
            Some("exported field hides coordination in `Mutex`; expose a proved protocol instead")
        }
        "std::sync::RwLock" => {
            Some("exported field hides coordination in `RwLock`; expose a proved protocol instead")
        }
        "std::sync::OnceLock" | "std::sync::LazyLock" => Some(
            "exported field hides coordination in `OnceLock`; expose a proved protocol instead",
        ),
        _ => None,
    }
}
