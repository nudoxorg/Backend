//! Ask / Search results read model.

use super::common::{DeclRef, Known};
use super::symbol::SignatureText;
use std::sync::Arc;

/// Why one row matched, read lexically from the row the producer returned.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MatchReason {
    /// The declaration name equals the query (case-insensitive).
    ExactName,
    /// The declaration name contains the query.
    Name,
    /// The signature contains the query.
    Signature,
    /// The first documentation line contains the query.
    Docs,
    /// The producer returned the row; none of the visible fields contain the
    /// query text (for example a path or semantic match).
    Producer,
}

impl MatchReason {
    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ExactName => "exact-name",
            Self::Name => "name",
            Self::Signature => "signature",
            Self::Docs => "docs",
            Self::Producer => "producer",
        }
    }
}

/// One ranked result row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRow {
    /// Producer rank, zero-based, in reply order.
    pub rank: usize,
    /// The declaration.
    pub decl: DeclRef,
    /// Owning package spelling, when the coordinate names one.
    pub package: Option<Arc<str>>,
    /// Producer score; the local engine currently publishes none.
    pub score: Known<u32>,
    /// Signature, when captured as source text.
    pub signature: Known<SignatureText>,
    /// First documentation line.
    pub snippet: Option<Arc<str>>,
    /// One reason the row matched.
    pub reason: MatchReason,
}

/// Opaque continuation for the next page of one query. It is pinned to the
/// read session that issued it, because the producer certificate lives there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchContinuation {
    /// Owner-issued cursor.
    pub cursor: backend_library::PageContinuation,
    /// Read-pool worker whose session holds the certificate.
    pub worker: usize,
}

/// One page of results.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchPage {
    /// Query text exactly as asked.
    pub query: Arc<str>,
    /// Rows in producer order.
    pub rows: Arc<[SearchRow]>,
    /// Lane coverage behind the rows.
    pub coverage: backend_present::CoverageLine,
    /// Continuation, when the producer has more rows.
    pub next: Option<SearchContinuation>,
}
