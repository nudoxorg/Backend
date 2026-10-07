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
//! frame shows two texts over each other. Page and Code peel around the
//! measured declaration line; reduced motion cuts, and a Close still marks
//! the row.
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
use super::focus::{NativeFocusDeparture, Targets};
use super::keyboard::NativeReturnLease;
use super::kit::HoverIntent;
use super::region::{Links, Region, RegionCore, a11y_inert};
use super::jump::route_symbol;
use crate::model::AppSnapshot;
use crate::model::pages::{PageKey, Stamp};
use crate::navigation::{BrowseRoute, OrbitRoute, Overlay, Route, View};
use crate::runtime::store::{Branch, CargoReadAdmission, DataStore, OwnerAttachment, RouteDependencies, StoreEvent};
use facet::anatomy::symbol::key::FoldKey;
use facet::motion::{Carry, Edge, Presence, band, masked, offset, print};
use facet::tokens::ty;
use facet::tokens::fluid::{NOTES, Notes, READER_PAD, READER_TOP, WIDE_FOLIO};
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{
    App, AppContext as _, Bounds, Context, ElementId, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement, Pixels, Point,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div, point, px, size,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// Kept per declaration across lens changes; bounded to protect long sessions.
#[derive(Clone)]
pub(crate) struct SymbolDisclosure {
    scope: String,
    /// The folds the page has open, by what they are (never by a position in a render).
    open: BTreeSet<FoldKey>,
    unrolls: Rc<std::cell::RefCell<std::collections::BTreeMap<FoldKey, Presence>>>,
    /// The simple page's own state: the list's filters.
    pub(crate) ui: facet::anatomy::symbol::Ui,
    /// What the page remembers between frames (where its sections are).
    pub(crate) spots: Rc<facet::anatomy::symbol::Spots>,
    /// The page's own flow, so its parts spring when its layout changes mode
    /// and one declaration's parts never flow into another's.
    pub(crate) flow: facet::motion::Flow,
}

/// A route that is not a declaration has no disclosure to keep.
impl Default for SymbolDisclosure {
    fn default() -> Self {
        Self {
            scope: String::new(),
            open: BTreeSet::new(),
            unrolls: Rc::default(),
            ui: facet::anatomy::symbol::Ui::default(),
            spots: Rc::default(),
            flow: facet::motion::Flow::new("symbol-page"),
        }
    }
}

impl SymbolDisclosure {
    fn for_symbol(symbol: &crate::model::pages::SymbolRef) -> Self {
        // `lands_lines`: a part that changes line (the rail going under the
        // page) lands there, never flying across the page's words; nothing
        // waits, so words the owner sends are shown the frame they land.
        Self { scope: symbol.as_str().to_owned(), flow: facet::motion::Flow::new(format!("symbol-page-{}", symbol.as_str())).lands_lines(), ..Self::default() }
    }
    pub(crate) fn is_open(&self, fold: &FoldKey) -> bool { self.open.contains(fold) }
    pub(crate) fn unroll(&self, fold: FoldKey) -> Presence {
        let mut unrolls = self.unrolls.borrow_mut();
        if unrolls.len() >= 128 && !unrolls.contains_key(&fold) {
            if let Some(key) = unrolls.keys().next().cloned() { unrolls.remove(&key); }
        }
        unrolls.entry(fold.clone()).or_insert_with(|| Presence::new(format!("symbol-unroll-{}-{fold:?}", self.scope))).clone()
    }
    fn toggle(&mut self, fold: FoldKey) {
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
    /// The same declaration's page and source exchange through a strip
    /// centred on its measured hero or source line.
    Peel,
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
            (Verb::Close | Verb::Fold | Verb::Peel, None) => None,
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
            Verb::Peel => if self.carry.target() >= 0.5 { p >= 0.94 } else { p <= LANDED },
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
    /// Where the plate's edges are headed and how fast they move with `p`.
    plate_course: Course,
    /// The gem's, when it travels.
    gem_course: Option<Course>,
}

/// Where a rectangle the plate's openness drives is headed, and how far each
/// of its edges (left, top, right, bottom) moves per unit of openness at
/// this frame: what the probe is told, so each edge is judged on its own
/// curve (its bands are not the driver's).
#[derive(Clone, Copy, Debug)]
struct Course {
    to: Bounds<Pixels>,
    per_p: [f32; 4],
}

impl Course {
    fn of(at: impl Fn(f32) -> Bounds<Pixels>, p: f32, target: f32) -> Self {
        const STEP: f32 = 1e-3;
        let (low, high) = ((p - STEP).max(0.0), (p + STEP).min(1.0));
        let span = (high - low).max(1e-6);
        let (a, b) = (edges(at(low)), edges(at(high)));
        Self { to: at(target), per_p: [0, 1, 2, 3].map(|index| (b[index] - a[index]) / span) }
    }
}

/// A rectangle's edges: left, top, right, bottom.
fn edges(rect: Bounds<Pixels>) -> [f32; 4] {
    [f32::from(rect.left()), f32::from(rect.top()), f32::from(rect.right()), f32::from(rect.bottom())]
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

/// A Peel has no sideways travel: the source file opens above and below the
/// actual declaration row, exposing exactly one page in every painted pixel.
fn peel_at(reader: Bounds<Pixels>, row: Bounds<Pixels>, p: f32) -> Bounds<Pixels> {
    let open = band(p, LANDED, 0.94);
    let top = lerp(row.top().clamp(reader.top(), reader.bottom()), reader.top(), open);
    let bottom = lerp(row.bottom().clamp(reader.top(), reader.bottom()), reader.bottom(), open);
    Bounds::from_corners(point(reader.left(), top), point(reader.right(), bottom.max(top)))
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

#[derive(Clone)]
struct NativeReturn {
    /// One foreground return request; an old painted callback cannot complete
    /// a later request even when route, target text, and authority agree.
    ticket: Rc<()>,
    input: NativeReturnLease,
    visit: crate::navigation::presentation::VisitId,
    place: u64,
    route: Route,
    root: crate::core::VersionedRoot,
    id: SharedString,
    attachment: OwnerAttachment,
    read_stamp: Option<(PageKey, Stamp)>,
}

/// What permission a mounted Reader callback needs beyond its exact visit.
/// Recovery and local presentation work through an unavailable owner;
/// navigation from published bytes requires that owner and resource now.
#[derive(Clone)]
pub(crate) enum NativeActionLease {
    LocalUi,
    OwnerSnapshot(Option<OwnerAttachment>),
    Resource { attachment: Option<OwnerAttachment>, stamp: Option<(PageKey, Stamp)> },
}

#[derive(Clone)]
struct FindFocusReturn {
    place: u64,
    route: Route,
    root: crate::core::VersionedRoot,
    focused: FocusHandle,
    interruption: u64,
    attachment: Option<OwnerAttachment>,
    retry: Option<crate::runtime::store::OwnerRetryAttachment>,
    /// Armed only after the cover retires; later native input revokes it.
    after_cover_input: Option<u64>,
}

/// The actual mounted Find input, reported by its component after paint.
#[derive(Clone)]
struct MountedFindQuery {
    visit: crate::navigation::presentation::VisitId,
    state: facet::browse::find::FindState,
    place: u64,
    route: Route,
    root: crate::core::VersionedRoot,
    focus: Option<FocusHandle>,
}

/// The reader region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewportScroll {
    PageDown,
    PageUp,
    Top,
    Bottom,
}

/// A viewport choice remains the scroll owner across reflow until a new
/// target walk. Top and bottom stay anchored to their respective edge;
/// displaced pages retain their actual pixel position on later reflows.
#[derive(Clone, Copy)]
enum ViewportOrigin {
    Offset(Pixels),
    Restored(Pixels),
    Top,
    Bottom,
}

/// Keys wait for the next layout, where the Reader's actual content height
/// and viewport are both known. Their order is preserved before that paint:
/// Home followed by Page Down is one page from the new top.
struct ViewportIntent {
    place: u64,
    origin: ViewportOrigin,
    from: Point<Pixels>,
    focused: Option<SharedString>,
    commands: Vec<ViewportRun>,
}

/// Autorepeat is one operation regardless of how many Page keys arrive
/// before a usable layout. Absolute commands discard preceding runs because
/// their result is independent of the earlier position.
struct ViewportRun {
    command: ViewportScroll,
    count: u64,
}

const MAX_QUEUED_VIEWPORT_RUNS: usize = 128;

struct PendingScrollRestore {
    place: u64,
    offset: Point<Pixels>,
    /// A returned reading visit owns even a saved Home at zero. A fresh
    /// visit may instead reveal its initial source target.
    origin: Option<ViewportOrigin>,
}

/// Visit IDs are checked monotonic allocations. The previous live place
/// seeds the first comparison; thereafter one high watermark identifies a
/// Back/Forward return even if unpainted intermediate visits were abandoned.
fn returned_visit(
    highest: &mut Option<crate::navigation::presentation::VisitId>,
    previous: crate::navigation::presentation::VisitId,
    incoming: crate::navigation::presentation::VisitId,
) -> bool {
    let watermark = (*highest).unwrap_or(previous);
    let returning = incoming <= watermark;
    *highest = Some(watermark.max(incoming));
    returning
}

#[cfg(test)]
mod viewport_intent_tests {
    use super::*;
    use crate::navigation::presentation::VisitId;

    #[test]
    fn visit_watermark_recognizes_an_old_return_after_a_full_history_branch() {
        let first = VisitId::initial();
        let mut highest = None;
        let mut current = first;
        for _ in 0..crate::navigation::MAX_ROUTE_HISTORY {
            let next = current.next().expect("checked visit allocation");
            assert!(!returned_visit(&mut highest, current, next));
            current = next;
        }
        let old = current;
        let prior = first.next().expect("first fresh visit");
        assert!(returned_visit(&mut highest, old, prior), "Back returns to an issued ID");
        let branch = old.next().expect("new branch visit");
        assert!(!returned_visit(&mut highest, prior, branch));
        assert!(returned_visit(&mut highest, branch, first),
            "the oldest still-retained visit remains a return after forward history was abandoned");
    }

    #[test]
    fn queued_runs_refuse_only_a_new_alternation_at_capacity() {
        let mut intent = ViewportIntent {
            place: 1,
            origin: ViewportOrigin::Offset(px(0.0)),
            from: point(px(0.0), px(0.0)),
            focused: None,
            commands: Vec::new(),
        };
        for index in 0..MAX_QUEUED_VIEWPORT_RUNS {
            let command = if index % 2 == 0 { ViewportScroll::PageDown } else { ViewportScroll::PageUp };
            assert!(intent.push(command));
        }
        let before = intent.resolve(px(100.0), px(250.0));
        assert_eq!(intent.commands.len(), MAX_QUEUED_VIEWPORT_RUNS);
        assert!(!intent.push(ViewportScroll::PageDown), "an alternating overflow is refused");
        assert_eq!(intent.commands.len(), MAX_QUEUED_VIEWPORT_RUNS);
        assert_eq!(intent.resolve(px(100.0), px(250.0)), before,
            "refusal does not silently alter an accepted program");
        assert!(intent.push(ViewportScroll::PageUp), "same-direction repeat coalesces at capacity");
        assert_eq!(intent.commands.len(), MAX_QUEUED_VIEWPORT_RUNS);
        assert_eq!(intent.commands.last().map(|run| run.count), Some(2));
        intent.commands.last_mut().expect("last run").count = u64::MAX;
        let saturated = intent.resolve(px(100.0), px(250.0));
        assert!(intent.push(ViewportScroll::PageUp), "a saturated repeat still reaches the same edge");
        assert_eq!(intent.commands.len(), MAX_QUEUED_VIEWPORT_RUNS);
        assert_eq!(intent.commands.last().map(|run| run.count), Some(u64::MAX));
        assert_eq!(intent.resolve(px(100.0), px(250.0)), saturated);
        assert!(intent.push(ViewportScroll::Top), "Home supersedes a full queue");
        assert_eq!(intent.commands.len(), 1);
        assert!(intent.push(ViewportScroll::PageDown));
        assert_eq!(intent.resolve(px(100.0), px(250.0)), px(-100.0));
    }
}

impl ViewportIntent {
    /// Refuse an additional alternating run at the cap. Same-direction
    /// autorepeat and absolute Home/End remain admissible. The caller then
    /// propagates a refused key without changing focus or reveal ownership.
    fn push(&mut self, command: ViewportScroll) -> bool {
        if matches!(command, ViewportScroll::Top | ViewportScroll::Bottom) {
            self.commands.clear();
        } else if let Some(last) = self.commands.last_mut()
            && last.command == command {
            last.count = last.count.saturating_add(1);
            return true;
        } else if self.commands.len() >= MAX_QUEUED_VIEWPORT_RUNS {
            return false;
        }
        self.commands.push(ViewportRun { command, count: 1 });
        true
    }

    fn resolve(&self, height: Pixels, max: Pixels) -> Pixels {
        let mut y = match self.origin {
            ViewportOrigin::Offset(y) | ViewportOrigin::Restored(y) => y.clamp(-max, px(0.0)),
            ViewportOrigin::Top => px(0.0),
            ViewportOrigin::Bottom => -max,
        };
        // More than this many pages always reaches an edge, regardless of
        // the starting offset. Bound the multiplication, not the key count.
        let to_edge = ((f32::from(max) / f32::from(height)).ceil() as u64).saturating_add(1);
        for run in &self.commands {
            let distance = height * run.count.min(to_edge) as f32;
            y = match run.command {
                ViewportScroll::PageDown => y - distance,
                ViewportScroll::PageUp => y + distance,
                ViewportScroll::Top => px(0.0),
                ViewportScroll::Bottom => -max,
            }.clamp(-max, px(0.0));
        }
        y
    }

    fn settled_origin(&self, y: Pixels) -> ViewportOrigin {
        match self.commands.last().map(|run| run.command) {
            Some(ViewportScroll::Top) => ViewportOrigin::Top,
            Some(ViewportScroll::Bottom) => ViewportOrigin::Bottom,
            Some(ViewportScroll::PageDown | ViewportScroll::PageUp) => ViewportOrigin::Offset(y),
            None => self.origin,
        }
    }
}

/// Exactly the Reader branch that mounts the shared GPUI scroll container.
/// A Settings/Inbox page replaces even a Graph route, and a retained World
/// body uses the Reader when there is no live Graph to paint.
pub(crate) fn mounts_scroll_body(store: &DataStore, route: &Route, page_overlay: Option<Overlay>) -> bool {
    !bodies::graph::is_graph(route) || page_overlay.is_some() || bodies::saved_world(store, route, page_overlay)
}

pub(crate) struct Reader {
    core: RegionCore,
    /// The shell's occupied Ask rectangle for this frame, if results show.
    ask_geometry: Option<super::frame::AskGeometry>,
    /// The shell's painted Ask scene, rather than its already-cleared overlay,
    /// decides when a newly arrived body may claim native keyboard focus.
    ask_background_input_allowed: bool,
    map: Option<Entity<bodies::graph::Map>>,
    graph_source: Option<crate::model::pages::SymbolRef>,
    links: Links,
    pub(crate) targets: Targets,
    hover: HoverIntent,
    scroll: ScrollHandle,
    /// The visit whose actual scroll container completed prepaint. A route
    /// render alone cannot make the previous body's measured extent current.
    scroll_mounted: Rc<Cell<Option<u64>>>,
    /// A typed viewport origin remains ahead of target-follow through
    /// subsequent reflows. A new target walk or visit retires that ownership.
    viewport_intent: Rc<RefCell<Option<ViewportIntent>>>,
    /// Bounded source cursor/history memory keyed by exact code route and owner revision.
    source_paging: SourcePagingMemory,
    /// Shared empty handle for non-code bodies; they never write paging state.
    empty_source_paging: Rc<RefCell<Option<bodies::PagingState>>>,
    /// Bounded Library disclosure and virtual-list scroll state for exact trees.
    library_state: LibraryStateMemory,
    /// Bounded neutral README pagination for this Reader's window and visits.
    readme_paging: Rc<RefCell<bodies::ReadmePagingMemory>>,
    /// Applied only after the destination content can paint.
    pending_scroll_restore: Option<PendingScrollRestore>,
    /// One checked monotonic visit watermark, seeded from the live place on
    /// first arrival; no parallel cache of route history is kept.
    highest_visit: Option<crate::navigation::presentation::VisitId>,
    /// Bring the focused target into view in the next frame's prepaint (a
    /// keyboard walk, or a reflow that may have moved it).
    reveal: Rc<Cell<bool>>,
    /// Initial source-line focus, latched once per exact place and resource
    /// revision so ordinary frames never steal focus back from the user.
    source_focus_applied: Rc<Cell<Option<(u64, u32, Option<SourceGeneration>)>>>,
    /// A Back return belongs to one visit, and waits for its live control to
    /// mount before transferring native focus from the Shell.
    native_return: Option<NativeReturn>,
    /// The Find query's exact focused handle before Ask or Add took the keyboard.
    find_focus_return: Option<FindFocusReturn>,
    mounted_find_query: Option<MountedFindQuery>,
    /// Tab/J/overlay interruptions invalidate a deferred component return.
    native_return_interruption: u64,
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
    /// The change in flight's last published tracks, ended at rest once it
    /// lands (the probe).
    in_flight: std::cell::RefCell<Vec<facet::probe::TrackSample>>,
    /// The Library's ring of names: its own flow, so a name that wraps to
    /// another line glides there (one per reader, not one per app).
    ring_flow: facet::motion::Flow,
    /// The key of the place the last frame drew as the current page: what a
    /// page change leaves. A place no frame drew (a route another one
    /// superseded in the same instant) was never on screen.
    painted: Option<u64>,
    /// The owner revision of the last painted place, for exact overlay returns.
    painted_root: Option<crate::core::VersionedRoot>,
    /// Graph paint belongs to one mounted scene and serving attachment.
    painted_graph: Option<(gpui::EntityId, bodies::graph::MountedGraph, OwnerAttachment)>,
    /// Deferred Back focus belongs to the requested place, never its predecessor.
    pending_page_focus: Option<SettingsReturn>,
    /// Reader focus when Settings covered a painted place.
    settings_departure: Option<SettingsDeparture>,
    settings_native_origin: Option<SettingsNativeOrigin>,
    /// One return focus attempt after the uncovered page actually registers targets.
    pending_settings_focus: Option<PendingSettingsReturn>,
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
    find_held_root: Option<crate::core::VersionedRoot>,
    find_workspace: Option<crate::core::LocalProjectId>,
}

impl Reader {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        let snapshot = store.snapshot();
        Self {
            core: RegionCore::new(
                store,
                &[Branch::Root, Branch::Route, Branch::Reading, Branch::Overlay, Branch::Workspace, Branch::Settings],
            ),
            ask_geometry: None,
            ask_background_input_allowed: true,
            links,
            map: None,
            graph_source: None,
            targets: Targets::named("reader"),
            hover: HoverIntent::default(),
            scroll: ScrollHandle::new(),
            scroll_mounted: Rc::new(Cell::new(None)),
            viewport_intent: Rc::new(RefCell::new(None)),
            source_paging: SourcePagingMemory::default(),
            empty_source_paging: Rc::new(RefCell::new(None)),
            library_state: LibraryStateMemory::default(),
            readme_paging: Rc::new(RefCell::new(bodies::ReadmePagingMemory::default())),
            pending_scroll_restore: None,
            highest_visit: None,
            reveal: Rc::new(Cell::new(false)),
            source_focus_applied: Rc::new(Cell::new(None)),
            native_return: None,
            find_focus_return: None,
            mounted_find_query: None,
            native_return_interruption: 0,
            laid_out: None,
            lens: Lens::Reference,
            route: snapshot.route().clone(),
            overlay: snapshot.overlay(),
            descents: 0,
            last_way: None,
            painted: None,
            painted_root: None,
            painted_graph: None,
            pending_page_focus: None,
            settings_departure: None,
            settings_native_origin: None,
            pending_settings_focus: None,
            // The ring re-wraps as the library grows: a name that moves to
            // another line lands there, never flying across the others.
            ring_flow: facet::motion::Flow::new("orbit-ring").wrapped(),
            in_flight: std::cell::RefCell::default(),
            places: vec![Place {
                key: 0,
                visit: snapshot.session().reading.current.id,
                route: snapshot.route().clone(),
                overlay: snapshot.page_overlay(),
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
            find_held_root: None,
            find_workspace: snapshot.workspace().host.clone(),
        }
    }

    pub(crate) fn graph_ready(&self, cx: &gpui::App) -> bool {
        self.map.as_ref().is_none_or(|map| map.read(cx).ready(cx))
    }

    pub(crate) fn graph_work_status(
        &self,
        cx: &gpui::App,
    ) -> Option<bodies::graph::MapWorkStatus> {
        self.map
            .as_ref()
            .and_then(|map| map.read(cx).work_status(cx))
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
    pub(crate) fn graph_projection_evidence(&self, cx: &gpui::App)
        -> Option<(crate::runtime::indexed_world::Key, crate::runtime::indexed_world::Origin)>
    {
        self.map.as_ref().and_then(|map| map.read(cx).projection_evidence())
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
            let snapshot = self.links.snapshot(cx);
            self.links.dispatch(crate::navigation::Intent::SetReading { visit: snapshot.session().reading.current.id,
                change: crate::navigation::presentation::ReadingChange::ReaderLens(presentation_lens(lens)) }, cx);
            cx.notify();
        }
    }

    /// The test rig enters the same navigation adapter as mounted actions.
    #[cfg(test)]
    pub(crate) fn navigation_links(&self) -> Links { self.links.clone() }

    pub(crate) fn capture_reading(&self, snapshot: &AppSnapshot) -> Vec<crate::navigation::presentation::ReadingChange> {
        use crate::navigation::presentation::{ReadingChange, ReadingFocus, ReadingOffset, ReadingText};
        if snapshot.page_overlay().is_some() || snapshot.session().preview.is_some()
            || self.route != *snapshot.route() || !self.native_input_for(snapshot.route(), None)
            || self.places.last().is_none_or(|place| place.visit != snapshot.session().reading.current.id) { return Vec::new(); }
        let mut changes = Vec::new();
        let offset = self.scroll.offset();
        if let Some(offset) = ReadingOffset::new(f32::from(offset.x), f32::from(offset.y)) {
            changes.push(ReadingChange::ReaderOffset(offset));
        }
        changes.push(ReadingChange::ReaderLens(presentation_lens(self.lens)));
        if let Some(key) = self.targets.leaving_focus(snapshot.route()) {
            if let Some(key) = ReadingText::new(key.to_string()) {
                changes.push(ReadingChange::Focus(Some(ReadingFocus::Reader(key))));
            }
        }
        changes
    }

    /// The disclosure of `symbol`, made when there is none.
    pub(crate) fn symbol_disclosure(&mut self, symbol: &crate::model::pages::SymbolRef) -> SymbolDisclosure {
        if let Some((_, state)) = self.symbol_disclosures.iter().find(|(key, _)| key == symbol) { return state.clone(); }
        let state = SymbolDisclosure::for_symbol(symbol);
        self.symbol_disclosures.push((symbol.clone(), state.clone()));
        if self.symbol_disclosures.len() > 24 { self.symbol_disclosures.remove(0); }
        state
    }

    /// Changes `symbol`'s disclosure (kept most recent last, at most 24) and repaints.
    fn with_disclosure(&mut self, symbol: crate::model::pages::SymbolRef, change: impl FnOnce(&mut SymbolDisclosure), cx: &mut Context<Self>) {
        let mut state = self.symbol_disclosures.iter().position(|(key, _)| key == &symbol)
            .map(|at| self.symbol_disclosures.remove(at).1).unwrap_or_else(|| SymbolDisclosure::for_symbol(&symbol));
        change(&mut state);
        self.symbol_disclosures.push((symbol, state));
        if self.symbol_disclosures.len() > 24 { self.symbol_disclosures.remove(0); }
        cx.notify();
    }

    pub(crate) fn toggle_symbol(&mut self, symbol: crate::model::pages::SymbolRef, fold: FoldKey, cx: &mut Context<Self>) {
        self.with_disclosure(symbol, |state| state.toggle(fold), cx);
    }

    /// Applies a change to the simple page's state (its filters).
    pub(crate) fn change_symbol(&mut self, symbol: crate::model::pages::SymbolRef, change: &facet::anatomy::symbol::Change, cx: &mut Context<Self>) {
        self.with_disclosure(symbol, |state| state.ui = state.ui.clone().apply(change), cx);
    }

    pub(crate) fn toggle_package_outline(&mut self, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Package(_)) {
            self.package_outline_expanded = !self.package_outline_expanded;
            cx.notify();
        }
    }

    pub(crate) fn set_find_held(&mut self, held: Vec<facet::browse::find::HeldPackage>, cx: &mut Context<Self>) {
        let root = self.links.snapshot(cx).key();
        if self.find_held_root.is_some_and(|old| !old.same_authority(root)) {
            self.find_held.clear();
            self.find_held_root = None;
            cx.notify();
            return;
        }
        let mut unique: Vec<facet::browse::find::HeldPackage> = Vec::with_capacity(4);
        for package in held {
            if !unique.iter().any(|item| item.key == package.key) { unique.push(package); }
            if unique.len() == 4 { break; }
        }
        if self.find_held != unique {
            self.find_held = unique;
            self.find_held_root = (!self.find_held.is_empty()).then_some(root);
            cx.notify();
        }
    }

    pub(crate) fn holds_find_packages_at(&self, keys: &[SharedString], root: crate::core::VersionedRoot) -> bool {
        self.find_held_root.is_some_and(|held| held.same_authority(root))
            && self.find_held.iter().map(|item| &item.key).eq(keys.iter())
    }

    /// A Compare removal edits the current held hand, never the hand captured
    /// by an older painted callback or one stamped by a different owner.
    pub(crate) fn remove_find_held_package(&mut self, key: &SharedString, root: crate::core::VersionedRoot, cx: &mut Context<Self>) {
        if !self.find_held_root.is_some_and(|held| held.same_authority(root)) { return; }
        let before = self.find_held.len();
        self.find_held.retain(|package| package.key != *key);
        if self.find_held.len() != before {
            if self.find_held.is_empty() { self.find_held_root = None; }
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
        // A new keyboard target is a new intent; it may follow again after
        // the user explicitly chose Home, End or a page displacement.
        self.viewport_intent.borrow_mut().take();
        self.reveal.set(true);
    }

    /// A Reader child may own native focus even while the Shell's logical
    /// zone has not caught up with a pointer or accessibility focus move.
    pub(crate) fn owns_keyboard_scroll_focus(&self, window: &Window, cx: &App) -> bool {
        let snapshot = self.links.snapshot(cx);
        mounts_scroll_body(self.links.store.read(cx), snapshot.route(), snapshot.page_overlay())
            && self.native_input_for(snapshot.route(), snapshot.page_overlay())
            && self.targets.native_focused(window).is_some()
    }

    /// Admit a command for the one persistent Reader scroller. Prepaint uses
    /// its new measured geometry, so a resize or zoom waiting to paint cannot
    /// make Page Down use the previous viewport or End use the old extent.
    pub(crate) fn keyboard_scroll(&mut self, command: ViewportScroll, cx: &mut Context<Self>) -> bool {
        let snapshot = self.links.snapshot(cx);
        let Some(place) = self.places.last().map(|place| place.key) else { return false; };
        if !mounts_scroll_body(self.links.store.read(cx), snapshot.route(), snapshot.page_overlay())
            || !self.native_input_for(snapshot.route(), snapshot.page_overlay())
            || bodies::reading_destination(self.links.store.read(cx), snapshot.route(), snapshot.page_overlay()).pending()
            || self.pending_scroll_restore.is_some()
            || self.scroll_mounted.get() != Some(place)
        {
            return false;
        }
        let offset = self.scroll.offset();
        if !f32::from(offset.y).is_finite() { return false; }
        let focused = self.targets.focused();
        let accepted = {
            let mut intent = self.viewport_intent.borrow_mut();
            match intent.as_mut() {
                Some(queued) if queued.place == place && queued.from == offset && queued.focused == focused =>
                    queued.push(command),
                _ => {
                    let mut queued = ViewportIntent {
                        place,
                        origin: ViewportOrigin::Offset(offset.y),
                        from: offset,
                        focused,
                        commands: Vec::new(),
                    };
                    let accepted = queued.push(command);
                    *intent = Some(queued);
                    accepted
                }
            }
        };
        if !accepted { return false; }
        // Even a boundary Home or End supersedes a queued target reveal.
        self.reveal.set(false);
        cx.notify();
        true
    }

    /// A neutral view handle, not a focus capability. The Graph validates its
    /// exact native paint/owner/control scope after these Reader gates, while
    /// no Map entity is mutably leased.
    fn graph_native_view(&self, cx: &App) -> Result<Option<Entity<facet::graph::GraphView>>, ()> {
        let snapshot = self.links.snapshot(cx);
        if !self.native_input_allowed() || self.route != *snapshot.route()
            || snapshot.overlay().is_some() || snapshot.page_overlay().is_some()
            || !bodies::graph::is_graph(snapshot.route()) { return Err(()); }
        Ok(self.map.as_ref().and_then(|map| map.read(cx).native_graph()))
    }

    #[cfg(debug_assertions)]
    pub(crate) fn graph_native_diagnostic(&self, cx: &App) -> String {
        let snapshot = self.links.snapshot(cx);
        format!("graph_visit={:?}; input={}, settled={}, route_equal={}, overlay={}, page_overlay={}",
            self.graph_native_view(cx).map(|graph| graph.map(|graph| graph.entity_id())), self.native_input_allowed(), self.native_motion_settled(),
            self.route == *snapshot.route(), snapshot.overlay().is_some(), snapshot.page_overlay().is_some())
    }

    /// Move through the mounted native controls in the Reader's existing
    /// target order. Only a current boundary leaves Tab to the Shell zone
    /// walk; a denied Graph step consumes the key without moving focus.
    pub(crate) fn step_native(&self, forward: bool, window: &mut Window, cx: &mut gpui::App) -> bool {
        if bodies::graph::is_graph(self.links.snapshot(cx).route()) {
            match self.graph_native_view(cx) {
                Err(()) => return true, // A stale/covered visit is denial, never a boundary.
                Ok(Some(graph)) => return graph.update(cx, |graph, cx| graph.step_native(forward, window, cx))
                    != facet::graph::view::NativeFocusStep::Boundary,
                Ok(None) => {} // Current placeholder retains its existing Targets/zone walk.
            }
        }
        self.targets.native_step(forward, window, cx)
    }

    /// A pointer or native accessibility action can focus a mounted control
    /// without walking the Shell's logical zone. Admit only the settled,
    /// current place, then make that real handle the Reader walk origin.
    pub(crate) fn adopt_mounted_native_focus(&self, window: &Window, cx: &gpui::App) -> Option<bool> {
        if bodies::graph::is_graph(self.links.snapshot(cx).route()) {
            let graph = self.graph_native_view(cx).ok().flatten()?;
            return graph.read(cx).owns_native_focus(window, cx).then_some(false);
        }
        let current = self.places.last()?;
        let snapshot = self.links.snapshot(cx);
        if !self.native_motion_settled()
            || snapshot.route() != &current.route
            || snapshot.page_overlay() != current.overlay
        {
            return None;
        }
        let id = self.targets.native_focused(window)?;
        let changed = self.targets.focused().as_ref() != Some(&id);
        if changed { self.targets.focus(id); }
        Some(changed)
    }

    pub(crate) fn focus_native_current(&self, window: &mut Window, cx: &mut gpui::App) -> bool {
        if bodies::graph::is_graph(self.links.snapshot(cx).route()) {
            let Ok(Some(graph)) = self.graph_native_view(cx) else { return false; };
            return graph.update(cx, |graph, cx| graph.focus_native_current(window, cx));
        }
        self.targets.focused().is_some_and(|id| self.targets.focus_native(&id, window, cx))
    }

    pub(crate) fn request_native_return(&mut self, route: Route, visit: crate::navigation::presentation::VisitId, id: SharedString, input: NativeReturnLease, cx: &mut Context<Self>) {
        if self.route == route && self.overlay.is_none() {
            if let Some(place) = self.places.last() {
                let snapshot = self.links.snapshot(cx);
                if place.visit != visit || snapshot.session().reading.current.id != visit { return; }
                let root = snapshot.key();
                let store = self.links.store.read(cx);
                let Some(attachment) = store.current_owner_attachment() else { return };
                let read_stamp = RouteDependencies::new(&route, None).native_stamp(store, false);
                self.native_return = Some(NativeReturn { ticket: Rc::new(()), input, visit, place: place.key, route, root, id, attachment, read_stamp });
                cx.notify();
            }
        }
    }

    /// Complete only after this Reader's actual child has painted. Some
    /// components register their native targets in RenderOnce, after Reader
    /// finishes gathering its body; render-time list membership is premature.
    fn finish_painted_native_return(&mut self, ticket: &Rc<()>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.native_return.clone().filter(|pending| Rc::ptr_eq(&pending.ticket, ticket)) else { return; };
        let snapshot = self.links.snapshot(cx);
        let resources_current = {
            let store = self.links.store.read(cx);
            store.admits_owner_attachment(&pending.attachment)
                && pending.read_stamp.as_ref().is_none_or(|stamp|
                    RouteDependencies::new(&pending.route, None).admits_native_stamp(store, pending.root, stamp))
        };
        let input_owned = self.links.shell.upgrade().is_some_and(|shell| {
            let shell = shell.read(cx);
            pending.input.current(window.window_handle().window_id(), shell.focus_return_generation(), window.focus_epoch())
                && shell.allows_reader_native_return(window)
        });
        let current = self.places.last();
        if current.is_none_or(|place| place.key != pending.place || place.visit != pending.visit || place.route != pending.route || place.overlay.is_some())
            || snapshot.session().reading.current.id != pending.visit
            || snapshot.route() != &pending.route || snapshot.page_overlay().is_some()
            || !pending.root.same_authority(snapshot.key()) || !resources_current || !input_owned
            || super::titlebar::menu_open(window, cx)
            || self.targets.native_focused(window).is_some_and(|focused| focused != pending.id)
        {
            self.native_return = None;
            return;
        }
        if self.painted != Some(pending.place) || !self.native_input_allowed() { return; }
        let Some(mount) = self.targets.mount_claim(&pending.id) else {
            self.native_return = None;
            return;
        };
        // A scalar saved selection resolves against the newly painted target;
        // its real native owner must still be mounted in this exact Window.
        if !self.targets.admits_mount(&mount, window) {
            self.native_return = None;
            return;
        }
        if self.targets.focus_native(&pending.id, window, cx) {
            self.targets.focus(pending.id);
            self.native_return = None;
            self.reveal.set(true);
            cx.notify();
        } else {
            self.native_return = None;
        }
    }

    #[cfg(test)]
    pub(super) fn diagnostic_native_history_return_after_paint(&self) -> bool {
        self.native_return.as_ref().is_some_and(|pending|
            self.painted == Some(pending.place) && self.native_motion_settled())
    }

    #[cfg(test)]
    pub(super) fn diagnostic_native_history_return_pending(&self) -> bool {
        self.native_return.is_some()
    }

    pub(crate) fn cancel_native_return(&mut self) {
        self.native_return = None;
        self.find_focus_return = None;
        self.native_return_interruption = self.native_return_interruption.wrapping_add(1);
        for entry in &self.library_state.entries {
            entry.state.borrow_mut().cancel_return();
        }
    }

    /// Region observation can retire the old controls before the Shell sees
    /// the overlay event. Match its actual displaced native handle against
    /// that short-lived identity receipt, never against logical selection.
    pub(super) fn capture_settings_native_origin(&mut self, origin: Option<&FocusHandle>, cx: &mut App) -> bool {
        let target = origin.and_then(|origin| match &self.settings_departure {
            Some(departure) => departure.native.target_for(origin),
            None => self.targets.target_for_native_handle(origin),
        });
        let find = self.mounted_find_query.as_ref().filter(|mounted| {
            mounted.focus.as_ref() == origin && origin.is_some()
                && self.painted == Some(mounted.place)
                && mounted.state.focus_handle(cx).as_ref() == origin
                && mounted.route == *self.links.snapshot(cx).route()
                && mounted.root.same_authority(self.links.snapshot(cx).key())
        });
        let graph = self.map.as_ref().and_then(|map| map.update(cx, |map, cx| map.capture_settings_root(origin, cx)));
        let receipt = if let Some(target) = target {
            SettingsNativeOrigin::Target(target)
        } else if let Some(find) = find {
            SettingsNativeOrigin::Find(find.focus.clone().expect("matched native query"))
        } else if let Some(graph) = graph {
            SettingsNativeOrigin::Graph(graph)
        } else {
            SettingsNativeOrigin::Unmatched
        };
        // Ask/Add can cover the Settings page itself. Returning to that same
        // page must not replace its original Reader receipt with the retiring
        // cover's native editor. A fresh opening starts with Unmatched, so its
        // actual origin is still captured regardless of subscriber order.
        if matches!(receipt, SettingsNativeOrigin::Unmatched)
            && self.settings_departure.as_ref().is_some_and(|departure|
                !matches!(departure.origin, SettingsNativeOrigin::Unmatched)) {
            return false;
        }
        let reader_origin = !matches!(receipt, SettingsNativeOrigin::Unmatched);
        if let Some(mounted) = &self.mounted_find_query { mounted.state.suspend(cx); }
        if let Some(departure) = &mut self.settings_departure {
            departure.origin = receipt;
        } else {
            // Shell and Region are independent subscribers. The receipt is
            // identical whichever observes the opening event first.
            self.settings_native_origin = Some(receipt);
        }
        reader_origin
    }

    pub(super) fn arm_settings_focus_return(&mut self, lease: NativeReturnLease, cx: &mut Context<Self>) {
        if let Some(pending) = &mut self.pending_settings_focus {
            pending.lease = Some(lease);
            if let SettingsNativeOrigin::Graph(origin) = &pending.origin {
                if self.places.last().is_some_and(|place| place.visit == origin.visit)
                    && pending.focus.has_same_authority(self.links.snapshot(cx).key())
                    && let Some(map) = &self.map
                    && map.entity_id() == origin.component
                {
                    let origin = origin.clone();
                    let authority = pending.focus.root.authority();
                    map.update(cx, |map, cx| { map.arm_settings_root_return(origin, authority, lease, cx); });
                }
                self.targets.clear_focus();
                self.pending_settings_focus = None;
            }
            cx.notify();
        }
    }

    pub(crate) fn cancel_find_focus_return(&mut self) {
        self.find_focus_return = None;
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_find_return_pending(&self) -> bool {
        self.find_focus_return.is_some()
    }

    fn begin_find_focus_return(&mut self, focused: Option<FocusHandle>, cx: &App) {
        self.find_focus_return = None;
        let Some(focused) = focused else { return; };
        let Some(place) = self.places.last() else { return; };
        if !matches!(place.route, Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome | BrowseRoute::Find(_))))
            || self.painted != Some(place.key) { return; }
        let (attachment, retry) = {
            let store = self.links.store.read(cx);
            (store.current_owner_attachment(), store.current_owner_retry())
        };
        self.find_focus_return = Some(FindFocusReturn {
            place: place.key,
            route: place.route.clone(),
            root: self.links.snapshot(cx).key(),
            focused,
            interruption: self.native_return_interruption,
            attachment,
            retry,
            after_cover_input: None,
        });
    }

    /// Bind the return to the input generation after a cover's dismissing key.
    /// A later key or pointer must never lose a race to the deferred callback.
    pub(crate) fn arm_find_focus_return_after_cover(&mut self, focused: Option<&FocusHandle>, input: Option<u64>) -> bool {
        let (Some(focused), Some(input), Some(pending)) = (focused, input, self.find_focus_return.as_mut()) else { return false; };
        if pending.focused != *focused { return false; }
        pending.after_cover_input = Some(input);
        true
    }

    /// A cover may return focus only if its captured handle was the
    /// current Find component's mounted query, not a Shelf or old-page stop.
    pub(crate) fn begin_mounted_find_focus_return(&mut self, focused: Option<FocusHandle>, cx: &App) -> bool {
        let Some(focused) = focused else { return false; };
        let Some(mounted) = self.mounted_find_query.as_ref() else { return false; };
        let Some(place) = self.places.last() else { return false; };
        if mounted.focus.as_ref() != Some(&focused) || mounted.place != place.key || mounted.route != place.route
            || !mounted.root.same_authority(self.links.snapshot(cx).key())
            || self.painted != Some(place.key) { return false; }
        self.begin_find_focus_return(Some(focused), cx);
        self.find_focus_return.is_some()
    }

    pub(crate) fn return_find_query_focus(
        &mut self,
        query: FocusHandle,
        source_place: u64,
        source_route: &Route,
        source_root: crate::core::VersionedRoot,
        window: &mut Window,
        cx: &mut App,
    ) -> facet::browse::library::ReturnDisposition {
        use facet::browse::library::ReturnDisposition;
        // This callback comes from the active, mounted native query after its
        // frame. It is the only source of the handle accepted at cover opening.
        if let Some(place) = self.places.last().filter(|place|
            place.key == source_place && &place.route == source_route
                && matches!(place.route, Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome | BrowseRoute::Find(_))))
                && self.painted == Some(place.key)
                && self.links.snapshot(cx).overlay().is_none()
                && self.links.snapshot(cx).key().same_authority(source_root)) {
            if let Some(mounted) = &mut self.mounted_find_query
                && mounted.visit == place.visit
                && mounted.state.focus_handle(cx).as_ref() == Some(&query)
            {
                mounted.place = place.key;
                mounted.route = place.route.clone();
                mounted.root = self.links.snapshot(cx).key();
                mounted.focus = Some(query.clone());
            }
        }
        if let Some(pending) = self.pending_settings_focus.as_ref()
            && pending.focus.place == source_place
            && matches!(&pending.origin, SettingsNativeOrigin::Find(focused) if *focused == query)
        {
            let current = pending.focus.has_same_authority(source_root)
                && self.links.snapshot(cx).overlay().is_none()
                && self.painted == Some(source_place) && self.native_input_allowed()
                && !super::titlebar::menu_open(window, cx)
                && self.mounted_find_query.as_ref().is_some_and(|mounted|
                    mounted.place == source_place && mounted.focus.as_ref() == Some(&query)
                        && mounted.state.focus_handle(cx).as_ref() == Some(&query));
            let lease_current = pending.lease.map(|lease| self.links.shell.upgrade().is_some_and(|shell| {
                let shell = shell.read(cx);
                lease.current(window.window_handle().window_id(), shell.focus_return_generation(), window.focus_epoch())
                    && shell.allows_reader_native_return(window)
            }));
            if lease_current == Some(false) || !pending.focus.has_same_authority(source_root) {
                self.pending_settings_focus = None;
                return ReturnDisposition::Invalid;
            }
            if current && lease_current == Some(true) && window.is_focus_handle_mounted(&query) {
                self.pending_settings_focus = None;
                window.focus(&query, cx);
                return ReturnDisposition::Applied;
            }
            return ReturnDisposition::Waiting;
        }
        let Some(pending) = self.find_focus_return.clone() else { return ReturnDisposition::Invalid; };
        if pending.place != source_place || &pending.route != source_route
            || !pending.root.same_authority(source_root) {
            // A late callback from an old Find frame must not retire the new
            // visit's pending return.
            return ReturnDisposition::Invalid;
        }
        if pending.interruption != self.native_return_interruption {
            self.find_focus_return = None;
            return ReturnDisposition::Invalid;
        }
        if let Some(expected) = pending.after_cover_input
            && self.links.shell.upgrade().and_then(|shell| shell.read(cx).focus_return_generation()) != Some(expected) {
            self.find_focus_return = None;
            return ReturnDisposition::Invalid;
        }
        let store = self.links.store.read(cx);
        let owner_current = store.current_owner_attachment() == pending.attachment
            && store.current_owner_retry() == pending.retry;
        drop(store);
        if pending.focused == query && owner_current
            && matches!(self.links.snapshot(cx).overlay(), Some(Overlay::AddProject | Overlay::CommandPalette)) {
            return ReturnDisposition::Waiting;
        }
        let result = if pending.focused == query && owner_current
            && self.links.snapshot(cx).key().same_authority(pending.root) {
            self.native_return_disposition(
                pending.place, &pending.route, None, pending.root,
                &NativeActionLease::LocalUi, pending.interruption, window, cx,
            )
        } else {
            ReturnDisposition::Invalid
        };
        if result != ReturnDisposition::Waiting { self.find_focus_return = None; }
        if result == ReturnDisposition::Applied { window.focus(&query, cx); }
        result
    }

    pub(crate) fn admits_native_visit(
        &self,
        place: u64,
        route: &Route,
        overlay: Option<Overlay>,
        root: crate::core::VersionedRoot,
        lease: &NativeActionLease,
        source: Option<SourceGeneration>,
        inventory_revision: Option<[u8; 32]>,
        cx: &gpui::App,
    ) -> bool {
        if self.places.last().is_none_or(|current| current.key != place)
            || self.route != *route
            || self.overlay != overlay
            || self.links.snapshot(cx).route() != route
            || self.links.snapshot(cx).overlay() != overlay
        {
            return false;
        }
        // Local setup and recovery belong to this mounted visit, not to a
        // producer. A fresh installation has an intentionally unserved root.
        if matches!(lease, NativeActionLease::LocalUi) { return true; }
        if !self.links.snapshot(cx).key().same_authority(root) { return false; }
        let store = self.links.store.read(cx);
        if !store.snapshot().key().same_authority(root) { return false; }
        let read_stamp = match lease {
            NativeActionLease::LocalUi => unreachable!("local UI returned before owner admission"),
            NativeActionLease::OwnerSnapshot(attachment) => {
                if attachment.as_ref().is_none_or(|attachment| !store.admits_owner_attachment(attachment)) { return false; }
                None
            }
            NativeActionLease::Resource { attachment, stamp } => {
                if attachment.as_ref().is_none_or(|attachment| !store.admits_owner_attachment(attachment))
                    || stamp.as_ref().is_none_or(|stamp| !RouteDependencies::new(route, overlay).admits_native_stamp(store, root, stamp))
                { return false; }
                stamp.as_ref()
            }
        };
        if let Route::CargoSource(file) = route {
            if !matches!(lease, NativeActionLease::Resource { .. }) || read_stamp.is_none() { return false; }
            let Ok(package) = crate::model::pages::PackageRef::parse(file.package.as_str()) else { return false };
            if file.browse.context().is_none() {
                return source.is_none() && inventory_revision.is_none()
                    && RouteDependencies::new(route, overlay).current_cargo_package(store, &package).is_some();
            }
            let Some(context) = file.browse.context() else { return false; };
            match source {
                Some(SourceGeneration::Cargo(digest)) => {
                    let key = crate::model::pages::CargoSourceKey {
                        context: context.clone(), package, target: file.target.clone(),
                    };
                    let resource = store.cargo_source(&key);
                    if resource.loaded_value().is_none_or(|page| page.content_digest != digest)
                    {
                        return false;
                    }
                }
                None => {
                    let Some(revision) = inventory_revision else { return false };
                    let key = crate::model::browse::BrowseKey::CargoSourceInventory(
                        crate::model::browse::CargoSourceInventoryKey {
                            context: context.clone(), package,
                        },
                    );
                    let resource = store.pages().browse(&key);
                    if !matches!(resource.loaded_value(), Some(crate::model::browse::BrowseValue::CargoSourceInventory(model)) if model.source_revision == revision)
                    {
                        return false;
                    }
                }
                Some(SourceGeneration::Indexed(_) | SourceGeneration::Retained(_)) => return false,
            }
        }
        true
    }

    /// Facet supplies the exact mounted virtual-row handle, but this Reader
    /// alone admits its deferred focus after the return page has settled.
    pub(crate) fn native_return_disposition(
        &self,
        place: u64,
        route: &Route,
        overlay: Option<Overlay>,
        root: crate::core::VersionedRoot,
        lease: &NativeActionLease,
        interruption: u64,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> facet::browse::library::ReturnDisposition {
        use facet::browse::library::ReturnDisposition;
        if self.native_return_interruption != interruption
            || !self.admits_native_visit(place, route, overlay, root, lease, None, None, cx)
            || !self
                .links
                .shell
                .upgrade()
                .is_some_and(|shell| shell.read(cx).allows_reader_native_return(window))
            || super::titlebar::menu_open(window, cx)
        {
            return ReturnDisposition::Invalid;
        }
        if self.painted != Some(place) || !self.native_input_allowed() {
            return ReturnDisposition::Waiting;
        }
        ReturnDisposition::Applied
    }

    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> Point<Pixels> {
        self.scroll.offset()
    }

    #[cfg(test)]
    pub(crate) fn mounted_scroll_geometry(&self) -> (Pixels, Pixels, Option<u64>) {
        (self.scroll.bounds().size.height, self.scroll.max_offset().y, self.scroll_mounted.get())
    }

    #[cfg(test)]
    pub(crate) fn set_scroll_offset(&self, offset: Point<Pixels>) {
        self.scroll.set_offset(offset);
    }

    fn arrive(&mut self, next: &Route, overlay: Option<Overlay>, reading: &crate::navigation::presentation::ReadingVisit) {
        self.scroll_mounted.set(None);
        self.viewport_intent.borrow_mut().take();
        // The first Tree departure records the opened release. A later
        // departure from its returning visit abandons that pending focus,
        // even when no virtual row mounted to schedule a deferred callback.
        if let Some(current) = self.places.last()
            && self.library_state.return_pending_for(&current.route, current.key)
        {
            self.cancel_native_return();
        }
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
        // Settings and Inbox are local pages, not a plate opening over the
        // route they cover. An Open's first frame would paint the old route
        // outside its zero-width plate while native accessibility already
        // exposes the local page. This is especially misleading when the old
        // route is a failed owner read: the failure would remain visible
        // beneath Settings until a later motion frame. Give each local page
        // its own body in the first painted frame, including when another
        // route transition was still in flight.
        let local_page = matches!(overlay, Some(Overlay::Settings(_) | Overlay::Inbox));
        let arrival = if local_page { None } else { self.plan(next, overlay, way) };
        if local_page {
            self.transit = None;
            self.tint = None;
        }
        let hop_forward = arrival.as_ref().is_some_and(|arrival| arrival.verb == Verb::Open);
        self.graph_source = if self.overlay.is_none() { immediate_page_source(&self.route, next) } else { None };
        let from = (self.route.clone(), self.overlay);
        let previous_visit = self.places.last().map_or(reading.id, |place| place.visit);
        self.route = next.clone();
        self.overlay = overlay;
        self.descents = self.descents.wrapping_add(1);
        self.last_way = Some(way);
        self.places.push(Place {
            key: self.descents,
            visit: reading.id,
            route: next.clone(),
            overlay,
            way,
            lens: Lens::Reference,
            from: Some(from),
            // A cut is still a visit from its underlying route. Keep the
            // explicit return marker that `begin(Open)` would have written,
            // so Back closes to that route and restores its saved focus.
            opened: local_page.then_some(None),
            hop: hop_forward,
        });
        if !view_switch {
            self.lens = if overlay.is_none() { reader_lens(reading.presentation.controls().lens) } else { Lens::Reference };
        }
        let (x, y) = if overlay.is_none() { reading.presentation.controls().offset.pixels() } else { (0.0, 0.0) };
        let returning = returned_visit(&mut self.highest_visit, previous_visit, reading.id);
        self.pending_scroll_restore = Some(PendingScrollRestore {
            place: self.descents,
            offset: point(px(x), px(y)),
            origin: (returning || y < 0.0).then_some(ViewportOrigin::Restored(px(y))),
        });
        self.arrival = arrival.map(|arrival| Arrival { key: self.descents, ..arrival });
        self.targets.new_page();
        self.native_return = None;
        self.find_focus_return = None;
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
        if way == Way::View {
            let page_code = matches!((&self.route, next), (Route::Symbol(from), Route::Symbol(to))
                if from.same_place(to)
                    && matches!((from.view, to.view), (View::Page, View::Code) | (View::Code, View::Page)));
            return page_code.then(|| arrival(Verb::Peel, Vec::new(), None));
        }
        if way == Way::GraphPage {
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
            Verb::Peel => {
                let Some((route, _)) = route_of(&self.places, arrival.leaving) else { return };
                let Some(symbol) = route_symbol(&route) else { return };
                let Some((target_route, target_overlay)) = arriving.as_ref() else { return };
                if !RouteDependencies::new(target_route, *target_overlay).content_loaded(self.links.store.read(cx)) {
                    // A source or page still loading must show its honest
                    // loading state; never unroll a skeleton as if it were
                    // the arrived declaration.
                    return;
                }
                let code = if matches!(&route, Route::Symbol(symbol) if symbol.view == View::Code) {
                    Some(route.clone())
                } else {
                    arriving.as_ref().map(|(route, _)| route.clone())
                };
                let source_line = code.as_ref().and_then(|route| {
                    let Route::Symbol(route) = route else { return None };
                    route.line.or_else(|| self.links.store.read(cx).source(&symbol).loaded_value()
                        .and_then(|view| view.declaration.known().map(|span| span.first)))
                });
                let code_row = source_line.and_then(|line| self.targets.bounds_of(source_line_shared_id(line).as_str()));
                let hero = facet::motion::shared::last_bounds(crate::shell::kit::shared_id(&symbol), window, cx);
                let row = if matches!(&route, Route::Symbol(symbol) if symbol.view == View::Code) {
                    code_row.or(hero)
                } else {
                    hero.or(code_row)
                }.filter(shown);
                let Some(row) = row else { return };
                let reversed = live.as_ref().filter(|transit| transit.verb == Verb::Peel
                    && transit.inside == arrival.leaving && route_of(&self.places, transit.outside) == arriving);
                let resumed = live.as_ref().filter(|transit| transit.verb == Verb::Peel
                    && transit.outside == arrival.leaving && route_of(&self.places, transit.inside) == arriving);
                self.transit = Some(if let Some(transit) = reversed.or(resumed) {
                    let mut transit = transit.clone();
                    transit.carry.retarget(if reversed.is_some() { 0.0 } else { 1.0 }, now);
                    if reversed.is_some() { transit.outside = arrival.key; }
                    else { transit.inside = arrival.key; }
                    transit.start = now;
                    transit.scroll = arrival.scroll;
                    transit
                } else {
                    Transit {
                        verb: Verb::Peel,
                        inside: arrival.key,
                        outside: arrival.leaving,
                        row: Some(row),
                        row_id: None,
                        find: None,
                        carry: Carry::new(0.0, 1.0, now),
                        start: now,
                        scroll: arrival.scroll,
                        fold: None,
                        print_after: Duration::ZERO,
                        gem: None,
                        symbol: None,
                    }
                });
            }
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
                        Transit { verb: Verb::Open, inside: arrival.key, find: None, row_id: None, carry, start: now, fold: None, print_after: if transit.row.is_some() { PRINT_AFTER } else { Duration::ZERO }, ..transit.clone() }
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
                        // An edge launch has full height from its first pixel; the plate's
                        // native mask already admits the ready ink as room is uncovered.
                        // Only a physical row must first clear the page's top.
                        print_after: if row.is_some() { PRINT_AFTER } else { Duration::ZERO },
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
                    print_after: if node.is_some() { UNFOLD_PRINT_AFTER } else { Duration::ZERO },
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

    /// The reader's column geometry for one exact place.
    fn layout(&self, route: &Route, overlay: Option<Overlay>, measure: &Measure, facet: &facet::Facet) -> Layout {
        let width = self.core.width();
        // Ask's plate and this preview use one occupied rectangle. Reserve
        // the part of the reader beneath that plate before choosing the
        // folio and margin-note modes.
        let reserved = self.ask_geometry.and_then(|ask| ask.preview_left.map(|left|
            (left - ask.reader_left).max(px(0.0)))).unwrap_or(px(0.0)).min(width);
        let room = facet::fluid::Room::new((width - reserved).max(px(0.0)), measure.scale());
        // The gutters glide from 16 px on a phone to the design's 40; nothing
        // here compares the width with a number.
        let right_pad = READER_PAD.at(room);
        let pad = right_pad.max(reserved);
        let content = (width - pad - right_pad).max(px(0.0));
        let scale = measure.scale();
        let notes_possible = matches!(route, Route::Symbol(route) if route.view == View::Code)
            && overlay.is_none();
        let margin_notes = self.core.modes().settle(&NOTES, room).mode == Notes::Beside;
        let wide = margin_notes && notes_possible;
        let gutter = px(GUTTER * facet.density.space() * scale);
        let margin = px(MARGIN * scale);
        let beside = if wide { margin + gutter } else { px(0.0) };
        // A declaration page (the simple symbol page) lays out its own
        // column and rail from the whole room; every other page reads at the
        // folio's measure.
        let own_width = matches!(route, Route::Symbol(route) if route.view == View::Page) && overlay.is_none();
        let folio = if own_width { content } else { px(FOLIO * scale).min((content - beside).max(px(0.0))) };
        Layout {
            pad,
            right_pad,
            top: READER_TOP.at(room),
            wide_measure: Measure::new(WIDE_FOLIO.at(room).min(content), facet),
            folio,
            beside,
            content,
            wide,
            gutter,
            margin,
            measure: Measure::new((width - reserved).max(px(0.0)), facet),
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let still = facet::motion::still(cx);
        let body = self.body(place, false, snapshot, layout, facet, edge, None, window, cx);
        drop(still);
        let body = gpui::inert(("leaving-page", place.key), format!("Previous page: {}", place_name(&place.route)), body);
        let page = div().absolute().top_0().left_0().right_0().bottom_0().child(a11y_inert(masked(
            mask,
            offset(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(scroll.y)
                    .pl(layout.pad)
                    .pr(layout.right_pad)
                    .pt(layout.top)
                    .child(div().relative().w_full().flex().justify_center().child(body)),
            )
            .x(drift),
        )));
        #[cfg(test)]
        let page = transit_tests::owned_ink(Some(place.key), page);
        page.into_any_element()
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
        let plate_at = |p| match (transit.verb, transit.row) {
            (Verb::Peel, Some(row)) => peel_at(reader, row, p),
            _ => plate_at(reader, column, transit.row, p),
        };
        let plate = plate_at(p);
        let plate_course = Course::of(plate_at, p, transit.carry.target());
        let height = f32::from(reader.size.height);
        let edge = transit.edge_local(now, height).map(|local| {
            let y = reader.top() + px(local);
            match transit.verb {
                Verb::Open | Verb::Unfold => Edge::down(y, px(PRINT_SETTLE * scale), px(PRINT_SPEED * scale * 32.0)),
                Verb::Close | Verb::Fold => Edge::fold(y),
                Verb::Peel => unreachable!("Peel has no print edge"),
            }
        });
        // The gem travels between the hero and the node on the plate's own
        // driver: a Fold's from the hero (p = 1) to the node (p = 0), an
        // Unfold's from the node to where the page lays its gem out.
        let ends = match (transit.verb, transit.gem, transit.row) {
            (Verb::Fold, Some((kind, hero)), Some(node)) => Some((kind, node, hero)),
            (Verb::Unfold, Some((kind, node)), _) => transit
                .symbol
                .as_ref()
                .and_then(|symbol| facet::motion::shared::last_bounds(crate::shell::kit::shared_id(symbol), window, cx))
                .map(|hero| (kind, node, hero)),
            _ => None,
        };
        let gem = ends.map(|(kind, node, hero)| (kind, lerp_rect(node, hero, band(p, LANDED, 0.9))));
        let gem_course = ends.map(|(_, node, hero)| Course::of(|p| lerp_rect(node, hero, band(p, LANDED, 0.9)), p, transit.carry.target()));
        let drift = if graph || transit.verb == Verb::Peel { 0.0 } else { DRIFT * scale };
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
            plate_course,
            gem_course,
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
            right_pad: _,
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

fn place_name(route: &Route) -> String {
    let (name, release) = match route {
        Route::Symbol(symbol) => (
            format!("{}{}", symbol.id.as_str(), if symbol.view == View::Code { " source" } else { "" }),
            symbol.at.as_ref(),
        ),
        Route::Package(package) => (package.package.as_str().to_owned(), package.at.as_ref()),
        Route::Orbit(_) => return "Library".to_owned(),
        Route::World => return "Graph".to_owned(),
        Route::CargoSource(source) => return format!("{} · {}", source.package.as_str(), source.target.path().as_str()),
    };
    release.map_or(name.clone(), |at| format!("{name} @ {}", at.as_str()))
}

/// Stable shared-element key for a one-based source row.
pub(crate) fn source_line_shared_id(line: u32) -> SharedString {
    format!("source-line-{line}").into()
}

/// Drops from the end of `places` every place no frame drew (`painted` is the
/// key of the one the last frame drew as current): a route another one
/// superseded before the reader rendered was never on screen, so it is not a
/// page that can leave. The oldest place stays whether or not it was drawn
/// (there is nothing older). Whether any went.
fn drop_unpainted(places: &mut Vec<Place>, painted: Option<u64>) -> bool {
    let mut dropped = false;
    while places.len() > 1 && places.last().is_some_and(|place| Some(place.key) != painted) {
        places.pop();
        dropped = true;
    }
    dropped
}

/// One page the reader shows (or is still showing on its way out).
fn reader_lens(lens: crate::navigation::presentation::ReaderLens) -> Lens {
    use crate::navigation::presentation::ReaderLens as P;
    match lens { P::Reference => Lens::Reference, P::Relations => Lens::Relations, P::Usage => Lens::Usage, P::History => Lens::History }
}
fn presentation_lens(lens: Lens) -> crate::navigation::presentation::ReaderLens {
    use crate::navigation::presentation::ReaderLens as P;
    match lens { Lens::Reference => P::Reference, Lens::Relations => P::Relations, Lens::Usage => P::Usage, Lens::History => P::History }
}

#[derive(Clone)]
struct Place {
    key: u64,
    visit: crate::navigation::presentation::VisitId,
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

struct SettingsDeparture {
    route: Route,
    root: crate::core::VersionedRoot,
    origin: SettingsNativeOrigin,
    native: NativeFocusDeparture,
}

struct PendingSettingsReturn {
    focus: SettingsReturn,
    origin: SettingsNativeOrigin,
    lease: Option<NativeReturnLease>,
}

#[derive(Clone)]
enum SettingsNativeOrigin {
    Target(SharedString),
    Find(FocusHandle),
    Graph(bodies::graph::SettingsGraphOrigin),
    Unmatched,
}

struct SettingsReturn {
    place: u64,
    root: crate::core::VersionedRoot,
    target: Option<SharedString>,
}

impl SettingsReturn {
    fn has_same_authority(&self, root: crate::core::VersionedRoot) -> bool {
        self.root.same_authority(root)
    }
}

#[cfg(test)]
mod settings_return_tests {
    use super::SettingsReturn;
    use crate::core::VersionedRoot;
    use gpui::SharedString;

    fn root(tag: &str, epoch: u64, sequence: u64, observation: u64) -> VersionedRoot {
        let digest = backend_library::view_state_root(&[("settings".to_owned(), tag.to_owned())]);
        VersionedRoot::from_revision(
            epoch,
            backend_library::Cursor::at(digest, sequence),
            observation,
        )
    }

    #[test]
    fn settings_return_survives_observation_only_updates_and_rejects_other_authority() {
        let captured = root("same", 2, 8, 1);
        let return_to = SettingsReturn {
            place: 17,
            root: captured,
            target: Some(SharedString::from("graph-target")),
        };
        assert!(return_to.has_same_authority(root("same", 2, 8, 9)));
        assert!(!return_to.has_same_authority(root("different", 2, 8, 9)));
        assert!(!return_to.has_same_authority(root("same", 3, 8, 9)));
        assert!(!return_to.has_same_authority(root("same", 2, 9, 9)));
    }
}

const MAX_SOURCE_PAGING_MEMORY: usize = 32;
const MAX_LIBRARY_STATE_MEMORY: usize = 8;

#[derive(Default)]
struct LibraryStateMemory {
    entries: Vec<LibraryStateEntry>,
}

struct LibraryStateEntry {
    route: Route,
    revision: Option<crate::core::VersionedRoot>,
    attachment: Option<OwnerAttachment>,
    state: Rc<RefCell<facet::browse::library::State>>,
}

impl LibraryStateMemory {
    fn return_pending_for(&self, route: &Route, place_key: u64) -> bool {
        self.entries.iter().any(|entry| {
            entry.route == *route && entry.state.borrow().pending_return_for(place_key)
        })
    }

    fn for_route(
        &mut self,
        route: &Route,
        revision: Option<crate::core::VersionedRoot>,
        attachment: Option<OwnerAttachment>,
        active: bool,
    ) -> Rc<RefCell<facet::browse::library::State>> {
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.route == *route && match (entry.revision, revision) {
                (Some(earlier), Some(current)) => earlier.same_authority(current),
                (None, None) => true,
                _ => false,
            }
        })
        {
            // Observation is diagnostic metadata. Local scroll/disclosure
            // follows producer authority; action intent remains attachment-
            // and stamp-gated by the current Reader visit.
            self.entries[index].revision = revision;
            if self.entries[index].attachment != attachment {
                // Scroll and disclosure are local reading state. Only the
                // actionable return intent is tied to the old attachment.
                self.entries[index].state.borrow_mut().cancel_return();
                self.entries[index].attachment = attachment;
            }
            if active {
                let entry = self.entries.remove(index);
                let state = Rc::clone(&entry.state);
                self.entries.push(entry);
                return state;
            }
            return Rc::clone(&self.entries[index].state);
        }
        let state = Rc::new(RefCell::new(facet::browse::library::State::default()));
        if active {
            self.entries.push(LibraryStateEntry {
                route: route.clone(),
                revision,
                attachment,
                state: Rc::clone(&state),
            });
            if self.entries.len() > MAX_LIBRARY_STATE_MEMORY {
                self.entries.remove(0);
            }
        }
        state
    }
}

#[cfg(test)]
mod library_state_memory_tests {
    use super::{LibraryStateMemory, MAX_LIBRARY_STATE_MEMORY};
    use crate::core::{LocalProjectId, VersionedRoot};
    use crate::navigation::{BrowseRoute, OrbitRoute, Route};
    use std::path::Path;
    use std::rc::Rc;

    fn tree(name: &str) -> Route {
        let project = LocalProjectId::from_path(Path::new(name)).expect("tree identity");
        Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project)))
    }

    #[test]
    fn library_state_survives_observation_bumps_but_not_tree_authority() {
        let mut memory = LibraryStateMemory::default();
        let route = tree("/tmp/library-memory-one");
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("library".to_owned(), "same".to_owned())]), 7,
        );
        let changed = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("library".to_owned(), "changed".to_owned())]), 7,
        );
        let first = memory.for_route(&route, Some(root), None, true);
        assert!(Rc::ptr_eq(
            &first,
            &memory.for_route(&route, Some(root), None, true)
        ));
        for observation in 1..=16 {
            let observed = root.observed_at(observation);
            assert!(Rc::ptr_eq(&first, &memory.for_route(&route, Some(observed), None, true)));
            assert_eq!(memory.entries.len(), 1, "diagnostic observations cannot multiply neutral cache entries");
            assert_eq!(memory.entries[0].revision, Some(observed));
        }
        assert!(!Rc::ptr_eq(
            &first,
            &memory.for_route(&route, Some(changed), None, true)
        ));
        assert!(!Rc::ptr_eq(
            &first,
            &memory.for_route(&tree("/tmp/library-memory-two"), Some(root), None, true)
        ));
    }

    #[test]
    fn leaving_tree_does_not_admit_state_and_old_trees_are_evicted() {
        let mut memory = LibraryStateMemory::default();
        let first_route = tree("/tmp/library-memory-first");
        let transient = memory.for_route(&first_route, None, None, false);
        let first = memory.for_route(&first_route, None, None, true);
        assert!(!Rc::ptr_eq(&transient, &first));
        for at in 0..MAX_LIBRARY_STATE_MEMORY {
            memory.for_route(
                &tree(&format!("/tmp/library-memory-{at}")),
                None,
                None,
                true,
            );
        }
        assert_eq!(memory.entries.len(), MAX_LIBRARY_STATE_MEMORY);
        assert!(!Rc::ptr_eq(
            &first,
            &memory.for_route(&first_route, None, None, true)
        ));
    }
}

/// Source pager identity. Cargo file contents can change without advancing
/// the indexed view root, so their independent owner-verified digest is used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceGeneration {
    Indexed(crate::core::VersionedRoot),
    Cargo([u8; 32]),
    /// Display-only cursor memory, never a present source action lease.
    Retained([u8; 32]),
}

#[derive(Default)]
struct SourcePagingMemory {
    entries: Vec<SourcePagingEntry>,
}

impl SourcePagingMemory {
    fn for_route(
        &mut self,
        route: &Route,
        revision: Option<SourceGeneration>,
        active: bool,
    ) -> Rc<RefCell<Option<bodies::PagingState>>> {
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.route == *route && entry.revision == revision
        }) {
            // The leaving page can still paint, but cannot reorder the current page's recency.
            if active {
                let entry = self.entries.remove(index);
                let state = Rc::clone(&entry.state);
                self.entries.push(entry);
                return state;
            }
            return Rc::clone(&self.entries[index].state);
        }
        let state = Rc::new(RefCell::new(None));
        if active {
            self.entries.push(SourcePagingEntry {
                route: route.clone(),
                revision,
                state: Rc::clone(&state),
            });
            if self.entries.len() > MAX_SOURCE_PAGING_MEMORY {
                self.entries.remove(0);
            }
        }
        state
    }
}

struct SourcePagingEntry {
    route: Route,
    revision: Option<SourceGeneration>,
    state: Rc<RefCell<Option<bodies::PagingState>>>,
}

#[cfg(test)]
mod source_paging_memory_tests {
    use super::{SourceGeneration, SourcePagingMemory, MAX_SOURCE_PAGING_MEMORY};
    use crate::core::VersionedRoot;
    use crate::navigation::{Route, View};
    use crate::shell::tests::view_route;
    use std::rc::Rc;

    #[test]
    fn exact_source_route_and_owner_revision_retain_only_their_own_page_state() {
        let mut memory = SourcePagingMemory::default();
        let route = view_route("RelationLabel", View::Code);
        let first_root = VersionedRoot::unserved();
        let changed_root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("paging".to_owned(), "replacement".to_owned())]),
            5,
        );
        let saved = memory.for_route(&route, Some(SourceGeneration::Indexed(first_root)), true);
        let revisit = memory.for_route(&route, Some(SourceGeneration::Indexed(first_root)), true);
        assert!(Rc::ptr_eq(&saved, &revisit));
        assert!(!Rc::ptr_eq(&saved, &memory.for_route(&route, Some(SourceGeneration::Indexed(changed_root)), true)));

        let Route::Symbol(mut different_line) = route.clone() else { unreachable!() };
        different_line.line = Some(42);
        assert!(!Rc::ptr_eq(
            &saved,
            &memory.for_route(&Route::Symbol(different_line), Some(SourceGeneration::Indexed(first_root)), true),
        ));
    }

    #[test]
    fn cargo_file_pager_follows_verified_content_not_index_root() {
        use crate::navigation::{CargoSourcePath, CargoSourceRoute};
        let mut memory = SourcePagingMemory::default();
        let package = crate::core::PackageId::new(
            "pkg:cargo/demo@1.0.0?cargo-authority=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ).expect("authority address");
        let project = crate::core::LocalProjectId::new("/tmp/nudox-cargo-reader").expect("tree address");
        let file = CargoSourcePath::new("src/lib.rs").expect("file");
        let route = Route::CargoSource(CargoSourceRoute::new(crate::navigation::cargo_browse::fixture_context(project), package, file, Some(77)).expect("route"));
        let original = memory.for_route(&route, Some(SourceGeneration::Cargo([1; 32])), true);
        assert!(Rc::ptr_eq(&original, &memory.for_route(&route, Some(SourceGeneration::Cargo([1; 32])), true)));
        assert!(!Rc::ptr_eq(&original, &memory.for_route(&route, Some(SourceGeneration::Cargo([2; 32])), true)));
    }

    #[test]
    fn leaving_pages_do_not_admit_entries_and_long_sessions_evict_old_pages() {
        let mut memory = SourcePagingMemory::default();
        let route = view_route("RelationLabel", View::Code);
        let transient = memory.for_route(&route, None, false);
        assert_eq!(memory.entries.len(), 0);
        let saved = memory.for_route(&route, None, true);
        assert!(!Rc::ptr_eq(&transient, &saved));
        for line in 1..=MAX_SOURCE_PAGING_MEMORY as u32 {
            let Route::Symbol(mut different_line) = route.clone() else { unreachable!() };
            different_line.line = Some(line);
            memory.for_route(&Route::Symbol(different_line), None, true);
        }
        assert_eq!(memory.entries.len(), MAX_SOURCE_PAGING_MEMORY);
        assert!(!Rc::ptr_eq(&saved, &memory.for_route(&route, None, true)));
    }
}

/// The reader's column geometry for one frame.
#[derive(Clone, Copy)]
struct Layout {
    /// The scroller's left and right padding, then top padding.
    pad: Pixels,
    right_pad: Pixels,
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

    fn measure_frame(&mut self, frame: Bounds<Pixels>) {
        // Both Reveal's ordinary scroll container and its graph container
        // fill this embedding. Content padding, Ask's reservation and a pin
        // beside Reader affect its contents or embedding, never this viewport.
        // Publish before render: a resize must stage the plate against this
        // frame, rather than the previous Reveal prepaint.
        self.frame.set(Some(frame));
    }

    fn keys(&self, snapshot: &AppSnapshot) -> Vec<PageKey> {
        reader_keys(snapshot)
    }

    fn observe(&mut self, event: &StoreEvent, store: &DataStore) {
        if event.is_branch(Branch::Route) || event.is_branch(Branch::Overlay) || event.is_branch(Branch::Reading) {
            let snapshot = store.snapshot();
            let overlay = snapshot.page_overlay();
            self.targets.bind_reading(snapshot.route().clone(), snapshot.session().reading.current.id, snapshot.session().reading.current.presentation.controls().focus.clone());
            if overlay.is_none() { self.lens = reader_lens(snapshot.session().reading.current.presentation.controls().lens); }
            if *snapshot.route() != self.route || overlay != self.overlay
                || self.places.last().is_some_and(|place| place.visit != snapshot.session().reading.current.id) {
                if overlay == self.overlay && self.places.last().is_some_and(|place| place.visit == snapshot.session().reading.current.id) && (find_refinement(&self.route, snapshot.route()) || cargo_binding_refinement(&self.route, snapshot.route())) {
                    // Typing refines one place. Keeping its keyed surface
                    // holds the live input and selection while results reflow.
                    self.route = snapshot.route().clone();
                    if let Some(current) = self.places.last_mut() { current.route = self.route.clone(); }
                    return;
                }
                // (A release change or a view switch replaces the entry; it
                // still arrives as a new page.)
                let opening_settings = !matches!(self.overlay, Some(Overlay::Settings(_)))
                    && matches!(overlay, Some(Overlay::Settings(_)));
                let closing_settings = matches!(self.overlay, Some(Overlay::Settings(_)))
                    && !matches!(overlay, Some(Overlay::Settings(_)));
                if opening_settings {
                    self.settings_departure = self.painted_root
                        .filter(|_| self.places.last().is_some_and(|place| Some(place.key) == self.painted))
                        .map(|root| SettingsDeparture {
                            route: self.route.clone(),
                            root,
                            origin: self.settings_native_origin.take().unwrap_or(SettingsNativeOrigin::Unmatched),
                            native: self.targets.take_native_departure(),
                        });
                }
                let departure = if closing_settings { self.settings_departure.take() } else { None };
                self.arrive(snapshot.route(), overlay, &snapshot.session().reading.current);
                self.pending_settings_focus = if closing_settings && overlay.is_none() {
                    let origin = departure.filter(|departure| {
                        departure.route == *snapshot.route()
                            && departure.root.same_authority(snapshot.key())
                    }).map_or(SettingsNativeOrigin::Unmatched, |departure| departure.origin);
                    Some(PendingSettingsReturn { focus: SettingsReturn {
                        place: self.descents,
                        root: snapshot.key(),
                        target: None,
                    }, origin, lease: None })
                } else {
                    None
                };
            }
        }
    }
}

fn cargo_binding_refinement(previous: &Route, next: &Route) -> bool {
    let (Route::CargoSource(previous), Route::CargoSource(next)) = (previous, next) else { return false };
    next.browse.context().cloned().and_then(|context| previous.resolve_context(context)).as_ref() == Some(next)
}

pub(super) fn find_refinement(previous: &Route, next: &Route) -> bool {
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
    place_keys(snapshot.route(), snapshot.page_overlay())
}

impl Reader {
    /// The page cannot claim native input while its arrival still lacks a
    /// measured frame or any of its departure/arrival plate is in flight.
    /// A Find field may still focus before its read completes, so resource
    /// readiness and `painted` are intentionally separate from this gate.
    pub(crate) fn native_motion_settled(&self) -> bool {
        self.arrival.is_none() && self.transit.is_none()
    }

    pub(crate) fn native_input_allowed(&self) -> bool {
        self.ask_background_input_allowed && self.native_motion_settled()
    }

    pub(crate) fn native_input_for(&self, route: &Route, overlay: Option<Overlay>) -> bool {
        self.native_input_allowed() && self.route == *route && self.overlay == overlay
            && self.places.last().is_some_and(|place| self.painted == Some(place.key))
    }

    /// Page/Code/Graph controls belong to the settled, actually painted page.
    /// A closing Settings or Inbox plate still blocks the underlay until its
    /// replacement owns a painted frame, even after the snapshot has cleared
    /// the modal overlay.
    pub(crate) fn local_navigation_allowed(&self, cx: &App) -> bool {
        let store = self.links.store.read(cx);
        let snapshot = store.snapshot();
        snapshot.overlay().is_none()
            && snapshot.page_overlay().is_none()
            && self.native_input_for(snapshot.route(), None)
    }

    pub(crate) fn mode_input_allowed(&self, cx: &App) -> bool {
        let store = self.links.store.read(cx);
        let snapshot = store.snapshot();
        self.local_navigation_allowed(cx)
            && store.current_owner_attachment().is_some()
            && (!bodies::graph::is_graph(snapshot.route())
                || (self.painted_root.is_some_and(|root| root.same_authority(snapshot.key()))
                    && self.painted_graph.as_ref().is_some_and(|(map_id, presentation, owner)| {
                        store.admits_owner_attachment(owner)
                            && self.map.as_ref().is_some_and(|map| map.entity_id() == *map_id
                                && map.read(cx).mounted_presentation(cx) == Some(*presentation))
                    })))
    }

    pub(crate) fn current_place_key(&self) -> Option<u64> {
        self.places.last().map(|place| place.key)
    }

    pub(crate) fn set_ask_scene(&mut self, geometry: Option<super::frame::AskGeometry>, background_input_allowed: bool, cx: &mut Context<Self>) {
        if self.ask_geometry != geometry || self.ask_background_input_allowed != background_input_allowed {
            self.ask_geometry = geometry;
            self.ask_background_input_allowed = background_input_allowed;
            cx.notify();
        }
    }

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
        retained_pages: Option<&Pages>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let palette = facet.palette();
        let dependencies = RouteDependencies::new(&place.route, place.overlay);
        let pages = retained_pages.cloned().unwrap_or_else(|| Pages::gather(self.links.store.read(cx), &dependencies));
        let links = self.links.clone();
        let targets = if current { self.targets.clone() } else { Targets::default() };
        let mut said = Vec::new();
        let mut hero = Vec::new();
        let mut scratch_hover = HoverIntent::default();
        let mut hover = if current { std::mem::take(&mut self.hover) } else { HoverIntent::default() };
        let source_generation = match &place.route {
            Route::CargoSource(_) => dependencies.cargo().and_then(|cargo| {
                let store = self.links.store.read(cx);
                let resource = store.cargo_source(&cargo.file);
                (store.cargo_read_admission(&PageKey::CargoSource(cargo.file.clone()), &resource)
                    == CargoReadAdmission::Current)
                    .then(|| resource.loaded_value().map(|page| SourceGeneration::Cargo(page.content_digest)))
                    .flatten()
            }),
            _ => route_symbol(&place.route).and_then(|symbol| pages.source(&symbol).value_root().map(SourceGeneration::Indexed)),
        };
        // Saved text partitions only the local pager's memory. It never
        // enters Ctx's current source generation or native action guards.
        let paging_generation = source_generation.or_else(|| bodies::saved_source_generation(
            self.links.store.read(cx), &place.route, place.overlay,
        ).map(SourceGeneration::Retained));
        let source_paging = if matches!(&place.route, Route::Symbol(symbol) if symbol.view == View::Code)
            || matches!(&place.route, Route::CargoSource(_) | Route::Package(_))
            || matches!(paging_generation, Some(SourceGeneration::Retained(_))) {
            self.source_paging.for_route(&place.route, paging_generation, current)
        } else {
            Rc::clone(&self.empty_source_paging)
        };
        let library_state = match &place.route {
            Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project))) => {
                let revision = pages
                    .browse(&crate::model::browse::BrowseKey::Tree(project.clone()))
                    .value_root();
                let attachment = self.links.store.read(cx).current_owner_attachment();
                self.library_state
                    .for_route(&place.route, revision, attachment, current)
            }
            _ => Rc::new(RefCell::new(facet::browse::library::State::default())),
        };
        let leaves = {
            let symbol_disclosure = route_symbol(&place.route).map(|symbol| self.symbol_disclosure(&symbol)).unwrap_or_default();
            let find_state = if matches!(place.route, Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome | BrowseRoute::Find(_)))) {
                // The edit belongs to the visit, independently of producer
                // replacement. Its mounted/root receipt is refreshed only by
                // the current frame's callback, so retaining text grants no
                // old authority permission to return focus or invoke results.
                if current && self.mounted_find_query.as_ref().is_none_or(|mounted|
                    mounted.visit != place.visit) {
                    self.mounted_find_query = Some(MountedFindQuery {
                        visit: place.visit, state: Default::default(), place: place.key,
                        route: place.route.clone(), root: snapshot.key(), focus: None,
                    });
                }
                self.mounted_find_query.as_ref().filter(|mounted| mounted.visit == place.visit)
                    .map(|mounted| mounted.state.clone()).unwrap_or_default()
            } else {
                if current && place.overlay.is_none() { self.mounted_find_query = None; }
                Default::default()
            };
            let mut ctx = Ctx {
                reader: cx.weak_entity(),
                active: current,
                native_input_active: current && self.native_input_allowed(),
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
                reader_reveal: Rc::clone(&self.reveal),
                source_focus_applied: Rc::clone(&self.source_focus_applied),
                place_key: place.key,
                native_return_interruption: self.native_return_interruption,
                source_generation,
                source_paging,
                library_state,
                readme_paging: bodies::ReadmePagingScope::new(
                    window.window_handle().window_id(),
                    Rc::clone(&self.readme_paging),
                ),
                lens: if current { self.lens } else { place.lens },
                said: &mut said,
                hero: &mut hero,
                symbol_disclosure,
                package_outline_expanded: self.package_outline_expanded,
                find_held: self.find_held.clone(),
                find_state,
                // A hop forward from another declaration: it is ringed on this page.
                arrived_from: place.hop
                    .then(|| place.from.as_ref().and_then(|(route, _)| route_symbol(route)))
                    .flatten()
                    .filter(|from| route_symbol(&place.route).as_ref() != Some(from)),
            };
            let hover = if current { &mut hover } else { &mut scratch_hover };
            bodies::build(&place.route, place.overlay, snapshot, &pages, &mut ctx, hover, window, cx)
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
    RouteDependencies::new(route, overlay).into_keys()
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
        let reflowed = self.laid_out.is_some_and(|last| last != laid_out) && self.targets.focused().is_some();
        if reflowed && self.viewport_intent.borrow().is_none() {
            // A reflow follows the target only when the user has not chosen
            // an explicit viewport for this upcoming layout.
            self.reveal.set(true);
        }
        self.laid_out = Some(laid_out);
        let palette = facet.palette();
        let snapshot = self.links.snapshot(cx);
        if self.find_workspace.as_ref() != snapshot.workspace().host.as_ref()
            || self.find_held_root.is_some_and(|held| !held.same_authority(snapshot.key())) {
            self.find_workspace = snapshot.workspace().host.clone();
            self.find_held.clear();
            self.find_held_root = None;
        }
        let Some(requested) = self.places.last().cloned() else { return div(); };
        let destination = bodies::reading_destination(self.links.store.read(cx), &requested.route, requested.overlay);
        let waiting = destination.pending();
        // A read has not painted its destination yet. Keep the last actual
        // departure as the one presentation until the real terminal answer;
        // its existing motion may finish, but no new empty plate grows.
        let retained_departure = waiting.then(|| self.arrival.as_ref()).flatten()
            .and_then(|arrival| self.places.iter().find(|place| place.key == arrival.leaving))
            .cloned();
        let retaining_departure = retained_departure.is_some();
        let current = retained_departure.unwrap_or_else(|| requested.clone());
        if waiting {
            if let Some(target) = self.targets.focused() {
                self.pending_page_focus = Some(SettingsReturn { place: requested.key, root: snapshot.key(), target: Some(target) });
                self.targets.clear_focus();
            }
        }
        if self.pending_page_focus.as_ref().is_some_and(|focus| focus.place != requested.key || !focus.root.same_authority(snapshot.key())) {
            self.pending_page_focus = None;
        }
        let on_graph = !mounts_scroll_body(self.links.store.read(cx), &current.route, current.overlay);
        let layout = self.layout(&current.route, current.overlay, &measure, &facet);
        let Layout { pad, right_pad, top, folio, beside, content, .. } = layout;
        let scale = measure.scale();
        let mut restored_origin = None;
        // Local scroll restoration belongs to a displayed destination,
        // including its admitted saved text. It grants no current source
        // actions; those still require the separate owner read admission.
        if !waiting && destination.displayed() {
            if let Some(restore) = self.pending_scroll_restore.take() {
                if restore.place == current.key {
                    self.scroll.set_offset(restore.offset);
                    restored_origin = restore.origin;
                } else {
                    self.pending_scroll_restore = Some(restore);
                }
            }
        } else if !waiting && self
            .pending_scroll_restore
            .as_ref()
            .is_some_and(|restore| restore.place != current.key)
        {
            self.pending_scroll_restore = None;
        }
        // The place change in flight, this frame (window space).
        let reader = self.frame.get();
        // Arrival is a prepared route-owned change. Its spring starts only
        // once a real destination can paint; readiness has no wall-clock
        // timeout and does not request motion frames while it is pending.
        // Check the frame before consuming it, including after a resize.
        let can_begin = !waiting;
        if can_begin && let Some(reader) = reader
            && let Some(arrival) = self.arrival.take()
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
        let keep = |key: u64| key == current.key || key == requested.key
            || self.arrival.as_ref().is_some_and(|arrival| arrival.leaving == key)
            || transit.as_ref().is_some_and(|t| t.inside == key || t.outside == key);
        self.places.retain(|place| keep(place.key));
        // The page drawn away from the scroller this frame (the one leaving
        // an Open, the one folding on a Close's or a Fold's plate).
        let leaving = transit.as_ref().and_then(|transit| {
            let key = match transit.verb {
                Verb::Open => transit.outside,
                Verb::Close | Verb::Fold => transit.inside,
                Verb::Unfold => return None,
                Verb::Peel => if current.key == transit.inside { transit.outside } else { transit.inside },
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
            map.update(cx, |map, cx| map.show(&current.route, source.as_ref(), window, cx));
            self.said = vec!["Graph fixture · pages resolve through your local index".into()];
            self.hero.clear();
            let tint = self.tint_now(cx);
            self.publish(staged, tint.map(|(_, strength)| strength), cx);
            let framed = Reveal {
                pending: Rc::new(Cell::new(false)),
                viewport_intent: Rc::new(RefCell::new(None)),
                viewport_admitted: false,
                content_layout: Rc::new(Cell::new(None)),
                targets: self.targets.clone(),
                scroll: self.scroll.clone(),
                frame: Rc::clone(&self.frame),
                land: Vec::new(),
                reading: None,
                scroll_mount: None,
                native_return: None,
                child: div().size_full().child(map.clone()).into_any_element(),
            };
            let framed = if retaining_departure {
                gpui::inert("pending-map", "Previous graph while the destination opens", framed).into_any_element()
            } else { framed.into_any_element() };
            #[cfg(test)]
            let framed = transit_tests::owned_ink(None, framed);
            let mut root = div().relative().size_full()
                .text_color(palette.ink1.hsla()).font_family(facet::fonts::family(ty::BODY));
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
                    let page = self.still_page(&leaving, transit.scroll, staged.plate, staged.edge, Pixels::ZERO, &snapshot, &layout, &facet, window, cx);
                    root = root
                        .child(plate_ground(&staged))
                        .child(div().id("folding-plate").absolute().top_0().left_0().size_full().child(page))
                        .children(gem(&staged));
                }
                _ => root = root.child(framed),
            }
            // A sheet (including an unreadably narrow transitional panel)
            // leaves no Reader pixels. Keep the map and route state above
            // current, but do not mount descendants under a zero-opacity
            // wrapper: they still enter the painted-text probe.
            return if self.ask_geometry.is_some_and(|ask| ask.preview_left.is_none()) {
                self.painted = None;
                self.painted_root = None;
                self.painted_graph = None;
                div().size_full()
            } else {
                // The graph branch does not run the ordinary page body below.
                // Record the visible exact Map presentation for this place.
                // A first immutable scene mount qualifies next frame; an exact
                // declaration terminal preserves its separate Page/Code lease.
                let presentation = map.read(cx).mounted_presentation(cx);
                self.painted_graph = presentation
                    .zip(self.links.store.read(cx).current_owner_attachment())
                    .map(|(presentation, owner)| (map.entity_id(), presentation, owner));
                // Local input/cover return belongs to the actual painted
                // destination, including an immutable retained scene. Only
                // resource header actions consume painted_graph's owner.
                self.painted = presentation.map(|_| current.key);
                self.painted_root = presentation.map(|_| snapshot.key());
                if retaining_departure { root = root.child(opening_status(palette)); }
                root
            };
        }
        self.painted_graph = None;
        if let Some(map) = &self.map { map.update(cx, |map, cx| map.suspend(window, cx)); }

        // The current page, in the scroller: inside the plate when it opens
        // or unfolds, outside it (above) when the plate closes over it.
        let current_edge = staged.filter(|staged| matches!(staged.verb, Verb::Open | Verb::Unfold)).and_then(|staged| staged.edge);
        // The Library's ring is the live ring only on the Library: away from
        // it, the ring forgets where its names were, so they stand where they
        // lay out when it comes back (never flying from a place last seen
        // before the page left).
        let on_the_library = !matches!(current.overlay, Some(Overlay::Settings(_) | Overlay::Inbox)) && matches!(&current.route, Route::Orbit(orbit) if !matches!(orbit, crate::navigation::OrbitRoute::Browse(_)));
        if !on_the_library {
            self.ring_flow.forget(cx);
        }
        // Gather for this exact typed destination even while an owner is
        // starting, failed, or replacing its root. Bodies decide whether a
        // retained value has a valid embedded identity for read-only paint.
        let body = self.body(&current, !retaining_departure, &snapshot, &layout, &facet, current_edge,
            None, window, cx);
        self.painted = Some(current.key);
        self.painted_root = Some(snapshot.key());
        if !waiting && staged.is_some() && self.pending_page_focus.is_some() {
            if self.pending_page_focus.is_none() && let Some(target) = self.targets.focused() {
                self.pending_page_focus = Some(SettingsReturn { place: current.key, root: snapshot.key(), target: Some(target) });
            }
            self.targets.clear_focus();
        }
        if !waiting && staged.is_none() && self.native_input_allowed()
            && let Some(focus) = self.pending_page_focus.take()
            && focus.place == current.key && focus.root.same_authority(snapshot.key())
            && let Some(target) = focus.target
        {
            self.targets.focus(target);
            if self.targets.current().is_none() { self.targets.clear_focus(); }
            else { self.reveal.set(true); cx.notify(); }
        }
        self.targets.finish_native();
        if let Some(pending) = self.pending_settings_focus.take()
            && pending.focus.place == current.key
        {
            let lease_current = pending.lease.map(|lease| {
                self.links.shell.upgrade().is_some_and(|shell| {
                    let shell = shell.read(cx);
                    lease.current(window.window_handle().window_id(), shell.focus_return_generation(), window.focus_epoch())
                        && shell.allows_reader_native_return(window)
                        && !super::titlebar::menu_open(window, cx)
                })
            });
            if lease_current == Some(false) {
                // A newer input choice wins even if this page is still landing.
            } else if lease_current == Some(true) && self.painted == Some(current.key) && self.native_input_allowed() {
                if pending.focus.has_same_authority(snapshot.key()) {
                    match pending.origin.clone() {
                        SettingsNativeOrigin::Target(target) if self.targets.is_active() => {
                            if self.targets.focus_native(&target, window, cx) {
                                self.targets.focus(target);
                                self.reveal.set(true);
                                cx.notify();
                            }
                        }
                        SettingsNativeOrigin::Find(_) => {
                            // Its existing after-frame callback registers and
                            // restores the retained native input after AX paint.
                            self.pending_settings_focus = Some(pending);
                        }
                        _ => { self.pending_page_focus = None; self.targets.clear_focus(); }
                    }
                }
            } else {
                self.pending_settings_focus = Some(pending);
            }
        }
        if let Some(reader) = reader {
            self.follow(reader, cx);
        }
        let drift = staged.map_or(Pixels::ZERO, |staged| match staged.verb {
            Verb::Open | Verb::Unfold => staged.inside_drift,
            Verb::Close | Verb::Fold => staged.outside_drift,
            Verb::Peel => Pixels::ZERO,
        });
        let stack = div()
            .relative()
            .w_full()
            .flex()
            .justify_center()
            .child(offset(div().id(("place", current.key)).child(body)).x(drift));

        let glow = self.targets.glow(&measure);
        // Focus is parked while the destination is pending. Settle its old
        // geometry beneath the same native still scope as the retained body.
        let glow = if waiting {
            gpui::inert("waiting-focus-glow", "Focus resumes when the destination arrives", glow).into_any_element()
        } else { glow.into_any_element() };
        if let Some(origin) = restored_origin {
            self.viewport_intent.borrow_mut().replace(ViewportIntent {
                place: current.key,
                origin,
                from: self.scroll.offset(),
                focused: self.targets.focused(),
                commands: Vec::new(),
            });
        }
        let content_layout = Rc::new(Cell::new(None));
        let content = ScrollContent {
            layout: Rc::clone(&content_layout),
            child: div().w_full().pl(pad).pr(right_pad).pt(top).pb(px(96.0 * scale))
                .child(stack).into_any_element(),
        };
        let scroller = div()
            .id("reader-scroll")
            .debug_selector(|| "reader-scroll".to_owned())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .key_context(super::keys::READER_VIEWPORT)
            .child(content);
        // A reflow that scrolls to keep the focus in view lands the page's
        // moving parts: a flight from where they were would carry the focus
        // back off screen.
        let land = if reflowed {
            let mut flows = vec![self.ring_flow.clone()];
            if let Some(symbol) = route_symbol(&current.route) {
                flows.push(self.symbol_disclosure(&symbol).flow);
            }
            flows
        } else {
            Vec::new()
        };
        let scroller = facet::probe::scroll_scope("reader-scroll", Reveal {
            pending: Rc::clone(&self.reveal),
            viewport_intent: Rc::clone(&self.viewport_intent),
            viewport_admitted: !waiting && self.pending_scroll_restore.is_none()
                && self.native_input_for(snapshot.route(), snapshot.page_overlay()),
            content_layout,
            targets: self.targets.clone(),
            scroll: self.scroll.clone(),
            frame: Rc::clone(&self.frame),
            land,
            reading: (!waiting && self.pending_scroll_restore.is_none() && self.native_input_for(snapshot.route(), snapshot.page_overlay()) && snapshot.page_overlay().is_none())
                .then(|| (snapshot.session().reading.current.id, snapshot.session().reading.current.presentation.controls().offset, self.links.clone())),
            scroll_mount: Some((Rc::clone(&self.scroll_mounted), current.key)),
            native_return: self.native_return.as_ref().map(|pending| (cx.weak_entity(), Rc::clone(&pending.ticket))),
            child: scroller.into_any_element(),
        });
        let scroller = facet::motion::flow::local_paint("reader-flow-paint", scroller);
        let scroller = if retaining_departure {
            gpui::inert("pending-reader", "Previous page while the destination opens", scroller).into_any_element()
        } else { scroller.into_any_element() };
        #[cfg(test)]
        let scroller = transit_tests::owned_ink(Some(current.key), scroller).into_any_element();
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
            (Some(staged), Some(transit), Some(leaving)) if staged.verb == Verb::Peel => {
                let frame = reader.unwrap_or(staged.plate);
                let leaving_layout = self.layout(&leaving.route, leaving.overlay, &measure, &facet);
                if current.key == transit.inside {
                    // Entering Code or Page: the old view remains outside the
                    // unrolling strip. Its pixels are covered by one opaque
                    // plate before the arriving text is drawn inside it.
                    let old = self.still_page(&leaving, transit.scroll, frame, None, Pixels::ZERO, &snapshot, &leaving_layout, &facet, window, cx);
                    root = root.child(old).child(plate_ground(&staged)).child(masked(staged.plate, scroller));
                } else {
                    // A mid-flight Back contracts the same strip from its
                    // painted position and velocity; the newly current view
                    // is already visible around it.
                    let old = self.still_page(&leaving, transit.scroll, staged.plate, None, Pixels::ZERO, &snapshot, &leaving_layout, &facet, window, cx);
                    root = root.child(scroller).child(plate_ground(&staged)).child(old);
                }
            }
            (Some(staged), Some(transit), Some(leaving)) if staged.verb == Verb::Open => {
                // The old page stays where it was, drifting left, cut to what
                // the plate has not reached; the new page is on the plate.
                for (index, mask) in staged.outside.into_iter().enumerate() {
                    if mask.size.height > Pixels::ZERO && mask.size.width > Pixels::ZERO {
                        let page = self.still_page(&leaving, transit.scroll, mask, None, staged.outside_drift, &snapshot, &layout, &facet, window, cx);
                        root = root.child(div().id(("leaving", index)).absolute().top_0().left_0().size_full().child(page));
                    }
                }
                root = root.child(plate_ground(&staged)).child(masked(staged.plate, scroller));
            }
            (Some(staged), Some(transit), Some(leaving)) if staged.verb == Verb::Close => {
                // One live parent layout is uncovered beneath the old page.
                // Its local Flow draws finish before this later plate overlay;
                // resize cannot assemble a second, differently sampled parent.
                root = root.child(scroller);
                let occupied = staged.edge.map_or(staged.plate, |edge| {
                    let bottom = edge.y.max(staged.plate.top()).min(staged.plate.bottom());
                    Bounds::from_corners(staged.plate.origin, point(staged.plate.right(), bottom))
                });
                if occupied.size.height > Pixels::ZERO && occupied.size.width > Pixels::ZERO {
                    root = root.child(masked(occupied, plate_ground(&staged)));
                }
                let page = self.still_page(&leaving, transit.scroll, staged.plate,
                    staged.edge, staged.inside_drift, &snapshot, &layout, &facet, window, cx);
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
                    let graph = div().size_full().child(map.clone());
                    #[cfg(test)]
                    let graph = transit_tests::owned_ink(None, graph);
                    root = root.child(div().id("unfolding-map").absolute().top_0().left_0().size_full().child(a11y_inert(masked(map_mask, graph))));
                }
                root = root.child(plate_ground(&staged)).child(masked(staged.plate, scroller)).children(gem(&staged));
            }
            _ => root = root.child(scroller),
        }
        // A full Ask sheet or a transitional plate with less than the
        // readable preview minimum owns these pixels. Preserve the Reader's
        // route, body, and motion state above, then omit its visual subtree
        // until the same sampled Ask geometry exposes room beside the plate.
        if self.ask_geometry.is_some_and(|ask| ask.preview_left.is_none()) {
            return div().size_full();
        }
        if retaining_departure { root = root.child(opening_status(palette)); }
        root.child(facet::probe::scroll_probe("reader-scroll", self.scroll.clone()))
            .child(glow)
            .text_color(palette.ink1.hsla())
            .font_family(facet::fonts::family(ty::BODY))
    }
}

fn opening_status(palette: &facet::Palette) -> gpui::Stateful<gpui::Div> {
    div().id("reader-opening-status").role(gpui::Role::Status).aria_label("Opening page")
        .absolute().right(px(16.0)).top(px(12.0)).px(px(12.0)).py(px(6.0))
        .max_w(px(240.0)).bg(palette.g1.hsla()).text_color(palette.ink2.hsla())
        .child("Opening page…")
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
        let track = |key: String, value: f32, target: f32, velocity: f32, started_ms: f64, budget_ms: f64, group: Option<&str>| facet::probe::TrackSample {
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
            group: group.map(ToOwned::to_owned),
        };
        if self.transit.is_none() || staged.is_none() {
            // The change landed: its tracks end at rest where they were
            // heading, so the next change starts from a rest, not from a
            // sample still in flight. The driver (`reader.carry`, never
            // painted) lands by design once its page has landed; the plate's
            // and the gem's edges are painted, and their last step is judged.
            let ended = std::mem::take(&mut *self.in_flight.borrow_mut());
            for mut sample in ended {
                sample.kind = if sample.key == "reader.carry" { facet::probe::TrackKind::Snap } else { facet::probe::TrackKind::Spring };
                sample.value = sample.target;
                sample.velocity = 0.0;
                sample.live = false;
                sample.budget_ms = 0.0;
                sample.at_ms = at_ms;
                facet::probe::record_track(cx, || sample);
            }
        }
        if let (Some(transit), Some(staged)) = (&self.transit, staged) {
            let (value, velocity) = transit.carry.sample(now);
            let started = millis(transit.carry.start());
            let budget = transit.carry.budget(0.001).as_secs_f64() * 1000.0;
            let mut samples = vec![track("reader.carry".to_owned(), value, transit.carry.target(), velocity, started, budget, Some("reader.transit"))];
            // The plate's and the gem's edges ride the driver through their
            // own bands (the plate's floor lags its sides): each is told its
            // own target and speed, and none claims lockstep with the driver.
            const NAMES: [&str; 4] = ["left", "top", "right", "bottom"];
            let publish_course = |name: &str, rect: Bounds<Pixels>, course: Course, samples: &mut Vec<facet::probe::TrackSample>| {
                let (at, to) = (edges(rect), edges(course.to));
                for index in 0..4 {
                    samples.push(track(format!("reader.{name}.{}", NAMES[index]), at[index], to[index], course.per_p[index] * velocity, started, budget, None));
                }
            };
            publish_course("plate", staged.plate, staged.plate_course, &mut samples);
            if let (Some((_, gem)), Some(gem_course)) = (staged.gem, staged.gem_course) {
                publish_course("gem", gem, gem_course, &mut samples);
            }
            // A change that starts from rest is born where it starts: its
            // plate at the row it opens from, its driver at the start. That
            // first frame is a designed start, not a step from the last
            // change's rest. (One that interrupts a change in flight is
            // judged against where that one was.)
            // (A render may publish its frame twice: the birth frame stays a
            // birth, or the probe keeps the later sample and loses it.)
            let born = self.in_flight.borrow().first().is_none_or(|last| last.kind == facet::probe::TrackKind::Snap && (last.at_ms - at_ms).abs() < 1e-6);
            if born {
                for sample in &mut samples {
                    sample.kind = facet::probe::TrackKind::Snap;
                }
            }
            self.in_flight.replace(samples.clone());
            for sample in samples {
                facet::probe::record_track(cx, || sample);
            }
        }
        if let (Some(tint), Some(strength)) = (&self.tint, tint) {
            let sample = track(format!("reader.tint.{}", tint.id), strength, 0.0, 0.0, millis(tint.start), 0.0, Some("reader.transit"));
            facet::probe::record_track(cx, || sample);
        }
    }
}

/// Exposes the actual content layout before GPUI's scroll container prepaints.
/// The container has this single direct child, so its measured height is the
/// extent GPUI itself uses when clamping the offset.
struct ScrollContent {
    layout: Rc<Cell<Option<gpui::LayoutId>>>,
    child: gpui::AnyElement,
}

impl IntoElement for ScrollContent {
    type Element = Self;

    fn into_element(self) -> Self { self }
}

impl gpui::Element for ScrollContent {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> { None }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, ()) {
        let layout = self.child.request_layout(window, cx);
        self.layout.set(Some(layout));
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        self.child.paint(window, cx);
    }
}

/// The reader's scroll container, bringing the focused target into view
/// when asked. It runs before the container applies its offset, from this
/// frame's layout, so the frame that walks or reflows already shows the
/// target (the storm's seed 3 saw focus left off a 320 px window after the
/// text grew).
struct Reveal {
    pending: Rc<Cell<bool>>,
    viewport_intent: Rc<RefCell<Option<ViewportIntent>>>,
    viewport_admitted: bool,
    content_layout: Rc<Cell<Option<gpui::LayoutId>>>,
    targets: Targets,
    scroll: ScrollHandle,
    /// The current viewport in window space, also measured by Region before
    /// render. Reveal records the actual scroll viewport before its child.
    frame: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The page's flows, landed when a reflow scrolls to the focus.
    land: Vec<facet::motion::Flow>,
    reading: Option<(crate::navigation::presentation::VisitId, crate::navigation::presentation::ReadingOffset, Links)>,
    scroll_mount: Option<(Rc<Cell<Option<u64>>>, u64)>,
    native_return: Option<(gpui::WeakEntity<Reader>, Rc<()>)>,
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
        // Resolve the explicit command against this frame's Taffy content
        // layout before GPUI's scroll Div prepaints. Its single child and zero
        // container padding make this the same extent Div clamps to below.
        let mut viewport_owned = false;
        {
            let mut viewport = self.viewport_intent.borrow_mut();
            if let Some(intent) = viewport.as_mut() {
                if self.scroll_mount.as_ref().is_none_or(|(_, place)| *place != intent.place) {
                    viewport.take();
                } else if self.viewport_admitted {
                    if self.targets.focused() != intent.focused {
                        // A new native target walk restores target-follow.
                        viewport.take();
                        self.pending.set(true);
                    } else {
                        let offset = self.scroll.offset();
                        if offset != intent.from {
                            // A newer wheel or programmatic scroll supersedes
                            // queued keys and becomes the viewport's origin.
                            intent.origin = ViewportOrigin::Offset(offset.y);
                            intent.commands.clear();
                            intent.from = offset;
                        } else {
                            let height = f32::from(view.size.height);
                            let content = self.content_layout.get().map(|layout|
                                f32::from(window.layout_bounds(layout).size.height));
                            if let Some(content) = content
                                && height.is_finite() && height > 0.0 && content.is_finite() && content >= 0.0 {
                                // Match ScrollHandle's two-decimal scroll-range clamp.
                                let max = px(((content - height).max(0.0) * 100.0).round() / 100.0);
                                let y = intent.resolve(view.size.height, max);
                                self.scroll.set_offset(point(offset.x, y));
                                intent.origin = intent.settled_origin(y);
                                intent.from = point(offset.x, y);
                                intent.commands.clear();
                            }
                            // If layout has no usable size, retain the exact
                            // command order for a positive-size mounted frame.
                        }
                        self.pending.set(false);
                        viewport_owned = true;
                    }
                }
            }
        }
        if self.viewport_admitted && !viewport_owned && self.pending.take()
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
                for flow in &self.land {
                    flow.land();
                }
            }
        }
        self.child.prepaint(window, cx);
        if let Some((mounted, visit)) = &self.scroll_mount {
            mounted.set(Some(*visit));
        }
        if let Some((visit, remembered, links)) = &self.reading {
            let actual = self.scroll.offset();
            if let Some(offset) = crate::navigation::presentation::ReadingOffset::new(f32::from(actual.x), f32::from(actual.y))
                && offset != *remembered {
                links.dispatch(crate::navigation::Intent::SetReading { visit: *visit,
                    change: crate::navigation::presentation::ReadingChange::ReaderOffset(offset) }, cx);
            }
            if self.targets.is_active() && let Some(key) = self.targets.focused()
                && let Some(key) = crate::navigation::presentation::ReadingText::new(key.to_string()) {
                let focus = crate::navigation::presentation::ReadingFocus::Reader(key);
                if links.snapshot(cx).session().reading.current.presentation.controls().focus.as_ref() != Some(&focus) {
                    links.dispatch(crate::navigation::Intent::SetReading { visit: *visit,
                        change: crate::navigation::presentation::ReadingChange::Focus(Some(focus)) }, cx);
                }
            }
        }
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
        if let Some((reader, ticket)) = &self.native_return {
            let reader = reader.clone();
            let ticket = Rc::clone(ticket);
            window.defer(cx, move |window, cx| {
                let _ = reader.update(cx, |reader, cx| reader.finish_painted_native_return(&ticket, window, cx));
            });
        }
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

    include!("reader/transit_capture_tests.rs");

    /// Actual paint calls, attributed by the mounted child that issued them.
    /// None denotes Map; Some is the exact page place. No word or geometry
    /// filter is used, including when a child paints outside its viewport.
    #[derive(Default)]
    struct InkTrace(Vec<(Option<u64>, PaintedText)>);
    impl gpui::Global for InkTrace {}

    pub(super) fn owned_ink(page: Option<u64>, child: impl gpui::IntoElement)
        -> impl gpui::IntoElement
    {
        OwnedInk { page, child: child.into_element() }
    }

    struct OwnedInk<E> { page: Option<u64>, child: E }
    impl<E: gpui::Element> gpui::IntoElement for OwnedInk<E> {
        type Element = Self;
        fn into_element(self) -> Self { self }
    }
    impl<E: gpui::Element> gpui::Element for OwnedInk<E> {
        type RequestLayoutState = E::RequestLayoutState;
        type PrepaintState = E::PrepaintState;
        fn id(&self) -> Option<gpui::ElementId> { self.child.id() }
        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            self.child.source_location()
        }
        fn a11y_role(&self) -> Option<gpui::Role> { self.child.a11y_role() }
        fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
            self.child.write_a11y_info(node);
        }
        fn a11y_synthetic_children(&mut self, state: &mut Self::PrepaintState,
            builder: &mut gpui::A11ySubtreeBuilder) {
            self.child.a11y_synthetic_children(state, builder);
        }
        fn request_layout(&mut self, id: Option<&gpui::GlobalElementId>,
            inspector: Option<&gpui::InspectorElementId>, window: &mut gpui::Window,
            cx: &mut gpui::App) -> (gpui::LayoutId, Self::RequestLayoutState) {
            self.child.request_layout(id, inspector, window, cx)
        }
        fn prepaint(&mut self, id: Option<&gpui::GlobalElementId>,
            inspector: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>,
            state: &mut Self::RequestLayoutState, window: &mut gpui::Window,
            cx: &mut gpui::App) -> Self::PrepaintState {
            self.child.prepaint(id, inspector, bounds, state, window, cx)
        }
        fn paint(&mut self, id: Option<&gpui::GlobalElementId>,
            inspector: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>,
            layout: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState,
            window: &mut gpui::Window, cx: &mut gpui::App) {
            let first = window.painted_texts().len();
            self.child.paint(id, inspector, bounds, layout, prepaint, window, cx);
            if cx.has_global::<InkTrace>() {
                let trace = cx.global_mut::<InkTrace>();
                trace.0.extend(window.painted_texts()[first..].iter().cloned()
                    .map(|text| (self.page, text)));
            }
        }
    }

    struct HeldDestination {
        gate: crate::runtime::owner::OwnerGate,
        entered: std::sync::mpsc::Sender<()>,
    }

    impl crate::runtime::reads::PageReader for HeldDestination {
        fn read(&mut self, request: &crate::runtime::reads::ReadRequest,
            context: &crate::runtime::reads::ReadContext<'_>)
            -> Result<crate::model::pages::PageValue, crate::model::pages::ReadFailure>
        {
            if matches!(request, crate::runtime::reads::ReadRequest::Symbol(symbol)
                if symbol == &crate::shell::tests::symbol("TransitHeldDestination")) {
                let _ = self.entered.send(());
                self.gate.wait_cancelled(context.cancel)
                    .map_err(|_| crate::model::pages::ReadFailure::Cancelled)?;
            }
            crate::runtime::reads::PageReader::read(&mut crate::shell::tests::Fixture, request, context)
        }
    }

    struct ReleaseDestination(crate::runtime::owner::OwnerGate, crate::core::VersionedRoot);
    impl Drop for ReleaseDestination {
        fn drop(&mut self) {
            self.0.publish(crate::runtime::owner::OwnerState::Ready {
                key: self.1, mode: crate::model::ServiceMode::Attached,
            });
        }
    }

    #[gpui::test]
    fn a_pending_destination_retains_its_departure_and_reverses_at_200_percent(
        cx: &mut TestAppContext,
    ) {
        use crate::navigation::Intent;
        let gate = crate::runtime::owner::OwnerGate::starting();
        let held = gate.clone();
        let (entered, received) = std::sync::mpsc::channel();
        let pool = crate::runtime::reads::ReadPool::start(2, move |_| HeldDestination {
            gate: held.clone(), entered: entered.clone(),
        }).expect("real held destination pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx,
            Some(crate::shell::tests::page_route("RelationLabel")), 1440.0, 900.0, pool);
        let key = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
        let release = ReleaseDestination(gate, key);
        let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
        rig.go(Intent::ZoomTo { display, percent: 200 });
        rig.cx.update(|window, cx| {
            facet::probe::enable(cx);
            cx.set_global(gpui::TextTrace);
            window.set_a11y_forced(true);
        });
        rig.graph.root.update(rig.cx, |root, cx| root.queue(
            Intent::Navigate(crate::shell::tests::page_route("TransitHeldDestination")), cx));
        let first = shoot(&mut rig, 0, 0);
        received.recv_timeout(Duration::from_secs(1)).expect("actual destination read entered");
        assert_eq!(first.p, None, "a genuine pending read has not started an empty plate");
        assert!(first.plate.is_none(), "the readable departure owns the whole Reader");
        assert!(reading(&first).any(|text| text.alpha > 0.0 && text.text.as_ref() == "RelationLabel"),
            "the departure still paints while the read is held: {first:#?}");
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx)), 2);
        assert!(rig.graph.store.read_with(rig.cx, |store, _| {
            store.symbol(&crate::shell::tests::symbol("TransitHeldDestination")).loaded_value().is_none()
        }), "no synthetic loaded value stands in for the held read");
        let opened = shoot(&mut rig, 112, 112);
        assert!(opened.p.is_none() && opened.plate.is_none(),
            "waiting is not an expanding blank animation: {opened:#?}");
        assert!(reading(&opened).any(|text| text.text.as_ref() == "It names one relation group."),
            "the actual departure prose remains readable across the unknown read delay");
        let native = rig.cx.update(|window, _| window.debug_a11y_tree_json().expect("pending native tree"));
        let tree: serde_json::Value = serde_json::from_str(&native).expect("pending native JSON");
        let status = tree["nodes"].as_object().expect("native nodes").values()
            .find(|node| node["aria"]["role"] == "Status" && node["aria"]["label"] == "Opening page")
            .expect("the pending status is a real named native Status");
        let width = status["bounds"]["width"].as_f64().expect("native status width");
        assert!(width > 0.0 && width <= 240.5, "the 200% native status stays bounded: {status}");
        assert_eq!(rig.cx.update(|window, cx| window.simulate_next_frame(cx)), 0,
            "awaiting the real read creates no motion wake");
        rig.cx.simulate_keystrokes("secondary-[");
        let turned = shoot(&mut rig, 128, 16);
        assert!(turned.p.is_none() && turned.plate.is_none(),
            "Back cancels the unpainted prepared visit without a phantom reversing page: {turned:#?}");
        rig.cx.simulate_resize(size(px(1000.0), px(700.0)));
        let resized = shoot(&mut rig, 128, 0);
        let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
        let frame = reader.read_with(rig.cx, |reader, _| reader.frame.get().expect("current measured Reader frame"));
        let viewport = rig.cx.debug_bounds("reader-scroll").expect("the actual current scroll viewport");
        assert_eq!(frame, viewport, "the measured embedding and Reveal's scroll viewport agree on the first resized frame");
        assert!(resized.plate.is_none(), "the cancelled visit cannot return a plate after resize");
        for text in reading(&resized) {
            assert!(inside(text.bounds, frame),
                "200% resize clips actual Reader ink to its native viewport: {text:#?} vs {frame:?}");
        }
        let native = rig.cx.update(|window, _| window.debug_a11y_tree_json().expect("resized native tree"));
        assert!(!native.contains("TransitHeldDestination"),
            "the departed pending visit contributes no stale native control: {native}");
        drop(release);
        rig.settle();
        assert_eq!(rig.route(), crate::shell::tests::page_route("RelationLabel"),
            "a late cancelled read cannot replace the newer Back destination");
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx)), 1);
        assert_eq!(rig.cx.update(|window, cx| window.simulate_next_frame(cx)), 0,
            "the settled route stops requesting motion frames");
    }

    #[test]
    fn page_code_peel_opens_from_the_measured_line_and_reverses_without_lateral_motion() {
        let reader = Bounds::new(point(px(24.0), px(40.0)), size(px(320.0), px(520.0)));
        let row = Bounds::new(point(px(86.0), px(292.0)), size(px(178.0), px(22.0)));
        let start = super::peel_at(reader, row, 0.0);
        assert_eq!((start.left(), start.right()), (reader.left(), reader.right()));
        assert_eq!((start.top(), start.bottom()), (row.top(), row.bottom()));
        let mut prior = start;
        for progress in [0.12, 0.35, 0.62, 0.84, 0.94, 1.0] {
            let plate = super::peel_at(reader, row, progress);
            assert_eq!((plate.left(), plate.right()), (reader.left(), reader.right()));
            assert!(plate.top() <= prior.top() && plate.bottom() >= prior.bottom(), "the same plate grows around the line");
            assert!(plate.top() <= row.top() && plate.bottom() >= row.bottom(), "the declaration line stays covered");
            prior = plate;
        }
        assert_eq!(prior, reader);
        for progress in [0.84, 0.62, 0.35, 0.12, 0.0] {
            let plate = super::peel_at(reader, row, progress);
            assert!(plate.top() >= prior.top() && plate.bottom() <= prior.bottom(), "Back contracts the same plate");
            prior = plate;
        }
        assert_eq!(prior, start);
    }

    /// One drawn frame: the plate (if a change is in flight), the driver, the
    /// tints, the targets and every painted text line.
    #[derive(Debug)]
    struct Shot {
        at: u64,
        reader: Bounds<Pixels>,
        plate: Option<Bounds<Pixels>>,
        gem: Option<Bounds<Pixels>>,
        p: Option<f32>,
        tint: Vec<(String, f32)>,
        targets: Vec<(String, Bounds<Pixels>)>,
        texts: Vec<PaintedText>,
        ink: Vec<(Option<u64>, PaintedText)>,
    }

    /// A place in the reader's list, for the list's own rules.
    fn place(key: u64, route: Route) -> super::Place {
        super::Place { key, visit: Default::default(), route, overlay: None, way: super::Way::Across, lens: super::Lens::Reference, from: None, opened: None, hop: false }
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
            cargo: None,
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
        let _ = rig.cx.update(|_, cx| {
            cx.set_global(InkTrace::default());
            facet::probe::take(cx)
        });
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
            reader: rig.shell.read_with(rig.cx, |shell, cx| shell.reader_entity().read(cx).frame.get().expect("Reader viewport")),
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
            ink: rig.cx.update(|_, cx| std::mem::take(&mut cx.global_mut::<InkTrace>().0)),
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
        let old: Vec<String> = old.into_iter().chain(reading(&before).map(|text| text.text.to_string())).collect();
        let (_, focused) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        let row_id = focused.expect("j focuses a row").to_string();
        let row = before.targets.iter().find(|(key, _)| *key == row_id).map(|(_, at)| *at).expect("the row is drawn");
        let shots = film(&mut rig, |rig| rig.cx.simulate_keystrokes("enter"), 400);
        assert!(matches!(rig.route(), Route::Symbol(_)), "enter opened a page");
        rig.settle();
        // The symbol page paints its words itself too (its crumb names the
        // package): what the new page says includes what it paints.
        let landed = shoot(&mut rig, 0, 0);
        let new: Vec<String> = rig.said().into_iter().chain(reading(&landed).map(|text| text.text.to_string())).collect();
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
            for text in reading(shot) {
                let content = text.text.to_string();
                if old_only.contains(&content) && !crosses(text.bounds, plate) {
                    outside.insert(content.clone());
                }
                if new_only.contains(&content) {
                    assert!(inside(text.bounds, plate), "`{content}` (the new page's) is painted outside the plate at {} ms: {:?} vs {plate:?}, Reader {:?}", shot.at, text.bounds, shot.reader);
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
        // The symbol page paints its words itself (they are not all in
        // `said`): what the page says includes what it painted.
        let opened = shoot(&mut rig, 0, 0);
        let page: Vec<String> = rig.said().into_iter().chain(reading(&opened).map(|text| text.text.to_string())).collect();
        let shots = film(&mut rig, |rig| rig.cx.simulate_keystrokes("secondary-["), 1000);
        assert!(matches!(rig.route(), Route::Package(_)), "back came home");
        // The package page's cards paint their words themselves (they are not
        // in `said`): what the parent says includes what it painted once home.
        let parent: Vec<String> = parent.into_iter().chain(shots.last().into_iter().flat_map(|shot| reading(shot).map(|text| text.text.to_string()))).collect();
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
            // What the fold has taken from the plate shows the parent (so no
            // frame has an empty reader): on the plate, the parent is only
            // ever below every line of the page still folding on it.
            let floor = shot
                .texts
                .iter()
                .filter(|text| page_only.contains(text.text.as_ref()) && text.bounds.size.height > px(1.0))
                .map(|text| text.bounds.bottom())
                .fold(plate.top(), Pixels::max);
            for text in &shot.texts {
                let content = text.text.to_string();
                if page_only.contains(&content) && text.bounds.size.height > px(1.0) {
                    assert!(inside(text.bounds, plate), "`{content}` (the page's) is painted outside the plate at {} ms: {:?} vs {plate:?}", shot.at, text.bounds);
                    folding += 1;
                }
                if parent_only.contains(&content) {
                    if crosses(text.bounds, plate) {
                        assert!(
                            text.bounds.top() >= floor - px(0.5),
                            "`{content}` (the parent's) is painted on the plate above the folding page at {} ms: {:?}, the page's lowest line ends at {floor:?}",
                            shot.at,
                            text.bounds
                        );
                    } else {
                        uncovered.insert(content);
                    }
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
        let close = film_at(&mut rig, |rig| rig.cx.simulate_keystrokes("secondary-["), &[0, 40, 80, 120, 160, 240, 320, 700, 1000]);
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
        shot.ink.iter().map(|(_, text)| text)
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
        let before = shoot(&mut rig, 0, 0);
        let page = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_entity().read(cx).painted.expect("the page was really painted"));
        assert!(before.ink.iter().any(|(owner, text)| *owner == Some(page) && text.text.as_ref() == "RelationLabel"));
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
            for (owner, text) in &shot.ink {
                assert!(inside(text.bounds, shot.reader), "native masked owner {owner:?} ink escapes Reader: {text:?}");
                if held {
                    assert_eq!(*owner, Some(page), "Map paints while the page still folds at {} ms: {text:?}", shot.at);
                    folding += 1;
                } else if owner.is_none() {
                    uncovered.insert(text.text.to_string());
                }
            }
            if !held {
                // The page has folded off its plate before the plate moves.
                // Graph has its own same-label RelationLabel. Attribution is
                // to actual paint calls, so even that label cannot hide stale
                // page ink anywhere, including outside the shrinking plate.
                let on_plate: Vec<_> = shot.ink.iter().filter(|(owner, _)| *owner == Some(page)).collect();
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
        rig.cx.simulate_keystrokes("secondary-[");
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
            cargo: None,
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
            ("back symbol→package (⌘[)", key("secondary-[")),
            ("forward package→symbol (⌘])", key("secondary-]")),
            ("across symbol→symbol (KindGlyph)", navigate(page_route("KindGlyph"))),
            ("back across (⌘[)", key("secondary-[")),
            ("view page→code (⌘.)", key("secondary-.")),
            ("view code→page (⌘.)", key("secondary-.")),
            ("route-up (⌘↑)", key("secondary-up")),
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
