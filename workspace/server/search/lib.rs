//! This module is responsible for providing all of the search operations on the
//! backing databases, read-only, if you're looking to create/edit/write on the
//! databases themselves, you should go to their relevant top-level workspace
//! module

pub mod query;
pub mod symbolic;

use heart::Id;
use runtime::graph::GraphStore;
use runtime::vector::Scored;

use crate::store::StoreError;

pub use query::{AbstractQuery, Filter, Match, Query, Search};

/// A trait for stores/targets of search to support both abstract and literal search queries.
pub trait SearchTarget {
	/// What this target yields.
    type Item;

    /// Per-implementor failure mode.
    type Error: StoreError;

	/// Make a search, returning a stream of results
	async fn search(&self, request: &Search, scope: Option<Filter>) -> Result<impl Stream<Item = Result<Scored<Self::Item>, Self::Error>>, Self::Error>;

	/// Find an item by an id
	async fn get_by_id(&self, id: Guid) -> Result<Option<Self::Item>, Self::Error>;

	// Again I don't believe in listing
}

//! Not doing any kind of embedder trait, because again, it's just keyed to something specific, we're not going to have more of these unfort

/// A store of symbols with both semantic and precise search
pub trait SymbolStore: SearchTarget<Item = Symbol> + GraphStore {
    /// Given a hit from search, walk its relationships and score them.
    async fn related_hits(
        &self,
        hit: &Scored<Symbol>,
    ) -> Result<Vec<Scored<Symbol>>, <Self as SearchTarget>::Error>;
}
