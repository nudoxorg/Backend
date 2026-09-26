//! Flow: FLIP layout motion on *layout epochs*.
//!
//! Wrap keyed elements with [`Flow::item`]. When an element's laid-out
//! bounds change across an epoch — a room class, density, text scale, route,
//! list order or data change — it springs from where it was painted to where
//! it now is. Between epochs layout motion is followed directly: a window or
//! splitter drag, or a presence slot opening above it, moves it with no lag.
//!
//! ```ignore
//! // Every render: the epoch token names what counts as a discrete change.
//! // Width is not in it (drags are followed); the room class is.
//! self.flow.epoch((measure.room(), facet.density, facet.text_scale.to_bits(), self.order));
//! div().children(self.rows.iter().map(|row| self.flow.item(row.id, render_row(row))))
//! ```
//!
//! # The model
//!
//! Each item keeps a spring per axis for its offset from layout (0 at rest)
//! and the velocity its layout moves at. Every frame the layout's step is
//! compared with what that velocity predicted (plus what carriers announce:
//! a [presence](super::presence) slot opening or closing above it):
//!
//! - a step that continues the motion — slower in the same direction, or
//!   within half of how far the item was moving anyway — is **followed**:
//!   painted moves with the layout;
//! - anything else is **absorbed**: the item keeps its predicted course and
//!   the difference goes into its spring (value and velocity kept). That is
//!   an epoch's jump, and equally a breakpoint nobody declared, a pixel-snap
//!   step of a slowly creeping layout, or the first frame of a drag (the
//!   frame that measures its speed).
//!
//! On an epoch frame the whole step except the prediction is absorbed, so an
//! epoch in the middle of a drag springs from where the item is painted and
//! keeps following the drag. The painted position never jumps.
//!
//! Nested items measure their layout from their nearest flow ancestor's, so
//! a child that moved with its parent does not move twice and a child that
//! stayed put while its parent moved counter-moves to stay put.
//! [`Resize::Scale`] also springs a size change through the compositing layer
//! (for leaves: heroes, gems, cards). Reduced motion snaps. Settled, the
//! offset is exactly zero: painted == laid out.

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

/// Rest threshold in px: a settled track is snapped to exactly 0 (the
/// engine's own springs rest at 1e-3 too; the last snap is invisible).
const REST: f64 = 1e-3;
/// How much a layout step may differ from its prediction and still be
/// followed, as a share of how far the item was moving anyway. Half: layout
/// lands on the device-pixel grid (half a pixel at 2x), so a steady move of a
/// pixel or more a frame steps by up to half again its speed; anything
/// further off is absorbed.
const FOLLOW: f32 = 0.5;
/// Differences below this (px) are float noise.
const NOISE: f32 = 1e-3;
/// Size changes smaller than this (px) are not a jump.
const JUMP: f32 = 0.01;

/// A spring on one component of an item's offset from layout.
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

    /// Adds `jump` to the offset, keeping value continuity and velocity.
    /// Whether it did (float noise is ignored).
    fn rebase(&mut self, spring: Spring, now: Instant, jump: f32) -> bool {
        if jump.abs() < NOISE {
            return false;
        }
        let phase = self.sample(spring, now);
        self.moving = Some((
            Phase {
                offset: phase.offset + f64::from(jump),
                velocity: phase.velocity,
            },
            now,
        ));
        true
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
}

/// Whether a layout step continues the item's motion: its layout was
/// predicted to move `predicted` and its spring moved it `sprung` this frame.
/// Followed: no step, a step that slows the predicted one down (never past
/// it), or one that differs from the prediction by at most [`FOLLOW`] of how
/// far the item was moving anyway.
fn follows(step: f32, predicted: f32, sprung: f32) -> bool {
    let slower = step * predicted >= 0.0 && step.abs() <= predicted.abs();
    step.abs() <= NOISE || slower || (step - predicted).abs() <= FOLLOW * (predicted + sprung).abs() + NOISE
}

/// One axis of an item's position: its spring, and how its layout moves.
#[derive(Clone, Copy, Debug, Default)]
struct Axis {
    spring: Track,
    /// The layout's velocity (px/s) beyond what carriers announce, as last
    /// measured.
    velocity: f32,
}

impl Axis {
    /// The layout moved `step` px in `dt` s, while carriers announced `was`
    /// px/s at the start of that interval and `carry` at its end. Follows or
    /// absorbs the step (see the module docs); whether the spring was
    /// rebased.
    fn place(
        &mut self,
        spring: Spring,
        (then, now): (Instant, Instant),
        (step, grid): (f32, f32),
        (was, carry): (f32, f32),
        epoch: bool,
    ) -> bool {
        let dt = now.saturating_duration_since(then).as_secs_f32();
        // What it was moving at, as published last frame; and what its
        // spring did meanwhile.
        let predicted = (self.velocity + was) * dt;
        #[allow(clippy::cast_possible_truncation)]
        let sprung = (self.spring.sample(spring, now).offset - self.spring.sample(spring, then).offset) as f32;
        let followed = !epoch && follows(step, predicted, sprung);
        // An epoch's jump is not motion; anything else between frames is.
        // The layout's own share of the step is known only within bounds:
        // the carriers moved it by somewhere between their velocities at the
        // two ends of the interval (a carrier starting now has not moved it
        // yet), and both ends sit on the device-pixel grid (each off by up
        // to half a `grid`). Its speed changes only as much as those bounds
        // force: a steady move whose steps alternate on the grid (2.5, 3,
        // 2.5 px) keeps one speed.
        if !epoch && dt > 0.0 {
            let low = step - grid - was.max(carry) * dt;
            let high = step + grid - was.min(carry) * dt;
            self.velocity = (self.velocity * dt).clamp(low, high) / dt;
        }
        !followed && self.spring.rebase(spring, now, predicted - step)
    }
}

#[derive(Clone, Debug)]
struct Record {
    /// Its layout, the origin measured from its nearest flow ancestor's.
    layout: Bounds<Pixels>,
    x: Axis,
    y: Axis,
    w: Track,
    h: Track,
    /// The carriers' announced layout velocity when it was last placed.
    carry: Point<f32>,
    seen: u64,
    /// When it was last placed.
    at: Instant,
    /// The probe's current segment: when it started, its settle budget, and
    /// whether its target moves (then it has no step-response bound). Every
    /// rebase starts a new one, and so does a target that starts moving.
    segment: Option<Segment>,
    /// A live sample went to the probe; the settle sends a final one.
    reported: bool,
    /// Where it was last published.
    shown: Point<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    started: Instant,
    budget: Duration,
    drifting: bool,
}

/// How an item's carriers move it this frame, px/s.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Carry {
    /// Its own layout: rooms opening or closing inside its nearest flow
    /// ancestor.
    pub(crate) layout: Point<f32>,
    /// Everything carrying it (that, plus its flow ancestors' motion and
    /// whatever carries them).
    pub(crate) total: Point<f32>,
}

/// Where an item paints this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    /// Offset from layout.
    pub(crate) offset: Point<Pixels>,
    /// Painted size minus laid-out size ([`Resize::Scale`]).
    pub(crate) grow: Size<Pixels>,
    /// Whether its spring is still moving.
    pub(crate) live: bool,
    /// Velocity of the offset (its spring), px/s.
    spring: Point<f32>,
    /// Velocity of its layout beyond what carriers announce, px/s.
    velocity: Point<f32>,
    /// Whether the spring was rebased this frame (a new probe segment).
    rebased: bool,
    /// The probe segment it is in, if its spring is live.
    segment: Option<Segment>,
}

/// The pure model: per-key tracks on an explicit clock.
#[derive(Clone, Debug)]
pub(crate) struct Model {
    spring: Spring,
    token: Option<u64>,
    epoch: bool,
    generation: u64,
    records: HashMap<ElementId, Record>,
    /// Items that stopped being drawn while their spring was live, and where
    /// they were last shown: the probe gets a final sample for each.
    vanished: Vec<(ElementId, Point<f32>)>,
    /// Whether any item's layout was moving last frame, and this frame so far
    /// (an epoch in the middle of a drag springs towards moving targets).
    drifted: bool,
    drifting: bool,
    /// The device-pixel grid layout lands on (px; 0 when exact).
    grid: f32,
}

impl Model {
    pub(crate) fn new() -> Self {
        Self {
            spring: SNAPPY,
            token: None,
            epoch: false,
            generation: 0,
            records: HashMap::new(),
            vanished: Vec::new(),
            drifted: false,
            drifting: false,
            grid: 0.0,
        }
    }

    /// Starts a frame: this frame is an epoch if `token` differs from the
    /// last frame's. Records not placed last frame are dropped.
    pub(crate) fn frame(&mut self, token: u64) {
        self.epoch = self.token.is_some_and(|last| last != token);
        self.token = Some(token);
        self.drifted = std::mem::take(&mut self.drifting);
        let last = self.generation;
        self.generation += 1;
        let vanished = &mut self.vanished;
        self.records.retain(|key, record| {
            let kept = record.seen >= last;
            if !kept && record.reported {
                vanished.push((key.clone(), record.shown));
            }
            kept
        });
    }

    /// Places `key`, laid out at `layout` (its origin measured from its
    /// nearest flow ancestor's layout, window space if none), carried as
    /// `carried` says.
    pub(crate) fn place(
        &mut self,
        key: &ElementId,
        layout: Bounds<Pixels>,
        carried: Carry,
        resize: Resize,
        now: Instant,
        reduced: bool,
    ) -> Placement {
        let carry = carried.layout;
        let (spring, epoch, generation) = (self.spring, self.epoch, self.generation);
        let record = self.records.entry(key.clone()).or_insert_with(|| Record {
            layout,
            x: Axis::default(),
            y: Axis::default(),
            w: Track::default(),
            h: Track::default(),
            carry,
            seen: generation,
            at: now,
            segment: None,
            reported: false,
            shown: point(f32::from(layout.origin.x), f32::from(layout.origin.y)),
        });
        record.seen = generation;
        let step = point(
            f32::from(layout.origin.x - record.layout.origin.x),
            f32::from(layout.origin.y - record.layout.origin.y),
        );
        // Reduced motion follows everything (the springs are cleared below).
        let (then, grid) = (record.at, self.grid);
        let mut rebased = record.x.place(spring, (then, now), (step.x, grid), (record.carry.x, carry.x), epoch);
        rebased |= record.y.place(spring, (then, now), (step.y, grid), (record.carry.y, carry.y), epoch);
        if epoch && !reduced && resize == Resize::Scale {
            let (dw, dh) = (
                f32::from(record.layout.size.width - layout.size.width),
                f32::from(record.layout.size.height - layout.size.height),
            );
            if dw.abs() >= JUMP {
                rebased |= record.w.rebase(spring, now, dw);
            }
            if dh.abs() >= JUMP {
                rebased |= record.h.rebase(spring, now, dh);
            }
        }
        record.layout = layout;
        record.carry = carry;
        record.at = now;
        if reduced {
            record.x.spring = Track::default();
            record.y.spring = Track::default();
            record.w = Track::default();
            record.h = Track::default();
        }
        if resize == Resize::Snap {
            record.w = Track::default();
            record.h = Track::default();
        }
        let (x, vx) = record.x.spring.advance(spring, now);
        let (y, vy) = record.y.spring.advance(spring, now);
        let (w, _) = record.w.advance(spring, now);
        let (h, _) = record.h.advance(spring, now);
        let tracks = [record.x.spring, record.y.spring, record.w, record.h];
        let live = tracks.iter().any(|track| track.moving.is_some());
        let drifting = (carried.total.x + record.x.velocity).abs() + (carried.total.y + record.y.velocity).abs() > NOISE;
        self.drifting |= drifting;
        // An epoch while the layout is moving (a class change mid-drag)
        // springs towards targets that keep moving.
        let drifting = drifting || (epoch && self.drifted);
        record.segment = match record.segment {
            _ if !live => None,
            Some(segment) if !rebased && (segment.drifting || !drifting) => Some(segment),
            _ => {
                // A new segment from here: the settle time from the phase now.
                let budget = tracks
                    .iter()
                    .map(|track| {
                        let phase = track.sample(spring, now);
                        Duration::from_secs_f64(spring.settle_time(phase, REST))
                    })
                    .max()
                    .unwrap_or_default();
                Some(Segment {
                    started: now,
                    budget,
                    drifting,
                })
            }
        };
        Placement {
            offset: point(px(x), px(y)),
            grow: Size {
                width: px(w),
                height: px(h),
            },
            live,
            spring: point(vx, vy),
            velocity: point(record.x.velocity, record.y.velocity),
            rebased: rebased && live,
            segment: record.segment,
        }
    }

    /// Whether anything still moves at `now`.
    pub(crate) fn is_settled(&self, now: Instant) -> bool {
        let spring = self.spring;
        self.records.values().all(|record| {
            [record.x.spring, record.y.spring, record.w, record.h].iter().all(|track| {
                track
                    .moving
                    .is_none_or(|_| spring.at_rest(track.sample(spring, now), REST))
            })
        })
    }

    fn settle(&mut self) {
        for record in self.records.values_mut() {
            record.x.spring = Track::default();
            record.y.spring = Track::default();
            record.w = Track::default();
            record.h = Track::default();
        }
    }
}

/// What an enclosing motion element does to everything inside it this frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Carrier {
    /// A flow item: its own layout origin (its descendants measure theirs
    /// from it) and the offset it paints them at.
    anchor: Option<(Point<Pixels>, Point<Pixels>)>,
    /// How fast it moves their layout, px/s: a presence slot pushed by rooms
    /// opening or closing before it; a flow item's own layout velocity.
    layout: Point<f32>,
    /// How fast it moves their paint offset, px/s: a flow item's spring.
    paint: Point<f32>,
}

type Stack = Vec<Carrier>;

thread_local! {
    /// The motion elements being prepainted around the current element,
    /// outermost first.
    static STACK: RefCell<Stack> = const { RefCell::new(Vec::new()) };
}

/// Prepaints `f` with everything inside carried at `velocity` px/s by its
/// layout: a presence slot that the rooms before it are pushing.
pub(crate) fn carried<R>(velocity: Point<f32>, f: impl FnOnce() -> R) -> R {
    STACK.with(|stack| {
        stack.borrow_mut().push(Carrier {
            anchor: None,
            layout: velocity,
            paint: point(0.0, 0.0),
        });
    });
    let out = f();
    STACK.with(|stack| stack.borrow_mut().pop());
    out
}

/// Where an item is placed from, by what carries it.
struct Context {
    /// The nearest flow ancestor's layout origin.
    anchor: Point<Pixels>,
    /// The flow ancestors' offsets, summed.
    offset: Point<Pixels>,
    /// Layout velocity announced inside the nearest flow ancestor (what moves
    /// the item's own layout), px/s.
    carry: Point<f32>,
    /// Velocity of everything carrying it, px/s.
    carried: Point<f32>,
}

fn context() -> Context {
    STACK.with(|stack| {
        let mut context = Context {
            anchor: point(px(0.0), px(0.0)),
            offset: point(px(0.0), px(0.0)),
            carry: point(0.0, 0.0),
            carried: point(0.0, 0.0),
        };
        for carrier in stack.borrow().iter() {
            context.carried = context.carried + carrier.layout + carrier.paint;
            match carrier.anchor {
                Some((anchor, offset)) => {
                    context.anchor = anchor;
                    context.offset = context.offset + offset;
                    context.carry = point(0.0, 0.0);
                }
                None => context.carry = context.carry + carrier.layout,
            }
        }
        context
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
    /// changes flow; the same token makes them followed (or absorbed, when
    /// they are not motion; see the [module docs](self)).
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
    /// The carriers this item's descendants prepaint under.
    stack: Rc<RefCell<Stack>>,
}

/// Prepaints its child under a saved carrier stack, so descendants of a
/// deferred (moving) item are still measured from it and its ancestors.
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
        let context = context();
        // Its layout: the bounds without any paint offset around it (flow
        // springs, presence poses, scrolling), exactly as gpui snapped them.
        let layout = bounds.origin - window.pixel_snap_point(window.element_offset());
        let own = Bounds {
            origin: layout - context.anchor,
            size: bounds.size,
        };
        let (placement, scope, spring, finished, vanished) = {
            let mut inner = self.inner.borrow_mut();
            let carried = Carry {
                layout: context.carry,
                total: context.carried,
            };
            inner.model.grid = 1.0 / window.scale_factor().max(1.0);
            let placement = inner.model.place(&self.key, own, carried, self.resize, now, reduced);
            let vanished = std::mem::take(&mut inner.model.vanished);
            let finished = inner.model.records.get(&self.key).is_some_and(|record| record.reported && !placement.live);
            (placement, inner.scope.clone(), inner.model.spring, finished, vanished)
        };
        if placement.live {
            request_frame(window, cx);
        }
        if probe::enabled(cx) {
            for (key, shown) in vanished {
                publish(cx, &scope, &key, Sample::gone(shown), now);
            }
            // Its flow position: where its flow ancestors and its own spring
            // put it (a presence pose on top is the presence's to report).
            let target = layout + context.offset;
            let target = point(f32::from(target.x), f32::from(target.y));
            let value = point(
                target.x + f32::from(placement.offset.x),
                target.y + f32::from(placement.offset.y),
            );
            let velocity = context.carried + placement.velocity + placement.spring;
            let moving = placement.live || placement.rebased || velocity.x.abs() + velocity.y.abs() > NOISE;
            if moving || finished {
                publish(
                    cx,
                    &scope,
                    &self.key,
                    Sample {
                        value,
                        target,
                        velocity,
                        live: placement.live,
                        started: placement.segment.map(|segment| segment.started),
                        budget: placement.segment.map_or(Duration::ZERO, |segment| segment.budget),
                        // A moving target has no step-response bound (the
                        // harness's `f32::MAX`); otherwise the spring's.
                        overshoot: if placement.segment.is_some_and(|segment| segment.drifting) {
                            f32::MAX
                        } else {
                            spring.overshoot_ratio()
                        },
                    },
                    now,
                );
            }
            if let Some(record) = self.inner.borrow_mut().model.records.get_mut(&self.key) {
                record.reported = placement.live;
                record.shown = value;
            }
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
            chain.push(Carrier {
                anchor: Some((layout, placement.offset)),
                layout: placement.velocity,
                paint: placement.spring,
            });
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

/// One probe sample of an item's flow position.
struct Sample {
    value: Point<f32>,
    target: Point<f32>,
    velocity: Point<f32>,
    live: bool,
    started: Option<Instant>,
    budget: Duration,
    overshoot: f32,
}

impl Sample {
    /// The last word on an item that stopped being drawn mid-flight: at rest
    /// where it was last shown.
    fn gone(shown: Point<f32>) -> Self {
        Self {
            value: shown,
            target: shown,
            velocity: point(0.0, 0.0),
            live: false,
            started: None,
            budget: Duration::ZERO,
            overshoot: 0.0,
        }
    }
}

/// Publishes `{scope}.{key}.x/.y`: the flow position against where it rests
/// (an epoch does not move what is painted, so the track is continuous
/// across it), with the velocity of everything that moves it.
fn publish(cx: &mut App, scope: &SharedString, key: &ElementId, sample: Sample, now: Instant) {
    let epoch = motion_epoch(cx);
    let millis = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
    let started_ms = sample.started.map_or(0.0, millis);
    let budget_ms = sample.budget.as_secs_f64() * 1000.0;
    let at_ms = millis(now);
    let group = probe::current_group();
    for (axis, value, target, velocity) in [
        ("x", sample.value.x, sample.target.x, sample.velocity.x),
        ("y", sample.value.y, sample.target.y, sample.velocity.y),
    ] {
        probe::record_track(cx, || TrackSample {
            key: format!("{scope}.{key}.{axis}"),
            kind: TrackKind::Spring,
            value,
            target,
            velocity,
            started_ms,
            budget_ms,
            at_ms,
            live: sample.live,
            overshoot_ratio: sample.overshoot,
            overshoot_absolute: 0.0,
            group: group.clone(),
        });
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
mod tests {
    use super::{Carry, Model, Placement, Resize};
    use gpui::{Bounds, ElementId, Pixels, Point, point, px, size};
    use std::time::{Duration, Instant};

    fn at(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(100.0), px(20.0)))
    }

    fn origin() -> Point<Pixels> {
        point(px(0.0), px(0.0))
    }

    /// No carrier moves the layout.
    fn still() -> Carry {
        Carry::default()
    }

    /// A carrier moving the layout at `v` px/s down.
    fn down(v: f32) -> Carry {
        Carry {
            layout: point(0.0, v),
            total: point(0.0, v),
        }
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
        let first = model.place(&key, at(0.0, 0.0), still(), Resize::Snap, t0, false);
        assert!(!first.live);
        // Epoch: it now lays out 200 px lower.
        model.frame(1);
        let jumped = model.place(&key, at(0.0, 200.0), still(), Resize::Snap, t0, false);
        assert_eq!(painted(at(0.0, 200.0), jumped), (0.0, 0.0), "painted where it was");
        assert!(jumped.live);
        let mut last = 0.0;
        let mut t = t0;
        for _ in 0..200 {
            t += ms(16);
            model.frame(1);
            let p = model.place(&key, at(0.0, 200.0), still(), Resize::Snap, t, false);
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

    /// A drag from rest: its first frame is the one that measures its speed
    /// (nothing predicted it, so it is absorbed and painted holds); from the
    /// second on, painted moves with the layout and the held step drains
    /// while the drag goes on.
    #[test]
    fn a_drag_is_followed_once_it_moves() {
        let t0 = Instant::now();
        let key = ElementId::Integer(1);
        let mut model = Model::new();
        model.frame(7);
        model.place(&key, at(0.0, 0.0), still(), Resize::Snap, t0, false);
        let mut held = f32::INFINITY;
        let mut previous = (0.0, 0.0);
        for step in 1..60 {
            model.frame(7);
            let x = step as f32 * 13.0;
            let p = model.place(&key, at(x, 0.0), still(), Resize::Snap, t0 + ms(step * 16), false);
            let now = painted(at(x, 0.0), p);
            assert!((p.velocity.x - 812.5).abs() < 0.5, "measured at 13 px / 16 ms: {}", p.velocity.x);
            let lag = -f32::from(p.offset.x);
            if step == 1 {
                assert_eq!(now, (0.0, 0.0), "the first frame holds");
            } else {
                let moved = now.0 - previous.0;
                // The drag's 13 px, plus the held step draining (SNAPPY's
                // 0.5 % overshoot can pull back a hair).
                assert!((12.9..=13.0 * 1.5).contains(&moved), "step {step}: moved {moved}, dragged 13");
                assert!((-0.1..=held).contains(&lag), "step {step}: the held step only drains: {held} -> {lag}");
            }
            held = held.min(lag.max(0.0));
            previous = now;
        }
        let last = model.place(&key, at(59.0 * 13.0, 0.0), still(), Resize::Snap, t0 + ms(59 * 16), false);
        assert_eq!(last.offset, origin(), "followed with no lag once moving");
        assert!(!last.live);
    }

    #[test]
    fn an_interruption_keeps_position_and_momentum() {
        let t0 = Instant::now();
        let key = ElementId::Integer(1);
        let mut model = Model::new();
        model.frame(0);
        model.place(&key, at(0.0, 0.0), still(), Resize::Snap, t0, false);
        model.frame(1);
        model.place(&key, at(0.0, 300.0), still(), Resize::Snap, t0, false);
        let mid = t0 + ms(80);
        model.frame(1);
        let before = model.place(&key, at(0.0, 300.0), still(), Resize::Snap, mid, false);
        // Another epoch at the same instant: back up to 100.
        model.frame(2);
        let after = model.place(&key, at(0.0, 100.0), still(), Resize::Snap, mid, false);
        assert_eq!(painted(at(0.0, 300.0), before), painted(at(0.0, 100.0), after));
        assert!((before.spring.y - after.spring.y).abs() < 1e-3, "{before:?} {after:?}");
        assert!(before.spring.y > 100.0, "it was moving down fast");
    }

    /// Children are placed from their parent's layout.
    #[test]
    fn a_child_moving_with_its_parent_does_not_move_twice() {
        let t0 = Instant::now();
        let (parent, child) = (ElementId::Integer(1), ElementId::Integer(2));
        let mut model = Model::new();
        model.frame(0);
        model.place(&parent, at(0.0, 0.0), still(), Resize::Snap, t0, false);
        model.place(&child, at(10.0, 10.0), still(), Resize::Snap, t0, false);
        // Epoch: the parent drops 200 and the child moves with it.
        model.frame(1);
        let p = model.place(&parent, at(0.0, 200.0), still(), Resize::Snap, t0, false);
        let c = model.place(&child, at(10.0, 10.0), still(), Resize::Snap, t0, false);
        assert_eq!(c.offset, origin(), "the parent's offset already carries it");
        assert!(!c.live);
        // A child that stays put (210) while its parent moves to 400
        // counter-moves.
        model.frame(2);
        let p2 = model.place(&parent, at(0.0, 400.0), still(), Resize::Snap, t0, false);
        let c = model.place(&child, at(10.0, 210.0 - 400.0), still(), Resize::Snap, t0, false);
        // It was painted at 10 (riding the parent's -200 offset); still is.
        let child_painted = 210.0 + f32::from(p2.offset.y) + f32::from(c.offset.y);
        assert_eq!(child_painted, 10.0, "painted where it was");
        assert!(c.live, "and it flows home against its parent's motion");
        assert_eq!(f32::from(p.offset.y), -200.0);
    }

    /// A carrier (a presence slot opening above it) announces how fast it
    /// moves the layout: the item follows from the carrier's first frame,
    /// with no hold, and publishes that speed before the layout has moved.
    #[test]
    fn announced_carriers_are_followed_from_their_first_frame() {
        let t0 = Instant::now();
        let key = ElementId::Integer(1);
        let mut model = Model::new();
        model.frame(0);
        model.place(&key, at(0.0, 100.0), still(), Resize::Snap, t0, false);
        // The room starts opening: announced at 1500 px/s, easing out.
        let carry = |t: f32| 1500.0 * (1.0 - t / 0.08).max(0.0);
        let mut y = 100.0;
        let mut last_carry = carry(0.0);
        model.frame(0);
        let start = model.place(&key, at(0.0, y), down(last_carry), Resize::Snap, t0 + ms(16), false);
        assert!(!start.live && start.velocity.y == 0.0, "{start:?}");
        for step in 1..10_u64 {
            let t = step as f32 * 0.008;
            let c = carry(t);
            // Where the room has pushed it (snapped to the device grid).
            y += 0.5 * (last_carry + c) * 0.008;
            let snapped = (y * 2.0).round() / 2.0;
            model.frame(0);
            let p = model.place(&key, at(0.0, snapped), down(c), Resize::Snap, t0 + ms(16) + ms(8 * step), false);
            assert!(
                f32::from(p.offset.y).abs() <= 0.5,
                "step {step}: follows the room (lag {})",
                -f32::from(p.offset.y)
            );
            last_carry = c;
        }
    }

    #[test]
    fn reduced_motion_snaps() {
        let t0 = Instant::now();
        let key = ElementId::Integer(1);
        let mut model = Model::new();
        model.frame(0);
        model.place(&key, at(0.0, 0.0), still(), Resize::Scale, t0, true);
        model.frame(1);
        let p = model.place(&key, at(50.0, 90.0), still(), Resize::Scale, t0, true);
        assert_eq!(p.offset, origin());
        assert!(!p.live);
    }

    #[test]
    fn records_not_placed_last_frame_are_dropped() {
        let t0 = Instant::now();
        let mut model = Model::new();
        model.frame(0);
        for key in 0..10_u64 {
            model.place(&ElementId::Integer(key), at(0.0, key as f32 * 20.0), still(), Resize::Snap, t0, false);
        }
        model.frame(0);
        model.place(&ElementId::Integer(3), at(0.0, 60.0), still(), Resize::Snap, t0, false);
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
    /// child riding every row. Invariants: within one instant nothing painted
    /// moves (parent or child), epoch or not (a layout change in no time is a
    /// jump, never motion); velocities stay finite; after a neutral tail
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
            // A child's layout, from its parent's.
            let child_layout = Bounds::new(point(px(6.0), px(4.0)), size(px(30.0), px(10.0)));
            // What was painted last, per key: (parent, child, when).
            type Seen = ((f32, f32), (f32, f32), Instant);
            let mut last: std::collections::HashMap<u64, Seen> = std::collections::HashMap::new();
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
                token = version * 4 + columns(width);
                model.frame(token);
                for &key in &order {
                    let parent_layout = layout(&order, width, key);
                    let parent = model.place(&ElementId::Integer(key), parent_layout, still(), Resize::Snap, now, false);
                    let child = model.place(&ElementId::Integer(100 + key), child_layout, still(), Resize::Snap, now, false);
                    assert!(parent.velocity.y.is_finite() && parent.spring.x.is_finite() && child.velocity.x.is_finite());
                    let painted_parent = painted(parent_layout, parent);
                    let painted_child = (
                        f32::from(parent_layout.origin.x + px(6.0) + parent.offset.x + child.offset.x),
                        f32::from(parent_layout.origin.y + px(4.0) + parent.offset.y + child.offset.y),
                    );
                    if let Some((before, before_child, when)) = last.get(&key) {
                        let moved = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() + (a.1 - b.1).abs();
                        if *when == now {
                            assert!(
                                moved(*before, painted_parent) < 0.01,
                                "seed {seed} step {step}: row {key} moved {} within an instant",
                                moved(*before, painted_parent)
                            );
                            assert!(
                                moved(*before_child, painted_child) < 0.01,
                                "seed {seed} step {step}: child of {key} moved {} within an instant",
                                moved(*before_child, painted_child)
                            );
                        }
                    }
                    last.insert(key, (painted_parent, painted_child, now));
                }
            }
            // A neutral tail.
            now += ms(5_000);
            model.frame(token);
            for &key in &order {
                let parent_layout = layout(&order, width, key);
                let p = model.place(&ElementId::Integer(key), parent_layout, still(), Resize::Snap, now, false);
                let c = model.place(&ElementId::Integer(100 + key), child_layout, still(), Resize::Snap, now, false);
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

            // A drag (no epoch): the gap opens 1 px per 16 ms frame, row 3 is
            // pushed 2 px a frame. The first frame measures the speed (held);
            // from then on row 3 moves with the drag, and it lands exactly.
            let mut previous = 60.0;
            for gap in 1..=10 {
                cx.executor().advance_clock(Duration::from_millis(16));
                view.update(cx, |list, cx| {
                    list.gap = gap as f32;
                    cx.notify();
                });
                let (_, ledger) = frame(cx);
                let now = y(&ledger, 3) - top;
                let moved = now - previous;
                if gap == 1 {
                    assert_eq!(moved, 0.0, "the drag's first frame holds");
                } else {
                    assert!((1.5..=3.0).contains(&moved), "gap {gap}: followed the drag: moved {moved}");
                }
                previous = now;
            }
            for _ in 0..40 {
                cx.executor().advance_clock(Duration::from_millis(16));
                frame(cx);
            }
            let (_, ledger) = frame(cx);
            assert_eq!(y(&ledger, 3) - top, 80.0, "landed on layout, no lag left");

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
