use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{FieldDef, Mutability};
use rustc_lint::LateContext;
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint;

declare_lint! {
    /// ### What it does
    ///
    /// Finds semantic state fields named `step`, `expected`, or `observed` that use a string slice
    /// instead of a closed type.
    pub NUDOX_STRINGLY_STATE_FIELD,
    Warn,
    "a semantic state field is represented by a string slice"
}

pub(crate) fn check(context: &LateContext<'_>, field: &FieldDef<'_>) {
    if field.span.from_expansion()
        || !matches!(field.ident.name.as_str(), "step" | "expected" | "observed")
    {
        return;
    }
    let field_type = context
        .tcx
        .type_of(field.def_id)
        .instantiate_identity()
        .skip_norm_wip();
    let TyKind::Ref(_, inner, Mutability::Not) = field_type.kind() else {
        return;
    };
    if !inner.is_str() {
        return;
    }

    span_lint_and_help(
        context,
        NUDOX_STRINGLY_STATE_FIELD,
        field.span,
        format!(
            "semantic state field `{}` is stringly typed",
            field.ident.name
        ),
        None,
        "replace the open string vocabulary with a closed enum or semantic type",
    );
}
