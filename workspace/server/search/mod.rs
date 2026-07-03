//! The read-only search surfaces and the traits over them.
//!
//! Everything here is read-plane. A [`SearchTarget`] is any store answering a
//! [`Search`] with a *stream* of [`Scored`] hits (never a materialized `Vec`);
//! a [`SymbolStore`] additionally walks graph relationships. Streams are
//! keyset-paginated via the query's [`query::Page`] cursor.

pub mod planner;
pub mod query;
pub mod registry;
pub mod semantic;
pub mod symbolic;

use futures::Stream;
use heart::{SymbolId, Scored, StoreError, Symbol};
use runtime::graph::GraphStore;

pub use planner::SearchPlanner;
pub use query::{AbstractQuery, Filter, Pagination, Query, Search, SymbolCursor};

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
pub trait SymbolStore: SearchTarget<Item = Symbol> + GraphStore {
	/// Given a hit, walk its graph relationships and score the related symbols —
	/// the "expand from here" operation that powers session-based exploration.
	async fn related_hits(
		&self,
		hit: &Scored<Symbol>,
	) -> Result<Vec<Scored<Symbol>>, <Self as SearchTarget>::Error>;
}
