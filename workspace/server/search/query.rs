//! The query, filter, and match types that describe a search request and its results.

use std::num::NonZeroU32;

use heart::{Cursor, Language, PackageVersion, Score};
use nonempty::NonEmpty;
use registry::package::PackageName;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AbstractQuery {
	NaturalLanguage(String),
	CodeSnippet { ecosystem: Option<Language>, code: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiteralQuery(String);

impl LiteralQuery {
	pub fn parse(raw: &str) -> Result<Self, QueryError> {
		let _ = raw;
		todo!("reject empty, escape tantivy special chars per policy")
	}

	pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Query {
	Abstract(AbstractQuery),
	Literal(LiteralQuery),
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Filter {
	pub ecosystems: Option<NonEmpty<Language>>,
	pub packages: Option<NonEmpty<PackageSelector>>,
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

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
	#[error("empty query")]
	Empty,
	#[error("invalid literal query: {0}")]
	Invalid(String),
}
