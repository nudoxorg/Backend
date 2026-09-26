//! The graph camera `(x, y, w)`: the world point at the centre of the view
//! and the visible world width (gui-plan §8.2).
//!
//! Pointer pan follows the input immediately. Indirect motion owns one
//! mutually exclusive segment:
//! - **flight**: contextual lift, one stable bend and exact landing through
//!   [`motion::flight`](crate::motion::flight), retaining velocity when
//!   navigation changes during travel;
//! - **inertia**: a released drag keeps its velocity, decaying over 260 ms;
//! - **ease**: wheel, pinch and keys move a target; the camera follows it
//!   with a 70 ms exponential ease in log-width. Its coupled centre path
//!   keeps every world point inside its endpoint screen-coordinate bounds.
//!
//! Every step reads the executor clock, so a headless run is reproducible.

use super::layout::Box2;
use super::model::NodeId;
use crate::motion::flight::Pacing;
pub use crate::motion::flight::Travel;
use crate::motion::{self, Camera, Flights};
use crate::probe::{self, TrackKind, TrackSample};
use gpui::{App, Window};
use std::time::Instant;

/// The flight store key of the graph camera.
pub const KEY: &str = "graph-camera";

/// Where the canvas is in the window, logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl View {
    /// Pixels per world unit at `cam`.
    #[must_use]
    pub fn k(&self, cam: &Camera) -> f64 {
        f64::from(self.w) / cam.w
    }

    /// World → window.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn to_screen(&self, cam: &Camera, x: f32, y: f32) -> (f32, f32) {
        let k = self.k(cam);
        (
            self.x + ((f64::from(x) - cam.x) * k) as f32 + self.w / 2.0,
            self.y + ((f64::from(y) - cam.y) * k) as f32 + self.h / 2.0,
        )
    }

    /// Window → world.
    #[must_use]
    pub fn to_world(&self, cam: &Camera, px: f32, py: f32) -> (f64, f64) {
        let k = self.k(cam);
        (
            f64::from(px - self.x - self.w / 2.0) / k + cam.x,
            f64::from(py - self.y - self.h / 2.0) / k + cam.y,
        )
    }

    /// The world box the view shows at `cam`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn world_box(&self, cam: &Camera) -> Box2 {
        let (x0, y0) = self.to_world(cam, self.x, self.y);
        let (x1, y1) = self.to_world(cam, self.x + self.w, self.y + self.h);
        Box2 {
            x0: x0 as f32,
            y0: y0 as f32,
            x1: x1 as f32,
            y1: y1 as f32,
        }
    }

    /// The width that fits `b` with `pad` (app.js `fitW`).
    #[must_use]
    pub fn fit_w(&self, b: Box2, pad: f64) -> f64 {
        let aspect = f64::from(self.w) / f64::from(self.h.max(1.0));
        (f64::from(b.width()).max(f64::from(b.height()) * aspect) * pad).max(1e-3)
    }

    /// The camera framing `b` (app.js `frame=`: pad 1.2).
    #[must_use]
    pub fn frame(&self, b: Box2, pad: f64) -> Camera {
        let (cx, cy) = b.center();
        Camera::new(f64::from(cx), f64::from(cy), self.fit_w(b, pad))
    }

    /// Frames `b` within the measured usable room of this full canvas.
    /// The room sets the scale and screen centre; the resulting camera
    /// remains expressed against the full viewport used for painting.
    #[must_use]
    pub fn frame_in(&self, b: Box2, room: &Self, pad: f64) -> Camera {
        let framed = room.frame(b, pad);
        let full_width = f64::from(self.w.max(1.0));
        let room_width = f64::from(room.w.max(1.0));
        let w = framed.w * (full_width / room_width);
        let k = room_width / framed.w;
        let dx = f64::from(self.x) + f64::from(self.w) / 2.0
            - f64::from(room.x)
            - f64::from(room.w) / 2.0;
        let dy = f64::from(self.y) + f64::from(self.h) / 2.0
            - f64::from(room.y)
            - f64::from(room.h) / 2.0;
        Camera::new(framed.x + dx / k, framed.y + dy / k, w)
    }
}

/// What happens when a flight lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    /// Gather the prism of this symbol.
    Gather(NodeId),
}

/// A velocity in world units/s and log-width/s.
type Velocity = (f64, f64, f64);

/// A direct segment starts on its first drawn frame. Retargeted eases use
/// the last drawn frame as their origin, so wheel bursts keep moving.
#[derive(Clone, Copy)]
enum Timeline {
    FirstFrame,
    Running(Instant),
}

impl Timeline {
    fn start(&mut self, now: Instant) -> Instant {
        match *self {
            Self::FirstFrame => {
                *self = Self::Running(now);
                now
            }
            Self::Running(at) => at,
        }
    }
}

/// A coupled camera path. Centres are affine in width, so every fixed
/// world point is affine in inverse width on screen. Monotone width thus
/// bounds all intermediate screen coordinates by their two endpoints,
/// including a pan that interrupts a pending pointer-anchored zoom.
#[derive(Clone, Copy)]
enum Curve {
    Pan,
    Zoom { log_ratio: f64, width_span: f64 },
}

/// One absolute-time progress response drives the complete camera path.
/// Below the original perceptual threshold a quintic approach preserves
/// velocity and acceleration and lands exactly at rest.
#[derive(Clone, Copy)]
struct Ease {
    from: Camera,
    to: Camera,
    timeline: Timeline,
    curve: Curve,
    progress: Decay,
    budget: f64,
}

/// The complete direct-motion sample; the same sample drives paint, flight
/// velocity handoff, and the probe ledger.
#[derive(Clone, Copy, Debug)]
struct Sample {
    camera: Camera,
    velocity: Velocity,
    live: bool,
}

const EASE_MS: f64 = 70.0;
const GLIDE_MS: f64 = 260.0;
const ZOOM_EPS: f64 = 1e-4;

/// Time to arrive exactly after exponential decay down to `eps`, then its
/// same finite-duration final approach. No Euler or per-frame tail.
fn settle_ms(delta: f64, eps: f64) -> f64 {
    let ratio = delta.abs() / eps;
    EASE_MS * (ratio.max(1.0).ln() + ratio.min(1.0))
}

/// Cached scalar response. Thresholds, tangent and duration are planned
/// once; every axis uses the same exponential factor on a sampled frame.
#[derive(Clone, Copy)]
struct Decay {
    from: f64,
    to: f64,
    cutoff: f64,
    duration: f64,
    tail: f64,
    speed: f64,
}

impl Decay {
    fn new(from: f64, to: f64, eps: f64) -> Self {
        let delta = to - from;
        Self {
            from,
            to,
            cutoff: EASE_MS * (delta.abs() / eps).max(1.0).ln(),
            duration: settle_ms(delta, eps),
            tail: delta.abs().min(eps) * delta.signum(),
            speed: eps / EASE_MS * delta.signum(),
        }
    }

    /// Value and derivative per second; exact at both endpoints.
    fn sample(&self, elapsed: f64, factor: f64) -> (f64, f64) {
        if elapsed >= self.duration {
            return (self.to, 0.0);
        }
        let (remaining, speed) = if elapsed < self.cutoff {
            let remaining = (self.to - self.from) * factor;
            (remaining, remaining / EASE_MS)
        } else {
            // The final, subpixel approach matches the exponential’s
            // value, slope and acceleration, then lands with zero slope
            // and acceleration: (1−u)³(1+2u+3.5u²). It uses the same
            // finite duration as the old tangent tail.
            let u = ((elapsed - self.cutoff) / (self.duration - self.cutoff)).clamp(0.0, 1.0);
            let remain = 1.0 - u;
            (
                self.tail * remain.powi(3) * (1.0 + 2.0 * u + 3.5 * u * u),
                self.speed * remain.powi(2) * (1.0 + u + 17.5 * u * u),
            )
        };
        (
            if elapsed <= 0.0 {
                self.from
            } else {
                self.to - remaining
            },
            speed * 1000.0,
        )
    }
}

impl Ease {
    fn new(from: Camera, to: Camera, timeline: Timeline, k: f64) -> Self {
        let width_span = to.w - from.w;
        let relative = width_span / from.w;
        // ln_1p retains tiny zooms; the ordinary ratio handles extreme
        // zoom-ins without rounding relative to -1.
        let log_ratio = if relative.abs() < 0.5 {
            relative.ln_1p()
        } else {
            (to.w / from.w).ln()
        };
        let curve = if width_span == 0.0 {
            Curve::Pan
        } else {
            Curve::Zoom {
                log_ratio,
                width_span,
            }
        };
        let pixel_span = (to.x - from.x).abs().max((to.y - from.y).abs()) * k;
        let eps = (0.05 / pixel_span).min(ZOOM_EPS / log_ratio.abs());
        let progress = Decay::new(0.0, 1.0, eps);
        // Exactly the original maximum x/y/log-width exponential cutoff
        // plus its 120 ms margin. Coupling adds no time or overshoot budget.
        let budget = progress.cutoff + 120.0;
        Self {
            from,
            to,
            timeline,
            curve,
            progress,
            budget,
        }
    }

    fn sample(&self, elapsed: f64) -> Sample {
        if elapsed >= self.progress.duration {
            return Sample {
                camera: self.to,
                velocity: (0.0, 0.0, 0.0),
                live: false,
            };
        }
        let (progress, speed) = self.progress.sample(elapsed, (-elapsed / EASE_MS).exp());
        let (w, fraction, slope, zoom) = match self.curve {
            Curve::Pan => (self.from.w, progress, 1.0, 0.0),
            Curve::Zoom {
                log_ratio,
                width_span,
            } => {
                // Evaluate from the narrower endpoint to retain precision
                // at the destination of a very large zoom-in.
                let w = if log_ratio < 0.0 {
                    self.to.w * (-log_ratio * (1.0 - progress)).exp()
                } else {
                    self.from.w * (log_ratio * progress).exp()
                };
                let fraction = if log_ratio.abs() < 1e-4 {
                    (log_ratio * progress).exp_m1() / log_ratio.exp_m1()
                } else if log_ratio < 0.0 {
                    // Remaining centre fraction, measured from the goal.
                    (w - self.to.w) / -width_span
                } else {
                    (w - self.from.w) / width_span
                };
                (w, fraction, w * log_ratio / width_span, log_ratio * speed)
            }
        };
        let (dx, dy) = (self.to.x - self.from.x, self.to.y - self.from.y);
        let remaining = matches!(self.curve, Curve::Zoom { log_ratio, .. } if log_ratio <= -1e-4);
        let (x, y) = if remaining {
            (self.to.x - dx * fraction, self.to.y - dy * fraction)
        } else {
            (self.from.x + dx * fraction, self.from.y + dy * fraction)
        };
        Sample {
            camera: if elapsed <= 0.0 {
                self.from
            } else {
                Camera::new(x, y, w)
            },
            velocity: (dx * slope * speed, dy * slope * speed, zoom),
            live: true,
        }
    }
}

/// A drag release follows the analytic 260 ms exponential to the finite
/// point where its speed reaches the existing 0.02 px/ms cutoff.
#[derive(Clone, Copy)]
struct Glide {
    from: Camera,
    to: Camera,
    velocity: (f64, f64),
    timeline: Timeline,
    duration: f64,
}

impl Glide {
    fn new(from: Camera, vx: f64, vy: f64, k: f64) -> Self {
        let duration = GLIDE_MS * (vx.hypot(vy) * k / 0.02).max(1.0).ln();
        let distance = GLIDE_MS * -(-duration / GLIDE_MS).exp_m1();
        let to = Camera::new(from.x + vx * distance, from.y + vy * distance, from.w);
        Self {
            from,
            to,
            velocity: (vx, vy),
            timeline: Timeline::FirstFrame,
            duration,
        }
    }

    fn sample(&self, elapsed: f64) -> Sample {
        if elapsed >= self.duration {
            return Sample {
                camera: self.to,
                velocity: (0.0, 0.0, 0.0),
                live: false,
            };
        }
        let distance = GLIDE_MS * -(-elapsed / GLIDE_MS).exp_m1();
        let (vx, vy) = self.velocity;
        let decay = (-elapsed / GLIDE_MS).exp();
        Sample {
            camera: Camera::new(
                self.from.x + vx * distance,
                self.from.y + vy * distance,
                self.from.w,
            ),
            velocity: (vx * decay * 1000.0, vy * decay * 1000.0, 0.0),
            live: true,
        }
    }
}

/// These variants are mutually exclusive. A camera cannot glide and fly,
/// keep an old ease anchor during a flight, or retain a stale probe segment.
#[derive(Clone, Copy)]
enum Segment {
    Still,
    Ease(Ease),
    Glide(Glide),
    Flight {
        to: Camera,
        landing: Option<Landing>,
        travel: Travel,
        origin: FlightOrigin,
    },
}

#[derive(Clone, Copy)]
enum FlightOrigin {
    Direct { camera: Camera, velocity: Velocity },
    Continuing,
}

/// The graph's camera and its one current motion segment.
pub struct Rig {
    /// The camera drawn this frame.
    pub cam: Camera,
    segment: Segment,
    velocity: Velocity,
    stopped: Option<Camera>,
    last: Option<Instant>,
    view_width: f64,
    flights: Flights,
}

fn flights() -> Flights {
    Flights::new().paced(Pacing::GRAPH_TRAVEL)
}

/// Smallest visible width (world units).
pub const MIN_W: f64 = 2.5;

impl Rig {
    /// A camera at rest at `cam`.
    #[must_use]
    pub fn new(cam: Camera) -> Self {
        let flights = flights();
        flights.jump(KEY, cam);
        Self {
            cam,
            segment: Segment::Still,
            velocity: (0.0, 0.0, 0.0),
            stopped: None,
            last: None,
            view_width: 1440.0,
            flights,
        }
    }

    /// The target belongs to the current segment; it cannot become stale.
    #[must_use]
    pub fn target(&self) -> Camera {
        match self.segment {
            Segment::Still => self.cam,
            Segment::Ease(ease) => ease.to,
            Segment::Glide(glide) => glide.to,
            Segment::Flight { to, .. } => to,
        }
    }

    /// Whether a flight is under way.
    #[must_use]
    pub fn flying(&self) -> bool {
        matches!(self.segment, Segment::Flight { .. })
    }

    /// Where the current flight lands, if flying.
    #[must_use]
    pub fn destination(&self) -> Option<Camera> {
        if let Segment::Flight { to, .. } = self.segment {
            Some(to)
        } else {
            None
        }
    }

    /// Puts the camera at `cam` at once.
    pub fn set(&mut self, cam: Camera) {
        self.cam = cam;
        self.segment = Segment::Still;
        self.velocity = (0.0, 0.0, 0.0);
        self.stopped = None;
        self.last = None;
        self.flights.jump(KEY, cam);
    }

    /// Flies to `to`, retaining velocity when navigation interrupts motion.
    pub fn fly(&mut self, to: Camera, landing: Option<Landing>) {
        let context = Camera::new(
            (to.x + self.cam.x) / 2.0,
            (to.y + self.cam.y) / 2.0,
            self.cam.w.max(to.w),
        );
        let travel = if landing.is_some() {
            Travel::Focus(context)
        } else {
            Travel::Survey(context)
        };
        self.fly_with(to, landing, travel);
    }

    /// Fly with stable spatial context from the target's territory.
    pub fn fly_with(&mut self, to: Camera, landing: Option<Landing>, travel: Travel) {
        let origin = match self.segment {
            Segment::Flight { origin, .. } => origin,
            _ => FlightOrigin::Direct {
                camera: self.cam,
                velocity: self.velocity,
            },
        };
        self.segment = Segment::Flight {
            to,
            landing,
            travel,
            origin,
        };
    }

    /// Current semantic route, retained through measured card corrections.
    #[must_use]
    pub fn travel(&self) -> Option<Travel> {
        match self.segment {
            Segment::Flight { travel, .. } => Some(travel),
            _ => None,
        }
    }

    /// Correct measured geometry while preserving an active flight's purpose and landing.
    pub fn reframe(&mut self, to: Camera) {
        let (travel, landing) = match self.segment {
            Segment::Flight {
                travel, landing, ..
            } => (travel, landing),
            _ => (Travel::Reframe, None),
        };
        self.fly_with(to, landing, travel);
    }

    /// Stops flight, inertia or ease at the drawn camera.
    pub fn hold(&mut self) {
        if !matches!(self.segment, Segment::Still) {
            self.stopped = Some(self.cam);
        }
        self.segment = Segment::Still;
        self.velocity = (0.0, 0.0, 0.0);
        self.flights.jump(KEY, self.cam);
    }

    /// A drag moved the camera; direct input positions are not animation.
    pub fn drag_to(&mut self, cam: Camera) {
        self.set(cam);
    }

    /// A released drag keeps `(vx, vy)` world units/ms.
    pub fn fling(&mut self, vx: f64, vy: f64) {
        self.hold();
        self.segment = Segment::Glide(Glide::new(self.cam, vx, vy, self.view_width / self.cam.w));
        self.velocity = (vx * 1000.0, vy * 1000.0, 0.0);
    }

    /// Returns the clock for a new direct segment. A moving ease continues
    /// through wheel bursts; input takes control of indirect motion at rest.
    fn ease_clock(&mut self) -> Timeline {
        if let Segment::Ease(Ease {
            timeline: Timeline::Running(_),
            ..
        }) = self.segment
        {
            self.last.map_or(Timeline::FirstFrame, Timeline::Running)
        } else {
            if matches!(self.segment, Segment::Flight { .. } | Segment::Glide(_)) {
                self.hold();
            }
            Timeline::FirstFrame
        }
    }

    fn ease_to(&mut self, to: Camera, clock: Timeline) {
        if self.cam == to {
            self.hold();
        } else {
            self.segment =
                Segment::Ease(Ease::new(self.cam, to, clock, self.view_width / self.cam.w));
        }
    }

    /// Direct pan follows the input exactly in screen pixels. A live
    /// pointer zoom keeps its timeline: translate both of its endpoints
    /// by their own width so the entire zoom moves by the same pixels.
    pub fn pan_px(&mut self, view: &View, dx: f32, dy: f32) {
        self.view_width = f64::from(view.w);
        let shift = |camera: &mut Camera| {
            camera.x -= f64::from(dx) * camera.w / self.view_width;
            camera.y -= f64::from(dy) * camera.w / self.view_width;
        };
        let moving = !matches!(self.segment, Segment::Still);
        shift(&mut self.cam);
        if let Segment::Ease(ease) = &mut self.segment
            && matches!(ease.curve, Curve::Zoom { .. })
        {
            shift(&mut ease.from);
            shift(&mut ease.to);
            self.velocity.0 -= f64::from(dx) * self.cam.w / self.view_width * self.velocity.2;
            self.velocity.1 -= f64::from(dy) * self.cam.w / self.view_width * self.velocity.2;
            self.stopped = None;
        } else {
            self.segment = Segment::Still;
            self.velocity = (0.0, 0.0, 0.0);
            if moving || self.stopped.is_some() {
                self.stopped = Some(self.cam);
            }
        }
        self.flights.jump(KEY, self.cam);
    }

    /// Zooms about the drawn point under the pointer on every frame. Wheel
    /// bursts compound target width; moving the pointer chooses a new anchor.
    pub fn zoom_about(&mut self, view: &View, px: f32, py: f32, factor: f64, max_w: f64) {
        self.view_width = f64::from(view.w);
        let clock = self.ease_clock();
        let w = (self.target().w * factor).clamp(MIN_W, max_w.max(MIN_W));
        let (x, y) = view.to_world(&self.cam, px, py);
        let ox = f64::from(px - view.x - view.w / 2.0) / f64::from(view.w);
        let oy = f64::from(py - view.y - view.h / 2.0) / f64::from(view.w);
        let to = if w == self.cam.w {
            self.cam
        } else {
            Camera::new(x - ox * w, y - oy * w, w)
        };
        self.ease_to(to, clock);
    }

    /// Pans the target by a fraction of its visible width.
    pub fn nudge(&mut self, dx: f64, dy: f64) {
        let clock = self.ease_clock();
        let mut to = self.target();
        to.x += dx * to.w;
        to.y += dy * to.w;
        self.ease_to(to, clock);
    }

    /// Scales the target width about the centre.
    pub fn scale(&mut self, factor: f64, max_w: f64) {
        let clock = self.ease_clock();
        let mut to = self.target();
        to.w = (to.w * factor).clamp(MIN_W, max_w.max(MIN_W));
        self.ease_to(to, clock);
    }

    /// Advances one absolute-time direct segment, shared with deterministic
    /// property tests. Returns its probe timing alongside the drawn sample.
    fn direct_at(&mut self, now: Instant) -> Option<(Sample, Instant, f64, Camera)> {
        let same_frame = self.last == Some(now);
        let (mut sample, start, budget, target) = match &mut self.segment {
            Segment::Ease(ease) => {
                let start = ease.timeline.start(now);
                (
                    ease.sample(now.saturating_duration_since(start).as_secs_f64() * 1000.0),
                    start,
                    ease.budget,
                    ease.to,
                )
            }
            Segment::Glide(glide) => {
                let start = glide.timeline.start(now);
                (
                    glide.sample(now.saturating_duration_since(start).as_secs_f64() * 1000.0),
                    start,
                    glide.duration + 34.0,
                    glide.to,
                )
            }
            _ => return None,
        };
        // Direct pan has already set the exact input pose. Repeated draws
        // on the same clock retain it instead of recomputing round-off.
        if same_frame {
            sample.camera = self.cam;
        }
        self.cam = sample.camera;
        self.velocity = sample.velocity;
        self.last = Some(now);
        if !sample.live {
            self.segment = Segment::Still;
        }
        Some((sample, start, budget, target))
    }

    /// Samples the executor clock and requests another frame only while the
    /// same sample is live. Flights own their trajectory/probe publication.
    pub fn step(
        &mut self,
        view: &View,
        window: &mut Window,
        cx: &mut App,
    ) -> (bool, Option<Landing>) {
        let now = motion::now(cx);
        self.view_width = f64::from(view.w);
        if let Some(held) = self.stopped.take() {
            self.flights.snap(KEY, held, cx);
        }
        if motion::reduced(cx) {
            let (to, landing) = match self.segment {
                Segment::Flight { to, landing, .. } => (to, landing),
                Segment::Glide(_) => (self.cam, None),
                _ => (self.target(), None),
            };
            self.flights.snap(KEY, to, cx);
            self.set(to);
            return (false, landing);
        }
        if let Segment::Flight {
            to,
            landing,
            travel,
            origin,
        } = self.segment
        {
            let shot = match origin {
                FlightOrigin::Direct { camera, velocity } => self
                    .flights
                    .fly_travel_from(KEY, camera, velocity, to, travel, window, cx),
                FlightOrigin::Continuing => self.flights.fly_travel(KEY, to, travel, window, cx),
            };
            self.cam = shot.camera;
            self.segment = if shot.live {
                Segment::Flight {
                    to,
                    landing,
                    travel,
                    origin: FlightOrigin::Continuing,
                }
            } else {
                Segment::Still
            };
            if !shot.live {
                self.velocity = (0.0, 0.0, 0.0);
            }
            return (shot.live, if shot.live { None } else { landing });
        }
        let Some((sample, start, budget, target)) = self.direct_at(now) else {
            return (false, None);
        };
        self.flights.jump(KEY, self.cam);
        self.publish(now, start, budget, target, sample, cx);
        if sample.live {
            motion::request_frame(window, cx);
        }
        (sample.live, None)
    }

    #[allow(clippy::cast_possible_truncation)]
    fn publish(
        &self,
        now: Instant,
        start: Instant,
        budget: f64,
        target: Camera,
        sample: Sample,
        cx: &mut App,
    ) {
        if !probe::enabled(cx) {
            return;
        }
        let epoch = motion::epoch(cx);
        let ms = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
        let (vx, vy, zoom) = sample.velocity;
        for (axis, value, goal, speed) in [
            ("x", sample.camera.x, target.x, vx),
            ("y", sample.camera.y, target.y, vy),
            ("w", sample.camera.w, target.w, zoom * sample.camera.w),
        ] {
            probe::record_track(cx, || TrackSample {
                key: format!("{KEY}.{axis}"),
                kind: TrackKind::Tween,
                value: value as f32,
                target: goal as f32,
                velocity: speed as f32,
                started_ms: ms(start),
                budget_ms: budget,
                at_ms: ms(now),
                live: sample.live,
                overshoot_ratio: 0.0,
                overshoot_absolute: 0.0,
                group: None,
            });
        }
    }
}

/// `smoothstep(a, b, v)`.
#[must_use]
pub fn smooth(v: f64, a: f64, b: f64) -> f64 {
    let t = ((v - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
#[path = "camera/properties.rs"]
mod properties;

#[cfg(test)]
mod tests {
    use super::{Landing, Rig, View, smooth};
    use crate::graph::layout::Box2;
    use crate::motion::{self, Camera};
    use crate::{Facet, set_facet};
    use gpui::{Context, IntoElement, Render, TestAppContext, VisualTestContext, Window, div};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    type Log = Rc<RefCell<Vec<(Camera, Option<Landing>, bool)>>>;

    /// A view that steps a rig once per render and logs every frame.
    struct Stepper {
        rig: Rc<RefCell<Rig>>,
        view: View,
        log: Log,
    }

    impl Render for Stepper {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let (moving, landed) = self.rig.borrow_mut().step(&self.view, window, cx);
            let cam = self.rig.borrow().cam;
            self.log.borrow_mut().push((cam, landed, moving));
            div()
        }
    }

    const VIEW: View = View {
        x: 0.0,
        y: 0.0,
        w: 1440.0,
        h: 824.0,
    };

    fn stepper(
        cx: &mut TestAppContext,
        start: Camera,
    ) -> (Rc<RefCell<Rig>>, Log, &mut VisualTestContext) {
        let rig = Rc::new(RefCell::new(Rig::new(start)));
        let log: Log = Rc::default();
        let (_view, vcx) = cx.add_window_view({
            let (rig, log) = (Rc::clone(&rig), Rc::clone(&log));
            move |_, _| Stepper {
                rig,
                view: VIEW,
                log,
            }
        });
        (rig, log, vcx)
    }

    /// One 16 ms platform frame on the virtual clock.
    fn frame(cx: &mut VisualTestContext) {
        frame_ms(cx, 16);
    }

    fn frame_ms(cx: &mut VisualTestContext, ms: u64) {
        cx.executor().advance_clock(Duration::from_millis(ms));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    #[gpui::test]
    fn a_wheel_zoom_keeps_the_point_under_the_pointer(cx: &mut TestAppContext) {
        let (rig, log, cx) = stepper(cx, Camera::new(10.0, -20.0, 400.0));
        frame(cx);
        let (px, py) = (1100.0, 200.0);
        let before = VIEW.to_world(&rig.borrow().cam, px, py);
        rig.borrow_mut().zoom_about(&VIEW, px, py, 0.5, 1e6);
        for _ in 0..60 {
            frame(cx);
            let at = VIEW.to_world(&rig.borrow().cam, px, py);
            assert!(
                (before.0 - at.0).abs() < 1e-10 && (before.1 - at.1).abs() < 1e-10,
                "anchor drifted during zoom: {before:?} -> {at:?}"
            );
        }
        let cam = rig.borrow().cam;
        let after = VIEW.to_world(&cam, px, py);
        assert!((cam.w - 200.0).abs() < 1e-3, "settled at w = {}", cam.w);
        assert!(
            (before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3,
            "{before:?} -> {after:?}"
        );
        // The ease is monotone in log-width and settles (stops moving).
        let ws: Vec<f64> = log.borrow().iter().map(|f| f.0.w).collect();
        assert!(ws.windows(2).all(|p| p[1] <= p[0] + 1e-9), "{ws:?}");
        assert!(
            !log.borrow().last().is_some_and(|f| f.2),
            "still moving after 60 frames"
        );
    }

    #[gpui::test]
    fn a_fling_decays_over_260_ms_and_stops(cx: &mut TestAppContext) {
        let (rig, log, cx) = stepper(cx, Camera::new(0.0, 0.0, 144.0));
        frame(cx);
        // 1 world unit per ms = 10 px/ms at k = 10.
        rig.borrow_mut().fling(1.0, 0.0);
        let expected = rig.borrow().target();
        for _ in 0..200 {
            frame(cx);
        }
        let travelled = rig.borrow().cam.x;
        // ∫ e^(-t/260) dt to the 0.02 px/ms cutoff, at k = 10.
        assert!((travelled - 259.48).abs() < 1e-10, "travelled {travelled}");
        assert_eq!(rig.borrow().cam, expected, "exact finite glide endpoint");
        let last_moving = log.borrow().iter().rposition(|f| f.2).unwrap_or(0);
        assert!(last_moving < 150, "still moving at frame {last_moving}");
    }

    #[gpui::test]
    fn a_flight_lands_exactly_once_with_the_graph_pacing(cx: &mut TestAppContext) {
        let from = Camera::new(-73.0, -37.0, 2460.0);
        let to = Camera::new(180.0, 241.0, 55.0);
        let (rig, log, cx) = stepper(cx, from);
        frame(cx);
        rig.borrow_mut().fly(to, Some(Landing::Gather(7)));
        for _ in 0..140 {
            frame(cx);
        }
        let log = log.borrow();
        let landed: Vec<usize> = log
            .iter()
            .enumerate()
            .filter(|(_, f)| f.1.is_some())
            .map(|(n, _)| n)
            .collect();
        assert_eq!(landed.len(), 1, "landings {landed:?}");
        assert_eq!(log[landed[0]].1, Some(Landing::Gather(7)));
        assert_eq!(rig.borrow().cam, to, "lands exactly");
        // clamp(170·S + 260, 320, 1250) ms: count the moving frames.
        let path = motion::flight::Path::new(from, to);
        let ms = (170.0 * path.length() + 260.0).clamp(320.0, 1250.0);
        #[allow(clippy::cast_precision_loss)]
        let flew = (landed[0] - 1) as f64 * 16.0;
        assert!(
            (flew - ms).abs() <= 32.0,
            "flew {flew} ms, pacing says {ms} ms"
        );
        // Continuity: no frame jumps more than the path allows (log-width).
        let worst = log
            .windows(2)
            .map(|p| (p[1].0.w.ln() - p[0].0.w.ln()).abs())
            .fold(0.0, f64::max);
        assert!(worst < 0.25, "a frame jumps {worst} in ln w");
    }

    #[gpui::test]
    fn a_wheel_mid_flight_holds_the_camera_where_it_is(cx: &mut TestAppContext) {
        let (rig, log, cx) = stepper(cx, Camera::new(0.0, 0.0, 2000.0));
        frame(cx);
        rig.borrow_mut().fly(Camera::new(500.0, 300.0, 40.0), None);
        for _ in 0..20 {
            frame(cx);
        }
        let mid = rig.borrow().cam;
        rig.borrow_mut().zoom_about(&VIEW, 720.0, 412.0, 1.0, 1e6);
        frame(cx);
        let after = rig.borrow().cam;
        assert!(!rig.borrow().flying());
        assert!(
            (after.x - mid.x).abs() < 1e-9 && (after.w - mid.w).abs() < 1e-9,
            "{mid:?} -> {after:?}"
        );
        assert!(
            log.borrow().iter().all(|f| f.1.is_none()),
            "an interrupted flight must not land"
        );
    }

    #[gpui::test]
    fn wheel_bursts_anchor_the_drawn_point_and_compound_width(cx: &mut TestAppContext) {
        let (rig, _, cx) = stepper(cx, Camera::new(10.0, -20.0, 400.0));
        frame(cx);
        let (px, py) = (1100.0, 200.0);
        let anchor = VIEW.to_world(&rig.borrow().cam, px, py);
        for _ in 0..4 {
            rig.borrow_mut().zoom_about(&VIEW, px, py, 0.8, 1e6);
            for _ in 0..3 {
                frame(cx);
                let at = VIEW.to_world(&rig.borrow().cam, px, py);
                assert!(
                    (at.0 - anchor.0).abs() < 1e-10 && (at.1 - anchor.1).abs() < 1e-10,
                    "wheel burst drift: {at:?}"
                );
            }
        }
        let expected = rig.borrow().target();
        for _ in 0..80 {
            frame(cx);
        }
        assert!((rig.borrow().cam.w - 163.84).abs() < 1e-10);
        assert_eq!(rig.borrow().cam, expected);
    }

    #[gpui::test]
    fn pointer_down_and_set_cancel_every_pending_ease(cx: &mut TestAppContext) {
        cx.update(crate::probe::enable);
        let (rig, log, cx) = stepper(cx, Camera::new(0.0, 0.0, 400.0));
        frame(cx);
        rig.borrow_mut().nudge(1.0, -0.5);
        for _ in 0..5 {
            frame(cx);
        }
        let held = rig.borrow().cam;
        cx.update(|_, cx| {
            crate::probe::take(cx);
        });
        rig.borrow_mut().hold();
        let requested = cx.update(|_, cx| motion::frames_requested(cx));
        for _ in 0..10 {
            frame(cx);
        }
        assert_eq!(rig.borrow().cam, held);
        let ledger = cx.update(|_, cx| crate::probe::take(cx));
        assert_eq!(
            ledger.tracks.len(),
            3,
            "the held camera closes all three tracks"
        );
        assert!(
            ledger
                .tracks
                .iter()
                .all(|track| !track.live && track.value == track.target)
        );
        assert!(!log.borrow().last().expect("frame").2);
        assert_eq!(cx.update(|_, cx| motion::frames_requested(cx)), requested);
        rig.borrow_mut().nudge(1.0, 0.0);
        frame(cx);
        let fixed = Camera::new(91.0, -72.0, 21.0);
        rig.borrow_mut().set(fixed);
        frame_ms(cx, 2_000);
        rig.borrow_mut().scale(0.5, 1e6);
        frame_ms(cx, 2_000);
        assert_eq!(
            rig.borrow().cam,
            fixed,
            "new ease begins at its first drawn frame"
        );
    }

    #[gpui::test]
    fn glide_is_independent_of_frame_cadence_and_long_gaps(cx: &mut TestAppContext) {
        let (rig, _, cx) = stepper(cx, Camera::new(5.0, -3.0, 144.0));
        frame(cx);
        let at_260 = Camera::new(
            5.0 + 130.0 * (1.0 - (-1.0_f64).exp()),
            -3.0 - 65.0 * (1.0 - (-1.0_f64).exp()),
            144.0,
        );
        for cadence in [
            [13, 13, 13, 13, 13, 13, 13, 13, 13, 143],
            [26; 10],
            [1, 1, 1, 1, 1, 1, 1, 1, 1, 251],
        ] {
            rig.borrow_mut().set(Camera::new(5.0, -3.0, 144.0));
            rig.borrow_mut().fling(0.5, -0.25);
            frame_ms(cx, 2_000); // a delayed first frame still starts at release
            assert_eq!(rig.borrow().cam, Camera::new(5.0, -3.0, 144.0));
            for ms in cadence {
                frame_ms(cx, ms);
            }
            let cam = rig.borrow().cam;
            assert!(
                (cam.x - at_260.x).abs() < 1e-10 && (cam.y - at_260.y).abs() < 1e-10,
                "{cam:?} vs {at_260:?}"
            );
            let expected = rig.borrow().target();
            frame_ms(cx, 2_000);
            assert_eq!(rig.borrow().cam, expected);
            assert!(matches!(rig.borrow().segment, super::Segment::Still));
        }
    }

    #[gpui::test]
    fn exact_settle_and_quiet_tail_request_no_frames(cx: &mut TestAppContext) {
        let (rig, log, cx) = stepper(cx, Camera::new(0.0, 0.0, 400.0));
        frame(cx);
        rig.borrow_mut().zoom_about(&VIEW, 1100.0, 200.0, 0.5, 1e6);
        let expected = rig.borrow().target();
        let mut settled = false;
        for _ in 0..100 {
            let requested = cx.update(|_, cx| motion::frames_requested(cx));
            frame(cx);
            if rig.borrow().cam == expected {
                assert!(
                    !log.borrow().last().expect("frame").2,
                    "exact arrival cannot remain live"
                );
                assert_eq!(
                    cx.update(|_, cx| motion::frames_requested(cx)),
                    requested,
                    "arrival schedules nothing"
                );
                settled = true;
                break;
            }
        }
        assert!(settled);
        let requested = cx.update(|_, cx| motion::frames_requested(cx));
        for _ in 0..10 {
            frame_ms(cx, 100);
        }
        assert_eq!(cx.update(|_, cx| motion::frames_requested(cx)), requested);
    }

    #[gpui::test]
    fn reduced_motion_stops_a_live_ease_glide_and_flight(cx: &mut TestAppContext) {
        let (rig, log, cx) = stepper(cx, Camera::new(0.0, 0.0, 400.0));
        frame(cx);
        for mode in 0..3 {
            cx.update(|_, cx| set_facet(Facet::default(), cx));
            rig.borrow_mut().set(Camera::new(0.0, 0.0, 400.0));
            match mode {
                0 => rig.borrow_mut().zoom_about(&VIEW, 1100.0, 200.0, 0.5, 1e6),
                1 => rig.borrow_mut().fling(0.5, -0.25),
                _ => rig
                    .borrow_mut()
                    .fly(Camera::new(500.0, 300.0, 40.0), Some(Landing::Gather(7))),
            }
            for _ in 0..5 {
                frame(cx);
            }
            let expected = match mode {
                1 => rig.borrow().cam,
                2 => Camera::new(500.0, 300.0, 40.0),
                _ => rig.borrow().target(),
            };
            assert!(
                mode != 2 || rig.borrow().flying(),
                "toggle must interrupt a live flight"
            );
            let before_toggle = log.borrow().len();
            cx.update(|_, cx| {
                set_facet(
                    Facet {
                        reduced_motion: true,
                        ..Facet::default()
                    },
                    cx,
                )
            });
            let requested = cx.update(|_, cx| motion::frames_requested(cx));
            frame(cx);
            assert_eq!(rig.borrow().cam, expected);
            assert!(!log.borrow().last().expect("frame").2);
            // The Facet observer can draw before the explicit platform
            // frame. Count the once-only landing across all new draws.
            let landings: Vec<_> = log.borrow()[before_toggle..]
                .iter()
                .filter_map(|frame| frame.1)
                .collect();
            assert_eq!(
                landings,
                if mode == 2 {
                    vec![Landing::Gather(7)]
                } else {
                    vec![]
                }
            );
            for _ in 0..10 {
                frame(cx);
            }
            assert_eq!(cx.update(|_, cx| motion::frames_requested(cx)), requested);
            assert!(log.borrow().last().expect("frame").1.is_none());
        }
    }

    #[gpui::test]
    fn direct_motion_hands_its_velocity_to_a_flight(cx: &mut TestAppContext) {
        let (rig, _, cx) = stepper(cx, Camera::new(0.0, 0.0, 400.0));
        frame(cx);
        for glide in [false, true] {
            rig.borrow_mut().set(Camera::new(0.0, 0.0, 400.0));
            if glide {
                rig.borrow_mut().fling(0.1, -0.05);
            } else {
                rig.borrow_mut().zoom_about(&VIEW, 1100.0, 200.0, 0.5, 1e6);
            }
            for _ in 0..5 {
                frame(cx);
            }
            let at = rig.borrow().cam;
            let velocity = rig.borrow().velocity;
            assert!(velocity.0.abs() + velocity.1.abs() + velocity.2.abs() > 1.0);
            rig.borrow_mut().fly(Camera::new(500.0, 300.0, 40.0), None);
            frame(cx);
            assert_eq!(
                rig.borrow().cam,
                at,
                "flight starts exactly at the drawn camera"
            );
            frame_ms(cx, 1);
            let after = rig.borrow().cam;
            let derivative = (
                (after.x - at.x) * 1000.0,
                (after.y - at.y) * 1000.0,
                (after.w.ln() - at.w.ln()) * 1000.0,
            );
            for (got, want) in [
                (derivative.0, velocity.0),
                (derivative.1, velocity.1),
                (derivative.2, velocity.2),
            ] {
                assert!(
                    (got - want).abs() < 0.03 * want.abs().max(1.0),
                    "velocity changed: {derivative:?} vs {velocity:?}"
                );
            }
        }
    }

    #[gpui::test]
    fn extreme_direct_input_to_focus_has_bounded_screen_geometry_and_quiet_rest(
        cx: &mut TestAppContext,
    ) {
        let (rig, _, cx) = stepper(cx, Camera::new(0.0, 0.0, 400.0));
        frame(cx);
        for width in [2.5, 400.0, 1e6] {
            for input in 0..3 {
                for factor in [2.5 / width, 2.0, 1e6 / width] {
                    if input != 0 && factor != 2.0 {
                        continue;
                    }
                    rig.borrow_mut().set(Camera::new(0.0, 0.0, width));
                    match input {
                        0 => rig
                            .borrow_mut()
                            .zoom_about(&VIEW, 1080.0, 206.0, factor, 1e6),
                        1 => rig.borrow_mut().pan_px(&VIEW, 1e6, -300_000.0),
                        _ => rig
                            .borrow_mut()
                            .fling(width * 1000.0 / 1440.0, -width * 300.0 / 1440.0),
                    }
                    frame(cx);
                    let from = rig.borrow().cam;
                    let goal = Camera::new(0.0, 0.0, 40.0);
                    rig.borrow_mut().fly(goal, None);
                    let travel = rig.borrow().travel().expect("graph intent");
                    let path = motion::flight::GraphPath::new(from, goal, travel);
                    let duration = (motion::flight::Pacing::GRAPH_TRAVEL.duration)(
                        motion::flight::Path::new(from, goal).length(),
                    )
                    .as_secs_f64();
                    frame(cx);
                    assert_eq!(rig.borrow().cam, from);
                    let start = cx.update(|_, cx| motion::now(cx));
                    for _ in 0..100 {
                        frame(cx);
                        let tau = cx.update(|_, cx| {
                            motion::now(cx)
                                .saturating_duration_since(start)
                                .as_secs_f64()
                        });
                        let base =
                            path.at_t((motion::flight::Pacing::GRAPH_TRAVEL.ease)(tau / duration));
                        let actual = rig.borrow().cam;
                        let ratio = actual.w / base.w;
                        assert!(
                            ratio >= 0.8 - 1e-10 && ratio <= 1.25 + 1e-10,
                            "input{input} width{width}: geometry collapsed {actual:?}"
                        );
                        let pan = (
                            (actual.x - base.x) * 1440.0 / actual.w,
                            (actual.y - base.y) * 1440.0 / actual.w,
                        );
                        assert!(
                            pan.0.abs() <= 216.0 + 1e-6 && pan.1.abs() <= 216.0 + 1e-6,
                            "inherited pan escaped fixed216px: {pan:?}"
                        );
                        let focus = (-actual.x * 1440.0 / actual.w, -actual.y * 1440.0 / actual.w);
                        let endpoint = (-from.x * 1440.0 / from.w, -from.y * 1440.0 / from.w);
                        assert!(
                            focus.0.abs() <= 1.25 * endpoint.0.abs() + 216.0 + 1e-6
                                && focus.1.abs() <= 1.25 * endpoint.1.abs() + 216.0 + 1e-6,
                            "input{input} width{width}: focused origin escaped by {focus:?}"
                        );
                    }
                    assert_eq!(rig.borrow().cam, goal);
                    assert!(!rig.borrow().flying());
                    let requests = cx.update(|_, cx| motion::frames_requested(cx));
                    for _ in 0..5 {
                        frame_ms(cx, 100);
                    }
                    assert_eq!(cx.update(|_, cx| motion::frames_requested(cx)), requests);
                }
            }
        }
    }

    #[test]
    fn screen_and_world_are_inverse() {
        let view = View {
            x: 10.0,
            y: 50.0,
            w: 1440.0,
            h: 824.0,
        };
        let cam = Camera::new(-12.5, 40.0, 300.0);
        let (sx, sy) = view.to_screen(&cam, 3.0, -7.0);
        let (wx, wy) = view.to_world(&cam, sx, sy);
        assert!(
            (wx - 3.0).abs() < 1e-3 && (wy + 7.0).abs() < 1e-3,
            "{wx} {wy}"
        );
        // The camera's centre is the view's centre.
        #[allow(clippy::cast_possible_truncation)]
        let (cx, cy) = view.to_screen(&cam, cam.x as f32, cam.y as f32);
        assert!((cx - 730.0).abs() < 1e-3 && (cy - 462.0).abs() < 1e-3);
    }

    #[test]
    fn framing_fits_the_tighter_axis() {
        let view = View {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 500.0,
        };
        let tall = Box2 {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 100.0,
        };
        // 100 tall at 2:1 needs 200 wide.
        assert!((view.fit_w(tall, 1.0) - 200.0).abs() < 1e-6);
        let wide = Box2 {
            x0: 0.0,
            y0: 0.0,
            x1: 300.0,
            y1: 10.0,
        };
        assert!((view.fit_w(wide, 1.2) - 360.0).abs() < 1e-4);
        assert!((smooth(0.5, 0.0, 1.0) - 0.5).abs() < 1e-12 && smooth(-1.0, 0.0, 1.0) == 0.0);
    }
}
