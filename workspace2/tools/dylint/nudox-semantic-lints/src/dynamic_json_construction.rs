use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::Expr;
use rustc_lint::LateContext;
use rustc_session::declare_lint;
use rustc_span::Span;
use std::collections::HashSet;

declare_lint! {
    /// ### What it does
    ///
    /// Finds shipping expressions expanded from `serde_json::json!`.
    pub NUDOX_DYNAMIC_JSON_CONSTRUCTION,
    Warn,
    "shipping JSON is constructed through serde_json::json!"
}

pub(crate) fn check(
    context: &LateContext<'_>,
    expression: &Expr<'_>,
    reported_calls: &mut HashSet<Span>,
) {
    let Some(call_site) = serde_json_call_site(context, expression.span) else {
        return;
    };
    if !reported_calls.insert(call_site) {
        return;
    }

    span_lint_and_help(
        context,
        NUDOX_DYNAMIC_JSON_CONSTRUCTION,
        call_site,
        "shipping JSON is constructed through `serde_json::json!`",
        None,
        "replace the closed payload with a typed Serialize DTO; allow this lint only at a documented genuinely open JSON boundary",
    );
}

fn serde_json_call_site(context: &LateContext<'_>, span: Span) -> Option<Span> {
    span.macro_backtrace().find_map(|expansion| {
        let macro_definition = expansion.macro_def_id?;
        (context.tcx.crate_name(macro_definition.krate).as_str() == "serde_json"
            && context.tcx.item_name(macro_definition).as_str() == "json")
            .then_some(expansion.call_site)
    })
}
