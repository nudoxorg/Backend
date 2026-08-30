use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{FieldDef, Mutability, def::DefKind};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::{self, TyKind};
use rustc_session::declare_lint;

declare_lint! {
    /// ### What it does
    ///
    /// Finds closed state/error vocabulary fields represented by static strings instead of a closed
    /// type.
    pub NUDOX_STRINGLY_STATE_FIELD,
    Warn,
    "a semantic state field is represented by an open string vocabulary"
}

pub(crate) fn check(context: &LateContext<'_>, field: &FieldDef<'_>) {
    if field.span.in_external_macro(context.sess().source_map())
        || !is_closed_vocabulary_name(context, field)
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
        TyKind::Ref(region, inner, Mutability::Not)
            if inner.is_str() && matches!(region.kind(), ty::RegionKind::ReStatic)
    );
    if !is_string_slice {
        return;
    }

    span_lint_and_help(
        context,
        NUDOX_STRINGLY_STATE_FIELD,
        field.span,
        format!(
            "closed state/error field `{}` is stringly typed",
            field.ident.name
        ),
        None,
        "replace the static string vocabulary with a closed enum or semantic type",
    );
}

fn is_closed_vocabulary_name(context: &LateContext<'_>, field: &FieldDef<'_>) -> bool {
    match field.ident.name.as_str() {
        "step" | "stage" | "phase" => true,
        "detail" | "expected" | "observed" | "field" => belongs_to_error_enum(context, field),
        _ => false,
    }
}

fn belongs_to_error_enum(context: &LateContext<'_>, field: &FieldDef<'_>) -> bool {
    let variant = context.tcx.parent(field.def_id.into());
    if context.tcx.def_kind(variant) != DefKind::Variant {
        return false;
    }
    let enum_definition = context.tcx.parent(variant);
    context.tcx.def_kind(enum_definition) == DefKind::Enum
        && context
            .tcx
            .item_name(enum_definition)
            .as_str()
            .ends_with("Error")
}
