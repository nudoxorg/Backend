//! The window root: the faceted ground, the regions, the transient layers,
//! and every key.
//!
//! The root renders rarely: on a resize (the regions' bounds change), while
//! the shelf or pins column animates, or when a transient layer (Ask, a
//! peek, hint labels) opens or closes. Page data, focus walks, hovers and
//! held modifiers re-render only the region they belong to.

use super::ask::Ask;
use super::ask_presentation::{AskPresentation, AskScene};
use super::facet_sync::{Surroundings, facet_for};
use super::system;
use facet::overlay::float;
use gpui_component::FocusTrapElement as _;
use gpui_component::WindowExt as _;
use super::focus::{Target, Zone};
use super::frame::{Frame, FrameInput, ShelfMode};
use super::hints::{HintMode, Hinted, Step};
use super::keyboard::KeyboardClaim;
use super::keys::{self, CONTEXT};
use super::pins::Pins;
use super::reader::{Reader, ViewportScroll, Way, mounts_scroll_body};
use super::region::{Links, a11y_inert, measured, new_region};
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
use crate::runtime::store::{Branch, OwnerAttachment, OwnerRetryAttachment, StoreEvent};
use crate::runtime::UiEntityGraph;
use facet::fluid::{Modes, Room};
use facet::motion::{Motion, spec};
use facet::paint::ground;
use facet::tokens::fluid::COLUMNS_SHARE;
use facet::tokens::geo;
use facet::{ActiveFacet as _, Measure, Reveal};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, InteractiveElement, IntoElement,
    KeyContext, KeyDownEvent, Modifiers, ModifiersChangedEvent, ParentElement, Render, SharedString,
    Pixels, StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Task, Window,
    WindowAppearance, div, px,
};

/// The drawer's paint priority: above every page's own deferred draws (a
/// fanned hand of tiles is 1 or 2) and below the float layer (`float::PRIORITY`,
/// 1000), so a card opened over the drawer still shows above it.
const DRAWER_PRIORITY: usize = 100;

/// Bounded, text-free observation for a captured native keyboard frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardDiagnostic {
    pub shell_frame: u64,
    pub native_frame: u64,
    pub window_active: bool,
    pub focus_owner: &'static str,
    pub target_mounted: bool,
    pub input_handler_present: bool,
    pub editor_chars: Option<usize>,
    pub pending_claim: bool,
}

/// A page control's lease on the current painted visit and owner. Ordinary
/// input interrupts a deferred focus return, but must not revoke the very
/// gesture being delivered to a native page control.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PageInputScope {
    generation: u64,
    authority: crate::core::ProducerAuthority,
    attachment: Option<OwnerAttachment>,
    retry: Option<OwnerRetryAttachment>,
}

impl PageInputScope {
    fn same_producer(&self, other: &Self) -> bool {
        self.authority == other.authority && self.attachment == other.attachment && self.retry == other.retry
    }
}

/// One checked clock, with an explicit projection for independent local input.
#[derive(Clone, Copy)]
enum InputOwnerChange { Structure, Producer }

/// A shelf control belongs to one current native scene, in its dock or drawer.
/// It shares the page input epoch and producer identity, never reading history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ShelfInputScope {
    input: PageInputScope,
    surface: ShelfNativeSurface,
}

/// Native ownership follows the settled responsive surface, not its pixel size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShelfNativeSurface { Hidden, Spine, Docked, Drawer }
impl ShelfNativeSurface {
    fn of(frame: Option<Frame>, drawer: bool) -> Self {
        let Some(frame) = frame else { return Self::Hidden };
        if frame.shelf_overlays && drawer { return Self::Drawer; }
        match frame.shelf {
            ShelfMode::Hidden => Self::Hidden,
            ShelfMode::Spine => Self::Spine,
            ShelfMode::Shelf => Self::Docked,
        }
    }
}
impl ShelfInputScope {
    pub(crate) const fn surface(&self) -> ShelfNativeSurface { self.surface }
}

/// Exact return claim for a covered native input owner. The model's top
/// overlay remains the authority; this stores only native focus and its visit.
#[derive(Clone)]
pub(crate) struct TransientFocusReturn {
    focus: Option<FocusHandle>,
    zone: Zone,
    route: Route,
    overlay: Option<Overlay>,
    root: crate::core::VersionedRoot,
    attachment: Option<crate::runtime::store::OwnerAttachment>,
    drawer: bool,
    generation: Option<u64>,
    /// The captured handle is the mounted Find query; its component owns the return.
    find_query: bool,
}

#[derive(Clone)]
struct HintScope {
    route: Route,
    overlay: Option<Overlay>,
    visit: crate::navigation::presentation::VisitId,
    local_input: gpui::NativeActivationScope,
}

struct HintSession {
    mode: HintMode,
    scope: HintScope,
}

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
    /// Last route observed by this window. A Find query refinement changes
    /// the route while keeping the same mounted native editor.
    observed_route: Route,
    /// Previous observed overlay only identifies a fresh keyboard handoff;
    /// the snapshot remains the authority for the current view.
    observed_overlay: Option<Overlay>,
    focus: FocusHandle,
    drawer_focus: FocusHandle,
    titlebar: Entity<Titlebar>,
    shelf: Entity<Shelf>,
    /// The full shelf opened over the reader on a narrow window.
    shelf_over: Entity<Shelf>,
    reader: Entity<Reader>,
    status: Entity<Status>,
    pins: Entity<Pins>,
    ask: Entity<Ask>,
    ask_presentation: AskPresentation,
    motion: Motion,
    zen: bool,
    /// The hand's Row rung is open (H, or the foot's marks).
    hand_open: bool,
    /// The card the keyboard stands on in the open hand (shown order).
    hand_at: usize,
    shelf_over_open: bool,
    drawer_return: Option<TransientFocusReturn>,
    drawer_departing: bool,
    ask_return: Option<TransientFocusReturn>,
    pending_transient_return: Option<TransientFocusReturn>,
    keyboard_claim: Option<KeyboardClaim>,
    keyboard_claim_scheduled: bool,
    /// Changes when the page's input owner changes, never mid-gesture.
    page_input_generation: Option<u64>,
    /// Structural receipt selected from the same clock, never separately advanced.
    local_native_input: gpui::NativeActivationScope,
    /// Last mounted producer receipt; diagnostic observation is deliberately absent.
    /// This shares the input factory's identity, not a second authority or epoch.
    painted_native_input: Option<PageInputScope>,
    /// Changes on every user input to cancel a deferred focus return.
    transient_generation: Option<u64>,
    /// The shelf's width the person has dragged it to, at 100 % text.
    shelf_width: Pixels,
    /// The shell's layout modes (the shelf beside the page, a spine, or a
    /// drawer; the pins column), held through their hysteresis bands.
    modes: Modes,
    zone: Zone,
    hold: RevealHold,
    hold_timer: Option<Task<()>>,
    hints: Option<HintSession>,
    /// The page the keyboard peek shows (Space), while its card is open.
    peeking: Option<PageKey>,
    /// How many peeks are pinned (the pins column exists only for pins).
    pinned: usize,
    ask_open: bool,
    /// True only while the current overlay has a physically mounted result
    /// plate; a just-opened query cannot focus a clipped-away destination.
    ask_results_mounted: bool,
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
            reader: Default::default(),
        };
        let titlebar = new_region(&links, cx, |store| Titlebar::new(links.clone(), store));
        let shelf = new_region(&links, cx, |store| Shelf::new("shelf", links.clone(), store));
        let shelf_over = new_region(&links, cx, |store| Shelf::new("shelf-over", links.clone(), store));
        let reader = new_region(&links, cx, |store| Reader::new(links.clone(), store));
        *links.reader.borrow_mut() = Some(reader.downgrade());
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
            } else if shell.keyboard_claim.is_some() {
                // A native app menu can change the overlay while the window
                // is inactive. Its first active paint schedules the one
                // verification; never focus into an externally blurred window.
                cx.notify();
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
        let input_window = window.window_handle().window_id();
        let keystrokes = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle().window_id() != input_window { return; }
            // Any keystroke is a chord, not a hold: disarm a pending reveal.
            // Bound editor actions run before raw capture listeners, so
            // input intent must retire delayed focus claims here as well.
            let _ = weak.update(cx, |shell, cx| {
                shell.hold.key_down();
                shell.advance_transient_generation();
                // The key dismissing Add belongs to the covered editor;
                // every later key is a new input choice, including Tab.
                if event.keystroke.key == "tab"
                    || shell.links.snapshot(cx).overlay() != Some(Overlay::AddProject)
                {
                    shell.input_left_find_visit(cx);
                }
            });
        });
        // Twins: what a hovered declaration lights elsewhere is drawn above the regions.
        let twins = cx.observe_global::<super::side::twin::Lit>(|_, cx| cx.notify());
        let reduced_motion = system::watch_reduced_motion();
        let system_reduced_motion = reduced_motion.initial();
        let observed_route = links.snapshot(cx).route().clone();
        let observed_overlay = links.snapshot(cx).overlay();
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
            observed_route,
            observed_overlay,
            focus,
            drawer_focus: cx.focus_handle(),
            titlebar,
            shelf,
            shelf_over,
            reader,
            status,
            pins,
            ask,
            ask_presentation: AskPresentation::default(),
            motion: Motion::new(),
            zen: false,
            hand_open: false,
            hand_at: 0,
            shelf_over_open: false,
            drawer_return: None,
            drawer_departing: false,
            ask_return: None,
            pending_transient_return: None,
            keyboard_claim: None,
            keyboard_claim_scheduled: false,
            page_input_generation: Some(0),
            local_native_input: gpui::NativeActivationScope::new(cx.entity_id(), Some(0)),
            painted_native_input: None,
            transient_generation: Some(0),
            shelf_width: geo::SHELF,
            modes: Modes::new(),
            zone: Zone::Reader,
            hold: RevealHold::default(),
            hold_timer: None,
            hints: None,
            peeking: None,
            pinned: 0,
            ask_open: false,
            ask_results_mounted: false,
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
    pub(crate) fn reader_entity(&self) -> Entity<Reader> { self.reader.clone() }

    #[cfg(test)]
    pub(crate) fn shelf_entity(&self) -> Entity<Shelf> { self.shelf.clone() }

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

    #[cfg(test)]
    pub(crate) fn titlebar_target_action(&self, id: &str, cx: &App) -> Option<super::focus::Act> {
        self.titlebar.read(cx).targets.placed().into_iter().find(|(target, _)| target.id == id).map(|(target, _)| target.action.callback())
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
            Zone::Shelf if self.shelf_over_open => self.shelf_over.read(cx).targets.focused(),
            Zone::Shelf => self.shelf.read(cx).targets.focused(),
            Zone::Reader => self.reader.read(cx).targets.focused(),
            Zone::Pins => self.pins.read(cx).targets.focused(),
        };
        (self.zone, focused)
    }

    pub(crate) fn allows_reader_native_return(&self, window: &Window) -> bool {
        window.is_window_active() && self.zone == Zone::Reader && !self.ask_open && self.focus.is_focused(window)
    }

    pub(crate) fn park_retired_reader_focus(&mut self, origin: &FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Option<super::keyboard::NativeReturnLease> {
        if !window.is_window_active() || window.focused(cx).as_ref() != Some(origin)
            || self.ask_open || self.shelf_over_open || !self.background_input_allowed()
            || self.links.snapshot(cx).page_overlay().is_some() || super::titlebar::menu_open(window, cx)
            || !window.is_focus_handle_mounted(&self.focus) { return None; }
        self.set_zone(Zone::Reader, cx);
        self.focus.focus(window, cx);
        super::keyboard::NativeReturnLease::new(window.window_handle().window_id(), self.transient_generation, window.focus_epoch())
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

    #[cfg(test)]
    pub(crate) fn hint_codes(&self) -> Vec<String> {
        self.hints.as_ref().map_or_else(Vec::new, |session| session.mode.visible().map(|(hint, _)| hint.code.clone()).collect())
    }

    #[cfg(test)]
    pub(crate) fn hint_code_for(&self, id: &str) -> Option<String> {
        self.hints.as_ref()?.mode.visible()
            .find(|(hint, _)| hint.target.id == id)
            .map(|(hint, _)| hint.code.clone())
    }

    #[cfg(test)]
    pub(crate) fn hinted_target_for(&self, id: &str) -> Option<Hinted> {
        self.hints.as_ref()?.mode.visible().find(|(hint, _)| hint.target.id == id)
            .map(|(hint, _)| hint.clone())
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

    /// Read after a native frame's paint. It never copies query text or key
    /// characters, and capture writes it only under the input-trace opt-in.
    pub fn keyboard_diagnostic(&self, window: &mut Window, cx: &App) -> KeyboardDiagnostic {
        let input = self.ask.read(cx).input().clone();
        let editor = input.read(cx);
        let editor_focus = editor.focus_handle(cx);
        let overlay = self.links.snapshot(cx).overlay();
        let focus_owner = if editor_focus.is_focused(window) {
            "ask-editor"
        } else if self.focus.is_focused(window) {
            "shell"
        } else if window.focused(cx).is_some() {
            "other"
        } else {
            "none"
        };
        KeyboardDiagnostic {
            shell_frame: self.renders,
            native_frame: window.a11y_frame_number(),
            window_active: window.is_window_active(),
            focus_owner,
            target_mounted: match overlay {
                Some(Overlay::CommandPalette) => window.is_focus_handle_mounted(&editor_focus),
                Some(Overlay::Settings(_)) => window.is_focus_handle_mounted(&self.focus),
                _ => false,
            },
            input_handler_present: window.has_input_handler(),
            editor_chars: (overlay == Some(Overlay::CommandPalette)).then(|| editor.value().chars().count()),
            pending_claim: self.keyboard_claim.is_some(),
        }
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
        // Owner publications may emit only a resource movement (including a
        // repeated failure with identical words). Mount the changed scene at
        // the root before a child can acquire listeners under old press state.
        if self.native_producer_changed(cx) { cx.notify(); }
        match event {
            StoreEvent::Snapshot(Branch::Settings) => {
                self.apply_facet(cx);
                self.project_native_layout(self.links.snapshot(cx).settings().shelf_open, self.zen, window, cx);
                // The shelf toggle moves the columns: the root lays them out.
                cx.notify();
            }
            StoreEvent::Snapshot(Branch::GraphFocus) => cx.notify(),
            StoreEvent::Snapshot(Branch::Overlay) => {
                self.advance_transient_generation();
                self.advance_page_input_generation(InputOwnerChange::Structure, cx);
                // A hint walk belongs to the uncovered page. A pointer can
                // open Ask while hints are active; its editor then owns the
                // next character, not the old hint session.
                self.hints = None;
                if self.links.snapshot(cx).overlay().is_some() {
                    self.reader
                        .update(cx, |reader, _| reader.cancel_native_return());
                }
                self.sync_overlay(window, cx);
            }
            StoreEvent::Snapshot(Branch::Route) => {
                self.advance_transient_generation();
                self.advance_page_input_generation(InputOwnerChange::Structure, cx);
                self.pending_transient_return = None;
                let snapshot = self.links.snapshot(cx);
                let route = snapshot.route().clone();
                let keeps_native_editor = super::reader::find_refinement(&self.observed_route, &route)
                    && snapshot.overlay().is_none();
                self.observed_route = route.clone();
                // Ask preview routes are covered visits, not committed
                // navigation. Keep the origin receipt until the preview
                // reducer restores it; committed departure revokes it.
                if snapshot.session().preview.is_none() {
                    if self.drawer_return.as_ref().is_some_and(|saved| &saved.route != snapshot.route()) { self.drawer_return = None; }
                    if self.ask_return.as_ref().is_some_and(|saved| &saved.route != snapshot.route()) { self.ask_return = None; }
                }
                // A preview changes the Reader while Ask retains keyboard ownership.
                if snapshot.overlay() != Some(Overlay::CommandPalette)
                    && !super::bodies::graph::is_graph(&route)
                    && !keeps_native_editor
                {
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
                    let reader = self.reader.clone();
                    cx.defer(move |cx| {
                        reader.update(cx, |reader, cx| {
                            reader.request_native_return(route, id, cx);
                        });
                    });
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
            StoreEvent::Snapshot(Branch::Root) => cx.notify(),
            StoreEvent::Snapshot(_) => {}
            StoreEvent::PackagesPublished(_) => {}
        }
    }

    fn request_keyboard_claim(
        &mut self,
        overlay: Overlay,
        target: FocusHandle,
        origin: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.links.snapshot(cx);
        let Some(mut claim) = KeyboardClaim::new(
            overlay,
            snapshot.session().reading.current.id,
            window.window_handle().window_id(),
            self.transient_generation,
            window.focus_epoch(),
            target,
            origin,
        ) else { return; };
        if window.is_window_active()
            && claim.may_reassert(window.focused(cx).as_ref())
        {
            claim.target().focus(window, cx);
        }
        claim.record_request(window.focus_epoch());
        self.keyboard_claim = Some(claim);
        self.keyboard_claim_scheduled = false;
    }

    /// One post-paint admission. The Shell ancestor contains Ask's input only
    /// when the current rendered dispatch tree mounted that exact handle.
    fn verify_keyboard_claim(&mut self, expected: &KeyboardClaim, window: &mut Window, cx: &mut Context<Self>) {
        if !self.keyboard_claim.as_ref().is_some_and(|claim| claim.same_request(expected)) { return; }
        self.keyboard_claim_scheduled = false;
        if !window.is_window_active() { return; }
        let Some(claim) = self.keyboard_claim.take() else { return; };
        let snapshot = self.links.snapshot(cx);
        if !claim.current(
            snapshot.overlay(),
            snapshot.session().reading.current.id,
            window.window_handle().window_id(),
            self.transient_generation,
        ) { return; }
        let mounted = window.is_focus_handle_mounted(claim.target())
            && (claim.target() == &self.focus || self.focus.contains(claim.target(), window));
        if !mounted
            || !claim.unchanged_focus(window.focus_epoch())
            || !claim.may_reassert(window.focused(cx).as_ref()) { return; }
        if !claim.target().is_focused(window) {
            claim.target().focus(window, cx);
        }
    }

    fn sync_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        let previous_overlay = std::mem::replace(&mut self.observed_overlay, snapshot.overlay());
        let entering_settings = !matches!(previous_overlay, Some(Overlay::Settings(_)))
            && matches!(snapshot.overlay(), Some(Overlay::Settings(_)));
        let leaving_settings = matches!(previous_overlay, Some(Overlay::Settings(_)))
            && snapshot.overlay().is_none();
        // Capture before Ask, Add, or Settings changes native focus. This is
        // still the old view's return origin, even if its handle has left the
        // rendered tree by the time the new view finishes painting.
        let origin = window.focused(cx);
        if entering_settings && self.reader.update(cx, |reader, cx| reader.capture_settings_native_origin(origin.as_ref(), cx)) {
            self.set_zone(Zone::Reader, cx);
        }
        let wants_ask = snapshot.overlay() == Some(Overlay::CommandPalette);
        let opening = wants_ask && !self.ask_open;
        let fresh_ask = opening && self.ask_return.is_none();
        if wants_ask != self.ask_open {
            self.ask_open = wants_ask;
            self.ask_results_mounted = false;
            if wants_ask {
                if self.ask_return.is_none() {
                    let mut saved = self.capture_transient_return(snapshot.session().covered_overlay(), window, cx);
                    // A component query can own native focus while the Shell's
                    // last zone still belongs to the Shelf that opened Find.
                    // Its mounted proof, rather than that zone's target list,
                    // owns the return just as it does for an Add cover.
                    saved.find_query = self.reader.update(cx, |reader, cx|
                        reader.begin_mounted_find_focus_return(saved.focus.clone(), cx));
                    self.ask_return = Some(saved);
                }
            } else {
                // Retire the old Ask owner before a newly opened dialog
                // captures focus; never overwrite that dialog's focus later.
                self.focus.focus(window, cx);
            }
        }
        let mut before = self.capture_transient_return(snapshot.session().covered_overlay(), window, cx);
        if snapshot.overlay() == Some(Overlay::AddProject) {
            let focused = before.focus.clone();
            before.find_query = self.reader.update(cx, |reader, cx|
                reader.begin_mounted_find_focus_return(focused, cx));
        }
        let dialog_return = super::onboard::sync(&self.links, before, window, cx);
        if wants_ask {
            if fresh_ask {
                self.ask.update(cx, |ask, cx| ask.opened(window, cx));
            }
        } else if !snapshot.session().overlay_is_covered(Overlay::CommandPalette) {
            if let Some(saved) = self.ask_return.take() {
                if saved.overlay == snapshot.overlay() { self.queue_transient_return(saved, window, cx); }
            }
        }
        let keyboard_request = match (snapshot.overlay(), opening, entering_settings) {
            (Some(Overlay::CommandPalette), true, _) => {
                let input = self.ask.read(cx).input().clone();
                Some((Overlay::CommandPalette, input.read(cx).focus_handle(cx)))
            }
            (Some(overlay @ Overlay::Settings(_)), _, true)
                if !super::titlebar::menu_open(window, cx) => {
                // A menu retained above Settings remains the top keyboard
                // owner. Stealing its focus makes its Escape binding
                // unreachable while the Shell also steps aside for Menu.
                Some((overlay, self.focus.clone()))
            }
            _ => None,
        };
        if let Some((overlay, target)) = keyboard_request {
            self.request_keyboard_claim(overlay, target, origin, window, cx);
        }
        match dialog_return {
            super::onboard::FocusHandoff::None => {}
            super::onboard::FocusHandoff::Restore(saved) => self.queue_transient_return(saved, window, cx),
            super::onboard::FocusHandoff::Landed => {
                // The submitted field is gone. Give the live window a key
                // receiver before another local shortcut can arrive.
                if self.links.snapshot(cx).overlay().is_none() && self.background_input_allowed() {
                    self.focus.focus(window, cx);
                }
            }
        }
        if leaving_settings && window.is_window_active() && !super::titlebar::menu_open(window, cx) {
            // The selected radio/editor belongs to the retired Settings page.
            // Keep global dispatch reachable while the destination mounts.
            self.keyboard_claim = None;
            self.keyboard_claim_scheduled = false;
            self.focus.focus(window, cx);
            if let Some(lease) = super::keyboard::NativeReturnLease::new(
                window.window_handle().window_id(), self.transient_generation, window.focus_epoch(),
            ) {
                let reader = self.reader.clone();
                let shell = cx.weak_entity();
                let generation = self.transient_generation;
                // Both Region and Shell subscribe to the same store. Arm only
                // after every subscriber has observed this uncovered place.
                cx.defer(move |cx| {
                    if shell.upgrade().is_some_and(|shell| shell.read(cx).focus_return_generation() == generation) {
                        reader.update(cx, |reader, cx| reader.arm_settings_focus_return(lease, cx));
                    }
                });
            }
        }
        cx.notify();
    }

    pub(crate) fn capture_transient_return(&self, overlay: Option<Overlay>, window: &Window, cx: &App) -> TransientFocusReturn {
        let snapshot = self.links.snapshot(cx);
        TransientFocusReturn { focus: window.focused(cx), zone: self.zone,
            route: snapshot.route().clone(), overlay, root: snapshot.key(),
            attachment: self.links.store.read(cx).current_owner_attachment(), drawer: self.shelf_over_open,
            generation: self.transient_generation, find_query: false }
    }

    pub(crate) fn queue_transient_return(&mut self, mut saved: TransientFocusReturn, window: &mut Window, cx: &mut Context<Self>) {
        saved.generation = self.transient_generation;
        if saved.find_query {
            // The exact native query reports its own mounted handle after the
            // uncovered frame. A generic target-list return would select a
            // Shelf row while that child field is still mounting.
            if self.links.snapshot(cx).overlay().is_none() && self.background_input_allowed() {
                self.focus.focus(window, cx);
            }
            if self.return_identity_current(&saved, cx) {
                let input_generation = self.transient_generation;
                if self.reader.update(cx, |reader, _| reader.arm_find_focus_return_after_cover(
                    saved.focus.as_ref(), input_generation,
                )) {
                    self.pending_transient_return = None;
                    self.set_zone(Zone::Reader, cx);
                    cx.notify();
                }
            }
            return;
        }
        // A dismissed native subtree cannot remain the keyboard dispatch
        // owner while its underlay waits for its first uncovered paint.
        if !matches!(saved.overlay, Some(Overlay::CommandPalette | Overlay::AddProject)) {
            if saved.drawer { self.drawer_focus.focus(window, cx); }
            else { self.focus.focus(window, cx); }
        }
        self.pending_transient_return = Some(saved);
        cx.notify();
    }

    fn advance_transient_generation(&mut self) {
        self.transient_generation = self.transient_generation.and_then(|generation| generation.checked_add(1));
        self.pending_transient_return = None;
    }

    fn advance_page_input_generation(&mut self, change: InputOwnerChange, cx: &mut Context<Self>) {
        self.page_input_generation = self.page_input_generation.and_then(|generation| generation.checked_add(1));
        if matches!(change, InputOwnerChange::Structure) || self.page_input_generation.is_none() {
            self.local_native_input = gpui::NativeActivationScope::new(cx.entity_id(), self.page_input_generation);
        }
        // Cached columns need fresh callbacks when native scene ownership changes.
        // An old callback retains its old scope; only a new paint gets this epoch.
        self.shelf.update(cx, |_, cx| cx.notify());
        self.shelf_over.update(cx, |_, cx| cx.notify());
        self.reader.update(cx, |_, cx| cx.notify());
    }

    // Resolve intent-time ownership with the same frame factory as paint. Keeping
    // the projected frame here records a hide/show cycle even if neither state
    // is painted; it is not another native input clock or producer authority.
    fn project_native_layout(&mut self, shelf_open: bool, zen: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(previous) = self.frame else { return; };
        let frame = Frame::resolve(FrameInput {
            window: previous.room,
            shelf_open,
            zen,
            shelf_width: self.shelf_width,
            pinned: self.pinned > 0,
        }, &self.modes);
        let old = (ShelfNativeSurface::of(Some(previous), self.shelf_over_open), previous.pins);
        let new = (ShelfNativeSurface::of(Some(frame), self.shelf_over_open), frame.pins);
        self.frame = Some(frame);
        if old != new {
            self.advance_page_input_generation(InputOwnerChange::Structure, cx);
        }
        self.rehome_unavailable_zone(old.0 != new.0, window, cx);
    }

    fn toggle_zen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.zen = !self.zen;
        self.project_native_layout(self.links.snapshot(cx).settings().shelf_open, self.zen, window, cx);
        cx.notify();
    }

    fn return_identity_current(&self, saved: &TransientFocusReturn, cx: &App) -> bool {
        let snapshot = self.links.snapshot(cx);
        snapshot.route() == &saved.route && snapshot.overlay() == saved.overlay
            && snapshot.key() == saved.root
            && self.links.store.read(cx).current_owner_attachment() == saved.attachment
            && self.shelf_over_open == saved.drawer
            && saved.generation.is_some() && saved.generation == self.transient_generation
    }

    fn flush_transient_return(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = self.pending_transient_return.take() else { return; };
        if !self.return_identity_current(&saved, cx) { return; }
        if self.drawer_departing || (!self.ask_open && !self.background_input_allowed()) {
            self.pending_transient_return = Some(saved);
            return;
        }
        let expected = window.focused(cx);
        let weak = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let _ = weak.update(cx, |shell, cx| {
                if !shell.return_identity_current(&saved, cx) || shell.drawer_departing || (!shell.ask_open && !shell.background_input_allowed()) || window.focused(cx) != expected { return; }
                let zone = shell.available_zone(saved.zone, saved.focus.as_ref(), window, cx);
                shell.set_zone(zone, cx);
                if saved.overlay == Some(Overlay::CommandPalette) {
                    let input = shell.ask.read(cx).input().clone();
                    input.update(cx, |input, cx| input.focus(window, cx));
                } else if saved.overlay == Some(Overlay::AddProject) {
                    super::onboard::focus_current(window, cx);
                } else if zone == saved.zone && let Some(handle) = saved.focus {
                    let mounted = match zone {
                        Zone::Titlebar => shell.titlebar.read(cx).targets.contains_native_handle(&handle),
                        Zone::Shelf if saved.drawer => shell.shelf_over.read(cx).targets.contains_native_handle(&handle),
                        Zone::Shelf => shell.shelf.read(cx).targets.contains_native_handle(&handle),
                        Zone::Reader => shell.reader.read(cx).targets.contains_native_handle(&handle),
                        Zone::Pins => shell.pins.read(cx).targets.contains_native_handle(&handle),
                    };
                    if mounted { handle.focus(window, cx); }
                }
                if zone != saved.zone && saved.overlay != Some(Overlay::CommandPalette)
                    && saved.overlay != Some(Overlay::AddProject) {
                    shell.focus.focus(window, cx);
                }
                // The old handle may no longer be mounted or may still be
                // inert; keep a valid shell owner rather than a dead field.
                if window.focused(cx).is_none() { shell.focus.focus(window, cx); }
            });
        });
    }

    pub(crate) fn shelf_input_owner(&self, overlay: bool) -> bool {
        !self.ask_open && self.background_input_allowed()
            && if overlay { self.shelf_over_open } else {
                !self.shelf_over_open && self.frame.is_some_and(|frame| frame.shelf == ShelfMode::Shelf)
            }
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
    pub(crate) fn toggle_shelf(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.advance_transient_generation();
        if self.frame.is_some_and(|frame| frame.shelf_overlays) {
            self.advance_page_input_generation(InputOwnerChange::Structure, cx);
            self.shelf_over_open = !self.shelf_over_open;
            cx.notify();
        } else {
            let shelf_open = !self.links.snapshot(cx).settings().shelf_open;
            self.project_native_layout(shelf_open, self.zen, window, cx);
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
            Zone::Shelf if self.shelf_over_open => self.shelf_over.update(cx, |region, _| f(&mut region.targets)),
            Zone::Shelf => self.shelf.update(cx, |region, _| f(&mut region.targets)),
            Zone::Reader => self.reader.update(cx, |region, _| f(&mut region.targets)),
            Zone::Pins => self.pins.update(cx, |region, _| f(&mut region.targets)),
        }
    }

    fn notify_zone(&mut self, zone: Zone, cx: &mut Context<Self>) {
        match zone {
            Zone::Titlebar => self.titlebar.update(cx, |_, cx| cx.notify()),
            Zone::Shelf if self.shelf_over_open => self.shelf_over.update(cx, |region, cx| {
                region.reveal_focused(); cx.notify();
            }),
            Zone::Shelf => self.shelf.update(cx, |region, cx| {
                region.reveal_focused(); cx.notify();
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
        self.reader.update(cx, |reader, _| reader.cancel_native_return());
        self.adopt_reader_native_zone(window, cx);
        if let Some(key) = self.peeking.take() {
            // The keyboard's peek belongs to where the keyboard stood.
            float::close(&peeks::float_key(&key), window, cx);
            cx.notify();
        }
        if self.with_zone(cx, |targets| targets.walk(delta)) {
            if self.zone != Zone::Reader
                || !self.reader.update(cx, |reader, cx| reader.focus_native_current(window, cx))
            {
                self.focus.focus(window, cx);
            }
            self.notify_zone(self.zone, cx);
        }
    }

    fn current(&mut self, cx: &mut Context<Self>) -> Option<Target> {
        self.with_zone(cx, |targets| targets.current())
    }

    fn zone_available(&self, zone: Zone) -> bool {
        match zone {
            Zone::Shelf => self.shelf_over_open || self.frame.map_or(true, |frame| frame.shelf != ShelfMode::Hidden),
            Zone::Pins => self.frame.is_some_and(|frame| frame.pins),
            Zone::Titlebar | Zone::Reader => true,
        }
    }

    fn available_zone(&self, zone: Zone, focus: Option<&FocusHandle>, window: &Window, cx: &App) -> Zone {
        if !self.zone_available(zone) { return Zone::Reader; }
        let targets = match zone {
            Zone::Shelf if self.shelf_over_open => Some(self.shelf_over.read(cx).targets.clone()),
            Zone::Shelf => Some(self.shelf.read(cx).targets.clone()),
            // Pins' native cards belong to W-Float, not this empty Targets
            // list. Their column's frame visibility is the Shell boundary.
            Zone::Pins | Zone::Titlebar | Zone::Reader => None,
        };
        if let Some(targets) = targets {
            let logical = targets.focused().is_some_and(|id|
                targets.admits_hint(&id, targets.hint_frame()));
            let owns = match focus {
                Some(handle) if handle == &self.focus || handle == &self.drawer_focus => logical,
                Some(handle) => targets.contains_native_handle(handle)
                    && window.is_focus_handle_mounted(handle),
                None => logical,
            };
            if !owns { return Zone::Reader; }
        }
        zone
    }

    fn rehome_unavailable_zone(&mut self, shelf_surface_changed: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.drawer_departing || self.shelf_over_open { return; }
        if !(self.zone == Zone::Shelf && shelf_surface_changed)
            && self.available_zone(self.zone, window.focused(cx).as_ref(), window, cx) == self.zone { return; }
        let focused = window.focused(cx);
        let hidden_owner = focused.as_ref().is_some_and(|handle|
            self.with_zone(cx, |targets| targets.contains_native_handle(handle)));
        let needs_native_owner = focused.is_none() || self.focus.is_focused(window) || hidden_owner
            || focused.as_ref().is_some_and(|handle| !window.is_focus_handle_mounted(handle));
        self.set_zone(Zone::Reader, cx);
        // Preserve any other mounted native owner, including a floating pin
        // or titlebar control. Only the old custom hand or an orphaned handle
        // moves to the Reader's persistent Shell owner.
        if !self.ask_open && !self.shelf_over_open && self.background_input_allowed()
            && self.links.snapshot(cx).overlay() != Some(Overlay::AddProject)
            && !super::titlebar::menu_open(window, cx) && !window.has_focused_input(cx)
            && needs_native_owner {
            self.focus.focus(window, cx);
        }
    }

    fn visible_zones(&self) -> Vec<Zone> {
        Zone::ALL.into_iter().filter(|zone| self.zone_available(*zone)).collect()
    }

    /// Tab: the next zone takes the keyboard.
    pub fn cycle_zone(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_transient_return = None;
        if self.shelf_over_open {
            self.zone = Zone::Shelf;
            if forward { window.focus_next(cx); } else { window.focus_prev(cx); }
            return;
        }
        self.reader.update(cx, |reader, _| reader.cancel_native_return());
        self.adopt_reader_native_zone(window, cx);
        if self.zone == Zone::Reader
            && self.reader.update(cx, |reader, cx| reader.step_native(forward, window, cx))
        {
            self.notify_zone(Zone::Reader, cx);
            return;
        }
        let zones = self.visible_zones();
        let at = zones.iter().position(|zone| *zone == self.zone).unwrap_or(0);
        let next = if forward {
            zones[(at + 1) % zones.len()]
        } else {
            zones[(at + zones.len() - 1) % zones.len()]
        };
        self.set_zone(next, cx);
        if next == Zone::Titlebar {
            // Titlebar FACET controls own real tab stops; hand the first
            // native stop input instead of leaving the dispatch root focused.
            self.focus.focus(window, cx);
            if forward { window.focus_next(cx); } else { window.focus_prev(cx); }
        } else if next != Zone::Reader
            || !self.reader.update(cx, |reader, cx| reader.focus_native_current(window, cx))
        {
            self.focus.focus(window, cx);
        }
    }

    /// The mounted native handle is the input origin even when a pointer or
    /// accessibility client moved it without a Shell zone action.
    fn adopt_reader_native_zone(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.ask_open || self.shelf_over_open || self.links.snapshot(cx).overlay() == Some(Overlay::AddProject) || !self.background_input_allowed() || super::titlebar::menu_open(window, cx) {
            return;
        }
        if let Some(changed) = self.reader.read(cx).adopt_mounted_native_focus(window, cx) {
            let already_reader = self.zone == Zone::Reader;
            self.set_zone(Zone::Reader, cx);
            if already_reader && changed { self.notify_zone(Zone::Reader, cx); }
        }
    }

    /// A custom navigation zone shares the shell's persistent native focus
    /// owner. A clicked row can disappear as it changes the list; that must
    /// not leave keyboard dispatch attached to the retired row.
    pub(crate) fn take_zone(&mut self, zone: Zone, window: &mut Window, cx: &mut Context<Self>) {
        self.set_zone(zone, cx);
        if zone == Zone::Shelf && self.shelf_over_open { self.drawer_focus.focus(window, cx); }
        else { self.focus.focus(window, cx); }
    }

    /// Updates the active targets inside the persistent keyboard owner.
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
        // The root's ReaderViewport key context follows the custom zone.
        if old == Zone::Reader || zone == Zone::Reader { cx.notify(); }
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // ↵ in the open hand goes to the card it stands on.
        if self.hand_open && self.page_keyboard_session_owns(window, cx) && self.hand_key("enter", cx) {
            return;
        }
        // A native Shelf button receives GPUI's Enter press/release Click
        // when it is also the selected target. After Down moved the custom
        // hand, its old parked native handle may still be focused: move to
        // the Shell owner before invoking the newly selected row, so KeyUp
        // cannot also click the old button.
        if self.zone == Zone::Shelf {
            let (selected, native) = self.with_zone(cx, |targets|
                (targets.focused(), targets.native_focused(window)));
            if let Some(native) = native {
                if selected.as_ref() == Some(&native) { return; }
                self.focus.focus(window, cx);
            }
        }
        if let Some(target) = self.current(cx) {
            run(target.action.callback(), window, cx);
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
        let zone = self.zone;
        let Some(mount) = self.hinted_targets(zone, cx).mount_claim(&target.id) else { return; };
        let Some(input) = self.target_input_claim(window) else { return; };
        let scope = self.target_scope(cx);
        let shell = cx.weak_entity();
        window.defer(cx, move |window, app| {
            let Some(shell) = shell.upgrade() else { return; };
            if !shell.read(app).target_claim_current(zone, &mount, &scope, &input, true, window, app)
                || !target.action.admits(app) { return; }
            let Some(anchor) = ({
                let shell = shell.read(app);
                shell.target_claim_current(zone, &mount, &scope, &input, true, window, app)
                    .then(|| shell.hinted_targets(zone, app).focused_bounds()).flatten()
            }) else { return; };
            let store = shell.read(app).links.store.clone();
            store.update(app, |store, cx| store.ensure(key.clone(), cx));
            let armed = shell.update(app, |shell, cx| {
                if !shell.target_claim_current(zone, &mount, &scope, &input, true, window, cx) { return false; }
                shell.peeking = Some(key.clone());
                cx.notify();
                true
            });
            if !armed { return; }
            let request = peeks::request(key.clone(), target.label, anchor, store);
            float::open(request, window, app);
        });
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
        if self.request_graph_view(View::Code, cx) { return; }
        if !self.mode_input_allowed(cx) { return; }
        let Some(target) = self.current(cx) else {
            let Some(input) = self.page_input_scope(cx) else { return; };
            let Some(symbol) = route_symbol(self.links.snapshot(cx).route()) else { return; };
            let shell = cx.weak_entity();
            let links = self.links.clone();
            window.defer(cx, move |_, app| {
                let Some(shell) = shell.upgrade() else { return; };
                if shell.read(app).admits_page_input_scope(&input, app)
                    && shell.read(app).mode_input_allowed(app)
                    && route_symbol(links.snapshot(app).route()).as_ref() == Some(&symbol) {
                    dispatch_symbol_code(&links, symbol, app);
                }
            });
            return;
        };
        let zone = self.zone;
        let Some(mount) = self.hinted_targets(zone, cx).mount_claim(&target.id) else { return; };
        let Some(input) = self.target_input_claim(window) else { return; };
        let scope = self.target_scope(cx);
        let fallback_input = self.page_input_scope(cx);
        let shell = cx.weak_entity();
        let links = self.links.clone();
        window.defer(cx, move |window, app| {
            let Some(shell) = shell.upgrade() else { return; };
            if !shell.read(app).mode_input_allowed(app)
                || !shell.read(app).target_claim_current(zone, &mount, &scope, &input, true, window, app)
                || !target.action.admits(app) { return; }
            if !shell.read(app).mode_input_allowed(app)
                || !shell.read(app).target_claim_current(zone, &mount, &scope, &input, true, window, app) { return; }
            // S on an unrelated focused control may still mean this page's
            // own declaration. That fallback is a producer action, so it
            // cannot borrow the control's potentially local admission.
            let symbol = target.source.or_else(|| {
                fallback_input.as_ref()
                    .filter(|input| shell.read(app).admits_page_input_scope(input, app))
                    .and_then(|_| route_symbol(links.snapshot(app).route()))
            });
            if let Some(symbol) = symbol { dispatch_symbol_code(&links, symbol, app); }
        });
    }

    /// All graph-to-declaration controls ask the typed UI owner to open the
    /// current graph selection. It captures the exact selection and owner
    /// attachment, and the Map rechecks both before resolving a read. The
    /// Reader's painted-graph receipt also fences owner replacement before
    /// a new scene has claimed the current attachment.
    fn request_graph_view(&mut self, view: View, cx: &mut Context<Self>) -> bool {
        let snapshot = self.links.snapshot(cx);
        if super::bodies::graph::is_graph(snapshot.route()) {
            if !self.page_input_allowed(cx) || snapshot.page_overlay().is_some() { return true; }
            let (eligibility, owner_attached) = {
                let store = self.links.store.read(cx);
                (store.graph_view_eligibility(), store.current_owner_attachment().is_some())
            };
            if let Some(message) = eligibility.guidance() {
                self.graph_view_notice(message, cx);
                return true;
            }
            if !owner_attached {
                self.graph_view_notice("The local index owner is unavailable. This graph selection cannot open Page or Code yet.", cx);
                return true;
            }
            if !self.mode_input_allowed(cx) {
                self.graph_view_notice("The graph is settling. Choose Page or Code again when its current view appears.", cx);
                return true;
            }
            self.links.dispatch_graph_view(view, cx);
            return true;
        }
        false
    }

    fn graph_view_notice(&mut self, message: &str, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        let notice = crate::runtime::graph_focus::Notice {
            visit: snapshot.route().clone(), root: snapshot.key(), message: message.into(), retry: None,
        };
        self.links.store.update(cx, |store, cx| store.set_notice(Some(notice), cx));
    }

    /// One synchronous boundary for a painted header control. The captured
    /// JumpVisit is checked by Titlebar before this call; keyboard depth and
    /// pointer activation then share the same graph request below.
    pub(crate) fn header_view(&mut self, view: View, cx: &mut Context<Self>) {
        if self.request_graph_view(view, cx) { return; }
        if view == View::Graph {
            if self.local_navigation_allowed(cx) && matches!(self.links.snapshot(cx).route(), Route::Symbol(_)) {
                self.links.dispatch(Intent::SetView(View::Graph), cx);
            }
        } else if self.mode_input_allowed(cx) && matches!(self.links.snapshot(cx).route(), Route::Symbol(_)) {
            self.links.dispatch(Intent::SetView(view), cx);
        }
    }

    /// G enters the graph, or opens the graph's current symbol page.
    fn toggle_graph(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        let local_navigation = matches!(snapshot.route(), Route::Orbit(_) | Route::Package(_) | Route::World)
            || matches!(snapshot.route(), Route::Symbol(route) if route.view != View::Graph);
        if local_navigation && !self.local_navigation_allowed(cx) { return; }
        if super::bodies::graph::is_graph(snapshot.route())
            && (self.reader.read(cx).graph_focused(cx) || matches!(snapshot.route(), Route::Symbol(_)))
        {
            self.request_graph_view(View::Page, cx);
            return;
        }
        let intent = match snapshot.route() {
            Route::Symbol(_) => Intent::SetView(View::Graph),
            Route::World => Intent::Back,
            Route::CargoSource(_) => return,
            Route::Orbit(_) | Route::Package(_) => {
                self.reader.update(cx, |reader, cx| reader.reset_world(cx));
                Intent::Navigate(Route::World)
            },
        };
        self.links.dispatch(intent, cx);
    }

    /// ⌘.: a declaration's code ↔ its page.
    fn code_page(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.request_graph_view(View::Code, cx) { return; }
        if !self.mode_input_allowed(cx) { return; }
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
        if !self.page_input_allowed(cx) { return; }
        let scope = self.target_scope(cx);
        let mut placed = Vec::new();
        let mut add = |zone: Zone, targets: &super::focus::Targets| {
            placed.extend(targets.placed().into_iter().filter_map(|(target, bounds)| {
                let mount = targets.mount_claim(&target.id)?;
                Some((zone, mount, target, bounds))
            }));
        };
        add(Zone::Titlebar, &self.titlebar.read(cx).targets);
        if self.frame.is_some_and(|frame| frame.shelf == ShelfMode::Shelf) {
            add(Zone::Shelf, &self.shelf.read(cx).targets);
        }
        add(Zone::Reader, &self.reader.read(cx).targets);
        if self.frame.is_some_and(|frame| frame.pins) {
            add(Zone::Pins, &self.pins.read(cx).targets);
        }
        if placed.is_empty() {
            return;
        }
        self.hints = Some(HintSession { mode: HintMode::new(placed), scope });
        cx.notify();
    }

    fn target_scope(&self, cx: &App) -> HintScope {
        let snapshot = self.links.snapshot(cx);
        HintScope {
            route: snapshot.route().clone(),
            overlay: snapshot.overlay(),
            visit: snapshot.session().reading.current.id,
            local_input: self.local_activation_scope(),
        }
    }

    fn hint_scope_current(&self, scope: &HintScope, cx: &App) -> bool {
        self.page_input_allowed(cx) && self.target_structure_current(scope, cx)
    }

    /// Hints and the open Hand are keyboard sessions on the uncovered page.
    /// A mounted editor or local overlay owns its own keys, even if a Hand
    /// row remains painted beneath it or a hint session has not retired yet.
    fn page_keyboard_session_owns(&self, window: &mut Window, cx: &mut App) -> bool {
        self.page_input_allowed(cx)
            && !super::titlebar::menu_open(window, cx)
            && (window.root::<gpui_component::Root>().flatten().is_none()
                || !window.has_focused_input(cx))
    }

    fn target_structure_current(&self, scope: &HintScope, cx: &App) -> bool {
        let snapshot = self.links.snapshot(cx);
        self.local_activation_scope() == scope.local_input
            && snapshot.route() == &scope.route
            && snapshot.overlay() == scope.overlay
            && snapshot.session().reading.current.id == scope.visit
    }

    fn hinted_targets(&self, zone: Zone, cx: &App) -> super::focus::Targets {
        match zone {
            Zone::Titlebar => self.titlebar.read(cx).targets.clone(),
            Zone::Shelf if self.shelf_over_open => self.shelf_over.read(cx).targets.clone(),
            Zone::Shelf => self.shelf.read(cx).targets.clone(),
            Zone::Reader => self.reader.read(cx).targets.clone(),
            Zone::Pins => self.pins.read(cx).targets.clone(),
        }
    }

    fn target_input_claim(&self, window: &Window) -> Option<super::keyboard::NativeReturnLease> {
        super::keyboard::NativeReturnLease::new(
            window.window_handle().window_id(), self.transient_generation, window.focus_epoch(),
        )
    }

    fn target_claim_current(&self, zone: Zone, claim: &super::focus::TargetMountClaim, scope: &HintScope, input: &super::keyboard::NativeReturnLease, must_be_focused: bool, window: &Window, cx: &App) -> bool {
        if !input.current(window.window_handle().window_id(), self.transient_generation, window.focus_epoch())
            || !window.is_focus_handle_mounted(&self.focus)
            || !self.target_structure_current(scope, cx) { return false; }
        let targets = self.hinted_targets(zone, cx);
        targets.admits_mount(claim, window)
            && (!must_be_focused || (self.zone == zone && targets.focused().as_deref() == Some(claim.id())))
    }

    fn activate_hint(&mut self, choice: Hinted, scope: &HintScope, window: &mut Window, cx: &mut Context<Self>) {
        let shell = cx.weak_entity();
        let scope = scope.clone();
        let Some(input) = self.target_input_claim(window) else { return; };
        window.defer(cx, move |window, app| {
            let Some(shell) = shell.upgrade() else { return; };
            if !shell.read(app).hint_scope_current(&scope, app)
                || !shell.read(app).target_claim_current(choice.zone, &choice.mount, &scope, &input, false, window, app)
                || !choice.target.action.admits(app) { return; }
            // Admission may itself update Reader state. Recheck the exact
            // structural visit and continuously mounted control before native focus.
            let focused = shell.update(app, |shell, cx| {
                if !shell.hint_scope_current(&scope, cx)
                    || !shell.target_claim_current(choice.zone, &choice.mount, &scope, &input, false, window, cx) { return false; }
                let targets = shell.hinted_targets(choice.zone, cx);
                shell.set_zone(choice.zone, cx);
                targets.focus(choice.target.id.clone());
                if !targets.focus_native(&choice.target.id, window, cx) {
                    // A raw target keeps the Shell's native keyboard owner.
                    // Popups opened below may replace it with their own.
                    shell.focus.focus(window, cx);
                }
                true
            });
            if focused { choice.target.action.run(window, app); }
        });
    }

    /// An input outside an open Ask leaves the interrupted Find visit. A Tab
    /// or pointer inside Ask's editor and results belongs to that detour.
    fn input_left_find_visit(&mut self, cx: &mut Context<Self>) {
        if !self.ask_open {
            self.reader.update(cx, |reader, _| reader.cancel_find_focus_return());
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.ask_presentation.blocks_background_input(self.ask_open)
            && self.links.snapshot(cx).overlay() != Some(Overlay::AddProject) {
            // The exit's painted plate still covers the page. The shell's
            // key context gates its actions; this also stops raw child keys.
            // A new Ask and the platform's close shortcuts stay available.
            let key = event.keystroke.key.as_str();
            let shortcut = event.keystroke.modifiers.secondary();
            if !(shortcut && matches!(key, "k" | "q" | "w")) {
                cx.stop_propagation();
            }
            return;
        }
        if self.ask_open {
            let key = event.keystroke.key.as_str();
            if !event.keystroke.modifiers.modified()
                && matches!(key, "up" | "down")
                && self.ask_results_mounted
                && self.ask.read(cx).owns_focus(window, cx)
            {
                let delta = if key == "up" { -1 } else { 1 };
                self.ask.update(cx, |ask, cx| ask.step(delta, window, cx));
                cx.stop_propagation();
                return;
            }
        }
        // This is the Shell's capture phase, before a focused input's raw
        // key/character delivery. Admit both transient keyboard sessions
        // from the same current owner before either can stop propagation.
        if !self.page_keyboard_session_owns(window, cx) {
            if self.hints.take().is_some() { cx.notify(); }
            return;
        }
        if self.hints.is_none() && self.hand_open && self.hand_key(event.keystroke.key.as_str(), cx) {
            cx.stop_propagation();
            return;
        }
        // A route or native target change can revoke a hint without leaving
        // the uncovered page. Do not consume the key that discovers this.
        if self.hints.as_ref().is_some_and(|session| !self.hint_scope_current(&session.scope, cx)) {
            self.hints = None;
            cx.notify();
            return;
        }
        let Some(session) = self.hints.as_mut() else {
            return;
        };
        let key = event.keystroke.key.as_str();
        cx.stop_propagation();
        if key == "backspace" {
            if !session.mode.backspace() {
                self.hints = None;
            }
            cx.notify();
            return;
        }
        let mut chars = key.chars();
        let (Some(character), None) = (chars.next(), chars.next()) else {
            return;
        };
        let scope = session.scope.clone();
        match session.mode.key(character) {
            Step::Narrowed => {}
            Step::Chosen(choice) => {
                self.hints = None;
                self.activate_hint(choice, &scope, window, cx);
            }
            Step::Missed => self.hints = None,
        }
        cx.notify();
    }

    fn background_input_allowed(&self) -> bool {
        !self.drawer_departing && !self.ask_presentation.blocks_background_input(self.ask_open)
    }

    pub(crate) const fn focus_return_generation(&self) -> Option<u64> {
        self.transient_generation
    }

    pub(crate) fn local_activation_scope(&self) -> gpui::NativeActivationScope {
        self.local_native_input
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_find_return(&self, cx: &App) -> (Option<u64>, bool) {
        (self.transient_generation, self.reader.read(cx).diagnostic_find_return_pending())
    }

    pub(crate) fn admits_local_activation_scope(&self, scope: gpui::NativeActivationScope, cx: &App) -> bool {
        self.local_native_input == scope && self.page_input_allowed(cx)
            && matches!(scope, gpui::NativeActivationScope::Active { .. })
    }

    pub(crate) fn page_input_scope(&self, cx: &App) -> Option<PageInputScope> {
        if !self.page_input_allowed(cx) { return None; }
        self.native_input_scope(cx)
    }

    fn native_input_scope(&self, cx: &App) -> Option<PageInputScope> {
        let store = self.links.store.read(cx);
        Some(PageInputScope {
            generation: self.page_input_generation?,
            authority: store.snapshot().key().authority(),
            attachment: store.current_owner_attachment(),
            retry: store.current_owner_retry(),
        })
    }

    fn native_producer_changed(&self, cx: &App) -> bool {
        let current = self.native_input_scope(cx);
        self.painted_native_input.as_ref().is_some_and(|previous|
            current.as_ref().is_none_or(|current| !previous.same_producer(current)))
    }

    pub(crate) fn admits_page_input_scope(&self, scope: &PageInputScope, cx: &App) -> bool {
        self.page_input_scope(cx).as_ref() == Some(scope)
    }

    pub(crate) fn shelf_input_scope(&self, drawer: bool, cx: &App) -> Option<ShelfInputScope> {
        let surface = ShelfNativeSurface::of(self.frame, self.shelf_over_open);
        let owns = if drawer { surface == ShelfNativeSurface::Drawer }
            else { matches!(surface, ShelfNativeSurface::Docked | ShelfNativeSurface::Spine) };
        if !owns || !self.background_input_allowed() || self.ask_open
            || matches!(self.links.snapshot(cx).overlay(), Some(Overlay::CommandPalette | Overlay::AddProject)) { return None; }
        self.native_input_scope(cx).map(|input| ShelfInputScope { input, surface })
    }

    pub(crate) fn admits_shelf_input_scope(&self, scope: &ShelfInputScope, cx: &App) -> bool {
        self.shelf_input_scope(scope.surface == ShelfNativeSurface::Drawer, cx).as_ref() == Some(scope)
    }

    fn page_input_allowed(&self, cx: &App) -> bool {
        self.background_input_allowed() && !self.ask_open && !self.shelf_over_open
            && !matches!(self.links.snapshot(cx).overlay(), Some(Overlay::CommandPalette | Overlay::AddProject))
    }

    /// The Shell's persistent focus handle stands for the active custom
    /// zone. Native Reader descendants can also own these keys directly;
    /// a Shelf, editor, menu or covered page never lends its focus to Reader.
    fn scroll_reader(&mut self, command: ViewportScroll, window: &mut Window, cx: &mut Context<Self>) {
        if !self.page_input_allowed(cx) || super::titlebar::menu_open(window, cx)
            || window.has_input_handler()
            || (window.root::<gpui_component::Root>().flatten().is_some() && window.has_focused_input(cx))
        {
            cx.propagate();
            return;
        }
        let shell_reader = self.zone == Zone::Reader && self.focus.is_focused(window);
        let native_reader = self.reader.read(cx).owns_keyboard_scroll_focus(window, cx);
        if !shell_reader && !native_reader {
            cx.propagate();
            return;
        }
        if !self.reader.update(cx, |reader, cx| reader.keyboard_scroll(command, cx)) {
            cx.propagate();
        }
    }

    pub(crate) fn mode_input_allowed(&self, cx: &App) -> bool {
        self.page_input_allowed(cx)
            && self.reader.read(cx).mode_input_allowed(cx)
    }

    fn local_navigation_allowed(&self, cx: &App) -> bool {
        self.page_input_allowed(cx) && self.reader.read(cx).local_navigation_allowed(cx)
    }

    fn with_background_input(&mut self, action: impl FnOnce(&mut Self)) {
        if self.background_input_allowed() {
            action(self);
        }
    }

    /// Ask owns an action at the shell's depth before the component root's Tab.
    fn ask_tab(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.ask_open && self.ask_results_mounted
            && self.links.snapshot(cx).overlay() == Some(Overlay::CommandPalette) {
            self.ask.update(cx, |ask, cx| ask.focus_next(backwards, window, cx));
        } else if self.ask_open {
            // The opening editor remains focused until results are exposed.
        } else if self.ask_presentation.blocks_background_input(false) {
            // The departing plate still covers the background. This Tab is
            // nevertheless a new input choice and retires the Find return.
            self.input_left_find_visit(cx);
        } else {
            cx.propagate();
        }
    }

    /// Find and Settings expose native field, row, and radio stops. Once a
    /// native control owns focus, Tab follows that real control order.
    fn folio_tab(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        if !self.background_input_allowed() || self.ask_open
            || !self.reader.read(cx).native_input_for(snapshot.route(), snapshot.overlay()) { return; }
        let find = matches!(snapshot.route(), Route::Orbit(crate::navigation::OrbitRoute::Browse(
            crate::navigation::BrowseRoute::FindHome | crate::navigation::BrowseRoute::Find(_)
        )));
        if find && snapshot.overlay().is_none()
            || matches!(snapshot.overlay(), Some(Overlay::Settings(_))) {
            if self.focus.is_focused(window) {
                // The custom shell hand still uses Tab to reach its zones.
                self.cycle_zone(!backwards, window, cx);
            } else if backwards { window.focus_prev(cx); } else { window.focus_next(cx); }
        } else {
            cx.propagate();
        }
    }

    /// The titlebar's Back action uses the same top-layer semantics as the
    /// key, while completing under its exact captured visit.
    pub(crate) fn header_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.links.snapshot(cx).overlay() == Some(Overlay::AddProject) || self.background_input_allowed() {
            self.take_zone(Zone::Titlebar, window, cx);
            self.back(window, cx);
        }
    }

    /// Esc: the topmost transient closes, one per press.
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let overlay = self.links.snapshot(cx).overlay();
        if super::titlebar::menu_open(window, cx)
            || self.shelf_over_open && !matches!(overlay, Some(Overlay::CommandPalette | Overlay::AddProject)) {
            self.escape(window, cx);
        } else { self.links.dispatch(Intent::Back, cx); }
    }

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
        if matches!(self.links.snapshot(cx).overlay(), Some(Overlay::CommandPalette | Overlay::AddProject)) {
            self.links.dispatch(Intent::DismissOverlay, cx);
            return;
        }
        if self.shelf_over_open {
            self.advance_transient_generation();
            self.advance_page_input_generation(InputOwnerChange::Structure, cx);
            self.shelf_over_open = false;
            cx.notify();
            return;
        }
        if self.hand_open {
            self.advance_transient_generation();
            self.hand_open = false;
            // A native card leaves with its layer. Keep keyboard dispatch on
            // the live shell rather than the now-retired card handle.
            self.focus.focus(window, cx);
            cx.notify();
            return;
        }
        let overlay = self.links.snapshot(cx).overlay();
        if overlay.is_some() {
            self.links.dispatch(Intent::DismissOverlay, cx);
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
            Route::CargoSource(route) => (route.package.clone(), None),
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
            Route::CargoSource(_) | Route::Symbol(_) | Route::Orbit(_) | Route::World => return,
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

    fn depth(&mut self, depth: RouteDepth, _window: &mut Window, cx: &mut Context<Self>) {
        if let RouteDepth::Page | RouteDepth::Source = depth {
            let view = if depth == RouteDepth::Page { View::Page } else { View::Code };
            if self.request_graph_view(view, cx) { return; }
        }
        let allowed = match depth {
            RouteDepth::Orbit | RouteDepth::Package => self.local_navigation_allowed(cx),
            RouteDepth::Page | RouteDepth::Source => self.mode_input_allowed(cx),
        };
        if !allowed { return; }
        let snapshot = self.links.snapshot(cx);
        let route = snapshot.route();
        match depth {
            RouteDepth::Page | RouteDepth::Source => {
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
        let hints = &self.hints.as_ref()?.mode;
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

    /// The active overlay owns the query and results. During exit the same
    /// rectangle holds only painted plate pixels: no Ask entity, result
    /// callbacks, or native accessibility descendants survive the overlay.
    fn ask_layer(&self, frame: &Frame, status: f32, scene: AskScene, cx: &App) -> Option<AnyElement> {
        if !scene.visible() {
            return None;
        }
        let palette = cx.facet().palette();
        let links = self.links.clone();
        let mut veil = div()
            .id(if self.ask_open { "ask-veil" } else { "ask-exit-veil" })
            // The retained Reader and Shelf still prepaint beneath this
            // sheet. Exclude their hitboxes, including native wheel input,
            // while later-painted Ask results remain interactive above it.
            .occlude()
            .absolute()
            .top(frame.titlebar)
            .bottom(px(status))
            .left_0()
            .right_0()
            .bg(palette.veil.alpha(scene.veil));
        if self.ask_open {
            veil = veil.on_click(move |_, _, cx| links.dispatch(Intent::DismissOverlay, cx));
        } else {
            // The page under a departing sheet is not yet available. Consume
            // its pointer hit without reviving any of Ask's old actions.
            veil = veil.on_click(|_, _, cx| cx.stop_propagation());
        }
        let plate = scene.geometry.map(|geometry| {
            let mut plate = div()
                .id(if !self.ask_open { "ask-exit-frame" } else if scene.live_results { "ask-frame" } else { "ask-enter-frame" })
                .debug_selector(|| "ask-plate".to_owned())
                .absolute()
                .top(geometry.plate.origin.y)
                .left(geometry.plate.origin.x)
                .w(geometry.plate.size.width)
                .h(geometry.plate.size.height)
                .overflow_hidden()
                .bg(palette.g2)
                .border_r_1()
                .border_color(palette.line2.hsla());
            if scene.paint_results {
                // The inner plate keeps its full readable measure while the
                // outer occupied rectangle reveals it and Reader follows.
                // An incomplete reveal paints the content but cannot expose
                // clipped row actions or take focus from the titlebar editor.
                let content = div().w(scene.content_width).h_full().child(self.ask.clone());
                plate = plate.on_click(|_, _, cx| cx.stop_propagation());
                plate = if scene.live_results {
                    plate.child(content)
                } else {
                    plate.child(gpui::inert("entering-ask-results", "Search results are opening", content))
                };
            } else {
                plate = plate.on_click(|_, _, cx| cx.stop_propagation());
            }
            plate
        });
        Some(
            div()
                .id(if self.ask_open { "ask-overlay" } else { "ask-exit" })
                .absolute()
                .inset_0()
                .child(veil)
                .children(plate)
                .into_any_element(),
        )
    }
}

/// Runs a target's act after the current update: an act may reach back
/// into the shell (open Ask, toggle the shelf), which must not re-enter it.
fn run(act: super::focus::Act, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| act(window, cx));
}

fn dispatch_symbol_code(links: &Links, symbol: crate::model::pages::SymbolRef, cx: &mut App) {
    let snapshot = links.snapshot(cx);
    if route_symbol(snapshot.route()).as_ref() == Some(&symbol) {
        links.dispatch(Intent::SetView(View::Code), cx);
        return;
    }
    let line = links.store.read(cx).symbol(&symbol).loaded_value()
        .and_then(|page| page.identity.line);
    let package = kit::package_of(&symbol).or_else(|| match snapshot.route() {
        Route::Symbol(route) => Some(route.package.as_str().to_owned()),
        Route::Package(route) => Some(route.package.as_str().to_owned()),
        Route::CargoSource(route) => Some(route.package.as_str().to_owned()),
        Route::Orbit(_) | Route::World => None,
    });
    if let Some(route) = package.and_then(|package| kit::symbol_view_route(&package, &symbol, View::Code, line)) {
        links.dispatch(Intent::Navigate(route), cx);
    }
}

impl Shell {
    /// Publishes the transient layers as the float stack the harness checks
    /// (unique keys, at most one of each, nothing left once settled).
    fn publish_stack(&self, ask: Option<(facet::probe::StackPhase, Vec<facet::probe::BoundsSample>)>, cx: &mut App) {
        let hints = self.hints.as_ref().map(|session| session.mode.remaining());
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
            if let Some((phase, bounds)) = ask {
                for bounds in bounds {
                    entries.push(facet::probe::StackEntry {
                        bounds: Some(bounds.clone()), phase,
                        ..entry(bounds.key.clone(), "dialog", false)
                    });
                }
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
        // Arm from the rendered Shell entity so GPUI reconciles this callback
        // against the scene produced below. A store event has no painted
        // entity owner and could otherwise run a callback against the old tree.
        if window.is_window_active() && self.keyboard_claim.is_some() && !self.keyboard_claim_scheduled {
            self.keyboard_claim_scheduled = true;
            let claim = self.keyboard_claim.clone().expect("checked claim");
            cx.on_next_frame(window, move |shell, window, cx| shell.verify_keyboard_claim(&claim, window, cx));
        }
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
        let previous_surface = ShelfNativeSurface::of(self.frame, self.shelf_over_open);
        let previous_pins = self.frame.is_some_and(|frame| frame.pins);
        self.frame = Some(frame);
        // The shelf opened over the reader answers a window too narrow to
        // hold it inline. A window that holds it has answered that ask: it
        // must not come back by itself when the window narrows again.
        if !frame.shelf_overlays {
            self.shelf_over_open = false;
        }
        let structure_changed = previous_surface != ShelfNativeSurface::of(self.frame, self.shelf_over_open)
            || previous_pins != frame.pins;
        if structure_changed || self.native_producer_changed(cx) {
            // Native ownership includes producer authority, exact serving attachment,
            // and retry attachment. A same-root replacement must also retire GPUI's
            // stored mouse-down; fresh listeners cannot inherit that old gesture.
            // Pixel-only resize and diagnostic observation preserve ownership.
            let change = if structure_changed { InputOwnerChange::Structure } else { InputOwnerChange::Producer };
            self.advance_page_input_generation(change, cx);
        }
        self.painted_native_input = self.native_input_scope(cx);
        // The status bar grows a line when the address's name has to wrap;
        // sized here from the same fit the bar sets, in the same frame.
        let (graph_focus, graph_notice) = { let store = self.links.store.read(cx); (store.graph_focus().cloned(), store.notice().cloned()) };
        // The foot grows only for what the graph says in it; the hand's
        // marks sit on one line.
        let status_height = if super::status::graph_speaks(&snapshot, graph_focus.as_ref(), graph_notice.as_ref()) {
            super::status::feedback_height(
                &snapshot, graph_focus.as_ref(), graph_notice.as_ref(), viewport.width,
                snapshot.session().hand.held().len(), f32::from(frame.status), window, cx,
            )
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
        if over && self.drawer_return.is_none() && !self.ask_open {
            let mut saved = self.capture_transient_return(snapshot.overlay(), window, cx);
            saved.drawer = false;
            self.drawer_return = Some(saved);
            self.reader.update(cx, |reader, _| reader.targets.set_active(false));
            self.shelf.update(cx, |shelf, _| shelf.targets.set_active(false));
            self.zone = Zone::Shelf;
            self.shelf_over.update(cx, |shelf, cx| {
                shelf.targets.set_active(true);
                if shelf.targets.focused().is_none() { shelf.targets.walk(1); }
                cx.notify();
            });
            self.drawer_focus.focus(window, cx);
        }
        let drawer = f32::from(frame.drawer);
        let over_x = self.motion.animate("over-x", if over { 0.0 } else { -drawer }, spec::SETTLE, window, cx).min(columns_cap);

        self.drawer_departing = !over && over_x > -drawer + 0.5;
        if !over && !self.drawer_departing {
            if let Some(saved) = self.drawer_return.take() {
                self.shelf_over.update(cx, |shelf, _| shelf.targets.set_active(false));
                // Switch the logical zone before restoring its native handle.
                self.zone = self.available_zone(saved.zone, saved.focus.as_ref(), window, cx);
                self.with_zone(cx, |targets| targets.set_active(true));
                self.queue_transient_return(saved, window, cx);
            }
        }
        self.rehome_unavailable_zone(previous_surface != ShelfNativeSurface::of(self.frame, self.shelf_over_open), window, cx);

        let ask_scene = self.ask_presentation.sample(
            self.ask_open,
            self.ask_open && self.ask.read(cx).shows(),
            &frame,
            viewport,
            px(status_height),
            px(shelf_width),
            &self.modes,
            window,
            cx,
        );
        self.ask_results_mounted = ask_scene.live_results;
        let background_input_allowed = !self.ask_open && !over && self.background_input_allowed()
            && snapshot.overlay() != Some(Overlay::AddProject);
        self.reader.update(cx, |reader, cx| reader.set_ask_scene(ask_scene.geometry, background_input_allowed, cx));

        let mut context = KeyContext::new_with_defaults();
        context.add(CONTEXT);
        if self.zone == Zone::Reader && self.focus.is_focused(window)
            && mounts_scroll_body(self.links.store.read(cx), snapshot.route(), snapshot.page_overlay()) {
            context.add(keys::READER_VIEWPORT);
        }
        if self.ask_open {
            // The modal owns Tab, arrows, and result activation. Leave shell
            // shortcuts available while its plain zone bindings step aside.
            context.add("Ask");
        } else if self.ask_presentation.blocks_background_input(false)
            && snapshot.overlay() != Some(Overlay::AddProject) {
            context.add("AskLeaving");
        }
        if background_input_allowed && !over
            && (matches!(snapshot.overlay(), Some(Overlay::Settings(_)))
                || snapshot.overlay().is_none() && matches!(snapshot.route(), Route::Orbit(
                    crate::navigation::OrbitRoute::Browse(
                        crate::navigation::BrowseRoute::FindHome | crate::navigation::BrowseRoute::Find(_)
                    )
                ))) {
            context.add("NativeFolio");
        }
        if over { context.add("Drawer"); }
        if snapshot.overlay() == Some(Overlay::AddProject) { context.add("NativeDialog"); }
        if self.hints.is_some() {
            context.add("hints");
        }
        // An open jump-bar menu owns the plain keys (arrows, ↵, type-ahead,
        // Esc): the shell's bindings step aside (`keys::binding`, `!Menu`).
        if super::titlebar::menu_open(window, cx) {
            self.reader
                .update(cx, |reader, _| reader.cancel_native_return());
            context.add("Menu");
        }

        let body = div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .children((shelf_width > 0.5).then(|| {
                let shelf = measured(
                    &self.shelf,
                    StyleRefinement::default().w(px(shelf_width)).h_full().flex_none(),
                );
                if over {
                    a11y_inert(shelf).into_any_element()
                } else {
                    shelf.into_any_element()
                }
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
        // Keep the page's native controls out until its pixels reappear from
        // beneath the last painted plate, including Ask's inert exit.
        let body: AnyElement = if ask_scene.visible() || over || self.drawer_departing
            || snapshot.overlay() == Some(Overlay::AddProject) {
            a11y_inert(body).into_any_element()
        } else {
            body.into_any_element()
        };

        let mut root = div()
            // Element identity remains stable. Activation ownership below is
            // independent of keyed editor, focus and local presentation state.
            .id("shell")
            // This is the actual keyboard-focus owner for the four custom
            // navigation zones. Their active target is reported as this
            // application's descendant rather than falling back to Window.
            .role(gpui::Role::Application)
            .aria_label("Nudox")
            .debug_selector(|| "shell-root".to_owned())
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(palette.g1)
            .text_color(palette.ink1.hsla())
            .font_family(facet::fonts::family(facet::tokens::ty::BODY))
            .track_focus(&self.focus)
            .key_context(context)
            .on_action(cx.listener(|shell, _: &keys::FocusNext, window, cx| shell.with_background_input(|shell| shell.walk(1, window, cx))))
            .on_action(cx.listener(|shell, _: &keys::FocusPrev, window, cx| shell.with_background_input(|shell| shell.walk(-1, window, cx))))
            .on_action(cx.listener(|shell, _: &keys::ReaderPageDown, window, cx| shell.scroll_reader(ViewportScroll::PageDown, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::ReaderPageUp, window, cx| shell.scroll_reader(ViewportScroll::PageUp, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::ReaderTop, window, cx| shell.scroll_reader(ViewportScroll::Top, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::ReaderBottom, window, cx| shell.scroll_reader(ViewportScroll::Bottom, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::Activate, window, cx| shell.with_background_input(|shell| shell.activate(window, cx))))
            .on_action(cx.listener(|shell, _: &keys::Peek, window, cx| shell.with_background_input(|shell| shell.peek(window, cx))))
            .on_action(cx.listener(|shell, _: &keys::PeelSource, window, cx| shell.with_background_input(|shell| shell.peel(window, cx))))
            .on_action(cx.listener(|shell, _: &keys::HintMode, _, cx| shell.with_background_input(|shell| shell.hint_mode(cx))))
            .on_action(cx.listener(|shell, _: &keys::Ask, _, cx| shell.open_ask(cx)))
            .on_action(cx.listener(|shell, _: &keys::Back, window, cx| {
                if shell.links.snapshot(cx).overlay() == Some(Overlay::AddProject) || shell.background_input_allowed() { shell.back(window, cx); }
            }))
            .on_action(cx.listener(|shell, _: &keys::Forward, _, cx| { if shell.page_input_allowed(cx) { shell.links.dispatch(Intent::Forward, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::Surface, _, cx| { if shell.page_input_allowed(cx) { shell.links.dispatch(Intent::ZoomOut, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::Zen, window, cx| shell.with_background_input(|shell| {
                shell.toggle_zen(window, cx);
            })))
            .on_action(cx.listener(|shell, _: &keys::ToggleShelf, window, cx| shell.with_background_input(|shell| shell.toggle_shelf(window, cx))))
            .on_action(cx.listener(|shell, _: &keys::NextZone, window, cx| shell.with_background_input(|shell| shell.cycle_zone(true, window, cx))))
            .on_action(cx.listener(|shell, _: &keys::PrevZone, window, cx| shell.with_background_input(|shell| shell.cycle_zone(false, window, cx))))
            .on_action(cx.listener(|shell, _: &keys::AskNext, window, cx| shell.ask_tab(false, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::AskPrev, window, cx| shell.ask_tab(true, window, cx)))
            .on_action(cx.listener(|shell, _: &keys::FolioNext, window, cx| shell.with_background_input(|shell| shell.folio_tab(false, window, cx))))
            .on_action(cx.listener(|shell, _: &keys::FolioPrev, window, cx| shell.with_background_input(|shell| shell.folio_tab(true, window, cx))))
            .on_action(cx.listener(|shell, _: &keys::Escape, window, cx| {
                if shell.links.snapshot(cx).overlay() == Some(Overlay::AddProject) || shell.background_input_allowed() { shell.escape(window, cx); }
            }))
            .on_action(cx.listener(|shell, _: &keys::DepthOrbit, window, cx| { if shell.page_input_allowed(cx) { shell.depth(RouteDepth::Orbit, window, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::DepthPackage, window, cx| { if shell.page_input_allowed(cx) { shell.depth(RouteDepth::Package, window, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::DepthPage, window, cx| { if shell.page_input_allowed(cx) { shell.depth(RouteDepth::Page, window, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::DepthCode, window, cx| { if shell.page_input_allowed(cx) { shell.depth(RouteDepth::Source, window, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::Graph, window, cx| shell.with_background_input(|shell| shell.toggle_graph(window, cx))))
            .on_action(cx.listener(|shell, _: &keys::CodePage, window, cx| shell.with_background_input(|shell| shell.code_page(window, cx))))
            .on_action(cx.listener(|shell, open: &facet::anatomy::Open, _, cx| shell.with_background_input(|shell| shell.open_anatomy(open, cx))))
            .on_action(cx.listener(|shell, _: &keys::ZoomIn, _, cx| shell.with_background_input(|shell| shell.zoom(ZoomStep::In, cx))))
            .on_action(cx.listener(|shell, _: &keys::ZoomOut, _, cx| shell.with_background_input(|shell| shell.zoom(ZoomStep::Out, cx))))
            .on_action(cx.listener(|shell, _: &keys::ZoomReset, _, cx| shell.with_background_input(|shell| shell.zoom(ZoomStep::Reset, cx))))
            .on_action(cx.listener(|shell, _: &keys::Hold, window, cx| { if shell.page_input_allowed(cx) { shell.hold(window, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::OpenHand, _, cx| shell.with_background_input(|shell| shell.toggle_hand(cx))))
            .on_action(cx.listener(|shell, _: &keys::HandCard1, _, cx| { if shell.page_input_allowed(cx) { shell.hand_card(0, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::HandCard2, _, cx| { if shell.page_input_allowed(cx) { shell.hand_card(1, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::HandCard3, _, cx| { if shell.page_input_allowed(cx) { shell.hand_card(2, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::HandCard4, _, cx| { if shell.page_input_allowed(cx) { shell.hand_card(3, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::HandCard5, _, cx| { if shell.page_input_allowed(cx) { shell.hand_card(4, cx); } }))
            .on_action(cx.listener(|shell, _: &keys::CopyAddress, _, cx| shell.with_background_input(|shell| shell.copy_address(cx))))
            .on_action(cx.listener(|shell, _: &keys::Tour, _, cx| { if shell.page_input_allowed(cx) { shell.tour(cx); } }))
            .on_action(cx.listener(|shell, _: &keys::OpenSettings, _, cx| shell.with_background_input(|shell| {
                shell.links.dispatch(Intent::OpenSettings(SettingsPage::Appearance), cx);
            })))
            .on_action(cx.listener(|shell, _: &keys::AddFolder, _, cx| shell.with_background_input(|shell| shell.links.dispatch(Intent::OpenAddProject, cx))))
            .on_modifiers_changed(cx.listener(|shell, event: &ModifiersChangedEvent, _, cx| {
                shell.with_background_input(|shell| shell.modifiers(event.modifiers, cx));
            }))
            .capture_key_down(cx.listener(|shell, event: &KeyDownEvent, window, cx| {
                shell.key_down(event, window, cx);
            }))
            .capture_any_mouse_down(cx.listener(|shell, _, _, cx| {
                shell.advance_transient_generation();
                shell.input_left_find_visit(cx);
                // Hint letters are a keyboard session. A pointer takes a new
                // owner, including a text field whose next key must edit it.
                if shell.hints.take().is_some() { cx.notify(); }
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
                    .child(super::hand::row(&view, self.hand_at, &self.links, &measure, palette, window, cx)),
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
            let shelf_body = measured(&self.shelf_over, StyleRefinement::default().size_full());
            let shelf_body: AnyElement = if over && !self.ask_open
                && snapshot.overlay() != Some(Overlay::AddProject)
                && self.background_input_allowed() {
                shelf_body.into_any_element()
            } else {
                a11y_inert(shelf_body).into_any_element()
            };
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
                                .occlude()
                                .absolute()
                                .inset_0()
                                .bg(palette.veil.alpha(opened))
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    // Consume the closing gesture while its hitbox still
                                    // owns this frame; dismissal never grants the underlay
                                    // another activation from this same MouseUp.
                                    cx.stop_propagation();
                                    shell.advance_transient_generation();
                                    shell.advance_page_input_generation(InputOwnerChange::Structure, cx);
                                    shell.shelf_over_open = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("shelf-drawer")
                                .role(gpui::Role::Dialog)
                                .aria_label("Library shelf")
                                .track_focus(&self.drawer_focus)
                                .occlude()
                                .on_click(|_, _, cx| cx.stop_propagation())
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(over_x))
                                .w(frame.drawer)
                                .bg(palette.g2)
                                .child(shelf_body)
                                .focus_trap("shelf-drawer-trap", &self.drawer_focus),
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
        // Ask is a dialog over the veiled page: its field (the titlebar) and
        // its plate are what a person reads; the page under the veil is not.
        let ask_bounds = ask_scene.phase.map(|phase| {
            let sample = |key: &str, x: Pixels, y: Pixels, width: Pixels, height: Pixels| facet::probe::BoundsSample {
                key: key.to_owned(),
                x: f32::from(x),
                y: f32::from(y),
                width: f32::from(width),
                height: f32::from(height),
            };
            let mut parts = Vec::new();
            if self.ask_open {
                parts.push(sample("ask-field", px(0.0), px(0.0), viewport.width, frame.titlebar));
            }
            // The same measured plate may still be painted after its live
            // query and results have been removed on exit.
            if let Some(geometry) = ask_scene.geometry {
                parts.push(sample("ask-plate", geometry.plate.origin.x, geometry.plate.origin.y,
                    geometry.plate.size.width, geometry.plate.size.height));
            }
            if parts.is_empty() {
                parts.push(sample("ask-veil", px(0.0), frame.titlebar, viewport.width,
                    (viewport.height - frame.titlebar - px(status_height)).max(px(0.0))));
            }
            (phase, parts)
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
        self.flush_transient_return(window, cx);
        gpui::native_activation_scope(
            gpui::NativeActivationScope::new(cx.entity_id(), self.page_input_generation),
            root.children(self.ask_layer(&frame, status_height, ask_scene, cx)
                .map(|ask| gpui::deferred(ask).with_priority(DRAWER_PRIORITY + 1)))
                .children(self.hint_layer(cx))
                .children(twins)
                .child(float),
        )
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
            cargo: None,
            project: None,
            package: held.package.clone(),
            lane: crate::navigation::PackageLane::Overview,
            selected: None,
            at: None,
        }),
    })
}

#[cfg(test)]
mod keyboard_claim_tests {
    use super::*;

    fn mounted_root(cx: &mut gpui::TestAppContext) -> super::super::tests::Rig {
        let mut rig = super::super::tests::rig(cx, Some(super::super::tests::page_route("RelationLabel")), 1440.0, 900.0);
        let shell = rig.shell.clone();
        rig.cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
            window.set_a11y_forced(true);
        });
        rig.settle();
        rig
    }

    fn opened_before_first_paint(cx: &mut gpui::TestAppContext) -> super::super::tests::Rig {
        let mut rig = mounted_root(cx);
        let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
        let links = reader.read_with(rig.cx, |reader, _| reader.navigation_links());
        rig.cx.update(|_, cx| links.dispatch(Intent::OpenCommandPalette, cx));
        assert!(rig.shell.read_with(rig.cx, |shell, _| shell.keyboard_claim.is_some()));
        rig
    }

    fn pending_inactive_ask(cx: &mut gpui::TestAppContext) -> super::super::tests::Rig {
        let mut rig = mounted_root(cx);
        rig.cx.deactivate_window();
        rig.cx.update(|window, _| window.blur());
        let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
        let links = reader.read_with(rig.cx, |reader, _| reader.navigation_links());
        rig.cx.update(|_, cx| links.dispatch(Intent::OpenCommandPalette, cx));
        rig.settle();
        assert!(rig.cx.update(|window, cx| !window.is_window_active() && window.focused(cx).is_none()));
        assert!(rig.shell.read_with(rig.cx, |shell, _| shell.keyboard_claim.is_some()));
        rig
    }

    #[gpui::test]
    fn a_later_explicit_blur_cancels_ask_focus_reassertion(cx: &mut gpui::TestAppContext) {
        let mut rig = opened_before_first_paint(cx);
        rig.cx.update(|window, _| window.blur());
        rig.settle();
        assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay()),
            Some(Overlay::CommandPalette));
        assert!(rig.cx.update(|window, cx| window.focused(cx).is_none()),
            "the current mounted Ask target cannot override a later explicit blur");
        assert!(rig.shell.read_with(rig.cx, |shell, _| shell.keyboard_claim.is_none()));
    }

    #[gpui::test]
    fn a_later_mounted_focus_choice_cancels_ask_focus_reassertion(cx: &mut gpui::TestAppContext) {
        let mut rig = opened_before_first_paint(cx);
        let shell_focus = rig.shell.read_with(rig.cx, |shell, _| shell.focus.clone());
        rig.cx.update(|window, cx| shell_focus.focus(window, cx));
        rig.settle();
        assert!(rig.cx.update(|window, _| shell_focus.is_focused(window)),
            "a newer mounted focus choice owns the next frame");
        assert!(rig.shell.read_with(rig.cx, |shell, _| shell.keyboard_claim.is_none()));
    }

    #[gpui::test]
    fn inactive_ask_claim_focuses_only_after_activation_and_current_paint(cx: &mut gpui::TestAppContext) {
        let mut rig = pending_inactive_ask(cx);
        rig.cx.update(|window, _| window.activate_window());
        rig.settle();
        let shell = rig.shell.clone();
        let keyboard = rig.cx.update(|window, cx| shell.read(cx).keyboard_diagnostic(window, cx));
        assert_eq!(keyboard.focus_owner, "ask-editor");
        assert!(keyboard.window_active && keyboard.target_mounted && keyboard.input_handler_present);
        assert!(!keyboard.pending_claim);
        rig.keys("f");
        let input = shell.read_with(rig.cx, |shell, cx| shell.ask.read(cx).input().clone());
        assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "f",
            "the first current native text event reaches the editor");
    }

    #[gpui::test]
    fn explicit_blur_of_none_cancels_an_inactive_ask_claim(cx: &mut gpui::TestAppContext) {
        let mut rig = pending_inactive_ask(cx);
        let before = rig.cx.update(|window, _| window.focus_epoch());
        rig.cx.update(|window, _| window.blur());
        assert_ne!(rig.cx.update(|window, _| window.focus_epoch()), before,
            "an explicit None-to-None blur is still a later focus intent");
        rig.cx.update(|window, _| window.activate_window());
        rig.settle();
        assert!(rig.cx.update(|window, cx| window.focused(cx).is_none()));
        assert!(rig.shell.read_with(rig.cx, |shell, _| shell.keyboard_claim.is_none()));
    }
}

#[cfg(test)]
mod transient_return_admission_tests {
    use super::*;

    #[gpui::test]
    fn a_return_claim_needs_the_current_attachment_and_input_generation(cx: &mut gpui::TestAppContext) {
        let mut rig = super::super::tests::rig(cx, Some(super::super::tests::page_route("RelationLabel")), 1440.0, 900.0);
        let shell = rig.shell.clone();
        let saved = rig.cx.update(|window, cx| shell.read(cx).capture_transient_return(None, window, cx));
        assert!(shell.read_with(rig.cx, |shell, cx| shell.return_identity_current(&saved, cx)));
        let mut absent_attachment = saved.clone();
        assert!(absent_attachment.attachment.is_some(), "the painted test owner has a serving attachment");
        absent_attachment.attachment = None;
        assert!(!shell.read_with(rig.cx, |shell, cx| shell.return_identity_current(&absent_attachment, cx)), "a missing/revoked attachment cannot inherit a serving owner's focus claim");
        let mut exhausted_generation = saved.clone();
        exhausted_generation.generation = None;
        assert!(!shell.read_with(rig.cx, |shell, cx| shell.return_identity_current(&exhausted_generation, cx)));
        shell.update(rig.cx, |shell, _| shell.advance_transient_generation());
        assert!(!shell.read_with(rig.cx, |shell, cx| shell.return_identity_current(&saved, cx)), "a subsequent native input generation retires the old return");
    }
}

#[cfg(test)]
mod deferred_target_admission_tests {
    use super::*;
    use crate::runtime::owner::{OwnerGate, OwnerState};
    use crate::runtime::reads::ReadPool;
    use gpui::AppContext as _;

    fn mounted_declaration_handoff(cx: &mut gpui::TestAppContext, source_key: bool) {
        let route = Route::Package(crate::navigation::PackageRoute {
            cargo: None,
            project: None,
            at: None,
            package: crate::core::PackageId::new(super::super::tests::PACKAGE).expect("package"),
            lane: crate::navigation::PackageLane::Overview,
            selected: None,
        });
        let mut rig = super::super::tests::rig(cx, Some(route.clone()), 1440.0, 900.0);
        rig.settle();
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader.read(cx).targets.clone());
        let frame = targets.hint_frame();
        let target = targets.placed().into_iter()
            .find(|(target, _)| target.peek.is_some() && target.source.is_some())
            .map(|(target, _)| target)
            .expect("mounted declaration supports both Space and S");
        targets.focus(target.id.clone());
        assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Reader);
        let shell = rig.shell.clone();
        rig.cx.update(|window, app| {
            let handoff_shell = shell.clone();
            window.defer(app, move |window, app| {
                // This effect precedes the Space/S effect in the same GPUI
                // cycle. The old row, its frame, and its producer stay live;
                // only the keyboard's zone has changed.
                handoff_shell.update(app, |shell, cx| shell.take_zone(Zone::Titlebar, window, cx));
                let old = handoff_shell.read(app).reader.read(app).targets.clone();
                assert_eq!(old.hint_frame(), frame);
                assert_eq!(old.focused(), Some(target.id.clone()));
                assert!(target.action.admits(app), "producer still admits the old row");
            });
            shell.update(app, |shell, cx| {
                if source_key { shell.peel(window, cx); }
                else { shell.peek(window, cx); }
            });
        });
        rig.settle();
        assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Titlebar);
        assert_eq!(rig.route(), route, "the old Reader row cannot navigate after a zone handoff");
        assert!(!rig.shell.read_with(rig.cx, |shell, _| shell.transients()).1,
            "the old Reader row cannot open a peek after a zone handoff");
    }

    #[gpui::test]
    fn space_rechecks_the_keyboard_zone_before_opening_a_mounted_peek(cx: &mut gpui::TestAppContext) {
        mounted_declaration_handoff(cx, false);
    }

    #[gpui::test]
    fn source_key_rechecks_the_keyboard_zone_before_navigating(cx: &mut gpui::TestAppContext) {
        mounted_declaration_handoff(cx, true);
    }

    fn serving_gate() -> (crate::core::VersionedRoot, OwnerGate) {
        let root = crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4,
        );
        (root, OwnerGate::ready(root, crate::model::ServiceMode::Attached))
    }

    #[gpui::test]
    fn revoked_reader_space_and_source_key_cannot_act_or_reenter_shell(cx: &mut gpui::TestAppContext) {
        let (root, gate) = serving_gate();
        let route = Route::Package(crate::navigation::PackageRoute {
            cargo: None,
            project: None,
            at: None,
            package: crate::core::PackageId::new(super::super::tests::PACKAGE).expect("package"),
            lane: crate::navigation::PackageLane::Overview,
            selected: None,
        });
        let mut rig = super::super::tests::rig_with_engine_gate(
            cx, Some(route.clone()), 1440.0, 900.0,
            ReadPool::start(2, |_| super::super::tests::Fixture).expect("pool"),
            super::super::tests::RootOnly, Some(gate.clone()),
        );
        rig.settle();
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader.read(cx).targets.clone());
        let selected = targets.placed().into_iter()
            .find(|(target, _)| target.peek.is_some() && target.source.is_some())
            .map(|(target, _)| target.id)
            .expect("mounted declaration supports both Space and S");
        targets.focus(selected.clone());
        let before_focus = rig.cx.update(|window, cx| window.focused(cx));
        gate.publish(OwnerState::Starting);
        gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
        rig.cx.simulate_keystrokes("space");
        rig.cx.simulate_keystrokes("s");
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), before_focus,
            "revoked Space and S cannot move native focus");
        rig.settle();
        assert_eq!(rig.route(), route, "revoked S cannot navigate through a stale declaration");
        assert!(!rig.shell.read_with(rig.cx, |shell, _| shell.transients()).1,
            "revoked Space cannot open a peek");
    }

    #[gpui::test]
    fn revoked_shelf_hint_cannot_move_focus_or_activate_a_stale_row(cx: &mut gpui::TestAppContext) {
        let (root, gate) = serving_gate();
        let mut rig = super::super::tests::rig_with_engine_gate(
            cx, Some(super::super::tests::page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(2, |_| super::super::tests::Fixture).expect("pool"),
            super::super::tests::RootOnly, Some(gate.clone()),
        );
        rig.settle();
        let id = rig.shell.read_with(rig.cx, |shell, cx| {
            shell.shelf.read(cx).targets.placed().into_iter().next().map(|(target, _)| target.id)
        }).expect("mounted shelf row");
        rig.keys("f");
        let code = rig.shell.read_with(rig.cx, |shell, _| shell.hint_code_for(&id))
            .expect("shelf row has a hint");
        let route = rig.route();
        let focused = rig.cx.update(|window, cx| window.focused(cx));
        gate.publish(OwnerState::Starting);
        gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
        rig.cx.simulate_keystrokes(&code.chars().map(|letter| letter.to_string()).collect::<Vec<_>>().join(" "));
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused,
            "a stale shelf hint cannot take native focus");
        rig.settle();
        assert_eq!(rig.route(), route, "the stale shelf row cannot navigate");
    }
}

#[cfg(test)]
mod shelf_scene_admission_tests {
    use super::*;

    #[gpui::test]
    fn mounted_shelf_down_twice_then_return_opens_the_exact_declaration(cx: &mut gpui::TestAppContext) {
        let route = Route::Package(crate::navigation::PackageRoute {
            cargo: None, project: None, at: None,
            package: crate::core::PackageId::new(super::super::tests::PACKAGE).expect("fixture package"),
            lane: crate::navigation::PackageLane::Overview, selected: None,
        });
        let mut rig = super::super::tests::rig(cx, Some(route.clone()), 1440.0, 900.0);
        rig.cx.update(|_, cx| facet::probe::enable(cx));
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        let functions = ledger.texts.iter().find(|text| text.content == "Functions"
            && text.bounds.x < 264.0).expect("real package Shelf paints Functions fold");
        let at = gpui::point(px(functions.bounds.x + functions.bounds.width / 2.0),
            px(functions.bounds.y + functions.bounds.height / 2.0));
        rig.cx.simulate_click(at, gpui::Modifiers::none());
        rig.settle();
        let shelf = rig.shell.read_with(rig.cx, |shell, _| shell.shelf.clone());
        let preceding: SharedString = "shelf-kind-types".into();
        assert!(shelf.read_with(rig.cx, |shelf, _| shelf.targets.placed()
            .iter().any(|(target, _)| target.id == preceding)),
            "the preceding semantic target is mounted before two Down keys");
        let shell = rig.shell.clone();
        rig.cx.update(|window, cx| shell.update(cx, |shell, cx| {
            shell.take_zone(Zone::Shelf, window, cx);
            shell.shelf.read(cx).targets.focus(preceding.clone());
            shell.notify_zone(Zone::Shelf, cx);
        }));
        rig.repaint();
        rig.keys("down down");
        let (_, focused) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        let wanted = format!("shelf-functions:{}", super::super::tests::symbol("relation_label").as_str());
        assert_eq!(focused.as_deref(), Some(wanted.as_str()),
            "two real Down key events select the function row before Return");
        assert_eq!(rig.route(), route, "selection itself does not navigate");
        rig.keys("enter");
        assert_eq!(rig.route(), super::super::tests::page_route("relation_label"),
            "Shell Return activates the same mounted target as a pointer click");
    }

    #[gpui::test]
    fn parked_native_shelf_row_continues_keys_then_hiding_shelf_rehomes_the_zone(cx: &mut gpui::TestAppContext) {
        let route = Route::Package(crate::navigation::PackageRoute {
            cargo: None, project: None, at: None,
            package: crate::core::PackageId::new(super::super::tests::PACKAGE).expect("fixture package"),
            lane: crate::navigation::PackageLane::Overview, selected: None,
        });
        let mut rig = super::super::tests::rig(cx, Some(route), 1440.0, 400.0);
        rig.settle();
        let shell = rig.shell.clone();
        let focused: SharedString = "shelf-kind-types".into();
        let shelf = shell.read_with(rig.cx, |shell, _| shell.shelf.clone());
        let handle = rig.cx.update(|window, cx| shell.update(cx, |shell, cx| {
            shell.take_zone(Zone::Shelf, window, cx);
            let targets = shelf.read(cx).targets.clone();
            targets.focus(focused.clone());
            let handle = targets.native_handle(&focused, cx);
            window.focus(&handle, cx);
            shell.notify_zone(Zone::Shelf, cx);
            handle
        }));
        rig.repaint();
        let scroll = shelf.read_with(rig.cx, |shelf, _| shelf.diagnostic_scroll_state());
        let viewport = scroll.viewport_bounds();
        for _ in 0..8 {
            crate::shell::tests::wheel(rig.cx, viewport.center(), gpui::point(px(0.0), px(-120.0)));
        }
        rig.repaint();
        assert!(scroll.logical_scroll_top().item_ix > 1,
            "native wheel moves the focused Types row beyond list overdraw");
        assert!(rig.cx.update(|window, _| handle.is_focused(window)),
            "the actual GPUI focus handle remains mounted while its row is parked");
        assert_eq!(rig.cx.update(|window, cx| shelf.read(cx).targets.native_focused(window)), Some(focused));
        rig.keys("down");
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).0, Zone::Shelf,
            "keyboard continuation stays in the Shelf after an offscreen wheel");
        rig.keys("secondary-\\");
        assert_ne!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).0, Zone::Shelf,
            "closing the inline Shelf must not leave its parked focus outline active in the Reader");
        assert!(!rig.cx.update(|window, _| handle.is_focused(window)),
            "a parked native row handle loses keyboard ownership when its presentation closes");
        assert!(rig.cx.update(|window, cx| shell.read(cx).focus.is_focused(window)),
            "the persistent Reader hand receives keyboard input after the hidden row leaves");
    }

    #[gpui::test]
    fn parked_native_shelf_origin_cannot_swallow_down_then_return(cx: &mut gpui::TestAppContext) {
        let route = Route::Package(crate::navigation::PackageRoute {
            cargo: None, project: None, at: None,
            package: crate::core::PackageId::new(super::super::tests::PACKAGE).expect("fixture package"),
            lane: crate::navigation::PackageLane::Overview, selected: None,
        });
        let mut rig = super::super::tests::rig(cx, Some(route.clone()), 1440.0, 400.0);
        rig.settle();
        let shell = rig.shell.clone();
        let shelf = shell.read_with(rig.cx, |shell, _| shell.shelf.clone());
        let focused: SharedString = "shelf-kind-types".into();
        let handle = rig.cx.update(|window, cx| shell.update(cx, |shell, cx| {
            shell.take_zone(Zone::Shelf, window, cx);
            let targets = shelf.read(cx).targets.clone();
            targets.focus(focused.clone());
            let handle = targets.native_handle(&focused, cx);
            handle.focus(window, cx);
            handle
        }));
        rig.repaint();
        let scroll = shelf.read_with(rig.cx, |shelf, _| shelf.diagnostic_scroll_state());
        for _ in 0..8 {
            crate::shell::tests::wheel(rig.cx, scroll.viewport_bounds().center(), gpui::point(px(0.0), px(-120.0)));
        }
        rig.repaint();
        assert!(scroll.logical_scroll_top().item_ix > 1);
        assert!(rig.cx.update(|window, _| handle.is_focused(window)), "old Types button is still natively parked");
        let folds = |rig: &mut super::super::tests::Rig| rig.graph.store.read_with(rig.cx, |store, _|
            store.snapshot().session().reading.current.presentation.controls().shelf.folds()
                .map(|key| key.as_str().to_owned()).collect::<Vec<_>>());
        let before = folds(&mut rig);
        rig.keys("down");
        assert_eq!(shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx).1).as_deref(), Some("shelf-kind-functions"),
            "Down selects Functions while native focus remains on the parked Types button");
        rig.native_press("enter");
        rig.settle();
        let after = folds(&mut rig);
        assert_eq!(after.iter().any(|key| key == "shelf-kind-functions"),
            !before.iter().any(|key| key == "shelf-kind-functions"),
            "Return folds the newly selected Functions group exactly once");
        assert_eq!(after.iter().any(|key| key == "shelf-kind-types"),
            before.iter().any(|key| key == "shelf-kind-types"),
            "the parked Types button cannot also fire on native KeyUp");
        assert_eq!(rig.route(), route);
        assert!(!rig.cx.update(|window, _| handle.is_focused(window)),
            "the mismatched native press is revoked before the custom selected action");
    }

    #[gpui::test]
    fn titlebar_toggle_and_structural_changes_rehome_a_focused_shelf_row(cx: &mut gpui::TestAppContext) {
        let mut rig = super::super::tests::rig(cx,
            Some(super::super::tests::page_route("RelationLabel")), 1440.0, 900.0);
        let shell = rig.shell.clone();
        let focus_shelf = |rig: &mut super::super::tests::Rig| {
            rig.cx.update(|window, cx| shell.update(cx, |shell, cx| {
                shell.take_zone(Zone::Shelf, window, cx);
                shell.shelf.read(cx).targets.focus("shelf-kind-types");
                shell.notify_zone(Zone::Shelf, cx);
            }));
            rig.repaint();
            assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Shelf);
        };
        focus_shelf(&mut rig);
        let toggle = super::super::tests::native_bounds_id(&mut rig, "tb-shelf", "Button", "Toggle the shelf", true)
            .expect("the real titlebar toggle is mounted");
        rig.cx.simulate_click(toggle.center(), gpui::Modifiers::none());
        rig.settle();
        assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Reader,
            "a titlebar pointer toggle retires the row hand just like the keyboard action");
        assert!(!shell.read_with(rig.cx, |shell, cx| shell.shelf.read(cx).targets.is_active()),
            "the hidden Shelf does not keep an active parked row outline");
        assert!(rig.cx.update(|window, cx| window.focused(cx).is_some()),
            "the visible titlebar control or Reader retains a native keyboard owner");

        rig.keys("secondary-\\");
        focus_shelf(&mut rig);
        rig.cx.simulate_resize(gpui::size(px(760.0), px(900.0)));
        rig.settle();
        assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Reader,
            "a Shelf row cannot own the keyboard after its column becomes a spine");
        rig.cx.simulate_resize(gpui::size(px(1440.0), px(900.0)));
        rig.settle();
        focus_shelf(&mut rig);
        rig.keys("secondary-.");
        assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Reader,
            "zen removes the Shelf's row targets and their keyboard zone");
    }

    #[gpui::test]
    fn a_mounted_spine_mark_keeps_native_keyboard_activation(cx: &mut gpui::TestAppContext) {
        let route = Route::Package(crate::navigation::PackageRoute {
            cargo: None, project: None, at: None,
            package: crate::core::PackageId::new(super::super::tests::PACKAGE).expect("fixture package"),
            lane: crate::navigation::PackageLane::Overview, selected: None,
        });
        let mut rig = super::super::tests::rig(cx, Some(route.clone()), 760.0, 900.0);
        rig.settle();
        let shell = rig.shell.clone();
        assert!(shell.read_with(rig.cx, |shell, _| shell.frame.is_some_and(|frame| frame.shelf == ShelfMode::Spine)));
        let mark: SharedString = "spine-shelf-kind-types".into();
        let handle = rig.cx.update(|window, cx| shell.update(cx, |shell, cx| {
            let targets = shell.shelf.read(cx).targets.clone();
            let handle = targets.reuse_native_handle(&mark).expect("the actual Types spine mark was rendered");
            assert!(window.is_focus_handle_mounted(&handle), "the mark has a native GPUI owner");
            shell.set_zone(Zone::Shelf, cx);
            handle.focus(window, cx);
            handle
        }));
        rig.repaint();
        assert!(rig.cx.update(|window, _| handle.is_focused(window)), "structural paint retains a live Spine mark");
        assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Shelf);
        let folded = |rig: &mut super::super::tests::Rig| rig.graph.store.read_with(rig.cx, |store, _|
            store.snapshot().session().reading.current.presentation.controls().shelf.folds().count());
        let before = folded(&mut rig);
        rig.native_press("enter");
        rig.settle();
        assert_eq!(rig.route(), route, "the Spine mark toggles its group in place");
        assert_ne!(folded(&mut rig), before, "native Return clicks the mounted mark, not a hidden full row");

        rig.cx.update(|window, cx| shell.update(cx, |shell, cx| shell.take_zone(Zone::Reader, window, cx)));
        let reader_stops = shell.read_with(rig.cx, |shell, cx|
            shell.reader.read(cx).targets.list_probe().upgrade().map_or(0, |list| list.borrow().len()));
        for _ in 0..reader_stops + Zone::ALL.len() {
            if shell.read_with(rig.cx, |shell, _| shell.zone) == Zone::Shelf { break; }
            rig.keys("shift-tab");
        }
        let (zone, selected) = shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        assert_eq!(zone, Zone::Shelf, "real Shift-Tab reaches the visible Spine zone");
        assert!(selected.as_deref().is_some_and(|id| id.starts_with("spine-")),
            "the zone selects a mounted Spine target, never a hidden full row: {selected:?}");
        let before_custom = folded(&mut rig);
        rig.keys("enter");
        assert_ne!(folded(&mut rig), before_custom,
            "custom shell-owner Return activates the selected Spine target once");
    }

    #[gpui::test]
    fn a_pinned_column_losing_width_or_zen_rehomes_its_zone(cx: &mut gpui::TestAppContext) {
        let mut rig = super::super::tests::rig(cx,
            Some(super::super::tests::page_route("RelationLabel")), 2560.0, 900.0);
        let shell = rig.shell.clone();
        let reader = shell.read_with(rig.cx, |shell, _| shell.reader.clone());
        let target = reader.read_with(rig.cx, |reader, _| reader.targets.placed().into_iter()
            .find(|(target, _)| target.peek.is_some()).map(|(target, _)| target.id)
            .expect("the mounted declaration offers a pin-capable peek"));
        rig.cx.update(|window, cx| shell.update(cx, |shell, cx| {
            shell.take_zone(Zone::Reader, window, cx);
            reader.read(cx).targets.focus(target);
            shell.peek(window, cx);
        }));
        rig.settle();
        rig.cx.update(|window, cx| shell.update(cx, |shell, cx| shell.peek(window, cx)));
        rig.settle();
        assert!(shell.read_with(rig.cx, |shell, _| shell.frame.is_some_and(|frame| frame.pins)),
            "a real W-Float pin created the native third column");
        rig.cx.update(|window, cx| shell.update(cx, |shell, cx| shell.take_zone(Zone::Pins, window, cx)));
        rig.cx.simulate_resize(gpui::size(px(1440.0), px(900.0)));
        rig.settle();
        assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Reader,
            "the pin survives, but its column and keyboard zone leave at narrower width");
        rig.cx.simulate_resize(gpui::size(px(2560.0), px(900.0)));
        rig.settle();
        assert!(shell.read_with(rig.cx, |shell, _| shell.frame.is_some_and(|frame| frame.pins)));
        rig.cx.update(|window, cx| shell.update(cx, |shell, cx| shell.take_zone(Zone::Pins, window, cx)));
        rig.keys("secondary-.");
        assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Reader,
            "zen cannot leave a hidden Pins zone active");
    }

    #[gpui::test]
    fn a_cover_cannot_return_focus_to_a_shelf_row_hidden_while_ask_owned_input(cx: &mut gpui::TestAppContext) {
        let mut rig = super::super::tests::rig(cx,
            Some(super::super::tests::page_route("RelationLabel")), 1440.0, 900.0);
        let shell = rig.shell.clone();
        let old = rig.cx.update(|window, cx| shell.update(cx, |shell, cx| {
            shell.take_zone(Zone::Shelf, window, cx);
            let targets = shell.shelf.read(cx).targets.clone();
            targets.focus("shelf-kind-types");
            let handle = targets.native_handle(&"shelf-kind-types".into(), cx);
            handle.focus(window, cx);
            handle
        }));
        rig.repaint();
        rig.go(Intent::OpenCommandPalette);
        let editor = shell.read_with(rig.cx, |shell, cx| shell.ask.read(cx).input().clone());
        assert!(rig.cx.update(|window, cx| editor.read(cx).focus_handle(cx).is_focused(window)),
            "Ask's actual editor owns the cover before the structural change");
        rig.cx.simulate_resize(gpui::size(px(480.0), px(900.0)));
        rig.settle();
        assert!(rig.cx.update(|window, cx| editor.read(cx).focus_handle(cx).is_focused(window)),
            "resizing the covered Shelf must not steal focus from Ask's editor");
        rig.go(Intent::DismissOverlay);
        assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Reader,
            "the deferred return resolves the saved zone against the uncovered frame");
        assert!(!rig.cx.update(|window, _| old.is_focused(window)),
            "the old native Shelf handle cannot return through a now-hidden column");
    }

    #[gpui::test]
    fn ask_returns_to_the_same_live_spine_mark_after_its_cover(cx: &mut gpui::TestAppContext) {
        let route = Route::Package(crate::navigation::PackageRoute {
            cargo: None, project: None, at: None,
            package: crate::core::PackageId::new(super::super::tests::PACKAGE).expect("fixture package"),
            lane: crate::navigation::PackageLane::Overview, selected: None,
        });
        let mut rig = super::super::tests::rig(cx, Some(route), 760.0, 900.0);
        rig.settle();
        let shell = rig.shell.clone();
        let mark: SharedString = "spine-shelf-kind-types".into();
        let handle = rig.cx.update(|window, cx| shell.update(cx, |shell, cx| {
            let targets = shell.shelf.read(cx).targets.clone();
            let handle = targets.reuse_native_handle(&mark).expect("mounted Spine mark");
            shell.set_zone(Zone::Shelf, cx);
            targets.focus(mark);
            handle.focus(window, cx);
            handle
        }));
        rig.repaint();
        rig.go(Intent::OpenCommandPalette);
        assert!(!rig.cx.update(|window, _| handle.is_focused(window)), "Ask owns the covered native editor");
        rig.go(Intent::DismissOverlay);
        assert_eq!(shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Shelf);
        assert!(rig.cx.update(|window, _| handle.is_focused(window)),
            "the unchanged Spine mark reuses its native handle across the inert cover");
    }

    #[gpui::test]
    fn real_shelf_resize_keeps_a_native_row_below_the_sticky_clip_on_its_first_frame(cx: &mut gpui::TestAppContext) {
        let mut rig = super::super::tests::rig(cx,
            Some(super::super::tests::page_route("RelationLabel")), 1440.0, 400.0);
        rig.cx.update(|_, cx| facet::probe::enable(cx));
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        let types = ledger.texts.iter().find(|text| text.content == "Types" && text.bounds.x < 264.0)
            .expect("real Shelf exposes its Types fold");
        let at = gpui::point(px(types.bounds.x + types.bounds.width / 2.0),
            px(types.bounds.y + types.bounds.height / 2.0));
        rig.cx.simulate_click(at, gpui::Modifiers::none());
        rig.settle();
        let shelf = rig.shell.read_with(rig.cx, |shell, _| shell.shelf.clone());
        assert!(shelf.update(rig.cx, |shelf, cx| {
            let moved = shelf.diagnostic_scroll_to_nested_row();
            cx.notify();
            moved
        }), "the actual folded outline has a nested native row");
        rig.repaint();
        assert!(rig.cx.debug_bounds("shelf-sticky-clip").is_some(),
            "the real Shelf paints its parent chain over the list");

        let scroll = shelf.read_with(rig.cx, |shelf, _| shelf.diagnostic_scroll_state());
        let row = shelf.read_with(rig.cx, |shelf, _| shelf.diagnostic_row_height());
        for wanted in [97.0, 99.0, 127.0] {
            // Chrome now shares height pressure with the list. Find the
            // requested native viewport through measured resizing instead
            // of assuming a window pixel always becomes a list pixel.
            let (mut lower, mut upper) = (200.0, 400.0);
            for _ in 0..16 {
                let height = (lower + upper) * 0.5;
                rig.cx.simulate_resize(gpui::size(px(1440.0), px(height)));
                rig.repaint(); // every measurement is the first resized frame
                let measured = f32::from(scroll.viewport_bounds().size.height);
                if (measured - wanted).abs() <= 0.5 { break; }
                if measured < wanted { lower = height; } else { upper = height; }
            }
            let viewport = scroll.viewport_bounds();
            let clip = rig.cx.debug_bounds("shelf-sticky-clip").expect("real current-frame sticky clip");
            let covered = shelf.read_with(rig.cx, |shelf, _| shelf.diagnostic_sticky_covered());
            assert!((f32::from(viewport.size.height) - wanted).abs() <= 1.0,
                "this fixture must exercise the requested nonmultiple viewport: {viewport:?}, wanted {wanted}");
            assert!((f32::from(clip.size.height) - f32::from(covered)).abs() <= 0.1
                && clip.bottom() <= viewport.bottom() - row,
                "first Shell paint and List prepaint must share exact sticky geometry: {clip:?}, {viewport:?}, {covered:?}");
            let first = scroll.logical_scroll_top().item_ix;
            let unobscured = (first..scroll.item_count()).filter_map(|index| scroll.bounds_for_item(index))
                .find(|bounds| bounds.top() >= clip.bottom() && bounds.bottom() <= viewport.bottom())
                .expect("a native row is fully painted outside the current-frame sticky hitbox");
            assert!(unobscured.size.height > px(0.0));
        }
    }

    #[gpui::test]
    fn real_drawer_rearms_inert_restoration_and_wheel_retires_queued_intent(cx: &mut gpui::TestAppContext) {
        let mut rig = super::super::tests::rig(cx,
            Some(super::super::tests::page_route("RelationLabel")), 360.0, 360.0);
        rig.keys("secondary-\\");
        let drawer = rig.shell.read_with(rig.cx, |shell, _| shell.shelf_over.clone());
        let anchor = drawer.read_with(rig.cx, |shelf, _|
            shelf.diagnostic_last_row_anchor()).expect("real mounted drawer has an item row");
        rig.keys("escape");
        assert!(!rig.shell.read_with(rig.cx, |shell, _| shell.shelf_over_open));
        drawer.update(rig.cx, |shelf, cx| {
            shelf.diagnostic_queue_restore(Default::default(), Some(anchor.clone()));
            cx.notify();
        });
        let waiting = drawer.read_with(rig.cx, |shelf, _|
            shelf.diagnostic_restore_ticket()).expect("checked restore queued while drawer is inert");
        rig.repaint();
        assert_eq!(drawer.read_with(rig.cx, |shelf, _| shelf.diagnostic_restore_ticket()), Some(waiting),
            "a closed drawer cannot consume its own pending measurement");

        rig.keys("secondary-\\");
        assert!(rig.shell.read_with(rig.cx, |shell, _| shell.shelf_over_open));
        assert_eq!(drawer.read_with(rig.cx, |shelf, _| shelf.diagnostic_restore_ticket()), None,
            "the same mounted owner must settle after the drawer becomes active again");

        let scroll = drawer.read_with(rig.cx, |shelf, _| shelf.diagnostic_scroll_state());
        let viewport = scroll.viewport_bounds();
        assert!(viewport.size.height > px(0.0), "the reopened native list has a real viewport");
        assert!(scroll.max_offset_for_scrollbar().y > px(0.0), "the real drawer has wheel travel");
        drawer.update(rig.cx, |shelf, cx| {
            shelf.diagnostic_queue_restore(Default::default(), Some(anchor));
            cx.notify();
        });
        let queued = drawer.read_with(rig.cx, |shelf, _|
            shelf.diagnostic_restore_ticket()).expect("second restore is queued before native input");
        let before_wheel = scroll.logical_scroll_top();
        crate::shell::tests::wheel(rig.cx, viewport.center(), gpui::point(px(0.0), px(120.0)));
        let wheeled = scroll.logical_scroll_top();
        assert!(wheeled.item_ix != before_wheel.item_ix || wheeled.offset_in_item != before_wheel.offset_in_item,
            "native wheel moved the real drawer before its queued restore prepaint");
        rig.repaint();
        assert_ne!(drawer.read_with(rig.cx, |shelf, _| shelf.diagnostic_restore_ticket()), Some(queued),
            "the real list wheel handler supersedes an older restore before its callback");
        rig.settle();
        let settled = scroll.logical_scroll_top();
        assert_eq!(settled.item_ix, wheeled.item_ix);
        assert_eq!(settled.offset_in_item, wheeled.offset_in_item,
            "the retired proportional adjustment cannot move the newer wheel position");
    }

    #[gpui::test]
    fn mounted_shelf_click_keeps_its_scene_but_back_never_revives_old_callbacks(cx: &mut gpui::TestAppContext) {
        let route = super::super::tests::page_route("RelationLabel");
        let mut rig = super::super::tests::rig(cx, Some(route.clone()), 1440.0, 900.0);
        let shell = rig.shell.clone();
        let scope = shell.read_with(rig.cx, |shell, cx| shell.shelf_input_scope(false, cx).expect("docked shelf scene"));
        let retired = shell.read_with(rig.cx, |shell, cx| {
            let list = shell.shelf.read(cx).targets.list_probe().upgrade().expect("mounted shelf targets");
            let target = list.borrow().first().cloned().expect("mounted actionable target");
            target.action.callback()
        });
        let tab = super::super::tests::native_bounds(&mut rig, "Tab", "Used by", true).expect("mounted native tab");
        rig.cx.simulate_click(tab.center(), gpui::Modifiers::none());
        rig.settle();
        assert!(shell.read_with(rig.cx, |shell, cx| shell.admits_shelf_input_scope(&scope, cx)), "ordinary mouse down/up does not retire the painted scene");
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native Shelf tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("tree JSON");
        assert!(tree["nodes"].as_object().expect("nodes").values().any(|node| node["aria"]["label"] == "Used by" && node["aria"]["selected"] == true), "native click selected the actual tab: {tree}");
        rig.go(Intent::Navigate(Route::World));
        rig.keys("secondary-[");
        assert_eq!(rig.route(), route);
        assert!(!shell.read_with(rig.cx, |shell, cx| shell.admits_shelf_input_scope(&scope, cx)), "history can restore reading intent, never a retired native scene");
        let focused = rig.cx.update(|window, cx| window.focused(cx));
        rig.cx.update(|window, cx| retired(window, cx));
        rig.settle();
        assert_eq!(rig.route(), route, "old painted target cannot navigate after Back");
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused, "denied old callback does not take focus");
        let fresh = shell.read_with(rig.cx, |shell, cx| shell.shelf_input_scope(false, cx).expect("fresh docked shelf scene"));
        assert!(shell.read_with(rig.cx, |shell, cx| shell.admits_shelf_input_scope(&fresh, cx)));
        rig.cx.simulate_resize(gpui::size(px(360.0), px(900.0)));
        rig.settle();
        rig.keys("secondary-\\");
        let drawer = shell.read_with(rig.cx, |shell, cx| shell.shelf_input_scope(true, cx).expect("current drawer"));
        assert!(shell.read_with(rig.cx, |shell, cx| shell.admits_shelf_input_scope(&drawer, cx)));
        assert!(!shell.read_with(rig.cx, |shell, cx| shell.admits_shelf_input_scope(&fresh, cx)), "drawer cannot borrow a docked callback");
        rig.keys("escape");
        assert!(!shell.read_with(rig.cx, |shell, cx| shell.admits_shelf_input_scope(&drawer, cx)), "closed drawer callbacks stay retired");
        rig.cx.simulate_resize(gpui::size(px(1440.0), px(900.0)));
        rig.settle();
        let current_tab = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("fresh docked tab after drawer dismissal");
        rig.cx.simulate_click(current_tab.center(), gpui::Modifiers::none());
        rig.settle();
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("fresh native tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("tree JSON");
        assert!(tree["nodes"].as_object().expect("nodes").values().any(|node| node["aria"]["label"] == "Rests on" && node["aria"]["selected"] == true), "a refreshed cached column has live callbacks: {tree}");
    }
}

#[cfg(test)]
mod responsive_shelf_scene_tests {
    use super::*;

    fn scope(rig: &mut super::super::tests::Rig, drawer: bool) -> ShelfInputScope {
        rig.shell.read_with(rig.cx, |shell, cx| shell.shelf_input_scope(drawer, cx).expect("current native shelf scope"))
    }
    fn admitted(rig: &mut super::super::tests::Rig, scope: &ShelfInputScope) -> bool {
        rig.shell.read_with(rig.cx, |shell, cx| shell.admits_shelf_input_scope(scope, cx))
    }
    fn resize(rig: &mut super::super::tests::Rig, width: f32) {
        rig.cx.simulate_resize(gpui::size(px(width), px(1400.0)));
        rig.settle();
    }
    fn selected_tab(rig: &mut super::super::tests::Rig, label: &str) -> bool {
        let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native Shelf tree");
        let tree: serde_json::Value = serde_json::from_str(&json).expect("tree JSON");
        tree["nodes"].as_object().expect("nodes").values().any(|node| node["aria"]["label"] == label && node["aria"]["selected"] == true)
    }

    #[gpui::test]
    fn native_shelf_press_cannot_survive_unpainted_toggle_pairs(cx: &mut gpui::TestAppContext) {
        for percent in [100_u16, 200] {
            for zen in [false, true] {
                let scale = f32::from(percent) / 100.0;
                let mut rig = super::super::tests::rig(cx, Some(super::super::tests::page_route("RelationLabel")), 1440.0 * scale, 1400.0);
                let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
                rig.go(Intent::ZoomTo { display, percent });
                let used = super::super::tests::native_bounds(&mut rig, "Tab", "Used by", true).expect("native current tab");
                rig.cx.simulate_click(used.center(), gpui::Modifiers::none());
                rig.settle();
                let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("native held tab");
                let old = scope(&mut rig, false);
                rig.cx.simulate_mouse_down(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
                let renders = rig.shell.read_with(rig.cx, |shell, _| shell.renders);
                // Actual action dispatch, not direct callbacks or seeded state;
                // the two ownership transitions have no intervening draw.
                rig.cx.update(|window, cx| {
                    if zen {
                        window.dispatch_action(Box::new(keys::Zen), cx);
                        window.dispatch_action(Box::new(keys::Zen), cx);
                    } else {
                        window.dispatch_action(Box::new(keys::ToggleShelf), cx);
                        window.dispatch_action(Box::new(keys::ToggleShelf), cx);
                    }
                });
                rig.cx.run_until_parked();
                assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.renders), renders, "pair must exercise the no-paint boundary");
                assert!(!admitted(&mut rig, &old), "unpainted ownership cycle retires the old receipt");
                rig.cx.simulate_mouse_up(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
                rig.settle();
                assert!(selected_tab(&mut rig, "Used by"), "old held press cannot activate after an unpainted cycle");
                let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("fresh native tab");
                rig.cx.simulate_click(rests.center(), gpui::Modifiers::none());
                rig.settle();
                assert!(selected_tab(&mut rig, "Rests on"), "fresh gesture remains admitted");
            }
        }
    }

    #[gpui::test]
    fn native_local_settings_press_survives_unrelated_settings_publication(cx: &mut gpui::TestAppContext) {
        for percent in [100_u16, 200] {
            let scale = f32::from(percent) / 100.0;
            let mut rig = super::super::tests::rig(cx, None, 1440.0 * scale, 1400.0 * scale);
            let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
            rig.go(Intent::ZoomTo { display, percent });
            rig.go(Intent::OpenSettings(crate::navigation::SettingsPage::Appearance));
            let full = super::super::tests::native_bounds(&mut rig, "RadioButton", "Full", true).expect("actual local motion control");
            rig.cx.simulate_mouse_down(full.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            let scope = rig.shell.read_with(rig.cx, |shell, _| shell.local_activation_scope());
            rig.go(Intent::SetAppearance(crate::model::AppearancePreference::Glacier));
            assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.local_activation_scope()), scope, "appearance does not change native layout ownership");
            rig.cx.simulate_mouse_up(full.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().settings().motion), crate::model::MotionPreference::Full);
        }
    }

    #[gpui::test]
    fn responsive_hide_show_retires_actions_and_presses_but_plain_resize_keeps_them(cx: &mut gpui::TestAppContext) {
        for percent in [100_u16, 200] {
            let scale = f32::from(percent) / 100.0;
            let route = super::super::tests::page_route("RelationLabel");
            let mut rig = super::super::tests::rig(cx, Some(route.clone()), 1440.0 * scale, 1400.0);
            let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
            rig.go(Intent::ZoomTo { display, percent });
            let dock = scope(&mut rig, false);
            assert_eq!(dock.surface(), ShelfNativeSurface::Docked);
            let retired = rig.shell.read_with(rig.cx, |shell, cx| {
                let list = shell.shelf.read(cx).targets.list_probe().upgrade().expect("native shelf targets");
                let target = list.borrow().first().cloned().expect("actionable native target");
                target.action.callback()
            });
            resize(&mut rig, 1480.0 * scale);
            assert!(admitted(&mut rig, &dock), "pixel-only resize preserves native owner at {percent}%");
            resize(&mut rig, 360.0 * scale);
            assert!(!admitted(&mut rig, &dock));
            resize(&mut rig, 1440.0 * scale);
            assert!(!admitted(&mut rig, &dock), "hide/show cannot revive dock callbacks");
            let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone());
            let focused = rig.cx.update(|window, cx| window.focused(cx));
            rig.cx.update(|window, cx| retired(window, cx));
            rig.settle();
            assert_eq!(rig.route(), route);
            assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.clone()), before);
            assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
            let used = super::super::tests::native_bounds(&mut rig, "Tab", "Used by", true).expect("fresh native tab");
            rig.cx.simulate_click(used.center(), gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Used by"));
            let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("native press target");
            rig.cx.simulate_mouse_down(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            resize(&mut rig, 360.0 * scale);
            resize(&mut rig, 1440.0 * scale);
            rig.cx.simulate_mouse_up(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Used by"), "old down cannot transfer into a repainted scene");
            let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("fresh native press target");
            rig.cx.simulate_click(rests.center(), gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Rests on"), "new scene callbacks remain live");
            let column = scope(&mut rig, false);
            rig.keys("secondary-\\");
            assert_eq!(scope(&mut rig, false).surface(), ShelfNativeSurface::Spine);
            assert!(!admitted(&mut rig, &column), "settings shelf close changes native surface");
            rig.keys("secondary-\\");
            assert_eq!(scope(&mut rig, false).surface(), ShelfNativeSurface::Docked);
            assert!(!admitted(&mut rig, &column), "settings shelf reopen cannot revive its old column");
            let current = scope(&mut rig, false);
            rig.keys("secondary-shift-.");
            assert!(!admitted(&mut rig, &current), "zen retires visible shelf ownership");
            rig.keys("secondary-shift-.");
            assert!(!admitted(&mut rig, &current), "leaving zen cannot revive its predecessor");
        }
    }

    #[gpui::test]
    fn responsive_spine_and_automatic_drawer_close_have_distinct_live_surfaces(cx: &mut gpui::TestAppContext) {
        for percent in [100_u16, 200] {
            let scale = f32::from(percent) / 100.0;
            let mut rig = super::super::tests::rig(cx, Some(super::super::tests::page_route("RelationLabel")), 1440.0 * scale, 1400.0);
            let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
            rig.go(Intent::ZoomTo { display, percent });
            let dock = scope(&mut rig, false);
            resize(&mut rig, 760.0 * scale);
            let spine = scope(&mut rig, false);
            assert_eq!(spine.surface(), ShelfNativeSurface::Spine);
            assert!(!admitted(&mut rig, &dock), "full-column ownership cannot become Spine");
            rig.cx.update(|window, _| window.set_a11y_forced(true));
            rig.repaint();
            let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("native Spine tree");
            let tree: serde_json::Value = serde_json::from_str(&json).expect("tree JSON");
            let node = tree["nodes"].as_object().expect("nodes").values().find(|node| node["aria"]["role"] == "Button"
                && node["element_id"].as_str().is_some_and(|id| id.contains("spine-"))).expect("actual native Spine button");
            let debug_id = node["element_id"].as_str().expect("Spine ID");
            let name = debug_id.strip_prefix("Name(").and_then(|id| id.strip_suffix(')')).expect("named Spine ID");
            let id: String = serde_json::from_str(name).expect("native debug name string");
            let label = node["aria"]["label"].as_str().expect("Spine label");
            let button = super::super::tests::native_bounds_id(&mut rig, &id, "Button", label, true).expect("fresh mounted Spine target");
            let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
            rig.cx.simulate_click(button.center(), gpui::Modifiers::none());
            rig.settle();
            let after = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
            assert!(after.route() != before.route() || after.session().reading.current.presentation != before.session().reading.current.presentation,
                "actual Spine click must activate its typed row, not be rejected as hidden: {id}/{label}");
            resize(&mut rig, 360.0 * scale);
            rig.keys("secondary-\\");
            let drawer = scope(&mut rig, true);
            let used = super::super::tests::native_bounds(&mut rig, "Tab", "Used by", true).expect("current drawer tab");
            rig.cx.simulate_click(used.center(), gpui::Modifiers::none());
            rig.settle();
            let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("drawer press target");
            rig.cx.simulate_mouse_down(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            resize(&mut rig, 1440.0 * scale);
            assert!(!admitted(&mut rig, &drawer), "responsive automatic close retires its drawer scene");
            resize(&mut rig, 360.0 * scale);
            rig.keys("secondary-\\");
            assert!(!admitted(&mut rig, &drawer), "reopen cannot revive an automatically closed drawer");
            let fresh = scope(&mut rig, true);
            assert_eq!(fresh.surface(), ShelfNativeSurface::Drawer);
            let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("fresh drawer tab");
            rig.cx.simulate_mouse_up(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Used by"), "automatic close/reopen cannot transfer a held drawer press");
            rig.cx.simulate_click(rests.center(), gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Rests on"), "fresh drawer gesture remains live");
        }
    }

    #[gpui::test]
    fn producer_identity_retires_a_held_native_press_but_observation_does_not(cx: &mut gpui::TestAppContext) {
        use crate::runtime::{owner::{OwnerGate, OwnerState}, reads::ReadPool};
        use crate::core::VersionedRoot;
        use std::sync::Arc;
        for percent in [100_u16, 200] {
            let scale = f32::from(percent) / 100.0;
            let root = VersionedRoot::synthetic(
                backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
            let gate = OwnerGate::ready(root, crate::model::ServiceMode::Attached);
            let mut rig = super::super::tests::rig_with_engine_gate(cx,
                Some(super::super::tests::page_route("RelationLabel")), 1440.0 * scale, 1400.0,
                ReadPool::start(2, |_| super::super::tests::Fixture).expect("pool"),
                super::super::tests::RootOnly, Some(gate.clone()));
            let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
            rig.go(Intent::ZoomTo { display, percent });
            let initial = scope(&mut rig, false);
            // Observation is local metadata, not a producer replacement. Preserve
            // an actual held press across its publication and repaint.
            let used = super::super::tests::native_bounds(&mut rig, "Tab", "Used by", true).expect("native tab");
            rig.cx.simulate_mouse_down(used.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.graph.store.update(rig.cx, |store, cx| {
                let snapshot = store.snapshot();
                let key = snapshot.key();
                store.admit_snapshot(Arc::new(snapshot.with_key(key.observed_at(key.observation() + 1), None)), cx);
            });
            rig.repaint();
            assert!(admitted(&mut rig, &initial));
            rig.cx.simulate_mouse_up(used.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Used by"));

            // Coalesced same-root Ready replacement: no intermediate Starting
            // paint is required to revoke the press from the predecessor.
            let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("native tab");
            let previous = scope(&mut rig, false);
            rig.cx.simulate_mouse_down(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
            rig.settle();
            assert!(!admitted(&mut rig, &previous));
            rig.cx.simulate_mouse_up(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Used by"), "new owner listeners cannot inherit old down");

            gate.publish(OwnerState::Failed("same fault".into()));
            rig.settle();
            let previous_fault = scope(&mut rig, false);
            let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("local tab remains usable during owner failure");
            rig.cx.simulate_mouse_down(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            gate.publish(OwnerState::Failed("same fault".into()));
            rig.settle();
            assert!(!admitted(&mut rig, &previous_fault), "same words do not identify the retry attachment");
            rig.cx.simulate_mouse_up(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Used by"));

            // Change the actual factory authority while keeping the route and
            // local presentation. This changes only the synthetic test factory authority.
            let previous_root = scope(&mut rig, false);
            let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("native tab");
            rig.cx.simulate_mouse_down(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.graph.store.update(rig.cx, |store, cx| {
                let snapshot = store.snapshot();
                let key = snapshot.key();
                store.admit_snapshot(Arc::new(snapshot.with_key(key.with_generation(key.generation() + 1), None)), cx);
            });
            rig.settle();
            assert!(!admitted(&mut rig, &previous_root));
            rig.cx.simulate_mouse_up(rests.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Used by"));
            let rests = super::super::tests::native_bounds(&mut rig, "Tab", "Rests on", true).expect("fresh native tab");
            rig.cx.simulate_click(rests.center(), gpui::Modifiers::none());
            rig.settle();
            assert!(selected_tab(&mut rig, "Rests on"), "fresh local gesture still acts");
            let failed = scope(&mut rig, false);
            gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
            rig.settle();
            assert!(!admitted(&mut rig, &failed), "recovery changes the native producer receipt");
        }
    }

    #[gpui::test]
    fn local_settings_press_survives_producer_replacement_but_not_coalesced_cover(cx: &mut gpui::TestAppContext) {
        use crate::runtime::{owner::{OwnerGate, OwnerState}, reads::ReadPool};
        use crate::{core::VersionedRoot, model::MotionPreference, navigation::SettingsPage};
        for percent in [100_u16, 200] {
            let scale = f32::from(percent) / 100.0;
            let root = VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
            let gate = OwnerGate::ready(root, crate::model::ServiceMode::Attached);
            let mut rig = super::super::tests::rig_with_engine_gate(cx, None, 1440.0 * scale, 1400.0 * scale,
                ReadPool::start(2, |_| super::super::tests::Fixture).expect("pool"), super::super::tests::RootOnly, Some(gate.clone()));
            let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
            rig.go(Intent::ZoomTo { display, percent });
            rig.go(Intent::OpenSettings(SettingsPage::Appearance));
            let full = super::super::tests::native_bounds(&mut rig, "RadioButton", "Full", true).expect("mounted local Settings control");
            rig.cx.simulate_mouse_down(full.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            let focus = rig.cx.update(|window, cx| window.focused(cx));
            let receipt = rig.shell.read_with(rig.cx, |shell, _| shell.local_activation_scope());
            gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
            rig.settle();
            assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.local_activation_scope()), receipt);
            assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focus, "producer replacement keeps the local native focus handle");
            rig.cx.simulate_mouse_up(full.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().settings().motion), MotionPreference::Full);
            rig.go(Intent::SetMotion(MotionPreference::System));
            let full = super::super::tests::native_bounds(&mut rig, "RadioButton", "Full", true).expect("current local control");
            rig.cx.simulate_mouse_down(full.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            let before = rig.shell.read_with(rig.cx, |shell, _| shell.local_activation_scope());
            let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
            let links = reader.read_with(rig.cx, |reader, _| reader.navigation_links());
            // Both real store transitions occur before another explicit paint.
            // Merely sampling the final overlay would miss this ownership cycle.
            rig.cx.update(|_, cx| {
                links.dispatch(Intent::OpenCommandPalette, cx);
                links.dispatch(Intent::DismissOverlay, cx);
            });
            rig.cx.run_until_parked();
            assert_ne!(rig.shell.read_with(rig.cx, |shell, _| shell.local_activation_scope()), before);
            rig.settle();
            let full = super::super::tests::native_bounds(&mut rig, "RadioButton", "Full", true).expect("returned Settings control");
            rig.cx.simulate_mouse_up(full.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().settings().motion), MotionPreference::System, "an uncovered field cannot inherit its covered down");
            rig.cx.simulate_click(full.center(), gpui::Modifiers::none());
            rig.settle();
            assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().settings().motion), MotionPreference::Full);
            // Wheel input uses the existing reader scroll state, not a newly
            // namespaced ancestor. Publication cannot reset that state.
            rig.cx.simulate_resize(gpui::size(px(1440.0 * scale), px(568.0 * scale)));
            rig.settle();
            rig.repaint();
            let viewport = rig.cx.debug_bounds("reader-scroll").expect("actual Settings reader viewport");
            let before_wheel = rig.shell.read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx));
            rig.cx.simulate_event(gpui::ScrollWheelEvent { position: viewport.center(), delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-320.0))), modifiers: gpui::Modifiers::none(), touch_phase: gpui::TouchPhase::Moved });
            rig.draw();
            let offset = rig.shell.read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx));
            assert_ne!(offset, before_wheel, "actual native wheel moves the local Settings viewport");
            gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
            rig.settle();
            assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx)), offset);
        }
    }

    #[gpui::test]
    fn mounted_find_query_entity_draft_and_focus_survive_same_root_replacement(cx: &mut gpui::TestAppContext) {
        use crate::runtime::{owner::{OwnerGate, OwnerState}, reads::ReadPool};
        use crate::{core::VersionedRoot, navigation::{BrowseRoute, OrbitRoute}};
        for percent in [100_u16, 200] {
            let root = VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]), 4);
            let gate = OwnerGate::ready(root, crate::model::ServiceMode::Attached);
            let scale = f32::from(percent) / 100.0;
            let mut rig = super::super::tests::rig_with_engine_gate(cx, None, 1440.0 * scale, 1400.0 * scale,
                ReadPool::start(2, |_| super::super::tests::Fixture).expect("pool"), super::super::tests::RootOnly, Some(gate.clone()));
            let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
            rig.go(Intent::ZoomTo { display, percent });
            rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))));
            let example = super::super::tests::native_bounds(&mut rig, "Button", "from_str", true).expect("mounted local Find example");
            rig.cx.simulate_mouse_down(example.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
            rig.settle();
            rig.cx.simulate_mouse_up(example.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
            rig.settle();
            assert!(matches!(rig.route(), Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))) if query.text.as_ref() == "from_str"), "a held local example still refines exactly once after producer replacement");
            rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))));
            rig.cx.update(|window, _| window.set_a11y_forced(true));
            rig.repaint();
            let query = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("Find native tree");
            let tree: serde_json::Value = serde_json::from_str(&query).expect("tree");
            let role = tree["nodes"].as_object().expect("nodes").values().find(|node| node["aria"]["label"] == "Find query")
                .and_then(|node| node["aria"]["role"].as_str()).expect("mounted query role").to_owned();
            let query = super::super::tests::native_bounds(&mut rig, &role, "Find query", true).expect("actual Find query");
            rig.cx.simulate_click(query.center(), gpui::Modifiers::none());
            rig.cx.simulate_input("local unsent draft");
            let focused = rig.cx.update(|window, cx| window.focused(cx));
            gate.publish(OwnerState::Ready { key: root, mode: crate::model::ServiceMode::Attached });
            // Repaint before the editing debounce commits the query route.
            rig.cx.run_until_parked();
            rig.repaint();
            assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
            let json = rig.cx.update(|window, _| window.debug_a11y_tree_json()).expect("renewed native tree");
            let tree: serde_json::Value = serde_json::from_str(&json).expect("tree");
            assert!(tree["nodes"].as_object().expect("nodes").values().any(|node| node["aria"]["label"] == "Find query" && node["aria"]["value"] == "local unsent draft"), "actual typed draft survives producer-only repaint: {tree}");
        }
    }

}

#[cfg(test)]
mod native_hint_receipt_tests {
    use super::*;
    use crate::model::pages::{Known, PageValue, SourceOrigin, SourceText};
    use crate::navigation::OrbitRoute;
    use gpui::AppContext as _;

    fn source_hint(cx: &mut gpui::TestAppContext) -> (super::super::tests::Rig, Hinted, HintScope) {
        let mut rig = super::super::tests::rig(cx,
            Some(super::super::tests::view_route("RelationLabel", View::Code)), 1440.0, 900.0);
        rig.keys("f");
        let (choice, scope) = rig.shell.read_with(rig.cx, |shell, _| {
            let choice = shell.hinted_target_for("source-copy-excerpt").expect("original mounted source hint");
            let scope = shell.hints.as_ref().expect("actual hint session").scope.clone();
            (choice, scope)
        });
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        assert!(rig.cx.update(|window, _| targets.admits_mount(&choice.mount, window)));
        assert!(rig.cx.update(|_, cx| choice.target.action.admits(cx)), "positive original resource receipt");
        (rig, choice, scope)
    }

    #[gpui::test]
    fn same_key_same_label_model_replacement_cannot_rebind_an_original_hint_action(cx: &mut gpui::TestAppContext) {
        let (mut rig, choice, scope) = source_hint(cx);
        let symbol = route_symbol(&rig.route()).expect("source route declaration");
        rig.graph.store.update(rig.cx, |store, _| {
            let mut source = store.source(&symbol).loaded_value().cloned().expect("current immutable source fixture");
            source.text = Known::Known(SourceText::new(Arc::from(
                "// replaced\npub enum RelationLabel {\n    Replacement,\n}\n// changed\n"),
                137, SourceOrigin::LocalFile, true).expect("valid replacement source"));
            crate::runtime::store::cargo_context_tests::force_land(store, &PageKey::Source(symbol), PageValue::Source(source));
        });
        let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
        reader.update(rig.cx, |_, cx| cx.notify());
        rig.repaint();
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        assert!(targets.mount_claim("source-copy-excerpt").is_some(), "same current copy control actually repainted");
        assert!(!rig.cx.update(|_, cx| choice.target.action.admits(cx)),
            "the original immutable source receipt refuses replacement even when visible key/label agree");
        rig.cx.write_to_clipboard(gpui::ClipboardItem::new_string("replacement-hint-sentinel".into()));
        let focused = rig.cx.update(|window, cx| window.focused(cx));
        let shell = rig.shell.clone();
        rig.cx.update(|window, app| shell.update(app, |shell, cx| shell.activate_hint(choice, &scope, window, cx)));
        rig.settle();
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
        assert_eq!(rig.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("replacement-hint-sentinel"));
    }

    #[gpui::test]
    fn a_later_native_focus_intent_wins_over_a_deferred_hint_on_the_same_painted_visit(cx: &mut gpui::TestAppContext) {
        let (mut rig, choice, scope) = source_hint(cx);
        rig.cx.write_to_clipboard(gpui::ClipboardItem::new_string("later-input-hint-sentinel".into()));
        let shell = rig.shell.clone();
        let original = choice.clone();
        rig.cx.update(|window, app| {
            let later = shell.clone();
            window.defer(app, move |window, app| {
                later.update(app, |shell, cx| shell.take_zone(Zone::Titlebar, window, cx));
                let targets = later.read(app).reader.read(app).targets.clone();
                assert!(targets.admits_mount(&original.mount, window), "the same physical source control is still mounted");
                assert!(original.target.action.admits(app), "its original source authority is still current");
            });
            shell.update(app, |shell, cx| shell.activate_hint(choice, &scope, window, cx));
        });
        rig.settle();
        assert_eq!(rig.shell.read_with(rig.cx, |shell, _| shell.zone), Zone::Titlebar);
        assert_eq!(rig.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("later-input-hint-sentinel"));
    }

    #[gpui::test]
    fn same_route_after_navigation_has_a_new_visit_and_cannot_reuse_an_original_hint(cx: &mut gpui::TestAppContext) {
        let (mut rig, choice, scope) = source_hint(cx);
        let route = rig.route();
        rig.go(Intent::Navigate(Route::Orbit(OrbitRoute::Home)));
        rig.go(Intent::Navigate(route.clone()));
        assert_ne!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.id), scope.visit);
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        assert!(targets.mount_claim("source-copy-excerpt").is_some());
        assert!(!rig.cx.update(|window, _| targets.admits_mount(&choice.mount, window)));
        rig.cx.write_to_clipboard(gpui::ClipboardItem::new_string("new-visit-hint-sentinel".into()));
        let focused = rig.cx.update(|window, cx| window.focused(cx));
        let shell = rig.shell.clone();
        rig.cx.update(|window, app| shell.update(app, |shell, cx| shell.activate_hint(choice, &scope, window, cx)));
        rig.settle();
        assert_eq!(rig.route(), route);
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
        assert_eq!(rig.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("new-visit-hint-sentinel"));
    }

    #[gpui::test]
    fn returning_from_a_cover_cannot_reuse_a_departed_hint(cx: &mut gpui::TestAppContext) {
        let (mut rig, choice, scope) = source_hint(cx);
        let route = rig.route();
        rig.go(Intent::OpenSettings(SettingsPage::Appearance));
        rig.go(Intent::DismissOverlay);
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        assert!(targets.mount_claim("source-copy-excerpt").is_some(), "the destination's fresh source control painted");
        assert!(!rig.cx.update(|window, _| targets.admits_mount(&choice.mount, window)),
            "same route/key after a painted Settings cover is a new mount");
        rig.cx.write_to_clipboard(gpui::ClipboardItem::new_string("remount-hint-sentinel".into()));
        let focused = rig.cx.update(|window, cx| window.focused(cx));
        let shell = rig.shell.clone();
        rig.cx.update(|window, app| shell.update(app, |shell, cx| shell.activate_hint(choice, &scope, window, cx)));
        rig.settle();
        assert_eq!(rig.route(), route);
        assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
        assert_eq!(rig.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("remount-hint-sentinel"));
    }
}
