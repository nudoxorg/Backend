use clippy_utils::{diagnostics::span_lint_and_help, peel_blocks};
use rustc_ast::ast::LitKind;
use rustc_hir::{ExprKind, Item, ItemKind, PatKind, def::Res};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty;
use rustc_session::declare_lint;

declare_lint! {
    /// ### What it does
    ///
    /// Finds a free function that maps a workspace-owned enum directly to string literals.
    pub NUDOX_ENUM_STATIC_STR_PROJECTION,
    Warn,
    "a workspace-owned enum is projected to static strings by a free function"
}

pub(crate) fn check<'tcx>(context: &LateContext<'tcx>, item: &'tcx Item<'_>) {
    if item.span.in_external_macro(context.sess().source_map()) {
        return;
    }
    let ItemKind::Fn { body: body_id, .. } = item.kind else {
        return;
    };
    let body = context.tcx.hir_body(body_id);
    let [parameter] = body.params else {
        return;
    };
    let PatKind::Binding(_, parameter_id, _, _) = parameter.pat.kind else {
        return;
    };
    let typeck = context.tcx.typeck(item.owner_id.def_id);
    let parameter_type = typeck.pat_ty(parameter.pat).peel_refs();
    let ty::Adt(enum_definition, _) = parameter_type.kind() else {
        return;
    };
    if !enum_definition.is_enum() || !enum_definition.did().is_local() {
        return;
    }

    let matched = peel_blocks(body.value);
    let ExprKind::Match(scrutinee, arms, _) = matched.kind else {
        return;
    };
    let ExprKind::Path(path) = peel_blocks(scrutinee).kind else {
        return;
    };
    if context.qpath_res(&path, scrutinee.hir_id) != Res::Local(parameter_id)
        || arms.is_empty()
        || !arms.iter().all(is_static_string_arm)
    {
        return;
    }

    span_lint_and_help(
        context,
        NUDOX_ENUM_STATIC_STR_PROJECTION,
        item.span,
        format!(
            "free function projects workspace-owned enum `{}` to static strings",
            context.tcx.item_name(enum_definition.did())
        ),
        None,
        "put the exhaustive textual projection on the enum (or derive its wire serialization) so the type owns its vocabulary",
    );
}

fn is_static_string_arm(arm: &rustc_hir::Arm<'_>) -> bool {
    arm.guard.is_none()
        && matches!(
            peel_blocks(arm.body).kind,
            ExprKind::Lit(literal) if matches!(literal.node, LitKind::Str(..))
        )
}
