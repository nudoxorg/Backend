//! The window root: the faceted ground, the regions, the transient layers,
//! and every key.
//!
//! The root renders rarely: on a resize (the regions' bounds change), while
//! the shelf or pins column animates, or when a transient layer (Ask, a
//! peek, hint labels) opens or closes. Page data, focus walks, hovers and
//! held modifiers re-render only the region they belong to.

use super::ask::Ask;
use super::bodies::graph::OpenView;
use super::facet_sync::{Surroundings, facet_for};
use super::system;
use facet::overlay::float;
use super::focus::{Target, Zone};
use super::frame::{Frame, FrameInput, ShelfMode};
use super::hints::{HintMode, Step};
use super::keys::{self, CONTEXT};
use super::pins::Pins;
use super::reader::{Reader, Way};
use super::region::{Links, measured, new_region};
use super::reveal::{HOLD, RevealHold};
use super::shelf::Shelf;
use super::status::Status;
use super::jump::route_symbol;
use super::titlebar::Titlebar;
use super::{kit, peeks};
use crate::model::pages::PageKey;
use crate::model::ZoomStep;
use crate::navigation::{Intent, Overlay, Route, RouteDepth, SettingsPage, View};
use std::sync::Arc;
use crate::runtime::store::{Branch, StoreEvent};
use crate::runtime::UiEntityGraph;
use facet::fluid::{Modes, Room};
use facet::motion::{Motion, spec};
use facet::paint::ground;
use facet::tokens::fluid::{ASK, ASK_PANEL, COLUMNS_SHARE, Float};
use facet::tokens::geo;
use facet::{ActiveFacet as _, Measure, Reveal};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    KeyContext, KeyDownEvent, Modifiers, ModifiersChangedEvent, ParentElement, Render, SharedString,
    Pixels, StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Task, Window,
    WindowAppearance, div, px,
};

/// The drawer's paint priority: above every page's own deferred draws (a
/// fanned hand of tiles is 1 or 2) and below the float layer (`float::PRIORITY`,
/// 1000), so a card opened over the drawer still shows above it.
const DRAWER_PRIORITY: usize = 100;

/// How many times each region rendered (isolation tests, the harness).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderCounts {
    /// The root.
    pub shell: u64,
    /// The titlebar.
    pub titlebar: u64,
    /// The inline shelf.
    pub shelf: u64,
    /// The reader.
    pub reader: u64,
    /// The status bar.
    pub status: u64,
    /// The pins column.
    pub pins: u64,
    /// Ask.
    pub ask: u64,
}

/// The window root.
pub struct Shell {
    links: Links,
    graph: UiEntityGraph,
    focus: FocusHandle,
    titlebar: Entity<Titlebar>,
    shelf: Entity<Shelf>,
    /// The full shelf opened over the reader on a narrow window.
    shelf_over: Entity<Shelf>,
    reader: Entity<Reader>,
    status: Entity<Status>,
    pins: Entity<Pins>,
    ask: Entity<Ask>,
    motion: Motion,
    zen: bool,
    /// The hand's Row rung is open (H, or the foot's marks).
    hand_open: bool,
    /// The card the keyboard stands on in the open hand (shown order).
    hand_at: usize,
    shelf_over_open: bool,
    /// The shelf's width the person has dragged it to, at 100 % text.
    shelf_width: Pixels,
    /// The shell's layout modes (the shelf beside the page, a spine, or a
    /// drawer; the pins column), held through their hysteresis bands.
    modes: Modes,
    zone: Zone,
    hold: RevealHold,
    hold_timer: Option<Task<()>>,
    hints: Option<HintMode>,
    /// The page the keyboard peek shows (Space), while its card is open.
    peeking: Option<PageKey>,
    /// How many peeks are pinned (the pins column exists only for pins).
    pinned: usize,
    ask_open: bool,
    /// An exact fixture-node link waiting for the index. Each new visit
    /// invalidates it even if Back later restores the same route.
    /// The system's appearance and text size, and the window's display.
    around: Surroundings,
    renders: u64,
    frame: Option<Frame>,
    /// Keeps the native reduced-motion observer installed while this window lives.
    _reduced_motion: system::ReducedMotionWatch,
    _subscriptions: Vec<Subscription>,
}

/// Opens the shell for an installed entity graph in `window`: the window
/// root the host (and the capture harness) mounts.
pub fn open_shell(graph: &UiEntityGraph, window: &mut Window, cx: &mut App) -> Entity<Shell> {
    cx.bind_keys(keys::bindings());
    cx.new(|cx| Shell::new(graph, window, cx))
}

impl Shell {
    pub(crate) fn new(graph: &UiEntityGraph, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let links = Links {
            root: graph.root.downgrade(),
            store: graph.store.clone(),
            shell: cx.entity().downgrade(),
        };
        let titlebar = new_region(&links, cx, |store| Titlebar::new(links.clone(), store));
        let shelf = new_region(&links, cx, |store| Shelf::new("shelf", links.clone(), store));
        let shelf_over = new_region(&links, cx, |store| Shelf::new("shelf-over", links.clone(), store));
        let reader = new_region(&links, cx, |store| Reader::new(links.clone(), store));
        let status = new_region(&links, cx, |store| Status::new(links.clone(), store));
        let pins = new_region(&links, cx, |store| Pins::new(links.clone(), store));
        let ask_links = links.clone();
        let ask = cx.new(|cx| Ask::new(ask_links, window, cx));
        // The titlebar draws the query in the bar's place while Ask is open.
        let ask_field = ask.read(cx).input().clone();
        titlebar.update(cx, |titlebar, _| titlebar.set_ask_input(ask_field));
        reader.update(cx, |reader, _| {
            reader.targets.set_active(true);
        });
        let focus = cx.focus_handle();
        let events = cx.subscribe_in(&graph.store, window, |shell: &mut Self, _, event: &StoreEvent, window, cx| {
            shell.store_event(event, window, cx);
        });
        let appearance = cx.observe_window_appearance(window, |shell: &mut Self, window, cx| {
            shell.around.dark = is_dark(window.appearance());
            shell.apply_facet(cx);
        });
        let activation = cx.observe_window_activation(window, |shell: &mut Self, window, cx| {
            if !window.is_window_active() {
                let change = shell.hold.clear();
                shell.hold_timer = None;
                if let Some(reveal) = change.reveal {
                    shell.apply_reveal(reveal, cx);
                }
            }
        });
        // The window moved to another display: that display's zoom applies.
        let moved = cx.observe_window_bounds(window, |shell: &mut Self, window, cx| {
            let display = system::display_key(window, cx);
            if display != shell.around.display {
                shell.around.display = display;
                shell.apply_facet(cx);
            }
        });
        // The system's text size is read once, off the UI thread.
        cx.spawn(async move |shell, cx| {
            let text = cx.background_executor().spawn(async { system::text_scale() }).await;
            let _ = shell.update(cx, |shell, cx| {
                if (shell.around.text - text).abs() > f32::EPSILON {
                    shell.around.text = text;
                    shell.apply_facet(cx);
                }
            });
        })
        .detach();
        let weak = cx.entity().downgrade();
        let keystrokes = cx.intercept_keystrokes(move |_, _, cx| {
            // Any keystroke is a chord, not a hold: disarm a pending reveal.
            let _ = weak.update(cx, |shell, _| shell.hold.key_down());
        });
        // Twins: what a hovered declaration lights elsewhere is drawn above the regions.
        let twins = cx.observe_global::<super::side::twin::Lit>(|_, cx| cx.notify());
        let reduced_motion = system::watch_reduced_motion();
        let system_reduced_motion = reduced_motion.initial();
        let motion_changes = reduced_motion.changes();
        cx.spawn(async move |shell, cx| {
            while let Ok(system) = motion_changes.recv().await {
                if shell.update(cx, |shell, cx| {
                    if shell.around.system_reduced_motion != system {
                        shell.around.system_reduced_motion = system;
                        shell.apply_facet(cx);
                    }
                }).is_err() {
                    break;
                }
            }
        }).detach();
        let mut shell = Self {
            links,
            graph: UiEntityGraph {
                root: graph.root.clone(),
                store: graph.store.clone(),
            },
            focus,
            titlebar,
            shelf,
            shelf_over,
            reader,
            status,
            pins,
            ask,
            motion: Motion::new(),
            zen: false,
            hand_open: false,
            hand_at: 0,
            shelf_over_open: false,
            shelf_width: geo::SHELF,
            modes: Modes::new(),
            zone: Zone::Reader,
            hold: RevealHold::default(),
            hold_timer: None,
            hints: None,
            peeking: None,
            pinned: 0,
            ask_open: false,
            around: Surroundings {
                dark: is_dark(window.appearance()),
                text: 1.0,
                system_reduced_motion,
                display: system::display_key(window, cx),
            },
            renders: 0,
            frame: None,
            _reduced_motion: reduced_motion,
            _subscriptions: vec![events, appearance, activation, moved, keystrokes, twins],
        };
        shell.apply_facet(cx);
        shell.focus.focus(window, cx);
        shell
    }

    /// The entity graph the shell renders.
    #[must_use]
    pub const fn graph(&self) -> &UiEntityGraph {
        &self.graph
    }

    /// Fixture world loading is part of the capture's real I/O quiet gate.
    #[must_use]
    pub fn graph_ready(&self, cx: &App) -> bool { self.reader.read(cx).graph_ready(cx) }

    pub(crate) fn graph_work_status(
        &self,
        cx: &App,
    ) -> Option<crate::shell::bodies::graph::MapWorkStatus> {
        self.reader.read(cx).graph_work_status(cx)
    }

    /// The retained map's actual node count, focus and camera.
    #[must_use]
    pub fn graph_report(&self, cx: &App) -> String { self.reader.read(cx).graph_report(cx) }

    #[cfg(feature = "visual-harness")]
    pub fn graph_state(&self, cx: &App) -> facet::gallery::json::Json {
        self.reader.read(cx).graph_state(cx)
    }

    #[cfg(test)]
    pub(crate) fn focus_graph_node(&mut self, node: facet::graph::NodeId, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, cx| reader.focus_graph_node(node, cx));
    }

    #[cfg(test)]
    pub(crate) fn graph_entity(&self, cx: &App) -> Option<Entity<facet::graph::GraphView>> { self.reader.read(cx).graph_entity(cx) }

    #[cfg(test)]
    pub(crate) fn ask_entity(&self) -> Entity<Ask> { self.ask.clone() }

    #[cfg(test)]
    pub(crate) fn graph_canvas_geometry(&self, node: facet::graph::NodeId, cx: &App) -> (Option<gpui::Bounds<gpui::Pixels>>, Option<gpui::Bounds<gpui::Pixels>>, gpui::LayerTransform) {
        self.reader.read(cx).graph_canvas_geometry(node, cx)
    }

    #[cfg(test)]
    pub(crate) fn graph_gem_morphing(&self, cx: &App) -> bool { self.reader.read(cx).graph_gem_morphing(cx) }

    #[cfg(test)]
    pub(crate) fn graph_focus_glyph(&self, cx: &App) -> Option<gpui::Bounds<gpui::Pixels>> { self.reader.read(cx).graph_focus_glyph(cx) }

    #[cfg(test)]
    pub(crate) fn graph_find_state(&self, window: &Window, cx: &App) -> (bool, bool) {
        self.reader.read(cx).graph_find_state(window, cx)
    }

    #[cfg(test)]
    pub(crate) fn titlebar_target_bounds(&self, id: &str, cx: &App) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.titlebar.read(cx).targets.placed().into_iter().find(|(target, _)| target.id == id).map(|(_, bounds)| bounds)
    }

    /// The reader's own targets: a clone still shares its focus and
    /// left-by state (see [`super::focus::Targets`]), so a test can drive
    /// them the same way a click does.
    #[cfg(test)]
    pub(crate) fn reader_targets(&self, cx: &App) -> super::focus::Targets {
        self.reader.read(cx).targets.clone()
    }

    /// Native reader viewport position for Back/Forward restoration tests.
    #[cfg(test)]
    pub(crate) fn source_reader_scroll_offset(&self, cx: &App) -> gpui::Point<gpui::Pixels> {
        self.reader.read(cx).scroll_offset()
    }

    /// Seeds an exact native viewport offset before navigating away in a restoration test.
    #[cfg(test)]
    pub(crate) fn set_source_reader_scroll_offset(&self, offset: gpui::Point<gpui::Pixels>, cx: &App) {
        self.reader.read(cx).set_scroll_offset(offset);
    }

    /// Render counters of the root and every region.
    #[must_use]
    pub fn render_counts(&self, cx: &App) -> RenderCounts {
        RenderCounts {
            shell: self.renders,
            titlebar: self.titlebar.read(cx).renders(),
            shelf: self.shelf.read(cx).renders(),
            reader: self.reader.read(cx).renders(),
            status: self.status.read(cx).renders(),
            pins: self.pins.read(cx).renders(),
            ask: self.ask.read(cx).renders(),
        }
    }

    #[cfg(test)]
    pub(crate) fn status_marks(&self, cx: &App) -> usize {
        self.status.read(cx).marks_drawn()
    }

    #[cfg(test)]
    pub(crate) fn shelf_upgrade(&self, cx: &App) -> Option<(SharedString, SharedString)> {
        self.shelf.read(cx).upgrade_line()
    }

    #[cfg(test)]
    pub(crate) fn shelf_comb(&self, cx: &App) -> Option<(Option<SharedString>, Option<SharedString>)> {
        self.shelf.read(cx).comb_marks()
    }

    #[cfg(test)]
    pub(crate) fn shelf_current_symbols(&self, cx: &App) -> Vec<crate::model::pages::SymbolRef> {
        self.shelf.read(cx).current_symbols()
    }

    /// Every string the reader's last render put on screen, in order.
    #[must_use]
    pub fn reader_text(&self, cx: &App) -> Vec<SharedString> {
        self.reader.read(cx).said().to_vec()
    }

    /// The reader's hero name, line by line, as last set.
    #[must_use]
    pub fn hero_lines(&self, cx: &App) -> Vec<String> {
        self.reader.read(cx).hero().iter().map(ToString::to_string).collect()
    }

    /// How many pages the reader draws (the current one plus any leaving).
    #[must_use]
    pub fn reader_pages(&self, cx: &App) -> usize {
        self.reader.read(cx).pages_on_screen()
    }

    /// The active keyboard zone and its focused target's id.
    #[must_use]
    pub fn focus_state(&self, cx: &App) -> (Zone, Option<SharedString>) {
        let focused = match self.zone {
            Zone::Titlebar => self.titlebar.read(cx).targets.focused(),
            Zone::Shelf => self.shelf.read(cx).targets.focused(),
            Zone::Reader => self.reader.read(cx).targets.focused(),
            Zone::Pins => self.pins.read(cx).targets.focused(),
        };
        (self.zone, focused)
    }

    /// How many descents the reader played and which way the last went.
    #[must_use]
    pub fn descent(&self, cx: &App) -> (u64, Option<Way>) {
        self.reader.read(cx).descent()
    }

    /// The last resolved frame.
    #[must_use]
    pub const fn frame(&self) -> Option<Frame> {
        self.frame
    }

    /// Whether Ask, a peek, or hint mode is open.
    #[must_use]
    pub fn transients(&self) -> (bool, bool, bool) {
        (self.ask_open, self.peeking.is_some(), self.hints.is_some())
    }

    /// What the shell's own chrome is doing, in words: the keyboard's zone
    /// and target, the transients, the hand, zen and the shelf. A journey
    /// that presses a key judges what the key did with these.
    #[must_use]
    pub fn chrome_words(&self, cx: &App) -> Vec<(&'static str, String)> {
        let (zone, focused) = self.focus_state(cx);
        let on = |open: bool| if open { "open" } else { "closed" }.to_owned();
        vec![
            ("zone", format!("{zone:?}").to_lowercase()),
            ("focus", focused.map_or_else(|| "none".to_owned(), |id| id.to_string())),
            ("ask", on(self.ask_open)),
            ("peek", on(self.peeking.is_some())),
            ("hints", on(self.hints.is_some())),
            ("hand", on(self.hand_open)),
            ("zen", if self.zen { "on" } else { "off" }.to_owned()),
            ("shelf", self.frame.map_or_else(|| "none".to_owned(), |frame| format!("{:?}", frame.shelf).to_lowercase())),
            ("drawer", on(self.shelf_over_open)),
        ]
    }

    /// Subscribes `notified` to every view the window draws (the root and
    /// each region): a notification is what dirties a real window, so tests
    /// count these to prove an idle window costs nothing.
    pub fn watch_views(shell: &Entity<Self>, cx: &mut App, notified: std::rc::Rc<std::cell::Cell<u64>>) -> Vec<Subscription> {
        fn watch<T: 'static>(entity: &Entity<T>, cx: &mut App, counter: &std::rc::Rc<std::cell::Cell<u64>>) -> Subscription {
            let counter = std::rc::Rc::clone(counter);
            cx.observe(entity, move |_, _| counter.set(counter.get() + 1))
        }
        let this = shell.read(cx);
        let (titlebar, shelf, shelf_over, reader, status, pins, ask) = (
            this.titlebar.clone(),
            this.shelf.clone(),
            this.shelf_over.clone(),
            this.reader.clone(),
            this.status.clone(),
            this.pins.clone(),
            this.ask.clone(),
        );
        vec![
            watch(shell, cx, &notified),
            watch(&titlebar, cx, &notified),
            watch(&shelf, cx, &notified),
            watch(&shelf_over, cx, &notified),
            watch(&reader, cx, &notified),
            watch(&status, cx, &notified),
            watch(&pins, cx, &notified),
            watch(&ask, cx, &notified),
        ]
    }

    /// The zoom key of the display the window is on.
    #[must_use]
    pub fn display_key(&self) -> Arc<str> {
        Arc::clone(&self.around.display)
    }

    /// Whether a reveal hold is waiting for its timer.
    #[must_use]
    pub const fn hold_armed(&self) -> bool {
        self.hold.is_armed()
    }

    // ── settings → facet ───────────────────────────────────────────────

    fn apply_facet(&mut self, cx: &mut Context<Self>) {
        let settings = self.links.snapshot(cx).settings().clone();
        let current = cx.facet();
        let wanted = facet_for(&settings, &self.around, current.reveal);
        if cx.try_global::<facet::Facet>() != Some(&wanted) {
            facet::set_facet(wanted, cx);
            facet::controls::sync_text_engine(cx);
        }
    }

    fn apply_reveal(&mut self, reveal: Reveal, cx: &mut Context<Self>) {
        let mut facet = cx.facet();
        if facet.reveal == reveal {
            return;
        }
        let keys = facet.reveal.keys != reveal.keys;
        let xray = facet.reveal.xray != reveal.xray;
        facet.reveal = reveal;
        // Not `set_facet`: that refreshes every view. A reveal repaints only
        // the regions that draw key caps (⌘) or rise a rung (⌥).
        cx.set_global(facet);
        if keys {
            self.titlebar.update(cx, |_, cx| cx.notify());
            self.shelf.update(cx, |_, cx| cx.notify());
            // The hand's marks show ⌘1–⌘5 while ⌘ is held.
            if !self.links.snapshot(cx).session().hand.is_empty() {
                self.status.update(cx, |_, cx| cx.notify());
            }
        }
        if xray {
            self.reader.update(cx, |_, cx| cx.notify());
        }
    }

    // ── the store ──────────────────────────────────────────────────────

    fn store_event(&mut self, event: &StoreEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            StoreEvent::Snapshot(Branch::Settings) => {
                self.apply_facet(cx);
                // The shelf toggle moves the columns: the root lays them out.
                cx.notify();
            }
            StoreEvent::Snapshot(Branch::GraphFocus) => cx.notify(),
            StoreEvent::Snapshot(Branch::Overlay) => self.sync_overlay(window, cx),
            StoreEvent::Snapshot(Branch::Route) => {
                let route = self.links.snapshot(cx).route().clone();
                if !super::bodies::graph::is_graph(&route) {
                    self.focus.focus(window, cx);
                }
                // A route a click once left (not a key walk) restores the
                // keyboard to the row that led away from it: this is how
                // Back reads as returning, not as a fresh, unfocused page.
                // `Reader::arrive` clears the reader's own focus on every
                // arrival, including this one still landing from the same
                // event, so the restore is deferred past it rather than
                // raced against it.
                let reader_targets = self.reader.read(cx).targets.clone();
                if let Some(id) = reader_targets.left_by(&route) {
                    cx.defer(move |_cx| reader_targets.focus(id));
                }
                // Navigation closes what floats (pins stay).
                let mut closed = float::close_all(window, cx);
                closed |= self.peeking.take().is_some();
                closed |= self.hints.take().is_some();
                if self.shelf_over_open {
                    self.shelf_over_open = false;
                    closed = true;
                }
                if closed {
                    cx.notify();
                }
                self.sync_overlay(window, cx);
            }
            StoreEvent::Resource(key) => {
                // The open card reads the store each frame: redraw it.
                if self.peeking.as_ref() == Some(key) {
                    cx.notify();
                }
            }
            StoreEvent::Snapshot(Branch::Root) => {}
            StoreEvent::Snapshot(_) => {}
        }
    }

    fn sync_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        super::onboard::sync(&self.links, window, cx);
        let wants_ask = self.links.snapshot(cx).overlay() == Some(Overlay::CommandPalette);
        if wants_ask == self.ask_open {
            return;
        }
        self.ask_open = wants_ask;
        if wants_ask {
            self.ask.update(cx, |ask, cx| ask.opened(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    // ── actions ────────────────────────────────────────────────────────

    /// Opens Ask.
    pub(crate) fn open_ask(&mut self, cx: &mut Context<Self>) {
        self.links.dispatch(Intent::OpenCommandPalette, cx);
    }

    fn find_symbol_link(&mut self, query: crate::model::pages::SearchQuery, cx: &mut Context<Self>) {
        self.links.dispatch(
            Intent::Navigate(Route::Orbit(crate::navigation::OrbitRoute::Browse(
                crate::navigation::BrowseRoute::Find(query),
            ))),
            cx,
        );
    }

    /// S2: the exact identity behind an anatomy link could not be resolved
    /// by this revision's complete indexed graph.
    /// The link does not move the page; the Notice says so.
    fn symbol_link_unresolved(&mut self, query: crate::model::pages::SearchQuery, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        let notice = crate::runtime::graph_focus::Notice {
            visit: snapshot.route().clone(),
            root: snapshot.key(),
            message: format!("{} isn't in the index", query.text).into(),
            retry: None,
        };
        self.links.store.update(cx, |store, cx| store.set_notice(Some(notice), cx));
    }

    /// Follow a graph node through the exact locator captured with the
    /// selected indexed world. A display name never becomes a symbol route.
    fn open_anatomy(&mut self, open: &facet::anatomy::Open, cx: &mut Context<Self>) {
        let node = match &open.target {
            facet::semantics::Target::Node(node) => *node,
            facet::semantics::Target::Path(path) => {
                // An unresolved type is a contextual index lookup, never an
                // invented exact symbol coordinate.
                if let Ok(query) = crate::model::pages::SearchQuery::new(path.as_ref(), 50) {
                    self.find_symbol_link(query, cx);
                }
                return;
            }
        };
        let snapshot = self.links.snapshot(cx);
        if snapshot.route().at().is_some() {
            self.links.store.update(cx, |store, cx| {
                store.set_notice(Some(crate::runtime::graph_focus::Notice {
                    visit: snapshot.route().clone(),
                    root: snapshot.key(),
                    message: "Historical graph identities are not available for this release.".into(),
                    retry: None,
                }), cx);
            });
            return;
        }
        let Some(key) = crate::runtime::indexed_world::key(
            snapshot.key(),
            crate::runtime::hand::preferred(&snapshot),
            cx,
        ) else {
            self.links.store.update(cx, |store, cx| store.set_notice(Some(crate::runtime::graph_focus::Notice {
                visit: snapshot.route().clone(),
                root: snapshot.key(),
                message: "The local graph reader is not ready.".into(),
                retry: None,
            }), cx));
            return;
        };
        let projection = match crate::runtime::indexed_world::get(&key, cx) {
            crate::runtime::indexed_world::State::Ready(projection) => projection,
            crate::runtime::indexed_world::State::Reading => {
                self.links.store.update(cx, |store, cx| store.set_notice(Some(crate::runtime::graph_focus::Notice {
                    visit: snapshot.route().clone(),
                    root: snapshot.key(),
                    message: "The current indexed graph is still being read.".into(),
                    retry: None,
                }), cx));
                return;
            }
            crate::runtime::indexed_world::State::Waiting => {
                self.links.store.update(cx, |store, cx| store.set_notice(Some(crate::runtime::graph_focus::Notice {
                    visit: snapshot.route().clone(),
                    root: snapshot.key(),
                    message: "Waiting for an available graph-read slot; this view will retry when one opens.".into(),
                    retry: None,
                }), cx));
                return;
            }
            crate::runtime::indexed_world::State::Unavailable(reason) => {
                self.links.store.update(cx, |store, cx| store.set_notice(Some(crate::runtime::graph_focus::Notice {
                    visit: snapshot.route().clone(),
                    root: snapshot.key(),
                    message: format!("The current indexed graph is unavailable: {reason}").into(),
                    retry: None,
                }), cx));
                return;
            }
        };
        let Some(resolved) = projection.identities.exact_node(node) else {
            let name = projection.world.nodes.get(node as usize).map(|node| node.name.to_string()).unwrap_or_else(|| "graph node".to_owned());
            if let Ok(query) = crate::model::pages::SearchQuery::new(&name, 200) {
                self.symbol_link_unresolved(query, cx);
            }
            return;
        };
        let Some(route) = kit::symbol_view_route(
            resolved.package.as_str(),
            &resolved.symbol,
            View::Page,
            resolved.line,
        ) else {
            return;
        };
        self.links.dispatch(Intent::Navigate(route), cx);
    }

    /// ⌘\: the shelf opens or closes; on a window too narrow to hold it the
    /// full shelf opens over the reader instead.
    pub(crate) fn toggle_shelf(&mut self, cx: &mut Context<Self>) {
        if self.frame.is_some_and(|frame| frame.shelf_overlays) {
            self.shelf_over_open = !self.shelf_over_open;
            cx.notify();
        } else {
            self.links.dispatch(Intent::ToggleShelf, cx);
        }
    }

    /// Tests and the harness drive the modifier state through this.
    pub fn modifiers(&mut self, modifiers: Modifiers, cx: &mut Context<Self>) {
        let change = self.hold.modifiers(modifiers);
        if let Some(generation) = change.arm {
            self.hold_timer = Some(cx.spawn(async move |shell, cx| {
                cx.background_executor().timer(HOLD).await;
                let _ = shell.update(cx, |shell, cx| {
                    let change = shell.hold.fire(generation);
                    if let Some(reveal) = change.reveal {
                        shell.apply_reveal(reveal, cx);
                    }
                });
            }));
        }
        if let Some(reveal) = change.reveal {
            self.hold_timer = None;
            self.apply_reveal(reveal, cx);
        }
    }

    fn with_zone<R>(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut super::focus::Targets) -> R) -> R {
        match self.zone {
            Zone::Titlebar => self.titlebar.update(cx, |region, _| f(&mut region.targets)),
            Zone::Shelf => self.shelf.update(cx, |region, _| f(&mut region.targets)),
            Zone::Reader => self.reader.update(cx, |region, _| f(&mut region.targets)),
            Zone::Pins => self.pins.update(cx, |region, _| f(&mut region.targets)),
        }
    }

    fn notify_zone(&mut self, zone: Zone, cx: &mut Context<Self>) {
        match zone {
            Zone::Titlebar => self.titlebar.update(cx, |_, cx| cx.notify()),
            Zone::Shelf => self.shelf.update(cx, |region, cx| {
                region.reveal_focused();
                cx.notify();
            }),
            Zone::Reader => self.reader.update(cx, |region, cx| {
                region.reveal_focused();
                cx.notify();
            }),
            Zone::Pins => self.pins.update(cx, |_, cx| cx.notify()),
        }
    }

    /// J/K: the focus walks inside the active zone (only that region
    /// re-renders; the glow springs to the next target).
    pub fn walk(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.peeking.take() {
            // The keyboard's peek belongs to where the keyboard stood.
            float::close(&peeks::float_key(&key), window, cx);
            cx.notify();
        }
        if self.with_zone(cx, |targets| targets.walk(delta)) {
            self.notify_zone(self.zone, cx);
        }
    }

    fn current(&mut self, cx: &mut Context<Self>) -> Option<Target> {
        self.with_zone(cx, |targets| targets.current())
    }

    fn visible_zones(&self) -> Vec<Zone> {
        let shelf = self.frame.map_or(true, |frame| frame.shelf != ShelfMode::Hidden);
        let pins = self.frame.is_some_and(|frame| frame.pins);
        Zone::ALL
            .into_iter()
            .filter(|zone| match zone {
                Zone::Shelf => shelf,
                Zone::Pins => pins,
                Zone::Titlebar | Zone::Reader => true,
            })
            .collect()
    }

    /// Tab: the next zone takes the keyboard.
    pub fn cycle_zone(&mut self, forward: bool, cx: &mut Context<Self>) {
        let zones = self.visible_zones();
        let at = zones.iter().position(|zone| *zone == self.zone).unwrap_or(0);
        let next = if forward {
            zones[(at + 1) % zones.len()]
        } else {
            zones[(at + zones.len() - 1) % zones.len()]
        };
        self.set_zone(next, cx);
    }

    /// The zone takes the keyboard (Tab, or a click in the sidebar).
    pub(crate) fn set_zone(&mut self, zone: Zone, cx: &mut Context<Self>) {
        if zone == self.zone {
            return;
        }
        let old = self.zone;
        let _ = self.with_zone(cx, |targets| targets.set_active(false));
        self.notify_zone(old, cx);
        self.zone = zone;
        self.with_zone(cx, |targets| {
            targets.set_active(true);
            if targets.focused().is_none() {
                let _ = targets.walk(1);
            }
        });
        self.notify_zone(zone, cx);
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // ↵ in the open hand goes to the card it stands on.
        if self.hand_open && self.hand_key("enter", cx) {
            return;
        }
        if let Some(target) = self.current(cx) {
            run(target.act, window, cx);
        }
    }

    /// Space: peek the focused target (W-Float's layer, keyboard: no delay);
    /// Space on an open peek pins it.
    fn peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.peeking.clone()
            && peeks::is_open(&key, window, cx)
        {
            if float::pin_top(window, cx) {
                self.peeking = None;
                self.sync_pins(window, cx);
            }
            cx.notify();
            return;
        }
        let Some(target) = self.current(cx) else {
            return;
        };
        let Some(key) = target.peek.clone() else {
            return;
        };
        let Some(anchor) = self.with_zone(cx, |targets| targets.focused_bounds()) else {
            return;
        };
        self.links.store.update(cx, |store, cx| {
            store.ensure(key.clone(), cx);
        });
        let request = peeks::request(key.clone(), target.label, anchor, self.links.store.clone());
        float::open(request, window, cx);
        self.peeking = Some(key);
        cx.notify();
    }

    /// Re-reads the pins from the float layer (the pins column exists only
    /// while something is pinned).
    fn sync_pins(&mut self, window: &Window, cx: &mut Context<Self>) {
        let pinned = float::pins(window, cx).len();
        if pinned != self.pinned {
            self.pinned = pinned;
            self.pins.update(cx, |_, cx| cx.notify());
            cx.notify();
        }
    }

    /// S: the focused declaration's code. On the page's own declaration it
    /// is a view switch (the entry is replaced); on another one it goes there.
    fn peel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_graph_view(OpenView::Code, window, cx) { return; }
        let snapshot = self.links.snapshot(cx);
        let own = route_symbol(snapshot.route());
        let symbol = self.current(cx).and_then(|target| target.source).or_else(|| own.clone());
        let Some(symbol) = symbol else {
            return;
        };
        if Some(&symbol) == own.as_ref() {
            self.links.dispatch(Intent::SetView(View::Code), cx);
            return;
        }
        let line = self
            .links
            .store
            .read(cx)
            .symbol(&symbol)
            .loaded_value()
            .and_then(|page| page.identity.line);
        let package = kit::package_of(&symbol).or_else(|| match snapshot.route() {
            Route::Symbol(route) => Some(route.package.as_str().to_owned()),
            Route::Package(route) => Some(route.package.as_str().to_owned()),
            Route::Orbit(_) | Route::World => None,
        });
        if let Some(route) = package.and_then(|package| kit::symbol_view_route(&package, &symbol, View::Code, line)) {
            self.links.dispatch(Intent::Navigate(route), cx);
        }
    }

    /// All graph-to-declaration commands use the visible graph selection.
    fn open_graph_view(&mut self, target: OpenView, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let snapshot = self.links.snapshot(cx);
        if super::bodies::graph::is_graph(snapshot.route()) && snapshot.overlay().is_none() {
            self.reader.update(cx, |reader, cx| reader.open_graph_current(target, window, cx));
            return true;
        }
        false
    }

    /// G enters the graph, or opens the graph's current symbol page.
    fn toggle_graph(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader.read(cx).graph_focused(cx) && self.open_graph_view(OpenView::Page, window, cx) { return; }
        let snapshot = self.links.snapshot(cx);
        let intent = match snapshot.route() {
            Route::Symbol(route) if route.view == View::Graph => Intent::Navigate(snapshot.route().with_view(View::Page).expect("symbol view")),
            Route::Symbol(_) => Intent::SetView(View::Graph),
            Route::World => Intent::Back,
            Route::Orbit(_) | Route::Package(_) => {
                self.reader.update(cx, |reader, cx| reader.reset_world(cx));
                Intent::Navigate(Route::World)
            },
        };
        self.links.dispatch(intent, cx);
    }

    /// ⌘.: a declaration's code ↔ its page.
    fn code_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_graph_view(OpenView::Code, window, cx) { return; }
        let snapshot = self.links.snapshot(cx);
        if let Route::Symbol(route) = snapshot.route() {
            let view = if route.view == View::Code { View::Page } else { View::Code };
            self.links.dispatch(Intent::SetView(view), cx);
        }
    }

    /// ⌘+ / ⌘− / ⌘0 on the display the window is on.
    fn zoom(&mut self, step: ZoomStep, cx: &mut Context<Self>) {
        self.links.dispatch(
            Intent::Zoom {
                display: Arc::clone(&self.around.display),
                step,
            },
            cx,
        );
    }

    /// F: labels over every visible target.
    fn hint_mode(&mut self, cx: &mut Context<Self>) {
        if self.hints.is_some() {
            self.hints = None;
            cx.notify();
            return;
        }
        let mut placed = Vec::new();
        placed.extend(self.titlebar.read(cx).targets.placed());
        if self.frame.is_some_and(|frame| frame.shelf == ShelfMode::Shelf) {
            placed.extend(self.shelf.read(cx).targets.placed());
        }
        placed.extend(self.reader.read(cx).targets.placed());
        if self.frame.is_some_and(|frame| frame.pins) {
            placed.extend(self.pins.read(cx).targets.placed());
        }
        if placed.is_empty() {
            return;
        }
        self.hints = Some(HintMode::new(placed));
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.hints.is_none() && self.hand_open && self.hand_key(event.keystroke.key.as_str(), cx) {
            cx.stop_propagation();
            return;
        }
        let Some(hints) = self.hints.as_mut() else {
            return;
        };
        let key = event.keystroke.key.as_str();
        cx.stop_propagation();
        if key == "backspace" {
            if !hints.backspace() {
                self.hints = None;
            }
            cx.notify();
            return;
        }
        let mut chars = key.chars();
        let (Some(character), None) = (chars.next(), chars.next()) else {
            return;
        };
        match hints.key(character) {
            Step::Narrowed => {}
            Step::Chosen(target) => {
                self.hints = None;
                run(target.act, window, cx);
            }
            Step::Missed => self.hints = None,
        }
        cx.notify();
    }

    /// Esc: the topmost transient closes, one per press.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hints.take().is_some() {
            cx.notify();
            return;
        }
        if float::step_back(window, cx) {
            if self.peeking.as_ref().is_some_and(|key| !peeks::is_open(key, window, cx)) {
                self.peeking = None;
            }
            cx.notify();
            return;
        }
        self.peeking = None;
        if self.hand_open {
            self.hand_open = false;
            cx.notify();
            return;
        }
        let overlay = self.links.snapshot(cx).overlay();
        if overlay.is_some() {
            self.links.dispatch(Intent::DismissOverlay, cx);
            return;
        }
        if self.shelf_over_open {
            self.shelf_over_open = false;
            cx.notify();
            return;
        }
        // The page folds what it has open (a module) before it leaves the past.
        if let Some(fold) = self.reader.read(cx).targets.escape() {
            run(fold, window, cx);
            return;
        }
        // Viewing another release: Esc returns to the one you pin.
        if self.links.snapshot(cx).route().at().is_some() {
            self.links.dispatch(Intent::SetRelease(None), cx);
            return;
        }
        if self.with_zone(cx, |targets| {
            let had = targets.focused().is_some();
            targets.clear_focus();
            had
        }) {
            self.notify_zone(self.zone, cx);
        }
    }

    /// ⌘1–⌘4: Orbit, the package, the page, the code. The last two are
    /// views of the declaration you are on, not places.
    /// ⌘D: hold what you are on (a declaration, else its package). Take →
    /// hand: the stone travels from where you held it (the hero's, its
    /// text-free mark) to its place in the foot.
    pub(crate) fn hold(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        // On the graph, what you hold is its focus (its indexed row), and
        // the stone leaves from the canvas glyph.
        let graph_focus = self.links.store.read(cx).graph_focus().cloned();
        if let Some(focus) = graph_focus {
            let Some(Route::Symbol(route)) = focus.indexed.as_ref().and_then(|(package, symbol)| kit::symbol_route(package.as_str(), symbol)) else {
                // Not in the index: nothing to hold, and the Notice says so.
                let notice = crate::runtime::graph_focus::Notice {
                    visit: snapshot.route().clone(),
                    root: snapshot.key(),
                    message: format!("{}::{}::{} isn't in the index", focus.package, focus.module, focus.name).into(),
                    retry: None,
                };
                self.links.store.update(cx, |store, cx| store.set_notice(Some(notice), cx));
                return;
            };
            if let Some(stone) = self.reader.read(cx).graph_focus_glyph(cx) {
                facet::motion::shared::remember(super::hand::take_key(route.id.as_str()), stone, window, cx);
            }
            let at = now_ms();
            let held = crate::model::hand::Held { package: route.package, id: Some(route.id), why: crate::model::hand::HeldWhy::Pin, held_at: at, touched_at: at };
            self.links.dispatch(Intent::Hold(held), cx);
            return;
        }
        let (package, id) = match snapshot.route() {
            Route::Symbol(route) => (route.package.clone(), Some(route.id.clone())),
            Route::Package(route) => (route.package.clone(), None),
            Route::Orbit(_) | Route::World => return,
        };
        if let Some(id) = &id
            && let Ok(symbol) = crate::model::pages::SymbolRef::new(id.as_str())
            && let Some(stone) = facet::motion::shared::last_bounds(kit::shared_id(&symbol), window, cx)
        {
            facet::motion::shared::remember(super::hand::take_key(id.as_str()), stone, window, cx);
        }
        let at = now_ms();
        let held = crate::model::hand::Held { package, id, why: crate::model::hand::HeldWhy::Pin, held_at: at, touched_at: at };
        self.links.dispatch(Intent::Hold(held), cx);
    }

    /// T: tour the package you are in (its page, or a declaration's) in
    /// the graph, from its first stop.
    pub(crate) fn tour(&mut self, cx: &mut Context<Self>) {
        let package = match self.links.snapshot(cx).route() {
            Route::Package(route) => route.package.clone(),
            Route::Symbol(route) if route.view != View::Graph => route.package.clone(),
            Route::Symbol(_) | Route::Orbit(_) | Route::World => return,
        };
        self.links.dispatch(Intent::Tour(package), cx);
    }

    /// H: open or close the hand (the keyboard starts on its first card).
    pub(crate) fn toggle_hand(&mut self, cx: &mut Context<Self>) {
        self.hand_open = !self.hand_open && !self.links.snapshot(cx).session().hand.is_empty();
        self.hand_at = 0;
        cx.notify();
    }

    /// Inside the open hand: ← / → walk the cards, ↵ goes, ⌫ lets go.
    /// Returns whether the key was the hand's.
    fn hand_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let snapshot = self.links.snapshot(cx);
        let hand = snapshot.session().hand.clone();
        let view = crate::runtime::hand::hand_view_for(&hand, &snapshot, cx);
        let count = view.cards.len();
        if count == 0 {
            return false;
        }
        match key {
            "left" => self.hand_at = self.hand_at.saturating_sub(1),
            "right" => self.hand_at = (self.hand_at + 1).min(count - 1),
            "enter" => {
                let at = self.hand_at.min(count - 1);
                self.hand_open = false;
                self.hand_card(at, cx);
            }
            "backspace" => {
                let at = self.hand_at.min(count - 1);
                self.links.dispatch(Intent::LetGo(view.cards[at].held.clone()), cx);
                self.hand_at = at.saturating_sub(usize::from(at + 1 == count));
                if count == 1 {
                    self.hand_open = false;
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// The card the keyboard stands on in the open hand.
    #[cfg(test)]
    pub(crate) const fn hand_at(&self) -> usize {
        self.hand_at
    }

    /// ⌘1–⌘5: go to the hand's nth card, in the order it is shown.
    pub(crate) fn hand_card(&mut self, n: usize, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        let hand = snapshot.session().hand.clone();
        let view = crate::runtime::hand::hand_view_for(&hand, &snapshot, cx);
        let Some(card) = view.cards.get(n) else {
            return;
        };
        if let Some(route) = held_route(&card.held) {
            self.links.dispatch(Intent::Navigate(route), cx);
            self.links.dispatch(Intent::TouchHeld(card.held.clone(), now_ms()), cx);
        }
    }

    /// ⌘⇧C: this place's `nudox://` address on the clipboard.
    pub(crate) fn copy_address(&mut self, cx: &mut Context<Self>) {
        let address = super::jump::address_parts(&self.links.snapshot(cx)).full();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(address));
    }

    fn depth(&mut self, depth: RouteDepth, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        let route = snapshot.route();
        match depth {
            RouteDepth::Page | RouteDepth::Source => {
                let target = if depth == RouteDepth::Page { OpenView::Page } else { OpenView::Code };
                if self.open_graph_view(target, window, cx) { return; }
                if matches!(route, Route::Symbol(_)) {
                    let view = if depth == RouteDepth::Page { View::Page } else { View::Code };
                    self.links.dispatch(Intent::SetView(view), cx);
                }
            }
            RouteDepth::Orbit | RouteDepth::Package => {
                let mut at = route.clone();
                let mut steps = 0;
                while at.depth().is_some_and(|here| here > depth) {
                    let Some(parent) = at.zoom_out() else {
                        break;
                    };
                    at = parent;
                    steps += 1;
                }
                for _ in 0..steps {
                    self.links.dispatch(Intent::ZoomOut, cx);
                }
            }
        }
    }

    // ── layers ─────────────────────────────────────────────────────────

    fn hint_layer(&self, cx: &App) -> Option<AnyElement> {
        let hints = self.hints.as_ref()?;
        let facet = cx.facet();
        let measure = Measure::new(px(240.0), &facet);
        let mut layer = div().absolute().inset_0();
        for (hinted, typed) in hints.visible() {
            let origin = hinted.bounds.origin;
            layer = layer.child(
                div()
                    .absolute()
                    .left(origin.x)
                    .top((origin.y - px(4.0)).max(px(0.0)))
                    .opacity(if typed > 0 { 0.9 } else { 1.0 })
                    .child(facet::controls::kbd(hinted.code[typed..].to_owned(), &measure).hint()),
            );
        }
        Some(layer.into_any_element())
    }

    /// Ask's results: a plate over the shelf's column below the titlebar
    /// (which draws the query itself), a panel from 320 to 440 px as the
    /// window grows and a sheet across it on a phone (`facet::tokens::fluid::ASK`,
    /// held through its hysteresis band). The rest of the page is veiled; a click
    /// on the veil puts Ask away.
    fn ask_layer(&self, frame: &Frame, status: f32, viewport: gpui::Size<Pixels>, cx: &App) -> Option<AnyElement> {
        if !self.ask_open {
            return None;
        }
        let palette = cx.facet().palette();
        let links = self.links.clone();
        let width = match self.modes.settle(&ASK, frame.room).mode {
            Float::Panel => ASK_PANEL.at(frame.room),
            Float::Sheet => viewport.width,
        };
        Some(
            div()
                .id("ask-veil")
                .absolute()
                .top(frame.titlebar)
                .bottom(px(status))
                .left_0()
                .right_0()
                .bg(palette.veil)
                .on_click(move |_, _, cx| links.dispatch(Intent::DismissOverlay, cx))
                // The plate is there once there is a query for it to answer:
                // an empty plate is not something to look at.
                .children(self.ask.read(cx).shows().then(|| {
                    div()
                        .id("ask-frame")
                        .debug_selector(|| "ask-plate".to_owned())
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .w(width)
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .capture_key_down({
                            let ask = self.ask.clone();
                            move |event: &KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                                "up" => {
                                    ask.update(cx, |ask, cx| ask.step(-1, cx));
                                    cx.stop_propagation();
                                }
                                "down" => {
                                    ask.update(cx, |ask, cx| ask.step(1, cx));
                                    cx.stop_propagation();
                                }
                                _ => {}
                            }
                        })
                        .child(self.ask.clone())
                }))
                .into_any_element(),
        )
    }
}

/// Runs a target's act after the current update: an act may reach back
/// into the shell (open Ask, toggle the shelf), which must not re-enter it.
fn run(act: super::focus::Act, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| act(window, cx));
}

impl Shell {
    /// Publishes the transient layers as the float stack the harness checks
    /// (unique keys, at most one of each, nothing left once settled).
    fn publish_stack(&self, ask: Option<Vec<facet::probe::BoundsSample>>, cx: &mut App) {
        let hints = self.hints.as_ref().map(HintMode::remaining);
        facet::probe::record_stack(cx, move || {
            let entry = |key: String, kind: &str, pinned: bool| facet::probe::StackEntry {
                key,
                kind: kind.to_owned(),
                parent: None,
                phase: facet::probe::StackPhase::Open,
                pinned,
                bounds: None,
            };
            // Peeks and pins are W-Float's layer's own entries; the shell
            // adds its transients: Ask and hint mode.
            let mut entries = Vec::new();
            for bounds in ask.into_iter().flatten() {
                entries.push(facet::probe::StackEntry { bounds: Some(bounds.clone()), ..entry(bounds.key.clone(), "dialog", false) });
            }
            if let Some(count) = hints {
                entries.push(entry(format!("hints:{count}"), "hints", false));
            }
            facet::probe::StackSample {
                layer: "shell".to_owned(),
                entries,
            }
        });
    }
}

fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The probe ledger describes one painted frame.
        facet::probe::draw_started(cx);
        self.renders = self.renders.saturating_add(1);
        let facet = cx.facet();
        let palette = facet.palette();
        let scale = facet.text_scale;
        let viewport = window.viewport_size();
        let snapshot = self.links.snapshot(cx);
        let frame = Frame::resolve(
            FrameInput {
                window: Room::new(viewport.width, scale),
                shelf_open: snapshot.settings().shelf_open,
                zen: self.zen,
                shelf_width: self.shelf_width,
                pinned: self.pinned > 0,
            },
            &self.modes,
        );
        self.frame = Some(frame);
        // The shelf opened over the reader answers a window too narrow to
        // hold it inline. A window that holds it has answered that ask: it
        // must not come back by itself when the window narrows again.
        if !frame.shelf_overlays {
            self.shelf_over_open = false;
        }
        // The status bar grows a line when the address's name has to wrap;
        // sized here from the same fit the bar sets, in the same frame.
        let (graph_focus, graph_notice) = { let store = self.links.store.read(cx); (store.graph_focus().cloned(), store.notice().cloned()) };
        // The foot grows only for what the graph says in it; the hand's
        // marks sit on one line.
        let status_height = if super::status::graph_speaks(&snapshot, graph_focus.as_ref(), graph_notice.as_ref()) {
            // Set beside the hand's marks when it holds anything (the same
            // room the foot sets it in).
            let room = super::status::line_room(viewport.width, frame.shelf_width, snapshot.session().hand.held().len(), scale);
            let (lines, role) = super::status::feedback_lines(&snapshot, graph_focus.as_ref(), graph_notice.as_ref(), room, cx);
            super::status::height(lines.len(), &role, f32::from(frame.status))
        } else {
            f32::from(frame.status)
        };
        // Structural changes animate (the shelf becoming a spine, the pins
        // column arriving); a window drag inside one mode tracks directly,
        // because the targets do not move.
        let columns_cap = f32::from(viewport.width) * COLUMNS_SHARE.at(frame.room);
        let shelf_width = self.motion.animate("shelf-w", f32::from(frame.shelf_width), spec::SETTLE, window, cx).min(columns_cap);
        let pins_width = self.motion.animate("pins-w", f32::from(frame.pins_width), spec::SETTLE, window, cx).min((columns_cap - shelf_width).max(0.0));
        let spine = geo::KSPINE * scale;
        self.shelf.update(cx, |shelf, _| shelf.set_rest(frame.shelf_body, spine));
        self.shelf_over.update(cx, |shelf, _| shelf.set_rest(frame.drawer, spine));
        // The hand's marks start from the reader column's left edge.
        let reader_left = px(shelf_width);
        self.status.update(cx, |status, cx| {
            if status.reader_left() != reader_left {
                status.set_reader_left(reader_left);
                cx.notify();
            }
        });
        let over = frame.shelf_overlays && self.shelf_over_open;
        let drawer = f32::from(frame.drawer);
        let over_x = self.motion.animate("over-x", if over { 0.0 } else { -drawer }, spec::SETTLE, window, cx).min(columns_cap);

        let mut context = KeyContext::new_with_defaults();
        context.add(CONTEXT);
        if self.hints.is_some() {
            context.add("hints");
        }
        // An open jump-bar menu owns the plain keys (arrows, ↵, type-ahead,
        // Esc): the shell's bindings step aside (`keys::binding`, `!Menu`).
        if super::titlebar::menu_open(window, cx) {
            context.add("Menu");
        }

        let body = div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .children((shelf_width > 0.5).then(|| {
                measured(
                    &self.shelf,
                    StyleRefinement::default().w(px(shelf_width)).h_full().flex_none(),
                )
            }))
            .child(measured(
                &self.reader,
                StyleRefinement::default().flex_1().h_full().min_w(px(0.0)),
            ))
            .children((pins_width > 0.5).then(|| {
                measured(
                    &self.pins,
                    StyleRefinement::default().w(px(pins_width)).h_full().flex_none(),
                )
            }));

        let mut root = div()
            .id("shell")
            .debug_selector(|| "shell-root".to_owned())
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(palette.g1)
            .text_color(palette.ink1.hsla())
            .font_family(facet::fonts::family(facet::tokens::ty::BODY))
            .track_focus(&self.focus)
            .key_context(context)
            .on_action(cx.listener(|shell, _: &keys::FocusNext, window, cx| shell.walk(1, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::FocusPrev, window, cx| shell.walk(-1, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::Activate, window, cx| shell.activate(window, cx)))
            .on_action(cx.listener(|shell, _: &keys::Peek, window, cx| shell.peek(window, cx)))
            .on_action(cx.listener(|shell, _: &keys::PeelSource, window, cx| shell.peel(window, cx)))
            .on_action(cx.listener(|shell, _: &keys::HintMode, _, cx| shell.hint_mode(cx)))
            .on_action(cx.listener(|shell, _: &keys::Ask, _, cx| shell.open_ask(cx)))
            .on_action(cx.listener(|shell, _: &keys::Back, _, cx| shell.links.dispatch(Intent::Back, cx)))
            .on_action(cx.listener(|shell, _: &keys::Forward, _, cx| shell.links.dispatch(Intent::Forward, cx)))
            .on_action(cx.listener(|shell, _: &keys::Surface, _, cx| shell.links.dispatch(Intent::ZoomOut, cx)))
            .on_action(cx.listener(|shell, _: &keys::Zen, _, cx| {
                shell.zen = !shell.zen;
                cx.notify();
            }))
            .on_action(cx.listener(|shell, _: &keys::ToggleShelf, _, cx| shell.toggle_shelf(cx)))
            .on_action(cx.listener(|shell, _: &keys::NextZone, _, cx| shell.cycle_zone(true, cx)))
            .on_action(cx.listener(|shell, _: &keys::PrevZone, _, cx| shell.cycle_zone(false, cx)))
            .on_action(cx.listener(|shell, _: &keys::Escape, window, cx| shell.escape(window, cx)))
            .on_action(cx.listener(|shell, _: &keys::DepthOrbit, window, cx| shell.depth(RouteDepth::Orbit, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::DepthPackage, window, cx| shell.depth(RouteDepth::Package, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::DepthPage, window, cx| shell.depth(RouteDepth::Page, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::DepthCode, window, cx| shell.depth(RouteDepth::Source, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::Graph, window, cx| shell.toggle_graph(window, cx)))
            .on_action(cx.listener(|shell, _: &keys::CodePage, window, cx| shell.code_page(window, cx)))
            .on_action(cx.listener(|shell, open: &facet::anatomy::Open, _, cx| shell.open_anatomy(open, cx)))
            .on_action(cx.listener(|shell, _: &keys::ZoomIn, _, cx| shell.zoom(ZoomStep::In, cx)))
            .on_action(cx.listener(|shell, _: &keys::ZoomOut, _, cx| shell.zoom(ZoomStep::Out, cx)))
            .on_action(cx.listener(|shell, _: &keys::ZoomReset, _, cx| shell.zoom(ZoomStep::Reset, cx)))
            .on_action(cx.listener(|shell, _: &keys::Hold, window, cx| shell.hold(window, cx)))
            .on_action(cx.listener(|shell, _: &keys::OpenHand, _, cx| shell.toggle_hand(cx)))
            .on_action(cx.listener(|shell, _: &keys::HandCard1, _, cx| shell.hand_card(0, cx)))
            .on_action(cx.listener(|shell, _: &keys::HandCard2, _, cx| shell.hand_card(1, cx)))
            .on_action(cx.listener(|shell, _: &keys::HandCard3, _, cx| shell.hand_card(2, cx)))
            .on_action(cx.listener(|shell, _: &keys::HandCard4, _, cx| shell.hand_card(3, cx)))
            .on_action(cx.listener(|shell, _: &keys::HandCard5, _, cx| shell.hand_card(4, cx)))
            .on_action(cx.listener(|shell, _: &keys::CopyAddress, _, cx| shell.copy_address(cx)))
            .on_action(cx.listener(|shell, _: &keys::Tour, _, cx| shell.tour(cx)))
            .on_action(cx.listener(|shell, _: &keys::OpenSettings, _, cx| {
                shell.links.dispatch(Intent::OpenSettings(SettingsPage::Appearance), cx);
            }))
            .on_action(cx.listener(|shell, _: &keys::AddFolder, _, cx| shell.links.dispatch(Intent::OpenAddProject, cx)))
            .on_modifiers_changed(cx.listener(|shell, event: &ModifiersChangedEvent, _, cx| {
                shell.modifiers(event.modifiers, cx);
            }))
            .capture_key_down(cx.listener(|shell, event: &KeyDownEvent, window, cx| {
                shell.key_down(event, window, cx);
            }))
            .child(ground())
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(measured(
                        &self.titlebar,
                        StyleRefinement::default().w_full().h(frame.titlebar).flex_none(),
                    ))
                    .child(body)
                    .child(measured(
                        &self.status,
                        StyleRefinement::default().w_full().h(px(status_height)).flex_none(),
                    )),
            );
        // The hand opened (the Row rung), just above the foot.
        let hand = snapshot.session().hand.clone();
        if self.hand_open && !hand.is_empty() {
            let view = crate::runtime::hand::hand_view_for(&hand, &snapshot, cx);
            let measure = Measure::new(viewport.width, &facet);
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(status_height + 6.0 * scale))
                    .left(px(shelf_width + 14.0 * scale))
                    .max_w(viewport.width - px(shelf_width + 28.0 * scale))
                    .child(super::hand::row(&view, self.hand_at, &self.links, &measure, palette)),
            );
        } else if self.hand_open {
            self.hand_open = false;
        }
        if over || over_x > -drawer + 0.5 {
            // The drawer's scrim: the page dims as the shelf slides over it,
            // and a click on the strip of page left beside it puts it away. The
            // whole is a deferred draw above the page's own (a fanned hand of
            // tiles paints deferred too, and must not cover the drawer) and
            // below the float layer's cards (`float::PRIORITY`).
            let opened = ((over_x + drawer) / drawer.max(1.0)).clamp(0.0, 1.0);
            root = root.child(
                gpui::deferred(
                    div()
                        .id("shelf-over")
                        .absolute()
                        .top(frame.titlebar)
                        .bottom(px(status_height))
                        .left_0()
                        .right_0()
                        .child(
                            div()
                                .id("shelf-scrim")
                                .absolute()
                                .inset_0()
                                .bg(palette.veil.alpha(opened))
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.shelf_over_open = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("shelf-drawer")
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(over_x))
                                .w(frame.drawer)
                                .bg(palette.g2)
                                .child(measured(&self.shelf_over, StyleRefinement::default().size_full())),
                        ),
                )
                .with_priority(DRAWER_PRIORITY),
            );
        }
        // A peek the layer closed by itself (pointer, click outside) is over.
        if let Some(key) = self.peeking.clone()
            && !peeks::is_open(&key, window, cx)
        {
            self.peeking = None;
        }
        let pinned = float::pins(window, cx).len();
        if pinned != self.pinned {
            self.pinned = pinned;
            self.pins.update(cx, |_, cx| cx.notify());
        }
        let ask_width = match self.modes.settle(&ASK, frame.room).mode {
            Float::Panel => ASK_PANEL.at(frame.room),
            Float::Sheet => viewport.width,
        };
        // Ask is a dialog over the veiled page: its field (the titlebar) and
        // its plate are what a person reads; the page under the veil is not.
        let ask_bounds = self.ask_open.then(|| {
            let sample = |key: &str, x: Pixels, y: Pixels, width: Pixels, height: Pixels| facet::probe::BoundsSample {
                key: key.to_owned(),
                x: f32::from(x),
                y: f32::from(y),
                width: f32::from(width),
                height: f32::from(height),
            };
            let mut parts = vec![sample("ask-field", px(0.0), px(0.0), viewport.width, frame.titlebar)];
            // The plate is drawn once there is a query for it to answer
            // (`ask_layer`): before that the page under the veil is all
            // there is (J9's ask-open frame held the shelf's words to text
            // contrast under a plate that was not there).
            if self.ask.read(cx).shows() {
                parts.push(sample("ask-plate", px(0.0), frame.titlebar, ask_width, (viewport.height - frame.titlebar - px(status_height)).max(px(0.0))));
            }
            parts
        });
        self.publish_stack(ask_bounds, cx);
        let float = float::layer(window, cx);
        // Twins: a ring on every other place the hovered declaration stands,
        // each clipped to the region it is in (the shelf's rows, the page).
        let twins = super::side::twin::lit(cx).and_then(|symbol| {
            let list = self.shelf.read(cx).viewport();
            let page = gpui::Bounds::new(
                gpui::point(px(shelf_width), frame.titlebar),
                gpui::size((viewport.width - px(shelf_width) - px(pins_width)).max(px(0.0)), (viewport.height - frame.titlebar - px(status_height)).max(px(0.0))),
            );
            let regions = [
                (list, self.shelf.read(cx).targets.twins_of(&symbol)),
                (page, self.reader.read(cx).targets.twins_of(&symbol)),
            ];
            super::side::twin::rings(&regions, window.mouse_position(), cx)
        });
        root.children(self.ask_layer(&frame, status_height, viewport, cx))
            .children(self.hint_layer(cx))
            .children(twins)
            .child(float)
    }
}

/// The wall clock in unix ms (the hand's held and touched times).
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
}

/// The page a held thing opens: its declaration's page, or its package.
pub(crate) fn held_route(held: &crate::model::hand::Held) -> Option<Route> {
    Some(match &held.id {
        Some(id) => Route::Symbol(crate::navigation::SymbolRoute {
            project: None,
            package: held.package.clone(),
            id: id.clone(),
            at: None,
            view: View::Page,
            line: None,
            selected: None,
        }),
        None => Route::Package(crate::navigation::PackageRoute {
            project: None,
            package: held.package.clone(),
            lane: crate::navigation::PackageLane::Overview,
            selected: None,
            at: None,
        }),
    })
}
