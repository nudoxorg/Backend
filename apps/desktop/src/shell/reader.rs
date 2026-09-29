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
//!
//! The graph is the same space zoomed out. Going to it, the page **folds**:
//! its body folds up with the graph not yet drawn, then its empty plate
//! closes into the focused node (followed as the camera moves it) while the
//! graph is uncovered around it, and the hero gem travels into the node.
//! Coming back, the node **unfolds**: its plate opens over the graph, the
//! node's gem grows into the hero, and the page prints once the plate
//! covers the reader (the graph is not drawn under a covered plate). With
//! no node (the world), the plate leaves and enters through the right edge.

use super::bodies::{self, Ctx, Lens, Pages};
use super::focus::Targets;
use super::kit::HoverIntent;
use super::region::{Links, Region, RegionCore};
use super::jump::{route_package, route_symbol};
use crate::model::AppSnapshot;
use crate::model::pages::PageKey;
use crate::navigation::{Overlay, Route, View};
use crate::runtime::store::{Branch, DataStore, StoreEvent};
use facet::motion::{Carry, Edge, Presence, band, masked, offset, print};
use facet::tokens::ty;
use facet::tokens::fluid::{NOTES, Notes, READER_PAD, READER_TOP, WIDE_FOLIO};
use facet::{ActiveFacet as _, Measure, Space};
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
    /// A fold of the simple symbol page.
    Page(facet::anatomy::symbol::key::FoldKey),
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
    /// The simple page's own state: the list's filters.
    pub(crate) ui: facet::anatomy::symbol::Ui,
    /// What the page remembers between frames (where its sections are, which
    /// side of the rail's breakpoint it drew).
    pub(crate) spots: Rc<facet::anatomy::symbol::Spots>,
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
    /// Page → graph: the page folds, then its plate closes into its node
    /// (the graph was always behind it) while the gem becomes the node.
    Fold,
    /// Graph → page: the node's plate opens into the page, the node becomes
    /// the gem, and the page prints once the plate covers the graph.
    Unfold,
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
    /// `print_after`; Close folds up from `(when, reader-local y)`.
    fold: Option<(Instant, f32)>,
    /// When an Open's or an Unfold's page starts printing.
    print_after: Duration,
    /// Fold / Unfold: the gem's kind and the end of its travel that holds
    /// still (Fold: the page's hero gem; Unfold: the node).
    gem: Option<(facet::icons::Kind, Bounds<Pixels>)>,
    /// Unfold: the arriving declaration, whose own gem is held at rest.
    symbol: Option<crate::model::pages::SymbolRef>,
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
            (Verb::Open | Verb::Unfold, None) => {
                let started = self.start + self.print_after;
                let ms = if now >= started { since(started) } else { -(started.saturating_duration_since(now).as_secs_f32() * 1000.0) };
                Some(PRINT_SPEED * ms)
            }
            (Verb::Close | Verb::Fold, None) => None,
        }
    }

    /// Whether it has landed: an Open once its plate fills the reader and
    /// its print has passed the floor, a Close once its plate is the row.
    fn done(&self, now: Instant, reader: Bounds<Pixels>) -> bool {
        let p = self.carry.value(now);
        match self.verb {
            Verb::Open | Verb::Unfold => {
                p >= 0.94 && self.edge_local(now, f32::from(reader.size.height)).is_none_or(|edge| edge >= f32::from(reader.size.height))
            }
            Verb::Close | Verb::Fold => p <= LANDED && now >= self.start + CLOSE_AFTER,
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
    /// Whether the plate covers the whole reader.
    covered: bool,
    /// Fold / Unfold: the travelling gem, this frame.
    gem: Option<(facet::icons::Kind, Bounds<Pixels>)>,
}

/// The row a Close came back to, tinted periwinkle for a moment.
#[derive(Clone, Debug)]
struct Tint {
    id: SharedString,
    start: Instant,
    reduced: bool,
}

/// Below this openness the plate is the row exactly (a Close has landed).
const LANDED: f32 = 0.02;
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
/// An Unfold from a node prints once CARRY has the plate over the whole
/// reader (p = .94 at 196 ms from rest); from the edge, as an Open does.
const UNFOLD_PRINT_AFTER: Duration = Duration::from_millis(196);
/// Where you were: the tint holds 180–700 ms, then lets go over 240 ms
/// (reduced motion: holds 1.2 s, settles over 160 ms).
const TINT: (u64, u64, u64) = (180, 700, 240);
const TINT_REDUCED: (u64, u64, u64) = (0, 1_200, 160);
const TINT_ALPHA: f32 = 0.09;

fn lerp(a: Pixels, b: Pixels, t: f32) -> Pixels {
    a + (b - a) * t
}

fn lerp_rect(a: Bounds<Pixels>, b: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    Bounds::from_corners(
        point(lerp(a.left(), b.left(), t), lerp(a.top(), b.top(), t)),
        point(lerp(a.right(), b.right(), t), lerp(a.bottom(), b.bottom(), t)),
    )
}

/// The plate at openness `p`: from the row (spanning the page's column) to
/// the reader, top and sides on one band and the floor on a slightly longer
/// one; without a row, its left edge crosses the reader from the right.
/// Monotone in `p`, so a plate that opens never shrinks and one that
/// closes never grows.
fn plate_at(reader: Bounds<Pixels>, column: (Pixels, Pixels), row: Option<Bounds<Pixels>>, p: f32) -> Bounds<Pixels> {
    let side = band(p, LANDED, 0.9);
    let floor = band(p, LANDED, 0.94);
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

/// The reader region.
pub(crate) struct Reader {
    core: RegionCore,
    map: Option<Entity<bodies::graph::Map>>,
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
    places: Vec<Place>,
    /// The Library's ring of names: its own flow, so a name that wraps to
    /// another line glides there (one per reader, not one per app).
    ring_flow: facet::motion::Flow,
    /// The key of the place the last frame drew as the current page: what a
    /// page change leaves. A place no frame drew (a route another one
    /// superseded in the same instant) was never on screen.
    painted: Option<u64>,
    /// A place change seen, waiting for the next render to start it.
    arrival: Option<Arrival>,
    /// The place change in flight.
    transit: Option<Transit>,
    /// The row a Close came back to, tinted for a moment.
    tint: Option<Tint>,
    /// The reader's bounds in window space, as last laid out.
    frame: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The shared gem an Unfold holds at rest (released when it lands).
    held: Option<ElementId>,
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
            painted: None,
            ring_flow: facet::motion::Flow::new("orbit-ring"),
            places: vec![Place {
                key: 0,
                route: snapshot.route().clone(),
                overlay: snapshot.overlay().filter(|overlay| matches!(overlay, Overlay::Settings(_) | Overlay::Inbox)),
                way: Way::Across,
                lens: Lens::Reference,
                from: None,
                opened: None,
                hop: false,
            }],
            arrival: None,
            transit: None,
            tint: None,
            frame: Rc::new(Cell::new(None)),
            held: None,
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

    /// Applies a change to the simple page's state (its filters).
    pub(crate) fn change_symbol(&mut self, symbol: crate::model::pages::SymbolRef, change: &facet::anatomy::symbol::Change, cx: &mut Context<Self>) {
        let mut state = self.symbol_disclosures.iter().position(|(key, _)| key == &symbol)
            .map(|at| self.symbol_disclosures.remove(at).1).unwrap_or_else(|| SymbolDisclosure::for_symbol(&symbol));
        state.ui = state.ui.clone().apply(change);
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
        // A place no frame drew was never on screen, so it is not what this
        // change leaves: three routes in one instant (A, B, C) are one change
        // from A to C, not a skeleton for B leaving and one for C arriving.
        // Drop each unpainted place (the first one, before any frame, stays:
        // there is nothing older) and arrive from the last one painted.
        let collapsed = drop_unpainted(&mut self.places, self.painted);
        if collapsed && let Some(last) = self.places.last() {
            self.route = last.route.clone();
            self.overlay = last.overlay;
        }
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
        if !collapsed && let Some(current) = self.places.last_mut() {
            current.lens = self.lens;
        }
        let arrival = self.plan(next, overlay, way);
        let hop_forward = arrival.as_ref().is_some_and(|arrival| arrival.verb == Verb::Open);
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
            hop: hop_forward,
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
        let arrival = |verb, rows, find| Arrival {
            verb,
            key: 0,
            leaving: current.key,
            scroll: self.scroll.offset(),
            rows,
            find,
            focused: self.targets.focused(),
        };
        // The graph is the same space zoomed out: a page folds into its
        // node, and a node unfolds into its page.
        let (from_graph, to_graph) = (
            bodies::graph::is_graph(&self.route) && self.overlay.is_none(),
            bodies::graph::is_graph(next) && overlay.is_none(),
        );
        match (from_graph, to_graph) {
            (false, true) => return Some(arrival(Verb::Fold, Vec::new(), None)),
            (true, false) => return Some(arrival(Verb::Unfold, Vec::new(), None)),
            (true, true) => return None,
            (false, false) => {}
        }
        if matches!(way, Way::View | Way::GraphPage) {
            return None;
        }
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
                // An Open still in flight from the same page goes on to the newer
                // route: the plate keeps opening and the page on it is the new one.
                let retargeted = live.as_ref().filter(|transit| transit.verb == Verb::Open && transit.outside == arrival.leaving);
                let transit = match (reversed, retargeted) {
                    (Some(transit), _) => {
                        let mut carry = transit.carry;
                        carry.retarget(1.0, now);
                        Transit { verb: Verb::Open, inside: arrival.key, find: None, row_id: None, carry, start: now, fold: None, print_after: PRINT_AFTER, ..transit.clone() }
                    }
                    (None, Some(transit)) => Transit { inside: arrival.key, ..transit.clone() },
                    (None, None) => Transit {
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
                        print_after: PRINT_AFTER,
                        gem: None,
                        symbol: None,
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
                        print_after: PRINT_AFTER,
                        gem: None,
                        symbol: None,
                    },
                };
                if let Some((id, _)) = arrival.rows.first() {
                    self.tint = Some(Tint { id: id.clone(), start: now, reduced: false });
                }
                self.transit = Some(transit);
            }
            Verb::Fold => {
                // The page folds on its plate; the plate then closes into the
                // node (followed as the camera moves it), or, with no node
                // (the world), out through the right edge. The hero gem
                // travels into the node from where it was painted.
                let height = f32::from(reader.size.height);
                let symbol = self.places.iter().find(|place| place.key == arrival.leaving).and_then(|place| route_symbol(&place.route));
                let gem = symbol.as_ref().and_then(|symbol| {
                    let rect = facet::motion::shared::last_bounds(crate::shell::kit::shared_id(symbol), window, cx)?;
                    Some((self.kind_of(symbol, cx)?, rect))
                });
                self.transit = Some(Transit {
                    verb: Verb::Fold,
                    inside: arrival.leaving,
                    outside: arrival.key,
                    row: self.graph_focus_glyph(cx).filter(|node| shown(node)),
                    row_id: None,
                    find: None,
                    carry: Carry::new(1.0, 0.0, now + CLOSE_AFTER),
                    start: now,
                    scroll: arrival.scroll,
                    fold: Some((now, height)),
                    print_after: PRINT_AFTER,
                    gem,
                    symbol: None,
                });
            }
            Verb::Unfold => {
                // The node's plate opens into the page; the node becomes the
                // gem. With no node (the world), the page enters from the
                // right edge as an Open does.
                let node = self.graph_focus_glyph(cx).filter(|node| shown(node));
                let symbol = arriving.as_ref().and_then(|(route, _)| route_symbol(route));
                let gem = node.zip(symbol.as_ref()).and_then(|(node, symbol)| Some((self.kind_of(symbol, cx)?, node)));
                self.transit = Some(Transit {
                    verb: Verb::Unfold,
                    inside: arrival.key,
                    outside: arrival.leaving,
                    row: node,
                    row_id: None,
                    find: None,
                    carry: Carry::new(0.0, 1.0, now),
                    start: now,
                    scroll: Point::default(),
                    fold: None,
                    print_after: if node.is_some() { UNFOLD_PRINT_AFTER } else { PRINT_AFTER },
                    gem,
                    symbol: symbol.filter(|_| node.is_some()),
                });
            }
        }
    }

    /// A declaration's kind, for the gem that carries it.
    fn kind_of(&self, symbol: &crate::model::pages::SymbolRef, cx: &gpui::App) -> Option<facet::icons::Kind> {
        let resource = self.links.store.read(cx).symbol(symbol);
        let page = resource.loaded_value()?;
        Some(crate::shell::kit::kind_of(page.identity.kind))
    }

    /// A Fold's node, followed every frame: the camera moves it while the
    /// plate closes onto it. With no node when the plate starts to move, the
    /// plate leaves through the right edge and keeps to it.
    fn follow_node(&mut self, reader: Bounds<Pixels>, cx: &gpui::App) {
        let now = facet::motion::now(cx);
        let node = self.graph_focus_glyph(cx).filter(|node| node.size.height > Pixels::ZERO && reader.intersects(node));
        let Some(transit) = self.transit.as_mut().filter(|transit| transit.verb == Verb::Fold) else { return };
        if transit.row.is_some() || now < transit.start + CLOSE_AFTER {
            if let Some(node) = node {
                transit.row = Some(node);
            }
        }
    }

    /// While an Unfold runs, the arriving page's own gem is held at its
    /// layout (the reader's gem travels there); released once it lands.
    fn hold_gem(&mut self, transit: Option<&Transit>, window: &Window, cx: &mut gpui::App) {
        let held = transit.filter(|transit| transit.verb == Verb::Unfold).and_then(|transit| transit.symbol.clone().zip(transit.row));
        let key = held.as_ref().map(|(symbol, _)| crate::shell::kit::shared_id(symbol));
        if let Some(previous) = self.held.take()
            && Some(&previous) != key.as_ref()
        {
            facet::motion::shared::release(previous, window, cx);
        }
        if let (Some((_, node)), Some(key)) = (held, key) {
            facet::motion::shared::drive(key.clone(), node, 1.0, window, cx);
            self.held = Some(key);
        }
    }

    /// The reader's column geometry for a snapshot's place.
    fn layout(&self, snapshot: &AppSnapshot, measure: &Measure, facet: &facet::Facet) -> Layout {
        let width = self.core.width();
        let room = measure.fluid_room();
        // The gutters glide from 16 px on a phone to the design's 40; nothing
        // here compares the width with a number.
        let pad = READER_PAD.at(room);
        let content = (width - pad * 2.0).max(px(0.0));
        let scale = measure.scale();
        let notes_possible = matches!(snapshot.route(), Route::Symbol(route) if route.view == View::Code)
            && snapshot.overlay().is_none();
        let margin_notes = self.core.modes().settle(&NOTES, room).mode == Notes::Beside;
        let wide = margin_notes && notes_possible;
        let gutter = px(GUTTER * facet.density.space() * scale);
        let margin = px(MARGIN * scale);
        let beside = if wide { margin + gutter } else { px(0.0) };
        // A declaration page (the simple symbol page) lays out its own
        // column and rail from the whole room; every other page reads at the
        // folio's measure.
        let own_width = matches!(snapshot.route(), Route::Symbol(route) if route.view == View::Page) && snapshot.overlay().is_none();
        let folio = if own_width { content } else { px(FOLIO * scale).min((content - beside).max(px(0.0))) };
        Layout {
            pad,
            top: READER_TOP.at(room),
            wide_measure: Measure::new(WIDE_FOLIO.at(room).min(content), facet),
            folio,
            beside,
            content,
            wide,
            gutter,
            margin,
            measure: *measure,
            folio_measure: Measure::new(folio, facet),
        }
    }

    /// A page drawn away from the scroller, where it was on screen: inert,
    /// cut to `mask`, drifted by `drift`, printed or folded by `edge`.
    #[allow(clippy::too_many_arguments)]
    fn still_page(
        &mut self,
        place: &Place,
        scroll: Point<Pixels>,
        mask: Bounds<Pixels>,
        edge: Option<Edge>,
        drift: Pixels,
        snapshot: &AppSnapshot,
        layout: &Layout,
        facet: &facet::Facet,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let body = self.body(place, false, snapshot, layout, facet, edge, cx);
        div().absolute().top_0().left_0().right_0().bottom_0().child(masked(
            mask,
            offset(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(scroll.y)
                    .px(layout.pad)
                    .pt(layout.top)
                    .child(div().relative().w_full().flex().justify_center().child(body)),
            )
            .x(drift),
        ))
    }

    /// This frame of the change in flight, in window space (and the change
    /// dropped once it has landed).
    fn stage(&mut self, reader: Bounds<Pixels>, column: (Pixels, Pixels), scale: f32, window: &Window, cx: &gpui::App) -> Option<Staged> {
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
        let graph = matches!(transit.verb, Verb::Fold | Verb::Unfold);
        // A row's plate spans the page's column; a node's plate is the node.
        let column = match (graph, transit.row) {
            (true, Some(node)) => (node.left(), node.right()),
            _ => column,
        };
        let plate = plate_at(reader, column, transit.row, p);
        let height = f32::from(reader.size.height);
        let edge = transit.edge_local(now, height).map(|local| {
            let y = reader.top() + px(local);
            match transit.verb {
                Verb::Open | Verb::Unfold => Edge::down(y, px(PRINT_SETTLE * scale), px(PRINT_SPEED * scale * 32.0)),
                Verb::Close | Verb::Fold => Edge::fold(y),
            }
        });
        // The gem travels between the hero and the node on the plate's own
        // driver: a Fold's from the hero (p = 1) to the node (p = 0), an
        // Unfold's from the node to where the page lays its gem out.
        let gem = match (transit.verb, transit.gem, transit.row) {
            (Verb::Fold, Some((kind, hero)), Some(node)) => Some((kind, lerp_rect(node, hero, band(p, LANDED, 0.9)))),
            (Verb::Unfold, Some((kind, node)), _) => transit
                .symbol
                .as_ref()
                .and_then(|symbol| facet::motion::shared::last_bounds(crate::shell::kit::shared_id(symbol), window, cx))
                .map(|hero| (kind, lerp_rect(node, hero, band(p, LANDED, 0.9)))),
            _ => None,
        };
        let drift = if graph { 0.0 } else { DRIFT * scale };
        Some(Staged {
            verb: transit.verb,
            p,
            plate,
            outside: uncovered(reader, plate, transit.row.is_some()),
            edge,
            inside_drift: px(drift * (1.0 - band(p, 0.5, 0.94))),
            outside_drift: px(-drift * p),
            moving: band(p, LANDED, 0.94) < 1.0 && band(p, LANDED, 0.94) > 0.0,
            has_row: transit.row.is_some(),
            covered: plate.size.width >= reader.size.width - px(0.5) && plate.size.height >= reader.size.height - px(0.5),
            gem,
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
            wide_measure,
            wide,
            gutter,
            margin,
            measure,
            folio_measure,
        } = *layout;
        let gap = folio_measure.space(Space::Wide);
        // A page with a wide block (a table, a ring of names) gets a column as
        // wide as that block may be; its prose keeps the reading column,
        // centred in it.
        let any_wide = leaves.iter().any(|leaf| leaf.wide);
        let reading = folio + beside;
        let column_width = if any_wide { wide_measure.width().max(reading).min(content) } else { reading };
        let mut column = div().flex().flex_col().gap(gap).w(column_width).max_w(content);
        for leaf in leaves {
            let is_wide = leaf.wide;
            // Each block prints under the one edge (reading order, by clip).
            let block = match (leaf.note, wide) {
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
            };
            let block = if any_wide && !is_wide {
                div().w(reading).max_w_full().mx_auto().child(block).into_any_element()
            } else {
                block
            };
            column = column.child(print(edge, block));
        }
        column
    }
}

/// Drops from the end of `places` every place no frame drew (`painted` is the
/// key of the one the last frame drew as current): a route another one
/// superseded before the reader rendered was never on screen, so it is not a
/// page that can leave. The oldest place stays whether or not it was drawn
/// (there is nothing older). Whether any went.
/// Whether the pages `keys` name have their content in the store (a
/// declaration, a package, the Library): what a reader that draws them shows
/// is the page, not its skeleton. Pages with no read of their own (the
/// graph, Find, settings) are always their content.
fn content_loaded(store: &DataStore, keys: &[PageKey]) -> bool {
    keys.iter().all(|key| match key {
        PageKey::Symbol(symbol) => store.symbol(symbol).is_loaded(),
        PageKey::Package(package) => store.package(package).is_loaded(),
        PageKey::Orbit => store.orbit().is_loaded(),
        PageKey::Source(_) | PageKey::Health | PageKey::Browse(_) | PageKey::Search(_) => true,
    })
}

fn drop_unpainted(places: &mut Vec<Place>, painted: Option<u64>) -> bool {
    let mut dropped = false;
    while places.len() > 1 && places.last().is_some_and(|place| Some(place.key) != painted) {
        places.pop();
        dropped = true;
    }
    dropped
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
    /// Reached by a hop forward (an Open, not a Back): a declaration page
    /// rings the declaration it came from.
    hop: bool,
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
    /// The measure wide content (tables, rails) may take (`WIDE_FOLIO`).
    wide_measure: Measure,
    wide: bool,
    gutter: Pixels,
    margin: Pixels,
    measure: Measure,
    folio_measure: Measure,
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
                wide: layout.wide_measure,
                content: Measure::new(layout.content, facet),
                modes: self.core.modes().clone(),
                ring_flow: self.ring_flow.clone(),
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
                // A hop forward from another declaration: it is ringed on this page.
                arrived_from: place.hop
                    .then(|| place.from.as_ref().and_then(|(route, _)| route_symbol(route)))
                    .flatten()
                    .filter(|from| route_symbol(&place.route).as_ref() != Some(from)),
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
        let on_graph = bodies::graph::is_graph(snapshot.route()) && snapshot.overlay().is_none();
        let layout = self.layout(&snapshot, &measure, &facet);
        let Layout { pad, top, folio, beside, content, .. } = layout;
        let scale = measure.scale();
        let Some(current) = self.places.last().cloned() else {
            return div();
        };
        // A page counts as painted once it drew its content: a skeleton "on its
        // way" is not a page that can leave (a route that supersedes it cuts
        // past it, and the change in flight goes on to the newer route).
        if content_loaded(self.links.store.read(cx), &place_keys(&current.route, current.overlay)) {
            self.painted = Some(current.key);
        }
        // The place change in flight, this frame (window space).
        let reader = self.frame.get();
        if let Some(arrival) = self.arrival.take()
            && let Some(reader) = reader
        {
            self.begin(arrival, reader, window, cx);
        }
        if let Some(reader) = reader {
            self.follow_node(reader, cx);
        }
        let column = reader.map(|reader| {
            let used = (folio + beside).min(content);
            let left = reader.left() + pad + (content - used) / 2.0;
            (left, left + used)
        });
        let staged = reader.zip(column).and_then(|(reader, column)| self.stage(reader, column, scale, window, cx));
        if staged.is_some() || self.tint.is_some() {
            facet::motion::request_frame(window, cx);
        }
        let transit = self.transit.clone().filter(|_| staged.is_some());
        let keep = |key: u64| key == current.key || transit.as_ref().is_some_and(|t| t.inside == key || t.outside == key);
        self.places.retain(|place| keep(place.key));
        // The page drawn away from the scroller this frame (the one leaving
        // an Open, the one folding on a Close's or a Fold's plate).
        let leaving = transit.as_ref().and_then(|transit| {
            let key = match transit.verb {
                Verb::Open => transit.outside,
                Verb::Close | Verb::Fold => transit.inside,
                Verb::Unfold => return None,
            };
            self.places.iter().find(|place| place.key == key).cloned()
        });
        // The declaration an Unfold arrives at carries its own gem: hold it
        // at its layout while the reader's gem travels there.
        self.hold_gem(transit.as_ref(), window, cx);
        let local = |bounds: Bounds<Pixels>| {
            let origin = reader.map_or(Point::default(), |reader| reader.origin);
            Bounds::new(bounds.origin - origin, bounds.size)
        };
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
                ground = match (staged.verb, staged.has_row) {
                    (Verb::Fold | Verb::Unfold, true) => ground.border_1(),
                    (_, true) => ground.border_t_1().border_b_1(),
                    (_, false) => ground.border_l_1(),
                }
                .border_color(hairline);
            }
            ground
        };
        let gem = |staged: &Staged| {
            staged.gem.map(|(kind, rect)| {
                let rect = local(rect);
                div()
                    .absolute()
                    .left(rect.origin.x)
                    .top(rect.origin.y)
                    .size(rect.size.height)
                    .child(facet::paint::gem(kind).size(f32::from(rect.size.height)))
            })
        };

        if on_graph {
            let map = self.map.get_or_insert_with(|| {
                let links = self.links.clone();
                cx.new(|cx| bodies::graph::Map::new(links, window, cx))
            }).clone();
            // A Fold carries the gem itself; the map's own handoff would
            // draw a second one.
            let source = self.graph_source.take().filter(|_| !transit.as_ref().is_some_and(|t| t.verb == Verb::Fold));
            map.update(cx, |map, cx| map.show(snapshot.route(), source.as_ref(), window, cx));
            self.said = vec!["Graph fixture · pages resolve through your local index".into()];
            self.hero.clear();
            let tint = self.tint_now(cx);
            self.publish(staged, tint.map(|(_, strength)| strength), cx);
            let framed = Reveal {
                pending: Rc::new(Cell::new(false)),
                targets: self.targets.clone(),
                scroll: self.scroll.clone(),
                frame: Rc::clone(&self.frame),
                child: div().size_full().child(map.clone()).into_any_element(),
            };
            let mut root = div().relative().size_full().text_color(palette.ink1.hsla()).font_family(facet::fonts::family(ty::BODY));
            match (staged, transit.as_ref(), leaving, reader) {
                (Some(staged), Some(transit), Some(leaving), Some(reader)) if staged.verb == Verb::Fold => {
                    // The graph was always behind the page: it is uncovered
                    // around the plate once the page has folded off it.
                    let map_mask = if staged.edge.is_some_and(|edge| edge.y > reader.top()) {
                        Bounds::new(reader.origin, size(Pixels::ZERO, Pixels::ZERO))
                    } else if staged.has_row {
                        reader
                    } else {
                        staged.outside[0]
                    };
                    root = root.child(masked(map_mask, framed));
                    let page = self.still_page(&leaving, transit.scroll, staged.plate, staged.edge, Pixels::ZERO, &snapshot, &layout, &facet, cx);
                    root = root
                        .child(plate_ground(&staged))
                        .child(div().id("folding-plate").absolute().top_0().left_0().size_full().child(page))
                        .children(gem(&staged));
                }
                _ => root = root.child(framed),
            }
            return root;
        }
        if let Some(map) = &self.map { map.update(cx, |map, cx| map.suspend(window, cx)); }

        // The current page, in the scroller: inside the plate when it opens
        // or unfolds, outside it (above) when the plate closes over it.
        let current_edge = staged.filter(|staged| matches!(staged.verb, Verb::Open | Verb::Unfold)).and_then(|staged| staged.edge);
        let body = self.body(&current, true, &snapshot, &layout, &facet, current_edge, cx);
        if let Some(reader) = reader {
            self.follow(reader, cx);
        }
        let drift = staged.map_or(Pixels::ZERO, |staged| match staged.verb {
            Verb::Open | Verb::Unfold => staged.inside_drift,
            Verb::Close | Verb::Fold => staged.outside_drift,
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
        match (staged, transit.as_ref(), leaving) {
            (Some(staged), Some(transit), Some(leaving)) if staged.verb == Verb::Open => {
                // The old page stays where it was, drifting left, cut to what
                // the plate has not reached; the new page is on the plate.
                for (index, mask) in staged.outside.into_iter().enumerate() {
                    if mask.size.height > Pixels::ZERO && mask.size.width > Pixels::ZERO {
                        let page = self.still_page(&leaving, transit.scroll, mask, None, staged.outside_drift, &snapshot, &layout, &facet, cx);
                        root = root.child(div().id(("leaving", index)).absolute().top_0().left_0().size_full().child(page));
                    }
                }
                root = root.child(plate_ground(&staged)).child(masked(staged.plate, scroller));
            }
            (Some(staged), Some(transit), Some(leaving)) if staged.verb == Verb::Close => {
                // The parent is uncovered around the closing plate; the page
                // it came back from folds on the plate, then the plate shuts.
                let [above, below] = staged.outside;
                root = root.child(masked(above, scroller));
                if below.size.height > Pixels::ZERO && below.size.width > Pixels::ZERO {
                    let page = self.still_page(&current, self.scroll.offset(), below, None, staged.outside_drift, &snapshot, &layout, &facet, cx);
                    root = root.child(div().id("parent-below").absolute().top_0().left_0().size_full().child(page));
                }
                // What the fold has taken from the plate is the parent, not an
                // empty ground: the page it came back from shows through as the
                // leaving page folds away, so there is no frame with an empty
                // reader between the one and the other.
                let folded = staged.edge.map(|edge| {
                    let top = edge.y.max(staged.plate.top()).min(staged.plate.bottom());
                    Bounds::from_corners(point(staged.plate.left(), top), staged.plate.bottom_right())
                });
                let page = self.still_page(&leaving, transit.scroll, staged.plate, staged.edge, staged.inside_drift, &snapshot, &layout, &facet, cx);
                root = root.child(plate_ground(&staged));
                if let Some(folded) = folded.filter(|folded| folded.size.height > Pixels::ZERO && folded.size.width > Pixels::ZERO) {
                    let parent = self.still_page(&current, self.scroll.offset(), folded, None, staged.outside_drift, &snapshot, &layout, &facet, cx);
                    root = root.child(div().id("parent-folded").absolute().top_0().left_0().size_full().child(parent));
                }
                root = root.child(div().id("leaving-plate").absolute().top_0().left_0().size_full().child(page));
            }
            (Some(staged), Some(_), _) if staged.verb == Verb::Unfold => {
                // The node's plate opens into the page over the graph; the
                // graph is drawn only where the plate has not reached, and
                // not at all once it is covered (the page prints then).
                if let (Some(map), Some(reader)) = (&self.map, reader) {
                    let map_mask = if staged.covered {
                        Bounds::new(reader.origin, size(Pixels::ZERO, Pixels::ZERO))
                    } else if staged.has_row {
                        reader
                    } else {
                        staged.outside[0]
                    };
                    root = root.child(div().id("unfolding-map").absolute().top_0().left_0().size_full().child(masked(map_mask, div().size_full().child(map.clone()))));
                }
                root = root.child(plate_ground(&staged)).child(masked(staged.plate, scroller)).children(gem(&staged));
            }
            _ => root = root.child(scroller),
        }
        root.child(super::kit::scroll_probe("reader-scroll", self.scroll.clone()))
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
    /// (`reader.plate.{left,top,right,bottom}`), a Fold's or an Unfold's
    /// travelling gem (`reader.gem.{…}`) and the where-you-were tint
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
            if let Some((_, gem)) = staged.gem {
                for (edge, at) in [("left", gem.left()), ("top", gem.top()), ("right", gem.right()), ("bottom", gem.bottom())] {
                    samples.push(track(format!("reader.gem.{edge}"), f32::from(at), f32::from(at), 0.0, started, budget));
                }
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
        gem: Option<Bounds<Pixels>>,
        p: Option<f32>,
        tint: Vec<(String, f32)>,
        targets: Vec<(String, Bounds<Pixels>)>,
        texts: Vec<PaintedText>,
    }

    /// A place in the reader's list, for the list's own rules.
    fn place(key: u64, route: Route) -> super::Place {
        super::Place { key, route, overlay: None, way: super::Way::Across, lens: super::Lens::Reference, from: None, opened: None, hop: false }
    }

    /// Three routes in one turn (A painted, then B, then C, no frame between)
    /// leave the list holding A: B was never on screen, so what leaves is A,
    /// not B's "on its way" skeleton. The oldest place is never dropped, and
    /// a place the last frame drew is kept.
    #[test]
    fn a_place_no_frame_drew_is_not_a_page_to_leave() {
        let (a, b, c) = (place(1, Route::World), place(2, package()), place(3, Route::World));
        let mut places = vec![a.clone(), b.clone(), c.clone()];
        assert!(super::drop_unpainted(&mut places, Some(1)), "B and C were never drawn");
        assert_eq!(places.iter().map(|place| place.key).collect::<Vec<_>>(), vec![1], "only A, the last page painted, is left to leave");
        let mut places = vec![a.clone(), b.clone(), c.clone()];
        assert!(super::drop_unpainted(&mut places, Some(2)));
        assert_eq!(places.iter().map(|place| place.key).collect::<Vec<_>>(), vec![1, 2], "B was drawn, C was not");
        let mut places = vec![a.clone()];
        assert!(!super::drop_unpainted(&mut places, None), "the first place stays, drawn or not");
        assert_eq!(places.len(), 1);
        let mut places = vec![a, b, c];
        assert!(!super::drop_unpainted(&mut places, Some(3)), "the current place was drawn: nothing to drop");
        assert_eq!(places.len(), 3);
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
        let gem = match (track("reader.gem.left"), track("reader.gem.top"), track("reader.gem.right"), track("reader.gem.bottom")) {
            (Some(l), Some(t), Some(r), Some(b)) => Some(Bounds::from_corners(point(px(l), px(t)), point(px(r), px(b)))),
            _ => None,
        };
        Shot {
            at,
            plate,
            gem,
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

    /// Films a change at exactly `times` (ms after its first frame).
    fn film_at(rig: &mut Rig, act: impl FnOnce(&mut Rig), times: &[u64]) -> Vec<Shot> {
        act(rig);
        let mut shots = Vec::new();
        let mut at = 0;
        for &time in times {
            shots.push(shoot(rig, time, time - at));
            at = time;
        }
        shots
    }

    fn start(cx: &mut TestAppContext) -> Rig {
        let rig = rig(cx, Some(package()), 1440.0, 900.0);
        rig.cx.update(|_, cx| {
            facet::probe::enable(cx);
            cx.set_global(gpui::TextTrace);
        });
        rig
    }

    /// The package page's first module, open: `j` walks to it, Enter opens it
    /// in place, and the next `j` will land on its first name.
    fn open_first_module(rig: &mut Rig) {
        rig.keys("j");
        rig.keys("enter");
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
        open_first_module(&mut rig);
        rig.keys("j");
        let old = rig.said();
        let before = shoot(&mut rig, 0, 0);
        // The package page's cards paint their words themselves (they are not
        // in `said`): what the old page says includes what it painted.
        let old: Vec<String> = old.into_iter().chain(before.texts.iter().map(|text| text.text.to_string())).collect();
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
        let new_only = only(&new, &old);
        let (mut printed, mut outside) = (BTreeSet::new(), BTreeSet::new());
        let mut last = first;
        for shot in &shots {
            let Some(plate) = shot.plate else { continue };
            assert!(inside(last, plate), "the plate never shrinks: {last:?} then {plate:?} at {} ms", shot.at);
            last = plate;
            for text in &shot.texts {
                let content = text.text.to_string();
                if old_only.contains(&content) && !crosses(text.bounds, plate) {
                    outside.insert(content.clone());
                }
                if new_only.contains(&content) {
                    assert!(inside(text.bounds, plate), "`{content}` (the new page's) is painted outside the plate at {} ms", shot.at);
                    printed.insert((shot.at, content.clone()));
                }
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
        // Not vacuous: the old page stayed readable outside the plate while
        // it opened, and the new page printed on it, top first.
        assert!(outside.len() >= 3, "the old page is seen outside the plate: {outside:?}");
        let first_print = printed.iter().map(|(at, _)| *at).min().expect("the new page prints on the plate");
        assert!((96..=160).contains(&first_print), "printing starts after the plate has cleared the top: {first_print} ms");
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
        open_first_module(&mut rig);
        rig.keys("j");
        let (_, focused) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        let row_id = focused.expect("j focuses a row").to_string();
        let parent = rig.said();
        rig.keys("enter");
        let page = rig.said();
        let shots = film(&mut rig, |rig| rig.cx.simulate_keystrokes("cmd-["), 1000);
        assert!(matches!(rig.route(), Route::Package(_)), "back came home");
        // The package page's cards paint their words themselves (they are not
        // in `said`): what the parent says includes what it painted once home.
        let parent: Vec<String> = parent.into_iter().chain(shots.last().into_iter().flat_map(|shot| shot.texts.iter().map(|text| text.text.to_string()))).collect();
        let page_only = only(&page, &parent);
        let row = shots
            .last()
            .and_then(|shot| shot.targets.iter().find(|(key, _)| *key == row_id).map(|(_, at)| *at))
            .expect("the row is drawn again");
        let parent_only = only(&parent, &page);
        assert!(!page_only.is_empty() && !parent_only.is_empty(), "the pages differ");
        let (mut folding, mut uncovered) = (0, BTreeSet::new());
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
                    folding += 1;
                }
                if parent_only.contains(&content) {
                    assert!(!crosses(text.bounds, plate), "`{content}` (the parent's) is painted on the plate at {} ms", shot.at);
                    uncovered.insert(content);
                }
            }
        }
        // Not vacuous: the page was seen folding, and the parent uncovered.
        assert!(folding > 0, "the page is seen on the plate as it folds");
        assert!(uncovered.len() >= 3, "the parent is seen around the closing plate: {uncovered:?}");
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

    /// Films the storyboard's pair as the real reader draws it — the
    /// RelationLabel page, a click on its SemanticLinkKind link (Open), then
    /// ⌘[ (Close) — writing every frame's plate, driver and painted text
    /// boxes to `$NUDOX_TRANSIT_FILM/transit-film.json` for the contact sheet
    /// beside `sig-open.png` / `sig-close.png`.
    #[gpui::test]
    #[ignore = "writes the storyboard films: NUDOX_TRANSIT_FILM=DIR … -- --ignored"]
    fn film_the_storyboard_pair(cx: &mut TestAppContext) {
        use crate::shell::tests::page_route;
        let Some(dir) = std::env::var_os("NUDOX_TRANSIT_FILM") else { return };
        let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
        rig.cx.update(|_, cx| {
            facet::probe::enable(cx);
            cx.set_global(gpui::TextTrace);
        });
        let rest = shoot(&mut rig, 0, 0);
        let want = std::env::var("NUDOX_TRANSIT_LINK").unwrap_or_else(|_| "SemanticLinkKind".to_owned());
        let link = rest
            .texts
            .iter()
            .filter(|text| text.bounds.origin.x > px(300.0) && text.bounds.origin.y > px(120.0))
            .find(|text| text.text.as_ref() == want)
            .or_else(|| {
                rest.texts.iter().find(|text| {
                    text.bounds.origin.x > px(300.0)
                        && rest.targets.iter().any(|(key, at)| key.starts_with("sym") && at.contains(&text.bounds.center()))
                })
            })
            .map(|text| text.bounds.center())
            .expect("a link on the page");
        rig.cx.simulate_mouse_move(link, None, gpui::Modifiers::none());
        rig.cx.run_until_parked();
        let _ = shoot(&mut rig, 0, 0);
        let open = film_at(&mut rig, |rig| rig.cx.simulate_click(link, gpui::Modifiers::none()), &[0, 40, 80, 120, 160, 240, 320, 400]);
        rig.settle();
        let close = film_at(&mut rig, |rig| rig.cx.simulate_keystrokes("cmd-["), &[0, 40, 80, 120, 160, 240, 320, 700, 1000]);
        let frame = |shot: &Shot| {
            let rect = |b: Bounds<Pixels>| serde_json::json!([f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)]);
            serde_json::json!({
                "t": shot.at,
                "p": shot.p,
                "plate": shot.plate.map(rect),
                "tint": shot.tint.iter().map(|(id, strength)| serde_json::json!({
                    "id": id,
                    "strength": strength,
                    "box": shot.targets.iter().find(|(key, _)| key == id).map(|(_, at)| rect(*at)),
                })).collect::<Vec<_>>(),
                "texts": shot.texts.iter().map(|text| serde_json::json!({"text": text.text.as_ref(), "box": rect(text.bounds), "alpha": text.alpha})).collect::<Vec<_>>(),
            })
        };
        let out = serde_json::json!({
            "link": [f32::from(link.x), f32::from(link.y)],
            "before": frame(&rest),
            "open": open.iter().map(frame).collect::<Vec<_>>(),
            "close": close.iter().map(frame).collect::<Vec<_>>(),
        });
        let path = std::path::PathBuf::from(dir).join("transit-film.json");
        std::fs::write(&path, out.to_string()).expect("write the film");
        eprintln!("wrote {}", path.display());
    }

    /// The page of `RelationLabel`, with the graph mounted behind it (its
    /// first visit lays the map out).
    fn page_over_graph(cx: &mut TestAppContext) -> Rig {
        use crate::navigation::{Intent, View};
        use crate::shell::tests::{page_route, view_route};
        let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
        rig.go(Intent::Navigate(page_route("RelationLabel")));
        rig.cx.update(|_, cx| {
            facet::probe::enable(cx);
            cx.set_global(gpui::TextTrace);
        });
        rig
    }

    fn set_view(view: crate::navigation::View) -> impl FnOnce(&mut Rig) {
        move |rig: &mut Rig| rig.graph.root.update(rig.cx, |root, cx| root.queue(crate::navigation::Intent::SetView(view), cx))
    }

    /// The reader's texts in a frame (the shelf and titlebar are not the
    /// reader's).
    fn reading(shot: &Shot) -> impl Iterator<Item = &PaintedText> {
        shot.texts.iter().filter(|text| text.bounds.origin.x >= px(264.0) && text.bounds.origin.y >= px(50.0))
    }

    fn near(a: Bounds<Pixels>, b: Bounds<Pixels>, within: f32) -> bool {
        [(a.left(), b.left()), (a.top(), b.top()), (a.right(), b.right()), (a.bottom(), b.bottom())]
            .iter()
            .all(|(x, y)| (*x - *y).abs() <= px(within))
    }

    /// G on a page: the page folds on its plate first, with the graph not
    /// drawn at all; then the empty plate closes into the node (never
    /// growing) while the graph is uncovered around it, and the gem travels
    /// from the hero into the node, landing on the node's glyph.
    #[gpui::test]
    fn a_page_folds_into_its_node_and_the_graph_is_uncovered_around_it(cx: &mut TestAppContext) {
        use crate::navigation::View;
        let mut rig = page_over_graph(cx);
        let page: BTreeSet<String> = reading(&shoot(&mut rig, 0, 0)).map(|text| text.text.to_string()).collect();
        let shots = film(&mut rig, set_view(View::Graph), 480);
        rig.settle();
        let node = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_focus_glyph(cx)).expect("the graph focuses the node");
        let (mut folding, mut uncovered) = (0, BTreeSet::new());
        let mut last: Option<Bounds<Pixels>> = None;
        for shot in &shots {
            let Some(plate) = shot.plate else { continue };
            if let Some(last) = last {
                assert!(inside(plate, last), "the plate never grows: {last:?} then {plate:?} at {} ms", shot.at);
            }
            last = Some(plate);
            let held = shot.p.is_some_and(|p| p >= 0.999);
            for text in reading(shot) {
                let content = text.text.to_string();
                if held {
                    assert!(page.contains(&content), "`{content}` (not the page's) is painted while the page folds, at {} ms: {:?}", shot.at, text.bounds);
                    folding += 1;
                } else {
                    if !page.contains(&content) {
                        uncovered.insert(content);
                    }
                }
            }
            if !held {
                // The page has folded off its plate before the plate moves.
                let on_plate: Vec<_> = reading(shot).filter(|text| page.contains(&text.text.to_string()) && inside(text.bounds, plate) && plate.size.width < px(1170.0)).map(|text| text.text.to_string()).collect();
                assert!(on_plate.is_empty(), "the page is still on the moving plate at {} ms: {on_plate:?}", shot.at);
            }
        }
        assert!(folding > 0, "the page is seen folding");
        assert!(!uncovered.is_empty(), "the graph is uncovered around the plate");
        let landing = shots.iter().filter(|shot| shot.plate.is_some()).last().expect("a plate");
        assert!(near(landing.plate.expect("plate"), node, 2.0), "the plate closes onto the node: {:?} vs {node:?}", landing.plate);
        assert!(near(landing.gem.expect("the gem travels"), node, 2.0), "the gem lands on the node: {:?} vs {node:?}", landing.gem);
        assert!(shots.iter().find(|shot| shot.at == 400).is_some_and(|shot| shot.plate.is_none()), "landed by 400 ms");
    }

    /// Back to the page from its node: the plate opens from the node's
    /// glyph (never shrinking) over the graph, the node's gem grows into
    /// the hero, no line of the page is drawn until the plate covers the
    /// reader, and none of the graph's after.
    #[gpui::test]
    fn a_node_unfolds_into_its_page_which_prints_once_the_plate_covers(cx: &mut TestAppContext) {
        use crate::navigation::View;
        let mut rig = page_over_graph(cx);
        let page: BTreeSet<String> = reading(&shoot(&mut rig, 0, 0)).map(|text| text.text.to_string()).collect();
        set_view(View::Graph)(&mut rig);
        rig.settle();
        let rest = shoot(&mut rig, 0, 0);
        let graph: BTreeSet<String> = reading(&rest).map(|text| text.text.to_string()).filter(|text| !page.contains(text)).collect();
        let page_only: BTreeSet<String> = page.iter().filter(|text| !reading(&rest).any(|seen| seen.text.as_ref() == text.as_str())).cloned().collect();
        assert!(!graph.is_empty() && !page_only.is_empty(), "the views differ: {graph:?}");
        let node = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_focus_glyph(cx)).expect("a focused node");
        let shots = film(&mut rig, set_view(View::Page), 480);
        rig.settle();
        let hero = rig
            .cx
            .update(|window, cx| facet::motion::shared::last_bounds(crate::shell::kit::shared_id(&crate::shell::tests::symbol("RelationLabel")), window, cx))
            .expect("the page paints its hero");
        let first = &shots[0];
        assert!(near(first.plate.expect("a plate"), node, 1.0), "frame 0's plate is the node: {:?} vs {node:?}", first.plate);
        assert!(near(first.gem.expect("a gem"), node, 1.0), "frame 0's gem is the node: {:?} vs {node:?}", first.gem);
        let (mut printed, mut behind) = (0, 0);
        let mut last = first.plate.expect("a plate");
        for shot in &shots {
            let Some(plate) = shot.plate else { continue };
            assert!(inside(last, plate), "the plate never shrinks: {last:?} then {plate:?} at {} ms", shot.at);
            last = plate;
            let covered = plate.size.width >= px(1175.0) && plate.size.height >= px(823.5);
            for text in reading(shot) {
                let content = text.text.to_string();
                if covered {
                    assert!(!graph.contains(&content), "`{content}` (the graph's) is painted over the covered reader at {} ms", shot.at);
                    printed += usize::from(page_only.contains(&content));
                } else {
                    assert!(!page_only.contains(&content), "`{content}` (the page's) is painted before the plate covers, at {} ms: {plate:?}", shot.at);
                    behind += usize::from(graph.contains(&content));
                }
            }
        }
        assert!(printed > 0 && behind > 0, "the graph is seen around the plate ({behind}) and the page prints on it ({printed})");
        let landing = shots.iter().filter(|shot| shot.gem.is_some()).last().expect("a gem");
        assert!(near(landing.gem.expect("gem"), hero, 1.0), "the gem lands on the hero: {:?} vs {hero:?}", landing.gem);
    }

    /// Films the Fold and the Unfold at the storyboard's t values into
    /// `$NUDOX_TRANSIT_FILM/fold-film.json` (the contact sheet beside
    /// `sig-fold.png` / `sig-unfold.png`).
    #[gpui::test]
    #[ignore = "writes the fold films: NUDOX_TRANSIT_FILM=DIR … -- --ignored"]
    fn film_the_fold_pair(cx: &mut TestAppContext) {
        use crate::navigation::View;
        let Some(dir) = std::env::var_os("NUDOX_TRANSIT_FILM") else { return };
        let mut rig = page_over_graph(cx);
        let rest = shoot(&mut rig, 0, 0);
        let times = [0, 40, 80, 120, 160, 240, 320, 480];
        let fold = film_at(&mut rig, set_view(View::Graph), &times);
        rig.settle();
        let unfold = film_at(&mut rig, set_view(View::Page), &times);
        let frame = |shot: &Shot| {
            let rect = |b: Bounds<Pixels>| serde_json::json!([f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)]);
            serde_json::json!({
                "t": shot.at,
                "p": shot.p,
                "plate": shot.plate.map(rect),
                "gem": shot.gem.map(rect),
                "tint": [],
                "texts": shot.texts.iter().map(|text| serde_json::json!({"text": text.text.as_ref(), "box": rect(text.bounds), "alpha": text.alpha})).collect::<Vec<_>>(),
            })
        };
        let out = serde_json::json!({
            "before": frame(&rest),
            "fold": fold.iter().map(frame).collect::<Vec<_>>(),
            "unfold": unfold.iter().map(frame).collect::<Vec<_>>(),
        });
        let path = std::path::PathBuf::from(dir).join("fold-film.json");
        std::fs::write(&path, out.to_string()).expect("write the film");
    }

    /// Back at 120 ms, mid-open: the one driver turns around from its
    /// painted value (no jump, it keeps its momentum for a moment) and the
    /// plate closes from where it was.
    #[gpui::test]
    fn back_mid_open_retargets_the_same_plate(cx: &mut TestAppContext) {
        let mut rig = start(cx);
        open_first_module(&mut rig);
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

/// The route film (catalog film A's acts) through the real shell, judged on
/// painted text boxes: while the fixture index blocks the harness (and its
/// pixels), this is the desktop's ledger. Self-contained (it names nothing
/// in this file), so the same module judges any reader.
///
/// `NUDOX_TRANSIT_LEDGER=DIR … -- --ignored route_film` writes
/// `DIR/route-film.ledger`.
#[cfg(test)]
mod transit_ledger {
    use crate::navigation::{Intent, Route};
    use crate::shell::tests::{PACKAGE, Rig, page_route, rig};
    use gpui::{Bounds, PaintedText, Pixels, TestAppContext, px};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt::Write as _;
    use std::time::Duration;

    /// A frame's reader texts, keyed `content` / `content#n` in paint order.
    fn frame(rig: &mut Rig, ms: u64) -> Vec<(String, Bounds<Pixels>, f32)> {
        if ms > 0 {
            rig.cx.executor().advance_clock(Duration::from_millis(ms));
        }
        rig.cx.run_until_parked();
        rig.cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let texts: Vec<PaintedText> = rig.cx.update(|window, _| window.painted_texts().to_vec());
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        texts
            .into_iter()
            .filter(|text| text.bounds.origin.x >= px(264.0) && text.bounds.origin.y >= px(50.0) && text.alpha > 0.0)
            .map(|text| {
                let content = text.text.to_string();
                let n = seen.entry(content.clone()).or_default();
                *n += 1;
                let key = if *n == 1 { content } else { format!("{content}#{n}") };
                (key, text.bounds, text.alpha)
            })
            .collect()
    }

    fn crossing(a: Bounds<Pixels>, b: Bounds<Pixels>) -> bool {
        let cut = a.intersect(&b);
        cut.size.width > px(1.0) && cut.size.height > px(1.0)
    }

    /// One act's window: overlap runs (pairs crossing while both are drawn
    /// at alpha ≥ .25, and not crossing at rest), faded runs (alpha in
    /// .05..<.95 for more than two frames), and how long text of the page
    /// that left is still drawn.
    fn judge(label: &str, film: &[Vec<(String, Bounds<Pixels>, f32)>], before: &BTreeSet<String>) -> (bool, String) {
        let rest = film.last().cloned().unwrap_or_default();
        let after: BTreeSet<String> = rest.iter().map(|(key, ..)| key.clone()).collect();
        // A pair that also crosses once everything is still is a layout
        // overlap, not motion (counted apart, as the pixel check does). It
        // is matched by content: occurrence keys shift as labels come and go.
        let content = |key: &str| key.rsplit_once('#').filter(|(_, n)| n.parse::<usize>().is_ok()).map_or(key, |(text, _)| text).to_owned();
        let at_rest = |a: &str, b: &str| {
            let (a, b) = (content(a), content(b));
            rest.iter().enumerate().any(|(i, (x, xb, _))| {
                content(x) == a && rest.iter().enumerate().any(|(j, (y, yb, _))| i != j && content(y) == b && crossing(*xb, *yb))
            })
        };
        let mut layout = BTreeSet::new();
        let (mut overlap, mut faded): (BTreeMap<(String, String), Vec<usize>>, BTreeMap<String, Vec<usize>>) = Default::default();
        let mut lingers = 0;
        for (index, texts) in film.iter().enumerate() {
            for (i, (a, at, alpha)) in texts.iter().enumerate() {
                if *alpha > 0.05 && *alpha < 0.95 {
                    faded.entry(a.clone()).or_default().push(index);
                }
                if before.contains(a) && !after.contains(a) && *alpha > 0.05 {
                    lingers = lingers.max(index);
                }
                for (b, bt, beta) in &texts[i + 1..] {
                    if *alpha >= 0.25 && *beta >= 0.25 && crossing(*at, *bt) {
                        let pair = if a <= b { (a.clone(), b.clone()) } else { (b.clone(), a.clone()) };
                        if at_rest(a, b) {
                            layout.insert((content(&pair.0), content(&pair.1)));
                        } else {
                            overlap.entry(pair).or_default().push(index);
                        }
                    }
                }
            }
        }
        let runs = |frames: &Vec<usize>| {
            let mut runs = Vec::new();
            for &f in frames {
                match runs.last_mut() {
                    Some((_, end)) if *end + 1 == f => *end = f,
                    _ => runs.push((f, f)),
                }
            }
            runs
        };
        let overlaps: Vec<_> = overlap.iter().flat_map(|(pair, frames)| runs(frames).into_iter().map(move |run| (pair.clone(), run))).collect();
        let fades: Vec<_> = faded
            .iter()
            .flat_map(|(key, frames)| runs(frames).into_iter().filter(|(a, b)| b - a + 1 > 2).map(move |run| (key.clone(), run)))
            .collect();
        let pass = overlaps.is_empty() && fades.is_empty();
        let mut out = format!(
            "{}  {label}  overlap {}  faded {}  (+{} layout overlaps at rest)  old text drawn until +{} ms  texts +{} -{}\n",
            if pass { "PASS" } else { "FAIL" },
            overlaps.len(),
            fades.len(),
            layout.len(),
            lingers * 16,
            after.difference(before).count(),
            before.difference(&after).count(),
        );
        let mut worst = overlaps.clone();
        worst.sort_by_key(|(_, (a, b))| std::cmp::Reverse(b - a));
        for ((a, b), (from, to)) in worst {
            let _ = writeln!(out, "      overlap {}..{} ms: `{a}` × `{b}`", from * 16, to * 16);
        }
        for (key, (from, to)) in fades {
            let _ = writeln!(out, "      faded   {}..{} ms: `{key}`", from * 16, to * 16);
        }
        for (a, b) in layout {
            let _ = writeln!(out, "      at rest: `{a}` × `{b}`");
        }
        (pass, out)
    }

    #[gpui::test]
    #[ignore = "the route film's box ledger: NUDOX_TRANSIT_LEDGER=DIR … -- --ignored"]
    fn route_film(cx: &mut TestAppContext) {
        let Some(dir) = std::env::var_os("NUDOX_TRANSIT_LEDGER") else { return };
        let package = Route::Package(crate::navigation::PackageRoute {
            project: None,
            at: None,
            package: crate::core::PackageId::new(PACKAGE).expect("package"),
            lane: crate::navigation::PackageLane::Overview,
            selected: None,
        });
        let mut rig = rig(cx, None, 1440.0, 900.0);
        rig.cx.update(|_, cx| cx.set_global(gpui::TextTrace));
        type Act = Box<dyn Fn(&mut Rig)>;
        let navigate = |route: Route| -> Act { Box::new(move |rig: &mut Rig| rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(route.clone()), cx))) };
        let key = |keys: &'static str| -> Act { Box::new(move |rig: &mut Rig| rig.cx.simulate_keystrokes(keys)) };
        let acts: Vec<(&str, Act)> = vec![
            ("route-down orbit→package", navigate(package.clone())),
            ("route-down package→symbol (RelationLabel)", navigate(page_route("RelationLabel"))),
            ("back symbol→package (⌘[)", key("cmd-[")),
            ("forward package→symbol (⌘])", key("cmd-]")),
            ("across symbol→symbol (KindGlyph)", navigate(page_route("KindGlyph"))),
            ("back across (⌘[)", key("cmd-[")),
            ("view page→code (⌘.)", key("cmd-.")),
            ("view code→page (⌘.)", key("cmd-.")),
            ("route-up (⌘↑)", key("cmd-up")),
            ("route-down package→symbol (again)", navigate(page_route("RelationLabel"))),
            ("graph-enter (G)", key("g")),
            ("graph-exit (route view=page)", navigate(page_route("RelationLabel"))),
            ("route world", navigate(Route::World)),
            ("world→package", navigate(package.clone())),
        ];
        let mut ledger = String::new();
        let mut passed = 0;
        for (label, act) in &acts {
            rig.settle();
            let before: BTreeSet<String> = frame(&mut rig, 0).into_iter().map(|(key, ..)| key).collect();
            act(&mut rig);
            let mut film = vec![frame(&mut rig, 0)];
            for _ in 0..50 {
                film.push(frame(&mut rig, 16));
            }
            let (pass, text) = judge(label, &film, &before);
            passed += usize::from(pass);
            ledger.push_str(&text);
        }
        let _ = writeln!(ledger, "route film: {passed} of {} transitions pass", acts.len());
        let path = std::path::PathBuf::from(dir).join("route-film.ledger");
        std::fs::write(&path, &ledger).expect("write the ledger");
        eprint!("{ledger}");
    }
}
