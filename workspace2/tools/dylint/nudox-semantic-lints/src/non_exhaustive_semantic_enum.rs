use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{Item, ItemKind};
use rustc_lint::{LateContext, LintContext};
use rustc_session::declare_lint;

declare_lint! {
    /// ### What it does
    ///
    /// Rejects `#[non_exhaustive]` on workspace-owned enums. These enums are deliberately closed so
    /// downstream matches cannot erase a newly added typed cause behind a wildcard arm.
    pub NUDOX_NON_EXHAUSTIVE_SEMANTIC_ENUM,
    Warn,
    "a workspace enum is non-exhaustive"
}

pub(crate) fn check(context: &LateContext<'_>, item: &Item<'_>) {
    if !matches!(item.kind, ItemKind::Enum(..))
        || !context
            .tcx
            .adt_def(item.owner_id.def_id)
            .is_variant_list_non_exhaustive()
        || item.span.in_external_macro(context.sess().source_map())
    {
        return;
    }

    span_lint_and_help(
        context,
        NUDOX_NON_EXHAUSTIVE_SEMANTIC_ENUM,
        item.span,
        "workspace enums must remain exhaustively matchable",
        None,
        "remove `#[non_exhaustive]` and add a typed variant when the vocabulary grows",
    );
}
