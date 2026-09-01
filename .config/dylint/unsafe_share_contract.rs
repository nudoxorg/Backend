//! Rejects manual unsafe thread-sharing implementations.
//! Resolves the implemented trait so aliases cannot evade the boundary.
//! Requires an external proof workflow rather than an inline exception.
use rustc_hir::{ImplPolarity, Item, ItemKind, Safety};
use rustc_lint::LateContext;
use rustc_span::sym;

use crate::UNSAFE_SHARE_CONTRACT;

/// Reports each unsafe positive implementation of `Send` or `Sync`.
pub(crate) fn check_item(context: &LateContext<'_>, item: &Item<'_>) {
    let ItemKind::Impl(implementation) = &item.kind else {
        return;
    };
    let Some(trait_header) = implementation.of_trait else {
        return;
    };
    if trait_header.safety != Safety::Unsafe || trait_header.polarity != ImplPolarity::Positive {
        return;
    }
    let Some(trait_id) = trait_header.trait_ref.trait_def_id() else {
        return;
    };
    let is_share_trait = [sym::Send, sym::Sync]
        .into_iter()
        .filter_map(|symbol| context.tcx.get_diagnostic_item(symbol))
        .any(|expected| expected == trait_id);
    if is_share_trait {
        crate::emit(
            context,
            UNSAFE_SHARE_CONTRACT,
            item.span,
            "manual unsafe `Send` or `Sync` implementation needs a separate proved concurrency boundary",
        );
    }
}
