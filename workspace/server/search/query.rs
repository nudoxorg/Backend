//! The query, filter, and match types that describe a search request and its results.

use std::num::NonZeroU32;

use heart::{Cursor, Language, PackageVersion, Score};
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

	/// The characters the tantivy query grammar treats as operators. Escaping
	/// them (rather than rejecting) means a user can search for `Option<T>` or
	/// `operator+` literally — the whole point of the precise surface.
	const TANTIVY_OPERATORS: &'static [char] =
		&['+', '-', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '\\'];

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
		let mut escaped = String::with_capacity(trimmed.len());
		for character in trimmed.chars() {
			if Self::TANTIVY_OPERATORS.contains(&character) {
				escaped.push('\\');
			}
			escaped.push(character);
		}
		Ok(Self(escaped))
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
	/// Symbols carry a [`heart::PackageId`], which cannot be re-derived from a
	/// bare name (identity needs origin + version), so the match is on the
	/// fully-qualified name's leading segment — the package/crate/module root
	/// every ecosystem's grammar puts first.
	pub fn matches(&self, symbol: &heart::Symbol) -> bool {
		symbol.ecosystem == self.name.ecosystem()
			&& symbol
				.name
				.fully_qualified
				.split([':', '.', '/'])
				.next()
				.is_some_and(|root| root == self.name.canonical())
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
	pub limit: NonZeroU32,
	pub after: Option<String>,
}

/// A complete search request: what to find, how to narrow it.
pub struct Search<'a> {
	pub query: Query,
	pub filter: Filter,
	pub page: Pagination,
	// scope field removed — AccessContext was deleted from heart
	pub _lifetime: std::marker::PhantomData<&'a ()>,
}

pub type SymbolCursorKey = (Score, heart::SymbolId);
pub type SymbolCursor = Cursor<SymbolCursorKey>;
