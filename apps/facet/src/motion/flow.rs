//! Flow: FLIP layout motion on *layout epochs*.
//!
//! Wrap keyed elements with [`Flow::item`]. When an element's laid-out
//! bounds change across an epoch — a room class, density, text scale, route,
//! list order or data change — it springs from where it was painted to where
//! it now is. Between epochs layout changes are tracked directly: a window or
//! splitter drag moves everything with the pointer, with no lag.
//!
//! ```ignore
//! // Every render: the epoch token names what counts as a discrete change.
//! // Width is not in it (drags track); the room class is (breakpoints flow).
//! self.flow.epoch((measure.room(), facet.density, facet.text_scale.to_bits(), self.order));
//! div().children(self.rows.iter().map(|row| self.flow.item(row.id, render_row(row))))
//! ```
//!
//! # The model
//!
//! Each item keeps a spring per axis for its offset from layout (0 at rest).
//! On an epoch frame its layout jump `old - new` is added to the offset
//! (*rebased*: value and velocity kept), so the painted position is exactly
//! where it was and the spring carries it home; an interruption mid-flight
//! rebases again and keeps the momentum. Nested items compensate for their
//! parent: an item only absorbs the part of its jump its flow ancestors have
//! not already absorbed, so a child that moved with its parent does not move
//! twice, and a child that stayed put while its parent moved counter-moves to
//! stay put. [`Resize::Scale`] also springs a size change through the
//! compositing layer (for leaves: heroes, gems, cards). Reduced motion snaps.
//! Settled, the offset is exactly zero: painted == laid out.

use super::spring::{Phase, SNAPPY, Spring};
use super::{epoch as motion_epoch, now, reduced, request_frame};
use crate::probe::{self, TrackKind, TrackSample};
use gpui::{
    AnyElement, App, Bounds, ElementId, Global, GlobalElementId, InspectorElementId, IntoElement,
    LayerTransform, LayoutId, Pixels, Point, SharedString, Size, Window, point, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How an item's size change animates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Resize {
    /// The new size applies at once; only the position flows (content that
    /// reflows: text, lists).
    #[default]
    Snap,
    /// The size flows too, by scaling the painted subtree from the old size
    /// (fixed content: gems, heroes, cards). Leaves only.
    Scale,
}

/// Rest threshold in px: a settled track is snapped to exactly 0.
const REST: f64 = 0.01;
/// Layout changes smaller than this (px) are float noise, not a jump.
const JUMP: f32 = 0.01;

/// One axis of an item's offset from layout.
#[derive(Clone, Copy, Debug, Default)]
struct Track {
    moving: Option<(Phase, Instant)>,
}

impl Track {
    fn sample(&self, spring: Spring, now: Instant) -> Phase {
        match self.moving {
            None => Phase {
                offset: 0.0,
                velocity: 0.0,
            },
            Some((origin, start)) => {
                spring.step(origin, now.saturating_duration_since(start).as_secs_f64())
            }
        }
    }

    /// Adds a layout jump to the offset, keeping value continuity and velocity.
    fn rebase(&mut self, spring: Spring, now: Instant, jump: f32) {
        if jump.abs() < JUMP {
            return;
        }
        let phase = self.sample(spring, now);
        self.moving = Some((
            Phase {
                offset: phase.offset + f64::from(jump),
                velocity: phase.velocity,
            },
            now,
        ));
    }

    /// Samples at `now`, coming to rest (exactly 0) when the spring has.
    #[allow(clippy::cast_possible_truncation)]
    fn advance(&mut self, spring: Spring, now: Instant) -> (f32, f32) {
        let phase = self.sample(spring, now);
        if self.moving.is_some() && spring.at_rest(phase, REST) {
            self.moving = None;
        }
        if self.moving.is_none() {
            return (0.0, 0.0);
        }
        (phase.offset as f32, phase.velocity as f32)
    }

    fn budget(&self, spring: Spring) -> Duration {
        self.moving.map_or(Duration::ZERO, |(origin, _)| {
            Duration::from_secs_f64(spring.settle_time(origin, REST))
        })
    }
}

#[derive(Clone, Debug)]
struct Record {
    layout: Bounds<Pixels>,
    x: Track,
    y: Track,
    w: Track,
    h: Track,
    seen: u64,
    /// When it was last placed (for the layout's drift velocity).
    at: Instant,
    /// The probe's current segment: when it started and its settle budget.
    /// A rebase or a layout that drifts while live starts a new one.
    segment: Option<(Instant, Duration)>,
    /// A live sample went to the probe; the settle sends a final one.
    reported: bool,
}

/// Where an item paints this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    /// Offset from layout.
    pub(crate) offset: Point<Pixels>,
    /// Painted size minus laid-out size ([`Resize::Scale`]).
    pub(crate) grow: Size<Pixels>,
    /// Layout jump this item absorbed this frame (its descendants' share of
    /// it is already taken care of).
    pub(crate) absorbed: Point<Pixels>,
    /// Whether it is still moving.
    pub(crate) live: bool,
    /// Velocity of the offset.
    velocity: Point<f32>,
    /// Velocity of the layout itself between epochs (a drag), px/s.
    drift: Point<f32>,
    started: Option<Instant>,
    budget: Duration,
}

/// The pure model: per-key tracks on an explicit clock.
#[derive(Clone, Debug)]
pub(crate) struct Model {
    spring: Spring,
    token: Option<u64>,
    epoch: bool,
    generation: u64,
    records: HashMap<ElementId, Record>,
}

impl Model {
    pub(crate) fn new() -> Self {
        Self {
            spring: SNAPPY,
            token: None,
            epoch: false,
            generation: 0,
            records: HashMap::new(),
        }
    }

    /// Starts a frame: this frame is an epoch if `token` differs from the
    /// last frame's. Records not placed last frame are dropped.
    pub(crate) fn frame(&mut self, token: u64) {
        self.epoch = self.token.is_some_and(|last| last != token);
        self.token = Some(token);
        let last = self.generation;
        self.generation += 1;
        self.records.retain(|_, record| record.seen >= last);
    }

    /// Places `key`, laid out at `layout` (window space, without flow
    /// offsets), given what its flow ancestors absorbed this frame.
    pub(crate) fn place(
        &mut self,
        key: &ElementId,
        layout: Bounds<Pixels>,
        absorbed_above: Point<Pixels>,
        resize: Resize,
        now: Instant,
        reduced: bool,
    ) -> Placement {
        let (spring, epoch, generation) = (self.spring, self.epoch, self.generation);
        let record = self.records.entry(key.clone()).or_insert_with(|| Record {
            layout,
            x: Track::default(),
            y: Track::default(),
            w: Track::default(),
            h: Track::default(),
            seen: generation,
            at: now,
            segment: None,
            reported: false,
        });
        record.seen = generation;
        // Between epochs the layout is tracked directly: its motion is drift.
        let dt = now.saturating_duration_since(record.at).as_secs_f32();
        let drift = if epoch || dt <= 0.0 {
            point(0.0, 0.0)
        } else {
            point(
                f32::from(layout.origin.x - record.layout.origin.x) / dt,
                f32::from(layout.origin.y - record.layout.origin.y) / dt,
            )
        };
        record.at = now;
        let mut rebased = false;
        let mut absorbed = point(px(0.0), px(0.0));
        if epoch && !reduced {
            // Only the part of the jump no ancestor absorbed.
            let own = point(
                record.layout.origin.x - layout.origin.x - absorbed_above.x,
                record.layout.origin.y - layout.origin.y - absorbed_above.y,
            );
            record.x.rebase(spring, now, f32::from(own.x));
            record.y.rebase(spring, now, f32::from(own.y));
            rebased = own.x.abs() >= px(JUMP) || own.y.abs() >= px(JUMP);
            absorbed = own;
            if resize == Resize::Scale {
                record.w.rebase(
                    spring,
                    now,
                    f32::from(record.layout.size.width - layout.size.width),
                );
                record.h.rebase(
                    spring,
                    now,
                    f32::from(record.layout.size.height - layout.size.height),
                );
            }
        }
        record.layout = layout;
        if reduced {
            record.x = Track::default();
            record.y = Track::default();
            record.w = Track::default();
            record.h = Track::default();
        }
        if resize == Resize::Snap {
            record.w = Track::default();
            record.h = Track::default();
        }
        let (x, vx) = record.x.advance(spring, now);
        let (y, vy) = record.y.advance(spring, now);
        let (w, _) = record.w.advance(spring, now);
        let (h, _) = record.h.advance(spring, now);
        let live = [record.x, record.y, record.w, record.h]
            .iter()
            .any(|track| track.moving.is_some());
        let drifting = drift.x.abs() + drift.y.abs() > 1e-3;
        record.segment = if !live {
            None
        } else if rebased || drifting || record.segment.is_none() {
            // A new segment from here: the settle time from the phase now.
            let remaining = [record.x, record.y, record.w, record.h]
                .iter()
                .map(|track| {
                    let phase = track.sample(spring, now);
                    Duration::from_secs_f64(spring.settle_time(phase, REST))
                })
                .max()
                .unwrap_or_default();
            Some((now, remaining))
        } else {
            record.segment
        };
        let (started, budget) = record
            .segment
            .map_or((None, Duration::ZERO), |(start, budget)| (Some(start), budget));
        Placement {
            offset: point(px(x), px(y)),
            grow: Size {
                width: px(w),
                height: px(h),
            },
            absorbed,
            live,
            velocity: point(vx, vy),
            drift,
            started,
            budget,
        }
    }

    /// Whether anything still moves at `now`.
    pub(crate) fn is_settled(&self, now: Instant) -> bool {
        let spring = self.spring;
        self.records.values().all(|record| {
            [record.x, record.y, record.w, record.h].iter().all(|track| {
                track
                    .moving
                    .is_none_or(|_| spring.at_rest(track.sample(spring, now), REST))
            })
        })
    }

    fn settle(&mut self) {
        for record in self.records.values_mut() {
            record.x = Track::default();
            record.y = Track::default();
            record.w = Track::default();
            record.h = Track::default();
        }
    }
}

thread_local! {
    /// The flow items being prepainted, outermost first: (offset applied,
    /// jump absorbed this frame).
    static STACK: RefCell<Stack> = const { RefCell::new(Vec::new()) };
}

/// What the enclosing flow items add up to: offset applied, jump absorbed
/// this frame, and the offsets' velocity.
fn inherited() -> (Point<Pixels>, Point<Pixels>, Point<f32>) {
    STACK.with(|stack| {
        stack.borrow().iter().fold(
            (point(px(0.0), px(0.0)), point(px(0.0), px(0.0)), point(0.0, 0.0)),
            |(offset, absorbed, velocity), (o, a, v)| (offset + *o, absorbed + *a, velocity + *v),
        )
    })
}

struct Inner {
    scope: SharedString,
    model: Model,
}

/// A FLIP scope (see the [module docs](self)). Cloning shares it.
#[derive(Clone)]
pub struct Flow {
    inner: Rc<RefCell<Inner>>,
}

impl Flow {
    /// A flow scope; `scope` names its probe tracks (`{scope}.{key}.x`).
    pub fn new(scope: impl Into<SharedString>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(Inner {
                scope: scope.into(),
                model: Model::new(),
            })),
        }
    }

    /// The flow registered under `scope`, shared by every view that asks.
    pub fn scoped(scope: impl Into<SharedString>, cx: &mut App) -> Self {
        let scope = scope.into();
        cx.default_global::<Scopes>()
            .0
            .entry(scope.clone())
            .or_insert_with(|| Self::new(scope))
            .clone()
    }

    /// The spring items move on (default `SNAPPY`).
    #[must_use]
    pub fn spring(self, spring: Spring) -> Self {
        self.inner.borrow_mut().model.spring = spring;
        self
    }

    /// Declares this frame's layout epoch. Call once per render, before the
    /// items: a token different from last frame's makes this frame's layout
    /// changes flow; the same token makes them track.
    pub fn epoch(&self, token: impl Hash) {
        let mut hasher = DefaultHasher::new();
        token.hash(&mut hasher);
        self.inner.borrow_mut().model.frame(hasher.finish());
    }

    /// Wraps a keyed element so its layout changes flow. While it moves it
    /// paints above static content (a row flying past its neighbours is not
    /// hidden under them), still inside its clip, transform and opacity.
    pub fn item(&self, key: impl Into<ElementId>, child: impl IntoElement) -> FlowItem {
        let stack = Rc::new(RefCell::new(Vec::new()));
        FlowItem {
            child: Some(
                StackScope {
                    stack: Rc::clone(&stack),
                    child: child.into_any_element(),
                }
                .into_any_element(),
            ),
            key: key.into(),
            inner: Rc::clone(&self.inner),
            resize: Resize::Snap,
            transform: LayerTransform::IDENTITY,
            stack,
        }
    }

    /// Whether nothing is moving.
    #[must_use]
    pub fn is_settled(&self, cx: &App) -> bool {
        self.inner.borrow().model.is_settled(now(cx))
    }

    /// Every item jumps to its layout.
    pub fn settle(&self) {
        self.inner.borrow_mut().model.settle();
    }

    /// Items tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.borrow().model.records.len()
    }

    /// Whether nothing is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Default)]
struct Scopes(HashMap<SharedString, Flow>);

impl Global for Scopes {}

/// A keyed element whose layout changes flow. Build with [`Flow::item`].
pub struct FlowItem {
    /// `None` once handed to a deferred draw (it is moving).
    child: Option<AnyElement>,
    key: ElementId,
    inner: Rc<RefCell<Inner>>,
    resize: Resize,
    transform: LayerTransform,
    /// The flow stack this item's descendants prepaint under.
    stack: Rc<RefCell<Stack>>,
}

type Stack = Vec<(Point<Pixels>, Point<Pixels>, Point<f32>)>;

/// Prepaints its child under a saved flow stack, so descendants of a
/// deferred (moving) item still compensate for it and its ancestors.
struct StackScope {
    stack: Rc<RefCell<Stack>>,
    child: AnyElement,
}

impl IntoElement for StackScope {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for StackScope {
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
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let saved = STACK.with(|stack| stack.replace(self.stack.borrow().clone()));
        self.child.prepaint(window, cx);
        STACK.with(|stack| stack.replace(saved));
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

impl FlowItem {
    /// How a size change animates (default [`Resize::Snap`]).
    #[must_use]
    pub fn resize(mut self, resize: Resize) -> Self {
        self.resize = resize;
        self
    }
}

impl IntoElement for FlowItem {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for FlowItem {
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
        let child = self
            .child
            .as_mut()
            .map(|child| child.request_layout(window, cx));
        (child.unwrap_or_else(|| window.request_layout(gpui::Style::default(), [], cx)), ())
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
        let now = now(cx);
        let reduced = reduced(cx);
        let (inherited, absorbed_above, carried) = inherited();
        let layout = Bounds {
            origin: bounds.origin - inherited,
            size: bounds.size,
        };
        let (placement, scope, finished) = {
            let mut inner = self.inner.borrow_mut();
            let placement =
                inner
                    .model
                    .place(&self.key, layout, absorbed_above, self.resize, now, reduced);
            let record = inner.model.records.get_mut(&self.key);
            let finished = record.is_some_and(|record| {
                let finished = record.reported && !placement.live;
                record.reported = placement.live;
                finished
            });
            (placement, inner.scope.clone(), finished)
        };
        if placement.live {
            request_frame(window, cx);
        }
        if probe::enabled(cx) && (placement.live || finished) {
            let painted = bounds.origin + placement.offset;
            let spring = self.inner.borrow().model.spring;
            publish(cx, &scope, &self.key, &placement, (painted, layout.origin, carried), spring, now);
        }
        let painted = Bounds {
            origin: bounds.origin + placement.offset,
            size: bounds.size,
        };
        self.transform = if placement.grow.width.abs() > px(JUMP) || placement.grow.height.abs() > px(JUMP) {
            let scale = Size {
                width: f32::from(bounds.size.width + placement.grow.width)
                    / f32::from(bounds.size.width).max(1e-3),
                height: f32::from(bounds.size.height + placement.grow.height)
                    / f32::from(bounds.size.height).max(1e-3),
            };
            LayerTransform::scale_about(painted.origin, scale)
        } else {
            LayerTransform::IDENTITY
        };
        *self.stack.borrow_mut() = STACK.with(|stack| {
            let mut chain = stack.borrow().clone();
            chain.push((placement.offset, placement.absorbed, placement.velocity));
            chain
        });
        let transform = self.transform;
        let Some(mut child) = self.child.take() else {
            return;
        };
        window.with_element_offset(placement.offset, |window| {
            window.with_layer_transform(transform, |window| {
                if placement.live {
                    // Moving: paint above static content, in the same clip.
                    let offset = window.element_offset();
                    let mask = window.content_mask();
                    window.defer_draw(child, offset, 0, Some(mask));
                } else {
                    child.prepaint(window, cx);
                    self.child = Some(child);
                }
            });
        });
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
        let transform = self.transform;
        if let Some(child) = self.child.as_mut() {
            window.with_layer_transform(transform, |window| child.paint(window, cx));
        }
    }
}

/// Publishes `{scope}.{key}.x/.y`: the painted position against the layout
/// (an epoch does not move what is painted, so the track is continuous
/// across it). Velocity is the offset's plus its ancestors' plus the
/// layout's own drift. A drifting layout is a moving target: no
/// step-response bound (the harness's `f32::MAX`); otherwise the spring's.
#[allow(clippy::cast_possible_truncation)]
fn publish(
    cx: &mut App,
    scope: &SharedString,
    key: &ElementId,
    placement: &Placement,
    (painted, target, carried): (Point<Pixels>, Point<Pixels>, Point<f32>),
    spring: Spring,
    now: Instant,
) {
    let epoch = motion_epoch(cx);
    let millis = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
    let started_ms = placement.started.map_or(0.0, millis);
    let budget_ms = placement.budget.as_secs_f64() * 1000.0;
    let at_ms = millis(now);
    let group = probe::current_group();
    let drifting = placement.drift.x.abs() + placement.drift.y.abs() > 1e-3;
    let overshoot = if drifting { f32::MAX } else { spring.overshoot_ratio() };
    for (axis, value, goal, velocity) in [
        (
            "x",
            painted.x,
            target.x,
            placement.velocity.x + carried.x + placement.drift.x,
        ),
        (
            "y",
            painted.y,
            target.y,
            placement.velocity.y + carried.y + placement.drift.y,
        ),
    ] {
        let (value, goal) = (f32::from(value), f32::from(goal));
        probe::record_track(cx, || TrackSample {
            key: format!("{scope}.{key}.{axis}"),
            kind: TrackKind::Spring,
            value: if placement.live { value } else { goal },
            target: goal,
            velocity: if placement.live { velocity } else { 0.0 },
            started_ms,
            budget_ms,
            at_ms,
            live: placement.live,
            overshoot_ratio: overshoot,
            group: group.clone(),
        });
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
mod tests {
    use super::{Model, Placement, Resize};
    use gpui::{Bounds, ElementId, Pixels, Point, point, px, size};
    use std::time::{Duration, Instant};

    fn at(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(100.0), px(20.0)))
    }

    fn origin() -> Point<Pixels> {
        point(px(0.0), px(0.0))
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn painted(layout: Bounds<Pixels>, placement: Placement) -> (f32, f32) {
        (
            f32::from(layout.origin.x + placement.offset.x),
            f32::from(layout.origin.y + placement.offset.y),
        )
    }

    #[test]
    fn an_epoch_starts_where_it_was_painted_and_settles_exactly_on_layout() {
        let t0 = Instant::now();
        let key = ElementId::Integer(1);
        let mut model = Model::new();
        model.frame(0);
        let first = model.place(&key, at(0.0, 0.0), origin(), Resize::Snap, t0, false);
        assert!(!first.live);
        // Epoch: it now lays out 200 px lower.
        model.frame(1);
        let jumped = model.place(&key, at(0.0, 200.0), origin(), Resize::Snap, t0, false);
        assert_eq!(painted(at(0.0, 200.0), jumped), (0.0, 0.0), "painted where it was");
        assert!(jumped.live);
        let mut last = 0.0;
        let mut t = t0;
        for _ in 0..200 {
            t += ms(16);
            model.frame(1);
            let p = model.place(&key, at(0.0, 200.0), origin(), Resize::Snap, t, false);
            let (_, y) = painted(at(0.0, 200.0), p);
            assert!((y - last).abs() < 60.0, "no jumps: {last} -> {y}");
            last = y;
            if !p.live {
                assert_eq!(p.offset, origin(), "settled offset is exactly zero");
                return;
            }
        }
        panic!("never settled");
    }

    #[test]
    fn between_epochs_layout_changes_are_tracked_directly() {
        let t0 = Instant::now();
        let key = ElementId::Integer(1);
        let mut model = Model::new();
        model.frame(7);
        model.place(&key, at(0.0, 0.0), origin(), Resize::Snap, t0, false);
        for step in 1..50 {
            model.frame(7);
            let x = step as f32 * 13.0;
            let p = model.place(&key, at(x, 0.0), origin(), Resize::Snap, t0 + ms(step * 16), false);
            assert_eq!(p.offset, origin(), "a drag moves it with no lag");
            assert!(!p.live);
        }
    }

    #[test]
    fn an_interruption_keeps_position_and_momentum() {
        let t0 = Instant::now();
        let key = ElementId::Integer(1);
        let mut model = Model::new();
        model.frame(0);
        model.place(&key, at(0.0, 0.0), origin(), Resize::Snap, t0, false);
        model.frame(1);
        model.place(&key, at(0.0, 300.0), origin(), Resize::Snap, t0, false);
        let mid = t0 + ms(80);
        model.frame(1);
        let before = model.place(&key, at(0.0, 300.0), origin(), Resize::Snap, mid, false);
        // Another epoch at the same instant: back up to 100.
        model.frame(2);
        let after = model.place(&key, at(0.0, 100.0), origin(), Resize::Snap, mid, false);
        assert_eq!(painted(at(0.0, 300.0), before), painted(at(0.0, 100.0), after));
        assert!((before.velocity.y - after.velocity.y).abs() < 1e-3, "{before:?} {after:?}");
        assert!(before.velocity.y > 100.0, "it was moving down fast");
    }

    #[test]
    fn a_child_moving_with_its_parent_does_not_move_twice() {
        let t0 = Instant::now();
        let (parent, child) = (ElementId::Integer(1), ElementId::Integer(2));
        let mut model = Model::new();
        model.frame(0);
        model.place(&parent, at(0.0, 0.0), origin(), Resize::Snap, t0, false);
        model.place(&child, at(10.0, 10.0), origin(), Resize::Snap, t0, false);
        // Epoch: the parent drops 200 and the child moves with it.
        model.frame(1);
        let p = model.place(&parent, at(0.0, 200.0), origin(), Resize::Snap, t0, false);
        let c = model.place(&child, at(10.0, 210.0), p.absorbed, Resize::Snap, t0, false);
        assert_eq!(c.offset, origin(), "the parent's offset already carries it");
        assert!(!c.live);
        // A child that stays put while its parent moves counter-moves.
        model.frame(2);
        let p = model.place(&parent, at(0.0, 400.0), origin(), Resize::Snap, t0, false);
        let c = model.place(&child, at(10.0, 210.0), p.absorbed, Resize::Snap, t0, false);
        // It was painted at 10 (riding the parent's -200 offset); still is.
        let child_painted = 210.0 + f32::from(p.offset.y) + f32::from(c.offset.y);
        assert_eq!(child_painted, 10.0, "painted where it was");
        assert!(c.live, "and it flows home against its parent's motion");
    }

    #[test]
    fn reduced_motion_snaps() {
        let t0 = Instant::now();
        let key = ElementId::Integer(1);
        let mut model = Model::new();
        model.frame(0);
        model.place(&key, at(0.0, 0.0), origin(), Resize::Scale, t0, true);
        model.frame(1);
        let p = model.place(&key, at(50.0, 90.0), origin(), Resize::Scale, t0, true);
        assert_eq!(p.offset, origin());
        assert!(!p.live);
    }

    #[test]
    fn records_not_placed_last_frame_are_dropped() {
        let t0 = Instant::now();
        let mut model = Model::new();
        model.frame(0);
        for key in 0..10_u64 {
            model.place(&ElementId::Integer(key), at(0.0, key as f32 * 20.0), origin(), Resize::Snap, t0, false);
        }
        model.frame(0);
        model.place(&ElementId::Integer(3), at(0.0, 60.0), origin(), Resize::Snap, t0, false);
        model.frame(0);
        assert_eq!(model.records.len(), 1);
    }

    /// xorshift64*.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    /// Random reorders, room changes and drags mid-flight, many per frame, a
    /// child riding every row. Invariants: within one instant an epoch never
    /// moves anything painted (parent or child) and a drag moves it exactly
    /// by its layout change; velocities stay finite; after a neutral tail
    /// everything is exactly on layout, nothing is live, no record leaks.
    #[test]
    fn storms_are_continuous_and_settle_exactly() {
        for seed in 1..=30_u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let mut now = Instant::now();
            let mut model = Model::new();
            let mut order: Vec<u64> = (0..8).collect();
            let mut width = 800.0_f32;
            let mut version = 0_u64;
            let columns = |width: f32| if width < 600.0 { 1_u64 } else { 2 };
            let layout = |order: &[u64], width: f32, key: u64| {
                let row = order.iter().position(|&k| k == key).unwrap_or(0) as f32;
                let cols = columns(width) as f32;
                let (col, line) = (row % cols, (row / cols).floor());
                at(col * width / cols, line * 40.0)
            };
            let child_of = |parent: Bounds<Pixels>| {
                Bounds::new(parent.origin + point(px(6.0), px(4.0)), size(px(30.0), px(10.0)))
            };
            // What was painted last, per key: (parent, child, layout, when).
            let mut last: std::collections::HashMap<u64, ((f32, f32), (f32, f32), Bounds<Pixels>, Instant)> =
                std::collections::HashMap::new();
            let mut token = version * 4 + columns(width);
            for step in 0..2_000 {
                match rng.below(8) {
                    0 => {
                        let (a, b) = (rng.below(8) as usize, rng.below(8) as usize);
                        order.swap(a, b);
                        version += 1;
                    }
                    1 => width = 400.0 + rng.below(800) as f32,
                    2 | 3 => width = (width + rng.below(21) as f32 - 10.0).max(200.0),
                    _ => {}
                }
                if rng.below(2) == 0 {
                    now += ms(rng.below(40));
                }
                let next = version * 4 + columns(width);
                let epoch = next != token;
                token = next;
                model.frame(token);
                for &key in &order {
                    let parent_layout = layout(&order, width, key);
                    let parent = model.place(&ElementId::Integer(key), parent_layout, origin(), Resize::Snap, now, false);
                    let child_layout = child_of(parent_layout);
                    let child = model.place(&ElementId::Integer(100 + key), child_layout, parent.absorbed, Resize::Snap, now, false);
                    assert!(parent.velocity.y.is_finite() && child.velocity.x.is_finite());
                    let painted_parent = painted(parent_layout, parent);
                    let painted_child = (
                        f32::from(child_layout.origin.x + parent.offset.x + child.offset.x),
                        f32::from(child_layout.origin.y + parent.offset.y + child.offset.y),
                    );
                    if let Some((before, before_child, before_layout, when)) = last.get(&key)
                        && *when == now
                    {
                        let moved = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() + (a.1 - b.1).abs();
                        let expected = if epoch {
                            0.0
                        } else {
                            f32::from((parent_layout.origin.x - before_layout.origin.x).abs()
                                + (parent_layout.origin.y - before_layout.origin.y).abs())
                        };
                        assert!(
                            (moved(*before, painted_parent) - expected).abs() < 0.01,
                            "seed {seed} step {step}: row {key} moved {} (expected {expected})",
                            moved(*before, painted_parent)
                        );
                        assert!(
                            (moved(*before_child, painted_child) - expected).abs() < 0.01,
                            "seed {seed} step {step}: child of {key} moved {} (expected {expected})",
                            moved(*before_child, painted_child)
                        );
                    }
                    last.insert(key, (painted_parent, painted_child, parent_layout, now));
                }
            }
            // A neutral tail.
            now += ms(5_000);
            model.frame(token);
            for &key in &order {
                let parent_layout = layout(&order, width, key);
                let p = model.place(&ElementId::Integer(key), parent_layout, origin(), Resize::Snap, now, false);
                let c = model.place(&ElementId::Integer(100 + key), child_of(parent_layout), p.absorbed, Resize::Snap, now, false);
                assert_eq!((p.offset, c.offset), (origin(), origin()), "seed {seed}: settled on layout");
                assert!(!p.live && !c.live);
            }
            assert!(model.is_settled(now), "seed {seed}");
            model.frame(token);
            assert_eq!(model.records.len(), 16, "seed {seed}: no leaked records");
        }
    }

    mod drawn {
        use crate::motion::flow::Flow;
        use crate::motion::{frames_requested, reset_epoch};
        use crate::probe;
        use gpui::{
            Context, ElementId, IntoElement, ParentElement, Render, Styled, TestAppContext,
            VisualTestContext, Window, div, px,
        };
        use std::time::Duration;

        struct List {
            flow: Flow,
            order: Vec<u64>,
            version: u64,
            gap: f32,
        }

        impl Render for List {
            fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                // The order is an epoch; the gap (a "drag") is not.
                self.flow.epoch(self.version);
                div().flex().flex_col().gap(px(self.gap)).children(self.order.iter().map(|&key| {
                    self.flow.item(
                        ElementId::Integer(key),
                        probe::measure(
                            ElementId::Name(format!("row-{key}").into()),
                            div().w(px(100.0)).h(px(30.0)),
                        ),
                    )
                }))
            }
        }

        fn frame(cx: &mut VisualTestContext) -> (usize, probe::Ledger) {
            cx.update(|window, cx| {
                let callbacks = window.simulate_next_frame(cx);
                window.refresh();
                window.draw(cx).clear(cx);
                (callbacks, probe::take(cx))
            })
        }

        fn y(ledger: &probe::Ledger, key: u64) -> f32 {
            ledger.bounds(&format!("row-{key}")).expect("row").y
        }

        #[gpui::test]
        fn a_reorder_flows_from_where_rows_were_painted_and_a_drag_tracks(cx: &mut TestAppContext) {
            let (view, cx) = cx.add_window_view(|_, _| List {
                flow: Flow::new("list"),
                order: vec![1, 2, 3],
                version: 0,
                gap: 0.0,
            });
            cx.update(|_, cx| {
                probe::enable(cx);
                reset_epoch(cx);
            });
            let (_, ledger) = frame(cx);
            let top = y(&ledger, 1);
            assert_eq!((y(&ledger, 2) - top, y(&ledger, 3) - top), (30.0, 60.0));

            // A drag-like change (no epoch): rows track at once.
            view.update(cx, |list, cx| {
                list.gap = 10.0;
                cx.notify();
            });
            let (_, ledger) = frame(cx);
            assert_eq!(y(&ledger, 3) - top, 80.0, "tracked, no lag");

            // Reorder (an epoch): 3 moves to the top, but is painted where it was.
            view.update(cx, |list, cx| {
                list.order = vec![3, 1, 2];
                list.version += 1;
                cx.notify();
            });
            let (_, ledger) = frame(cx);
            assert_eq!(y(&ledger, 3) - top, 80.0, "starts where it was painted");
            assert_eq!(y(&ledger, 1) - top, 0.0, "row 1 too");
            let mut previous = 80.0;
            for _ in 0..60 {
                cx.executor().advance_clock(Duration::from_millis(16));
                cx.run_until_parked();
                let (_, ledger) = frame(cx);
                let now = y(&ledger, 3) - top;
                assert!((now - previous).abs() < 30.0, "continuous: {previous} -> {now}");
                previous = now;
            }
            let (_, ledger) = frame(cx);
            assert_eq!(y(&ledger, 3) - top, 0.0, "settled exactly on layout");
            assert_eq!(y(&ledger, 2) - top, 80.0);
            let requested = cx.update(|_, cx| frames_requested(cx));
            for _ in 0..3 {
                cx.executor().advance_clock(Duration::from_millis(16));
                let (callbacks, _) = frame(cx);
                assert_eq!(callbacks, 0, "a settled flow requests no frames");
            }
            assert_eq!(cx.update(|_, cx| frames_requested(cx)), requested);
            assert!(view.read_with(cx, |list, cx| list.flow.is_settled(cx)));
        }
    }
}
