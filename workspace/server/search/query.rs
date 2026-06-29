//! The query, filter, and match types that describe a search request and its
//! results.

use std::num::NonZeroUsize;

use nonempty::NonEmpty;

use heart::{Language, SymbolKind as Kind};
use registry::Package;

/// An abstract query, fed to qdrant for responses based on semantic similarity rather than some ground response
pub enum AbstractQuery{
	/// A query like "error types" or "stuff in axum" that isn't structured in any kind of particular way
	NaturalLanguage(String),

	/// An (assumed to be) code snippet for an assumed language (little we can do to verify either)
	CodeSnippet(String)
}

pub enum Query {
	Abstract(AbstractQuery),
	Literal(String), // TODO: Replace with the native tantivy search type
}

pub struct Search {
	filter: Filter,
	query: Query
	// TODO: Find out the best way to support things like AND and OR (various ways to chain?)
}

/// The scope for which a search takes place
// Should maybe be a generic so we don't make people use vec![] for single language queries (also should be non-empty)
// Also this is something that would be a parameter on a search, as an optional (with a default implementation) to avoid a ton of optionals on fields
pub struct Filter {
	/// The language(s) we should search under
	pub language: Option<NonEmpty<Language>>,

	/// The package(s) we should search under
	pub package: Option<NonEmpty<Package>>,

	/// The range of packages we want to see
	/// You would use this to bound the number of results
	pub page: Pagination

	// Establish a curtain of popularity, only showing results that... i don't think this is that useful TODO: remove?
	// occurences: Occurences
}

struct Pagination {
    pub limit: NonZeroUsize,
    pub offset: usize,
}

/// We pulled a match !
/// Includes all of the possible useful information about the said match
/// Mainly used temporarily when displaying results
// Because we're only showing this for a moment, things like symbol origination are dumb (inferrable by context?) and same for metadata and embeddings and lifecycle(bruh)
pub struct Match {
	// TODO: Change to the actual type for symbols (which should include source)
	/// The fully qualified name of the symbol
	pub symbol_name: String,

	/// The kind of thing this is
	pub kind: Kind,
}
