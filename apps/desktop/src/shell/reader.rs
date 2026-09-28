//! The reader region: the page, laid out from its own measured width.
//!
//! The folio holds the page at a reading measure (784 px at 100 % text) and
//! centres it; a body's margin notes sit in a 250 px column beside their
//! block when the reader's room is wide, and fold under their block
//! otherwise.
//!
//! A place change is a plate, never a fade (W-Motion PLAN §2a, "plates, not
//! fades"). Going *into* a row (deeper, or across into a page the old page
//! links to), the clicked row's plate opens into the new page: its top edge
//! rises to the reader's top and its bottom edge falls to the floor, the old
//! page drifts 16 px left outside it, and the new page prints beneath in
//! reading order. Coming *back* (history, or up to the page that opened
//! this one), the body folds up from its last line, then the plate closes
//! into the row it came from, uncovering the parent, and that row keeps a
//! periwinkle "where you were" tint for a moment. With no row to carry the
//! change, the plate enters from the reader's right edge (in, rightward) and
//! leaves the same way. One [`Carry`] spring drives each change; Back mid-
//! open retargets it from its painted value and velocity. The two pages are
//! never drawn in the same place: the one outside the plate is cut to the
//! region the plate has not reached, the one inside to the plate, so no
//! frame shows two texts over each other. A view switch cuts (the Peel is a
//! later slice); reduced motion cuts, and a Close still marks the row.

use super::bodies::{self, Ctx, Lens, Pages};
use super::focus::Targets;
use super::kit::HoverIntent;
use super::region::{Links, Region, RegionCore};
use super::jump::{route_package, route_symbol};
use crate::model::AppSnapshot;
use crate::model::pages::PageKey;
use crate::navigation::{Overlay, Route, View};
use crate::runtime::store::{Branch, DataStore, StoreEvent};
use facet::motion::presence::{Act, Entry, Extent};
use facet::motion::{Carry, Edge, Keys, Pose, Presence, band, masked, offset, print};
use facet::tokens::motion;
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Room, Space};
use gpui::{
    AppContext as _, Bounds, Context, ElementId, Entity, InteractiveElement, IntoElement, ParentElement, Pixels, Point,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div, point, px, size,
};
use std::cell::Cell;
use std::rc::Rc;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// A disclosure has a semantic identity, never a position in a render.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum SymbolFold {
    Relations(&'static str),
    RelationPackage(&'static str, u32),
    IndexedRelationPage(&'static str, usize),
    Methods(crate::model::pages::Receiver),
    Member(String),
    Capabilities,
    Docs,
    Uses,
}

/// Kept per declaration across lens changes; bounded to protect long sessions.
#[derive(Clone, Default)]
pub(crate) struct SymbolDisclosure {
    scope: String,
    open: BTreeSet<SymbolFold>,
    unrolls: Rc<std::cell::RefCell<std::collections::BTreeMap<SymbolFold, Presence>>>,
}

impl SymbolDisclosure {
    fn for_symbol(symbol: &crate::model::pages::SymbolRef) -> Self { Self { scope: symbol.as_str().to_owned(), ..Self::default() } }
    pub(crate) fn is_open(&self, fold: &SymbolFold) -> bool { self.open.contains(fold) }
    pub(crate) fn indexed_relation_page(&self, label: &'static str) -> usize {
        self.open.iter().find_map(|fold| match fold {
            SymbolFold::IndexedRelationPage(word, page) if *word == label => Some(*page),
            _ => None,
        }).unwrap_or(0)
    }
    pub(crate) fn unroll(&self, fold: SymbolFold) -> Presence {
        let mut unrolls = self.unrolls.borrow_mut();
        if unrolls.len() >= 128 && !unrolls.contains_key(&fold) {
            if let Some(key) = unrolls.keys().next().cloned() { unrolls.remove(&key); }
        }
        unrolls.entry(fold.clone()).or_insert_with(|| Presence::new(format!("symbol-unroll-{}-{fold:?}", self.scope))).clone()
    }
    fn toggle(&mut self, fold: SymbolFold) {
        if let SymbolFold::IndexedRelationPage(label, page) = &fold {
            self.open.retain(|item| !matches!(item, SymbolFold::IndexedRelationPage(word, _) if word == label));
            if *page > 0 { self.open.insert(fold.clone()); }
            return;
        }
        if !self.open.remove(&fold) && self.open.len() < 128 { self.open.insert(fold); }
    }
}

/// The reading measure at 100 % text.
pub(crate) const FOLIO: f32 = 784.0;
/// The margin column at 100 % text.
pub(crate) const MARGIN: f32 = 250.0;
/// The gutter between folio and margin at 100 % text.
pub(crate) const GUTTER: f32 = 34.0;

/// Which way the last route change moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Way {
    /// Deeper: the plate opens.
    Down,
    /// Graph to page: the page cuts in while its focused gem morphs.
    GraphPage,
    /// Shallower: the plate closes.
    Up,
    /// Same depth: the plate opens from the linking row, or from the right.
    Across,
    /// The same declaration another way: a cut (W-Flow's shared elements
    /// morph the marks that carry the same id).
    View,
}

/// The plate move a place change plays (W-Motion PLAN §2a).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Verb {
    /// Into a row: its plate opens into the new page.
    Open,
    /// Back out: the page's plate closes into the row it came from.
    Close,
}

/// The row that carried a place change: which target drew it, where
/// (window space), and how far the page it sits on was scrolled.
#[derive(Clone, Debug, PartialEq)]
struct Origin {
    id: SharedString,
    rect: Bounds<Pixels>,
    scroll: Point<Pixels>,
}

/// A place change seen by `observe`, turned into a [`Transit`] by the next
/// render (which has the clock, the pointer and the reader's bounds).
#[derive(Clone, Debug)]
struct Arrival {
    verb: Verb,
    /// The page arriving (the new current place).
    key: u64,
    /// The page it replaces.
    leaving: u64,
    /// The leaving page's scroll offset: it stays where it was on screen.
    scroll: Point<Pixels>,
    /// Open: the old page's rows that link to the new page; Close: the row
    /// the history entry remembers.
    rows: Vec<(SharedString, Bounds<Pixels>)>,
    /// Close: which page key a row on the arriving page must link to (a
    /// route up, with no remembered row).
    find: Option<PageKey>,
    /// The target the keyboard stood on as it left.
    focused: Option<SharedString>,
}

/// A place change in flight: one plate on one [`Carry`].
#[derive(Clone, Debug)]
struct Transit {
    verb: Verb,
    /// The page inside the plate (Open: the arriving one; Close: the leaving one).
    inside: u64,
    /// The page outside it (Open: the leaving one; Close: the arriving one).
    outside: u64,
    /// The row it opens from or closes into (window space); `None`: the
    /// reader's right edge.
    row: Option<Bounds<Pixels>>,
    /// Close: the arriving page's target that draws that row, followed
    /// while the body folds (its layout is only known once it is drawn).
    row_id: Option<SharedString>,
    /// Close with no remembered row: the page key a row must link to, looked
    /// up on the arriving page until the fold ends.
    find: Option<PageKey>,
    /// The plate's openness `p`: 0 = the row, 1 = the whole reader.
    carry: Carry,
    /// When the change began.
    start: Instant,
    /// The leaving page's scroll offset.
    scroll: Point<Pixels>,
    /// The inside page's edge: Open prints down from the page's top after
    /// [`PRINT_AFTER`]; Close folds up from `(when, reader-local y)`.
    fold: Option<(Instant, f32)>,
}

impl Transit {
    /// The inside page's edge, reader-local: an Open prints down from the
    /// reader's top from [`PRINT_AFTER`] on (above it before then: nothing
    /// printed); a Close folds up from where its fold began.
    fn edge_local(&self, now: Instant, height: f32) -> Option<f32> {
        let since = |at: Instant| {
            #[allow(clippy::cast_precision_loss)]
            let ms = now.saturating_duration_since(at).as_micros() as f32 / 1000.0;
            ms
        };
        match (self.verb, self.fold) {
            (_, Some((at, from))) => Some(from - height / FOLD * since(at)),
            (Verb::Open, None) => {
                let started = self.start + PRINT_AFTER;
                let ms = if now >= started { since(started) } else { -(started.saturating_duration_since(now).as_secs_f32() * 1000.0) };
                Some(PRINT_SPEED * ms)
            }
            (Verb::Close, None) => None,
        }
    }

    /// Whether it has landed: an Open once its plate fills the reader and
    /// its print has passed the floor, a Close once its plate is the row.
    fn done(&self, now: Instant, reader: Bounds<Pixels>) -> bool {
        let p = self.carry.value(now);
        match self.verb {
            Verb::Open => {
                p >= 0.94 && self.edge_local(now, f32::from(reader.size.height)).is_none_or(|edge| edge >= f32::from(reader.size.height))
            }
            Verb::Close => p <= 0.04 && now >= self.start + CLOSE_AFTER,
        }
    }
}

/// One frame of a transit, in window space.
#[derive(Clone, Copy, Debug)]
struct Staged {
    verb: Verb,
    p: f32,
    plate: Bounds<Pixels>,
    /// What of the outside page is still uncovered (the second may be empty).
    outside: [Bounds<Pixels>; 2],
    /// The inside page's print (Open) or fold (Close) edge.
    edge: Option<Edge>,
    inside_drift: Pixels,
    outside_drift: Pixels,
    /// Whether the plate's edges still travel (its hairline shows).
    moving: bool,
    has_row: bool,
}

/// The row a Close came back to, tinted periwinkle for a moment.
#[derive(Clone, Debug)]
struct Tint {
    id: SharedString,
    start: Instant,
    reduced: bool,
}

/// How far the outside page drifts (in, to the right: it moves left).
const DRIFT: f32 = 16.0;
/// The print waits for the plate to clear the page's top.
const PRINT_AFTER: Duration = Duration::from_millis(100);
/// One line every 8 ms at the storyboard's 34 px pitch.
const PRINT_SPEED: f32 = 34.0 / 8.0;
/// Each block settles this far as the print enters it, over 32 ms of print.
const PRINT_SETTLE: f32 = 8.0;
/// A Close folds the whole body away in this long before the plate moves.
const FOLD: f32 = 70.0;
/// The plate starts closing once the fold is (nearly) done.
const CLOSE_AFTER: Duration = Duration::from_millis(64);
/// Where you were: the tint holds 180–700 ms, then lets go over 240 ms
/// (reduced motion: holds 1.2 s, settles over 160 ms).
const TINT: (u64, u64, u64) = (180, 700, 240);
const TINT_REDUCED: (u64, u64, u64) = (0, 1_200, 160);
const TINT_ALPHA: f32 = 0.09;

fn lerp(a: Pixels, b: Pixels, t: f32) -> Pixels {
    a + (b - a) * t
}

/// The plate at openness `p`: from the row (spanning the page's column) to
/// the reader, top and sides on one band and the floor on a slightly longer
/// one; without a row, its left edge crosses the reader from the right.
/// Monotone in `p`, so a plate that opens never shrinks and one that
/// closes never grows.
fn plate_at(reader: Bounds<Pixels>, column: (Pixels, Pixels), row: Option<Bounds<Pixels>>, p: f32) -> Bounds<Pixels> {
    let side = band(p, 0.0, 0.9);
    let floor = band(p, 0.0, 0.94);
    let (left, top, right, bottom) = match row {
        Some(row) => (
            lerp(column.0.min(row.left()), reader.left(), side),
            lerp(row.top(), reader.top(), side),
            lerp(column.1.max(row.right()), reader.right(), side),
            lerp(row.bottom(), reader.bottom(), floor),
        ),
        None => (lerp(reader.right(), reader.left(), floor), reader.top(), reader.right(), reader.bottom()),
    };
    Bounds::from_corners(point(left, top), point(right.max(left), bottom.max(top)))
}

/// What of the reader the plate leaves uncovered, as (at most) two bands.
fn uncovered(reader: Bounds<Pixels>, plate: Bounds<Pixels>, has_row: bool) -> [Bounds<Pixels>; 2] {
    let none = Bounds::new(reader.origin, size(Pixels::ZERO, Pixels::ZERO));
    if has_row {
        [
            Bounds::from_corners(reader.origin, point(reader.right(), plate.top().max(reader.top()))),
            Bounds::from_corners(point(reader.left(), plate.bottom().min(reader.bottom())), reader.bottom_right()),
        ]
    } else {
        [Bounds::from_corners(reader.origin, point(plate.left().max(reader.left()), reader.bottom())), none]
    }
}

/// The instant act the page presence keeps (the plate moves pages; the
/// presence only tells the graph's door which place was here).
fn still() -> Act {
    Act {
        duration: Duration::ZERO,
        pose: Keys::owned(Duration::ZERO, vec![(0.0, Pose::REST), (1.0, Pose::REST)], motion::GLIDE),
        room: Keys::owned(Duration::ZERO, vec![(0.0, Extent::FULL), (1.0, Extent::FULL)], motion::GLIDE),
    }
}

/// The reader region.
pub(crate) struct Reader {
    core: RegionCore,
    map: Option<Entity<bodies::graph::Map>>,
    map_presence: Presence,
    graph_arriving: bool,
    graph_source: Option<crate::model::pages::SymbolRef>,
    links: Links,
    pub(crate) targets: Targets,
    hover: HoverIntent,
    scroll: ScrollHandle,
    /// Bring the focused target into view in the next frame's prepaint (a
    /// keyboard walk, or a reflow that may have moved it).
    reveal: Rc<Cell<bool>>,
    /// What the last frame was laid out for: width, height, text scale,
    /// density. A change is a reflow.
    laid_out: Option<(Pixels, Pixels, f32, facet::Density)>,
    lens: Lens,
    route: Route,
    overlay: Option<Overlay>,
    /// Route changes seen (each arrival's key).
    descents: u64,
    last_way: Option<Way>,
    /// The graph door's view of which place is here (the reader's own pages
    /// move by the plate, not by presence).
    pages: Presence,
    places: Vec<Place>,
    /// A place change seen, waiting for the next render to start it.
    arrival: Option<Arrival>,
    /// The place change in flight.
    transit: Option<Transit>,
    /// The row a Close came back to, tinted for a moment.
    tint: Option<Tint>,
    /// The reader's bounds in window space, as last laid out.
    frame: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Every string the last render put on screen, in order.
    said: Vec<SharedString>,
    /// The hero name's lines as the last render set them.
    hero: Vec<SharedString>,
    symbol_disclosures: Vec<(crate::model::pages::SymbolRef, SymbolDisclosure)>,
    package_outline_expanded: bool,
    /// Up to four explicit Find choices survive Compare and Back.
    find_held: Vec<facet::browse::find::HeldPackage>,
    find_workspace: Option<crate::core::LocalProjectId>,
}

impl Reader {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        let snapshot = store.snapshot();
        Self {
            core: RegionCore::new(
                store,
                &[Branch::Route, Branch::Overlay, Branch::Workspace, Branch::Settings],
            ),
            links,
            map: None,
            map_presence: Presence::new("reader.graph"),
            graph_arriving: false,
            graph_source: None,
            targets: Targets::named("reader"),
            hover: HoverIntent::default(),
            scroll: ScrollHandle::new(),
            reveal: Rc::new(Cell::new(false)),
            laid_out: None,
            lens: Lens::Reference,
            route: snapshot.route().clone(),
            overlay: snapshot.overlay(),
            descents: 0,
            last_way: None,
            pages: Presence::new("reader.pages"),
            places: vec![Place {
                key: 0,
                route: snapshot.route().clone(),
                overlay: snapshot.overlay().filter(|overlay| matches!(overlay, Overlay::Settings(_) | Overlay::Inbox)),
                way: Way::Across,
                lens: Lens::Reference,
                from: None,
                opened: None,
            }],
            arrival: None,
            transit: None,
            tint: None,
            frame: Rc::new(Cell::new(None)),
            said: Vec::new(),
            hero: Vec::new(),
            symbol_disclosures: Vec::new(),
            package_outline_expanded: false,
            find_held: Vec::new(),
            find_workspace: snapshot.workspace().host.clone(),
        }
    }

    pub(crate) fn graph_ready(&self, cx: &gpui::App) -> bool {
        self.map.as_ref().is_none_or(|map| map.read(cx).ready(cx))
    }

    pub(crate) fn graph_report(&self, cx: &gpui::App) -> String {
        self.map.as_ref().map_or_else(|| "not mounted".into(), |map| map.read(cx).report(cx))
    }

    #[cfg(feature = "visual-harness")]
    pub(crate) fn graph_state(&self, cx: &gpui::App) -> facet::gallery::json::Json {
        self.map.as_ref().map_or(facet::gallery::json::Json::Null, |map| map.read(cx).inspection(cx))
    }

    pub(crate) fn reset_world(&mut self, cx: &mut Context<Self>) {
        if let Some(map) = &self.map { map.update(cx, bodies::graph::Map::reset_world); }
    }

    pub(crate) fn graph_focus_glyph(&self, cx: &gpui::App) -> Option<gpui::Bounds<Pixels>> {
        self.map.as_ref().and_then(|map| map.read(cx).focus_glyph(cx))
    }

    pub(crate) fn graph_focused(&self, cx: &gpui::App) -> bool {
        self.map.as_ref().is_some_and(|map| map.read(cx).focused(cx))
    }

    pub(crate) fn open_graph_current(&mut self, target: bodies::graph::OpenView, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(map) = &self.map { map.update(cx, |map, cx| map.open_current(target, window, cx)); }
    }

    #[cfg(test)]
    pub(crate) fn graph_entity(&self, cx: &gpui::App) -> Option<Entity<facet::graph::GraphView>> {
        self.map.as_ref().and_then(|map| map.read(cx).graph_entity())
    }

    #[cfg(test)]
    pub(crate) fn graph_canvas_geometry(&self, node: facet::graph::NodeId, cx: &gpui::App) -> (Option<gpui::Bounds<Pixels>>, Option<gpui::Bounds<Pixels>>, gpui::LayerTransform) {
        self.map.as_ref().map_or((None, None, gpui::LayerTransform::IDENTITY), |map| map.read(cx).canvas_geometry(node, cx))
    }

    #[cfg(test)]
    pub(crate) fn graph_gem_morphing(&self, cx: &gpui::App) -> bool {
        self.map.as_ref().is_some_and(|map| map.read(cx).gem_morphing())
    }

    #[cfg(test)]
    pub(crate) fn graph_find_state(&self, window: &Window, cx: &gpui::App) -> (bool, bool) {
        self.map.as_ref().map_or((false, false), |map| map.read(cx).find_state(window, cx))
    }

    #[cfg(test)]
    pub(crate) fn focus_graph_node(&mut self, node: facet::graph::NodeId, cx: &mut Context<Self>) {
        if let Some(map) = &self.map { map.update(cx, |map, cx| map.focus_node(node, cx)); }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }

    /// The strings the last render put on screen.
    pub(crate) fn said(&self) -> &[SharedString] {
        &self.said
    }

    /// The hero name's lines as the last render set them.
    pub(crate) fn hero(&self) -> &[SharedString] {
        &self.hero
    }

    /// Pages drawn by the last render (the current one plus any leaving).
    pub(crate) fn pages_on_screen(&self) -> usize {
        self.places.len()
    }

    /// How many descents played, and which way the last one went.
    pub(crate) const fn descent(&self) -> (u64, Option<Way>) {
        (self.descents, self.last_way)
    }

    pub(crate) fn set_lens(&mut self, lens: Lens, cx: &mut Context<Self>) {
        if self.lens != lens {
            self.lens = lens;
            cx.notify();
        }
    }

    pub(crate) fn symbol_disclosure(&mut self, symbol: &crate::model::pages::SymbolRef) -> SymbolDisclosure {
        if let Some((_, state)) = self.symbol_disclosures.iter().find(|(key, _)| key == symbol) { return state.clone(); }
        let state = SymbolDisclosure::for_symbol(symbol);
        self.symbol_disclosures.push((symbol.clone(), state.clone()));
        if self.symbol_disclosures.len() > 24 { self.symbol_disclosures.remove(0); }
        state
    }

    pub(crate) fn toggle_symbol(&mut self, symbol: crate::model::pages::SymbolRef, fold: SymbolFold, cx: &mut Context<Self>) {
        let mut state = self.symbol_disclosures.iter().position(|(key, _)| key == &symbol)
            .map(|at| self.symbol_disclosures.remove(at).1).unwrap_or_else(|| SymbolDisclosure::for_symbol(&symbol));
        state.toggle(fold);
        self.symbol_disclosures.push((symbol, state));
        if self.symbol_disclosures.len() > 24 { self.symbol_disclosures.remove(0); }
        cx.notify();
    }

    pub(crate) fn toggle_current_symbol(&mut self, fold: SymbolFold, cx: &mut Context<Self>) {
        if let Some(symbol) = route_symbol(&self.route) { self.toggle_symbol(symbol, fold, cx); }
    }

    pub(crate) fn toggle_package_outline(&mut self, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Package(_)) {
            self.package_outline_expanded = !self.package_outline_expanded;
            cx.notify();
        }
    }

    pub(crate) fn set_find_held(&mut self, held: Vec<facet::browse::find::HeldPackage>, cx: &mut Context<Self>) {
        let mut unique: Vec<facet::browse::find::HeldPackage> = Vec::with_capacity(4);
        for package in held {
            if !unique.iter().any(|item| item.key == package.key) { unique.push(package); }
            if unique.len() == 4 { break; }
        }
        if self.find_held != unique {
            self.find_held = unique;
            cx.notify();
        }
    }

    /// Align a tracked section heading with the reading viewport. This uses
    /// real prepaint geometry, so text zoom and open folds need no estimates.
    pub(crate) fn jump_symbol_section(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if let Some(bounds) = self.targets.bounds_of(id) {
            let offset = self.scroll.offset();
            let top = self.scroll.bounds().origin.y + px(24.0);
            self.scroll.set_offset(point(offset.x, (offset.y + top - bounds.origin.y).min(px(0.0))));
            cx.notify();
        }
    }

    /// A link was entered or left: hover intent may prefetch its page.
    pub(crate) fn hover_link(&mut self, key: PageKey, hovered: bool, cx: &mut Context<Self>) {
        let links = self.links.clone();
        self.hover.hover(key, hovered, &links, cx);
    }

    /// Keeps the focused target on screen after a keyboard walk (in the
    /// frame that draws the walk, from that frame's layout).
    pub(crate) fn reveal_focused(&self) {
        self.reveal.set(true);
    }

    fn arrive(&mut self, next: &Route, overlay: Option<Overlay>) {
        if self.route != *next {
            self.package_outline_expanded = false;
        }
        let view_switch = overlay == self.overlay && self.route.same_place(next);
        let way = if view_switch {
            if bodies::graph::is_graph(&self.route) && !bodies::graph::is_graph(next) { Way::GraphPage } else { Way::View }
        } else {
            match (self.route.depth(), next.depth()) {
                (Some(from), Some(to)) if to > from => Way::Down,
                (Some(from), Some(to)) if to < from => Way::Up,
                _ => Way::Across,
            }
        };
        let way = if overlay != self.overlay && self.route == *next { Way::Across } else { way };
        if let Some(current) = self.places.last_mut() {
            current.lens = self.lens;
        }
        let arrival = self.plan(next, overlay, way);
        self.graph_arriving = !bodies::graph::is_graph(&self.route) && bodies::graph::is_graph(next);
        self.graph_source = if self.overlay.is_none() { immediate_page_source(&self.route, next) } else { None };
        let from = (self.route.clone(), self.overlay);
        self.route = next.clone();
        self.overlay = overlay;
        self.descents = self.descents.wrapping_add(1);
        self.last_way = Some(way);
        self.places.push(Place {
            key: self.descents,
            route: next.clone(),
            overlay,
            way,
            lens: Lens::Reference,
            from: Some(from),
            opened: None,
        });
        if !view_switch {
            self.lens = Lens::Reference;
            // Back puts the parent where it was, so its row is where you left it.
            let back = arrival.as_ref().filter(|arrival| arrival.verb == Verb::Close);
            let scroll = back.and_then(|_| self.places.iter().rev().nth(1)).and_then(|leaving| leaving.opened.clone().flatten());
            self.scroll.set_offset(scroll.map_or(point(px(0.0), px(0.0)), |origin| origin.scroll));
        }
        self.arrival = arrival.map(|arrival| Arrival { key: self.descents, ..arrival });
        self.targets.clear_focus();
    }

    /// Which plate move a place change plays, and what it needs from the
    /// page it leaves (read before that page's targets are rebuilt).
    fn plan(&self, next: &Route, overlay: Option<Overlay>, way: Way) -> Option<Arrival> {
        let current = self.places.last()?;
        if matches!(way, Way::View | Way::GraphPage) || bodies::graph::is_graph(&self.route) || bodies::graph::is_graph(next) {
            return None;
        }
        let arrival = |verb, rows, find| Arrival {
            verb,
            key: 0,
            leaving: current.key,
            scroll: self.scroll.offset(),
            rows,
            find,
            focused: self.targets.focused(),
        };
        let leaving = place_keys(&current.route, current.overlay).into_iter().next();
        // Back to the page this one opened from: close into its row (or
        // into whichever row there links here).
        if let Some(opened) = &current.opened
            && current.from.as_ref().is_some_and(|(route, from_overlay)| route == next && *from_overlay == overlay)
        {
            let rows: Vec<_> = opened.iter().map(|origin| (origin.id.clone(), origin.rect)).collect();
            let find = rows.is_empty().then(|| leaving.clone()).flatten();
            return Some(arrival(Verb::Close, rows, find));
        }
        if way == Way::Up {
            return Some(arrival(Verb::Close, Vec::new(), leaving));
        }
        // Into a page the old one links to: the rows that link there.
        let arriving = place_keys(next, overlay).into_iter().next();
        let rows = self
            .targets
            .placed()
            .into_iter()
            .filter(|(target, _)| arriving.is_some() && target.peek == arriving)
            .map(|(target, rect)| (target.id, rect))
            .collect();
        Some(arrival(Verb::Open, rows, None))
    }

    /// Starts the place change `arrival` names, from wherever a change
    /// still in flight is painted (Back mid-open closes the same plate from
    /// its painted openness and velocity).
    fn begin(&mut self, arrival: Arrival, reader: Bounds<Pixels>, window: &Window, cx: &gpui::App) {
        let now = facet::motion::now(cx);
        let live = self.transit.take().filter(|transit| !transit.done(now, reader));
        let route_of = |places: &[Place], key: u64| {
            places.iter().find(|place| place.key == key).map(|place| (place.route.clone(), place.overlay))
        };
        let arriving = route_of(&self.places, arrival.key);
        if facet::motion::reduced(cx) {
            // A cut; the row you came back to still holds the mark.
            if let (Verb::Close, Some((id, _))) = (arrival.verb, arrival.rows.first()) {
                self.tint = Some(Tint { id: id.clone(), start: now, reduced: true });
            }
            return;
        }
        let shown = |rect: &Bounds<Pixels>| {
            rect.size.height > Pixels::ZERO && rect.top() >= reader.top() - px(1.0) && rect.bottom() <= reader.bottom() + px(1.0)
        };
        match arrival.verb {
            Verb::Open => {
                // The row under the pointer, else the one the keyboard stood
                // on, else the first on screen that links here.
                let mouse = window.mouse_position();
                let rows: Vec<&(SharedString, Bounds<Pixels>)> = arrival.rows.iter().filter(|(_, rect)| shown(rect)).collect();
                let row = rows
                    .iter()
                    .find(|(_, rect)| rect.contains(&mouse))
                    .or_else(|| rows.iter().find(|(id, _)| arrival.focused.as_ref() == Some(id)))
                    .or_else(|| rows.first())
                    .map(|(id, rect)| Origin { id: id.clone(), rect: *rect, scroll: arrival.scroll });
                let reversed = live.as_ref().filter(|transit| {
                    transit.verb == Verb::Close && route_of(&self.places, transit.inside) == arriving
                });
                let transit = match reversed {
                    Some(transit) => {
                        let mut carry = transit.carry;
                        carry.retarget(1.0, now);
                        Transit { verb: Verb::Open, inside: arrival.key, find: None, row_id: None, carry, start: now, fold: None, ..transit.clone() }
                    }
                    None => Transit {
                        verb: Verb::Open,
                        inside: arrival.key,
                        outside: arrival.leaving,
                        row: row.as_ref().map(|origin| origin.rect),
                        row_id: None,
                        find: None,
                        carry: Carry::new(0.0, 1.0, now),
                        start: now,
                        scroll: arrival.scroll,
                        fold: None,
                    },
                };
                let opened = match reversed {
                    Some(_) => self.places.iter().find(|place| Some((place.route.clone(), place.overlay)) == arriving && place.opened.is_some())
                        .and_then(|place| place.opened.clone()).unwrap_or(None),
                    None => row,
                };
                if let Some(place) = self.places.iter_mut().find(|place| place.key == arrival.key) {
                    place.opened = Some(opened);
                }
                self.transit = Some(transit);
            }
            Verb::Close => {
                let reversed = live.as_ref().filter(|transit| {
                    transit.verb == Verb::Open && route_of(&self.places, transit.outside) == arriving
                });
                let height = f32::from(reader.size.height);
                let transit = match reversed {
                    Some(transit) => {
                        // Fold what has printed so far, from its edge.
                        let edge = transit.edge_local(now, height).unwrap_or(height).clamp(0.0, height);
                        let mut carry = transit.carry;
                        carry.retarget(0.0, now);
                        Transit { verb: Verb::Close, outside: arrival.key, find: None, row_id: None, carry, start: now, fold: Some((now, edge)), ..transit.clone() }
                    }
                    None => Transit {
                        verb: Verb::Close,
                        inside: arrival.leaving,
                        outside: arrival.key,
                        row: arrival.rows.first().map(|(_, rect)| *rect),
                        row_id: arrival.rows.first().map(|(id, _)| id.clone()),
                        find: arrival.find.clone().filter(|_| arrival.rows.is_empty()),
                        carry: Carry::new(1.0, 0.0, now + CLOSE_AFTER),
                        start: now,
                        scroll: arrival.scroll,
                        fold: Some((now, height)),
                    },
                };
                if let Some((id, _)) = arrival.rows.first() {
                    self.tint = Some(Tint { id: id.clone(), start: now, reduced: false });
                }
                self.transit = Some(transit);
            }
        }
    }

    /// This frame of the change in flight, in window space (and the change
    /// dropped once it has landed).
    fn stage(&mut self, reader: Bounds<Pixels>, column: (Pixels, Pixels), scale: f32, cx: &gpui::App) -> Option<Staged> {
        let now = facet::motion::now(cx);
        if self.transit.as_ref().is_some_and(|transit| transit.done(now, reader)) {
            self.transit = None;
        }
        if facet::motion::reduced(cx) {
            // Reduced motion lands whatever is in flight, at once.
            self.transit = None;
        }
        let transit = self.transit.as_ref()?;
        let p = transit.carry.value(now);
        let plate = plate_at(reader, column, transit.row, p);
        let height = f32::from(reader.size.height);
        let edge = transit.edge_local(now, height).map(|local| {
            let y = reader.top() + px(local);
            match transit.verb {
                Verb::Open => Edge::down(y, px(PRINT_SETTLE * scale), px(PRINT_SPEED * scale * 32.0)),
                Verb::Close => Edge::fold(y),
            }
        });
        let drift = DRIFT * scale;
        Some(Staged {
            verb: transit.verb,
            p,
            plate,
            outside: uncovered(reader, plate, transit.row.is_some()),
            edge,
            inside_drift: px(drift * (1.0 - band(p, 0.5, 0.94))),
            outside_drift: px(-drift * p),
            moving: band(p, 0.04, 0.94) < 1.0 && band(p, 0.04, 0.94) > 0.0,
            has_row: transit.row.is_some(),
        })
    }

    /// A Close's row on the arriving page, followed while the body folds:
    /// the remembered target, else the one that links to the page being
    /// left. Once the plate moves it holds. Runs after the arriving page has
    /// registered its targets (their bounds are the last layout's).
    fn follow(&mut self, reader: Bounds<Pixels>, cx: &gpui::App) {
        let now = facet::motion::now(cx);
        let Some(transit) = self.transit.as_mut().filter(|transit| transit.verb == Verb::Close) else { return };
        if now >= transit.start + CLOSE_AFTER || (transit.row_id.is_none() && transit.find.is_none()) {
            return;
        }
        let shown = |rect: &Bounds<Pixels>| rect.top() >= reader.top() && rect.bottom() <= reader.bottom() && rect.size.height > Pixels::ZERO;
        let found = match (&transit.row_id, &transit.find) {
            (Some(id), _) => self.targets.bounds_of(id).filter(shown).map(|rect| (id.clone(), rect)),
            (None, Some(find)) => self
                .targets
                .placed()
                .into_iter()
                .find(|(target, rect)| target.peek.as_ref() == Some(find) && shown(rect))
                .map(|(target, rect)| (target.id, rect)),
            (None, None) => None,
        };
        transit.row = found.as_ref().map(|(_, rect)| *rect);
        if let Some((id, _)) = found {
            let start = transit.start;
            if self.tint.as_ref().is_none_or(|tint| tint.id != id) {
                self.tint = Some(Tint { id, start, reduced: false });
            }
        }
    }

    /// Lays a body's leaves out: notes beside their block when wide,
    /// under it otherwise.
    fn compose(leaves: Vec<bodies::Leaf>, layout: &Layout, palette: &facet::Palette, edge: Option<Edge>) -> gpui::Div {
        let Layout {
            pad: _,
            top: _,
            folio,
            beside,
            content,
            wide,
            gutter,
            margin,
            measure,
            folio_measure,
        } = *layout;
        let gap = folio_measure.space(Space::Wide);
        let mut column = div().flex().flex_col().gap(gap).w(folio + beside).max_w(content);
        for leaf in leaves {
            // Each block prints under the one edge (reading order, by clip).
            column = column.child(print(edge, match (leaf.note, wide) {
                (Some(note), true) => div()
                    .flex()
                    .items_start()
                    .child(div().w(folio).flex_none().child(leaf.main))
                    .child(div().w(gutter).flex_none())
                    .child(
                        div()
                            .w(margin)
                            .flex_none()
                            .pl(measure.space(Space::Roomy))
                            .border_l_1()
                            .border_color(palette.line1.hsla())
                            .child(note),
                    )
                    .into_any_element(),
                (Some(note), false) => div()
                    .flex()
                    .flex_col()
                    .gap(folio_measure.space(Space::Roomy))
                    .child(leaf.main)
                    .child(
                        div()
                            .flex()
                            .gap(folio_measure.space(Space::Base))
                            .child(
                                facet::icons::ui(facet::icons::Icon::Diamond, facet::icons::IconSize::S12, palette.ink4)
                                    .size(folio_measure.icon(10.0)),
                            )
                            .child(div().flex_1().min_w(px(0.0)).child(note)),
                    )
                    .into_any_element(),
                (None, _) => leaf.main,
            }));
        }
        column
    }
}

/// One page the reader shows (or is still showing on its way out).
#[derive(Clone)]
struct Place {
    key: u64,
    route: Route,
    overlay: Option<Overlay>,
    way: Way,
    lens: Lens,
    /// The place it was reached from.
    from: Option<(Route, Option<Overlay>)>,
    /// Reached by an Open: the row it opened from (`None` inside when it
    /// opened from the right edge). Back closes into it.
    opened: Option<Option<Origin>>,
}

/// The reader's column geometry for one frame.
#[derive(Clone, Copy)]
struct Layout {
    /// The scroller's side padding and top padding.
    pad: Pixels,
    top: Pixels,
    folio: Pixels,
    beside: Pixels,
    content: Pixels,
    wide: bool,
    gutter: Pixels,
    margin: Pixels,
    measure: Measure,
    folio_measure: Measure,
}

fn graph_enter() -> Act {
    let duration = std::time::Duration::from_millis(460);
    Act { duration,
        pose: Keys::owned(duration, vec![(0.0, Pose { opacity: 0.0, ..Pose::REST }), (1.0, Pose::REST)], motion::GLIDE),
        room: Keys::owned(duration, vec![(0.0, Extent::FULL), (1.0, Extent::FULL)], motion::GLIDE) }
}

fn graph_exit() -> Act {
    let duration = std::time::Duration::from_millis(460);
    Act { duration,
        pose: Keys::owned(duration, vec![(0.0, Pose::REST), (1.0, Pose { y: -24.0, opacity: 0.0, ..Pose::REST })], motion::GLIDE),
        room: Keys::owned(duration, vec![(0.0, Extent::FULL), (1.0, Extent::FULL)], motion::GLIDE) }
}

impl Region for Reader {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }

    fn keys(&self, snapshot: &AppSnapshot) -> Vec<PageKey> {
        reader_keys(snapshot)
    }

    fn observe(&mut self, event: &StoreEvent, store: &DataStore) {
        if event.is_branch(Branch::Route) || event.is_branch(Branch::Overlay) {
            let snapshot = store.snapshot();
            let overlay = snapshot.overlay().filter(|overlay| matches!(overlay, Overlay::Settings(_) | Overlay::Inbox));
            if *snapshot.route() != self.route || overlay != self.overlay {
                if overlay == self.overlay && find_refinement(&self.route, snapshot.route()) {
                    // Typing refines one place. Keeping its keyed surface
                    // holds the live input and selection while results reflow.
                    self.route = snapshot.route().clone();
                    if let Some(current) = self.places.last_mut() { current.route = self.route.clone(); }
                    return;
                }
                // (A release change or a view switch replaces the entry; it
                // still arrives as a new page.)
                self.arrive(snapshot.route(), overlay);
            }
        }
    }
}

fn find_refinement(previous: &Route, next: &Route) -> bool {
    let find = |route: &Route| matches!(route,
        Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::FindHome | crate::navigation::BrowseRoute::Find(_))));
    find(previous) && find(next)
}

/// Only an immediately preceding page of this exact typed declaration owns
/// an incoming hero. Code, overlays and another graph have no page endpoint.
fn immediate_page_source(previous: &Route, next: &Route) -> Option<crate::model::pages::SymbolRef> {
    if !matches!(previous, Route::Symbol(crate::navigation::SymbolRoute { view: View::Page, .. }))
        || !bodies::graph::is_graph(next) { return None; }
    let previous = route_symbol(previous)?;
    (route_symbol(next).as_ref() == Some(&previous)).then_some(previous)
}

/// The page keys the reader draws for a snapshot's place.
pub(crate) fn reader_keys(snapshot: &AppSnapshot) -> Vec<PageKey> {
    place_keys(snapshot.route(), snapshot.overlay())
}

impl Reader {
    /// Builds one place's body. The current place registers its targets and
    /// records its words; a leaving place is inert.
    #[allow(clippy::too_many_arguments)]
    fn body(
        &mut self,
        place: &Place,
        current: bool,
        snapshot: &AppSnapshot,
        layout: &Layout,
        facet: &facet::Facet,
        edge: Option<Edge>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let palette = facet.palette();
        let keys = place_keys(&place.route, place.overlay);
        let pages = Pages::gather(self.links.store.read(cx), &keys);
        let links = self.links.clone();
        let targets = if current { self.targets.clone() } else { Targets::default() };
        let mut said = Vec::new();
        let mut hero = Vec::new();
        let mut scratch_hover = HoverIntent::default();
        let mut hover = if current { std::mem::take(&mut self.hover) } else { HoverIntent::default() };
        let leaves = {
            let symbol_disclosure = route_symbol(&place.route).map(|symbol| self.symbol_disclosure(&symbol)).unwrap_or_default();
            let mut ctx = Ctx {
                active: current,
                measure: layout.folio_measure,
                note: if layout.wide { Measure::new(layout.margin, facet) } else { layout.folio_measure },
                palette,
                reveal: facet.reveal,
                links: &links,
                targets: &targets,
                reader_scroll: self.scroll.clone(),
                lens: if current { self.lens } else { place.lens },
                said: &mut said,
                hero: &mut hero,
                symbol_disclosure,
                package_outline_expanded: self.package_outline_expanded,
                find_held: self.find_held.clone(),
            };
            let hover = if current { &mut hover } else { &mut scratch_hover };
            bodies::build(&place.route, place.overlay, snapshot, &pages, &mut ctx, hover, cx)
        };
        if current {
            self.hover = hover;
            self.said = said;
            self.hero = hero;
        }
        Self::compose(leaves, layout, palette, edge)
    }
}

/// The page keys a place draws.
fn place_keys(route: &Route, overlay: Option<Overlay>) -> Vec<PageKey> {
    match overlay {
        Some(Overlay::Settings(_)) => return vec![PageKey::Health],
        Some(Overlay::Inbox) => return Vec::new(),
        _ => {}
    }
    match route {
        Route::Orbit(crate::navigation::OrbitRoute::Browse(browse)) => vec![PageKey::Browse(browse.into())],
        Route::Orbit(_) => vec![PageKey::Orbit, PageKey::Health],
        Route::World => Vec::new(),
        Route::Package(_) => route_package(route).map(PageKey::Package).into_iter().collect(),
        Route::Symbol(symbol) => match symbol.view {
            View::Page | View::Graph => route_symbol(route).map(PageKey::Symbol).into_iter().collect(),
            View::Code => route_symbol(route)
                .map(|id| vec![PageKey::Source(id.clone()), PageKey::Symbol(id)])
                .unwrap_or_default(),
        },
    }
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        let measure = self.core.measure(cx);
        let facet = cx.facet();
        // A reflow moves whatever the keyboard stands on: bring it back
        // into view in the same frame.
        let laid_out = (self.core.width(), window.viewport_size().height, facet.text_scale, facet.density);
        if self.laid_out.is_some_and(|last| last != laid_out) && self.targets.focused().is_some() {
            self.reveal.set(true);
        }
        self.laid_out = Some(laid_out);
        let palette = facet.palette();
        let snapshot = self.links.snapshot(cx);
        if self.find_workspace.as_ref() != snapshot.workspace().host.as_ref() {
            self.find_workspace = snapshot.workspace().host.clone();
            self.find_held.clear();
        }
        if bodies::graph::is_graph(snapshot.route()) && snapshot.overlay().is_none() {
            let map = self.map.get_or_insert_with(|| {
                let links = self.links.clone();
                cx.new(|cx| bodies::graph::Map::new(links, window, cx))
            }).clone();
            let source = self.graph_source.take();
            map.update(cx, |map, cx| map.show(snapshot.route(), source.as_ref(), window, cx));
            self.said = vec!["Graph fixture · pages resolve through your local index".into()];
            self.hero.clear();
            let items = self.map_presence.sync_entries([Entry::new("graph").enter(graph_enter()).exit(graph_exit())], window, cx);
            // The outgoing page lifts over the incoming map, inert. Retarget
            // its exit once; repeated frames never restart a departure.
            if self.graph_arriving {
                if let Some(previous) = self.places.iter().rev().find(|place| !bodies::graph::is_graph(&place.route)) {
                    self.pages.sync_entries([Entry::new(("place", previous.key)).exit(graph_exit())], window, cx);
                }
                self.graph_arriving = false;
            }
            let pages = self.pages.sync_entries([], window, cx);
            let mut root = div().relative().size_full().text_color(palette.ink1.hsla())
                .font_family(facet::fonts::family(ty::BODY))
                .children(items.iter().map(|item| item.slot(div().size_full().child(map.clone()))));
            let pad = measure.fluid(22.0, 40.0);
            let content = (self.core.width() - pad * 2.0).max(px(0.0));
            let folio = px(FOLIO * measure.scale()).min(content);
            let layout = Layout { pad, top: measure.fluid(22.0, 56.0), folio, beside: px(0.0), content, wide: false, gutter: px(0.0),
                margin: px(0.0), measure, folio_measure: Measure::new(folio, &facet) };
            let places = self.places.clone();
            for item in &pages {
                if let Some(place) = places.iter().find(|place| item.key == ElementId::from(("place", place.key))) {
                    let body = self.body(place, false, &snapshot, &layout, &facet, None, cx);
                    root = root.child(div().absolute().top_0().left_0().right_0().bottom_0()
                        .px(pad).pt(measure.fluid(22.0, 56.0)).flex().justify_center()
                        .child(item.slot(body)).occlude());
                }
            }
            let current = self.places.last().map(|place| place.key);
            self.places.retain(|place| Some(place.key) == current || pages.iter().any(|item|
                item.key == ElementId::from(("place", place.key))));
            return root;
        }
        if let Some(map) = &self.map { map.update(cx, |map, cx| map.suspend(window, cx)); }
        let leaving_graph = self.map_presence.sync_entries([], window, cx);
        let width = self.core.width();
        let pad = measure.fluid(22.0, 40.0);
        let content = (width - pad * 2.0).max(px(0.0));
        let scale = measure.scale();
        let notes_possible = matches!(snapshot.route(), Route::Symbol(route) if route.view == View::Code)
            && snapshot.overlay().is_none();
        let wide = measure.room() >= Room::Wide && notes_possible;
        let gutter = px(GUTTER * facet.density.space() * scale);
        let margin = px(MARGIN * scale);
        let beside = if wide { margin + gutter } else { px(0.0) };
        let folio = px(FOLIO * scale).min((content - beside).max(px(0.0)));
        let top = measure.fluid(22.0, 56.0);
        let layout = Layout {
            pad,
            top,
            folio,
            beside,
            content,
            wide,
            gutter,
            margin,
            measure,
            folio_measure: Measure::new(folio, &facet),
        };

        let Some(current) = self.places.last().cloned() else {
            return div();
        };
        // The graph's door reads which place is here; the plate moves pages.
        self.pages.sync_entries([Entry::new(("place", current.key)).enter(still()).exit(still())], window, cx);
        // The place change in flight, this frame (window space).
        let reader = self.frame.get();
        if let Some(arrival) = self.arrival.take()
            && let Some(reader) = reader
        {
            self.begin(arrival, reader, window, cx);
        }
        let column = reader.map(|reader| {
            let used = (folio + beside).min(content);
            let left = reader.left() + pad + (content - used) / 2.0;
            (left, left + used)
        });
        let staged = reader.zip(column).and_then(|(reader, column)| self.stage(reader, column, scale, cx));
        if staged.is_some() || self.tint.is_some() {
            facet::motion::request_frame(window, cx);
        }
        let transit = self.transit.clone().filter(|_| staged.is_some());
        let keep = |key: u64| key == current.key || transit.as_ref().is_some_and(|t| t.inside == key || t.outside == key);
        self.places.retain(|place| keep(place.key));
        let leaving = transit.as_ref().and_then(|transit| {
            let key = if transit.verb == Verb::Open { transit.outside } else { transit.inside };
            self.places.iter().find(|place| place.key == key).cloned()
        });

        // The current page, in the scroller: inside the plate when it opens,
        // outside it (above) when the plate closes over it.
        let current_edge = staged.filter(|staged| staged.verb == Verb::Open).and_then(|staged| staged.edge);
        let body = self.body(&current, true, &snapshot, &layout, &facet, current_edge, cx);
        if let Some(reader) = reader {
            self.follow(reader, cx);
        }
        let drift = staged.map_or(Pixels::ZERO, |staged| match staged.verb {
            Verb::Open => staged.inside_drift,
            Verb::Close => staged.outside_drift,
        });
        let stack = div()
            .relative()
            .w_full()
            .flex()
            .justify_center()
            .child(offset(div().id(("place", current.key)).child(body)).x(drift));

        let glow = self.targets.glow(&measure);
        let scroller = div()
            .id("reader-scroll")
            .debug_selector(|| "reader-scroll".to_owned())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(div().w_full().px(pad).pt(top).pb(px(96.0 * scale)).child(stack));
        let scroller = Reveal {
            pending: Rc::clone(&self.reveal),
            targets: self.targets.clone(),
            scroll: self.scroll.clone(),
            frame: Rc::clone(&self.frame),
            child: scroller.into_any_element(),
        };
        let local = |bounds: Bounds<Pixels>| {
            let origin = reader.map_or(Point::default(), |reader| reader.origin);
            Bounds::new(bounds.origin - origin, bounds.size)
        };
        // A page drawn away from the scroller, where it was on screen:
        // inert, cut to `mask`, drifted by `drift`.
        let mut still_page = |this: &mut Self, place: &Place, scroll: Point<Pixels>, mask: Bounds<Pixels>, edge: Option<Edge>, drift: Pixels, cx: &mut Context<Self>| {
            let body = this.body(place, false, &snapshot, &layout, &facet, edge, cx);
            div().absolute().top_0().left_0().right_0().bottom_0().child(masked(
                mask,
                offset(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(scroll.y)
                        .px(pad)
                        .pt(top)
                        .child(div().relative().w_full().flex().justify_center().child(body)),
                )
                .x(drift),
            ))
        };
        let mut root = div().relative().size_full();
        // Where you were: the row a Close came back to, tinted under the page.
        let tint = self.tint_now(cx);
        self.publish(staged, tint.map(|(_, strength)| strength), cx);
        if let Some(tint) = tint {
            root = root.child(
                div()
                    .absolute()
                    .left(local(tint.0).origin.x)
                    .top(local(tint.0).origin.y)
                    .w(tint.0.size.width)
                    .h(tint.0.size.height)
                    .bg(gpui::Hsla { alpha: TINT_ALPHA * tint.1, ..palette.peri.base.hsla() }),
            );
        }
        let plate_ground = |staged: &Staged| {
            let plate = local(staged.plate);
            let hairline = palette.peri.line.hsla();
            let mut ground = div()
                .absolute()
                .left(plate.origin.x)
                .top(plate.origin.y)
                .w(plate.size.width)
                .h(plate.size.height)
                .bg(palette.g1.hsla());
            if staged.moving {
                ground = if staged.has_row { ground.border_t_1().border_b_1() } else { ground.border_l_1() }.border_color(hairline);
            }
            ground
        };
        match (staged, transit.as_ref(), leaving) {
            (Some(staged), Some(transit), Some(leaving)) if staged.verb == Verb::Open => {
                // The old page stays where it was, drifting left, cut to what
                // the plate has not reached; the new page is on the plate.
                for (index, mask) in staged.outside.into_iter().enumerate() {
                    if mask.size.height > Pixels::ZERO && mask.size.width > Pixels::ZERO {
                        let page = still_page(self, &leaving, transit.scroll, mask, None, staged.outside_drift, cx);
                        root = root.child(div().id(("leaving", index)).size_full().absolute().child(page));
                    }
                }
                root = root.child(plate_ground(&staged)).child(masked(staged.plate, scroller));
            }
            (Some(staged), Some(transit), Some(leaving)) => {
                // The parent is uncovered around the closing plate; the page
                // it came back from folds on the plate, then the plate shuts.
                let [above, below] = staged.outside;
                root = root.child(masked(above, scroller));
                if below.size.height > Pixels::ZERO && below.size.width > Pixels::ZERO {
                    let page = still_page(self, &current, self.scroll.offset(), below, None, staged.outside_drift, cx);
                    root = root.child(div().id("parent-below").size_full().absolute().child(page));
                }
                let page = still_page(self, &leaving, transit.scroll, staged.plate, staged.edge, staged.inside_drift, cx);
                root = root.child(plate_ground(&staged)).child(div().id("leaving-plate").size_full().absolute().child(page));
            }
            _ => root = root.child(scroller),
        }
        root.children(self.map.as_ref().into_iter().flat_map(|map| leaving_graph.iter().map(move |item| {
                div().absolute().top_0().left_0().right_0().bottom_0()
                    .child(item.slot(div().size_full().child(map.clone()))).occlude()
            })))
            .child(super::kit::scroll_probe("reader-scroll", self.scroll.clone()))
            .child(glow)
            .text_color(palette.ink1.hsla())
            .font_family(facet::fonts::family(ty::BODY))
    }
}

impl Reader {
    /// The tinted row's bounds and strength this frame (and the tint
    /// dropped once it has let go).
    fn tint_now(&mut self, cx: &gpui::App) -> Option<(Bounds<Pixels>, f32)> {
        let tint = self.tint.as_ref()?;
        let now = facet::motion::now(cx);
        let (from, hold, release) = if tint.reduced { TINT_REDUCED } else { TINT };
        #[allow(clippy::cast_precision_loss)]
        let ms = now.saturating_duration_since(tint.start).as_millis() as f32;
        #[allow(clippy::cast_precision_loss)]
        let (from, hold, release) = (from as f32, hold as f32, release as f32);
        if ms >= hold + release {
            self.tint = None;
            return None;
        }
        let strength = if ms < from { 0.0 } else if ms < hold { 1.0 } else { 1.0 - (ms - hold) / release };
        let rect = self.targets.bounds_of(&tint.id)?;
        Some((rect, strength))
    }

    /// Publishes the change to the probe ledger: its driver
    /// (`reader.carry`), its plate's edges in window space
    /// (`reader.plate.{left,top,right,bottom}`) and the where-you-were tint
    /// (`reader.tint.<target>`, its strength).
    fn publish(&self, staged: Option<Staged>, tint: Option<f32>, cx: &mut gpui::App) {
        if !facet::probe::enabled(cx) {
            return;
        }
        let now = facet::motion::now(cx);
        let epoch = facet::motion::epoch(cx);
        let millis = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
        let at_ms = millis(now);
        let track = |key: String, value: f32, target: f32, velocity: f32, started_ms: f64, budget_ms: f64| facet::probe::TrackSample {
            key,
            kind: facet::probe::TrackKind::Spring,
            value,
            target,
            velocity,
            started_ms,
            budget_ms,
            at_ms,
            live: true,
            overshoot_ratio: 0.0,
            overshoot_absolute: 0.0,
            group: Some("reader.transit".to_owned()),
        };
        if let (Some(transit), Some(staged)) = (&self.transit, staged) {
            let (value, velocity) = transit.carry.sample(now);
            let started = millis(transit.carry.start());
            let budget = transit.carry.budget(0.001).as_secs_f64() * 1000.0;
            let mut samples = vec![track("reader.carry".to_owned(), value, transit.carry.target(), velocity, started, budget)];
            let plate = staged.plate;
            for (edge, at) in [("left", plate.left()), ("top", plate.top()), ("right", plate.right()), ("bottom", plate.bottom())] {
                samples.push(track(format!("reader.plate.{edge}"), f32::from(at), f32::from(at), 0.0, started, budget));
            }
            for sample in samples {
                facet::probe::record_track(cx, || sample);
            }
        }
        if let (Some(tint), Some(strength)) = (&self.tint, tint) {
            let sample = track(format!("reader.tint.{}", tint.id), strength, 0.0, 0.0, millis(tint.start), 0.0);
            facet::probe::record_track(cx, || sample);
        }
    }
}

/// The reader's scroll container, bringing the focused target into view
/// when asked. It runs before the container applies its offset, from this
/// frame's layout, so the frame that walks or reflows already shows the
/// target (the storm's seed 3 saw focus left off a 320 px window after the
/// text grew).
struct Reveal {
    pending: Rc<Cell<bool>>,
    targets: Targets,
    scroll: ScrollHandle,
    /// Where the reader is laid out, in window space, for the next frame's
    /// plate.
    frame: Rc<Cell<Option<Bounds<Pixels>>>>,
    child: gpui::AnyElement,
}

impl IntoElement for Reveal {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for Reveal {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        view: gpui::Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        self.frame.set(Some(window.layer_transform().apply_bounds(view)));
        if self.pending.take()
            && let Some(layout) = self.targets.focused_layout()
        {
            // The target's place in the content, as if unscrolled.
            let target = window.layout_bounds(layout);
            let offset = self.scroll.offset();
            let margin = px(48.0).min(view.size.height / 4.0);
            let top = target.origin.y + offset.y;
            let bottom = top + target.size.height;
            let lowest = view.origin.y + view.size.height - margin;
            let highest = view.origin.y + margin;
            let y = if bottom > lowest {
                offset.y - (bottom - lowest)
            } else if top < highest {
                (offset.y + (highest - top)).min(px(0.0))
            } else {
                offset.y
            };
            if y != offset.y {
                self.scroll.set_offset(point(offset.x, y));
            }
        }
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: gpui::Bounds<Pixels>,
        _state: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        self.child.paint(window, cx);
    }
}

/// Open and Close through the real shell, frame by frame (W-Motion PLAN §2a;
/// the brief's T1 tests). The plate, the driver and the tint come from the
/// probe ledger; every text line the window paints, from the text trace.
#[cfg(test)]
mod transit_tests {
    use super::CLOSE_AFTER;
    use crate::navigation::Route;
    use crate::shell::tests::{PACKAGE, Rig, rig};
    use gpui::{Bounds, PaintedText, Pixels, TestAppContext, point, px, size};
    use std::collections::BTreeSet;
    use std::time::Duration;

    /// One drawn frame: the plate (if a change is in flight), the driver, the
    /// tints, the targets and every painted text line.
    #[derive(Debug)]
    struct Shot {
        at: u64,
        plate: Option<Bounds<Pixels>>,
        p: Option<f32>,
        tint: Vec<(String, f32)>,
        targets: Vec<(String, Bounds<Pixels>)>,
        texts: Vec<PaintedText>,
    }

    fn package() -> Route {
        Route::Package(crate::navigation::PackageRoute {
            project: None,
            at: None,
            package: crate::core::PackageId::new(PACKAGE).expect("package"),
            lane: crate::navigation::PackageLane::Overview,
            selected: None,
        })
    }

    fn bounds(sample: &facet::probe::BoundsSample) -> Bounds<Pixels> {
        Bounds::new(point(px(sample.x), px(sample.y)), size(px(sample.width), px(sample.height)))
    }

    /// Draws the frame the platform would draw `ms` from now.
    fn shoot(rig: &mut Rig, at: u64, ms: u64) -> Shot {
        if ms > 0 {
            rig.cx.executor().advance_clock(Duration::from_millis(ms));
        }
        rig.cx.run_until_parked();
        let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
        rig.cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let (ledger, texts) = rig.cx.update(|window, cx| (facet::probe::take(cx), window.painted_texts().to_vec()));
        let track = |key: &str| ledger.tracks.iter().rev().find(|track| track.key == key).map(|track| track.value);
        let plate = match (track("reader.plate.left"), track("reader.plate.top"), track("reader.plate.right"), track("reader.plate.bottom")) {
            (Some(l), Some(t), Some(r), Some(b)) => Some(Bounds::from_corners(point(px(l), px(t)), point(px(r), px(b)))),
            _ => None,
        };
        Shot {
            at,
            plate,
            p: track("reader.carry"),
            tint: ledger
                .tracks
                .iter()
                .filter_map(|track| track.key.strip_prefix("reader.tint.").map(|id| (id.to_owned(), track.value)))
                .collect(),
            targets: ledger.targets.iter().map(|target| (target.key.clone(), bounds(&target.bounds))).collect(),
            texts,
        }
    }

    /// Films a change from its first frame (the input's own frame) every
    /// 16 ms to `until` ms.
    fn film(rig: &mut Rig, act: impl FnOnce(&mut Rig), until: u64) -> Vec<Shot> {
        act(rig);
        let mut shots = vec![shoot(rig, 0, 0)];
        let mut at = 0;
        while at < until {
            at += 16;
            shots.push(shoot(rig, at, 16));
        }
        shots
    }

    fn start(cx: &mut TestAppContext) -> Rig {
        let mut rig = rig(cx, Some(package()), 1440.0, 900.0);
        rig.cx.update(|_, cx| {
            facet::probe::enable(cx);
            cx.set_global(gpui::TextTrace);
        });
        rig
    }

    fn inside(rect: Bounds<Pixels>, of: Bounds<Pixels>) -> bool {
        rect.left() >= of.left() - px(0.5)
            && rect.top() >= of.top() - px(0.5)
            && rect.right() <= of.right() + px(0.5)
            && rect.bottom() <= of.bottom() + px(0.5)
    }

    fn crosses(rect: Bounds<Pixels>, of: Bounds<Pixels>) -> bool {
        let cut = rect.intersect(&of);
        cut.size.width > px(1.0) && cut.size.height > px(1.0)
    }

    /// Words only one page says.
    fn only(a: &[String], b: &[String]) -> BTreeSet<String> {
        let b: BTreeSet<&String> = b.iter().collect();
        a.iter().filter(|line| !b.contains(line) && line.chars().count() > 2).cloned().collect()
    }

    /// Enter on a row of the package page: the row's plate opens. Frame 0's
    /// plate is exactly the row's band; the plate only ever grows; it fills
    /// the reader by 240 ms (p ≥ .97); no line only the package page says is
    /// ever painted inside it, and no painted line straddles its edge (each
    /// page is cut to its own side, so nothing lies on anything).
    #[gpui::test]
    fn a_row_opens_into_its_page_on_one_growing_plate(cx: &mut TestAppContext) {
        let mut rig = start(cx);
        rig.keys("j");
        let old = rig.said();
        let before = shoot(&mut rig, 0, 0);
        let (_, focused) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        let row_id = focused.expect("j focuses a row").to_string();
        let row = before.targets.iter().find(|(key, _)| *key == row_id).map(|(_, at)| *at).expect("the row is drawn");
        let shots = film(&mut rig, |rig| rig.cx.simulate_keystrokes("enter"), 400);
        assert!(matches!(rig.route(), Route::Symbol(_)), "enter opened a page");
        rig.settle();
        let new = rig.said();
        let old_only = only(&old, &new);
        assert!(!old_only.is_empty(), "the pages differ: {old:?}");

        let first = shots[0].plate.expect("frame 0 has a plate");
        assert_eq!((first.top(), first.bottom()), (row.top(), row.bottom()), "frame 0's plate is the row's band: {first:?} vs {row:?}");
        assert!(first.left() <= row.left() && first.right() >= row.right(), "it spans the row: {first:?} vs {row:?}");
        let mut last = first;
        for shot in &shots {
            let Some(plate) = shot.plate else { continue };
            assert!(inside(last, plate), "the plate never shrinks: {last:?} then {plate:?} at {} ms", shot.at);
            last = plate;
            for text in &shot.texts {
                let content = text.text.to_string();
                if crosses(text.bounds, plate) {
                    assert!(
                        !old_only.contains(&content),
                        "`{content}` (only the package page says it) is painted inside the plate at {} ms: {:?} in {plate:?}",
                        shot.at,
                        text.bounds
                    );
                    assert!(
                        inside(text.bounds, plate),
                        "`{content}` straddles the plate's edge at {} ms: {:?} vs {plate:?}",
                        shot.at,
                        text.bounds
                    );
                }
            }
        }
        let landed = shots.iter().find(|shot| shot.at == 240).and_then(|shot| shot.p).expect("the driver at 240 ms");
        assert!(landed >= 0.97, "the plate lands by 240 ms: p = {landed:.3}");
        let early = shots.iter().find(|shot| shot.at == 80).and_then(|shot| shot.p).expect("the driver at 80 ms");
        assert!((early - 0.54).abs() < 0.02, "CARRY: p(80 ms) = {early:.3}");
        assert!(shots.last().is_some_and(|shot| shot.plate.is_none()), "the change is over by 400 ms");
    }

    /// ⌘[ back to the package page: the body folds, then the plate closes
    /// into the row it opened from (monotone, landing on the row's band),
    /// no line only the page says is ever outside the plate, and the row
    /// holds a periwinkle tint from 180 to 700 ms that lets go by 940 ms.
    #[gpui::test]
    fn back_closes_into_the_row_it_came_from_and_marks_it(cx: &mut TestAppContext) {
        let mut rig = start(cx);
        rig.keys("j");
        let (_, focused) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        let row_id = focused.expect("j focuses a row").to_string();
        let parent = rig.said();
        rig.keys("enter");
        let page = rig.said();
        let page_only = only(&page, &parent);
        let shots = film(&mut rig, |rig| rig.cx.simulate_keystrokes("cmd-["), 1000);
        assert!(matches!(rig.route(), Route::Package(_)), "back came home");
        let row = shots
            .last()
            .and_then(|shot| shot.targets.iter().find(|(key, _)| *key == row_id).map(|(_, at)| *at))
            .expect("the row is drawn again");
        let mut last: Option<Bounds<Pixels>> = None;
        for shot in &shots {
            let Some(plate) = shot.plate else { continue };
            if let Some(last) = last {
                assert!(inside(plate, last), "the plate never grows: {last:?} then {plate:?} at {} ms", shot.at);
            }
            last = Some(plate);
            for text in &shot.texts {
                let content = text.text.to_string();
                if page_only.contains(&content) && text.bounds.size.height > px(1.0) {
                    assert!(inside(text.bounds, plate), "`{content}` (the page's) is painted outside the plate at {} ms: {:?} vs {plate:?}", shot.at, text.bounds);
                }
            }
        }
        let closing = shots.iter().filter(|shot| shot.plate.is_some()).last().and_then(|shot| shot.plate).expect("a plate");
        assert!(
            (closing.top() - row.top()).abs() < px(3.0) && (closing.bottom() - row.bottom()).abs() < px(3.0),
            "the plate closes onto the row's band: {closing:?} vs {row:?}"
        );
        // The fold comes first: the plate holds until CLOSE_AFTER.
        let held = shots.iter().filter(|shot| shot.at < CLOSE_AFTER.as_millis() as u64).filter_map(|shot| shot.p);
        for p in held {
            assert!((p - 1.0).abs() < 1e-4, "the plate waits for the fold: p = {p}");
        }
        let tint = |at: u64| {
            shots.iter().find(|shot| shot.at == at).and_then(|shot| shot.tint.iter().find(|(id, _)| *id == row_id)).map(|(_, v)| *v)
        };
        assert_eq!(tint(160), Some(0.0), "no tint before 180 ms");
        assert_eq!(tint(400), Some(1.0), "the row holds the tint at 400 ms");
        assert!(tint(800).is_some_and(|v| v > 0.0 && v < 1.0), "it lets go after 700 ms: {:?}", tint(800));
        assert_eq!(tint(960), None, "gone by 940 ms");
    }

    /// Back at 120 ms, mid-open: the one driver turns around from its
    /// painted value (no jump, it keeps its momentum for a moment) and the
    /// plate closes from where it was.
    #[gpui::test]
    fn back_mid_open_retargets_the_same_plate(cx: &mut TestAppContext) {
        let mut rig = start(cx);
        rig.keys("j");
        let mut shots = film(&mut rig, |rig| rig.cx.simulate_keystrokes("enter"), 112);
        let before = shots.last().and_then(|shot| shot.p).expect("opening");
        assert!(before > 0.6 && before < 0.8, "p(112) = {before}");
        rig.cx.simulate_keystrokes("cmd-[");
        shots.push(shoot(&mut rig, 128, 16));
        let turned = shots.last().and_then(|shot| shot.p).expect("the same driver");
        assert!((turned - before).abs() < 0.12, "no jump: {before:.3} then {turned:.3}");
        let mut last = turned;
        let mut at = 128;
        while at < 600 {
            at += 16;
            let shot = shoot(&mut rig, at, 16);
            let Some(p) = shot.p else { break };
            assert!(p <= last + 0.02, "it heads home: {last:.3} then {p:.3} at {at} ms");
            last = p;
        }
        assert!(last < 0.1, "it closed: p = {last}");
        assert!(matches!(rig.route(), Route::Package(_)));
    }
}
