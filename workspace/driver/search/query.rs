//! The query, filter, and match types that describe a search request and its results.

use index::ecosystem::PackageNameExt as _;
use heart::{Cursor, Language, PackageVersion, PageSpecification, Score};
use nonempty::NonEmpty;
use crate::registry::package::PackageName;
use serde::{Deserialize, Serialize};

pub use crate::error::QueryError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AbstractQuery {
	NaturalLanguage(String),
	CodeSnippet { ecosystem: Option<Language>, code: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiteralQuery(String);

impl LiteralQuery {
	/// The longest literal query accepted; anything larger is a paste accident,
	/// not a name lookup.
	const MAXIMUM_LENGTH: usize = 1024;

	pub fn parse(raw: &str) -> Result<Self, QueryError> {
		let trimmed = raw.trim();
		if raw.trim().is_empty() {
			// Distinguish "provided nothing" from "only whitespace".
			if raw.is_empty() {
				return Err(QueryError::Empty);
			} else {
				return Err(QueryError::EmptyAfterTrim { raw: raw.to_owned() });
			}
		}
		let len = trimmed.len();
		if len > Self::MAXIMUM_LENGTH {
			return Err(QueryError::TooLong {
				len,
				max: Self::MAXIMUM_LENGTH,
				snippet: trimmed.chars().take(64).collect(),
			});
		}
		if let Some((position, _ch)) = trimmed.char_indices().find(|(_, c)| c.is_control()) {
			return Err(QueryError::ControlCharacter {
				position,
				snippet: trimmed.chars().skip(position.saturating_sub(8)).take(32).collect(),
			});
		}
		// Raw string, no tantivy-grammar escaping — the search path uses a
		// hand-built BooleanQuery tree that never passes text to QueryParser.
		Ok(Self(trimmed.to_owned()))
	}

	pub fn as_str(&self) -> &str { &self.0 }
}

impl AbstractQuery {
	/// The raw text to embed (or to degrade into a literal search when the
	/// semantic path is unavailable).
	pub fn text(&self) -> &str {
		match self {
			AbstractQuery::NaturalLanguage(text) => text,
			AbstractQuery::CodeSnippet { code, .. } => code,
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Query {
	Abstract(AbstractQuery),
	Literal(LiteralQuery),
}

impl Query {
	/// The query's raw text, whichever surface it targets.
	pub fn text(&self) -> &str {
		match self {
			Query::Abstract(query) => query.text(),
			Query::Literal(query) => query.as_str(),
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VersionConstraint {
	Exact(PackageVersion),
	SemverRange(semver::VersionReq),
	Pep440(uv_pep440::VersionSpecifiers),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSelector {
	pub name: PackageName,
	pub version: Option<VersionConstraint>,
}

impl PackageSelector {
	/// Whether a symbol plausibly belongs to the selected package.
	///
	/// Uses `StructuredName::symbol_roots()` so Go module paths, dotted Java
	/// groups, and npm scoped names all match correctly (Q2).
	pub fn matches(&self, symbol: &heart::Symbol) -> bool {
		symbol.ecosystem == self.name.ecosystem()
			&& self.name.structured().symbol_roots().iter().any(|root| {
				let fq = symbol.name.fully_qualified.as_str();
				fq == root
					|| fq
						.strip_prefix(root.as_str())
						.is_some_and(|rest| rest.starts_with([':', '.', '/']))
			})
	}
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Filter {
	pub ecosystems: Option<NonEmpty<Language>>,
	pub packages: Option<NonEmpty<PackageSelector>>,
}

impl Filter {
	/// Whether a symbol survives this filter. `None` dimensions are unbounded.
	pub fn admits(&self, symbol: &heart::Symbol) -> bool {
		self.ecosystems
			.as_ref()
			.is_none_or(|ecosystems| ecosystems.iter().any(|ecosystem| *ecosystem == symbol.ecosystem))
			&& self
				.packages
				.as_ref()
				.is_none_or(|packages| packages.iter().any(|selector| selector.matches(symbol)))
	}
}

/// A complete search request: what to find, how to narrow it.
pub struct Search<'a> {
	pub query: Query,
	pub filter: Filter,
	/// Keyset-pagination spec: how many results to return and where to resume from.
	pub page: PageSpecification,
	// scope field removed — AccessContext was deleted from heart
	pub _lifetime: std::marker::PhantomData<&'a ()>,
}

pub type SymbolCursorKey = (Score, heart::SymbolId);
pub type SymbolCursor = Cursor<SymbolCursorKey>;

#[cfg(test)]
mod tests {
	use super::*;

	/// Q1 regression: qualified Rust paths must parse unescaped (no backslash injection).
	#[test]
	fn literal_query_axum_router_parses_unescaped() {
		let q = LiteralQuery::parse("axum::Router").expect("should parse");
		assert_eq!(q.as_str(), "axum::Router");
	}

	/// Q1 regression: hyphenated npm-style names must parse unescaped.
	#[test]
	fn literal_query_react_query_parses_unescaped() {
		let q = LiteralQuery::parse("react-query").expect("should parse");
		assert_eq!(q.as_str(), "react-query");
	}

	/// Q1 regression: generic type syntax must parse unescaped.
	#[test]
	fn literal_query_option_t_parses_unescaped() {
		let q = LiteralQuery::parse("Option<T>").expect("should parse");
		assert_eq!(q.as_str(), "Option<T>");
	}

	#[test]
	fn literal_query_empty_is_rejected() {
		assert!(matches!(LiteralQuery::parse(""), Err(QueryError::Empty)));
	}

	#[test]
	fn literal_query_whitespace_only_is_rejected() {
		assert!(matches!(LiteralQuery::parse("   "), Err(QueryError::EmptyAfterTrim { .. })));
	}

	#[test]
	fn literal_query_too_long_is_rejected() {
		let long = "x".repeat(LiteralQuery::MAXIMUM_LENGTH + 1);
		assert!(matches!(LiteralQuery::parse(&long), Err(QueryError::TooLong { .. })));
	}

	#[test]
	fn literal_query_control_char_is_rejected() {
		assert!(matches!(LiteralQuery::parse("foo\x01bar"), Err(QueryError::ControlCharacter { .. })));
	}
}
