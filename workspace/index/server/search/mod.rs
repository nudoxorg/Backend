//! The read-only search surfaces and the traits over them.
//!
//! Everything here is read-plane. A [`SearchTarget`] is any store answering a
//! [`Search`] with a *stream* of [`Scored`] hits (never a materialized `Vec`);
//! a [`SymbolStore`] additionally walks graph relationships. Streams are
//! keyset-paginated via the query's [`query::Page`] cursor.
//!
//! Precise (literal) symbol search used to ride a replica-local tantivy
//! `TextIndex`. That plane is gone: literal queries answer empty, identity
//! lookup is pending a catalog `symbols_proj` read, and gated semantic search
//! (qdrant) remains the live symbol surface.

pub mod planner;
pub mod registry;
pub mod routing;
pub mod semantic;

use std::collections::HashSet;

use crate::server::registry::vector::EmbeddingModel;
use futures::Stream;
use heart::{Scored, StoreError, Symbol, SymbolId};

pub use heart::client::query::{
    AbstractQuery, ExecutionQuery as Query, Filter, LiteralQuery, PackageSelector, Search,
    SymbolCursor, SymbolCursorKey,
};
pub use planner::SearchPlanner;

use crate::server::SourceStores;
use crate::server::error::ServerError;

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
        self.global_store
            .symbol_by_id(id)
            .await
            .map_err(|error| ServerError::Registry(crate::server::registry::RegistryError::from(error)))
    }
}

// ── One federated source is itself a full symbol store: identity lookup and
// graph walking. Literal/precise search no longer has a backend; the server's
// federation logic still composes these per-source stores overlay-first so the
// semantic path and expand keep a uniform shape.

impl<M: EmbeddingModel> SearchTarget for SourceStores<M> {
    type Item = Symbol;
    type Error = ServerError;

    async fn search(
        &self,
        _request: &Search<'_>,
    ) -> Result<impl Stream<Item = Result<Scored<Symbol>, ServerError>> + Send, ServerError> {
        // Precise symbol search (tantivy TextIndex) was removed. Literal queries
        // and degraded abstract queries answer empty here; gated semantic search
        // is planned separately and never reaches this arm.
        Ok(futures::stream::empty())
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
