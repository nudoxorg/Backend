//! The semantic (qdrant) search surface — explicitly gated.

#[allow(unused_imports)]
use crate::server::{registry, vector};
pub mod embedder;

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use futures::Stream;
use heart::{Language, Score, Scored, SymbolId};
use registry::vector::{
    EmbedRole, Embedder, Embedding, EmbeddingCache, EmbeddingKey, EmbeddingModel, FilterClause,
    Payload, PayloadValue, SearchFilter, SearchHit, SearchRequest, SemanticGate, VectorStore,
};
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
    async fn search_scoped(
        &self,
        embedding: Embedding<M>,
        ecosystems: &nonempty::NonEmpty<Language>,
        target: usize,
        after_key: Option<(Score, SymbolId)>,
    ) -> Result<Vec<Scored<SymbolId>>, ServerError> {
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

    /// The unscoped path (W3c): fetch a fixed-depth global pool (no qdrant
    /// filter — every language is eligible), bucket the hits by their
    /// `language` payload value, then round-robin interleave the buckets so
    /// no single language's raw-cosine advantage crowds out the rest.
    ///
    /// Because interleaving reorders across languages, an item's position in
    /// the final SERP no longer tracks its raw qdrant score monotonically, so
    /// that score can't drive a keyset cursor. Instead every item is
    /// re-stamped with a strictly-descending synthetic score keyed on its
    /// final interleaved ordinal (mirroring the package plane's `rank_score`,
    /// `workspace/index/search/pipeline.rs`), and the whole fetch + bucket +
    /// interleave is recomputed from scratch on every page — same fixed
    /// [`UNSCOPED_FETCH`] depth regardless of `after` — so resuming "after
    /// ordinal N" stays well-defined across requests as long as the
    /// underlying qdrant data doesn't shift (the same best-effort assumption
    /// [`heart::Advisory`] cursors already make).
    async fn search_unscoped_interleaved(
        &self,
        embedding: Embedding<M>,
        target: usize,
        after_key: Option<(Score, SymbolId)>,
    ) -> Result<Vec<Scored<SymbolId>>, ServerError> {
        let request = SearchRequest {
            vector: embedding,
            filter: SearchFilter::default(),
            limit: UNSCOPED_FETCH,
            score_threshold: None,
        };

        let hits = self
            .store
            .search(request)
            .await
            .map_err(|error| ServerError::from(error))?;

        let mut by_language: BTreeMap<String, Vec<Scored<SymbolId>>> = BTreeMap::new();
        for hit in &hits {
            let (language, scored) = scored_symbol_with_language(hit)?;
            by_language.entry(language).or_default().push(scored);
        }
        // Best-first within each bucket, with a stable id tiebreak — the same
        // ordering contract the scoped path applies, just per-language.
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
}
