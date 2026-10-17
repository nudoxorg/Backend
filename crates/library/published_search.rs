//! A published-name read is useful while richer search is still preparing.
//! It has its own command, recipe and cursor domain: it never claims that
//! Tantivy considered the complete lexical corpus or that semantic IR is ready.

use crate::{Query, QueryPreparationState, ViewSnapshot};
use serde::{Deserialize, Serialize};

/// One admitted identifier or parent-qualified identifier lookup.
/// The immutable owner's name arrangement proves every returned binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedNameSearchQuery(Query);

impl PublishedNameSearchQuery {
    /// Accepts letter/digit/underscore names, with dotted/`::` qualification.
    /// Other identifier spellings conservatively retain full-search behavior.
    /// General text, operators and multiple clauses retain full-search semantics.
    ///
    /// # Errors
    /// Returns the original query when it cannot use the published-name stage.
    pub fn admit(query: Query) -> Result<Self, Query> {
        let text = query.text();
        if text.is_empty() || text.trim() != text || text.chars().any(char::is_whitespace) {
            return Err(query);
        }
        let segments = text.split("::").flat_map(|part| part.split('.'));
        for segment in segments {
            let mut chars = segment.chars();
            let Some(first) = chars.next() else {
                return Err(query);
            };
            if !(first == '_' || first.is_alphabetic())
                || !chars.all(|ch| ch == '_' || ch.is_alphanumeric())
            {
                return Err(query);
            }
        }
        Ok(Self(query))
    }

    /// Borrows the exact text, source, credit and continuation contract.
    #[must_use]
    pub const fn query(&self) -> &Query {
        &self.0
    }

    /// Changes delivery only within the exact issued credit of this stage.
    #[must_use]
    pub fn with_window(self, window: crate::QueryWindow) -> Option<Self> {
        self.0.with_window(window).map(Self)
    }

    /// Preserves this stage when resuming an authenticated position.
    #[must_use]
    pub fn with_cursor(self, cursor: crate::Cursor) -> Self {
        Self(self.0.with_cursor(cursor))
    }
}

/// Current full-search readiness, separate from optional vector availability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FullSearchReadiness {
    /// The exact current view's richer search preparation is running.
    Preparing,
    /// A superseded selection is retiring before current preparation can start.
    Retiring,
    /// Richer search is ready, but this cursor remains in the name-only stage.
    Ready,
}

impl From<QueryPreparationState> for FullSearchReadiness {
    fn from(state: QueryPreparationState) -> Self {
        match state {
            QueryPreparationState::Preparing => Self::Preparing,
            QueryPreparationState::Retiring => Self::Retiring,
        }
    }
}

/// Semantic work observed separately from the useful name-only response.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublishedSemanticReadiness {
    /// The full corpus must finish before semantic ranking can be queried.
    Pending,
    /// Full search is ready, but this name-only continuation never queries it.
    NotQueried,
    /// The owner independently knows this optional lane cannot currently run.
    Unavailable { reason: crate::SemanticSearchReason },
}

/// Layered observation; source publication, full lexical search and optional
/// semantic ranking are never collapsed into one successful/ready bit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedSearchReadiness {
    /// State of the full selected search projection.
    pub full_search: FullSearchReadiness,
    /// Independently observed semantic state, never an Available claim.
    pub semantic: PublishedSemanticReadiness,
}

impl PublishedSearchReadiness {
    /// Whether these observations can describe the same response.
    #[must_use]
    pub const fn is_consistent(self) -> bool {
        match (self.full_search, self.semantic) {
            (FullSearchReadiness::Ready, PublishedSemanticReadiness::Pending)
            | (
                FullSearchReadiness::Preparing | FullSearchReadiness::Retiring,
                PublishedSemanticReadiness::NotQueried,
            ) => false,
            _ => true,
        }
    }
}

/// Exact current name bindings; never a complete lexical/semantic search claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedNameSearchPage {
    /// Authenticated owner-root name rows and a name-stage-only continuation.
    pub snapshot: ViewSnapshot,
    /// Explicit independent full-search and semantic states.
    pub readiness: PublishedSearchReadiness,
}
