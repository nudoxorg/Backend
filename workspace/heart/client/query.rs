//! The transport-free search request vocabulary that belongs to `heart`:
//! query shapes, filter narrowing, version constraints, and the cursor aliases.
//!
//! These types describe *what* the caller wants — no transport, no index grammar,
//! no ecosystem-specific matching. The matching logic (`PackageSelector::matches`,
//! `Filter::admits`) lives in `index::ecosystem` as extension traits because it
//! needs the per-ecosystem `StructuredName` grammar.

use crate::cursor::Cursor;
use crate::package::PackageName;
use crate::query::PageSpecification;
use crate::score::Score;
use crate::{Language, PackageVersion, SymbolId};
use serde::{Deserialize, Serialize};

/// Validation errors produced when parsing or checking a search query's raw text.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The query string was completely empty (no characters at all).
    #[error("empty query")]
    Empty,

    /// The query contained only whitespace; nothing left after trim.
    #[error("query empty after trimming whitespace")]
    EmptyAfterTrim { raw: String },

    /// Query text exceeded the hard safety limit for literal paste queries.
    #[error("query length {len} exceeds maximum of {max}")]
    TooLong {
        len: usize,
        max: usize,
        /// First ~64 chars of the offending query for diagnostics.
        snippet: String,
    },

    /// Query text contained a control character (NUL, DEL, etc.).
    #[error("query contains control character")]
    ControlCharacter {
        position: usize,
        /// Surrounding snippet around the bad char.
        snippet: String,
    },

    /// An operator character was used in a context the literal surface does not
    /// support (future use by a full query parser surface).
    #[error("invalid operator '{op}' at position {position}")]
    InvalidOperator {
        query: String,
        position: usize,
        op: char,
    },

    /// Parentheses (or other grouping) were unbalanced.
    #[error("unbalanced parentheses in query")]
    UnbalancedParens { query: String, position: usize },

    /// A field name in a structured query clause is not known to this index.
    #[error("unknown field '{field}'")]
    UnknownField {
        query: String,
        field: String,
        position: Option<usize>,
    },

    /// A literal value supplied for a field had the wrong type for the field's
    /// schema (e.g. number where string expected).
    #[error("type mismatch for field '{field}': expected {expected}")]
    TypeMismatch {
        query: String,
        field: String,
        expected: &'static str,
        actual: String,
        position: Option<usize>,
    },

    /// Other query malformation not covered by the explicit cases above.
    /// Prefer adding a variant rather than widening this.
    #[error("malformed query: {detail}")]
    Malformed { detail: String, query: String },
}

pub use self::Error as QueryError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AbstractQuery {
    NaturalLanguage(String),
    CodeSnippet {
        ecosystem: Option<Language>,
        code: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiteralQuery(String);

impl LiteralQuery {
    /// The longest literal query accepted; anything larger is a paste accident,
    /// not a name lookup.
    const MAXIMUM_LENGTH: usize = 1024;

    pub fn parse(raw: &str) -> Result<Self, Error> {
        let trimmed = raw.trim();
        if raw.trim().is_empty() {
            // Distinguish "provided nothing" from "only whitespace".
            if raw.is_empty() {
                return Err(Error::Empty);
            } else {
                return Err(Error::EmptyAfterTrim {
                    raw: raw.to_owned(),
                });
            }
        }
        let len = trimmed.len();
        if len > Self::MAXIMUM_LENGTH {
            return Err(Error::TooLong {
                len,
                max: Self::MAXIMUM_LENGTH,
                snippet: trimmed.chars().take(64).collect(),
            });
        }
        if let Some((position, _ch)) = trimmed.char_indices().find(|(_, c)| c.is_control()) {
            return Err(Error::ControlCharacter {
                position,
                snippet: trimmed
                    .chars()
                    .skip(position.saturating_sub(8))
                    .take(32)
                    .collect(),
            });
        }
        // Raw string kept as-is — no query-grammar escaping on this path.
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
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
pub enum ExecutionQuery {
    Abstract(AbstractQuery),
    Literal(LiteralQuery),
}

impl ExecutionQuery {
    /// The query's raw text, whichever surface it targets.
    pub fn text(&self) -> &str {
        match self {
            ExecutionQuery::Abstract(query) => query.text(),
            ExecutionQuery::Literal(query) => query.as_str(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VersionConstraint {
    Exact(PackageVersion),
    SemverRange(semver::VersionReq),
    Pep440(uv_pep440::VersionSpecifiers),
}

/// A package narrowed by name and optionally version.
///
/// The matching method (`matches`) that checks whether a symbol belongs to this
/// selector lives in `index::ecosystem` — it needs the per-ecosystem structured
/// name grammar. Heart only owns the transport/domain data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSelector {
    pub name: PackageName,
    pub version: Option<VersionConstraint>,
}

/// Narrowing dimensions for a search.
///
/// The matching method (`admits`) that checks whether a symbol survives this
/// filter lives in `index::ecosystem` — it needs ecosystem-specific matching
/// via `PackageSelectorExt`. Heart only owns the transport/domain data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Filter {
    pub ecosystems: Option<nonempty::NonEmpty<Language>>,
    pub packages: Option<nonempty::NonEmpty<PackageSelector>>,
}

/// A complete search request: what to find, how to narrow it.
pub struct Search<'a> {
    pub query: ExecutionQuery,
    pub filter: Filter,
    /// Keyset-pagination spec: how many results to return and where to resume from.
    pub page: PageSpecification,
    // scope field removed — AccessContext was deleted from heart
    pub _lifetime: std::marker::PhantomData<&'a ()>,
}

pub type SymbolCursorKey = (Score, SymbolId);
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
        assert!(matches!(LiteralQuery::parse(""), Err(Error::Empty)));
    }

    #[test]
    fn literal_query_whitespace_only_is_rejected() {
        assert!(matches!(
            LiteralQuery::parse("   "),
            Err(Error::EmptyAfterTrim { .. })
        ));
    }

    #[test]
    fn literal_query_too_long_is_rejected() {
        let long = "x".repeat(LiteralQuery::MAXIMUM_LENGTH + 1);
        assert!(matches!(
            LiteralQuery::parse(&long),
            Err(Error::TooLong { .. })
        ));
    }

    #[test]
    fn literal_query_control_char_is_rejected() {
        assert!(matches!(
            LiteralQuery::parse("foo\x01bar"),
            Err(Error::ControlCharacter { .. })
        ));
    }
}
