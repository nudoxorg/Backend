//! Shared keyset-pagination helper for score-ranked backends.
//!
//! Both the text (tantivy) and vector (qdrant) backends paginate over a
//! `(score DESC, id ASC)` total order. Neither backend has a native seek-to-key,
//! so both over-fetch, sort client-side, and trim past the cursor key. This
//! module factors that loop into a single async generic function.

use std::future::Future;

use heart::{Score, Scored, SymbolId};

/// Perform one keyset-paginated fetch over a score-ranked list.
///
/// # Parameters
/// - `after`: optional resume key `(score, id)`; `None` = first page.
/// - `target`: the page size — at most this many items are returned.
/// - `max_fetch`: the ceiling on the over-fetch depth. The loop doubles
///   `fetch` until `target` is satisfied or this ceiling is reached, so
///   score-tied groups are never split mid-pack.
/// - `id_of`: extracts the stable [`SymbolId`] from a result item (used for
///   the secondary `id ASC` tiebreak in the keyset filter predicate).
/// - `fetch_ranked`: async closure that fetches at most `n` items from the
///   backend. Returns `(hits, exhausted)` where `exhausted` is `true` when
///   the backend returned fewer than `n` results (i.e., the result set is
///   fully consumed).
///
/// The hits returned by `fetch_ranked` are re-sorted into the stable
/// `(score DESC, id ASC)` total order before filtering.
///
/// # Returns
/// A page of at most `target` items strictly after `after`, in `(score DESC,
/// id ASC)` total order.
pub(crate) async fn keyset_page<T, E, F, Fut>(
    after: Option<(Score, SymbolId)>,
    target: usize,
    max_fetch: usize,
    id_of: impl Fn(&T) -> SymbolId,
    mut fetch_ranked: F,
) -> Result<Vec<Scored<T>>, E>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<(Vec<Scored<T>>, bool), E>>,
{
    let mut fetch = match after {
        None => target,
        Some(_) => (target * 2).min(max_fetch),
    };
    loop {
        let (mut hits, exhausted) = fetch_ranked(fetch).await?;
        // Stable keyset order: score descending, id ascending on ties.
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| id_of(&a.value).cmp(&id_of(&b.value)))
        });

        let page: Vec<Scored<T>> = hits
            .into_iter()
            .filter(|hit| {
                after.is_none_or(|(score, id)| {
                    hit.score < score || (hit.score == score && id_of(&hit.value) > id)
                })
            })
            .take(target)
            .collect();

        if !exhausted && fetch < max_fetch {
            fetch = (fetch * 2).min(max_fetch);
            continue;
        }
        return Ok(page);
    }
}
