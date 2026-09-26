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
use super::frame::{Frame, FrameInput, KSPINE, SHELF, ShelfMode};
use super::hints::{HintMode, Step};
use super::keys::{self, CONTEXT};
use super::pins::Pins;
use super::reader::{Reader, Way};
use super::region::{Links, measured, new_region};
use super::reveal::{HOLD, RevealHold};
use super::shelf::Shelf;
use super::status::Status;
use super::thread::route_symbol;
use super::titlebar::Titlebar;
use super::{kit, peeks};
use crate::model::pages::PageKey;
use crate::model::ZoomStep;
use crate::navigation::{Intent, Overlay, Route, RouteDepth, SettingsPage, View};
use std::sync::Arc;
use crate::runtime::store::{Branch, StoreEvent};
use crate::runtime::UiEntityGraph;
use facet::motion::{Motion, spec};
use facet::paint::ground;
use facet::{ActiveFacet as _, Measure, Reveal};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    KeyContext, KeyDownEvent, Modifiers, ModifiersChangedEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Task, Window,
    WindowAppearance, div, px,
};

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
    shelf_over_open: bool,
    shelf_width: f32,
    zone: Zone,
    hold: RevealHold,
    hold_timer: Option<Task<()>>,
    hints: Option<HintMode>,
    /// The page the keyboard peek shows (Space), while its card is open.
    peeking: Option<PageKey>,
    /// How many peeks are pinned (the pins column exists only for pins).
    pinned: usize,
    ask_open: bool,
    ask_trail: bool,
    /// The system's appearance and text size, and the window's display.
    around: Surroundings,
    renders: u64,
    frame: Option<Frame>,
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
            shelf_over_open: false,
            shelf_width: SHELF,
            zone: Zone::Reader,
            hold: RevealHold::default(),
            hold_timer: None,
            hints: None,
            peeking: None,
            pinned: 0,
            ask_open: false,
            ask_trail: false,
            around: Surroundings {
                dark: is_dark(window.appearance()),
                text: 1.0,
                display: system::display_key(window, cx),
            },
            renders: 0,
            frame: None,
            _subscriptions: vec![events, appearance, activation, moved, keystrokes],
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
    pub(crate) fn graph_gem_morphing(&self, cx: &App) -> bool { self.reader.read(cx).graph_gem_morphing(cx) }

    #[cfg(test)]
    pub(crate) fn graph_find_state(&self, window: &Window, cx: &App) -> (bool, bool) {
        self.reader.read(cx).graph_find_state(window, cx)
    }

    #[cfg(test)]
    pub(crate) fn titlebar_target_bounds(&self, id: &str, cx: &App) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.titlebar.read(cx).targets.placed().into_iter().find(|(target, _)| target.id == id).map(|(_, bounds)| bounds)
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
            Zone::Titlebar => self.titlebar.read(cx).targets.focused().cloned(),
            Zone::Shelf => self.shelf.read(cx).targets.focused().cloned(),
            Zone::Reader => self.reader.read(cx).targets.focused().cloned(),
            Zone::Pins => self.pins.read(cx).targets.focused().cloned(),
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
            StoreEvent::Snapshot(Branch::Overlay) => self.sync_overlay(window, cx),
            StoreEvent::Snapshot(Branch::Route) => {
                if !super::bodies::graph::is_graph(self.links.snapshot(cx).route()) {
                    self.focus.focus(window, cx);
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
            StoreEvent::Snapshot(_) => {}
        }
    }

    fn sync_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wants_ask = self.links.snapshot(cx).overlay() == Some(Overlay::CommandPalette);
        if wants_ask == self.ask_open {
            return;
        }
        self.ask_open = wants_ask;
        if wants_ask {
            let trail = self.ask_trail;
            self.ask.update(cx, |ask, cx| ask.opened(trail, window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    // ── actions ────────────────────────────────────────────────────────

    /// Opens Ask; `trail` lists the walked trail instead of results.
    pub(crate) fn open_ask(&mut self, trail: bool, cx: &mut Context<Self>) {
        self.ask_trail = trail;
        self.links.dispatch(Intent::OpenCommandPalette, cx);
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

    fn set_zone(&mut self, zone: Zone, cx: &mut Context<Self>) {
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

    fn ask_layer(&self, cx: &App) -> Option<AnyElement> {
        if !self.ask_open {
            return None;
        }
        let palette = cx.facet().palette();
        let links = self.links.clone();
        Some(
            div()
                .id("ask-veil")
                .absolute()
                .inset_0()
                .bg(palette.veil)
                .flex()
                .justify_center()
                .pt(px(72.0 * cx.facet().text_scale))
                .on_click(move |_, _, cx| links.dispatch(Intent::DismissOverlay, cx))
                .child(
                    div()
                        .id("ask-frame")
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
                        .child(self.ask.clone()),
                )
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
    fn publish_stack(&self, cx: &mut App) {
        let ask = self.ask_open;
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
            if ask {
                entries.push(entry("ask".to_owned(), "dialog", false));
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
        let frame = Frame::resolve(FrameInput {
            width: f32::from(viewport.width),
            scale,
            shelf_open: snapshot.settings().shelf_open,
            zen: self.zen,
            shelf_width: self.shelf_width,
            pinned: self.pinned > 0,
        });
        self.frame = Some(frame);
        // The status bar grows a line when the address's name has to wrap;
        // sized here from the same fit the bar sets, in the same frame.
        let (address, address_role) = super::status::address_lines(&snapshot, viewport.width, cx);
        let status_height = super::status::height(address.len(), &address_role, frame.status);
        // Structural changes animate (the shelf becoming a spine, the pins
        // column arriving); a window drag inside one mode tracks directly,
        // because the targets do not move.
        let shelf_width = self.motion.animate("shelf-w", frame.shelf_width, spec::SETTLE, window, cx);
        let pins_width = self.motion.animate("pins-w", frame.pins_width, spec::SETTLE, window, cx);
        let spine = KSPINE * scale;
        self.shelf.update(cx, |shelf, _| shelf.set_rest(px(frame.shelf_body), px(spine)));
        self.shelf_over.update(cx, |shelf, _| shelf.set_rest(px(frame.shelf_body), px(spine)));
        let over = frame.shelf_overlays && self.shelf_over_open;
        let over_x = self.motion.animate("over-x", if over { 0.0 } else { -frame.shelf_body }, spec::SETTLE, window, cx);

        let mut context = KeyContext::new_with_defaults();
        context.add(CONTEXT);
        if self.hints.is_some() {
            context.add("hints");
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
            .on_action(cx.listener(|shell, _: &keys::Ask, _, cx| shell.open_ask(false, cx)))
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
            .on_action(cx.listener(|shell, _: &keys::ZoomIn, _, cx| shell.zoom(ZoomStep::In, cx)))
            .on_action(cx.listener(|shell, _: &keys::ZoomOut, _, cx| shell.zoom(ZoomStep::Out, cx)))
            .on_action(cx.listener(|shell, _: &keys::ZoomReset, _, cx| shell.zoom(ZoomStep::Reset, cx)))
            .on_action(cx.listener(|shell, _: &keys::OpenSettings, _, cx| {
                shell.links.dispatch(Intent::OpenSettings(SettingsPage::Appearance), cx);
            }))
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
                        StyleRefinement::default().w_full().h(px(frame.titlebar)).flex_none(),
                    ))
                    .child(body)
                    .child(measured(
                        &self.status,
                        StyleRefinement::default().w_full().h(px(status_height)).flex_none(),
                    )),
            );
        if over || over_x > -frame.shelf_body + 0.5 {
            let links = self.links.clone();
            let _ = links;
            root = root.child(
                div()
                    .absolute()
                    .top(px(frame.titlebar))
                    .bottom(px(status_height))
                    .left(px(over_x))
                    .w(px(frame.shelf_body))
                    .bg(palette.g2)
                    .child(measured(&self.shelf_over, StyleRefinement::default().size_full())),
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
        self.publish_stack(cx);
        let float = float::layer(window, cx);
        root.children(self.ask_layer(cx))
            .children(self.hint_layer(cx))
            .child(float)
    }
}

#[allow(dead_code)]
fn _key(_: PageKey) {}
