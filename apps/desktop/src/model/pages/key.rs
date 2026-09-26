//! Identity keys for page resources.

use super::common::{KeyError, PackageRef, SymbolRef};
use std::fmt;
use std::sync::Arc;

/// One search query as a resource identity. Further pages of the same query
/// append to the same resource, so the key does not carry a cursor.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SearchQuery {
    /// Query text, trimmed.
    pub text: Arc<str>,
    /// Page size (1..=200).
    pub limit: u16,
}

impl SearchQuery {
    /// Default page size for Ask and the results page.
    pub const DEFAULT_LIMIT: u16 = 50;

    /// Admits one query.
    ///
    /// # Errors
    /// Returns [`KeyError`] for empty text or control characters.
    pub fn new(text: &str, limit: u16) -> Result<Self, KeyError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(KeyError::Empty);
        }
        if text.chars().any(char::is_control) {
            return Err(KeyError::ControlCharacter);
        }
        Ok(Self {
            text: Arc::from(text),
            limit: limit.clamp(1, 200),
        })
    }
}

/// Identity of one keyed page resource.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PageKey {
    /// A declaration page.
    Symbol(SymbolRef),
    /// A declaration's source view.
    Source(SymbolRef),
    /// A package dossier.
    Package(PackageRef),
    /// One search query's accumulated result pages.
    Search(SearchQuery),
    /// The Orbit model (one per window).
    Orbit,
    /// The owner health model (one per window).
    Health,
}

impl PageKey {
    /// Returns the stable lowercase resource family.
    #[must_use]
    pub const fn family(&self) -> &'static str {
        match self {
            Self::Symbol(_) => "symbol",
            Self::Source(_) => "source",
            Self::Package(_) => "package",
            Self::Search(_) => "search",
            Self::Orbit => "orbit",
            Self::Health => "health",
        }
    }
}

impl fmt::Display for PageKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbol(symbol) => write!(formatter, "symbol {symbol}"),
            Self::Source(symbol) => write!(formatter, "source {symbol}"),
            Self::Package(package) => write!(formatter, "package {package}"),
            Self::Search(query) => write!(formatter, "search {:?} x{}", query.text, query.limit),
            Self::Orbit => formatter.write_str("orbit"),
            Self::Health => formatter.write_str("health"),
        }
    }
}
