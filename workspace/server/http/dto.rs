//! Request/response DTOs — the serialized shapes the API speaks, kept separate
//! from the internal domain types they project from.
//!
//! Pagination is always by **opaque cursor token** (never offset): a page
//! response carries `next` only if more results exist, and the client echoes it
//! back verbatim. This keeps paging correct over an eventually-consistent index
//! (see [`heart::Cursor`]).

use heart::{Ecosystem, GlobalSymbolId, PackageId, Score, SymbolKind};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// A search request as it arrives on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequestDto {
	/// The raw query text.
	pub query: String,
	/// Whether to treat the query as a natural-language / semantic query. The
	/// semantic path is still gated server-side; this is only a request.
	#[serde(default)]
	pub semantic: bool,
	/// Optional ecosystem scope.
	#[serde(default)]
	pub ecosystems: Vec<Ecosystem>,
	/// Optional package-name scope (any version), by canonical name.
	#[serde(default)]
	pub packages: Vec<String>,
	/// Page size.
	pub limit: std::num::NonZeroU32,
	/// Opaque cursor from a previous page, if continuing.
	#[serde(default)]
	pub cursor: Option<String>,
}

/// One search hit, flattened for display. Deliberately minimal — richer symbol
/// data is fetched by id on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchDto {
	/// The symbol's durable global id (stable across queries and stores).
	pub id: GlobalSymbolId,
	/// The package the symbol belongs to.
	pub package: PackageId,
	/// The bare symbol name.
	pub name: SmolStr,
	/// The fully-qualified name.
	pub fq_name: SmolStr,
	/// What kind of symbol it is.
	pub kind: SymbolKind,
	/// Its relevance score.
	pub score: Score,
}

/// A single page of results plus the opaque continuation token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
	/// The results on this page.
	pub items: Vec<T>,
	/// The opaque cursor to fetch the next page, or `None` if exhausted.
	pub next: Option<String>,
}

/// A request to add/index a package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddPackageDto {
	/// The ecosystem the package lives in.
	pub ecosystem: Ecosystem,
	/// The package name as published.
	pub name: String,
	/// The version requested (concrete or a range to resolve).
	pub version: String,
	/// The origin registry, if not the ecosystem default.
	#[serde(default)]
	pub origin: Option<String>,
}

/// The response to an add/index request: the resolved id and current lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddPackageResponseDto {
	/// The deterministic id the package resolved to (idempotent: a duplicate add
	/// returns the existing id).
	pub package: PackageId,
	/// The package's current lifecycle state, serialized.
	pub state: heart::ResolutionState,
}

/// The health/readiness projection returned by the health endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthDto {
	/// Overall readiness.
	pub ready: bool,
	/// Backends currently degraded/unreachable, if any.
	pub degraded: Vec<heart::BackendKind>,
}
