//! Closed product commands and bounded query descriptors.

use crate::{Cursor, ViewRoot, ViewSnapshot, ViewStateRoot};
use crate::{Document, NameRecord, Outline, PackageKey, Row, SymbolKey};
use backend_semantic::ReadManifest;

/// Closed command identity shared by CLI, MCP, and desktop transports.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CommandId {
    /// List selected packages.
    Packages,
    /// Add one package.
    Add,
    /// Remove one package.
    Remove,
    /// Read one declaration document.
    Document,
    /// Compatibility spelling for [`CommandId::Document`].
    Show,
    /// Read one package outline.
    Outline,
    /// Resolve a canonical name.
    Name,
    /// Compatibility spelling for a name resolution request.
    Resolve,
    /// Search text and names.
    Search,
    /// Read graph neighbors.
    Graph,
    /// Read engine health.
    Health,
    /// Read the constant-size current revision token.
    Revision,
}

/// Constant-size handle for the owner's current immutable view.
///
/// The root is admitted from the authenticated producer rather than rebuilt
/// from every visible row. The paired cursor is the exact resume point for
/// subscriptions, including intent-only progress that does not alter rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionReceipt {
    root: ViewStateRoot,
    cursor: Cursor,
    source: crate::SemanticObject,
}

/// Opaque reference to an immutable visible view used as a query precondition.
///
/// A reference can cross a request boundary without transferring the
/// relation preimage. It never grants access to relation contents: the owner
/// compares its bytes with a materialized [`ViewStateRoot`] before executing
/// the query, while replies still carry fully admitted identities.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ViewRevision([u8; backend_version::ID_BYTES]);

impl ViewRevision {
    /// Creates a request reference from fixed-width wire bytes.
    pub(crate) const fn from_bytes(bytes: [u8; backend_version::ID_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width revision bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; backend_version::ID_BYTES] {
        &self.0
    }

    /// Checks this request reference against an owner-materialized root.
    #[must_use]
    pub fn matches(self, root: ViewStateRoot) -> bool {
        self.0 == *root.as_bytes()
    }
}

impl From<ViewStateRoot> for ViewRevision {
    fn from(root: ViewStateRoot) -> Self {
        Self(root.to_bytes())
    }
}

impl PartialEq<ViewStateRoot> for ViewRevision {
    fn eq(&self, other: &ViewStateRoot) -> bool {
        self.0 == *other.as_bytes()
    }
}

impl PartialEq<ViewRevision> for ViewStateRoot {
    fn eq(&self, other: &ViewRevision) -> bool {
        self.as_bytes() == &other.0
    }
}

impl RevisionReceipt {
    /// Creates a receipt from owner-admitted identities.
    #[must_use]
    pub const fn new(root: ViewStateRoot, cursor: Cursor, source: crate::SemanticObject) -> Self {
        Self {
            root,
            cursor,
            source,
        }
    }

    /// Returns the current immutable visible root.
    #[must_use]
    pub const fn root(self) -> ViewStateRoot {
        self.root
    }

    /// Returns the exact owner subscription cursor.
    #[must_use]
    pub const fn cursor(self) -> Cursor {
        self.cursor
    }

    /// Returns the producer-authorized source object for the root claim.
    #[must_use]
    pub const fn source(self) -> crate::SemanticObject {
        self.source
    }
}

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

/// Exact document lookup pinned to a source root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentQuery {
    /// Declaration identity.
    pub(crate) symbol: SymbolKey,
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
            symbol,
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

    /// Returns the declaration identity.
    #[must_use]
    pub const fn symbol(&self) -> SymbolKey {
        self.symbol
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
pub struct GraphQuery {
    /// Declaration identity at the center of the neighborhood.
    pub(crate) symbol: SymbolKey,
    /// Source root expected by the caller.
    pub(crate) basis: ViewRevision,
}

impl GraphQuery {
    /// Creates an exact graph-neighborhood lookup.
    #[must_use]
    pub const fn new(symbol: SymbolKey, basis: ViewStateRoot) -> Self {
        Self {
            symbol,
            basis: ViewRevision(basis.to_bytes()),
        }
    }

    /// Returns the declaration identity.
    #[must_use]
    pub const fn symbol(&self) -> SymbolKey {
        self.symbol
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

/// One closed in-process command. Transport DTOs lower into this enum once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// List selected packages.
    Packages,
    /// Append an add-package intent.
    Add {
        /// Package identity.
        package: PackageKey,
    },
    /// Append a remove-package intent.
    Remove {
        /// Package identity.
        package: PackageKey,
    },
    /// Read one document.
    Document(DocumentQuery),
    /// Compatibility spelling for document lookup.
    Show {
        /// Declaration identity.
        symbol: SymbolKey,
    },
    /// Read one package outline.
    Outline(OutlineQuery),
    /// Resolve one canonical name.
    Name(NameQuery),
    /// Compatibility spelling for name resolution.
    Resolve {
        /// Name text.
        text: String,
    },
    /// Search names and document text.
    Search(Query),
    /// Read graph neighbors for one declaration.
    Graph(GraphQuery),
    /// Read truthful health/coverage state.
    Health,
    /// Read a constant-size revision and subscription cursor.
    Revision,
}

impl Command {
    /// Returns this command's closed identity.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        match self {
            Self::Packages => CommandId::Packages,
            Self::Add { .. } => CommandId::Add,
            Self::Remove { .. } => CommandId::Remove,
            Self::Document(_) => CommandId::Document,
            Self::Show { .. } => CommandId::Show,
            Self::Outline(_) => CommandId::Outline,
            Self::Name(_) => CommandId::Name,
            Self::Resolve { .. } => CommandId::Resolve,
            Self::Search(_) => CommandId::Search,
            Self::Graph(_) => CommandId::Graph,
            Self::Health => CommandId::Health,
            Self::Revision => CommandId::Revision,
        }
    }

    /// Converts a compatibility show command into the document query form.
    #[must_use]
    pub const fn as_document_query(&self, basis: ViewStateRoot) -> Option<DocumentQuery> {
        match self {
            Self::Document(query) => Some(*query),
            Self::Show { symbol } => Some(DocumentQuery {
                symbol: *symbol,
                basis: ViewRevision(basis.to_bytes()),
                source: None,
            }),
            _ => None,
        }
    }

    /// Converts a compatibility resolve command into the name query form.
    #[must_use]
    pub fn as_name_query(&self, basis: ViewStateRoot, limit: QueryLimit) -> Option<NameQuery> {
        match self {
            Self::Name(query) => Some(query.clone()),
            Self::Resolve { text } => Some(NameQuery::new(text.clone(), basis, limit)),
            _ => None,
        }
    }
}

/// Typed command reply; successful views retain their coherent basis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandReply {
    /// Package shelf view.
    Packages(ViewSnapshot),
    /// Accepted add intent.
    Added(crate::IntentId),
    /// Accepted remove intent.
    Removed(crate::IntentId),
    /// Declaration document.
    Document(Document),
    /// Compatibility page spelling for a document.
    Page(Document),
    /// Package outline.
    Outline(Outline),
    /// Name arrangement view.
    Names(ViewSnapshot),
    /// Compatibility resolved rows.
    Resolved(Box<[Row]>),
    /// Search arrangement view.
    Search(ViewSnapshot),
    /// Graph arrangement view.
    Graph(ViewSnapshot),
    /// Health/coverage view.
    Health(ViewRoot),
    /// Constant-size current revision.
    Revision(RevisionReceipt),
    /// Typed transport-visible failure from a command/query boundary.
    Error(String),
}

/// A materialized document/name/outline record used at a projection boundary.
///
/// The production library does not own a mutable record registry. Engine
/// arrangements or test fixtures may use this closed value when lowering a
/// query result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryRecord {
    /// Document record.
    Document(Document),
    /// Name arrangement record.
    Name(NameRecord),
    /// Outline record.
    Outline(Outline),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{Cursor, package_key, symbol_key, view_state_root};

    #[test]
    fn query_limits_are_positive_and_bounded() {
        assert!(QueryLimit::new(0).is_none());
        assert!(QueryLimit::new(QueryLimit::MAX).is_some());
        assert!(QueryLimit::new(QueryLimit::MAX + 1).is_none());
        assert_eq!(QueryLimit::default().get(), 25);
    }

    #[test]
    fn compatibility_commands_lower_to_pinned_queries() {
        let basis = view_state_root(&[]);
        let symbol = symbol_key("pkg::Thing");
        let document = Command::Show { symbol };
        assert_eq!(
            document.as_document_query(basis),
            Some(DocumentQuery {
                symbol,
                basis: basis.into(),
                source: None,
            })
        );
        assert!(Command::Health.as_document_query(basis).is_none());

        let resolve = Command::Resolve {
            text: "Thing".to_owned(),
        };
        let name = resolve
            .as_name_query(basis, QueryLimit::default())
            .expect("resolve lowers");
        assert_eq!(name.text, "Thing");
        assert_eq!(name.basis(), basis);
        assert_eq!(name.cursor(), None);
        assert_eq!(name.limit(), QueryLimit::default());
        assert!(
            Command::Health
                .as_name_query(basis, QueryLimit::default())
                .is_none()
        );
    }

    #[test]
    fn query_cursor_keeps_the_requested_continuation() {
        let basis = view_state_root(&[]);
        let cursor = Cursor::new();
        let query = Query::new("Thing", basis, QueryLimit::default()).with_cursor(cursor);
        assert_eq!(query.cursor(), Some(cursor));
        assert_eq!(
            Command::Add {
                package: package_key("pkg")
            }
            .id(),
            CommandId::Add
        );
    }

    #[test]
    fn query_manifest_is_part_of_the_bound_recipe() {
        let basis = view_state_root(&[]);
        let manifest = ReadManifest::new(Vec::new()).expect("empty manifest");
        let query =
            Query::new("Thing", basis, QueryLimit::default()).with_read_manifest(manifest.clone());
        assert_eq!(query.read_manifest(), Some(&manifest));
        let names = NameQuery::new("Thing", basis, QueryLimit::default())
            .with_read_manifest(manifest.clone());
        assert_eq!(names.read_manifest(), Some(&manifest));
    }
}
