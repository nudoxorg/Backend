//! Reader tabs, per-tab history, the page cache, and hover cards.
//! A tab keeps showing the page it has until the next one has fully arrived.
//! Nothing here flashes: loading is a reserved state, not an empty screen.
//!
//! The cache is keyed by coordinate and evicted least-recently-used, which
//! makes back and forward instant for the path a reader actually walked while
//! keeping memory bounded on a shelf with ten thousand declarations. Hover
//! cards share the same discipline on a smaller budget: a card is requested
//! only after the pointer has rested, and the answer is remembered so the
//! second hover costs nothing.

use super::events::DocumentEvent;
use super::service::{Endpoint, Outcome, Request};
use super::workspace::WorkspaceStore;
use crate::presentation::fault::{self, Fault, Operand};
use crate::presentation::identity::Identity;
use crate::presentation::page::{Page, RelationGroup, RelationLane};
use backend_library::{DeclarationKind, RowId, SymbolKey};
use gpui::AppContext as _;
use gpui::{Entity, EventEmitter, Point, Pixels, ScrollHandle, Task};
use gpui::Context;
use std::time::Duration;

/// How many pages are remembered across tabs.
const PAGE_CACHE: usize = 64;

/// How many hover cards are remembered.
const HOVER_CACHE: usize = 128;

/// How long the pointer must rest before a hover card is requested.
const HOVER_DELAY: Duration = Duration::from_millis(350);

/// What a tab is showing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Subject {
    /// One declaration, addressed by its admitted identity.
    Declaration {
        /// Admitted declaration key.
        symbol: SymbolKey,
        /// Exact producer coordinate.
        coordinate: String,
    },
    /// One project, addressed by its coordinate.
    Project {
        /// Exact project coordinate.
        coordinate: String,
    },
}

impl Subject {
    /// Returns the coordinate this subject is keyed by.
    pub(crate) fn coordinate(&self) -> &str {
        match self {
            Self::Declaration { coordinate, .. } | Self::Project { coordinate } => coordinate,
        }
    }

    /// Returns the readable identity of this subject.
    pub(crate) fn identity(&self) -> Identity {
        Identity::parse(self.coordinate())
    }
}

/// Where a newly opened subject should land.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    /// Replace what the active tab is showing.
    Here,
    /// Open a new tab and make it active.
    NewTab,
    /// Open a new tab behind the active one.
    Background,
}

/// What a tab has to draw right now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Content {
    /// Nothing has been opened in this tab yet.
    Blank,
    /// A declaration page.
    Page(Box<Page>),
    /// A project page; its rows come from the workspace store.
    Project {
        /// Exact project coordinate.
        coordinate: String,
    },
    /// The subject could not be read.
    Faulted(Box<Fault>),
}

/// One reader tab.
pub(crate) struct Tab {
    history: Vec<Subject>,
    cursor: usize,
    content: Content,
    pending: Option<Identity>,
    scroll: ScrollHandle,
    generation: u64,
}

impl Tab {
    fn new(subject: Subject) -> Self {
        Self {
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

    /// Returns the tab strip label.
    pub(crate) fn title(&self) -> String {
        self.subject()
            .map_or_else(|| "Untitled".to_owned(), |subject| {
                subject.identity().name().to_owned()
            })
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
    /// Returns the declaration being described.
    pub(crate) const fn symbol(&self) -> SymbolKey {
        self.symbol
    }

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
    tabs: Vec<Tab>,
    active: usize,
    pages: Vec<(String, Page)>,
    hover: Option<Hover>,
    cards: Vec<(String, HoverCard)>,
    loading: Vec<Task<()>>,
    hover_task: Option<Task<()>>,
}

impl EventEmitter<DocumentEvent> for DocumentStore {}

impl DocumentStore {
    /// Creates an empty reader bound to one workspace.
    pub(crate) const fn new(endpoint: Endpoint, workspace: Entity<WorkspaceStore>) -> Self {
        Self {
            endpoint,
            workspace,
            tabs: Vec::new(),
            active: 0,
            pages: Vec::new(),
            hover: None,
            cards: Vec::new(),
            loading: Vec::new(),
            hover_task: None,
        }
    }

    /// Returns every open tab.
    pub(crate) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Returns the active tab index.
    pub(crate) const fn active(&self) -> usize {
        self.active
    }

    /// Returns the active tab.
    pub(crate) fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// Returns the hover card state.
    pub(crate) const fn hover(&self) -> Option<&Hover> {
        self.hover.as_ref()
    }

    /// Returns whether nothing is open.
    pub(crate) fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Opens one subject, replacing or adding a tab.
    pub(crate) fn open(&mut self, subject: Subject, target: Target, cx: &mut Context<Self>) {
        match target {
            Target::Here if !self.tabs.is_empty() => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.push(subject);
                }
            }
            Target::Background => {
                self.tabs.push(Tab::new(subject));
            }
            _ => {
                self.tabs.push(Tab::new(subject));
                self.active = self.tabs.len().saturating_sub(1);
            }
        }
        let index = match target {
            Target::Background => self.tabs.len().saturating_sub(1),
            _ => self.active,
        };
        cx.emit(DocumentEvent::TabsChanged);
        cx.notify();
        self.load(index, cx);
    }

    /// Makes one tab active.
    pub(crate) fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        cx.emit(DocumentEvent::TabsChanged);
        cx.notify();
    }

    /// Closes one tab, activating its neighbour.
    pub(crate) fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        cx.emit(DocumentEvent::TabsChanged);
        cx.notify();
    }

    /// Walks the active tab's history back one step.
    pub(crate) fn back(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if !tab.can_go_back() {
            return;
        }
        tab.cursor = tab.cursor.saturating_sub(1);
        let index = self.active;
        cx.emit(DocumentEvent::Navigated);
        self.load(index, cx);
    }

    /// Walks the active tab's history forward one step.
    pub(crate) fn forward(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if !tab.can_go_forward() {
            return;
        }
        tab.cursor = tab.cursor.saturating_add(1);
        let index = self.active;
        cx.emit(DocumentEvent::Navigated);
        self.load(index, cx);
    }

    /// Re-reads whatever the active tab is showing.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let index = self.active;
        if let Some(tab) = self.tabs.get_mut(index) {
            if let Some(coordinate) = tab.subject().map(|subject| subject.coordinate().to_owned()) {
                self.pages.retain(|(key, _)| *key != coordinate);
            }
        }
        self.load(index, cx);
    }
}

/// Loading pages.
impl DocumentStore {
    fn load(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(subject) = self
            .tabs
            .get(index)
            .and_then(Tab::subject)
            .cloned()
        else {
            return;
        };
        match subject {
            Subject::Project { coordinate } => self.show_project(index, coordinate, cx),
            Subject::Declaration { symbol, coordinate } => {
                self.show_declaration(index, symbol, coordinate, cx);
            }
        }
    }

    fn show_project(&mut self, index: usize, coordinate: String, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.pending = None;
            tab.content = Content::Project { coordinate };
        }
        cx.emit(DocumentEvent::Navigated);
        cx.notify();
    }

    fn show_declaration(
        &mut self,
        index: usize,
        symbol: SymbolKey,
        coordinate: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(cached) = self.cached(&coordinate) {
            if let Some(tab) = self.tabs.get_mut(index) {
                tab.pending = None;
                tab.content = Content::Page(Box::new(cached));
            }
            cx.emit(DocumentEvent::Navigated);
            cx.notify();
            return;
        }
        let generation = self.begin(index, &coordinate, cx);
        let base = self.base_page(symbol, &coordinate, cx);
        let endpoint = self.endpoint.clone();
        let task = cx.spawn(async move |this, cx| {
            let parts = cx
                .background_spawn(async move { fetch(&endpoint, symbol, &coordinate) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish(index, generation, base, parts, cx);
            });
        });
        self.loading.push(task);
    }

    fn begin(&mut self, index: usize, coordinate: &str, cx: &mut Context<Self>) -> u64 {
        let Some(tab) = self.tabs.get_mut(index) else {
            return 0;
        };
        tab.generation = tab.generation.saturating_add(1);
        tab.pending = Some(Identity::parse(coordinate));
        cx.emit(DocumentEvent::Navigated);
        cx.notify();
        tab.generation
    }

    fn base_page(&self, symbol: SymbolKey, coordinate: &str, cx: &Context<Self>) -> Page {
        let workspace = self.workspace.read(cx);
        workspace.row(RowId::Symbol(symbol)).map_or_else(
            || Page::from_row(&placeholder_row(coordinate)),
            |row| {
                let children = workspace.children_of(symbol);
                Page::from_row(row).with_members(&children)
            },
        )
    }

    fn finish(
        &mut self,
        index: usize,
        generation: u64,
        base: Page,
        parts: Parts,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if tab.generation != generation {
            return;
        }
        tab.pending = None;
        let content = match parts.into_page(base, &self.workspace, cx) {
            Ok(page) => {
                let coordinate = page.identity().coordinate().to_owned();
                self.remember(coordinate, page.clone());
                Content::Page(Box::new(page))
            }
            Err(fault) => Content::Faulted(Box::new(fault)),
        };
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.content = content;
        }
        cx.emit(DocumentEvent::Navigated);
        cx.notify();
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
        coordinate: String,
        anchor: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.hover.as_ref().is_some_and(|hover| hover.symbol == symbol) {
            return;
        }
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
        let base = self.hover_base(symbol, &coordinate, cx);
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            let outcome = cx
                .background_spawn(async move {
                    super::service::run(endpoint.path(), &Request::DocumentSymbol { symbol })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.install_card(symbol, coordinate, base, outcome, cx);
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

    fn hover_base(&self, symbol: SymbolKey, coordinate: &str, cx: &Context<Self>) -> Page {
        let workspace = self.workspace.read(cx);
        workspace
            .row(RowId::Symbol(symbol))
            .map_or_else(|| Page::from_row(&placeholder_row(coordinate)), Page::from_row)
    }

    fn install_card(
        &mut self,
        symbol: SymbolKey,
        coordinate: String,
        base: Page,
        outcome: Result<Outcome, backend_client::ClientError>,
        cx: &mut Context<Self>,
    ) {
        let page = match outcome {
            Ok(Outcome::Document(document)) => base.with_document(&document),
            _ => base,
        };
        let card = HoverCard {
            kind: page.kind(),
            signature: page.signature().preview(140),
            summary: page.summary(),
            identity: page.identity().clone(),
        };
        self.cards.retain(|(key, _)| *key != coordinate);
        self.cards.push((coordinate, card.clone()));
        while self.cards.len() > HOVER_CACHE {
            self.cards.remove(0);
        }
        if let Some(hover) = self.hover.as_mut() {
            if hover.symbol == symbol {
                hover.card = Some(card);
                cx.emit(DocumentEvent::HoverChanged);
                cx.notify();
            }
        }
    }
}

/// The three replies a declaration page is assembled from.
struct Parts {
    document: Result<Box<backend_library::Document>, backend_client::ClientError>,
    graph: Option<Vec<backend_library::Row>>,
    related: Option<Vec<backend_library::Row>>,
}

impl Parts {
    fn into_page(
        self,
        base: Page,
        workspace: &Entity<WorkspaceStore>,
        cx: &Context<DocumentStore>,
    ) -> Result<Page, Fault> {
        let document = self.document.map_err(|error| {
            fault::from_client(
                &error,
                Operand::Declaration {
                    spelling: base.identity().coordinate().to_owned(),
                },
            )
        })?;
        let symbol = base.symbol();
        let mut groups = Vec::new();
        if let Some(rows) = self.graph {
            groups.push(RelationGroup::new(RelationLane::Graph, &rows, symbol));
        }
        if let Some(rows) = self.related {
            groups.push(RelationGroup::new(RelationLane::Related, &rows, symbol));
        }
        let store = workspace.read(cx);
        let resolver = |name: &str| store.resolve_name(name);
        Ok(base
            .with_document(&document)
            .with_relations(groups)
            .resolve_signature(&resolver))
    }
}

fn fetch(endpoint: &Endpoint, symbol: SymbolKey, coordinate: &str) -> Parts {
    let document = match super::service::run(
        endpoint.path(),
        &Request::DocumentSymbol { symbol },
    ) {
        Ok(Outcome::Document(document)) => Ok(document),
        Ok(_) => Err(backend_client::ClientError::Protocol(format!(
            "desktop document reply changed shape for {coordinate}"
        ))),
        Err(error) => Err(error),
    };
    Parts {
        document,
        graph: rows_of(endpoint, &Request::Graph { symbol }),
        related: rows_of(endpoint, &Request::Related { symbol }),
    }
}

fn rows_of(endpoint: &Endpoint, request: &Request) -> Option<Vec<backend_library::Row>> {
    match super::service::run(endpoint.path(), request) {
        Ok(Outcome::Rows(page)) => Some(page.into_rows()),
        _ => None,
    }
}

fn placeholder_row(coordinate: &str) -> backend_library::Row {
    backend_library::Row::new(
        RowId::Object(backend_library::object_version(coordinate.as_bytes())),
        backend_library::Basis::new(
            backend_library::view_state_root(&[]),
            backend_library::object_version(&[]),
        ),
        coordinate,
    )
}
