//! The query, filter, and match types that describe a search request and its
//! results.
//!
//! Typing fixes over the original: no raw `Language` (uses [`Language`]); no
//! raw tantivy string (a validated [`LiteralQuery`] with an escape policy);
//! package scope is a [`PackageSelector`] with an *optional version constraint*
//! so "any version of axum" is expressible; pagination is opaque-cursor, not
//! offset; and every search carries an [`AccessContext`].

use std::num::NonZeroU32;

use heart::{AccessContext, Cursor, Language, PackageName, PackageVersion, Score};
use nonempty::NonEmpty;
use serde::{Deserialize, Serialize};

/// A semantic query, embedded and matched by similarity rather than exact terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AbstractQuery {
	/// Free-form natural language ("error types", "an http router").
	NaturalLanguage(String),

	/// A code snippet, with the ecosystem it is assumed to be written in (used
	/// to shape the embedding text). `None` means "unknown / let the embedder
	/// decide".
	CodeSnippet { ecosystem: Option<Language>, code: String },
}

/// A precise, term-based query against the tantivy index. Constructed through
/// [`LiteralQuery::parse`] so raw user input can never inject tantivy query
/// syntax — special characters are escaped per an explicit policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiteralQuery(String);

impl LiteralQuery {
	/// Validate and escape raw user input into a safe literal query.
	pub fn parse(raw: &str) -> Result<Self, QueryError> {
		let _ = raw;
		todo!("reject empty, escape tantivy special chars per policy")
	}

	/// The escaped query string.
	pub fn as_str(&self) -> &str { &self.0 }
}

/// A search query: either precise or semantic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Query {
	/// Semantic (gated) query.
	Abstract(AbstractQuery),
	/// Precise (default) query.
	Literal(LiteralQuery),
}

/// A version constraint on a package scope — exact or a range, per ecosystem
/// grammar. Both range grammars are *typed* (no stringly-typed specifier), so an
/// unparseable constraint is rejected at the boundary, not at query time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VersionConstraint {
	/// Exactly this version.
	Exact(PackageVersion),
	/// A SemVer range (crates/npm).
	SemverRange(semver::VersionReq),
	/// A PEP 440 specifier set (Python), e.g. `>=1.2,<2`.
	Pep440(uv_pep440::VersionSpecifiers),
}

/// A package to scope a search to: a name plus an optional version constraint
/// (absent = any version).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSelector {
	/// The (normalized) package name.
	pub name: PackageName,
	/// The version constraint, or `None` for any version.
	pub version: Option<VersionConstraint>,
}

/// The optional scope narrowing a search. Absent fields mean "no restriction".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Filter {
	/// Restrict to these ecosystems.
	pub ecosystems: Option<NonEmpty<Language>>,
	/// Restrict to these packages.
	pub packages: Option<NonEmpty<PackageSelector>>,
}

/// Opaque-cursor pagination *request*. Keyset-based, never offset. (The response
/// side is [`heart::Page`], whose items are `heart::Scored<T>`; this is only the
/// request knob, hence the distinct name.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
	/// Maximum results in this page.
	pub limit: NonZeroU32,
	/// The opaque continuation cursor from a prior page, if any.
	pub after: Option<String>,
}

/// A complete search request: what to find, how to narrow it, who is asking.
pub struct Search<'a> {
	/// The query.
	pub query: Query,
	/// The optional scope filter.
	pub filter: Filter,
	/// The page request.
	pub page: Pagination,
	/// The authenticated context this search runs under.
	pub scope: &'a AccessContext,
}

// A "search hit" is deliberately not its own struct: it is `heart::Scored<Symbol>`
// (and the wire form is `heart::Page<Symbol>`), so there is exactly one hit shape
// across the search surfaces, the HTTP layer, and the graph layer.

/// Typed keyset key for symbol-search pagination: score then id, so ties order
/// deterministically.
pub type SymbolCursorKey = (Score, heart::SymbolId);

/// A concrete symbol-search cursor.
pub type SymbolCursor = Cursor<SymbolCursorKey>;

/// Why a query could not be parsed/validated.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
	/// The query text was empty.
	#[error("empty query")]
	Empty,
	/// The literal query could not be escaped/validated.
	#[error("invalid literal query: {0}")]
	Invalid(String),
}
