//! The read-only search surfaces and the traits over them.
//!
//! Everything here is read-plane. A [`SearchTarget`] is any store answering a
//! [`Search`] with a *stream* of [`Scored`] hits (never a materialized `Vec`);
//! a [`SymbolStore`] additionally walks graph relationships. Streams are
//! keyset-paginated via the query's [`query::Page`] cursor.
//!
//! Precise (literal) symbol search rides the replica-local
//! [`crate::runtime::text::TextIndex`] tantivy index — kept caught up by
//! [`crate::server::poll::text_index_poller`] off the catalog's Text-sink
//! outbox. Gated semantic search (qdrant) is the other live symbol surface;
//! identity lookup answers from a catalog `symbols_proj` read.

pub mod planner;
pub mod registry;
pub mod routing;
pub mod semantic;

use std::collections::HashSet;
use std::num::NonZeroUsize;

use crate::ecosystem::FilterExt as _;
use crate::runtime::text::{TextCursorKey, TextQuery};
use crate::server::registry::vector::EmbeddingModel;
use futures::{Stream, StreamExt};
use heart::{Scored, StoreError, Symbol, SymbolId};

pub use heart::client::query::{
    AbstractQuery, ExecutionQuery as Query, Filter, LiteralQuery, PackageSelector, Search,
    SymbolCursor, SymbolCursorKey,
};
pub use planner::SearchPlanner;

use crate::server::SourceStores;
use crate::server::error::{BadRequestReason, ServerError};

/// A store/target that answers searches with a stream of scored results.
pub trait SearchTarget {
    /// What this target yields.
    type Item;

    /// Per-implementor failure mode.
    type Error: StoreError;

    /// Run a search, streaming scored results (keyset-paginated via the request's
    /// `page`). The stream is the contract — results are never collected here.
    async fn search(
        &self,
        request: &Search<'_>,
    ) -> Result<impl Stream<Item = Result<Scored<Self::Item>, Self::Error>> + Send, Self::Error>;

    /// Fetch a single item by its durable global id.
    async fn get_by_id(&self, id: SymbolId) -> Result<Option<Self::Item>, Self::Error>;
}

/// A store of symbols supporting both precise and (gated) semantic search, plus
/// graph-relationship expansion. This is what the server's symbol-search surface
/// is generic over.
pub trait SymbolStore: SearchTarget<Item = Symbol> {
    /// Given a hit, walk its graph relationships and score the related symbols —
    /// the "expand from here" operation that powers session-based exploration.
    async fn related_hits(
        &self,
        hit: &Scored<Symbol>,
    ) -> Result<Vec<Scored<Symbol>>, <Self as SearchTarget>::Error>;
}

/// Fold per-source result groups — supplied in federation **precedence order**
/// (overlays first, then the definitive base) — into one ranked page. The first
/// source to claim a key wins (overlay-override), then everything ranks by
/// score, bounded by `limit`.
pub fn merge_overlay_first<T, K>(
    groups: impl IntoIterator<Item = Vec<Scored<T>>>,
    key: impl Fn(&T) -> K,
    limit: usize,
) -> Vec<Scored<T>>
where
    K: Eq + std::hash::Hash,
{
    let mut claimed = HashSet::new();
    let mut merged = Vec::new();
    for group in groups {
        for hit in group {
            if claimed.insert(key(&hit.value)) {
                merged.push(hit);
            }
        }
    }
    merged.sort_by_key(|left| std::cmp::Reverse(left.score));
    merged.truncate(limit);
    merged
}

impl<M: EmbeddingModel> SourceStores<M> {
    /// Look one symbol up by its durable id within this source.
    ///
    /// Symbol identity used to be answered by the replica-local tantivy text
    /// index. That plane is removed; catalog `symbols_proj` lookup
    /// (`GlobalStore::symbol_by_id`) is the replacement — the only source of
    /// symbol rows now is `coordination::compile_inprocess` /
    /// `ingest_ir_bytes`'s catalog writes, read back through the same
    /// `symbols_proj` projection `symbols_for` (the vector outbox consumer)
    /// already reads.
    pub async fn symbol_by_id(&self, id: SymbolId) -> Result<Option<Symbol>, ServerError> {
        self.global_store.symbol_by_id(id).await.map_err(|error| {
            ServerError::Registry(crate::server::registry::RegistryError::from(error))
        })
    }
}

// ── One federated source is itself a full symbol store: precise (tantivy)
// search, identity lookup, and graph walking. The server's federation logic
// composes these per-source stores overlay-first so the precise, semantic,
// and expand paths keep a uniform shape.

/// Lift a [`crate::runtime::error::TextError`] into the server's error
/// vocabulary via [`crate::runtime::error::RuntimeError`] (`ServerError::Runtime`).
fn text_error(error: crate::runtime::error::TextError) -> ServerError {
    ServerError::Runtime(error.into())
}

impl<M: EmbeddingModel> SearchTarget for SourceStores<M> {
    type Item = Symbol;
    type Error = ServerError;

    async fn search(
        &self,
        request: &Search<'_>,
    ) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send, ServerError> {
        // The planner (`SearchPlanner::plan`) routes both literal queries and
        // quota-degraded abstract queries here; a genuinely gated abstract
        // query never reaches this arm. Either way `query.text()` is the
        // right search string.
        let terms = request.query.text();
        if terms.trim().is_empty() {
            return Ok(futures::stream::empty().boxed());
        }
        let text_query = TextQuery::new(terms);

        let limit = NonZeroUsize::new(request.page.limit as usize).unwrap_or(NonZeroUsize::MIN);
        let after = match request.page.cursor.as_deref() {
            Some(token) => {
                // The text index mints `Enforced` cursors: freshness is
                // re-checked against the live snapshot at decode, not merely
                // carried as a hint (see `heart::Cursor::<_, Enforced>`).
                // `TextIndex::snapshot` is synchronous (tantivy reload), so it
                // runs on the blocking pool like every other tantivy call.
                let text = std::sync::Arc::clone(&self.text);
                let live = tokio::task::spawn_blocking(move || text.snapshot())
                    .await
                    .unwrap_or_else(|join| {
                        Err(crate::runtime::error::TextError::Io(std::io::Error::other(
                            join,
                        )))
                    })
                    .map_err(text_error)?;
                let decoded = heart::Cursor::<TextCursorKey, heart::Enforced>::decode(token, live)
                    .map_err(|source| BadRequestReason::InvalidCursor {
                        token: token.to_owned(),
                        source,
                    })?;
                Some(decoded)
            }
            None => None,
        };

        // Post-filter (ecosystem/package selectors), matching the semantic
        // path's `request.filter.admits(&symbol)` — the same over-fetch/limit
        // tradeoff applies here as there: a filter can shrink a full page.
        //
        // Drained eagerly (not left lazy) rather than adapted in place: under
        // Rust 2024's `impl Trait` capture rules, `TextIndex::search`'s
        // returned stream is treated as borrowing its `&TextQuery` argument
        // for as long as the stream lives, so a lazily-adapted stream cannot
        // outlive this call's local `text_query`. Every real caller collects
        // this trait's stream into a `Vec` immediately anyway
        // (`precise_hits`/`semantic_hits`), so draining here costs nothing
        // observable.
        let filter = request.filter.clone();
        let stream = self.text.search(&text_query, limit, after);
        futures::pin_mut!(stream);
        let mut hits = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(scored) if filter.admits(&scored.value) => hits.push(Ok(scored)),
                Ok(_) => {}
                Err(error) => hits.push(Err(text_error(error))),
            }
        }
        Ok(futures::stream::iter(hits).boxed())
    }

    async fn get_by_id(&self, id: SymbolId) -> Result<Option<Symbol>, ServerError> {
        self.symbol_by_id(id).await
    }
}

impl<M: EmbeddingModel> SymbolStore for SourceStores<M> {
    async fn related_hits(
        &self,
        _hit: &Scored<Symbol>,
    ) -> Result<Vec<Scored<Symbol>>, ServerError> {
        // Symbol-graph expansion moved off the (removed) Terminus store onto the
        // IR reverse-position index + `Target::Usages` plane (INDEX-PLAN §5.5).
        // In-process IR is not materialized yet (IP-7), so a hit's neighbourhood
        // is honestly empty here rather than served from a symbol-document graph.
        Ok(Vec::new())
    }
}
