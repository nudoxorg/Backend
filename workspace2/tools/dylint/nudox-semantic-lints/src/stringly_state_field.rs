use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{FieldDef, def::DefKind};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::{GenericArgsRef, Ty, TyKind};
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
    if !is_open_text(context, field_type) {
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
        "class" | "kind" | "phase" | "role" | "stage" | "state" | "status" | "step" => true,
        "detail" | "expected" | "field" | "observed" | "operation" | "resource" => {
            belongs_to_diagnostic_owner(context, field)
        }
        _ => false,
    }
}

fn is_open_text(context: &LateContext<'_>, field_type: Ty<'_>) -> bool {
    let field_type = field_type.peel_refs();
    if field_type.is_str() {
        return true;
    }
    let TyKind::Adt(definition, arguments) = field_type.kind() else {
        return false;
    };
    match context.tcx.def_path_str(definition.did()).as_str() {
        "alloc::string::String" => true,
        "alloc::borrow::Cow" | "alloc::boxed::Box" | "alloc::rc::Rc" | "alloc::sync::Arc" => {
            contains_str(arguments)
        }
        _ => false,
    }
}

fn contains_str(arguments: GenericArgsRef<'_>) -> bool {
    arguments.types().any(|argument| argument.is_str())
}

fn belongs_to_diagnostic_owner(context: &LateContext<'_>, field: &FieldDef<'_>) -> bool {
    let parent = context.tcx.parent(field.def_id.into());
    let owner = match context.tcx.def_kind(parent) {
        DefKind::Struct => parent,
        DefKind::Variant => context.tcx.parent(parent),
        _ => return false,
    };
    if !matches!(context.tcx.def_kind(owner), DefKind::Enum | DefKind::Struct) {
        return false;
    }
    let name = context.tcx.item_name(owner);
    ["Cause", "Error", "Failure", "Rejection", "Terminal"]
        .into_iter()
        .any(|suffix| name.as_str().ends_with(suffix))
}
