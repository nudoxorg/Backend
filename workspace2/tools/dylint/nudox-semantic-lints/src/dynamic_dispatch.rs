use clippy_utils::diagnostics::span_lint_and_help;
use core::ops::ControlFlow;
use rustc_hir::{AmbigArg, QPath, Ty as HirTy, TyKind, def::DefKind, def::Res};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::{self, Ty, TyCtxt, TypeSuperVisitable, TypeVisitable, TypeVisitor};
use rustc_session::declare_lint;

declare_lint! {
    /// ### What it does
    ///
    /// Finds explicit trait-object types in shipping code.
    pub NUDOX_DYNAMIC_DISPATCH,
    Warn,
    "an explicit trait object introduces dynamic dispatch"
}

pub(crate) fn check(context: &LateContext<'_>, ty: &HirTy<'_, AmbigArg>) {
    if ty.span.in_external_macro(context.sess().source_map()) {
        return;
    }
    let explicitly_dynamic = matches!(ty.kind, TyKind::TraitObject(..));
    let resolves_to_dynamic = alias_is_dynamic(context, ty.kind);
    if !explicitly_dynamic && !resolves_to_dynamic {
        return;
    }

    span_lint_and_help(
        context,
        NUDOX_DYNAMIC_DISPATCH,
        ty.span,
        "explicit trait object introduces dynamic dispatch",
        None,
        "use an enum, a generic parameter, or an associated type at the shipping boundary",
    );
}

fn alias_is_dynamic(context: &LateContext<'_>, kind: TyKind<'_, AmbigArg>) -> bool {
    match kind {
        TyKind::Path(QPath::Resolved(_, path)) => match path.res {
            Res::Def(DefKind::TyAlias, alias) if alias.is_local() => {
                let resolved = context
                    .tcx
                    .type_of(alias)
                    .instantiate_identity()
                    .skip_norm_wip();
                resolved.visit_with(&mut DynamicType).is_break()
            }
            _ => false,
        },
        _ => false,
    }
}

struct DynamicType;

impl<'tcx> TypeVisitor<TyCtxt<'tcx>> for DynamicType {
    type Result = ControlFlow<()>;

    fn visit_ty(&mut self, ty: Ty<'tcx>) -> Self::Result {
        if matches!(ty.kind(), ty::Dynamic(..)) {
            ControlFlow::Break(())
        } else {
            ty.super_visit_with(self)
        }
    }
}
