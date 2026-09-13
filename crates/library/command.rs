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
    /// Execute one bounded structured graph query.
    GraphQuery,
    /// Read one declaration's captured source.
    Source,
    /// Read symbols related to one declaration.
    Related,
    /// Read several declaration documents.
    Read,
    /// Compare package declaration versions.
    Diff,
    /// Browse registry packages.
    Explore,
    /// Read one registry package profile.
    Package,
    /// Read packages depending on one registry package.
    Dependents,
    /// Read packages published by one owner.
    Owner,
    /// Search the local registry index.
    IndexSearch,
    /// Read recorded package versions.
    PackageVersions,
    /// Read immutable compiler generation history.
    SemanticVersions,
    /// Select an exact immutable compiler generation.
    SelectSemanticVersion,
    /// Read the latest package profile and history.
    PackageProfile,
    /// Follow one package for releases.
    Subscribe,
    /// Stop following one package.
    Unsubscribe,
    /// List followed packages.
    Subscriptions,
    /// Read new followed-package releases.
    Releases,
    /// List project folders.
    Projects,
    /// Create a project folder.
    ProjectCreate,
    /// Delete a project folder.
    ProjectDelete,
    /// Add a package to a project folder.
    ProjectAdd,
    /// Remove a package from a project folder.
    ProjectRemove,
    /// Reconcile a project with its lockfile.
    ProjectSync,
    /// Read the cross-surface session tree.
    Tree,
    /// Open a subject in the session tree.
    TreeOpen,
    /// Close a node or branch in the session tree.
    TreeClose,
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

/// Constant-size owner health report.
///
/// This retains the exact visible revision, source basis, cursor, coverage,
/// and cardinality without transferring the view relation's rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthReport {
    revision: RevisionReceipt,
    basis: crate::Basis,
    coverage: Box<[crate::Coverage]>,
    row_count: u64,
    capabilities: crate::CapabilityInventory,
}

impl HealthReport {
    /// Captures health from one coherent owner root without cloning its rows.
    #[must_use]
    pub fn from_root(root: &ViewRoot, cursor: Cursor) -> Self {
        Self {
            revision: RevisionReceipt::new(root.root(), cursor, root.basis().object),
            basis: root.basis(),
            coverage: root.coverage().to_vec().into_boxed_slice(),
            row_count: root.row_count(),
            capabilities: crate::CapabilityInventory::explicitly_unavailable(),
        }
    }

    /// Replaces the explicit baseline with owner-observed capability states.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: crate::CapabilityInventory) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Constructs a report after protocol admission of all identities.
    #[must_use]
    pub fn from_admitted_parts(
        revision: RevisionReceipt,
        basis: crate::Basis,
        coverage: Box<[crate::Coverage]>,
        row_count: u64,
        capabilities: crate::CapabilityInventory,
    ) -> Self {
        Self {
            revision,
            basis,
            coverage,
            row_count,
            capabilities,
        }
    }

    /// Returns the current visible revision and exact owner cursor.
    #[must_use]
    pub const fn revision(&self) -> RevisionReceipt {
        self.revision
    }

    /// Returns the complete source basis observed by the projection.
    #[must_use]
    pub const fn basis(&self) -> crate::Basis {
        self.basis
    }

    /// Returns typed coverage for every declared retrieval lane.
    #[must_use]
    pub fn coverage(&self) -> &[crate::Coverage] {
        &self.coverage
    }

    /// Returns the number of rows committed by the visible root.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Returns the bounded executable capability inventory.
    #[must_use]
    pub const fn capabilities(&self) -> &crate::CapabilityInventory {
        &self.capabilities
    }
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

/// One closed in-process command. Transport DTOs lower into this enum once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// List selected packages.
    Packages,
    /// Read one bounded package page.
    PackagePage(PageRequest),
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
    /// Read one declaration's captured source through its exact document row.
    Source(DocumentQuery),
    /// Compatibility spelling for document lookup.
    Show {
        /// Declaration identity.
        symbol: SymbolKey,
    },
    /// Read one package outline.
    Outline(OutlineQuery),
    /// Read package outline rows as a bounded flat page.
    OutlinePage {
        /// Package whose flat outline membership is paged.
        package: PackageKey,
        /// Shared root/query-bound paging request.
        page: PageRequest,
    },
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
    Graph(GraphNeighborhoodQuery),
    /// Read symbols related to one declaration.
    Related(GraphNeighborhoodQuery),
    /// Read graph-neighborhood rows as a bounded page.
    GraphPage {
        /// Declaration at the center of the graph neighborhood.
        symbol: SymbolAddress,
        /// Shared root/query-bound paging request.
        page: PageRequest,
    },
    /// Execute or resume one structured graph query at an immutable revision.
    GraphQuery(crate::GraphQueryRequest),
    /// Execute one daemon-owned durable product-surface operation.
    Surface(crate::SurfaceCommand),
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
            Self::Packages | Self::PackagePage(_) => CommandId::Packages,
            Self::Add { .. } => CommandId::Add,
            Self::Remove { .. } => CommandId::Remove,
            Self::Document(_) => CommandId::Document,
            Self::Source(_) => CommandId::Source,
            Self::Show { .. } => CommandId::Show,
            Self::Outline(_) | Self::OutlinePage { .. } => CommandId::Outline,
            Self::Name(_) => CommandId::Name,
            Self::Resolve { .. } => CommandId::Resolve,
            Self::Search(_) => CommandId::Search,
            Self::Graph(_) | Self::GraphPage { .. } => CommandId::Graph,
            Self::Related(_) => CommandId::Related,
            Self::GraphQuery(_) => CommandId::GraphQuery,
            Self::Surface(command) => command.id(),
            Self::Health => CommandId::Health,
            Self::Revision => CommandId::Revision,
        }
    }

    /// Converts a compatibility show command into the document query form.
    #[must_use]
    pub const fn as_document_query(&self, basis: ViewStateRoot) -> Option<DocumentQuery> {
        match self {
            Self::Document(query) | Self::Source(query) => Some(*query),
            Self::Show { symbol } => Some(DocumentQuery {
                symbol: SymbolAddress::canonical(*symbol),
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
    /// Shared bounded package/outline/graph page.
    ProjectionPage(ProjectionPage),
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
    /// Bounded structured graph-query rows and terminal.
    GraphQueryPage(crate::GraphQueryPage),
    /// Result from one durable product-surface owner.
    Surface(crate::SurfaceReply),
    /// Health/coverage view.
    Health(ViewRoot),
    /// Constant-size health/readiness report for process boundaries.
    Readiness(HealthReport),
    /// Constant-size current revision.
    Revision(RevisionReceipt),
    /// Typed transport-visible failure from a command/query boundary.
    Error(String),
    /// Closed application failure that callers can handle without parsing prose.
    Failed(CommandFailure),
}

/// Closed failures produced by the application-service boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandFailure {
    /// The requested package, symbol, or document does not exist.
    NotFound,
    /// The request was pinned to a revision other than the owner's current revision.
    WrongBasis {
        /// Current owner revision.
        expected: ViewRevision,
        /// Revision supplied by the request.
        observed: ViewRevision,
    },
    /// The query failed bounded semantic validation.
    InvalidQuery(String),
    /// The cursor belongs to another recipe, revision, or stream position.
    CursorMismatch,
    /// The retained view could not satisfy an invariant.
    IncoherentView(String),
    /// The owner cannot represent another event in its sequence space.
    SequenceOverflow,
    /// This operation must be submitted to the durable engine owner.
    MutationRequiresOwner,
}

impl From<crate::LibraryError> for CommandFailure {
    fn from(error: crate::LibraryError) -> Self {
        match error {
            crate::LibraryError::NotFound => Self::NotFound,
            crate::LibraryError::WrongBasis { expected, observed } => Self::WrongBasis {
                expected: expected.into(),
                observed,
            },
            crate::LibraryError::View(error) => Self::IncoherentView(format!("{error:?}")),
            crate::LibraryError::InvalidQuery(message) => Self::InvalidQuery(message),
            crate::LibraryError::SequenceOverflow => Self::SequenceOverflow,
            crate::LibraryError::CursorMismatch => Self::CursorMismatch,
        }
    }
}

impl core::fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("library record not found"),
            Self::WrongBasis { .. } => {
                formatter.write_str("query basis does not match the view revision")
            }
            Self::InvalidQuery(message) => write!(formatter, "invalid query: {message}"),
            Self::CursorMismatch => formatter.write_str("cursor does not match the view"),
            Self::IncoherentView(message) => write!(formatter, "incoherent view: {message}"),
            Self::SequenceOverflow => formatter.write_str("event sequence overflowed"),
            Self::MutationRequiresOwner => {
                formatter.write_str("mutation requires the durable engine owner")
            }
        }
    }
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
                symbol: SymbolAddress::canonical(symbol),
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
