//! The probe ledger: live motion tracks and measured element bounds, published
//! per frame for the harness.
//!
//! Off by default. When off, every publishing call is one global lookup and a
//! branch; the sample itself is built lazily inside a closure that never runs.
//! The harness turns it on with [`enable`], draws a frame, and drains that
//! frame's records with [`take`].

use crate::tokens::TypeRole;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Font, FontStyle, FontWeight, Global,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, TextRun,
    Window, px,
};
use std::cell::RefCell;

/// Which engine produced a track sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TrackKind {
    /// A duration and a curve.
    Tween,
    /// An analytic spring.
    Spring,
    /// A keyframe channel.
    Keys,
    /// The ambient pulse.
    Pulse,
}

impl TrackKind {
    /// A stable lower-case name for reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tween => "tween",
            Self::Spring => "spring",
            Self::Keys => "keys",
            Self::Pulse => "pulse",
        }
    }
}

/// One track, as it stood when it was sampled this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSample {
    /// The track's stable key (`channel` appended for keyframe poses).
    pub key: String,
    /// The engine.
    pub kind: TrackKind,
    /// The value painted this frame.
    pub value: f32,
    /// Where the track is heading.
    pub target: f32,
    /// Units per second.
    pub velocity: f32,
    /// When the current segment started, in virtual ms since the motion epoch.
    pub started_ms: f64,
    /// The segment's settle budget in ms (delay + duration, or the spring's
    /// estimated settle time).
    pub budget_ms: f64,
    /// When the sample was taken, in virtual ms since the motion epoch.
    pub at_ms: f64,
    /// Whether the track still needs frames.
    pub live: bool,
    /// How far past its target this segment's own curve/spring is allowed to
    /// travel, as a fraction of the segment's start-to-target span (0.0 for a
    /// curve/spring that never overshoots, such as `glide` or a critically
    /// damped spring; positive for `bounce`-like curves and underdamped
    /// springs). Computed from the curve/spring itself
    /// ([`crate::tokens::motion::Bezier::overshoot`],
    /// [`crate::motion::Spring::overshoot_ratio`]), not hand-tagged per key.
    pub overshoot_ratio: f32,
    /// Additional bound in channel units for trajectories that can bulge
    /// even when start and target are equal (camera flights and carries).
    pub overshoot_absolute: f32,
    /// An optional alignment group: elements that are meant to move in
    /// lockstep (a compound shape, a multi-part control) tag their tracks
    /// with the same group via [`grouped`] so the harness can catch one part
    /// lagging another mid-flight.
    pub group: Option<String>,
}

/// An element's painted bounds, by key.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundsSample {
    /// The key the element was measured under.
    pub key: String,
    /// Left edge in logical px.
    pub x: f32,
    /// Top edge in logical px.
    pub y: f32,
    /// Width in logical px.
    pub width: f32,
    /// Height in logical px.
    pub height: f32,
}

impl BoundsSample {
    /// Whether this rectangle and `other` overlap (touching edges do not
    /// count as overlap).
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }

    /// Whether the point lies inside (left/top edges in, right/bottom out).
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// Whether this rectangle fits entirely inside a `width` x `height`
    /// viewport at the origin.
    #[must_use]
    pub fn within(&self, width: f32, height: f32) -> bool {
        self.x >= 0.0
            && self.y >= 0.0
            && self.x + self.width <= width
            && self.y + self.height <= height
    }
}

/// How a text element handles content wider than its box.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TextOverflow {
    /// Content wider than the box is cut off mid-glyph.
    Clip,
    /// Content wider than the box is cut off with a drawn ellipsis.
    Ellipsis,
    /// Content wider than the box wraps to more lines (never clipped).
    Wrap,
}

/// A text element's box and its natural (unconstrained) width, published so
/// the layout lints can tell "wrapped/ellipsised on purpose" from "silently
/// clipped".
#[derive(Clone, Debug, PartialEq)]
pub struct TextSample {
    /// The key the element was measured under.
    pub key: String,
    /// The painted box, in logical px.
    pub bounds: BoundsSample,
    /// The width the text would take with no box constraint, in logical px.
    pub natural_width: f32,
    /// The declared overflow handling.
    pub overflow: TextOverflow,
    /// The text itself (for reports; lints never compare it).
    pub content: String,
    /// The widest unbreakable word, in logical px (what wrapping cannot fix).
    pub min_width: f32,
    /// The line height the role sets, in logical px.
    pub line_height: f32,
    /// The font size, in logical px.
    pub size: f32,
    /// The font weight.
    pub weight: f32,
    /// The [`region`] the text was built in (overlap is only checked
    /// between texts of one region).
    pub region: Option<String>,
}

impl TextSample {
    /// Whether the box is narrower than the content would need and the
    /// element does not draw an ellipsis to say so (for wrapping text: a
    /// single word wider than the box).
    #[must_use]
    pub fn clipped_without_ellipsis(&self) -> bool {
        match self.overflow {
            TextOverflow::Clip => self.natural_width > self.bounds.width + 0.5,
            TextOverflow::Wrap => self.min_width > self.bounds.width + 0.5,
            TextOverflow::Ellipsis => false,
        }
    }

    /// Whether the box is shorter than one line of its role.
    #[must_use]
    pub fn clipped_vertically(&self) -> bool {
        self.bounds.height + 0.5 < self.line_height
    }
}

/// Everything published since the last [`take`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    /// Track samples in publication (render) order.
    pub tracks: Vec<TrackSample>,
    /// Measured bounds in paint order.
    pub bounds: Vec<BoundsSample>,
    /// Measured text boxes, in paint order.
    pub texts: Vec<TextSample>,
    /// Interactive elements and the state their components believe they are
    /// in, in paint order.
    pub targets: Vec<TargetSample>,
    /// Scrollable containers that published this frame (see [`ScrollSample`]).
    pub scrolls: Vec<ScrollSample>,
    /// Overlay stacks, one per floating layer that published this frame.
    pub stacks: Vec<StackSample>,
    /// The motion gate's running count of requested frames.
    pub frames_requested: u64,
    /// Whether motion was reduced when the frame was taken (every track
    /// snaps, so jumps are the design).
    pub reduced_motion: bool,
    /// Number of `Render::render` calls this frame that opted into counting
    /// via [`rendered`]. Adoption is per-component; today only
    /// [`crate::gallery::SceneRoot`] counts itself, so this is a lower bound
    /// until more components call [`rendered`] from their own `render`.
    pub renders: u64,
}

impl Ledger {
    /// Whether any published track is still live.
    #[must_use]
    pub fn any_live(&self) -> bool {
        self.tracks.iter().any(|track| track.live)
    }

    /// The last sample published under `key`.
    #[must_use]
    pub fn track(&self, key: &str) -> Option<&TrackSample> {
        self.tracks.iter().rev().find(|track| track.key == key)
    }

    /// The last bounds published under `key`.
    #[must_use]
    pub fn bounds(&self, key: &str) -> Option<&BoundsSample> {
        self.bounds.iter().rev().find(|bounds| bounds.key == key)
    }
}

/// What a component believes about one of its interactive elements this
/// frame. The storm runner checks these beliefs against the pointer, the
/// buttons and the window's focus (a hovered element the pointer is not over
/// is a stuck hover).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Target {
    /// The component shows its hover state.
    pub hovered: bool,
    /// The component shows its pressed state.
    pub pressed: bool,
    /// The component shows keyboard focus.
    pub focused: bool,
    /// Keyboard focus can land here.
    pub focusable: bool,
    /// A pointer activates it (the 24 px hit-target rule applies).
    pub clickable: bool,
}

/// An interactive element's painted bounds and believed state.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetSample {
    /// The key the element was published under.
    pub key: String,
    /// Painted bounds in logical px.
    pub bounds: BoundsSample,
    /// What the component believes.
    pub state: Target,
}

/// A scrollable container's viewport and its full scrollable content extent,
/// both in window space (as if scrolled to the origin), published by a
/// container so the `offscreen` lint can tell "below the fold, reachable by
/// scrolling" from "clipped by a fixed box or the window, unreachable".
///
/// This is an opt-in seam: a scroll container calls [`record_scroll`] once
/// per frame it paints. Nothing calls it yet in the shipped shell (see
/// `apps/desktop/src/shell/*`, none of which this lane owns) — until one
/// does, focusables inside a real scroll container still lint as
/// `offscreen` exactly as before. The rule and its exemption are proven by
/// canary scenes in `apps/facet/src/gallery/bench.rs` that call this
/// directly.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollSample {
    /// The container's key.
    pub key: String,
    /// The container's own visible viewport, in window space.
    pub viewport: BoundsSample,
    /// The full scrollable content extent, in the same window space, as it
    /// would be laid out at scroll offset zero (i.e. everything reachable by
    /// scrolling this container, not just what is visible right now).
    pub content: BoundsSample,
}

impl ScrollSample {
    /// Whether `bounds` is reachable by scrolling this container: inside the
    /// full content extent on every axis the container actually scrolls, and
    /// inside the viewport on every axis it does not (scrolling that axis
    /// cannot help, so the container's own fixed cross-axis size still
    /// bounds it). A container whose content does not exceed its viewport on
    /// either axis does not scroll at all, so it reaches nothing extra.
    #[must_use]
    pub(crate) fn reaches(&self, bounds: &BoundsSample) -> bool {
        let scrolls_y = self.content.height > self.viewport.height + 0.5;
        let scrolls_x = self.content.width > self.viewport.width + 0.5;
        if !scrolls_x && !scrolls_y {
            return false;
        }
        let within_content = bounds.x >= self.content.x - 0.5
            && bounds.y >= self.content.y - 0.5
            && bounds.x + bounds.width <= self.content.x + self.content.width + 0.5
            && bounds.y + bounds.height <= self.content.y + self.content.height + 0.5;
        let within_viewport_x = bounds.x >= self.viewport.x - 0.5
            && bounds.x + bounds.width <= self.viewport.x + self.viewport.width + 0.5;
        let within_viewport_y = bounds.y >= self.viewport.y - 0.5
            && bounds.y + bounds.height <= self.viewport.y + self.viewport.height + 0.5;
        within_content && (scrolls_x || within_viewport_x) && (scrolls_y || within_viewport_y)
    }
}

/// Where a floating entry is in its life.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StackPhase {
    /// Arriving: animating in.
    Entering,
    /// Fully open.
    Open,
    /// Leaving: animating out, still painted, no longer interactive.
    Leaving,
}

impl StackPhase {
    /// A stable lower-case name for reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Entering => "entering",
            Self::Open => "open",
            Self::Leaving => "leaving",
        }
    }
}

/// One entry of a floating layer (a tip, a peek, a menu, a dialog…).
#[derive(Clone, Debug, PartialEq)]
pub struct StackEntry {
    /// The entry's stable key (usually its trigger's key).
    pub key: String,
    /// What kind of float it is (`tip`, `peek`, `lens`, `menu`, `dialog`, `toast`).
    pub kind: String,
    /// The entry it was chained from, if any.
    pub parent: Option<String>,
    /// Where it is in its life.
    pub phase: StackPhase,
    /// Pinned entries survive Esc and pointer leaves.
    pub pinned: bool,
    /// Its painted card, in logical px (None if it painted nothing).
    pub bounds: Option<BoundsSample>,
}

/// A floating layer's stack as it stood when the layer painted, bottom first.
///
/// The float layer publishes this once per frame with [`record_stack`]. The
/// harness holds it to: unique keys; every parent present and below its
/// child; at most one tip; every painted card inside the viewport; nothing
/// left leaving once motion settles; nothing but pinned entries after the
/// storm's neutral tail (Esc, pointer out).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StackSample {
    /// Which layer (one per window: `float`).
    pub layer: String,
    /// Bottom first.
    pub entries: Vec<StackEntry>,
}

/// A component that owns a floating stack implements this so its layer can
/// publish with one call from `render`: `probe::publish_stack(&self.stack, cx)`.
pub trait StackProbe {
    /// The stack as it stands now.
    fn stack_sample(&self) -> StackSample;
}

/// Publishes a stack while recording (`sample` only runs while recording).
pub fn record_stack(cx: &mut App, sample: impl FnOnce() -> StackSample) {
    if enabled(cx) {
        let sample = sample();
        cx.default_global::<Probe>().ledger.stacks.push(sample);
    }
}

/// Publishes whatever `stack` reports while recording.
pub fn publish_stack(stack: &impl StackProbe, cx: &mut App) {
    record_stack(cx, || stack.stack_sample());
}

/// Publishes an interactive element's bounds and believed state while
/// recording.
pub fn record_target(cx: &mut App, key: &ElementId, bounds: Bounds<Pixels>, state: Target) {
    if enabled(cx) {
        let sample = TargetSample {
            key: key.to_string(),
            bounds: bounds_sample(key, bounds),
            state,
        };
        cx.default_global::<Probe>().ledger.targets.push(sample);
    }
}

/// Publishes a scroll container's viewport and full content extent while
/// recording (see [`ScrollSample`]). A container calls this once per frame
/// it paints so the `offscreen` lint can tell content reachable by scrolling
/// it from content genuinely clipped away.
pub fn record_scroll(
    cx: &mut App,
    key: &ElementId,
    viewport: Bounds<Pixels>,
    content: Bounds<Pixels>,
) {
    if enabled(cx) {
        let sample = ScrollSample {
            key: key.to_string(),
            viewport: bounds_sample(key, viewport),
            content: bounds_sample(key, content),
        };
        cx.default_global::<Probe>().ledger.scrolls.push(sample);
    }
}

/// Wraps an interactive element so its painted bounds and the state its
/// component believes it is in are published under `key`.
pub fn target(key: impl Into<ElementId>, state: Target, child: impl IntoElement) -> TargetProbe {
    TargetProbe {
        key: key.into(),
        state,
        child: child.into_any_element(),
    }
}

/// See [`target`].
pub struct TargetProbe {
    key: ElementId,
    state: Target,
    child: AnyElement,
}

impl IntoElement for TargetProbe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TargetProbe {
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
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        record_target(cx, &self.key, bounds, self.state);
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[derive(Default)]
struct Probe {
    enabled: bool,
    ledger: Ledger,
}

impl Global for Probe {}

/// Starts recording.
pub fn enable(cx: &mut App) {
    cx.default_global::<Probe>().enabled = true;
}

/// Stops recording and drops anything unread.
pub fn disable(cx: &mut App) {
    if let Some(probe) = cx.try_global::<Probe>()
        && !probe.enabled
    {
        return;
    }
    let probe = cx.default_global::<Probe>();
    probe.enabled = false;
    probe.ledger = Ledger::default();
}

/// Whether the probe is recording.
#[must_use]
pub fn enabled(cx: &App) -> bool {
    cx.try_global::<Probe>().is_some_and(|probe| probe.enabled)
}

/// Publishes a track sample; `sample` only runs while recording.
pub fn record_track(cx: &mut App, sample: impl FnOnce() -> TrackSample) {
    if enabled(cx) {
        let sample = sample();
        cx.default_global::<Probe>().ledger.tracks.push(sample);
    }
}

/// Publishes an element's bounds under `key` while recording.
pub fn record_bounds(cx: &mut App, key: &ElementId, bounds: Bounds<Pixels>) {
    if enabled(cx) {
        let sample = bounds_sample(key, bounds);
        cx.default_global::<Probe>().ledger.bounds.push(sample);
    }
}

fn bounds_sample(key: &ElementId, bounds: Bounds<Pixels>) -> BoundsSample {
    BoundsSample {
        key: key.to_string(),
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
    }
}

/// Publishes a text element's box, natural width and overflow handling while
/// recording.
pub fn record_text(cx: &mut App, key: &ElementId, bounds: Bounds<Pixels>, sample: TextSample) {
    if enabled(cx) {
        let sample = TextSample {
            key: key.to_string(),
            bounds: bounds_sample(key, bounds),
            ..sample
        };
        cx.default_global::<Probe>().ledger.texts.push(sample);
    }
}

/// Marks the start of a draw: a window root calls it first thing in its
/// `render`. Geometry (bounds, texts, targets, stacks) describes one painted
/// frame, so what earlier, unobserved draws published is dropped (GPUI also
/// draws inside event dispatch when the window is dirty); track samples
/// carry their own times and are kept.
pub fn draw_started(cx: &mut App) {
    if enabled(cx) {
        let ledger = &mut cx.default_global::<Probe>().ledger;
        ledger.bounds.clear();
        ledger.texts.clear();
        ledger.targets.clear();
        ledger.stacks.clear();
    }
}

/// Counts one `Render::render` invocation for this frame. Call it once at the
/// top of a `Render::render` implementation to be counted by the harness's
/// frame budget report ([`crate::gallery::perf`]). Free when the probe is
/// off: one global lookup and an increment.
pub fn rendered(cx: &mut App) {
    if enabled(cx) {
        cx.default_global::<Probe>().ledger.renders += 1;
    }
}

thread_local! {
    static GROUP: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static REGION: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` with `name` as the active layout region: every [`text`] built
/// inside publishes it, and the overlap lint only compares texts of one
/// region (a floating card's text may lie over the page's; two labels of
/// one row may not). Regions nest; the innermost wins.
pub fn region<R>(name: impl Into<String>, f: impl FnOnce() -> R) -> R {
    REGION.with(|stack| stack.borrow_mut().push(name.into()));
    let result = f();
    REGION.with(|stack| {
        stack.borrow_mut().pop();
    });
    result
}

/// The innermost active [`region`], if any.
#[must_use]
pub fn current_region() -> Option<String> {
    REGION.with(|stack| stack.borrow().last().cloned())
}

/// Runs `f` with `group` as the active alignment group: every
/// [`crate::motion::Motion`] track animated inside the closure (directly or
/// through nested calls) publishes this group on its [`TrackSample`], so the
/// harness can compare tracks that are meant to move in lockstep. Groups
/// nest; the innermost active group wins.
pub fn grouped<R>(group: impl Into<String>, f: impl FnOnce() -> R) -> R {
    GROUP.with(|stack| stack.borrow_mut().push(group.into()));
    let result = f();
    GROUP.with(|stack| {
        stack.borrow_mut().pop();
    });
    result
}

/// The innermost active [`grouped`] scope, if any.
#[must_use]
pub fn current_group() -> Option<String> {
    GROUP.with(|stack| stack.borrow().last().cloned())
}

/// Drains everything published since the last call.
pub fn take(cx: &mut App) -> Ledger {
    let frames_requested = crate::motion::frames_requested(cx);
    if !enabled(cx) {
        return Ledger {
            frames_requested,
            ..Ledger::default()
        };
    }
    let reduced_motion = crate::motion::reduced(cx);
    let mut ledger = std::mem::take(&mut cx.default_global::<Probe>().ledger);
    ledger.frames_requested = frames_requested;
    ledger.reduced_motion = reduced_motion;
    ledger
}

/// Wraps an element so its painted bounds are published under `key`.
pub fn measure(key: impl Into<ElementId>, child: impl IntoElement) -> Measure {
    Measure {
        key: key.into(),
        child: child.into_any_element(),
    }
}

/// See [`measure`].
pub struct Measure {
    key: ElementId,
    child: AnyElement,
}

impl IntoElement for Measure {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Measure {
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
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        record_bounds(cx, &self.key, bounds);
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
mod grouping_tests {
    use super::{BoundsSample, TextOverflow, TextSample, current_group, grouped};

    #[test]
    fn nested_groups_use_the_innermost_scope_and_unwind() {
        assert_eq!(current_group(), None);
        grouped("outer", || {
            assert_eq!(current_group().as_deref(), Some("outer"));
            grouped("inner", || {
                assert_eq!(current_group().as_deref(), Some("inner"));
            });
            assert_eq!(current_group().as_deref(), Some("outer"));
        });
        assert_eq!(current_group(), None);
    }

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> BoundsSample {
        BoundsSample {
            key: "k".to_owned(),
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn overlap_and_viewport_containment_are_exact() {
        assert!(bounds(0.0, 0.0, 10.0, 10.0).overlaps(&bounds(5.0, 5.0, 10.0, 10.0)));
        assert!(!bounds(0.0, 0.0, 10.0, 10.0).overlaps(&bounds(10.0, 0.0, 10.0, 10.0)));
        assert!(bounds(0.0, 0.0, 100.0, 50.0).within(100.0, 50.0));
        assert!(!bounds(-1.0, 0.0, 100.0, 50.0).within(100.0, 50.0));
        assert!(!bounds(0.0, 0.0, 101.0, 50.0).within(100.0, 50.0));
    }

    #[test]
    fn text_is_flagged_clipped_only_when_narrower_and_not_ellipsised() {
        let wide_clip = TextSample {
            key: "t".to_owned(),
            bounds: bounds(0.0, 0.0, 40.0, 16.0),
            natural_width: 120.0,
            overflow: TextOverflow::Clip,
            content: "a long label".to_owned(),
            min_width: 30.0,
            line_height: 16.0,
            size: 12.0,
            weight: 400.0,
            region: None,
        };
        assert!(wide_clip.clipped_without_ellipsis());

        let wide_ellipsis = TextSample {
            overflow: TextOverflow::Ellipsis,
            ..wide_clip.clone()
        };
        assert!(!wide_ellipsis.clipped_without_ellipsis());

        let fits = TextSample {
            natural_width: 30.0,
            ..wide_clip
        };
        assert!(!fits.clipped_without_ellipsis());
    }
}

/// Wraps a text element so the harness can tell a deliberate ellipsis/wrap
/// from a silent clip, measure its contrast, and find overlapping labels:
/// publishes the painted box with the width `content` needs when set in
/// `role` at `scale` on one line (shaped by the real text system, only while
/// the probe records).
pub fn text(
    key: impl Into<ElementId>,
    content: impl Into<SharedString>,
    role: TypeRole,
    scale: f32,
    overflow: TextOverflow,
    child: impl IntoElement,
) -> Text {
    Text {
        key: key.into(),
        content: content.into(),
        role,
        scale,
        overflow,
        region: current_region(),
        child: child.into_any_element(),
    }
}

/// The width `content` takes set in `role` at `scale` on one line.
#[must_use]
pub fn natural_width(
    content: &SharedString,
    role: TypeRole,
    scale: f32,
    window: &Window,
) -> Pixels {
    let size = role.size * scale;
    let font = Font {
        family: SharedString::new_static(crate::fonts::family(role)),
        features: crate::fonts::features(role),
        fallbacks: None,
        weight: FontWeight(role.weight),
        style: if role.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
    };
    let tracking = role.tracking * size;
    let run = TextRun {
        len: content.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: (tracking != 0.0).then(|| px(tracking)),
    };
    content
        .split('\n')
        .map(|line| {
            let line = SharedString::from(line.to_owned());
            let run = TextRun {
                len: line.len(),
                ..run.clone()
            };
            window
                .text_system()
                .shape_line(line, px(size), &[run], None)
                .width
        })
        .fold(px(0.0), Pixels::max)
}

/// See [`text`].
pub struct Text {
    key: ElementId,
    content: SharedString,
    role: TypeRole,
    scale: f32,
    overflow: TextOverflow,
    region: Option<String>,
    child: AnyElement,
}

impl IntoElement for Text {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Text {
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
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if enabled(cx) {
            let natural = natural_width(&self.content, self.role, self.scale, window);
            let widest_word = self
                .content
                .split_whitespace()
                .map(|word| {
                    f32::from(natural_width(
                        &SharedString::from(word.to_owned()),
                        self.role,
                        self.scale,
                        window,
                    ))
                })
                .fold(0.0_f32, f32::max);
            let sample = TextSample {
                key: String::new(),
                bounds: bounds_sample(&self.key, bounds),
                natural_width: f32::from(natural),
                overflow: self.overflow,
                content: self.content.to_string(),
                min_width: widest_word,
                line_height: self.role.line * self.scale,
                size: self.role.size * self.scale,
                weight: self.role.weight,
                region: self.region.clone(),
            };
            record_text(cx, &self.key, bounds, sample);
        }
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}
