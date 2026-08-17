//! Shared keyset-pagination helper for score-ranked backends.
//!
//! Score-ranked backends (text/tantivy, vector/qdrant, …) paginate over a
//! `(score DESC, key ASC)` total order. Backends without a native seek-to-key
//! over-fetch, sort client-side, and trim past the cursor key. This module
//! factors that loop into a single async generic function, generic over the
//! item type `T`, the tiebreak key `K`, and the backend error `E`.
//!
//! Recovered from the former `registry::runtime::pagination` and generalized:
//! the tiebreak key is now any `K: Ord + Copy` (was the registry-specific
//! `SymbolId`), so any scored-item stream can paginate through it.

use std::future::Future;

use crate::score::{Score, Scored};

/// Perform one keyset-paginated fetch over a score-ranked list.
///
/// # Parameters
/// - `after`: optional resume key `(score, key)`; `None` = first page.
/// - `target`: the page size — at most this many items are returned.
/// - `max_fetch`: the ceiling on the over-fetch depth. The loop doubles
///   `fetch` until `target` is satisfied or this ceiling is reached, so
///   score-tied groups are never split mid-pack.
/// - `key_of`: extracts the stable tiebreak key `K` from a result item (used
///   for the secondary `key ASC` tiebreak in the keyset filter predicate).
/// - `fetch_ranked`: async closure that fetches at most `n` items from the
///   backend. Returns `(hits, exhausted)` where `exhausted` is `true` when the
///   backend returned fewer than `n` results (the result set is fully consumed).
///
/// The hits returned by `fetch_ranked` are re-sorted into the stable
/// `(score DESC, key ASC)` total order before filtering.
///
/// # Returns
/// A page of at most `target` items strictly after `after`, in
/// `(score DESC, key ASC)` total order.
pub async fn keyset_page<T, K, E, F, Fut>(
    after: Option<(Score, K)>,
    target: usize,
    max_fetch: usize,
    key_of: impl Fn(&T) -> K,
    mut fetch_ranked: F,
) -> Result<Vec<Scored<T>>, E>
where
    K: Ord + Copy,
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<(Vec<Scored<T>>, bool), E>>,
{
    let mut fetch = match after {
        None => target,
        Some(_) => (target * 2).min(max_fetch),
    };
    loop {
        let (mut hits, exhausted) = fetch_ranked(fetch).await?;
        // Stable keyset order: score descending, key ascending on ties.
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| key_of(&a.value).cmp(&key_of(&b.value)))
        });

        let page: Vec<Scored<T>> = hits
            .into_iter()
            .filter(|hit| {
                after.is_none_or(|(score, key)| {
                    hit.score < score || (hit.score == score && key_of(&hit.value) > key)
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
