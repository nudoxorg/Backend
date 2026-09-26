//! Bounded query descriptors pinned to one source revision.

use super::ViewRevision;
use crate::{Cursor, PackageKey, SymbolKey, ViewRoot, ViewSnapshot, ViewStateRoot};
use backend_semantic::ReadManifest;

/// Bounded result count.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct QueryLimit(u16);

impl QueryLimit {
    /// Maximum rows returned by one query page.
    pub const MAX: u16 = 200;

    /// Creates a positive bounded result count.
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        if value > 0 && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the bounded count.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Default for QueryLimit {
    fn default() -> Self {
        Self(25)
    }
}

/// Search request pinned to one source root and optional continuation cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    /// Search/name text.
    pub(crate) text: String,
    /// Maximum rows in this page.
    pub(crate) limit: QueryLimit,
    /// Exact source root accepted by the caller.
    pub(crate) basis: ViewRevision,
    /// Cursor for the next bounded page.
    pub(crate) cursor: Option<Cursor>,
    /// Optional semantic read manifest that defines the query's dependency
    /// basis. When present, the resulting view recipe is bound to its exact
    /// canonical manifest bytes.
    pub(crate) read_manifest: Option<ReadManifest>,
}

impl Query {
    /// Creates a first-page query pinned to a source root.
    #[must_use]
    pub fn new(text: impl Into<String>, basis: impl Into<ViewRevision>, limit: QueryLimit) -> Self {
        let basis = basis.into();
        Self {
            text: text.into(),
            limit,
            basis,
            cursor: None,
            read_manifest: None,
        }
    }

    /// Returns the query text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the exact source basis.
    #[must_use]
    pub const fn basis(&self) -> ViewRevision {
        self.basis
    }

    /// Returns the bounded page size.
    #[must_use]
    pub const fn limit(&self) -> QueryLimit {
        self.limit
    }

    /// Returns the optional continuation cursor.
    #[must_use]
    pub const fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    /// Returns the semantic dependency manifest bound to this query.
    #[must_use]
    pub fn read_manifest(&self) -> Option<&ReadManifest> {
        self.read_manifest.as_ref()
    }

    /// Returns this query bound to a continuation cursor.
    #[must_use]
    pub const fn with_cursor(mut self, cursor: Cursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Binds the exact semantic dependency manifest used by this query.
    #[must_use]
    pub fn with_read_manifest(mut self, manifest: ReadManifest) -> Self {
        self.read_manifest = Some(manifest);
        self
    }
}

/// A declaration locator whose authority is recovered from the pinned view.
///
/// Canonical symbols carry their text-derived key. Selected symbols carry
/// only the digest of a key that was already admitted from a producer reply;
/// after a process hop that digest remains a selector until it is matched to
/// a typed row in the exact query basis. It cannot be used to publish or mint
/// a [`SymbolKey`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolAddress {
    /// A symbol key derived from its canonical text preimage.
    Canonical(SymbolKey),
    /// An opaque producer key selected from an admitted immutable view.
    Selected([u8; 32]),
}

impl SymbolAddress {
    /// Wraps a canonical text-derived symbol key.
    #[must_use]
    pub const fn canonical(symbol: SymbolKey) -> Self {
        Self::Canonical(symbol)
    }

    /// Creates a view-selected address from a producer-admitted symbol key.
    #[must_use]
    pub const fn selected(symbol: SymbolKey) -> Self {
        Self::Selected(symbol.to_bytes())
    }

    /// Retains an opaque wire claim as a selector without promoting it to a
    /// publication-authoritative key.
    #[must_use]
    pub(crate) const fn from_selected_bytes(bytes: [u8; 32]) -> Self {
        Self::Selected(bytes)
    }

    /// Returns the claimed bytes used for bounded wire encoding and lookup.
    #[must_use]
    pub const fn claimed_bytes(self) -> [u8; 32] {
        match self {
            Self::Canonical(symbol) => symbol.to_bytes(),
            Self::Selected(bytes) => bytes,
        }
    }

    /// Returns whether this address relies on membership in a selected view.
    #[must_use]
    pub const fn is_selected(self) -> bool {
        matches!(self, Self::Selected(_))
    }

    /// Checks an admitted symbol against this locator.
    #[must_use]
    pub fn matches(self, symbol: SymbolKey) -> bool {
        self.claimed_bytes() == symbol.to_bytes()
    }

    /// Resolves this locator against one exact admitted view.
    ///
    /// An opaque selector becomes a `SymbolKey` only by borrowing the typed
    /// key from a row already present in `view`.
    #[must_use]
    pub fn resolve(self, view: &ViewRoot) -> Option<SymbolKey> {
        match self {
            Self::Canonical(symbol) => Some(symbol),
            Self::Selected(bytes) => view.resolve_symbol_commitment(bytes),
        }
    }
}

/// Exact document lookup pinned to a source root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentQuery {
    /// Declaration address.
    pub(crate) symbol: SymbolAddress,
    /// Source root expected by the caller.
    pub(crate) basis: ViewRevision,
    /// Complete source basis expected by the caller, when the request was
    /// produced by an authority that knows the source object and stream.
    pub(crate) source: Option<crate::Basis>,
}

impl DocumentQuery {
    /// Creates an exact document lookup.
    #[must_use]
    pub const fn new(symbol: SymbolKey, basis: ViewStateRoot) -> Self {
        Self {
            symbol: SymbolAddress::canonical(symbol),
            basis: ViewRevision(basis.to_bytes()),
            source: None,
        }
    }

    /// Creates a lookup for a producer-admitted row in the selected view.
    #[must_use]
    pub const fn selected(symbol: SymbolKey, basis: ViewStateRoot) -> Self {
        Self {
            symbol: SymbolAddress::selected(symbol),
            basis: ViewRevision(basis.to_bytes()),
            source: None,
        }
    }

    /// Binds this lookup to the complete source basis used by its producer.
    #[must_use]
    pub const fn with_source_basis(mut self, source: crate::Basis) -> Self {
        self.basis = ViewRevision(source.root.to_bytes());
        self.source = Some(source);
        self
    }

    /// Returns the declaration locator.
    #[must_use]
    pub const fn symbol(&self) -> SymbolAddress {
        self.symbol
    }

    /// Resolves the declaration against the exact admitted view.
    #[must_use]
    pub fn resolve_symbol(&self, view: &ViewRoot) -> Option<SymbolKey> {
        self.symbol.resolve(view)
    }

    /// Returns the exact source basis.
    #[must_use]
    pub const fn basis(&self) -> ViewRevision {
        self.basis
    }

    /// Returns the complete source basis, when this request carries it.
    #[must_use]
    pub const fn source_basis(&self) -> Option<crate::Basis> {
        self.source
    }
}

/// Name lookup pinned to a source root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameQuery {
    /// Canonical name text or prefix.
    pub(crate) text: String,
    /// Maximum rows returned.
    pub(crate) limit: QueryLimit,
    /// Source root expected by the caller.
    pub(crate) basis: ViewRevision,
    /// Optional continuation cursor.
    pub(crate) cursor: Option<Cursor>,
    /// Optional semantic read manifest that defines the query's dependency
    /// basis.
    pub(crate) read_manifest: Option<ReadManifest>,
}

impl NameQuery {
    /// Creates a bounded name query.
    #[must_use]
    pub fn new(text: impl Into<String>, basis: impl Into<ViewRevision>, limit: QueryLimit) -> Self {
        Self {
            text: text.into(),
            limit,
            basis: basis.into(),
            cursor: None,
            read_manifest: None,
        }
    }

    /// Returns the query text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the bounded page size.
    #[must_use]
    pub const fn limit(&self) -> QueryLimit {
        self.limit
    }

    /// Returns the exact source basis.
    #[must_use]
    pub const fn basis(&self) -> ViewRevision {
        self.basis
    }

    /// Returns the optional continuation cursor.
    #[must_use]
    pub const fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    /// Returns this name query bound to a continuation cursor.
    #[must_use]
    pub const fn with_cursor(mut self, cursor: Cursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Returns the semantic dependency manifest bound to this query.
    #[must_use]
    pub fn read_manifest(&self) -> Option<&ReadManifest> {
        self.read_manifest.as_ref()
    }

    /// Binds the exact semantic dependency manifest used by this query.
    #[must_use]
    pub fn with_read_manifest(mut self, manifest: ReadManifest) -> Self {
        self.read_manifest = Some(manifest);
        self
    }
}

/// Package outline lookup pinned to a source root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutlineQuery {
    /// Package identity.
    pub(crate) package: PackageKey,
    /// Source root expected by the caller.
    pub(crate) basis: ViewRevision,
    /// Complete source basis expected by the caller, when available.
    pub(crate) source: Option<crate::Basis>,
}

/// Graph-neighborhood lookup pinned to a source root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphNeighborhoodQuery {
    /// Declaration address at the center of the neighborhood.
    pub(crate) symbol: SymbolAddress,
    /// Source root expected by the caller.
    pub(crate) basis: ViewRevision,
}

/// Opaque continuation for any bounded catalog projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageContinuation(Cursor);

impl PageContinuation {
    /// Wraps a root, recipe, and offset-bound cursor returned by a page.
    #[must_use]
    pub const fn from_cursor(cursor: Cursor) -> Self {
        Self(cursor)
    }

    /// Returns the opaque cursor for transport encoding or a follow-up request.
    #[must_use]
    pub const fn cursor(self) -> Cursor {
        self.0
    }
}

/// Shared bounded page request used by packages, outlines, and graphs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageRequest {
    basis: ViewRevision,
    limit: QueryLimit,
    continuation: Option<PageContinuation>,
}

impl PageRequest {
    /// Creates the first page request at an exact owner revision.
    #[must_use]
    pub fn new(basis: impl Into<ViewRevision>, limit: QueryLimit) -> Self {
        Self {
            basis: basis.into(),
            limit,
            continuation: None,
        }
    }

    /// Resumes from a continuation returned by the preceding page.
    #[must_use]
    pub const fn with_continuation(mut self, continuation: PageContinuation) -> Self {
        self.continuation = Some(continuation);
        self
    }

    /// Returns the exact owner revision.
    #[must_use]
    pub const fn basis(self) -> ViewRevision {
        self.basis
    }

    /// Returns the bounded page size.
    #[must_use]
    pub const fn limit(self) -> QueryLimit {
        self.limit
    }

    /// Returns the opaque continuation, when resuming.
    #[must_use]
    pub const fn continuation(self) -> Option<PageContinuation> {
        self.continuation
    }
}

/// Terminal state of one bounded projection page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageTerminal {
    /// The complete projection has been emitted.
    Complete,
    /// Another page follows at this root and query recipe.
    More(PageContinuation),
    /// Work was cancelled before another page could be produced.
    Cancelled,
}

/// One bounded projection page with an explicit terminal state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionPage {
    /// Immutable rows and freshness for this page.
    pub snapshot: ViewSnapshot,
    /// Explicit completion, continuation, or cancellation state.
    pub terminal: PageTerminal,
}

impl GraphNeighborhoodQuery {
    /// Creates an exact graph-neighborhood lookup.
    #[must_use]
    pub const fn new(symbol: SymbolKey, basis: ViewStateRoot) -> Self {
        Self {
            symbol: SymbolAddress::canonical(symbol),
            basis: ViewRevision(basis.to_bytes()),
        }
    }

    /// Creates a graph lookup for a producer-admitted row in the selected view.
    #[must_use]
    pub const fn selected(symbol: SymbolKey, basis: ViewStateRoot) -> Self {
        Self {
            symbol: SymbolAddress::selected(symbol),
            basis: ViewRevision(basis.to_bytes()),
        }
    }

    /// Returns the declaration locator.
    #[must_use]
    pub const fn symbol(&self) -> SymbolAddress {
        self.symbol
    }

    /// Resolves the declaration against the exact admitted view.
    #[must_use]
    pub fn resolve_symbol(&self, view: &ViewRoot) -> Option<SymbolKey> {
        self.symbol.resolve(view)
    }

    /// Replaces a view-selected locator with the typed key borrowed from that
    /// same view while preserving the already checked query basis.
    #[must_use]
    pub const fn with_resolved_symbol(mut self, symbol: SymbolKey) -> Self {
        self.symbol = SymbolAddress::canonical(symbol);
        self
    }

    /// Returns the exact source root.
    #[must_use]
    pub const fn basis(&self) -> ViewRevision {
        self.basis
    }
}

impl OutlineQuery {
    /// Creates an exact package outline lookup.
    #[must_use]
    pub const fn new(package: PackageKey, basis: ViewStateRoot) -> Self {
        Self {
            package,
            basis: ViewRevision(basis.to_bytes()),
            source: None,
        }
    }

    /// Binds this lookup to the complete source basis used by its producer.
    #[must_use]
    pub const fn with_source_basis(mut self, source: crate::Basis) -> Self {
        self.basis = ViewRevision(source.root.to_bytes());
        self.source = Some(source);
        self
    }

    /// Returns the package identity.
    #[must_use]
    pub const fn package(&self) -> PackageKey {
        self.package
    }

    /// Returns the exact source basis.
    #[must_use]
    pub const fn basis(&self) -> ViewRevision {
        self.basis
    }

    /// Returns the complete source basis, when this request carries it.
    #[must_use]
    pub const fn source_basis(&self) -> Option<crate::Basis> {
        self.source
    }
}
