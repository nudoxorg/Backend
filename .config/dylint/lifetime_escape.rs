//! Enforces non-static borrowing at exported data boundaries.
//! Inspects the HIR signature to retain source-location precision.
//! Leaves owned values and caller-bounded references unmodified.
use rustc_hir::{FnRetTy, FnSig, Item, ItemKind, LifetimeKind, Ty, TyKind};
use rustc_lint::LateContext;

use crate::LIFETIME_ESCAPE;

/// Reports exported function signatures that return a static reference.
pub(crate) fn check_item(context: &LateContext<'_>, item: &Item<'_>) {
    let ItemKind::Fn {
        sig: FnSig { decl, .. },
        ..
    } = &item.kind
    else {
        return;
    };
    if !context
        .effective_visibilities
        .is_exported(item.owner_id.def_id)
    {
        return;
    }
    let FnRetTy::Return(output) = decl.output else {
        return;
    };
    if contains_static_reference(output) {
        crate::emit(
            context,
            LIFETIME_ESCAPE,
            output.span,
            "exported function returns a static reference; return an owned or capability-bound value",
        );
    }
}

/// Returns whether a return type contains an explicit `&'static` reference.
fn contains_static_reference(ty: &Ty<'_>) -> bool {
    match ty.kind {
        TyKind::Ref(lifetime, mutable) => {
            matches!(lifetime.kind, LifetimeKind::Static) || contains_static_reference(mutable.ty)
        }
        TyKind::Slice(element) | TyKind::Array(element, _) => contains_static_reference(element),
        TyKind::Ptr(pointer) => contains_static_reference(pointer.ty),
        TyKind::Tup(elements) => elements.iter().any(contains_static_reference),
        TyKind::Path(_) | TyKind::Infer(..) | TyKind::Never | TyKind::Err(_) => false,
        _ => false,
    }
}
