//! Registry search — searching the *registry itself* (finding packages), as
//! distinct from symbol/code search (which lives in the serving plane).
//!
//! # Single entry
//!
//! High-level discovery **must** go through [`pipeline::retrieve_and_rank`] /
//! [`pipeline::retrieve_and_rank_page`]. [`PackageSearchRequest`] is the only
//! query carrier; callers build it directly and feed it into the pipeline.
//! Wrappers cannot bypass structured parse + intent-aware rank + (when
//! unscoped) per-eco interleave.
//!
//! **LocalEnrichment** is desktop-only and is **not** applied on this path.

use heart::{PackageId, Scored, search::Page};

use crate::{GlobalPackage, error::SearchError, metadata::Synonyms};

// Retrieval / query-parse plane (the `search` root).
pub mod alias;
/// Typed ID-8 ranking factors. The cascade in `ranking` is the lexical
/// pipeline; this module is the scored fusion with named weights.
pub mod factors;
pub mod id8;
pub mod health;
pub mod pipeline;
pub mod spell;
pub mod structured;
pub mod tantivy;
pub mod usages;

// Ranking plane (signals, the scoring cascade, gates, fusion, interleave, eval).
pub mod ranking;

// Modules whose paths are referenced by name from outside the crate
// (`registry::search::{gates,listing_signals,squat,local_enrichment}` in
// `server::coordination::indexing` and `gui`) are re-exported so the move into
// `ranking/` stays source-compatible.
pub use ranking::{gates, listing_signals, local_enrichment, squat};

// pipeline.rs uses `super::popularity` and `super::rrf` by module path;
// re-export so those references resolve without moving the pipeline file.
pub use ranking::{eval, interleave, multi_parent, policy, popularity, rrf};

pub use alias::{
    AliasConfidence, AliasExpander, ResolvedAlias, expand_cpp_bare_token, presence_facet,
};
pub use health::PackageIndexHealth;
pub use pipeline::{
    PackageSearchDeps, PackageSearchRequest, retrieve_and_rank, retrieve_and_rank_page,
};
pub use ranking::local_enrichment::{
    DepRelation, EnrichedHit, HitSource, LocalContext, LocalEnrichment, LocalLabel,
    LocalOnlyPackage, RankedItem, UsageStat,
};
pub use ranking::popularity::assign_percentiles;
pub use structured::StructuredQuery;
pub use usages::{
    ReverseIndexUsageBackend, SharedUsageBackend, Unsupported, Usage, UsageQueryBackend,
    UsageQueryError, build_usage_scope,
};

/// The keyset a registry-search cursor advances over.
pub type SearchKey = (heart::Score, PackageId);

/// The single, canonical keyset resume predicate for package search
/// (INDEX-PLAN cursor single-home).
///
/// A hit is *after* the cursor key under the `(score desc, id asc)` total order
/// iff it either scores strictly lower, or ties on score with a strictly higher
/// id. This is the **one** place the ordering half of pagination is expressed;
/// both the per-source ranking pipeline ([`pipeline::retrieve_and_rank_page`])
/// and the federated merge (`server::Server::search_packages`) call it, so the
/// resume seam can never drift between the two decode sites the review flagged.
///
/// `after` is `None` for a first page (every hit qualifies).
#[must_use]
pub fn keyset_is_after(after: Option<SearchKey>, score: heart::Score, id: PackageId) -> bool {
    match after {
        None => true,
        Some((cursor_score, cursor_id)) => {
            score < cursor_score || (score == cursor_score && id > cursor_id)
        }
    }
}

/// Did-you-mean suggestions for free terms (does not re-query the index).
#[must_use]
pub fn dym_suggestions_for_request(
    vocab: impl IntoIterator<Item = impl AsRef<str>>,
    query: &PackageSearchRequest,
    limit: usize,
) -> Vec<String> {
    let sq = StructuredQuery::parse(&query.text, query.ecosystem);
    if sq.terms.is_empty() {
        return Vec::new();
    }
    spell::dym_suggestions(vocab, &sq.terms, limit)
}

/// Optional zero-hit auto-repair for bare free-text queries.
#[must_use]
pub fn maybe_repair_request(
    spell_index: &spell::SpellIndex,
    query: &PackageSearchRequest,
) -> Option<PackageSearchRequest> {
    let sq = StructuredQuery::parse(&query.text, query.ecosystem);
    if sq.terms.is_empty() {
        return None;
    }
    if query.text.trim() != sq.terms.trim() {
        return None;
    }
    let repaired = spell::maybe_repair_query(spell_index, &sq.terms)?;
    Some(PackageSearchRequest {
        text: repaired,
        ecosystem: query.ecosystem,
        limit: query.limit,
        after: query.after.clone(),
        semantic: query.semantic.clone(),
    })
}

/// One page of results — thin wrapper over [`retrieve_and_rank_page`].
pub async fn search_page(
    index: &tantivy::PackageIndex,
    query: &PackageSearchRequest,
    synonyms: Option<&Synonyms>,
) -> Result<Page<GlobalPackage>, SearchError> {
    retrieve_and_rank_page(
        index,
        query,
        PackageSearchDeps {
            synonyms,
            ..Default::default()
        },
    )
    .await
}

/// Full ranked order — thin wrapper over [`retrieve_and_rank`].
pub async fn collect_ranked_hits(
    index: &tantivy::PackageIndex,
    query: &PackageSearchRequest,
    synonyms: Option<&Synonyms>,
) -> Result<Vec<Scored<GlobalPackage>>, SearchError> {
    collect_ranked_hits_hybrid(index, query, &[], synonyms).await
}

/// Hybrid BM25 + semantic RRF path.
pub async fn collect_ranked_hits_hybrid(
    index: &tantivy::PackageIndex,
    query: &PackageSearchRequest,
    semantic: &[PackageId],
    synonyms: Option<&Synonyms>,
) -> Result<Vec<Scored<GlobalPackage>>, SearchError> {
    let mut req = query.clone();
    req.semantic = semantic.to_vec();
    retrieve_and_rank(
        index,
        &req,
        PackageSearchDeps {
            synonyms,
            ..Default::default()
        },
    )
    .await
}

/// Clamp a raw tantivy score onto the provably-finite [`heart::Score`].
pub fn finite_score(raw: f32) -> heart::Score {
    heart::Score::try_new(raw)
        .unwrap_or_else(|_| heart::Score::try_new(0.0).expect("0.0 is finite"))
}

#[cfg(test)]
mod keyset_home_tests {
    use super::{SearchKey, keyset_is_after};
    use heart::{PackageId, Score};
    use uuid::Uuid;

    fn score(raw: f32) -> Score {
        Score::try_new(raw).expect("finite")
    }

    fn id(byte: u8) -> PackageId {
        PackageId::from_uuid(Uuid::from_bytes([byte; 16]))
    }

    /// A `None` cursor (first page) admits every hit.
    #[test]
    fn first_page_admits_everything() {
        assert!(keyset_is_after(None, score(9.0), id(1)));
        assert!(keyset_is_after(None, score(0.0), id(255)));
    }

    /// Under `(score desc, id asc)`: strictly-lower score is after; equal score
    /// with a strictly-higher id is after; the cursor key itself is not after
    /// (strictness → no repeat at the page seam).
    #[test]
    fn ordering_is_score_desc_then_id_asc() {
        let cursor: SearchKey = (score(5.0), id(10));
        // Lower score → after.
        assert!(keyset_is_after(Some(cursor), score(4.9), id(1)));
        // Higher score → not after.
        assert!(!keyset_is_after(Some(cursor), score(5.1), id(255)));
        // Tie on score, higher id → after.
        assert!(keyset_is_after(Some(cursor), score(5.0), id(11)));
        // Tie on score, lower id → not after.
        assert!(!keyset_is_after(Some(cursor), score(5.0), id(9)));
        // The cursor key itself is excluded (no page-boundary repeat).
        assert!(!keyset_is_after(Some(cursor), score(5.0), id(10)));
    }

    /// Single-home seam property: paging a fixed descending order with the
    /// predicate never repeats and never skips a hit. This is the regression the
    /// review flagged — the *one* predicate governs both the per-source pipeline
    /// and the federated merge, so page-1 → resume is seam-free.
    #[test]
    fn paging_is_seam_free_over_a_fixed_order() {
        // A total order under (score desc, id asc): distinct scores keep it simple.
        let order: Vec<SearchKey> = (0..10u8)
            .map(|n| (score(10.0 - f32::from(n)), id(n)))
            .collect();

        let mut seen: Vec<SearchKey> = Vec::new();
        let mut after: Option<SearchKey> = None;
        let page_size = 3;
        loop {
            let page: Vec<SearchKey> = order
                .iter()
                .copied()
                .filter(|(s, i)| keyset_is_after(after, *s, *i))
                .take(page_size)
                .collect();
            if page.is_empty() {
                break;
            }
            seen.extend_from_slice(&page);
            after = page.last().copied();
        }

        assert_eq!(
            seen, order,
            "paging must reproduce the whole order exactly once"
        );
    }
}
