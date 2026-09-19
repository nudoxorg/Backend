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
use super::index::IndexStore;
use super::service::{Endpoint, Outcome, Request};
use super::workspace::{WorkspaceStore, coordinate_operand};
use crate::presentation::fault::wrong_shape;
use backend_library::{DeclarationKind, Row, SymbolKey, encode_id};
use backend_present::{
    Affordance, Cause, CauseSlug, Coordinate, Fault, FaultSlug, Identity, Operand, Page,
    page_from_document,
};
use gpui::AppContext as _;
use gpui::Context;
use gpui::{Entity, EventEmitter, Modifiers, Pixels, Point, ScrollHandle, Task};
use std::time::Duration;

/// How many pages are remembered across tabs.
const PAGE_CACHE: usize = 64;

/// How many hover cards are remembered.
const HOVER_CACHE: usize = 128;

/// Character budget for a hover card's signature line.
const CARD_PREVIEW: usize = 140;

/// How long the pointer must rest before a hover card is requested.
const HOVER_DELAY: Duration = Duration::from_millis(350);

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
}

impl Subject {
    /// Returns the coordinate this subject is keyed by; empty for home.
    pub(crate) fn coordinate(&self) -> &str {
        match self {
            Self::Home => "",
            Self::Declaration { coordinate, .. }
            | Self::Project { coordinate }
            | Self::Package { coordinate } => coordinate,
        }
    }

    /// Returns the readable identity of this subject, when it has one.
    pub(crate) fn identity(&self) -> Option<Identity> {
        match self {
            Self::Home => None,
            Self::Declaration { coordinate, .. }
            | Self::Project { coordinate }
            | Self::Package { coordinate } => Some(Identity::parse(coordinate)),
        }
    }

    /// Returns the tab title.
    pub(crate) fn title(&self) -> String {
        match self {
            Self::Home => "Browse".to_owned(),
            Self::Declaration { .. } | Self::Project { .. } | Self::Package { .. } => self
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
            Self::Project { coordinate } => Some(coordinate.clone()),
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
    Page(Box<Page>),
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
    pending: Option<Identity>,
    scroll: ScrollHandle,
    generation: u64,
}

impl Tab {
    fn new(id: TabId, parent: Option<TabId>, subject: Subject) -> Self {
        Self {
            id,
            parent,
            history: vec![subject],
            cursor: 0,
            content: Content::Blank,
            pending: None,
            scroll: ScrollHandle::new(),
            generation: 0,
        }
    }

    /// Returns what this tab is showing.
    pub(crate) const fn content(&self) -> &Content {
        &self.content
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
    pages: Vec<(String, Page)>,
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
            pages: Vec::new(),
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
        self.tab()
            .map_or(tab, |open| tab.wrapping_mul(1024).wrapping_add(open.generation))
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
        for child in self.tabs.iter().filter(|other| other.parent == Some(tab.id)) {
            self.walk(child, depth.saturating_add(1), rows);
        }
    }

    /// Opens one declaration by key, resolving its coordinate from the index.
    ///
    /// A key the shelf does not hold — a link into a package that is not
    /// indexed — opens as a fault that says so, rather than as a request the
    /// engine would refuse or a page titled by sixty-four hex digits.
    pub(crate) fn open_symbol(&mut self, symbol: SymbolKey, target: Target, cx: &mut Context<Self>) {
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
            self.show_static(id, Content::Page(Box::new(cached)), cx);
            return;
        }
        let generation = self.begin(id, &coordinate, cx);
        let endpoint = self.endpoint.clone();
        let wanted = coordinate.clone();
        cx.spawn(async move |this, cx| {
            let parts = cx
                .background_spawn(async move { fetch(&endpoint, symbol, &wanted) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish(id, generation, symbol, &coordinate, parts, cx);
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
        symbol: SymbolKey,
        coordinate: &str,
        parts: Parts,
        cx: &mut Context<Self>,
    ) {
        let stale = self
            .find(id)
            .is_none_or(|tab| tab.generation != generation);
        if stale {
            return;
        }
        let content = match self.assemble(symbol, coordinate, parts, cx) {
            Ok(page) => {
                self.remember(coordinate.to_owned(), page.clone());
                Content::Page(Box::new(page))
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
        let near = index.project_of(symbol).map(|project| project.root().to_owned());
        let resolver = |name: &str| index.resolve_name(name, near.as_deref());
        Ok(match page.signature().cloned() {
            Some(signature) => page.with_signature(signature.resolve_types(resolver)),
            None => page,
        })
    }

    fn cached(&self, coordinate: &str) -> Option<Page> {
        self.pages
            .iter()
            .find(|(key, _)| key == coordinate)
            .map(|(_, page)| page.clone())
    }

    fn remember(&mut self, coordinate: String, page: Page) {
        self.pages.retain(|(key, _)| *key != coordinate);
        self.pages.push((coordinate, page));
        while self.pages.len() > PAGE_CACHE {
            self.pages.remove(0);
        }
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
        let kind = self.index.read(cx).entry(symbol).and_then(super::index::Entry::kind);
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
