//! The semantic (qdrant) search surface — explicitly gated.

#[allow(unused_imports)]
use crate::server::{registry, vector};
pub mod embedder;

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::num::NonZeroUsize;

use futures::Stream;
use heart::{Language, Score, Scored, SymbolId};
use registry::vector::{
    EmbedRole, Embedder, Embedding, EmbeddingCache, EmbeddingKey, EmbeddingModel, FilterClause,
    Payload, PayloadValue, PointId, SearchFilter, SearchHit, SearchRequest, SemanticGate,
    VectorStore,
};
use strum::IntoEnumIterator;
use vector::remote::store::RemoteStore;

use crate::search::ranking::interleave::interleave_columns;
use crate::server::error::ServerError;
use heart::client::query::{AbstractQuery, SymbolCursor};

use super::Filter;

/// The deepest a keyset resume will over-fetch before truncating a page. Qdrant
/// cannot filter on the *computed* similarity score, so resuming past a cursor
/// means fetching from the top and skipping client-side (capped here).
const MAX_FETCH: usize = 4096;

/// How deep to fetch from qdrant, globally, for an UNSCOPED search (no
/// requested ecosystems) before bucketing by `language` and interleaving
/// (W3c). There is no qdrant-side filter to narrow the candidate pool in this
/// case, so this has to be deep enough that a language whose monikers embed
/// farther from the query text on *average* still gets a real bucket, not
/// just whatever spills into the dominant language's top-k.
///
/// Unlike the scoped path, this depth does **not** vary with `after` (see
/// [`SemanticSurface::search_unscoped_interleaved`]): pagination over the
/// unscoped path recomputes the fetch, bucket, and interleave from scratch on
/// every page — the same fixed depth every time — mirroring the package
/// plane's `retrieve_and_rank`/`rank_score` pattern
/// (`workspace/index/search/pipeline.rs`) rather than pushing a
/// `score_threshold` into qdrant, because once results are interleaved across
/// languages, raw cosine score no longer relates monotonically to SERP
/// position.
const UNSCOPED_FETCH: usize = MAX_FETCH;

/// How deep to fetch from qdrant for a MULTI-ecosystem SCOPED search (G-A:
/// `filter.ecosystems` names 2+ languages) before bucketing by `language` and
/// interleaving, mirroring [`UNSCOPED_FETCH`]'s reasoning. The qdrant-side
/// `language` filter (`language_filter`) already narrows the candidate pool
/// to just the requested ecosystems, so this depth is comfortably deep
/// *per language* — it only ever has to cover a subset of the corpus
/// [`UNSCOPED_FETCH`] has to cover in full.
///
/// A single-language scope skips this path entirely (`search_scoped` only
/// calls into the interleaved path when `ecosystems.len() > 1`) — one bucket
/// has nothing to interleave against, so it keeps the plain real-score,
/// cursor-varies-with-`after` fetch below.
const SCOPED_INTERLEAVE_FETCH: usize = MAX_FETCH;

/// G-B: a small, *guaranteed* per-language sub-fetch depth, topped up onto the
/// unscoped pool for every [`Language`] variant, so a rare language whose best
/// hits embed farther from the query text than the flat top-[`UNSCOPED_FETCH`]
/// pool (raw cosine, pre-interleave) still gets *some* representation instead
/// of zero — even though the interleave downstream would happily give it a
/// slot once bucketed.
///
/// # Mechanism and why
/// [`registry::vector::VectorStore`] only exposes flat top-K search
/// (confirmed: no `group_by`/grouped-search method on the trait, even though
/// the vendored qdrant client crate has one — plumbing that through is a
/// `registry/vector/**` change, out of this file's ownership). Given that,
/// the only way to get an exact per-language floor is one filtered query per
/// language. `Language` is a small, closed set (8 variants today,
/// [`Language::iter`]), so this fans out to `1 + Language::iter().count()`
/// requests total (the main pool fetch plus one per language) — issued
/// **concurrently** via `futures::future::try_join_all` in
/// [`SemanticSurface::search_unscoped_interleaved`], not sequentially. Wall-clock
/// latency is therefore ~one round trip (the slowest of the batch), not `N`×
/// one round trip; the *sizing* of the added qdrant-side work is what's kept
/// small, via this constant being tiny relative to [`UNSCOPED_FETCH`], not the
/// request count.
///
/// This is a **bounded approximation**, not a mathematically exact "every
/// language gets its true top-K" guarantee: two sub-fetches (or the main
/// fetch and a sub-fetch) can't see each other's results, so if a language's
/// true best hits are deeper than `PER_LANGUAGE_FLOOR` within that language
/// alone, they're still invisible to this search. What this constant *does*
/// guarantee is `min(PER_LANGUAGE_FLOOR, <that language's true hit count>)`
/// candidates in the bucket for every language present in the corpus,
/// regardless of how the language's average embedding distance compares to
/// others' — closing the "zero representation" failure mode, not the
/// "shallower than ideal" one.
const PER_LANGUAGE_FLOOR: usize = 128;

pub struct SemanticSurface<'a, M: EmbeddingModel, E: Embedder<Model = M>> {
    store: &'a RemoteStore<M>,
    embedder: &'a E,
    cache: &'a EmbeddingCache<M>,
}

impl<'a, M: EmbeddingModel, E: Embedder<Model = M>> SemanticSurface<'a, M, E> {
    /// Borrow a surface over one source's vector store, sharing the server-wide
    /// embedder + cache (embeddings are model-scoped, not source-scoped).
    pub fn new(store: &'a RemoteStore<M>, embedder: &'a E, cache: &'a EmbeddingCache<M>) -> Self {
        Self {
            store,
            embedder,
            cache,
        }
    }

    pub async fn search(
        &self,
        gate: SemanticGate,
        query: &AbstractQuery,
        filter: &Filter,
        limit: NonZeroUsize,
        after: Option<SymbolCursor>,
    ) -> Result<impl Stream<Item = Result<Scored<SymbolId>, ServerError>> + Send, ServerError> {
        // The gate is the capability token: holding it authorizes this search.
        // The new `VectorStore::search` takes no gate, so the check is here —
        // the store is only reached once the gate is proven.
        tracing::debug!(reason = gate.reason(), "semantic search authorized");

        let embedding = self.embed(query.text()).await?;
        let target = limit.get();
        let after_key = after.as_ref().map(|cursor| cursor.after);

        // W3a/W3b: when the caller named specific ecosystems, push that as a
        // real qdrant payload filter (`language` field) so the vector store
        // itself only ranks within scope — the ranking never sees, let alone
        // truncates before, out-of-scope hits (W3b's silent recall loss).
        //
        // W3c: when unscoped (every language competes), a single global
        // top-k lets whichever language's monikers embed closer to the query
        // text on raw cosine similarity crowd out the rest. Bucket by
        // language and interleave instead, mirroring the package plane's
        // per-ecosystem rank + `interleave_by_ecosystem`
        // (`workspace/index/search/ranking/interleave.rs`).
        let scored = match &filter.ecosystems {
            Some(ecosystems) => {
                self.search_scoped(embedding, ecosystems, target, after_key)
                    .await?
            }
            None => {
                self.search_unscoped_interleaved(embedding, target, after_key)
                    .await?
            }
        };

        Ok(futures::stream::iter(scored.into_iter().map(Ok)))
    }

    /// The scoped path (W3a/W3b): a single qdrant query filtered to the
    /// requested `language` values, so `limit` is honored *within* scope
    /// instead of being computed on an unfiltered global top-k and only
    /// narrowed afterwards.
    ///
    /// G-A: when the scope names 2+ languages, that single combined query
    /// still returns hits in raw-cosine order *across* the requested
    /// languages, so whichever one embeds closer to the query text on
    /// average still crowds out the rest within scope — the same crowd-out
    /// [`SemanticSurface::search_unscoped_interleaved`] (W3c) fixes for the
    /// fully-unscoped case, just narrowed to the requested subset. Delegate
    /// to [`SemanticSurface::search_scoped_interleaved`] for that case; a
    /// single-language scope has exactly one bucket (interleaving one column
    /// is an identity op — see `bucket_then_interleave_single_language_is_unaffected`),
    /// so it isn't worth paying the extra bookkeeping and stays on the plain
    /// real-score path below.
    async fn search_scoped(
        &self,
        embedding: Embedding<M>,
        ecosystems: &nonempty::NonEmpty<Language>,
        target: usize,
        after_key: Option<(Score, SymbolId)>,
    ) -> Result<Vec<Scored<SymbolId>>, ServerError> {
        if ecosystems.len() > 1 {
            return self
                .search_scoped_interleaved(embedding, ecosystems, target, after_key)
                .await;
        }

        // Keyset resume over the `(score, id)` cursor. Qdrant returns best-first
        // but cannot resume from an arbitrary score, so when a cursor is present
        // we over-fetch (capped) and skip past its key client-side; without one
        // we fetch exactly the page.
        let fetch = if after_key.is_some() {
            MAX_FETCH.max(target)
        } else {
            target
        };

        let request = SearchRequest {
            vector: embedding,
            filter: language_filter(ecosystems.iter()),
            limit: fetch,
            // A resuming page prunes everything strictly below the cursor score
            // server-side; ties on the exact score are broken client-side by id.
            score_threshold: after_key.map(|(score, _)| score.into_inner()),
        };

        let hits = self
            .store
            .search(request)
            .await
            .map_err(|error| ServerError::from(error))?;

        // Map each hit back to a scored symbol identity via the payload
        // (`symbol_id`); the derived point id is not the symbol uuid.
        let mut scored: Vec<Scored<SymbolId>> = Vec::with_capacity(hits.len());
        for hit in &hits {
            scored.push(scored_symbol(hit)?);
        }

        sort_best_first_and_resume(&mut scored, after_key);
        scored.truncate(target);
        Ok(scored)
    }

    /// The multi-ecosystem scoped path (G-A), reached from `search_scoped`
    /// only when `ecosystems.len() > 1`: over-fetch [`SCOPED_INTERLEAVE_FETCH`]
    /// deep with the qdrant-side `language` filter still applied (so recall
    /// stays within the requested scope — this only ever changes the *order*
    /// of in-scope results, never which ones are eligible), then bucket by
    /// language and round-robin interleave exactly like
    /// [`SemanticSurface::search_unscoped_interleaved`] (W3c) does for the
    /// fully-unscoped case, via the shared [`bucket_interleave_and_page`].
    ///
    /// Same cursor caveat as the unscoped path: interleaving makes raw qdrant
    /// score non-monotonic with final SERP position, so `bucket_interleave_and_page`
    /// re-stamps a synthetic `rank_score` and resumes over *that*, recomputing
    /// the whole fetch + bucket + interleave from scratch every page (fixed
    /// depth regardless of `after`) rather than varying fetch depth with the
    /// cursor the way the single-language path does.
    async fn search_scoped_interleaved(
        &self,
        embedding: Embedding<M>,
        ecosystems: &nonempty::NonEmpty<Language>,
        target: usize,
        after_key: Option<(Score, SymbolId)>,
    ) -> Result<Vec<Scored<SymbolId>>, ServerError> {
        let request = SearchRequest {
            vector: embedding,
            filter: language_filter(ecosystems.iter()),
            limit: SCOPED_INTERLEAVE_FETCH,
            score_threshold: None,
        };

        let hits = self
            .store
            .search(request)
            .await
            .map_err(|error| ServerError::from(error))?;

        bucket_interleave_and_page(&hits, target, after_key)
    }

    /// The unscoped path (W3c): fetch a fixed-depth global pool (no qdrant
    /// filter — every language is eligible), topped up with a small
    /// guaranteed [`PER_LANGUAGE_FLOOR`] sub-fetch per language (G-B — see
    /// that constant's doc comment for the mechanism and its latency/
    /// exactness tradeoff), then bucket the merged pool by `language` and
    /// round-robin interleave via the shared [`bucket_interleave_and_page`]
    /// so no single language's raw-cosine advantage crowds out the rest.
    ///
    /// Because interleaving reorders across languages, an item's position in
    /// the final SERP no longer tracks its raw qdrant score monotonically, so
    /// that score can't drive a keyset cursor. Instead every item is
    /// re-stamped with a strictly-descending synthetic score keyed on its
    /// final interleaved ordinal (mirroring the package plane's `rank_score`,
    /// `workspace/index/search/pipeline.rs`), and the whole fetch + bucket +
    /// interleave is recomputed from scratch on every page — same fixed
    /// [`UNSCOPED_FETCH`]/[`PER_LANGUAGE_FLOOR`] depths regardless of `after`
    /// — so resuming "after ordinal N" stays well-defined across requests as
    /// long as the underlying qdrant data doesn't shift (the same
    /// best-effort assumption [`heart::Advisory`] cursors already make).
    async fn search_unscoped_interleaved(
        &self,
        embedding: Embedding<M>,
        target: usize,
        after_key: Option<(Score, SymbolId)>,
    ) -> Result<Vec<Scored<SymbolId>>, ServerError> {
        // The main flat pool, plus one small guaranteed sub-fetch per
        // `Language` variant (G-B / `PER_LANGUAGE_FLOOR`). All requests share
        // one `store.search` method (same concrete future type — `VectorStore`
        // is `#[async_trait]`-boxed), so they can be driven concurrently
        // through a single `try_join_all` instead of N sequential round trips.
        let mut requests = Vec::with_capacity(1 + Language::iter().count());
        requests.push(SearchRequest {
            vector: embedding.clone(),
            filter: SearchFilter::default(),
            limit: UNSCOPED_FETCH,
            score_threshold: None,
        });
        requests.extend(Language::iter().map(|language| SearchRequest {
            vector: embedding.clone(),
            filter: language_filter(std::iter::once(&language)),
            limit: PER_LANGUAGE_FLOOR,
            score_threshold: None,
        }));

        let pools = futures::future::try_join_all(
            requests.into_iter().map(|request| self.store.search(request)),
        )
        .await
        .map_err(|error| ServerError::from(error))?;

        // Merge every pool into one candidate set, de-duplicating on the
        // store's point id: the main fetch and a language's own sub-fetch
        // very often return the same points (any well-represented language's
        // true top-K already sits inside the global top-`UNSCOPED_FETCH`), so
        // a hit returned by both would otherwise double-count in its bucket
        // instead of occupying a single interleave slot.
        let mut pools = pools.into_iter();
        let mut hits = pools.next().unwrap_or_default();
        let mut seen: HashSet<PointId> = hits.iter().map(|hit| hit.id).collect();
        for pool in pools {
            for hit in pool {
                if seen.insert(hit.id) {
                    hits.push(hit);
                }
            }
        }

        bucket_interleave_and_page(&hits, target, after_key)
    }

    /// Embed the query text, cache-first: identical text under the same model
    /// never pays the network round-trip twice.
    ///
    /// Queries always use [`EmbedRole::Query`] so Voyage-class models select the
    /// query encoder path.
    async fn embed(&self, text: &str) -> Result<Embedding<M>, ServerError> {
        self.cache
            .get_or_embed(
                EmbeddingKey::new(M::id(), EmbedRole::Query, text),
                self.embedder,
                text,
            )
            .await
            .map_err(|error| ServerError::from(error))
    }
}

/// Recover a [`Scored<SymbolId>`] from a search hit: the [`SymbolId`] comes from
/// the `symbol_id` payload key (the store point id is a derived UUID, not the
/// symbol uuid), and the backend score is validated finite.
fn scored_symbol(hit: &SearchHit) -> Result<Scored<SymbolId>, ServerError> {
    let raw = payload_str(&hit.payload, "symbol_id").ok_or_else(|| {
        ServerError::Internal(crate::server::error::InternalError::Other {
            message: format!("semantic hit {} missing symbol_id payload", hit.id),
        })
    })?;
    let uuid = raw.parse::<uuid::Uuid>().map_err(|error| {
        ServerError::Internal(crate::server::error::InternalError::Other {
            message: format!("semantic hit symbol_id {raw:?} is not a uuid: {error}"),
        })
    })?;
    let score = Score::try_new(hit.score).map_err(|_| {
        ServerError::Internal(crate::server::error::InternalError::Other {
            message: format!("semantic hit {} has a non-finite score", hit.id),
        })
    })?;
    Ok(Scored::new(SymbolId::from_uuid(uuid), score))
}

/// Read a string payload value by key, or `None` if absent / not a string.
fn payload_str<'p>(payload: &'p Payload, key: &str) -> Option<&'p str> {
    match payload.get(key)? {
        PayloadValue::Str(value) => Some(value.as_str()),
        _ => None,
    }
}

/// Like [`scored_symbol`], but also recovers the `language` payload value (the
/// bakery-stamped [`heart::Language`] wire token, `workspace/index/server/bakery/mod.rs::symbol_payload`)
/// so the caller can bucket hits by language before interleaving (W3c).
fn scored_symbol_with_language(hit: &SearchHit) -> Result<(String, Scored<SymbolId>), ServerError> {
    let language = payload_str(&hit.payload, "language").ok_or_else(|| {
        ServerError::Internal(crate::server::error::InternalError::Other {
            message: format!("semantic hit {} missing language payload", hit.id),
        })
    })?;
    Ok((language.to_owned(), scored_symbol(hit)?))
}

/// Shared core of both interleaved search paths (G-A's
/// [`SemanticSurface::search_scoped_interleaved`] and W3c's
/// [`SemanticSurface::search_unscoped_interleaved`]): bucket a candidate pool
/// by its `language` payload, sort each bucket best-first
/// (`sort_best_first_and_resume`), round-robin interleave the buckets
/// (`interleave_columns`), then re-stamp with the synthetic, strictly-
/// descending `rank_score` and apply the keyset cursor + page limit over
/// *that* score (raw qdrant score stops being position-monotonic once hits
/// are interleaved across languages, so it can't drive the cursor).
///
/// The two callers differ only in how `hits` was fetched from qdrant — one
/// language-filtered combined query (G-A) vs. a flat global query merged with
/// small per-language top-up sub-fetches (W3c/G-B) — everything downstream of
/// "here is the candidate pool" is identical, so it lives here once.
fn bucket_interleave_and_page(
    hits: &[SearchHit],
    target: usize,
    after_key: Option<(Score, SymbolId)>,
) -> Result<Vec<Scored<SymbolId>>, ServerError> {
    let mut by_language: BTreeMap<String, Vec<Scored<SymbolId>>> = BTreeMap::new();
    let mut skipped = 0usize;
    for hit in hits {
        match scored_symbol_with_language(hit) {
            Ok((language, scored)) => by_language.entry(language).or_default().push(scored),
            // A single vector with a missing/undecodable `language` or
            // `symbol_id` payload must not sink the whole search — skip it and
            // keep serving the rest, but count it so the drop is observable
            // (a warn), never silent.
            Err(error) => {
                skipped += 1;
                tracing::warn!(hit = %hit.id, %error, "semantic interleave: skipping hit with undecodable payload");
            }
        }
    }
    // ...but if *every* hit failed to decode, that's systemic (schema drift /
    // wrong collection), not one bad row — fail loud rather than silently
    // returning an empty page that a caller reads as "no results".
    if !hits.is_empty() && by_language.is_empty() {
        return Err(ServerError::Internal(
            crate::server::error::InternalError::Other {
                message: format!(
                    "semantic interleave: all {} candidate hits had undecodable payloads",
                    hits.len()
                ),
            },
        ));
    }
    if skipped > 0 {
        tracing::warn!(skipped, total = hits.len(), "semantic interleave dropped undecodable hits");
    }
    // Best-first within each bucket, with a stable id tiebreak — the same
    // ordering contract the single-language scoped path applies, just
    // per-language.
    for bucket in by_language.values_mut() {
        sort_best_first_and_resume(bucket, None);
    }

    let columns: Vec<(String, Vec<Scored<SymbolId>>)> = by_language.into_iter().collect();
    let interleaved = interleave_columns(columns);

    let total = interleaved.len();
    let mut scored: Vec<Scored<SymbolId>> = interleaved
        .into_iter()
        .enumerate()
        .map(|(ordinal, hit)| Scored::new(hit.value, rank_score(ordinal, total)))
        .collect();

    // `scored` is already in strictly-descending synthetic-score (i.e.
    // interleaved-ordinal) order by construction, so this only needs to
    // drop everything at/before the cursor, not re-sort.
    if let Some((after_score, after_id)) = after_key {
        scored.retain(|hit| match hit.score.cmp(&after_score) {
            Ordering::Less => true,
            Ordering::Equal => hit.value > after_id,
            Ordering::Greater => false,
        });
    }
    scored.truncate(target);
    Ok(scored)
}

/// Best-first sort with a stable id tiebreak, then (if a cursor key is given)
/// drop everything at or before it. Shared by the scoped path (over the real
/// qdrant score) and the unscoped path's per-language buckets (before
/// interleaving; called with `after_key: None` there since resume happens
/// once, after the interleave, over the synthetic rank score).
fn sort_best_first_and_resume(scored: &mut Vec<Scored<SymbolId>>, after_key: Option<(Score, SymbolId)>) {
    scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.value.cmp(&b.value)));
    if let Some((after_score, after_id)) = after_key {
        scored.retain(|hit| match hit.score.cmp(&after_score) {
            Ordering::Less => true,
            Ordering::Equal => hit.value > after_id,
            Ordering::Greater => false,
        });
    }
}

/// Build a qdrant payload filter (W3a) admitting only the given languages'
/// `language` field. The payload key/value format is whatever the bakery
/// stamps at write time (`workspace/index/server/bakery/mod.rs::symbol_payload`):
/// key `"language"`, value `PayloadValue::Str(symbol.ecosystem.to_string())`
/// — `Language`'s lowercase wire token (`#[strum(serialize_all = "lowercase")]`,
/// e.g. `"rust"`, `"typescript"`, `"csharp"`). Matched here exactly via the
/// same `Display`/`to_string()`.
fn language_filter<'a>(ecosystems: impl Iterator<Item = &'a Language>) -> SearchFilter {
    SearchFilter {
        must: vec![FilterClause::Any {
            key: "language".into(),
            values: ecosystems
                .map(|ecosystem| PayloadValue::Str(ecosystem.to_string().into()))
                .collect(),
        }],
    }
}

/// Map an interleaved ordinal (`0` = best) onto a strictly-descending,
/// provably finite [`Score`] — see [`SemanticSurface::search_unscoped_interleaved`].
/// `total - ordinal` for `ordinal < total` is always a positive integer well
/// within `f32`'s exact-integer range, so this can never fail to be finite.
fn rank_score(ordinal: usize, total: usize) -> Score {
    Score::try_new((total.saturating_sub(ordinal)) as f32)
        .expect("total - ordinal is a small non-negative integer, always finite")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::registry::vector::{PointId, SourceTag};

    /// Unwrap a `SearchFilter` built by `language_filter` into the plain list
    /// of language tokens it names, asserting the shape (a single `Any`
    /// clause keyed on `"language"`) along the way.
    fn language_tokens(filter: &SearchFilter) -> Vec<String> {
        assert_eq!(filter.must.len(), 1, "expected exactly one filter clause");
        match &filter.must[0] {
            FilterClause::Any { key, values } => {
                assert_eq!(key.as_str(), "language");
                values
                    .iter()
                    .map(|value| match value {
                        PayloadValue::Str(token) => token.to_string(),
                        other => panic!("expected a string payload value, got {other:?}"),
                    })
                    .collect()
            }
            other => panic!("expected FilterClause::Any, got {other:?}"),
        }
    }

    // --- W3a: Filter.ecosystems -> qdrant payload filter -----------------

    #[test]
    fn language_filter_names_the_language_field() {
        let languages = [Language::Rust, Language::Python];
        let filter = language_filter(languages.iter());
        assert_eq!(
            language_tokens(&filter),
            vec!["rust".to_owned(), "python".to_owned()]
        );
    }

    #[test]
    fn language_filter_single_ecosystem() {
        let languages = [Language::Go];
        let filter = language_filter(languages.iter());
        assert_eq!(language_tokens(&filter), vec!["go".to_owned()]);
    }

    #[test]
    fn language_filter_matches_bakery_wire_tokens_for_every_variant() {
        // Regression guard: the filter's values must byte-for-byte match
        // whatever `bakery::symbol_payload` stamped into the point's
        // `language` payload (`symbol.ecosystem.to_string()`), for every
        // `Language` variant — not just the "obvious" ones. `CSharp` is the
        // one where a wrong guess (`"c#"`, `"cs"`) is most tempting.
        use strum::IntoEnumIterator;
        let expected = [
            (Language::Rust, "rust"),
            (Language::Typescript, "typescript"),
            (Language::Python, "python"),
            (Language::Go, "go"),
            (Language::Java, "java"),
            (Language::Nix, "nix"),
            (Language::CSharp, "csharp"),
            (Language::Cpp, "cpp"),
        ];
        // Fails loudly if `Language` grows a variant this test doesn't know
        // about, rather than silently under-covering it.
        assert_eq!(
            expected.len(),
            Language::iter().count(),
            "add the new Language variant's expected wire token above"
        );
        for (language, token) in expected {
            assert_eq!(language.to_string(), token, "{language:?} wire token drifted");
            let filter = language_filter(std::iter::once(&language));
            assert_eq!(language_tokens(&filter), vec![token.to_owned()]);
        }
    }

    // --- rank_score: strictly descending, cursor-friendly -----------------

    #[test]
    fn rank_score_is_strictly_descending_by_ordinal() {
        let total = 5;
        let scores: Vec<f32> = (0..total)
            .map(|ordinal| rank_score(ordinal, total).into_inner())
            .collect();
        for window in scores.windows(2) {
            assert!(window[0] > window[1], "{scores:?} is not strictly descending");
        }
    }

    #[test]
    fn rank_score_best_ordinal_is_zero() {
        // ordinal 0 (the interleave's first item) must sort ahead of every
        // other ordinal under `Scored`'s "higher score first" order.
        assert!(rank_score(0, 10) > rank_score(9, 10));
    }

    // --- scored_symbol_with_language: payload decoding ---------------------

    fn hit(language: Option<&str>, symbol_id: Option<&str>, score: f32) -> SearchHit {
        let mut payload = Payload::new();
        if let Some(language) = language {
            payload.insert("language".into(), PayloadValue::Str(language.into()));
        }
        if let Some(symbol_id) = symbol_id {
            payload.insert("symbol_id".into(), PayloadValue::Str(symbol_id.into()));
        }
        SearchHit {
            id: PointId::from_uuid(uuid::Uuid::new_v4()),
            score,
            payload,
            source: SourceTag::IndexJina,
        }
    }

    #[test]
    fn scored_symbol_with_language_recovers_both_fields() {
        let symbol_id = uuid::Uuid::new_v4();
        let hit = hit(Some("rust"), Some(&symbol_id.to_string()), 0.87);
        let (language, scored) = scored_symbol_with_language(&hit).expect("both fields present");
        assert_eq!(language, "rust");
        assert_eq!(scored.value, SymbolId::from_uuid(symbol_id));
    }

    #[test]
    fn scored_symbol_with_language_errors_when_language_missing() {
        let symbol_id = uuid::Uuid::new_v4();
        let hit = hit(None, Some(&symbol_id.to_string()), 0.5);
        assert!(scored_symbol_with_language(&hit).is_err());
    }

    // --- bucket-then-interleave, end to end over Scored<SymbolId> ----------
    //
    // `interleave_columns` itself is unit-tested generically in
    // `workspace/index/search/ranking/interleave.rs`; this exercises the
    // exact composition `search_unscoped_interleaved` performs — bucket a
    // mixed-language `Vec<Scored<SymbolId>>` by language, sort each bucket
    // best-first, interleave, then re-stamp with the synthetic rank score —
    // without needing a live qdrant.

    fn scored(id_byte: u8, score: f32) -> Scored<SymbolId> {
        let mut bytes = [0u8; 16];
        bytes[15] = id_byte;
        Scored::new(
            SymbolId::from_uuid(uuid::Uuid::from_bytes(bytes)),
            Score::try_new(score).unwrap(),
        )
    }

    #[test]
    fn bucket_then_interleave_gives_every_language_a_fair_share() {
        // Python "dominates" on raw score (as W3c describes: whichever
        // language's monikers embed closer to the query text wins a global
        // top-k), but the fair merge must not let it crowd out rust/go.
        let mut by_language: BTreeMap<String, Vec<Scored<SymbolId>>> = BTreeMap::new();
        by_language.insert(
            "python".to_owned(),
            vec![scored(1, 0.99), scored(2, 0.98), scored(3, 0.97), scored(4, 0.96)],
        );
        by_language.insert("rust".to_owned(), vec![scored(5, 0.80)]);
        by_language.insert("go".to_owned(), vec![scored(6, 0.75)]);

        for bucket in by_language.values_mut() {
            sort_best_first_and_resume(bucket, None);
        }
        let columns: Vec<(String, Vec<Scored<SymbolId>>)> = by_language.into_iter().collect();
        let interleaved = interleave_columns(columns);

        // go < python < rust by key order; each column contributes its head
        // before any column's second item appears.
        let order: Vec<u8> = interleaved
            .iter()
            .map(|s| s.value.as_uuid().as_bytes()[15])
            .collect();
        assert_eq!(order, vec![6, 1, 5, 2, 3, 4]);

        // Re-stamping with the synthetic rank score must land the first
        // three languages (go, rust, python-head) ahead of python's
        // remaining tail, matching the interleaved order exactly — i.e. the
        // page a caller sees for `limit = 3` is a genuine cross-language mix,
        // not python's own top 3.
        let total = interleaved.len();
        let restamped: Vec<Scored<SymbolId>> = interleaved
            .into_iter()
            .enumerate()
            .map(|(ordinal, hit)| Scored::new(hit.value, rank_score(ordinal, total)))
            .collect();
        let mut sorted_by_restamped_score = restamped.clone();
        sorted_by_restamped_score.sort_by(|a, b| b.score.cmp(&a.score));
        let top_three: Vec<u8> = sorted_by_restamped_score[..3]
            .iter()
            .map(|s| s.value.as_uuid().as_bytes()[15])
            .collect();
        assert_eq!(top_three, vec![6, 1, 5], "top 3 must be the fair interleave, not python's own top 3 (1,2,3)");
    }

    #[test]
    fn bucket_then_interleave_single_language_is_unaffected() {
        let mut by_language: BTreeMap<String, Vec<Scored<SymbolId>>> = BTreeMap::new();
        by_language.insert("rust".to_owned(), vec![scored(1, 0.9), scored(2, 0.5)]);
        for bucket in by_language.values_mut() {
            sort_best_first_and_resume(bucket, None);
        }
        let columns: Vec<(String, Vec<Scored<SymbolId>>)> = by_language.into_iter().collect();
        let interleaved = interleave_columns(columns);
        let order: Vec<u8> = interleaved.iter().map(|s| s.value.as_uuid().as_bytes()[15]).collect();
        assert_eq!(order, vec![1, 2]);
    }

    // --- bucket_interleave_and_page: the shared G-A/W3c-G-B core -----------
    //
    // These exercise the exact function `search_scoped_interleaved` (G-A) and
    // `search_unscoped_interleaved` (W3c/G-B) both call once they have their
    // candidate pool in hand, using plain synthetic `SearchHit`s instead of a
    // live qdrant fetch — i.e. this is the seam a reviewer should target for
    // "a 2-language scope where one language dominates raw cosine".

    fn hit_with_id(language: &str, id_byte: u8, score: f32) -> SearchHit {
        let mut symbol_bytes = [0u8; 16];
        symbol_bytes[15] = id_byte;
        let symbol_id = uuid::Uuid::from_bytes(symbol_bytes);
        hit(Some(language), Some(&symbol_id.to_string()), score)
    }

    #[test]
    fn bucket_interleave_and_page_fairness_over_a_two_language_scoped_pool() {
        // G-A: simulates `search_scoped_interleaved`'s candidate pool for a
        // `[python, rust]` scope where python's monikers embed closer to the
        // query text on raw cosine — a flat qdrant top-k over the combined
        // filter would return python's 4 hits before rust's single hit ever
        // appears. The interleave must not let that happen.
        let hits = vec![
            hit_with_id("python", 1, 0.99),
            hit_with_id("python", 2, 0.98),
            hit_with_id("python", 3, 0.97),
            hit_with_id("python", 4, 0.96),
            hit_with_id("rust", 5, 0.50),
        ];
        let page = bucket_interleave_and_page(&hits, 2, None).expect("valid pool");
        let order: Vec<u8> = page.iter().map(|s| s.value.as_uuid().as_bytes()[15]).collect();
        // python < rust by key order, so python's head goes first, but rust's
        // sole hit must land in the first page (limit 2) despite its raw
        // score trailing python's entire top 4 — a raw-cosine sort would have
        // produced [1, 2] here instead.
        assert_eq!(order, vec![1, 5], "rust must not be crowded out of a 2-language scope by python's raw-cosine lead");
    }

    #[test]
    fn bucket_interleave_and_page_cursor_resumes_over_the_synthetic_rank_score() {
        // Page 1 (no cursor) establishes the interleaved order; page 2 must
        // pick up exactly where page 1 left off when resumed with the last
        // item's *returned* (synthetic) score/id, not its original qdrant
        // score — this is the keyset-cursor coherence trick both interleaved
        // paths rely on.
        let hits = vec![
            hit_with_id("go", 1, 0.40),
            hit_with_id("python", 2, 0.99),
            hit_with_id("python", 3, 0.90),
            hit_with_id("rust", 4, 0.60),
        ];
        let page1 = bucket_interleave_and_page(&hits, 2, None).expect("valid pool");
        assert_eq!(page1.len(), 2);
        let cursor = (page1[1].score, page1[1].value);

        let page2 = bucket_interleave_and_page(&hits, 2, Some(cursor)).expect("valid pool");
        let full = bucket_interleave_and_page(&hits, hits.len(), None).expect("valid pool");
        assert_eq!(
            page1.iter().chain(page2.iter()).map(|s| s.value).collect::<Vec<_>>(),
            full.iter().map(|s| s.value).collect::<Vec<_>>(),
            "page1 ++ page2 (resumed via the synthetic cursor) must equal the unpaginated full order"
        );
    }

    // --- reviewer adversarial battery (harder inputs than the sanity set) ---

    #[test]
    fn adversarial_full_pagination_walk_has_no_gaps_or_dups() {
        // Uneven buckets across four languages, walked one page at a time to
        // exhaustion for several page sizes — the concatenation must equal the
        // unpaginated order exactly: no skipped item, no repeat.
        let hits = vec![
            hit_with_id("python", 1, 0.99),
            hit_with_id("python", 2, 0.98),
            hit_with_id("python", 3, 0.97),
            hit_with_id("rust", 4, 0.90),
            hit_with_id("rust", 5, 0.20),
            hit_with_id("go", 6, 0.55),
            hit_with_id("java", 7, 0.10),
        ];
        let full = bucket_interleave_and_page(&hits, hits.len(), None).expect("pool");
        let full_ids: Vec<u8> = full.iter().map(|s| s.value.as_uuid().as_bytes()[15]).collect();
        assert_eq!(full_ids.len(), hits.len(), "full page returns every decodable hit once");

        for page_size in [1usize, 2, 3] {
            let mut walked: Vec<u8> = Vec::new();
            let mut cursor: Option<(Score, SymbolId)> = None;
            loop {
                let page = bucket_interleave_and_page(&hits, page_size, cursor).expect("pool");
                if page.is_empty() {
                    break;
                }
                for s in &page {
                    walked.push(s.value.as_uuid().as_bytes()[15]);
                }
                let last = page.last().unwrap();
                cursor = Some((last.score, last.value));
            }
            assert_eq!(
                walked, full_ids,
                "page_size {page_size}: paginated walk must reproduce the full order with no gaps or dups"
            );
        }
    }

    #[test]
    fn adversarial_tied_scores_within_a_bucket_break_by_id_deterministically() {
        // Three same-language hits with identical raw scores: must order by
        // ascending id (stable tiebreak) and be identical across repeated calls.
        let hits = vec![
            hit_with_id("rust", 9, 0.5),
            hit_with_id("rust", 3, 0.5),
            hit_with_id("rust", 7, 0.5),
        ];
        let a = bucket_interleave_and_page(&hits, 3, None).expect("pool");
        let b = bucket_interleave_and_page(&hits, 3, None).expect("pool");
        let ids_a: Vec<u8> = a.iter().map(|s| s.value.as_uuid().as_bytes()[15]).collect();
        let ids_b: Vec<u8> = b.iter().map(|s| s.value.as_uuid().as_bytes()[15]).collect();
        assert_eq!(ids_a, ids_b, "same input must yield the same order");
        assert_eq!(ids_a, vec![3, 7, 9], "tied scores break by ascending id");
    }

    #[test]
    fn adversarial_empty_pool_and_zero_target_are_empty_not_panics() {
        assert!(bucket_interleave_and_page(&[], 10, None).expect("empty ok").is_empty());
        let hits = vec![hit_with_id("rust", 1, 0.5)];
        assert!(bucket_interleave_and_page(&hits, 0, None).expect("zero target ok").is_empty());
    }

    #[test]
    fn adversarial_one_malformed_hit_does_not_sink_the_whole_search() {
        // A hit missing its `language` payload, and one with a non-uuid
        // symbol_id, must be skipped — not turned into a 500 that drops every
        // other result. Regression guard for the "one bad row kills the search"
        // fragility the reviewer found.
        let good_rust = hit_with_id("rust", 1, 0.9);
        let good_go = hit_with_id("go", 2, 0.8);
        let missing_language = hit(None, Some(&uuid::Uuid::new_v4().to_string()), 0.95);
        let bad_symbol_id = hit(Some("python"), Some("not-a-uuid"), 0.99);
        let hits = vec![missing_language, good_rust, bad_symbol_id, good_go];
        let page = bucket_interleave_and_page(&hits, 10, None).expect("partial corruption still serves");
        let ids: Vec<u8> = page.iter().map(|s| s.value.as_uuid().as_bytes()[15]).collect();
        assert_eq!(ids.len(), 2, "the two decodable hits survive; the two bad ones are skipped");
        assert!(ids.contains(&1) && ids.contains(&2));
    }

    #[test]
    fn adversarial_total_payload_corruption_fails_loud_not_silent_empty() {
        // If EVERY hit is undecodable, that's systemic — must error, not return
        // an empty page that reads as an honest "no matches".
        let hits = vec![
            hit(None, Some(&uuid::Uuid::new_v4().to_string()), 0.9),
            hit(Some("rust"), Some("also-not-a-uuid"), 0.8),
        ];
        assert!(
            bucket_interleave_and_page(&hits, 10, None).is_err(),
            "all-undecodable must fail loud, never silently empty"
        );
    }

    #[test]
    fn adversarial_stale_cursor_does_not_panic_and_stays_descending() {
        // A cursor whose id matches no current item (data shifted under an
        // Advisory cursor) must degrade gracefully: no panic, page stays
        // descending by synthetic score.
        let hits = vec![
            hit_with_id("go", 1, 0.4),
            hit_with_id("python", 2, 0.99),
            hit_with_id("rust", 3, 0.6),
        ];
        let bogus = (rank_score(1, hits.len()), scored(250, 0.0).value);
        let page = bucket_interleave_and_page(&hits, 10, Some(bogus)).expect("no panic on stale cursor");
        for w in page.windows(2) {
            assert!(w[0].score >= w[1].score, "page must stay descending by synthetic score");
        }
    }

    #[test]
    fn adversarial_every_present_language_appears_in_a_page_sized_to_their_count() {
        // Fairness guarantee under a dominating language: a page sized to the
        // number of present languages contains each language's head, never the
        // dominant language's own top-N.
        let hits = vec![
            hit_with_id("python", 1, 0.99),
            hit_with_id("python", 2, 0.98),
            hit_with_id("python", 3, 0.97),
            hit_with_id("python", 4, 0.96),
            hit_with_id("rust", 5, 0.50),
            hit_with_id("go", 6, 0.40),
        ];
        // languages present: go, python, rust => 3
        let page = bucket_interleave_and_page(&hits, 3, None).expect("pool");
        let ids: HashSet<u8> = page.iter().map(|s| s.value.as_uuid().as_bytes()[15]).collect();
        assert!(
            ids.contains(&6) && ids.contains(&1) && ids.contains(&5),
            "a 3-slot page over 3 languages must include each language's head, got {ids:?} (not python's own top-3)"
        );
    }
}
