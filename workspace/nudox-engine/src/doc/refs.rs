//! The References tab: collect and page the callers of a symbol.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::wire::{DocEvent, RefRow, RefsPage, SharedStr, SymbolKey};

use super::REFS_PAGE_SIZE;

// ---------------------------------------------------------------------------
// Refs collection
// ---------------------------------------------------------------------------

/// Collect all `RefRow`s for the symbol identified by `target`.
///
/// Uses `PackageIndexes::usages_of` — resolved-reference postings at
/// `Confidence >= Index` — which represent active use sites (call sites,
/// field accesses, etc.).  See the `stream_symbol` comment above for why
/// `mentions_of` is intentionally excluded here.
pub(crate) fn collect_refs(
    target: nudox_ir::change::IntroId,
    pkg: &crate::store::package::PackageView,
    lineage: nudox_ir::change::PackageLineageId,
) -> Vec<RefRow> {
    use nudox_ir::change::StableRef;

    let indexes = pkg.indexes();
    let view = pkg.view();

    let target_sr = StableRef::new(lineage.clone(), target);
    let usage_ids = indexes.usages_of(&target_sr);

    let mut rows: Vec<RefRow> = usage_ids
        .iter()
        .filter_map(|&owner| {
            // Skip entries that no longer exist (shouldn't happen in a sealed
            // table, but be defensive against future non-live entries).
            let entry = view.entry(owner)?;
            let path = indexes
                .path_of(owner)
                .map_or_else(|| SharedStr::from(entry.sym().name.as_str()), |p| {
                    SharedStr::from(&**p)
                });

            // Derive a short kind label from the entry's kind discriminant.
            // This is the "precision badge" the wire spec describes on `RefRow`.
            let kind_tag = entry
                .kind()
                .discriminant()
                .map_or_else(|| SharedStr::from("ref"), |d| {
                    SharedStr::from(format!("{d:?}").as_str())
                });

            Some(RefRow {
                target: SymbolKey::new(lineage.clone(), owner),
                path,
                kind_tag,
            })
        })
        .collect();

    // Sort by path for deterministic ordering. The key is the triomphe Arc
    // (cheap clone) so no heap string is allocated per row.
    rows.sort_unstable_by_key(|a| a.path.as_arc().clone());
    rows
}

/// Emit `Refs` pages for `rows`, paginated by `REFS_PAGE_SIZE`.
///
/// Always emits at least one page (possibly empty) so the GUI can exit the
/// "still loading" state.  The last page carries `done: true`.
pub(crate) async fn emit_refs_pages(
    tx: &flume::Sender<DocEvent>,
    cancel: &CancellationToken,
    rows: Vec<RefRow>,
) {
    let total = rows.len() as u64;
    if rows.is_empty() {
        let _ = tx
            .send_async(DocEvent::Refs {
                page: RefsPage {
                    refs: Arc::from([] as [RefRow; 0]),
                    total: 0,
                },
                done: true,
            })
            .await;
        return;
    }

    let mut chunks = rows.chunks(REFS_PAGE_SIZE).peekable();
    while let Some(chunk) = chunks.next() {
        if cancel.is_cancelled() {
            return;
        }
        let done = chunks.peek().is_none();
        let page = RefsPage {
            refs: chunk.to_vec().into(),
            total,
        };
        if tx.send_async(DocEvent::Refs { page, done }).await.is_err() {
            debug!("doc stream: receiver dropped during Refs");
            return;
        }
    }
}
