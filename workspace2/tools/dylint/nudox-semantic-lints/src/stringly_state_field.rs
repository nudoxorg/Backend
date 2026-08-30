use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{FieldDef, Mutability};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint;

declare_lint! {
    /// ### What it does
    ///
    /// Finds semantic state fields named `step`, `expected`, or `observed` that use an open string
    /// vocabulary instead of a closed type.
    pub NUDOX_STRINGLY_STATE_FIELD,
    Warn,
    "a semantic state field is represented by an open string vocabulary"
}

pub(crate) fn check(context: &LateContext<'_>, field: &FieldDef<'_>) {
    if field.span.in_external_macro(context.sess().source_map())
        || !matches!(field.ident.name.as_str(), "step" | "expected" | "observed")
    {
        return;
    }
    let field_type = context
        .tcx
        .type_of(field.def_id)
        .instantiate_identity()
        .skip_norm_wip();
    let is_string_slice = matches!(
        field_type.kind(),
        TyKind::Ref(_, inner, Mutability::Not) if inner.is_str()
    );
    let is_owned_string = matches!(
        field_type.kind(),
        TyKind::Adt(definition, _)
            if context.tcx.lang_items().string() == Some(definition.did())
    );
    if !is_string_slice && !is_owned_string {
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
