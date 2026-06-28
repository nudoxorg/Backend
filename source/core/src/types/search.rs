use serde::{Deserialize, Serialize};
use crate::{BlobRef, GlobalSymbolId, OccurrenceId, RepoId};
use super::primitives::{Language, SymbolKind};
use super::pipeline::BlobInfo;

/// A single hit returned from the full-text search index.
#[derive(Debug, Clone)]
pub struct SearchHit {
	/// Reference to the indexed blob.
	pub blob_ref:      BlobRef,
	/// Occurrence identifier for the matching record.
	pub occurrence_id: OccurrenceId,
	/// Fully-qualified symbol name of the matching record.
	pub symbol_name:   String,
	/// Relevance score (higher is more relevant; scale is index-dependent).
	pub score:         f32,
}

/// A single hit returned from the vector similarity index.
#[derive(Debug, Clone)]
pub struct VectorHit {
	/// Reference to the indexed blob.
	pub blob_ref:  BlobRef,
	/// Global symbol identifier associated with this vector point.
	pub global_id: GlobalSymbolId,
	/// Cosine similarity score in `[-1.0, 1.0]` (higher is more similar).
	pub score:     f32,
}

// ── Symbol search types ────────────────────────────────────────────────────

/// The form of a body (implementation) query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BodyQuery {
	/// Free-text description of what the symbol does; will be embedded and
	/// matched by vector search.
	NaturalLanguage(String),
	/// A raw code snippet; will be parsed, embedded, and matched by vector
	/// search.
	CodeSnippet(String),
}

/// Restricts search results to a specific language and/or repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeFilter {
	/// If set, only return results in this language.
	pub lang:    Option<Language>,
	/// If set, only return results from this repository.
	pub repo_id: Option<RepoId>,
}

/// Filter results by the number of indexed occurrences of the resolved global
/// symbol.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OccurrenceFilter {
	/// Minimum number of occurrences (inclusive).
	pub min_count: Option<usize>,
	/// Maximum number of occurrences (inclusive).
	pub max_count: Option<usize>,
}

/// How to combine name-pattern and body-query results when both are specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CombineMode {
	/// Union: return anything matching either criterion (default).
	#[default]
	Or,
	/// Intersection: only return results satisfying both criteria.
	And,
}

/// A compound query for symbol search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolQuery {
	/// Optional name pattern (substring match; case-insensitive).
	pub name_pattern:      Option<String>,
	/// Optional semantic body query (natural language or code snippet).
	pub body_query:        Option<BodyQuery>,
	/// Optional scope filter restricting by language or repository.
	pub scope:             Option<ScopeFilter>,
	/// Optional kind filter restricting the symbol type.
	pub kind:              Option<SymbolKind>,
	/// Optional filter on the number of known occurrences.
	pub occurrence_filter: Option<OccurrenceFilter>,
	/// How to combine `name_pattern` and `body_query` results.
	#[serde(default)]
	pub combine:           CombineMode,
	/// Maximum number of results to return.
	#[serde(default = "default_limit")]
	pub limit:             usize,
}

fn default_limit() -> usize { 20 }

impl Default for SymbolQuery {
	fn default() -> Self {
		Self {
			name_pattern:      None,
			body_query:        None,
			scope:             None,
			kind:              None,
			occurrence_filter: None,
			combine:           CombineMode::Or,
			limit:             20,
		}
	}
}

/// A single result returned by symbol search.
#[derive(Debug, Clone)]
pub struct SymbolMatch {
	/// Full blob information for the matched symbol occurrence.
	pub blob:        BlobInfo,
	/// Relevance score (higher is more relevant; scale depends on the search path
	/// taken).
	pub score:       f32,
	/// All known occurrence identifiers for the resolved global symbol.
	/// Empty if the symbol has not been resolved or no global-symbol query is
	/// available.
	pub occurrences: Vec<OccurrenceId>,
}
