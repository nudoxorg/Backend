use clippy_utils::{diagnostics::span_lint_and_help, peel_blocks};
use rustc_hir::{ExprKind, ImplItem, ImplItemImplKind, ImplItemKind, PatKind, def::Res};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty;
use rustc_session::declare_lint;

declare_lint! {
    /// ### What it does
    ///
    /// Finds public inherent methods that only return one field from `self`.
    ///
    /// Direct public facts should be fields. When construction must remain sealed, expose a
    /// read-only public view through `Deref` rather than multiplying one-line getters.
    pub NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
    Warn,
    "a public method only returns a field"
}

pub(crate) fn check<'tcx>(context: &LateContext<'tcx>, item: &'tcx ImplItem<'_>) {
    if item.span.in_external_macro(context.sess().source_map())
        || !matches!(item.impl_kind, ImplItemImplKind::Inherent { .. })
        || !context
            .tcx
            .associated_item(item.owner_id.def_id)
            .is_method()
        || !context.tcx.visibility(item.owner_id.def_id).is_public()
    {
        return;
    }
    let ImplItemKind::Fn(_, body_id) = item.kind else {
        return;
    };
    let body = context.tcx.hir_body(body_id);
    let [receiver_parameter] = body.params else {
        return;
    };
    let PatKind::Binding(_, receiver_id, _, _) = receiver_parameter.pat.kind else {
        return;
    };
    let returned = peel_blocks(body.value);
    let ExprKind::Field(receiver, field_name) = returned.kind else {
        return;
    };
    let ExprKind::Path(receiver_path) = receiver.kind else {
        return;
    };
    if context.qpath_res(&receiver_path, receiver.hir_id) != Res::Local(receiver_id) {
        return;
    }
    let typeck = context.tcx.typeck(item.owner_id.def_id);
    let receiver_type = typeck.expr_ty(receiver).peel_refs();
    let ty::Adt(definition, _) = receiver_type.kind() else {
        return;
    };
    let field_index = typeck.field_index(returned.hir_id);
    let field = &definition.non_enum_variant().fields[field_index];
    span_lint_and_help(
        context,
        NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
        item.span,
        format!(
            "public accessor only returns the field `{}`",
            field_name.name
        ),
        None,
        if context.tcx.visibility(field.did).is_public() {
            "delete the accessor and use direct field access"
        } else {
            "use a public field when it is independently mutable, or seal construction behind an immutable Deref view when fields form one validated invariant"
        },
    );
}
