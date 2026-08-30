use clippy_utils::{diagnostics::span_lint_and_help, peel_blocks};
use rustc_hir::{Expr, ExprKind, PatKind};
use rustc_lint::LateContext;
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint;
use rustc_span::sym;

declare_lint! {
    /// ### What it does
    ///
    /// Finds `Result::map_err` closures that bind their source as `_` and directly replace it with
    /// a constructed or named error value.
    pub NUDOX_ERASED_MAP_ERR,
    Warn,
    "a Result::map_err wildcard directly replaces a source error"
}

pub(crate) fn check<'tcx>(context: &LateContext<'tcx>, expression: &'tcx Expr<'_>) {
    let ExprKind::MethodCall(method, receiver, [mapper], _) = expression.kind else {
        return;
    };
    if method.ident.name != sym::map_err || expression.span.from_expansion() {
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
    let [parameter] = body.params else {
        return;
    };
    if !matches!(parameter.pat.kind, PatKind::Wild)
        || !matches!(
            peel_blocks(body.value).kind,
            ExprKind::Path(_) | ExprKind::Struct(..)
        )
    {
        return;
    }

    span_lint_and_help(
        context,
        NUDOX_ERASED_MAP_ERR,
        parameter.pat.span,
        "wildcard Result::map_err discards the source error",
        None,
        "retain the source in the replacement error or pass it to an exact diagnostic function",
    );
}
