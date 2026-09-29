//! The sidebar: the shelf region (`shell::shelf` re-exports it).
//!
//! It follows `v6/cohesion/COHESION.md`, "The sidebar: primitives": one
//! contextual scope with a way out, four lenses, narrowing in place, state
//! on rows, hoisting, and (with the hand) held chips. The model is pure and
//! lives in files that name no entity or window:
//!
//! - [`scope`]: where the sidebar is, and how it follows the reader;
//! - [`lens`], [`row`], [`state`], [`narrow`], [`outline`], [`listing`]: what
//!   it lists (rows are typed; what a row does is a [`row::Do`], not a
//!   closure);
//! - [`input`]: the keys it reads.
//!
//! [`view`] and [`glyph`] paint it, and this file is the entity that holds
//! the state and carries out what a row does.
//!
//! The region lays itself out from its measured width: wide enough for the
//! shelf, it draws the shelf at its resting width (so an animating column
//! clips it instead of reflowing it); narrow, it draws the spine; in
//! between, only while the shell animates the column, both, crossfaded.

mod glyph;
mod hold;
mod input;
mod lens;
mod listing;
mod narrow;
mod outline;
mod peek;
mod row;
mod scope;
mod state;
mod view;

pub(crate) use outline::{is_test_module, shelf_name};
#[cfg(test)]
pub(crate) use row::TESTS_ROW;
pub(crate) mod twin;

use super::focus::{Act, Target, Targets};
use super::kit::HoverIntent;
use super::region::{Links, Region, RegionCore};
use crate::model::AppSnapshot;
use crate::model::pages::{PackageRef, PageKey, SymbolRef};
use crate::navigation::{Intent, Overlay, Route};
use crate::runtime::store::{Branch, DataStore};
use facet::overlay::float;
use facet::tokens::fluid::{SIDE, SideForm};
use facet::{ActiveFacet as _, Measure};
use gpui::{
    App, Context, InteractiveElement, IntoElement, KeystrokeEvent, ParentElement, Pixels, Render, ScrollStrategy, SharedString, Styled,
    Subscription, Task, UniformListScrollHandle, Window, div, px,
};
use input::{Chord, Peek, Query, SideKey, Typed};
use lens::Lens;
use listing::{Inputs, Listing};
use narrow::{Filter, Matched, Narrow};
use row::{Do, Fold, Folds, Item, Row, RowId};
use scope::{Crumbs, Scope};
use state::{StateBook, WorkspaceCrate};
use std::rc::Rc;

/// Where along the column's travel (0 = spine, 1 = shelf) the shelf's rows and the spine's
/// marks swap. The longest row ends near 63 % of the travel, so swapping between 60 % and
/// 66 % means the rows are gone before the column's clip reaches their text. A fade that
/// spans the whole travel lingers for 9–12 frames, and the legibility law allows a
/// translucent text for only 2.
const SWAP_AT: f32 = 0.60;
/// How much of the travel the swap takes.
const SWAP_OVER: f32 = 0.06;

/// The place a route is: a book (the package as pinned, so reading another
/// release of it is the same place) and the declaration on it. Following
/// clears what belongs to the old place (a narrowing, a lens on another
/// book) and leaves what belongs to the new release of the same one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Place {
    book: Option<SharedString>,
    symbol: Option<SymbolRef>,
}

impl Place {
    fn of(route: &Route) -> Self {
        let book = match route {
            Route::Package(route) => Some(route.package.as_str().to_owned().into()),
            Route::Symbol(route) => Some(route.package.as_str().to_owned().into()),
            Route::Orbit(_) | Route::World => None,
        };
        Self { book, symbol: super::jump::route_symbol(route) }
    }
}

/// What the state book was made from: read again only when one of these
/// changes.
#[derive(Clone, Debug, Eq, PartialEq)]
struct BookKey {
    package: PackageRef,
    from: SharedString,
    to: Option<SharedString>,
}

/// The shelf region.
pub(crate) struct Shelf {
    core: RegionCore,
    links: Links,
    pub(crate) targets: Targets,
    hover: HoverIntent,
    /// Where the sidebar is (following the reader, or hoisted).
    crumbs: Crumbs,
    /// The place `crumbs` last followed.
    followed: Place,
    /// The lens on the scope, and the book it was chosen in: a new book
    /// opens on its contents, the same book keeps the lens across its pages
    /// and across the releases it is read at.
    lens: Lens,
    lens_book: Option<SharedString>,
    /// Groups the person opened or closed by hand.
    folds: Folds,
    /// The words typed to narrow the scope.
    narrow: Narrow,
    /// The crate of yours whose uses Contents is narrowed to.
    via: Option<WorkspaceCrate>,
    /// The pending `G` of a lens chord, and the timer that turns it into a
    /// letter.
    chord: Chord,
    chord_timer: Option<Task<()>>,
    /// The keystroke interceptor (see [`input`]).
    keys: Option<Subscription>,
    /// A peek is following the selection.
    peeking: bool,
    /// How the room the column has sets its lens strip and its rows.
    form: SideForm,
    /// The follow has moved the reader's row: scroll it into view.
    reveal: bool,
    /// The resting width of the full shelf, from the shell.
    rest: Pixels,
    /// The resting width of the spine, from the shell.
    spine: Pixels,
    /// What was last drawn.
    rows: Rc<Vec<Row>>,
    matched: Option<Matched>,
    /// What each item carries, and what it was made from.
    book: Option<(BookKey, Rc<StateBook>)>,
    no_book: Rc<StateBook>,
    scroll: UniformListScrollHandle,
    /// The comb's pinned and viewed releases as last drawn (tests).
    #[cfg(test)]
    comb: std::cell::Cell<Option<(Option<usize>, Option<usize>)>>,
    #[cfg(test)]
    comb_versions: std::cell::RefCell<Vec<SharedString>>,
    /// The upgrade line's releases as last drawn (tests; the line's words
    /// are not published to the probe).
    #[cfg(test)]
    upgrade: std::cell::RefCell<Option<(SharedString, SharedString)>>,
}

impl Shelf {
    /// A shelf called `name` (`shelf`, or `shelf-over` for the overlay one).
    pub(crate) fn new(name: &'static str, links: Links, store: &DataStore) -> Self {
        let route = store.snapshot().route().clone();
        Self {
            core: RegionCore::new(store, &[Branch::Route, Branch::Overlay, Branch::Workspace, Branch::GraphFocus]),
            links,
            targets: Targets::named(name),
            hover: HoverIntent::default(),
            crumbs: Crumbs::following(&route),
            followed: Place::of(&route),
            lens: Lens::Contents,
            lens_book: Place::of(&route).book,
            folds: Folds::default(),
            narrow: Narrow::default(),
            via: None,
            chord: Chord::Idle,
            chord_timer: None,
            keys: None,
            peeking: false,
            form: SideForm::Full,
            reveal: true,
            rest: px(264.0),
            spine: px(42.0),
            rows: Rc::new(Vec::new()),
            matched: None,
            book: None,
            no_book: Rc::new(StateBook::none()),
            scroll: UniformListScrollHandle::new(),
            #[cfg(test)]
            comb: std::cell::Cell::new(None),
            #[cfg(test)]
            comb_versions: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            upgrade: std::cell::RefCell::new(None),
        }
    }

    /// The upgrade line's releases as last drawn (tests).
    #[cfg(test)]
    pub(crate) fn upgrade_line(&self) -> Option<(SharedString, SharedString)> {
        self.upgrade.borrow().clone()
    }

    /// The comb's pinned and viewed versions as last drawn (tests).
    #[cfg(test)]
    pub(crate) fn comb_marks(&self) -> Option<(Option<SharedString>, Option<SharedString>)> {
        let versions = self.comb_versions.borrow();
        self.comb.get().map(|(pinned, viewing)| {
            (pinned.and_then(|i| versions.get(i).cloned()), viewing.and_then(|i| versions.get(i).cloned()))
        })
    }

    #[cfg(test)]
    pub(crate) fn current_symbols(&self) -> Vec<SymbolRef> {
        self.rows.iter().filter_map(Row::item).filter(|item| item.current).filter_map(|item| item.source.clone()).collect()
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }

    /// The shell tells the shelf its resting widths (no notify: a change of
    /// resting width is always a change of the column's bounds too).
    pub(crate) fn set_rest(&mut self, rest: Pixels, spine: Pixels) {
        self.rest = rest;
        self.spine = spine;
    }

    /// Where the list of rows is on screen (window coordinates): what a twin
    /// ring on a row is clipped to.
    pub(crate) fn viewport(&self) -> gpui::Bounds<Pixels> {
        self.scroll.0.borrow().base_handle.bounds()
    }

    /// Keeps the focused row on screen after a keyboard walk.
    pub(crate) fn reveal_focused(&self) {
        if let Some(index) = self.targets.focused().and_then(|id| self.rows.iter().position(|row| row.item().is_some_and(|item| item.key == id))) {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    // ------------------------------------------------------------ the store

    /// What the store holds, listed.
    fn listing(&mut self, snapshot: &AppSnapshot, cx: &mut Context<Self>) -> Listing {
        let route = snapshot.route();
        let settings = match snapshot.overlay() {
            Some(Overlay::Settings(page)) => Some(page),
            _ => None,
        };
        let package = self.crumbs.package().cloned();
        let diffs = package.as_ref().and_then(|package| crate::runtime::fixture_releases::release_data(package, cx));
        let book = self.book_of(package.as_ref(), route, diffs);
        let held = hold::chips(
            crate::runtime::fixture_world::hand_view_for(&snapshot.session().hand, cx).cards.iter().map(|card| (card.held.clone(), card.name.clone(), card.kind)),
        );
        let back = snapshot.session().back.to_vec();
        let store = self.links.store.read(cx);
        let dossier = package.as_ref().map(|package| store.package(package));
        let orbit = store.orbit();
        let current = match store.graph_focus() {
            Some(focus) => focus.indexed.as_ref().filter(|(owner, _)| Some(owner) == package.as_ref()).map(|(_, symbol)| symbol.clone()),
            None => super::jump::route_symbol(route),
        };
        let inputs = Inputs {
            snapshot,
            route,
            crumbs: &self.crumbs,
            lens: self.lens,
            folds: &self.folds,
            filter: Filter { query: self.narrow.query(), via: self.via.as_ref() },
            book: &book,
            dossier: dossier.as_ref().and_then(|resource| resource.loaded_value()),
            orbit: orbit.loaded_value(),
            current,
            diffs,
            settings,
            held,
            trail: hold::trail(&back, route),
        };
        listing::build(&inputs)
    }

    /// The state book for the package being read, made again only when the
    /// package, the pin or the release being read changes.
    fn book_of(&mut self, package: Option<&PackageRef>, route: &Route, diffs: Option<&'static facet::data::release::Crate>) -> Rc<StateBook> {
        let (Some(package), Some(krate)) = (package, diffs) else { return Rc::clone(&self.no_book) };
        let spelled = |version: &str| crate::runtime::fixture_releases::spelled(krate, version).unwrap_or_else(|| version.to_owned().into());
        let pinned = match route {
            Route::Package(route) => PackageRef::parse(route.package.as_str()).ok(),
            Route::Symbol(route) => PackageRef::parse(route.package.as_str()).ok(),
            Route::Orbit(_) | Route::World => None,
        }
        .filter(|routed| scope::book_of(routed) == scope::book_of(package))
        .and_then(|package| package.version().map(str::to_owned));
        let Some(pinned) = pinned else { return Rc::clone(&self.no_book) };
        let to = route.at().filter(|_| pinned_here(route, package)).map(|at| spelled(at.as_str()));
        let key = BookKey { package: package.clone(), from: spelled(&pinned), to };
        if let Some((known, book)) = &self.book
            && *known == key
        {
            return Rc::clone(book);
        }
        let book = Rc::new(StateBook::from_release(krate, package.display_name(), &key.from, key.to.as_deref()));
        self.book = Some((key, Rc::clone(&book)));
        book
    }

    // ------------------------------------------------------------ what a row does

    /// Carries out what a row (or a key) asks.
    fn perform(&mut self, does: &Do, cx: &mut Context<Self>) {
        match does {
            Do::Nothing => {}
            Do::Go(route) => self.links.dispatch(Intent::Navigate(route.clone()), cx),
            Do::Fold(id) => self.flip(id.clone(), cx),
            Do::Lens(lens) => self.set_lens(*lens, cx),
            Do::Release(at) => self.links.dispatch(Intent::SetRelease(at.clone()), cx),
            Do::Project(id) => self.links.dispatch(Intent::ActivateProject(id.clone()), cx),
            Do::Settings(page) => self.links.dispatch(Intent::OpenSettings(*page), cx),
            Do::Via(name) => {
                // Choosing your crate again lets it go; choosing one shows
                // what it uses in Contents.
                self.via = if self.via.as_ref() == Some(name) { None } else { Some(name.clone()) };
                self.lens = Lens::Contents;
                self.list_changed(cx);
            }
            Do::Widen(query) => {
                let find = Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query.clone())));
                self.links.dispatch(Intent::Navigate(find), cx);
            }
        }
    }

    fn flip(&mut self, id: RowId, cx: &mut Context<Self>) {
        self.folds.flip(id);
        cx.notify();
    }

    fn set_lens(&mut self, lens: Lens, cx: &mut Context<Self>) {
        if self.lens != lens {
            self.lens = lens;
            self.list_changed(cx);
        }
    }

    /// The list is a different list now: back to its top.
    fn list_changed(&mut self, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Scopes the sidebar into `scope` (browsing: the reader does not move).
    fn hoist(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.crumbs.hoist(scope);
        self.watch_scope(cx);
        self.narrow.clear();
        self.via = None;
        self.list_changed(cx);
    }

    /// Asks the store for the package the sidebar is scoped to and watches it:
    /// a package hoisted into from the Library is not the one the reader is on.
    fn watch_scope(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        let keys = self.keys(&snapshot);
        let store = self.links.store.clone();
        store.update(cx, |store, cx| {
            for key in keys.iter().cloned() {
                store.ensure(key, cx);
            }
        });
        self.core.watch_keys(store.read(cx), keys);
    }

    /// Steps out one scope. Returns whether there was one.
    fn step_out(&mut self, cx: &mut Context<Self>) -> bool {
        let moved = self.crumbs.step_out();
        if moved {
            self.narrow.clear();
            self.via = None;
            self.list_changed(cx);
        }
        moved
    }

    /// The row the keyboard stands on.
    fn focused_item(&self) -> Option<&Item> {
        let id = self.targets.focused()?;
        self.rows.iter().filter_map(Row::item).find(|item| item.key == id)
    }

    // ------------------------------------------------------------ keys

    /// Reads a keystroke the shell has not bound, while the sidebar has the
    /// keyboard. Returns whether it was the sidebar's.
    fn intercept(&mut self, event: &KeystrokeEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.owns_keyboard(event, window, cx) {
            return false;
        }
        self.key(&event.keystroke, window, cx)
    }

    /// Whether this keystroke is the sidebar's: its zone is the active one,
    /// the shelf is on screen, and the focus is not in a place that owns
    /// the letters (a text input, a menu, the graph, hint mode).
    fn owns_keyboard(&self, event: &KeystrokeEvent, window: &Window, cx: &mut Context<Self>) -> bool {
        const OWN_THE_LETTERS: [&str; 5] = ["Input", "Menu", "Graph", "BrowseCompare", "hints"];
        if !self.targets.is_active() {
            return false;
        }
        let stack = &event.context_stack;
        let in_shell = stack.iter().any(|context| context.contains(super::keys::CONTEXT));
        let elsewhere = stack.iter().any(|context| OWN_THE_LETTERS.iter().any(|name| context.contains(name)));
        let snapshot = self.links.snapshot(cx);
        let on_screen = self
            .links
            .shell
            .upgrade()
            .and_then(|shell| shell.read(cx).frame())
            .is_some_and(|frame| frame.shelf == super::ShelfMode::Shelf);
        let _ = window;
        in_shell && !elsewhere && on_screen && snapshot.overlay().is_none()
    }

    /// One key of the sidebar's. Returns whether it was used.
    fn key(&mut self, keystroke: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(key) = input::decode(keystroke) else { return false };
        // The peek belongs to the row it was opened on: anything that changes
        // the list closes it (moving the selection follows it instead).
        let peek_open = self.peek_is_open(window, cx);
        if peek_open && !matches!(key, SideKey::Up | SideKey::Down | SideKey::Space | SideKey::Escape) {
            let holds = matches!(key, SideKey::Char('h' | 'H')) && self.narrow.is_empty() && !self.chord.is_armed();
            let graphs = matches!(key, SideKey::Char('g' | 'G'));
            if !holds && !graphs {
                self.close_peek(window, cx);
            }
        }
        match key {
            SideKey::Up => self.walk(-1, window, cx),
            SideKey::Down => self.walk(1, window, cx),
            SideKey::Space => self.toggle_peek(window, cx),
            SideKey::Char('h' | 'H') if peek_open && self.narrow.is_empty() && !self.chord.is_armed() => self.hold_selection(window, cx),
            SideKey::Char(character) => {
                match self.chord.feed(character, Query::of(self.narrow.is_empty()), Peek::of(peek_open)) {
                    Typed::Graph => {
                        self.chord_timer = None;
                        self.close_peek(window, cx);
                        self.graph_selection(cx);
                    }
                    Typed::Armed => self.arm_chord(cx),
                    Typed::Lens(lens) => {
                        self.chord_timer = None;
                        self.set_lens(lens, cx);
                        cx.notify();
                    }
                    Typed::Text(text) => {
                        self.chord_timer = None;
                        self.narrow.push_str(&text);
                        self.list_changed(cx);
                    }
                }
                true
            }
            SideKey::Backspace => {
                let flushed = self.flush_chord_now();
                let popped = self.narrow.pop();
                if flushed || popped {
                    self.list_changed(cx);
                }
                flushed || popped
            }
            SideKey::Left => self.left(cx),
            SideKey::Right => self.right(cx),
            // Esc closes the peek first (the shell's, through the float
            // layer), then clears the narrowing.
            SideKey::Escape => !peek_open && self.escape(cx),
        }
    }

    /// ↑ ↓: moves the selection; an open peek follows it.
    fn walk(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.targets.walk(delta) {
            self.reveal_focused();
            cx.notify();
            self.follow_peek(window, cx);
        }
        true
    }

    // ------------------------------------------------------------ the peek

    fn peek_is_open(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let open = self.peeking && float::is_open(&peek::key(), window, cx);
        self.peeking = open;
        open
    }

    fn close_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.peeking {
            float::close(&peek::key(), window, cx);
            self.peeking = false;
        }
    }

    /// Space: shows the peek of the row the keyboard is on, or puts it away.
    /// Returns whether the key was used (a row with no page has no peek).
    fn toggle_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.peek_is_open(window, cx) {
            self.close_peek(window, cx);
            return true;
        }
        self.show_peek(window, cx)
    }

    fn show_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = self.focused_item().cloned() else { return false };
        let Some(page) = item.warm.clone() else { return false };
        let Some(anchor) = self.targets.focused_bounds() else { return false };
        let store = self.links.store.clone();
        store.update(cx, |store, cx| {
            store.ensure(page.clone(), cx);
        });
        let book = self.book.as_ref().map_or_else(|| Rc::clone(&self.no_book), |(_, book)| Rc::clone(book));
        let extras = peek::Extras { symbol: item.source.clone(), path: item.path.clone(), book, store, hoists: item.hoists.is_some() };
        float::open(peek::request(page, item.name.clone(), anchor, extras), window, cx);
        self.peeking = true;
        true
    }

    /// The selection moved: the peek moves with it, once the row it moved to
    /// has been laid out (a row scrolled into view has no place until the
    /// next frame).
    fn follow_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.peek_is_open(window, cx) {
            return;
        }
        let shelf = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = shelf.update(cx, |shelf, cx| {
                if !shelf.show_peek(window, cx) {
                    shelf.close_peek(window, cx);
                }
            });
        });
    }

    /// H: holds the row the peek is on in the hand.
    fn hold_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let held = self.focused_item().and_then(|item| match &item.does {
            Do::Go(Route::Symbol(route)) => Some((route.package.clone(), Some(route.id.clone()))),
            Do::Go(Route::Package(route)) => Some((route.package.clone(), None)),
            _ => None,
        });
        let Some((package, id)) = held else { return false };
        let at = super::root::now_ms();
        self.links.dispatch(Intent::Hold(crate::model::hand::Held { package, id, why: crate::model::hand::HeldWhy::Pin, held_at: at, touched_at: at }), cx);
        self.close_peek(window, cx);
        true
    }

    /// G G: the graph of the row the peek is on.
    fn graph_selection(&mut self, cx: &mut Context<Self>) {
        let route = self.focused_item().and_then(|item| match &item.does {
            Do::Go(route @ Route::Symbol(_)) => route.with_view(crate::navigation::View::Graph),
            _ => None,
        });
        if let Some(route) = route {
            self.links.dispatch(Intent::Navigate(route), cx);
        }
    }

    /// `G` waits for its letter for [`input::CHORD_WINDOW`], then it is a `g`.
    fn arm_chord(&mut self, cx: &mut Context<Self>) {
        self.chord_timer = Some(cx.spawn(async move |shelf, cx| {
            cx.background_executor().timer(input::CHORD_WINDOW).await;
            let _ = shelf.update(cx, |shelf, cx| {
                shelf.chord_timer = None;
                if shelf.flush_chord_now() {
                    shelf.list_changed(cx);
                }
            });
        }));
        cx.notify();
    }

    /// A pending `G` becomes the `g` of the query. Returns whether one was.
    fn flush_chord_now(&mut self) -> bool {
        self.chord_timer = None;
        match self.chord.flush() {
            Some(text) => {
                self.narrow.push_str(text);
                true
            }
            None => false,
        }
    }

    /// ←: closes the group the keyboard is on, or steps out.
    fn left(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.crumbs.is_hoisted()
            && let Some(item) = self.focused_item()
            && item.fold == Some(Fold::Open)
        {
            let id = item.id.clone();
            self.flip(id, cx);
            return true;
        }
        self.step_out(cx)
    }

    /// →: scopes into the row the keyboard is on; a closed group with
    /// nothing to scope into opens.
    fn right(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = self.focused_item() else { return false };
        if let Some(scope) = item.hoists.clone() {
            self.hoist(scope, cx);
            return true;
        }
        if item.fold == Some(Fold::Shut) {
            let id = item.id.clone();
            self.flip(id, cx);
            return true;
        }
        false
    }

    /// Esc: clears the narrowing (the typed words first, then the crate).
    /// Returns whether there was one to clear.
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) -> bool {
        let flushed = self.flush_chord_now();
        let cleared = self.narrow.clear() || self.via.take().is_some();
        if cleared || flushed {
            self.list_changed(cx);
        }
        cleared || flushed
    }

    /// A route change moved the reader: the sidebar follows it.
    fn follow(&mut self, store: &DataStore) {
        let snapshot = store.snapshot();
        let route = snapshot.route();
        let place = Place::of(route);
        let tree = crate::runtime::store::route_package(route).map(|package| store.package(&package));
        if place == self.followed {
            // Another release of the same book: what is shown stays shown.
            self.crumbs.retarget(route);
        } else {
            self.crumbs.follow(route, tree.as_ref().and_then(|resource| resource.loaded_value()).and_then(|dossier| dossier.outline.known()));
        }
        if place.book != self.followed.book {
            self.lens = Lens::Contents;
            self.lens_book = place.book.clone();
        }
        if place != self.followed {
            // A new place starts with only its own group open, unnarrowed.
            self.folds.clear();
            self.narrow.clear();
            self.via = None;
            self.reveal = true;
        }
        self.followed = place;
    }
}

/// Whether the reader is on the book `package` is (at any release of it).
fn pinned_here(route: &Route, package: &PackageRef) -> bool {
    crate::runtime::store::route_package(route).is_some_and(|reading| scope::book_of(&reading) == scope::book_of(package))
}

impl Region for Shelf {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }

    fn keys(&self, snapshot: &AppSnapshot) -> Vec<PageKey> {
        if matches!(snapshot.overlay(), Some(Overlay::Settings(_))) {
            return Vec::new();
        }
        let mut keys = vec![PageKey::Orbit];
        // What the reader is on, and what the sidebar has been scoped into.
        for package in crate::runtime::store::route_package(snapshot.route()).into_iter().chain(self.crumbs.package().cloned()) {
            let key = PageKey::Package(package);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }

    fn observe(&mut self, event: &crate::runtime::store::StoreEvent, store: &DataStore) {
        if event.is_branch(Branch::Route) {
            self.follow(store);
        }
    }
}

impl Render for Shelf {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        if self.keys.is_none() {
            let shelf = cx.weak_entity();
            self.keys = Some(cx.intercept_keystrokes(move |event, window, cx| {
                if shelf.update(cx, |shelf, cx| shelf.intercept(event, window, cx)).unwrap_or(false) {
                    cx.stop_propagation();
                }
            }));
        }
        let measure = self.core.measure(cx);
        let facet = cx.facet();
        let palette = facet.palette();
        let width = self.core.width();
        let snapshot = self.links.snapshot(cx);
        let Listing { head, rows, matched } = self.listing(&snapshot, cx);
        let weak = cx.weak_entity();
        for row in &rows {
            if let Row::Item(item) = row
                && item.is_target()
            {
                self.targets.push(Target {
                    id: item.key.clone(),
                    label: item.name.clone(),
                    act: act(&weak, item.does.clone()),
                    peek: item.warm.clone(),
                    source: item.source.clone(),
                });
            }
        }
        self.matched = matched;
        self.rows = Rc::new(rows);
        self.settle_focus();
        if std::mem::take(&mut self.reveal)
            && let Some(index) = self.rows.iter().position(|row| row.item().is_some_and(|item| item.current))
        {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }

        // Where the column sits between spine and shelf (0 = spine, 1 = shelf), and how far
        // the content has swapped. The column glides on its spring; the content swaps
        // quickly, near the point where the clip would start to cut the rows' text.
        let span = (self.rest - self.spine).max(px(1.0));
        let travel = ((width - self.spine) / span).clamp(0.0, 1.0);
        let open = ((travel - SWAP_AT) / SWAP_OVER).clamp(0.0, 1.0);
        let open = open * open * (3.0 - 2.0 * open);
        let rest_measure = Measure::new(self.rest, &facet);
        self.form = self.core.modes().settle(&SIDE, rest_measure.fluid_room()).mode;
        let mut root = div()
            .id("shelf")
            .relative()
            .size_full()
            .overflow_hidden()
            .border_r_1()
            .border_color(palette.line1.hsla());
        if open > 0.001 {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(self.rest.max(width))
                    .opacity(open)
                    .child(self.column(&head, &rest_measure, palette, cx)),
            );
        }
        if open < 0.999 {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(self.spine)
                    .opacity(1.0 - open)
                    .child(self.spine_column(&measure, palette, cx)),
            );
        }
        let _ = window;
        // The travelling bevel outlines a row; in the spine there are no rows
        // (its bounds are the last the rows had).
        if open > 0.001 {
            root = root.child(self.targets.glow(&measure));
        }
        root
    }
}

impl Shelf {
    /// Keeps the keyboard on a row that is in the list: when a narrowing (or
    /// a lens) took its row away, it lands on the first one.
    fn settle_focus(&mut self) {
        let Some(id) = self.targets.focused() else { return };
        let there = self.rows.iter().filter_map(Row::item).any(|item| item.key == id && item.is_target());
        if there {
            return;
        }
        match self.rows.iter().filter_map(Row::item).find(|item| item.is_target()) {
            Some(first) if !self.narrow.is_empty() || self.via.is_some() => self.targets.focus(first.key.clone()),
            _ => self.targets.clear_focus(),
        }
    }
}

/// What a target does when activated: whatever its row does.
fn act(shelf: &gpui::WeakEntity<Shelf>, does: Do) -> Act {
    let shelf = shelf.clone();
    Rc::new(move |_: &mut Window, cx: &mut App| {
        let _ = shelf.update(cx, |shelf, cx| shelf.perform(&does, cx));
    })
}
