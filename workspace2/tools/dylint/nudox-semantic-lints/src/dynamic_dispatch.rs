use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{AmbigArg, Ty};
use rustc_lint::LateContext;
use rustc_session::declare_lint;

declare_lint! {
    /// ### What it does
    ///
    /// Finds explicit trait-object types in shipping code.
    pub NUDOX_DYNAMIC_DISPATCH,
    Warn,
    "an explicit trait object introduces dynamic dispatch"
}

pub(crate) fn check(context: &LateContext<'_>, ty: &Ty<'_, AmbigArg>) {
    if ty.span.from_expansion() || !matches!(ty.kind, rustc_hir::TyKind::TraitObject(..)) {
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
