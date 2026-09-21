//! Reader tabs as a tree, per-tab history, the page cache, and hover cards.
//! A tab keeps showing the page it has until the next one has fully arrived.
//! Nothing here flashes: loading is a reserved state, not an empty screen.
//!
//! Tabs form a tree rather than a strip. A declaration followed from a page
//! opens *under* that page by default, so the reader's exploration is recorded
//! as the shape it actually had — a trunk and the branches taken from it —
//! instead of a flat row that forgets which page led where. A subject already
//! open anywhere in the tree is revisited, never duplicated, so the tree stays
//! a map of what has been read rather than a log of every click. Closing a
//! branch hands its children to its parent; nothing is orphaned and nothing
//! is lost.
//!
//! Every open goes through [`DocumentStore::open_symbol`], which resolves the
//! declaration's coordinate from the index before a tab exists. A symbol the
//! shelf does not hold is answered with a fault in place, never with a page
//! titled by a hash.
//!
//! The page itself is assembled by [`backend_present::page_from_document`] —
//! the same function `backend page` and the MCP `backend.page` tool call — so
//! this store's job is only to decide *which* replies to ask for and when to
//! install them, never what they mean.

use super::events::DocumentEvent;
use super::index::{IndexStore, ProjectIndex};
use super::service::{Endpoint, Outcome, Request};
use super::workspace::{WorkspaceStore, coordinate_operand};
use crate::presentation::fault::wrong_shape;
use backend_library::{DeclarationKind, Row, SymbolKey, encode_id};
use backend_present::{
    Affordance, Cause, CauseSlug, Coordinate, Fault, FaultSlug, Identity, Operand, Page, Source,
    page_from_document,
};
use gpui::AppContext as _;
use gpui::Context;
use gpui::{Entity, EventEmitter, Modifiers, Pixels, Point, ScrollHandle, Task};
use std::sync::Arc;
use std::time::Duration;

/// How many pages are remembered across tabs.
const PAGE_CACHE: usize = 64;

/// How many hover cards are remembered.
const HOVER_CACHE: usize = 128;

/// Character budget for a hover card's signature line.
const CARD_PREVIEW: usize = 140;

/// How long the pointer must rest before a hover card is requested.
const HOVER_DELAY: Duration = Duration::from_millis(350);

type PageHandle = Arc<Page>;

#[derive(Default)]
struct PageCache {
    entries: Vec<(String, PageHandle)>,
}

impl PageCache {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn get(&self, coordinate: &str) -> Option<PageHandle> {
        self.entries
            .iter()
            .find(|(key, _)| key == coordinate)
            .map(|(_, page)| Arc::clone(page))
    }

    fn insert(&mut self, coordinate: String, page: PageHandle) {
        self.entries.retain(|(key, _)| *key != coordinate);
        self.entries.push((coordinate, page));
        while self.entries.len() > PAGE_CACHE {
            self.entries.remove(0);
        }
    }

    fn retain(&mut self, keep: impl FnMut(&(String, PageHandle)) -> bool) {
        self.entries.retain(keep);
    }
}

/// What a tab is showing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Subject {
    /// The browse page: the shelf at a glance and the registry to explore.
    Home,
    /// One declaration, addressed by its admitted identity.
    Declaration {
        /// Admitted declaration key.
        symbol: SymbolKey,
        /// Exact producer coordinate.
        coordinate: String,
    },
    /// One project on the shelf, addressed by its coordinate.
    Project {
        /// Exact project coordinate.
        coordinate: String,
    },
    /// One registry package, addressed by its pinned coordinate.
    Package {
        /// Pinned package coordinate, `pkg:ecosystem/name@version`.
        coordinate: String,
    },
    /// The declaration outline for a project/package handoff.
    ///
    /// This is intentionally separate from [`Subject::Project`].  A docs
    /// route is a reader page rooted at the project, while a code route is an
    /// outline projection. Keeping that distinction in the subject prevents
    /// the package surface from accidentally collapsing every tab into the
    /// same project view.
    Outline {
        /// Exact project coordinate whose declarations are outlined.
        coordinate: String,
    },
}

impl Subject {
    /// Returns the coordinate this subject is keyed by; empty for home.
    pub(crate) fn coordinate(&self) -> &str {
        match self {
            Self::Home => "",
            Self::Declaration { coordinate, .. }
            | Self::Project { coordinate }
            | Self::Package { coordinate }
            | Self::Outline { coordinate } => coordinate,
        }
    }

    /// Returns the readable identity of this subject, when it has one.
    pub(crate) fn identity(&self) -> Option<Identity> {
        match self {
            Self::Home => None,
            Self::Declaration { coordinate, .. }
            | Self::Project { coordinate }
            | Self::Package { coordinate }
            | Self::Outline { coordinate } => Some(Identity::parse(coordinate)),
        }
    }

    /// Returns the tab title.
    pub(crate) fn title(&self) -> String {
        match self {
            Self::Home => "Browse".to_owned(),
            Self::Declaration { .. }
            | Self::Project { .. }
            | Self::Package { .. }
            | Self::Outline { .. } => self
                .identity()
                .map(|identity| identity.name().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| self.coordinate().to_owned()),
        }
    }

    /// Returns the project root this subject lives in, when it lives in one.
    pub(crate) fn project_root(&self) -> Option<String> {
        match self {
            Self::Home | Self::Package { .. } => None,
            Self::Project { coordinate } | Self::Outline { coordinate } => Some(coordinate.clone()),
            Self::Declaration { .. } => self
                .identity()
                .and_then(|identity| identity.project().map(|project| project.root().to_owned())),
        }
    }
}

/// Where a newly opened subject should land.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    /// Replace what the active tab is showing, keeping its history.
    Here,
    /// Open under the active tab and switch to it — or, when the subject is
    /// already open anywhere in the tree, switch to that tab instead.
    Child,
    /// Open under the active tab without switching.
    Background,
}

/// Reader surface requested by a package entry point.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ReaderIntent {
    /// Package documentation and declaration pages.
    Docs,
    /// Captured source for a declaration.
    Source,
    /// The package's indexed declarations.
    Code,
    /// Code and documentation search scoped to the package.
    Search,
}

/// The concrete reader route currently owning the active tab.
///
/// Page and overlay labels are harness vocabulary. This value stays in the
/// document owner so semantic probes can distinguish a docs handoff from a
/// declaration page even when both resolve to the same admitted `Page`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderSurface {
    /// The browse landing page.
    Browse,
    /// A project outline/root.
    Project,
    /// A registry package landing page.
    Package,
    /// A declaration page.
    Declaration,
    /// A captured source sheet for a declaration.
    Source,
    /// An indexed code outline.
    Code,
    /// Documentation reached through a package handoff.
    Docs,
    /// The expanded declaration graph.
    Graph,
    /// Direct dependencies for a package.
    Dependencies,
    /// Reverse dependents for a package.
    Dependents,
    /// Release history for a package.
    Releases,
    /// Security/advisory details for a package.
    Security,
    /// Scoped code search.
    CodeSearch,
}

impl ReaderSurface {
    /// Stable route spelling used by semantic artifacts and assertions.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Browse => "browse",
            Self::Project => "project",
            Self::Package => "package",
            Self::Declaration => "declaration",
            Self::Source => "source",
            Self::Code => "code",
            Self::Docs => "docs",
            Self::Graph => "graph",
            Self::Dependencies => "dependencies",
            Self::Dependents => "dependents",
            Self::Releases => "releases",
            Self::Security => "security",
            Self::CodeSearch => "code-search",
        }
    }
}

/// A typed package-to-reader handoff.
///
/// The entry carries both store generations observed when it was produced.
/// A package route that arrives after the shelf changed is rejected instead
/// of showing a textual placeholder for data that is no longer authoritative.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReaderEntry {
    coordinate: String,
    intent: ReaderIntent,
    document_generation: u64,
    index_version: [u8; 32],
}

/// The concrete route resolved from one package handoff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReaderRoute {
    /// Documentation resolves to the package/project hierarchy or a known declaration.
    Docs { subject: Subject },
    /// Source resolves only for an admitted declaration.
    Source { subject: Subject, symbol: SymbolKey },
    /// Code resolves to the indexed package/project outline.
    Code { subject: Subject },
    /// Search retains the package/project scope for the search store.
    Search { scope: String },
}

/// Why a typed package handoff cannot be opened against the current stores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderEntryFault {
    /// The handoff was created against a previous tab or index revision.
    Stale,
    /// The coordinate is absent from the live index.
    NotIndexed,
    /// Source requires a declaration coordinate, not only a package root.
    DeclarationRequired,
}

/// Closed reader coverage failures.  These are deliberately separate from a
/// transport/client error: a missing capture is a truthful product state,
/// while a failed request is an operational failure and a pending tab is
/// neither.  Keeping the states typed prevents a renderer from turning all
/// three into the same vague "unavailable" panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderCoverageFault {
    /// The producer admitted the declaration but retained no source bytes.
    AbsentCapture,
    /// A source site exists, but the semantic authority did not provide bytes.
    SemanticAuthorityUnavailable,
    /// The requested declaration is still being fetched.
    Pending,
    /// The response belongs to a superseded tab/index generation.
    Stale,
    /// The local service failed while resolving the reader page.
    Service,
}

impl ReaderCoverageFault {
    /// Classifies the source coverage encoded in a shared presentation page.
    pub(crate) const fn for_source(source: &Source) -> Option<Self> {
        match source {
            Source::Captured { .. } => None,
            Source::Sited { .. } => Some(Self::SemanticAuthorityUnavailable),
            Source::Absent { .. } => Some(Self::AbsentCapture),
        }
    }
}

impl ReaderEntry {
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    pub(crate) const fn intent(&self) -> ReaderIntent {
        self.intent
    }

    pub(crate) fn is_current(&self, document_generation: u64, index_version: [u8; 32]) -> bool {
        self.document_generation == document_generation && self.index_version == index_version
    }

    fn resolve(&self, index: &IndexStore) -> Result<ReaderRoute, ReaderEntryFault> {
        let symbol = index.symbol_for(&self.coordinate);
        let project = index
            .project(&self.coordinate)
            .or_else(|| symbol.and_then(|symbol| index.project_of(symbol)));
        self.resolve_parts(symbol, project.map(ProjectIndex::root))
    }

    fn resolve_parts(
        &self,
        symbol: Option<SymbolKey>,
        project_root: Option<&str>,
    ) -> Result<ReaderRoute, ReaderEntryFault> {
        match self.intent {
            ReaderIntent::Docs => {
                if let Some(symbol) = symbol {
                    Ok(ReaderRoute::Docs {
                        subject: Subject::Declaration {
                            symbol,
                            coordinate: self.coordinate.clone(),
                        },
                    })
                } else if let Some(coordinate) = project_root {
                    Ok(ReaderRoute::Docs {
                        subject: Subject::Project {
                            coordinate: coordinate.to_owned(),
                        },
                    })
                } else {
                    Err(ReaderEntryFault::NotIndexed)
                }
            }
            ReaderIntent::Source => {
                let Some(symbol) = symbol else {
                    return Err(if project_root.is_some() {
                        ReaderEntryFault::DeclarationRequired
                    } else {
                        ReaderEntryFault::NotIndexed
                    });
                };
                Ok(ReaderRoute::Source {
                    subject: Subject::Declaration {
                        symbol,
                        coordinate: self.coordinate.clone(),
                    },
                    symbol,
                })
            }
            ReaderIntent::Code => project_root
                .map(|coordinate| ReaderRoute::Code {
                    subject: Subject::Outline {
                        coordinate: coordinate.to_owned(),
                    },
                })
                .ok_or(ReaderEntryFault::NotIndexed),
            ReaderIntent::Search => project_root
                .map(|scope| ReaderRoute::Search {
                    scope: Identity::parse(scope).name().to_owned(),
                })
                .ok_or(ReaderEntryFault::NotIndexed),
        }
    }
}

/// Returns where a click with these modifiers should open a link.
///
/// A plain click grows the tree: the page followed from this one becomes a
/// leaf under it, so the shape of an exploration is recorded as it happens.
/// The primary modifier reads in place instead — the same tab, one more
/// history step — and alt opens the leaf behind. A subject already open
/// anywhere is never opened twice; the click goes to the tab that has it.
pub(crate) fn modifier_target(modifiers: Modifiers) -> Target {
    if modifiers.alt {
        Target::Background
    } else if modifiers.secondary() {
        Target::Here
    } else {
        Target::Child
    }
}

/// What a tab has to draw right now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Content {
    /// Nothing has been opened in this tab yet.
    Blank,
    /// The browse page.
    Home,
    /// A declaration page.
    Page(Arc<Page>),
    /// A project page; its rows come from the index store.
    Project {
        /// Exact project coordinate.
        coordinate: String,
    },
    /// A registry package page; its facts come from the registry store.
    Package {
        /// Pinned package coordinate.
        coordinate: String,
    },
    /// A declaration outline, distinct from the docs/project page.
    Outline {
        /// Exact project coordinate.
        coordinate: String,
    },
    /// The subject could not be read.
    Faulted(Box<Fault>),
}

/// The stable identity of one tab for the life of the window.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct TabId(u64);

/// One reader tab.
pub(crate) struct Tab {
    id: TabId,
    parent: Option<TabId>,
    history: Vec<Subject>,
    cursor: usize,
    content: Content,
    surface: ReaderSurface,
    pending: Option<Identity>,
    scroll: ScrollHandle,
    generation: u64,
}

impl Tab {
    fn new(id: TabId, parent: Option<TabId>, subject: Subject) -> Self {
        let surface = match &subject {
            Subject::Home => ReaderSurface::Browse,
            Subject::Project { .. } => ReaderSurface::Project,
            Subject::Package { .. } => ReaderSurface::Package,
            Subject::Outline { .. } => ReaderSurface::Code,
            Subject::Declaration { .. } => ReaderSurface::Declaration,
        };
        Self {
            id,
            parent,
            history: vec![subject],
            cursor: 0,
            content: Content::Blank,
            surface,
            pending: None,
            scroll: ScrollHandle::new(),
            generation: 0,
        }
    }

    /// Returns what this tab is showing.
    pub(crate) const fn content(&self) -> &Content {
        &self.content
    }

    /// Returns the typed route owning this tab.
    pub(crate) const fn surface(&self) -> ReaderSurface {
        self.surface
    }

    /// Returns the subject at the history cursor.
    pub(crate) fn subject(&self) -> Option<&Subject> {
        self.history.get(self.cursor)
    }

    /// Returns the identity currently being loaded, if any.
    pub(crate) const fn pending(&self) -> Option<&Identity> {
        self.pending.as_ref()
    }

    /// Returns the scroll handle for this tab's reader body.
    pub(crate) const fn scroll(&self) -> &ScrollHandle {
        &self.scroll
    }

    /// Returns the tab title.
    pub(crate) fn title(&self) -> String {
        self.subject()
            .map_or_else(|| "Untitled".to_owned(), Subject::title)
    }

    /// Returns whether there is anywhere to go back to.
    pub(crate) const fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    /// Returns whether there is anywhere to go forward to.
    pub(crate) fn can_go_forward(&self) -> bool {
        self.cursor.saturating_add(1) < self.history.len()
    }

    fn push(&mut self, subject: Subject) {
        self.history.truncate(self.cursor.saturating_add(1));
        if self.history.last() == Some(&subject) {
            return;
        }
        self.history.push(subject);
        self.cursor = self.history.len().saturating_sub(1);
    }
}

/// One row of the tab tree, in the order a tree panel draws them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeRow {
    /// The tab.
    pub(crate) id: TabId,
    /// How many ancestors the tab has.
    pub(crate) depth: usize,
    /// The tab title.
    pub(crate) title: String,
    /// The subject, for its glyph.
    pub(crate) subject: Option<Subject>,
    /// Whether this is the active tab.
    pub(crate) active: bool,
    /// Whether the tab is still loading its subject.
    pub(crate) loading: bool,
}

/// A small anchored card describing one declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HoverCard {
    identity: Identity,
    kind: Option<DeclarationKind>,
    signature: String,
    summary: Option<String>,
}

impl HoverCard {
    /// Returns the declaration's identity.
    pub(crate) const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the declaration's typed kind.
    pub(crate) const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the signature line.
    pub(crate) fn signature(&self) -> &str {
        &self.signature
    }

    /// Returns the first sentence of documentation, when there is one.
    pub(crate) fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }
}

/// A hover in progress or resolved.
#[derive(Clone, Debug)]
pub(crate) struct Hover {
    symbol: SymbolKey,
    anchor: Point<Pixels>,
    card: Option<HoverCard>,
}

impl Hover {
    /// Returns where the card should be anchored.
    pub(crate) const fn anchor(&self) -> Point<Pixels> {
        self.anchor
    }

    /// Returns the card, once it has arrived.
    pub(crate) const fn card(&self) -> Option<&HoverCard> {
        self.card.as_ref()
    }
}

/// Every open tab and the caches behind them.
pub(crate) struct DocumentStore {
    endpoint: Endpoint,
    workspace: Entity<WorkspaceStore>,
    index: Entity<IndexStore>,
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: u64,
    pages: PageCache,
    hover: Option<Hover>,
    cards: Vec<(String, HoverCard)>,
    hover_task: Option<Task<()>>,
}

impl EventEmitter<DocumentEvent> for DocumentStore {}

impl DocumentStore {
    /// Creates an empty reader bound to one workspace and its index.
    pub(crate) const fn new(
        endpoint: Endpoint,
        workspace: Entity<WorkspaceStore>,
        index: Entity<IndexStore>,
    ) -> Self {
        Self {
            endpoint,
            workspace,
            index,
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            pages: PageCache::new(),
            hover: None,
            cards: Vec::new(),
            hover_task: None,
        }
    }

    /// Returns the active tab.
    pub(crate) fn tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.find(id))
    }

    /// Returns one tab by identity.
    pub(crate) fn find(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    fn find_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    /// Returns the hover card state.
    pub(crate) const fn hover(&self) -> Option<&Hover> {
        self.hover.as_ref()
    }

    /// Returns a value that changes whenever the shown page changes.
    ///
    /// The reader keys its fade on this, so a page that arrives fades in once
    /// and a page that is merely re-rendered does not flicker.
    pub(crate) fn generation(&self) -> u64 {
        let tab = self.active.map_or(0, |id| id.0);
        self.tab().map_or(tab, |open| {
            tab.wrapping_mul(1024).wrapping_add(open.generation)
        })
    }

    /// Creates a typed package handoff from the live document and index state.
    pub(crate) fn reader_entry(
        &self,
        coordinate: &str,
        intent: ReaderIntent,
        cx: &Context<Self>,
    ) -> Option<ReaderEntry> {
        (!coordinate.trim().is_empty()).then(|| ReaderEntry {
            coordinate: coordinate.to_owned(),
            intent,
            document_generation: self.generation(),
            index_version: self.index.read(cx).version(),
        })
    }

    /// Resolves a typed package handoff through the real document store.
    ///
    /// The caller receives `false` when its route was produced against stale
    /// tab or index state, so a package surface never falls back to a
    /// misleading unavailable panel.
    pub(crate) fn open_reader_entry(
        &mut self,
        entry: &ReaderEntry,
        target: Target,
        cx: &mut Context<Self>,
    ) -> Result<ReaderRoute, ReaderEntryFault> {
        let current_generation = self.generation();
        let current_version = self.index.read(cx).version();
        if !entry.is_current(current_generation, current_version) {
            return Err(ReaderEntryFault::Stale);
        }
        let route = entry.resolve(&self.index.read(cx));
        if let Ok(route) = &route {
            match route {
                ReaderRoute::Docs { subject } => {
                    self.open(subject.clone(), target, cx);
                    self.set_active_surface(ReaderSurface::Docs);
                }
                ReaderRoute::Source { subject, .. } => {
                    self.open(subject.clone(), target, cx);
                    self.set_active_surface(ReaderSurface::Source);
                }
                ReaderRoute::Code { subject } => {
                    self.open(subject.clone(), target, cx);
                    self.set_active_surface(ReaderSurface::Code);
                }
                ReaderRoute::Search { .. } => {
                    self.set_active_surface(ReaderSurface::CodeSearch);
                }
            }
        }
        route
    }

    /// Changes the typed route of the active tab after a route adapter has
    /// selected a capability within an already opened subject.
    pub(crate) fn set_active_surface(&mut self, surface: ReaderSurface) {
        if let Some(id) = self.active
            && let Some(tab) = self.find_mut(id)
        {
            tab.surface = surface;
        }
    }

    /// Returns the typed route of the active tab, or browse before a tab is
    /// created.
    pub(crate) fn active_surface(&self) -> ReaderSurface {
        self.tab().map_or(ReaderSurface::Browse, Tab::surface)
    }

    /// Returns the tabs as a tree, parents before children, in creation order.
    pub(crate) fn tree(&self) -> Vec<TreeRow> {
        let mut rows = Vec::with_capacity(self.tabs.len());
        for root in self.tabs.iter().filter(|tab| self.is_root(tab)) {
            self.walk(root, 0, &mut rows);
        }
        rows
    }

    fn is_root(&self, tab: &Tab) -> bool {
        tab.parent.is_none_or(|parent| self.find(parent).is_none())
    }

    fn walk(&self, tab: &Tab, depth: usize, rows: &mut Vec<TreeRow>) {
        if depth > 32 {
            return;
        }
        rows.push(TreeRow {
            id: tab.id,
            depth,
            title: tab.title(),
            subject: tab.subject().cloned(),
            active: self.active == Some(tab.id),
            loading: tab.pending.is_some(),
        });
        for child in self
            .tabs
            .iter()
            .filter(|other| other.parent == Some(tab.id))
        {
            self.walk(child, depth.saturating_add(1), rows);
        }
    }

    /// Opens one declaration by key, resolving its coordinate from the index.
    ///
    /// A key the shelf does not hold — a link into a package that is not
    /// indexed — opens as a fault that says so, rather than as a request the
    /// engine would refuse or a page titled by sixty-four hex digits.
    pub(crate) fn open_symbol(
        &mut self,
        symbol: SymbolKey,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        let coordinate = self
            .index
            .read(cx)
            .coordinate_of(symbol)
            .map(ToOwned::to_owned);
        if let Some(coordinate) = coordinate {
            self.open(Subject::Declaration { symbol, coordinate }, target, cx);
            return;
        }
        let encoded = encode_id(symbol.as_bytes());
        let id = self.place(
            Subject::Declaration {
                symbol,
                coordinate: encoded.clone(),
            },
            target,
            cx,
        );
        if let Some(tab) = self.find_mut(id) {
            tab.pending = None;
            tab.content = Content::Faulted(Box::new(not_on_shelf(&encoded)));
        }
        cx.emit(DocumentEvent::Navigated);
        cx.notify();
    }

    /// Opens one subject, replacing or adding a tab.
    pub(crate) fn open(&mut self, subject: Subject, target: Target, cx: &mut Context<Self>) {
        let settled = target != Target::Here
            && self.showing(&subject).is_some_and(|id| {
                self.find(id)
                    .is_some_and(|tab| tab.content != Content::Blank)
            });
        let id = self.place(subject, target, cx);
        if settled {
            return;
        }
        self.load(id, cx);
    }

    /// Opens the browse page: the tab that already shows it, or a new root.
    ///
    /// Home is the trunk of the tree. It is never opened twice and never
    /// nested under a page, because "back to the start" should land on the
    /// same tab every time.
    pub(crate) fn open_home(&mut self, cx: &mut Context<Self>) {
        if let Some(existing) = self.showing(&Subject::Home) {
            self.activate(existing, cx);
            return;
        }
        let id = self.spawn(None, Subject::Home);
        self.active = Some(id);
        cx.emit(DocumentEvent::TabsChanged);
        self.load(id, cx);
    }

    /// Returns the tab whose current subject is this one, if any.
    fn showing(&self, subject: &Subject) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|tab| tab.subject() == Some(subject))
            .map(|tab| tab.id)
    }

    /// Records a subject in a tab and returns which tab holds it.
    fn place(&mut self, subject: Subject, target: Target, cx: &mut Context<Self>) -> TabId {
        let already = (target != Target::Here)
            .then(|| self.showing(&subject))
            .flatten();
        let id = match (target, already, self.active) {
            (_, Some(existing), _) => {
                if target == Target::Child {
                    self.active = Some(existing);
                }
                existing
            }
            (Target::Here, None, Some(active)) if self.find(active).is_some() => {
                if let Some(tab) = self.find_mut(active) {
                    tab.push(subject);
                }
                active
            }
            (Target::Here, None, _) => {
                let id = self.spawn(None, subject);
                self.active = Some(id);
                id
            }
            (Target::Child, None, active) => {
                let id = self.spawn(active, subject);
                self.active = Some(id);
                id
            }
            (Target::Background, None, active) => self.spawn(active, subject),
        };
        cx.emit(DocumentEvent::TabsChanged);
        cx.notify();
        id
    }

    fn spawn(&mut self, parent: Option<TabId>, subject: Subject) -> TabId {
        let id = TabId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.tabs.push(Tab::new(id, parent, subject));
        id
    }

    /// Makes one tab active.
    pub(crate) fn activate(&mut self, id: TabId, cx: &mut Context<Self>) {
        if self.find(id).is_none() || self.active == Some(id) {
            return;
        }
        self.active = Some(id);
        cx.emit(DocumentEvent::TabsChanged);
        cx.notify();
    }

    /// Makes the tab at one tree position active, counting from zero.
    pub(crate) fn activate_nth(&mut self, at: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.tree().get(at) {
            self.activate(row.id, cx);
        }
    }

    /// Activates the tab one step before or after the active one, in tree order.
    pub(crate) fn activate_neighbour(&mut self, forward: bool, cx: &mut Context<Self>) {
        let rows = self.tree();
        let Some(at) = rows.iter().position(|row| row.active) else {
            return;
        };
        let next = if forward {
            at.saturating_add(1).min(rows.len().saturating_sub(1))
        } else {
            at.saturating_sub(1)
        };
        if let Some(row) = rows.get(next) {
            self.activate(row.id, cx);
        }
    }

    /// Closes one tab, handing its children to its parent.
    ///
    /// The tab activated next is the parent when there is one — the page the
    /// closed branch was opened from is where a reader's attention came from
    /// — and otherwise the nearest remaining row in tree order.
    pub(crate) fn close(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(closing) = self.find(id) else {
            return;
        };
        let parent = closing.parent.filter(|parent| self.find(*parent).is_some());
        let order = self.tree();
        let position = order.iter().position(|row| row.id == id).unwrap_or(0);
        for tab in &mut self.tabs {
            if tab.parent == Some(id) {
                tab.parent = parent;
            }
        }
        self.tabs.retain(|tab| tab.id != id);
        if self.active == Some(id) {
            self.active = parent.or_else(|| {
                order
                    .iter()
                    .filter(|row| row.id != id)
                    .nth(position.saturating_sub(1))
                    .or_else(|| order.iter().find(|row| row.id != id))
                    .map(|row| row.id)
            });
        }
        cx.emit(DocumentEvent::TabsChanged);
        cx.notify();
    }

    /// Closes the active tab.
    pub(crate) fn close_active(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = self.active {
            self.close(active, cx);
        }
    }

    /// Closes every tab whose subject lives in one project.
    ///
    /// A project taken off the shelf must take its pages with it: a reader
    /// left looking at a fault for something they just removed would be
    /// looking at an error they caused on purpose.
    pub(crate) fn close_project(&mut self, root: &str, cx: &mut Context<Self>) {
        let doomed: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|tab| {
                tab.subject()
                    .and_then(Subject::project_root)
                    .is_some_and(|held| held == root)
            })
            .map(|tab| tab.id)
            .collect();
        for id in doomed {
            self.close(id, cx);
        }
        self.pages.retain(|(key, _)| !key.starts_with(root));
    }

    /// Walks the active tab's history back one step.
    pub(crate) fn back(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active else {
            return;
        };
        let Some(tab) = self.find_mut(id) else {
            return;
        };
        if !tab.can_go_back() {
            return;
        }
        tab.cursor = tab.cursor.saturating_sub(1);
        cx.emit(DocumentEvent::Navigated);
        self.load(id, cx);
    }

    /// Walks the active tab's history forward one step.
    pub(crate) fn forward(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active else {
            return;
        };
        let Some(tab) = self.find_mut(id) else {
            return;
        };
        if !tab.can_go_forward() {
            return;
        }
        tab.cursor = tab.cursor.saturating_add(1);
        cx.emit(DocumentEvent::Navigated);
        self.load(id, cx);
    }

    /// Returns whether the active tab can walk back.
    pub(crate) fn can_go_back(&self) -> bool {
        self.tab().is_some_and(Tab::can_go_back)
    }

    /// Returns whether the active tab can walk forward.
    pub(crate) fn can_go_forward(&self) -> bool {
        self.tab().is_some_and(Tab::can_go_forward)
    }

    /// Re-reads whatever the active tab is showing.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active else {
            return;
        };
        if let Some(coordinate) = self
            .find(id)
            .and_then(Tab::subject)
            .map(|subject| subject.coordinate().to_owned())
        {
            self.pages.retain(|(key, _)| *key != coordinate);
        }
        self.load(id, cx);
    }
}

/// Loading pages.
impl DocumentStore {
    fn load(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(subject) = self.find(id).and_then(Tab::subject).cloned() else {
            return;
        };
        match subject {
            Subject::Home => self.show_static(id, Content::Home, cx),
            Subject::Project { coordinate } => {
                self.show_static(id, Content::Project { coordinate }, cx);
            }
            Subject::Outline { coordinate } => {
                self.show_static(id, Content::Outline { coordinate }, cx);
            }
            Subject::Package { coordinate } => {
                self.show_static(id, Content::Package { coordinate }, cx);
            }
            Subject::Declaration { symbol, coordinate } => {
                self.show_declaration(id, symbol, coordinate, cx);
            }
        }
    }

    fn show_static(&mut self, id: TabId, content: Content, cx: &mut Context<Self>) {
        if let Some(tab) = self.find_mut(id) {
            tab.pending = None;
            tab.content = content;
            tab.generation = tab.generation.saturating_add(1);
        }
        cx.emit(DocumentEvent::Navigated);
        cx.notify();
    }

    fn show_declaration(
        &mut self,
        id: TabId,
        symbol: SymbolKey,
        coordinate: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(cached) = self.cached(&coordinate) {
            self.show_static(id, Content::Page(cached), cx);
            return;
        }
        let generation = self.begin(id, &coordinate, cx);
        let index_version = self.index.read(cx).version();
        let endpoint = self.endpoint.clone();
        let wanted = coordinate.clone();
        cx.spawn(async move |this, cx| {
            let parts = cx
                .background_spawn(async move { fetch(&endpoint, symbol, &wanted) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish(
                    id,
                    generation,
                    index_version,
                    symbol,
                    &coordinate,
                    parts,
                    cx,
                );
            });
        })
        .detach();
    }

    fn begin(&mut self, id: TabId, coordinate: &str, cx: &mut Context<Self>) -> u64 {
        let Some(tab) = self.find_mut(id) else {
            return 0;
        };
        tab.generation = tab.generation.saturating_add(1);
        tab.pending = Some(Identity::parse(coordinate));
        cx.emit(DocumentEvent::Navigated);
        cx.notify();
        tab.generation
    }

    fn finish(
        &mut self,
        id: TabId,
        generation: u64,
        index_version: [u8; 32],
        symbol: SymbolKey,
        coordinate: &str,
        parts: Parts,
        cx: &mut Context<Self>,
    ) {
        let stale = self.find(id).is_none_or(|tab| {
            tab.generation != generation
                || tab
                    .subject()
                    .is_none_or(|subject| subject.coordinate() != coordinate)
        }) || self.index.read(cx).version() != index_version;
        if stale {
            return;
        }
        let content = match self.assemble(symbol, coordinate, parts, cx) {
            Ok(page) => {
                let page = Arc::new(page);
                self.remember(coordinate.to_owned(), Arc::clone(&page));
                Content::Page(page)
            }
            Err(fault) => Content::Faulted(Box::new(fault)),
        };
        if let Some(tab) = self.find_mut(id) {
            tab.pending = None;
            tab.content = content;
        }
        cx.emit(DocumentEvent::Navigated);
        cx.notify();
    }

    fn assemble(
        &self,
        symbol: SymbolKey,
        coordinate: &str,
        parts: Parts,
        cx: &Context<Self>,
    ) -> Result<Page, Fault> {
        let document = parts.document?;
        let workspace = self.workspace.read(cx);
        let index = self.index.read(cx);
        let members = workspace.page_rows(symbol);
        let relations = parts.relations.unwrap_or_default();
        let page = page_from_document(coordinate, &document, &members, &relations, parts.notes);
        let near = index
            .project_of(symbol)
            .map(|project| project.root().to_owned());
        let resolver = |name: &str| index.resolve_name(name, near.as_deref());
        Ok(match page.signature().cloned() {
            Some(signature) => page.with_signature(signature.resolve_types(resolver)),
            None => page,
        })
    }

    fn cached(&self, coordinate: &str) -> Option<Arc<Page>> {
        self.pages.get(coordinate)
    }

    fn remember(&mut self, coordinate: String, page: Arc<Page>) {
        self.pages.insert(coordinate, page);
    }
}

/// Hover cards.
impl DocumentStore {
    /// Begins a hover after the pointer has rested.
    pub(crate) fn hover_over(
        &mut self,
        symbol: SymbolKey,
        anchor: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self
            .hover
            .as_ref()
            .is_some_and(|hover| hover.symbol == symbol)
        {
            return;
        }
        let Some(coordinate) = self
            .index
            .read(cx)
            .coordinate_of(symbol)
            .map(ToOwned::to_owned)
        else {
            return;
        };
        let card = self
            .cards
            .iter()
            .find(|(key, _)| *key == coordinate)
            .map(|(_, card)| card.clone());
        self.hover = Some(Hover {
            symbol,
            anchor,
            card: card.clone(),
        });
        cx.emit(DocumentEvent::HoverChanged);
        cx.notify();
        if card.is_some() {
            self.hover_task = None;
            return;
        }
        let endpoint = self.endpoint.clone();
        let kind = self
            .index
            .read(cx)
            .entry(symbol)
            .and_then(super::index::Entry::kind);
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            let outcome = cx
                .background_spawn(async move {
                    super::service::run(endpoint.path(), &Request::DocumentSymbol { symbol })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.install_card(symbol, coordinate, kind, outcome, cx);
            });
        }));
    }

    /// Clears any hover state.
    pub(crate) fn clear_hover(&mut self, cx: &mut Context<Self>) {
        self.hover_task = None;
        if self.hover.is_none() {
            return;
        }
        self.hover = None;
        cx.emit(DocumentEvent::HoverChanged);
        cx.notify();
    }

    fn install_card(
        &mut self,
        symbol: SymbolKey,
        coordinate: String,
        kind: Option<DeclarationKind>,
        outcome: Result<Outcome, backend_client::ClientError>,
        cx: &mut Context<Self>,
    ) {
        let identity = Identity::parse(&coordinate);
        let card = match outcome {
            Ok(Outcome::Document(document)) => {
                let page = page_from_document(&coordinate, &document, &[], &[], Vec::new());
                HoverCard {
                    kind: page.kind().or(kind),
                    signature: page.signature().map_or_else(String::new, |signature| {
                        crate::ui::specimen::preview(signature, CARD_PREVIEW)
                    }),
                    summary: crate::ui::prose::summary(page.prose())
                        .filter(|summary| !crate::ui::prose::tautological(summary, &identity)),
                    identity,
                }
            }
            _ => HoverCard {
                kind,
                signature: String::new(),
                summary: None,
                identity,
            },
        };
        self.cards.retain(|(key, _)| *key != coordinate);
        self.cards.push((coordinate, card.clone()));
        while self.cards.len() > HOVER_CACHE {
            self.cards.remove(0);
        }
        if let Some(hover) = self.hover.as_mut()
            && hover.symbol == symbol
        {
            hover.card = Some(card);
            cx.emit(DocumentEvent::HoverChanged);
            cx.notify();
        }
    }
}

/// The replies a declaration page is assembled from.
struct Parts {
    document: Result<Box<backend_library::Document>, Fault>,
    relations: Option<Vec<Row>>,
    notes: Vec<Fault>,
}

fn fetch(endpoint: &Endpoint, symbol: SymbolKey, coordinate: &str) -> Parts {
    let operand = coordinate_operand(coordinate);
    let document = match super::service::run(endpoint.path(), &Request::DocumentSymbol { symbol }) {
        Ok(Outcome::Document(document)) => Ok(document),
        Ok(_) => Err(wrong_shape(operand.clone(), "a document")),
        Err(error) => Err(Fault::from_client_error(&error, operand.clone())),
    };
    let mut notes = Vec::new();
    let mut relations: Option<Vec<Row>> = None;
    for request in [Request::Graph { symbol }, Request::Related { symbol }] {
        match rows_of(endpoint, &request) {
            Ok(rows) => relations.get_or_insert_with(Vec::new).extend(rows),
            Err(fault) => notes.push(fault.about(operand.clone())),
        }
    }
    if let Some(rows) = relations.as_mut() {
        rows.sort_by(|left, right| left.label.cmp(&right.label));
        rows.dedup_by(|left, right| left.id == right.id);
    }
    Parts {
        document,
        relations,
        notes,
    }
}

fn rows_of(endpoint: &Endpoint, request: &Request) -> Result<Vec<Row>, Fault> {
    match super::service::run(endpoint.path(), request) {
        Ok(Outcome::Rows(page)) => Ok(page.into_rows()),
        Ok(_) => Err(wrong_shape(Operand::Whole, "a page of rows")),
        Err(error) => Err(Fault::from_client_error(&error, Operand::Whole)),
    }
}

/// Returns the fault shown for a link into a package the shelf does not hold.
fn not_on_shelf(encoded: &str) -> Fault {
    Fault::new(
        FaultSlug::NotFound,
        Operand::Coordinate(Coordinate::new(encoded)),
        Cause::new(
            CauseSlug::Absent,
            "this link points into a package that is not on the shelf; add the package and the link will resolve",
        ),
        Affordance::None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::symbol_key;
    use backend_present::{LineNumber, PackagePath, Source, SourceSite, Truncation};

    #[test]
    fn page_cache_reuses_the_same_page_handle_across_frames() {
        let site = SourceSite::new(
            PackagePath::new("src/lib.rs"),
            LineNumber::new(1).expect("one-based line"),
        );
        let page = Arc::new(Page::new(
            Identity::parse("/abs/polyglot::src/lib.rs:1::ferris"),
            None,
            Source::Captured {
                lines: Box::new([]),
                site,
                truncation: Truncation::Complete,
            },
        ));
        let mut cache = PageCache::default();
        cache.insert(
            "/abs/polyglot::src/lib.rs:1::ferris".to_owned(),
            Arc::clone(&page),
        );

        let first = cache
            .get("/abs/polyglot::src/lib.rs:1::ferris")
            .expect("cached page");
        let second = cache
            .get("/abs/polyglot::src/lib.rs:1::ferris")
            .expect("cached page");
        assert!(Arc::ptr_eq(&page, &first));
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn reader_entry_resolves_each_intent_to_a_distinct_store_route() {
        let symbol = symbol_key("/abs/polyglot::src/lib.rs:1::ferris");
        let coordinate = "/abs/polyglot::src/lib.rs:1::ferris".to_owned();
        let route = |intent| {
            ReaderEntry {
                coordinate: coordinate.clone(),
                intent,
                document_generation: 7,
                index_version: [3; 32],
            }
            .resolve_parts(Some(symbol), Some("/abs/polyglot"))
            .expect("indexed route")
        };

        assert!(matches!(
            route(ReaderIntent::Docs),
            ReaderRoute::Docs { .. }
        ));
        assert!(matches!(
            route(ReaderIntent::Source),
            ReaderRoute::Source { .. }
        ));
        assert!(matches!(
            route(ReaderIntent::Code),
            ReaderRoute::Code { .. }
        ));
        assert_eq!(
            route(ReaderIntent::Search),
            ReaderRoute::Search {
                scope: "polyglot".to_owned()
            }
        );
        assert_ne!(route(ReaderIntent::Docs), route(ReaderIntent::Source));
        assert_ne!(route(ReaderIntent::Docs), route(ReaderIntent::Code));
        assert_ne!(route(ReaderIntent::Code), route(ReaderIntent::Search));

        let entry = ReaderEntry {
            coordinate,
            intent: ReaderIntent::Docs,
            document_generation: 7,
            index_version: [3; 32],
        };
        assert!(entry.is_current(7, [3; 32]));
        assert!(!entry.is_current(8, [3; 32]));
        assert!(!entry.is_current(7, [4; 32]));
    }

    #[test]
    fn source_route_rejects_a_project_root_without_a_declaration() {
        let entry = ReaderEntry {
            coordinate: "/abs/polyglot".to_owned(),
            intent: ReaderIntent::Source,
            document_generation: 1,
            index_version: [0; 32],
        };
        assert_eq!(
            entry.resolve_parts(None, Some("/abs/polyglot")),
            Err(ReaderEntryFault::DeclarationRequired)
        );
    }

    #[test]
    fn reader_coverage_keeps_absent_and_authority_failures_distinct() {
        let site = SourceSite::new(
            PackagePath::new("src/lib.rs"),
            LineNumber::new(3).expect("one-based line"),
        );
        let absent = Source::Absent {
            fault: Fault::new(
                FaultSlug::NotFound,
                Operand::Whole,
                Cause::new(CauseSlug::Absent, "source was not captured"),
                Affordance::None,
            ),
        };
        let sited = Source::Sited {
            site,
            fault: Fault::new(
                FaultSlug::SourceUnavailable,
                Operand::Whole,
                Cause::new(
                    CauseSlug::NotResident,
                    "semantic source authority unavailable",
                ),
                Affordance::None,
            ),
        };
        assert_eq!(
            ReaderCoverageFault::for_source(&absent),
            Some(ReaderCoverageFault::AbsentCapture)
        );
        assert_eq!(
            ReaderCoverageFault::for_source(&sited),
            Some(ReaderCoverageFault::SemanticAuthorityUnavailable)
        );
        assert_ne!(
            ReaderCoverageFault::for_source(&absent),
            ReaderCoverageFault::for_source(&sited)
        );
    }
}
