//! Registry search — searching the *registry itself* (finding packages), as
//! distinct from symbol/code search (which lives in the serving plane).
//!
//! Postgres is the source of truth; a **replica-local** tantivy index is the
//! query abstraction layered over it, kept in sync by polling postgres from a
//! watermark (see [`tantivy`]). Results are [`heart::Scored`] packages,
//! keyset-paginated via [`heart::Cursor`], and access-filtered — every method
//! takes an [`AccessContext`].

use futures::Stream;
use heart::{
	PackageId, Scored,
	access::AccessContext,
	cursor::Cursor,
	ecosystem::Language,
	search::Page,
};

use crate::{GlobalPackage, error::SearchError};

pub mod multi_parent;
pub mod tantivy;

/// The keyset a registry-search cursor advances over: a relevance score paired
/// with the package id as the tiebreak, so pagination is stable across an
/// eventually-consistent replica.
pub type SearchKey = (heart::Score, PackageId);

/// A single query against the registry search surface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegistryQuery {
	/// The free-text query (package name / description / keywords).
	pub text: String,

	/// Restrict to a single ecosystem, or search across all when `None`.
	pub ecosystem: Option<Language>,

	/// Page size.
	pub limit: usize,

	/// The keyset cursor to resume after, or `None` for the first page.
	pub after: Option<Cursor<SearchKey>>,
}

/// The registry (package) search entry point the server's admin/read surface
/// calls. Wraps the replica-local tantivy index + the multi-parent de-dupe.
pub struct RegistrySearch {
	index: tantivy::PackageIndex,
}

impl RegistrySearch {
	/// Build over a replica-local package index.
	pub fn new(index: tantivy::PackageIndex) -> Self { Self { index } }

	/// Stream scored packages matching `query`, access-filtered and de-duped
	/// across multi-parent reachability.
	///
	/// Returns an `impl Stream` (native RPITIT, no `async_trait`) so a caller can
	/// consume results lazily and stop early. Each item carries the [`Scored`]
	/// package or a per-item error.
	pub fn search<'a>(
		&'a self,
		ctx: &'a AccessContext,
		query: &'a RegistryQuery,
	) -> impl Stream<Item = Result<Scored<GlobalPackage>, SearchError>> + 'a {
		let _ = (&self.index, ctx, query);
		// A concrete stream (async_stream over tantivy segment readers) fills in
		// here. A typed empty stream stands in so the signature is final.
		// TODO: stream scored, access-filtered, multi-parent-deduped packages.
		futures::stream::empty::<Result<Scored<GlobalPackage>, SearchError>>()
	}

	/// Fetch one page (materialized) plus the cursor for the next page, for
	/// callers that page rather than stream.
	pub async fn page(
		&self,
		ctx: &AccessContext,
		query: &RegistryQuery,
	) -> Result<Page<GlobalPackage>, SearchError> {
		let _ = (&self.index, ctx, query);
		todo!("run the query, access-filter, de-dupe multi-parent, build the next cursor")
	}
}
