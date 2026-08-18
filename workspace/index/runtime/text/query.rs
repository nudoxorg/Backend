//! Querying the tantivy text index — the default interface for searching through
//! items by name/signature.

use std::num::NonZeroUsize;

use futures::{Stream, TryFutureExt};
use serde::{Deserialize, Serialize};

use heart::{Cursor, Enforced, Language, Score, Scored, Symbol, SymbolKind};

use tantivy::{
    IndexReader, TantivyDocument, Term,
    collector::TopDocs,
    query::{BooleanQuery, Occur, Query, RegexQuery, TermQuery},
    schema::{Field, IndexRecordOption},
};

use crate::runtime::{
    error::{TextError, TextQueryError},
    pagination,
    text::{
        TextCursorKey,
        index::{TextIndex, TextSchema, kind_token, snapshot_hash, symbol_from_document},
        tokenizer,
    },
};

/// The deepest a keyset resume will dig into the ranking before truncating.
const MAX_FETCH: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextQuery {
    pub terms: String,
    pub ecosystem: Option<Language>,
    pub kinds: Vec<SymbolKind>,
}

impl TextQuery {
    pub fn new(terms: impl Into<String>) -> Self {
        Self {
            terms: terms.into(),
            ecosystem: None,
            kinds: Vec::new(),
        }
    }
}

impl heart::Probeable for TextIndex {
    fn backend(&self) -> heart::BackendKind {
        heart::BackendKind::Tantivy
    }

    async fn probe(&self) -> heart::Probe {
        use futures::StreamExt;
        heart::timed_probe(heart::BackendKind::Tantivy, async {
            let query = TextQuery::new("readiness");
            let hits = self.search(&query, NonZeroUsize::MIN, None);
            futures::pin_mut!(hits);
            match hits.next().await {
                None | Some(Ok(_)) => None,
                Some(Err(error)) => Some(error.to_string()),
            }
        })
        .await
    }
}

const _: fn() = || {
    heart::assert_probe_future_send::<TextIndex>();
};

impl TextIndex {
    /// Fetch a single symbol by its exact [`SymbolId`].
    ///
    /// Uses a term query on the stored `id` field — no tokenization, exact UUID
    /// match.
    pub async fn find_by_id(&self, id: heart::SymbolId) -> Result<Option<Symbol>, TextError> {
        let reader = self.reader.clone();
        let schema = self.schema().clone();
        let id_str = id.as_uuid().to_string();
        tokio::task::spawn_blocking(move || {
            reader.reload().map_err(TextError::Engine)?;
            let searcher = reader.searcher();
            let term = Term::from_field_text(schema.id, &id_str);
            let query = TermQuery::new(term, IndexRecordOption::Basic);
            let docs = searcher
                .search(&query, &TopDocs::with_limit(1))
                .map_err(TextError::Engine)?;
            docs.into_iter()
                .next()
                .map(|(_, address)| {
                    let document: TantivyDocument =
                        searcher.doc(address).map_err(TextError::Engine)?;
                    symbol_from_document(&schema, &document)
                })
                .transpose()
        })
        .await
        .unwrap_or_else(|join_error| Err(TextError::Io(std::io::Error::other(join_error))))
    }

    /// Search the text index, returning a stream of scored symbols.
    ///
    /// `after`, if provided, must be a `Cursor<TextCursorKey, Enforced>` —
    /// the [`Enforced`] brand signals that this cursor was minted against the
    /// live snapshot at the time the previous page was served. If the index has
    /// since been committed, this call returns [`TextError::Cursor`] so the
    /// caller can restart from the first page.
    ///
    /// The snapshot check and the keyset pagination loop share the same
    /// backing function as the vector (qdrant) path via
    /// [`crate::runtime::pagination::keyset_page`], keeping the over-fetch / sort /
    /// filter logic in one place.
    pub fn search(
        &self,
        query: &TextQuery,
        limit: NonZeroUsize,
        after: Option<Cursor<TextCursorKey, Enforced>>,
    ) -> impl Stream<Item = Result<Scored<Symbol>, TextError>> + Send {
        let reader = self.reader.clone();
        let schema = self.schema().clone();
        let query = query.clone();
        async move {
            // Snapshot check + page fetch; every tantivy call is behind
            // spawn_blocking so the async workers are never stalled.
            let hits = search_async(reader, schema, query, limit, after).await?;
            Ok(futures::stream::iter(hits.into_iter().map(Ok)))
        }
        .try_flatten_stream()
    }
}

/// The async search entry point. Performs a snapshot check and then delegates
/// the keyset loop to the shared [`pagination::keyset_page`] helper.
async fn search_async(
    reader: IndexReader,
    schema: TextSchema,
    query: TextQuery,
    limit: NonZeroUsize,
    after: Option<Cursor<TextCursorKey, Enforced>>,
) -> Result<Vec<Scored<Symbol>>, TextError> {
    // Reload the reader and grab the live snapshot — both are cheap disk reads.
    // Run on spawn_blocking because even a manual reload touches mmap'd files.
    let (live_snapshot, parsed) = {
        let reader = reader.clone();
        let schema = schema.clone();
        let query = query.clone();
        tokio::task::spawn_blocking(move || {
            reader.reload().map_err(TextError::Engine)?;
            let searcher = reader.searcher();
            let live_snapshot = snapshot_hash(&searcher);
            // Arc wrapping lets the query be shared across spawn_blocking tasks
            // without cloning (tantivy queries are Send+Sync but not Clone).
            let parsed: std::sync::Arc<dyn tantivy::query::Query> = build_query(&schema, &query)?;
            Ok::<_, TextError>((live_snapshot, parsed))
        })
        .await
        .unwrap_or_else(|e| Err(TextError::Io(std::io::Error::other(e))))?
    };

    // A cursor minted against an older snapshot no longer addresses this
    // ranking; reject it so the caller can restart cleanly.
    if let Some(cursor) = &after
        && cursor.snapshot != live_snapshot
    {
        return Err(TextError::Cursor(heart::CursorError::StaleSnapshot));
    }

    let target = limit.get();
    let after_key = after.as_ref().map(|c| c.after);

    // Delegate the over-fetch / sort / filter loop to the shared helper.
    // Each fetch iteration is a spawn_blocking call so the async worker is
    // never stalled on tantivy's synchronous search.
    pagination::keyset_page(
        after_key,
        target,
        MAX_FETCH,
        |symbol: &Symbol| symbol.id,
        |fetch| {
            // Arc::clone is O(1) — we share the immutable parsed query rather
            // than rebuilding it on every loop iteration.
            let reader = reader.clone();
            let schema = schema.clone();
            let parsed = std::sync::Arc::clone(&parsed);
            async move {
                tokio::task::spawn_blocking(move || {
                    reader.reload().map_err(TextError::Engine)?;
                    let searcher = reader.searcher();
                    let ranked = searcher
                        .search(parsed.as_ref(), &TopDocs::with_limit(fetch))
                        .map_err(TextError::Engine)?;
                    let exhausted = ranked.len() < fetch;
                    let hits = ranked
                        .into_iter()
                        .map(|(score, address)| {
                            let document: TantivyDocument =
                                searcher.doc(address).map_err(TextError::Engine)?;
                            Ok(Scored::new(
                                symbol_from_document(&schema, &document)?,
                                finite_score(score),
                            ))
                        })
                        .collect::<Result<Vec<_>, TextError>>()?;
                    Ok((hits, exhausted))
                })
                .await
                .unwrap_or_else(|e| Err(TextError::Io(std::io::Error::other(e))))
            }
        },
    )
    .await
}

/// A tantivy score is finite BM25 (or a constant for regex clauses); clamp the
/// pathological case rather than panic deep inside a search.
fn finite_score(raw: f32) -> Score {
    Score::try_new(raw).unwrap_or_else(|_| Score::try_new(0.0).expect("zero is finite"))
}

/// Lower a [`TextQuery`] into the tantivy query tree:
/// `(exact-name OR name-contains OR path-contains OR name-subtokens OR
/// path-subtokens) AND ecosystem? AND kinds?`.
///
/// The first three tiers match on the lowercased raw fields (exact term, then
/// case-insensitive substring). The subtoken tiers add what raw matching cannot:
/// a query for the *parts* of a name (`user` → `getUserById`, `read string` →
/// `read_to_string`) via the identifier-tokenized `name_tokens`/`fq_tokens`
/// fields. All tiers are `Should`, so any one satisfies the name requirement and
/// documents matching more tiers score higher.
///
/// Returns an [`Arc`] so the query can be shared across spawned blocking tasks
/// without cloning (tantivy queries are `Send` but not `Clone`).
pub(crate) fn build_query(
    schema: &TextSchema,
    query: &TextQuery,
) -> Result<std::sync::Arc<dyn Query>, TextError> {
    let needle = query.terms.trim().to_lowercase();
    if needle.is_empty() {
        return Err(TextError::Query(TextQueryError::Empty));
    }

    let contains = |field: Field| -> Result<Box<dyn Query>, TextError> {
        RegexQuery::from_pattern(&format!(".*{}.*", escape_regex(&needle)), field)
            .map(|matched| Box::new(matched) as Box<dyn Query>)
            .map_err(|cause| {
                TextError::Query(TextQueryError::InvalidPattern {
                    needle: needle.clone(),
                    cause,
                })
            })
    };
    let exact = Box::new(TermQuery::new(
        Term::from_field_text(schema.name_lower, &needle),
        IndexRecordOption::Basic,
    )) as Box<dyn Query>;

    // Split the *original-case* query so camelCase humps break before lowercasing,
    // matching how the index tokenized each stored name. A conjunction of the
    // resulting subtokens means every queried part must be present.
    let parts = tokenizer::subtokens(query.terms.trim());
    let subtoken_clause = |field: Field| -> Option<Box<dyn Query>> {
        if parts.is_empty() {
            return None;
        }
        let terms: Vec<(Occur, Box<dyn Query>)> = parts
            .iter()
            .map(|part| {
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(field, part),
                        IndexRecordOption::WithFreqs,
                    )) as Box<dyn Query>,
                )
            })
            .collect();
        Some(Box::new(BooleanQuery::new(terms)))
    };

    let mut name_tiers: Vec<(Occur, Box<dyn Query>)> = vec![
        (Occur::Should, exact),
        (Occur::Should, contains(schema.name_lower)?),
        (Occur::Should, contains(schema.fq_lower)?),
    ];
    if let Some(clause) = subtoken_clause(schema.name_tokens) {
        name_tiers.push((Occur::Should, clause));
    }
    if let Some(clause) = subtoken_clause(schema.fq_tokens) {
        name_tiers.push((Occur::Should, clause));
    }
    let names = BooleanQuery::new(name_tiers);

    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, Box::new(names))];
    if let Some(ecosystem) = query.ecosystem {
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(schema.ecosystem, ecosystem.as_token()),
                IndexRecordOption::Basic,
            )),
        ));
    }
    if !query.kinds.is_empty() {
        let kinds = query
            .kinds
            .iter()
            .map(|kind| {
                (
                    Occur::Should,
                    Box::new(TermQuery::new(
                        Term::from_field_text(schema.kind, &kind_token(*kind)),
                        IndexRecordOption::Basic,
                    )) as Box<dyn Query>,
                )
            })
            .collect::<Vec<_>>();
        clauses.push((Occur::Must, Box::new(BooleanQuery::new(kinds))));
    }
    Ok(std::sync::Arc::new(BooleanQuery::new(clauses)))
}

/// Escape the regex metacharacters of the term dictionary's pattern syntax so
/// user input always matches literally.
fn escape_regex(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for character in raw.chars() {
        if matches!(
            character,
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::text::index::TextIndex;
    use futures::TryStreamExt;
    use heart::{Language, Name, PackageId, Symbol, SymbolId, SymbolKind};
    use std::num::NonZeroUsize;

    fn tmp_dir(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("text-query-test-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("tempdir");
        path
    }

    fn sym(n: u64, plain: &str, fq: &str) -> Symbol {
        Symbol {
            id: SymbolId::from_uuid(uuid::Uuid::from_u128(0xBEEF_0000 + u128::from(n))),
            package: PackageId::from_uuid(uuid::Uuid::from_u128(0xCAFE_0000)),
            ecosystem: Language::Rust,
            name: Name {
                plain: plain.into(),
                fully_qualified: fq.into(),
            },
            kind: SymbolKind::Type,
        }
    }

    /// Q1 regression: `axum::Router` — colons in the raw query must not confuse
    /// the regex tier (escape_regex handles `:` as a literal character).
    #[tokio::test]
    async fn build_query_axum_router_produces_query() {
        let dir = tmp_dir("axum-router");
        let index = TextIndex::open_or_create(&dir).expect("index opens");
        index
            .upsert_batch(&[sym(1, "Router", "axum::Router")])
            .expect("indexed");
        let schema = index.schema();
        let q = TextQuery::new("axum::Router");
        let _built = build_query(schema, &q).expect("Q1: axum::Router must build a query");
        // Regex tier: the contains clause must match through the lowercased fq field.
        let hits: Vec<_> = index
            .search(&q, NonZeroUsize::new(10).unwrap(), None)
            .try_collect()
            .await
            .expect("search");
        assert!(
            !hits.is_empty(),
            "axum::Router should match itself end-to-end"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Q1 regression: `react-query` — hyphens must not be treated as subtraction.
    #[tokio::test]
    async fn build_query_react_query_produces_query() {
        let dir = tmp_dir("react-query");
        let index = TextIndex::open_or_create(&dir).expect("index opens");
        // npm-style: name stored as-is, fq same
        index
            .upsert_batch(&[sym(2, "react-query", "react-query")])
            .expect("indexed");
        let schema = index.schema();
        let q = TextQuery::new("react-query");
        let _built = build_query(schema, &q).expect("Q1: react-query must build a query");
        let hits: Vec<_> = index
            .search(&q, NonZeroUsize::new(10).unwrap(), None)
            .try_collect()
            .await
            .expect("search");
        assert!(
            !hits.is_empty(),
            "react-query should match itself end-to-end"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Q1 regression: `Option<T>` — angle brackets must not break the regex engine.
    #[tokio::test]
    async fn build_query_option_t_produces_query() {
        let dir = tmp_dir("option-t");
        let index = TextIndex::open_or_create(&dir).expect("index opens");
        index
            .upsert_batch(&[sym(3, "Option", "core::option::Option<T>")])
            .expect("indexed");
        let schema = index.schema();
        let q = TextQuery::new("Option<T>");
        let _built = build_query(schema, &q).expect("Q1: Option<T> must build a query");
        // The exact-name / subtoken tiers should still find the symbol.
        let hits: Vec<_> = index
            .search(
                &TextQuery::new("Option"),
                NonZeroUsize::new(10).unwrap(),
                None,
            )
            .try_collect()
            .await
            .expect("search");
        assert!(
            !hits.is_empty(),
            "Option subtoken should match the stored symbol"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
