//! The Implementations tab: collect and page the impls of a symbol.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::wire::{DocEvent, ImplRow, ImplsPage, SharedStr, SymbolKey};

use super::IMPLS_PAGE_SIZE;

// ---------------------------------------------------------------------------
// Impl collection
// ---------------------------------------------------------------------------

/// Collect all `ImplRow`s for the type identified by `target`.
///
/// One posting-list probe: the entries that name `target` in the
/// [`TypePosition::ImplSelf`] position *are* the impls whose `self_ty` is
/// `target`. The result is sorted by label string for deterministic emission
/// order.
///
/// # What this used to do, and why it stopped
///
/// It scanned the whole `by_kind[Impl]` bucket per symbol and re-matched every
/// impl's `self_ty` by hand, under a comment saying the scan existed *because*
/// `mentions` indexed `Impl::of` and not `Impl::self_ty`. That is no longer
/// true — `PackageIndexes::type_refs` carries the position — so the workaround
/// is a probe.
///
/// The swap also fixes two cases the hand-rolled matcher silently dropped,
/// because it only recursed one level into `Type::Apply` and knew nothing
/// about `Type::Annotated`:
///
/// * `impl Trait for Outer<Inner<T>>` — the matcher compared `target` against
///   `Outer` only at the outermost level, which happened to be right, but
///   `impl Trait for @NonNull Point` matched nothing at all.
/// * `head_stable_ref` (which builds the index) recurses through both, so the
///   index and the Implementations tab can no longer disagree about what an
///   impl is *for*.
///
/// [`TypePosition::ImplSelf`]: crate::store::package::TypePosition::ImplSelf
pub(crate) fn collect_impls(
    target: nudox_ir::change::IntroId,
    pkg: &crate::store::package::PackageView,
    lineage: nudox_ir::change::PackageLineageId,
) -> Vec<ImplRow> {
    use crate::store::package::TypePosition;
    use nudox_ir::change::StableRef;
    use nudox_ir::kind::Kind;
    use nudox_ir::kinds::Type;

    let view = pkg.view();

    // `ImplSelf` and not `ImplementedTrait`: this tab answers "what is
    // implemented *for* the symbol I opened". Reading the trait position here
    // would list, for `trait Display`, every impl of `Display` — which is a
    // real question, but a different one, and the one `Trait.implementors`
    // answers.
    let self_ref = StableRef::new(lineage.clone(), target);
    let impl_ids = pkg
        .indexes()
        .type_refs_in(&self_ref, TypePosition::ImplSelf);

    let mut rows: Vec<ImplRow> = impl_ids
        .iter()
        .filter_map(|&impl_intro| {
            let entry = view.entry(impl_intro)?;
            // A non-`Impl` entry cannot appear in the `ImplSelf` position —
            // only `Kind::Impl` contributes one — so this arm is unreachable
            // rather than a filter. It stays a `return None` instead of an
            // `expect` because a corpus inconsistency should cost one row, not
            // the whole page.
            let Some(Kind::Impl(impl_data)) = entry.kind().as_owned_kind() else {
                return None;
            };
            // Render the impl signature as the label.  `chunk::signature::tokens`
            // produces something like `impl Debug for Router<E>` which is exactly
            // what the Implementations tab wants to display.  We flatten the
            // token vec to a string for the sort key and for the label field.
            let sig_tokens = crate::chunk::signature::tokens(entry, pkg);
            let label_text = crate::chunk::signature::tokens_to_text(&sig_tokens);
            let key = SymbolKey::new(lineage.clone(), impl_intro);

            // is_blanket — directly from `ImplFlags`.
            let is_blanket = impl_data.flags.blanket;

            // trait_label — render `of` to text if present.
            let trait_label: Option<SharedStr> = impl_data.of.as_ref().map(|of_ty| {
                let mut toks = Vec::new();
                crate::chunk::signature::push_type(&mut toks, of_ty, pkg);
                SharedStr::from(crate::chunk::signature::tokens_to_text(&toks).as_str())
            });

            // self_generic_count — the number of args at the outermost Apply level.
            let self_generic_count: u32 = match &impl_data.self_ty {
                Type::Apply { args, .. } => args.len() as u32,
                _ => 0,
            };

            // The impl block's own location (docs/LIMITATIONS.md L42.3). Read from
            // the same entry the label came from, so a row can never describe
            // one impl and link to another.
            let source = crate::wire::SourceLocation::from_ir(entry.location());

            Some(ImplRow {
                key,
                label: SharedStr::from(label_text.as_str()),
                is_blanket,
                trait_label,
                self_generic_count,
                source,
            })
        })
        .collect();

    // Sort by label for stable, deterministic ordering across calls. The key
    // is the triomphe Arc (cheap clone) so no heap string is allocated per row.
    rows.sort_unstable_by_key(|a| a.label.as_arc().clone());
    rows
}

/// Emit `Impls` pages for `rows`, paginated by `IMPLS_PAGE_SIZE`.
///
/// Always emits at least one page (possibly empty) so the GUI can exit the
/// "still loading" state.  The last page carries `done: true`.
pub(crate) async fn emit_impls_pages(
    tx: &flume::Sender<DocEvent>,
    cancel: &CancellationToken,
    rows: Vec<ImplRow>,
) {
    let total = rows.len() as u64;
    if rows.is_empty() {
        // No impls — one empty terminal page.
        let _ = tx
            .send_async(DocEvent::Impls {
                page: ImplsPage {
                    impls: Arc::from([] as [ImplRow; 0]),
                    total: 0,
                },
                done: true,
            })
            .await;
        return;
    }

    let mut chunks = rows.chunks(IMPLS_PAGE_SIZE).peekable();
    while let Some(chunk) = chunks.next() {
        if cancel.is_cancelled() {
            return;
        }
        let done = chunks.peek().is_none();
        let page = ImplsPage {
            impls: chunk.to_vec().into(),
            total,
        };
        if tx.send_async(DocEvent::Impls { page, done }).await.is_err() {
            debug!("doc stream: receiver dropped during Impls");
            return;
        }
    }
}
