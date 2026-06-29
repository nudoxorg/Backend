//! The graph runtime store (terminus) for surfacing direct/known relationships
//! between symbols.

pub mod expansion;
pub mod resolution;

use crate::heart::{Id, Symbol};
use crate::store::StoreError;

/// A trait for objects which are or hold graph-based data storing mechanisms, for surfacing direct/known relationships
pub trait GraphStore {
	type Error: StoreError;

	// TODO: Store the kind of relation two items have? Like is this a member?
	// TODO: Make all streaming

	/// Return all of the symbols which hold the input within their declaration
	async fn get_occurrences(&self, item: Id<Symbol>) -> Result<Vec<Id<Symbol>>, Self::Error>;

    /// What points to this?
    async fn get_references(&self, item: Id<Symbol>) -> Result<Vec<Id<Symbol>>, Self::Error>;

    /// Determine if two items are related.
    async fn are_related(&self, from: Id<Symbol>, to: Id<Symbol>) -> Result<bool, Self::Error>;
}

/// Our graph database of choice (terminus)
pub struct Graph {

}
