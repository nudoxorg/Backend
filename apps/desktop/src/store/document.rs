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
//!
//! The page itself is assembled by [`backend_present::page_from_document`] —
//! the same function `backend page` and the MCP `backend.page` tool call — so
//! this store's job is only to decide *which* replies to ask for and when to
//! install them, never what they mean.

use super::events::DocumentEvent;
use super::service::{Endpoint, Outcome, Request};
use super::workspace::{WorkspaceStore, coordinate_operand};
use crate::presentation::fault::wrong_shape;
use backend_library::{DeclarationKind, Row, SymbolKey};
use backend_present::{Fault, Identity, Page, page_from_document};
use gpui::AppContext as _;
use gpui::Context;
use gpui::{Entity, EventEmitter, Pixels, Point, ScrollHandle, Task};
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
        self.subject().map_or_else(
            || "Untitled".to_owned(),
            |subject| subject.identity().name().to_owned(),
        )
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

    /// Returns a value that changes whenever the shown page changes.
    ///
    /// The reader keys its fade on this, so a page that arrives fades in once
    /// and a page that is merely re-rendered does not flicker.
    pub(crate) fn generation(&self) -> u64 {
        let tab = u64::try_from(self.active).unwrap_or(0);
        self.tab()
            .map_or(tab, |open| tab.wrapping_mul(1024).wrapping_add(open.generation))
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
            Target::Background => self.tabs.push(Tab::new(subject)),
            Target::Here => {
                self.tabs.push(Tab::new(subject));
                self.active = self.tabs.len().saturating_sub(1);
            }
        }
        let index = match target {
            Target::Background => self.tabs.len().saturating_sub(1),
            Target::Here => self.active,
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
        if let Some(coordinate) = self
            .tabs
            .get(index)
            .and_then(Tab::subject)
            .map(|subject| subject.coordinate().to_owned())
        {
            self.pages.retain(|(key, _)| *key != coordinate);
        }
        self.load(index, cx);
    }
}

/// Loading pages.
impl DocumentStore {
    fn load(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(subject) = self.tabs.get(index).and_then(Tab::subject).cloned() else {
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
        let endpoint = self.endpoint.clone();
        let wanted = coordinate.clone();
        let task = cx.spawn(async move |this, cx| {
            let parts = cx
                .background_spawn(async move { fetch(&endpoint, symbol, &wanted) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish(index, generation, symbol, &coordinate, parts, cx);
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

    fn finish(
        &mut self,
        index: usize,
        generation: u64,
        symbol: SymbolKey,
        coordinate: &str,
        parts: Parts,
        cx: &mut Context<Self>,
    ) {
        let stale = self
            .tabs
            .get(index)
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
        if let Some(tab) = self.tabs.get_mut(index) {
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
        let members = workspace.page_rows(symbol);
        let relations = parts.relations.unwrap_or_default();
        let page = page_from_document(coordinate, &document, &members, &relations, parts.notes);
        let resolver = |name: &str| workspace.resolve_name(name);
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
        coordinate: String,
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
        let kind = self.workspace.read(cx).kind_of(symbol);
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
                    summary: crate::ui::prose::summary(page.prose()),
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
        Ok(_) => Err(wrong_shape(
            backend_present::Operand::Whole,
            "a page of rows",
        )),
        Err(error) => Err(Fault::from_client_error(
            &error,
            backend_present::Operand::Whole,
        )),
    }
}
