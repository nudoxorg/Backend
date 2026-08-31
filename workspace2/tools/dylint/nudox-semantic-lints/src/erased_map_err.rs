use clippy_utils::{diagnostics::span_lint_and_help, usage::BindingUsageFinder};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint;
use rustc_span::sym;

declare_lint! {
    /// ### What it does
    ///
    /// Finds `Result::map_err` closures that do not use their source and directly replace it with a
    /// constructed or named error value.
    pub NUDOX_ERASED_MAP_ERR,
    Warn,
    "a Result::map_err closure directly replaces an unused source error"
}

pub(crate) fn check<'tcx>(context: &LateContext<'tcx>, expression: &'tcx Expr<'_>) {
    let ExprKind::MethodCall(method, receiver, [mapper], _) = expression.kind else {
        return;
    };
    if method.ident.name != sym::map_err
        || expression
            .span
            .in_external_macro(context.sess().source_map())
    {
        return;
    }
    let TyKind::Adt(receiver_type, _) = context.typeck_results().expr_ty(receiver).kind() else {
        return;
    };
    if !context
        .tcx
        .is_diagnostic_item(sym::Result, receiver_type.did())
    {
        return;
    }
    let ExprKind::Closure(closure) = mapper.kind else {
        return;
    };
    let body = context.tcx.hir_body(closure.body);
    if BindingUsageFinder::are_params_used(context, body) {
        return;
    }

    span_lint_and_help(
        context,
        NUDOX_ERASED_MAP_ERR,
        mapper.span,
        "Result::map_err discards its unused source error",
        None,
        "retain the source in the replacement error or pass it to an exact diagnostic function",
    );
}
