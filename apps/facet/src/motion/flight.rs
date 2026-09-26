//! Flight: a camera flying the van Wijk & Nuij (2003) optimal pan-and-zoom
//! path — out far enough to see both ends, across, and back in, which reads
//! as one continuous move instead of a pan that smears and a zoom that jumps.
//!
//! The camera is `(x, y, w)`: the world point at the centre of the view and
//! the visible world width. [`Path`] is the pure maths (ρ = √2); [`Flights`]
//! is the keyed, interruptible store a view samples in `render`, on the
//! executor clock, publishing `{key}.x/.y/.w` to the probe ledger.
//!
//! ```ignore
//! let shot = self.flights.fly("graph", target, window, cx);
//! // Reduced motion: no flight; the old framing crossfades out over 120 ms.
//! if let Some(from) = shot.from { draw_world(from).opacity(1.0 - shot.fade) }
//! draw_world(shot.camera).opacity(shot.fade)
//! ```
//!
//! # Numerics
//!
//! `r = ln(√(b²+1) − b)` is evaluated as `−asinh(b)` and
//! `cosh r0·tanh(ρs+r0) − sinh r0` as `sinh(ρs) / cosh(ρs+r0)` (the same
//! quantities), so neither cancels catastrophically as the pan distance goes
//! to zero: the zoom-only limit is continuous. For a pure zoom the path
//! length is van Wijk's `|ln(w1/w0)| / ρ` (never negative, so `S` is
//! symmetric).
//!
//! # Interruption
//!
//! A new target mid-flight re-plans from the camera's current position; the
//! difference between the camera's current velocity and the new path's
//! starting velocity is carried by a bounded response. Its horizon limits
//! pan displacement to 15% of a viewport width and width scaling to 1.25×
//! the optimal path. Pan is normalized by the sampled width, so inherited
//! world velocity cannot sweep the screen as the new path zooms in. The
//! correction starts with exactly the inherited velocity and fades to zero
//! value, slope and acceleration at landing. No position or velocity jumps,
//! and the flight still lands exactly on its target.

use super::{epoch as motion_epoch, now, reduced, request_frame};
use crate::probe::{self, TrackKind, TrackSample};
use gpui::{App, ElementId, Global, SharedString, Window};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A camera: the world point at the centre of the view and the visible
/// world width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// World x at the view's centre.
    pub x: f64,
    /// World y at the view's centre.
    pub y: f64,
    /// Visible world width (> 0).
    pub w: f64,
}

impl Camera {
    /// A camera at `(x, y)` seeing `w` world units across.
    #[must_use]
    pub const fn new(x: f64, y: f64, w: f64) -> Self {
        Self { x, y, w }
    }
}

/// The path's curvature: √2, van Wijk & Nuij's recommendation.
pub const RHO: f64 = std::f64::consts::SQRT_2;
/// The shortest flight.
pub const MIN_FLIGHT: Duration = Duration::from_millis(260);
/// The longest flight.
pub const MAX_FLIGHT: Duration = Duration::from_millis(1_100);
/// Reduced motion: the old framing crossfades out this fast instead.
pub const CROSSFADE: Duration = Duration::from_millis(120);

/// `ln(cosh(x))` without overflow.
fn ln_cosh(x: f64) -> f64 {
    let a = x.abs();
    a + (-2.0 * a).exp().ln_1p() - std::f64::consts::LN_2
}

/// An optimal pan-and-zoom path from `p0` to `p1`, parameterised by arc
/// length `s ∈ [0, S]` in van Wijk's metric.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Path {
    p0: Camera,
    p1: Camera,
    rho: f64,
    /// Unit pan direction (zero for a pure zoom).
    dir: (f64, f64),
    d1: f64,
    r0: f64,
    /// Pure zoom: the sign of ln(w1/w0).
    k: f64,
    length: f64,
}

impl Path {
    /// The path from `p0` to `p1` with ρ = √2.
    #[must_use]
    pub fn new(p0: Camera, p1: Camera) -> Self {
        Self::with_rho(p0, p1, RHO)
    }

    /// The path with another curvature.
    #[must_use]
    pub fn with_rho(p0: Camera, p1: Camera, rho: f64) -> Self {
        let (w0, w1) = (p0.w.max(1e-12), p1.w.max(1e-12));
        let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
        let d2 = dx * dx + dy * dy;
        let d1 = d2.sqrt();
        let (rho2, rho4) = (rho * rho, rho * rho * rho * rho);
        if d2 < 1e-12 {
            let ratio = (w1 / w0).ln();
            return Self {
                p0,
                p1,
                rho,
                dir: (0.0, 0.0),
                d1,
                r0: 0.0,
                k: if ratio < 0.0 { -1.0 } else { 1.0 },
                length: ratio.abs() / rho,
            };
        }
        let b0 = (w1 * w1 - w0 * w0 + rho4 * d2) / (2.0 * w0 * rho2 * d1);
        let b1 = (w1 * w1 - w0 * w0 - rho4 * d2) / (2.0 * w1 * rho2 * d1);
        let (r0, r1) = (-b0.asinh(), -b1.asinh());
        Self {
            p0,
            p1,
            rho,
            dir: (dx / d1, dy / d1),
            d1,
            r0,
            k: 1.0,
            length: (r1 - r0) / rho,
        }
    }

    /// The path length `S` (≥ 0).
    #[must_use]
    pub const fn length(&self) -> f64 {
        self.length
    }

    /// Where it starts.
    #[must_use]
    pub const fn start(&self) -> Camera {
        self.p0
    }

    /// Where it ends.
    #[must_use]
    pub const fn end(&self) -> Camera {
        self.p1
    }

    /// The camera `s` along the path; exactly `p0` at `s <= 0` and exactly
    /// `p1` at `s >= S`.
    #[must_use]
    pub fn at(&self, s: f64) -> Camera {
        if s <= 0.0 {
            return self.p0;
        }
        if s >= self.length {
            return self.p1;
        }
        self.analytic(s)
    }

    /// The formula itself, without the exact-endpoint clamp.
    fn analytic(&self, s: f64) -> Camera {
        let rho = self.rho;
        let w0 = self.p0.w.max(1e-12);
        if self.dir == (0.0, 0.0) {
            return Camera {
                x: self.p0.x,
                y: self.p0.y,
                w: w0 * (self.k * rho * s).exp(),
            };
        }
        let x = rho * s + self.r0;
        // Distance travelled along the pan line: w0/ρ² · sinh(ρs) / cosh(ρs + r0).
        let along = w0 / (rho * rho) * (rho * s).sinh() * (-ln_cosh(x)).exp();
        let w = w0 * (ln_cosh(self.r0) - ln_cosh(x)).exp();
        Camera {
            x: self.p0.x + self.dir.0 * along,
            y: self.p0.y + self.dir.1 * along,
            w,
        }
    }

    /// The camera at `t ∈ [0, 1]` of the path length.
    #[must_use]
    pub fn at_t(&self, t: f64) -> Camera {
        if t >= 1.0 {
            return self.p1;
        }
        self.at(t * self.length)
    }

    /// The flight time: `S` seconds (d3's pacing at ρ = √2), clamped to
    /// 260..=1100 ms.
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.length.max(0.0)).clamp(MIN_FLIGHT, MAX_FLIGHT)
    }

    /// Pan distance between the ends.
    #[must_use]
    pub const fn distance(&self) -> f64 {
        self.d1
    }
}

/// The purpose and stable spatial context of graph navigation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Travel {
    /// Approach a symbol through its enclosing package or module.
    Focus(Camera),
    /// Reveal a region through the larger territory it belongs to.
    Survey(Camera),
    /// Quietly correct a measured viewport or card without a scenic detour.
    Reframe,
}

/// Graph navigation: a bounded contextual lift, one spatially meaningful
/// bend, then an exact landing. The centre follows a projective endpoint
/// interpolation, so extreme zoom ratios cannot turn into a late pan sweep.
#[derive(Clone, Copy, Debug)]
pub struct GraphPath {
    metric: Path,
    travel: Travel,
    apex: f64,
    log_ratio: f64,
    lift: f64,
    normal: (f64, f64),
    bend: f64,
}

impl GraphPath {
    /// Build the route from its endpoints and semantic spatial context.
    #[must_use]
    pub fn new(from: Camera, to: Camera, travel: Travel) -> Self {
        let metric = Path::new(from, to);
        let distance = metric.distance();
        let widest = from.w.max(to.w);
        let (context, limit) = match travel {
            Travel::Focus(context) => (context, 0.085),
            Travel::Survey(context) => (context, 0.055),
            Travel::Reframe => (from, 0.0),
        };
        let apex = if matches!(travel, Travel::Reframe) {
            widest
        } else {
            widest
                .max(1.3 * distance)
                .max(context.w.min(widest + 1.8 * distance))
        };
        let normal = if distance > 1e-12 {
            (-(to.y - from.y) / distance, (to.x - from.x) / distance)
        } else {
            (0.0, 0.0)
        };
        let guide = normal.0 * (context.x - (from.x + to.x) / 2.0)
            + normal.1 * (context.y - (from.y + to.y) / 2.0);
        let bend = (0.3 * guide / apex).clamp(-limit, limit) * (3.0 * distance / apex).min(1.0);
        let rise = (apex / from.w).ln();
        let fall = (apex / to.w).ln();
        // The concave log-width arch has one apex, exactly at the selected
        // altitude. Its nonnegative lens sits above geometric endpoint
        // interpolation, which itself bounds the harmonic screen path.
        let lift = (rise + fall) / 4.0 + (rise * fall).sqrt() / 2.0;
        Self {
            metric,
            travel,
            apex,
            log_ratio: (to.w / from.w).ln(),
            lift,
            normal,
            bend,
        }
    }

    /// Sample normalized spatial progress (the flight clock supplies pacing).
    #[must_use]
    pub fn at_t(&self, progress: f64) -> Camera {
        let from = self.metric.start();
        let to = self.metric.end();
        if progress <= 0.0 {
            return from;
        }
        if progress >= 1.0 {
            return to;
        }
        let p = progress;
        let denominator = (1.0 - p) * to.w + p * from.w;
        let q = p * from.w / denominator;
        let remaining = (1.0 - p) * to.w / denominator;
        let harmonic = from.w * (to.w / denominator);
        let (x, y) = if q <= 0.5 {
            (from.x + (to.x - from.x) * q, from.y + (to.y - from.y) * q)
        } else {
            (
                to.x - (to.x - from.x) * remaining,
                to.y - (to.y - from.y) * remaining,
            )
        };
        if matches!(self.travel, Travel::Reframe) {
            return Camera::new(x, y, harmonic);
        }
        let lens = 4.0 * self.lift * p * (1.0 - p);
        // Evaluate from the nearer endpoint to retain narrow landing
        // precision, without a soft-max blend that can introduce extra
        // zoom turns. The clock leaves and lands at C2 rest.
        let w = if p <= 0.5 {
            from.w * (p * self.log_ratio + lens).exp()
        } else {
            to.w * ((p - 1.0) * self.log_ratio + lens).exp()
        };
        let arc = w * self.bend * 4.0 * p * (1.0 - p);
        Camera::new(x + self.normal.0 * arc, y + self.normal.1 * arc, w)
    }
}

#[derive(Clone, Copy, Debug)]
enum Route {
    Optimal(Path),
    Graph(GraphPath),
}

impl Route {
    fn start(self) -> Camera {
        match self {
            Self::Optimal(p) => p.start(),
            Self::Graph(p) => p.metric.start(),
        }
    }
    fn end(self) -> Camera {
        match self {
            Self::Optimal(p) => p.end(),
            Self::Graph(p) => p.metric.end(),
        }
    }
    fn length(self) -> f64 {
        match self {
            Self::Optimal(p) => p.length(),
            Self::Graph(p) => p.metric.length(),
        }
    }
    fn at_t(self, t: f64) -> Camera {
        match self {
            Self::Optimal(p) => p.at_t(t),
            Self::Graph(p) => p.at_t(t),
        }
    }
    fn travel(self) -> Option<Travel> {
        match self {
            Self::Optimal(_) => None,
            Self::Graph(p) => Some(p.travel),
        }
    }
    fn envelope(self) -> (f64, (f64, f64)) {
        match self {
            Self::Optimal(p) => (
                p.at((-p.r0 / p.rho).clamp(0.0, p.length()))
                    .w
                    .max(p.start().w)
                    .max(p.end().w),
                (0.0, 0.0),
            ),
            Self::Graph(p) => (
                p.apex,
                (
                    (p.normal.0 * p.bend).abs() * p.apex,
                    (p.normal.1 * p.bend).abs() * p.apex,
                ),
            ),
        }
    }
}

/// Ramp share at each end of the speed profile.
const RAMP: f64 = 0.18;

/// The flight's ease: constant speed along the (already optimal) path,
/// with half-cosine ramps over the first and last 18 % so the camera leaves
/// and lands at rest. Continuous in velocity and acceleration; peak speed is
/// 1 / (1 − 0.18) ≈ 1.22 × the mean.
#[must_use]
pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let speed = 1.0 / (1.0 - RAMP);
    let ramp = |t: f64| {
        speed
            * (t / 2.0
                - RAMP / (2.0 * std::f64::consts::PI) * (std::f64::consts::PI * t / RAMP).sin())
    };
    if t < RAMP {
        ramp(t)
    } else if t <= 1.0 - RAMP {
        speed * (RAMP / 2.0 + (t - RAMP))
    } else {
        1.0 - ramp(1.0 - t)
    }
}

/// How a flight is paced: its duration from the path length `S`, and the
/// ease on `t` along the (already velocity-optimal) path. Both eases must
/// leave and land at rest (zero slope at 0 and 1): the re-plan carries the
/// interrupted flight's velocity itself.
#[derive(Clone, Copy, Debug)]
pub struct Pacing {
    /// Flight time for a path of length `S`.
    pub duration: fn(f64) -> Duration,
    /// Progress along the path at linear time `t ∈ [0, 1]`.
    pub ease: fn(f64) -> f64,
}

impl Pacing {
    /// `S` seconds clamped to 260..=1100 ms, the ramp/constant/ramp [`ease`]
    /// (d3's pacing at ρ = √2).
    pub const DEFAULT: Self = Self {
        duration: |s| Duration::from_secs_f64(s.max(0.0)).clamp(MIN_FLIGHT, MAX_FLIGHT),
        ease,
    };
    /// Contextual graph travel: `170·S + 260` ms clamped to 320..=1250 ms,
    /// with the C2 ramp/constant/ramp clock. The contextual lens owns its
    /// separate lift and landing stages; double clock easing compresses them.
    pub const GRAPH_TRAVEL: Self = Self {
        duration: |s| Duration::from_secs_f64((0.17 * s.max(0.0) + 0.26).clamp(0.32, 1.25)),
        ease,
    };

    /// Original graph prototype pacing, retained for callers of optimal paths.
    pub const GRAPH: Self = Self {
        duration: |s| Duration::from_secs_f64((0.21 * s.max(0.0) + 0.26).clamp(0.32, 1.5)),
        ease: |t| (1.0 - (std::f64::consts::PI * t.clamp(0.0, 1.0)).cos()) / 2.0,
    };
}

impl Default for Pacing {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One flight in progress: an eased path plus the velocity correction it
/// inherited from an interrupted flight.
#[derive(Clone, Copy, Debug)]
struct Trip {
    route: Route,
    start: Instant,
    duration: Duration,
    ease: fn(f64) -> f64,
    /// Velocity (x, y, ln w per second) the path lacks at its start.
    carry: (f64, f64, f64),
    /// Bounded momentum response in seconds, cached from normalized velocity.
    horizon: f64,
    /// Cached absolute excursion outside the endpoint interval on each axis.
    overshoot: [f32; 3],
}

impl Trip {
    fn new(path: Path, start: Instant, carry: (f64, f64, f64), pacing: Pacing) -> Self {
        Self::along(Route::Optimal(path), start, carry, pacing)
    }

    fn along(path: Route, start: Instant, carry: (f64, f64, f64), pacing: Pacing) -> Self {
        let moving = carry.0.abs() + carry.1.abs() + carry.2.abs() > 1e-9;
        let duration = if path.length() <= 1e-12 && !moving {
            Duration::ZERO
        } else {
            (pacing.duration)(path.length())
        };
        // A bounded inherited response, independent of the flight's long
        // duration. Keep the initial derivative, but never permit a rapid
        // wheel or drag to accumulate an enormous camera displacement.
        let width = path.start().w.max(1e-12);
        let pan = carry.0.abs().max(carry.1.abs()) / width;
        let horizon = 0.07_f64
            .min(duration.as_secs_f64() / 4.0)
            .min(0.15 / pan)
            .min(1.25_f64.ln() / carry.2.abs());
        // h = T(1−e^(−τ/T)) · (1−u)³(1+3u+6u²), u=τ/D.
        // 0≤h≤T; pan is scaled by the full sampled width / start width.
        // Base width peaks where ρs+r0=0; cache the rigorous world envelope.
        let h = horizon;
        let (peak, base_pan) = path.envelope();
        let (low, high) = (
            path.start().w.min(path.end().w),
            path.start().w.max(path.end().w),
        );
        let log_hi = (carry.2 * h).max(0.0);
        let log_lo = (carry.2 * h).min(0.0);
        let peak = peak * log_hi.exp();
        #[allow(clippy::cast_possible_truncation)]
        let overshoot = [
            (base_pan.0 + carry.0.abs() * h * peak / width) as f32,
            (base_pan.1 + carry.1.abs() * h * peak / width) as f32,
            (peak - high).max(low * -log_lo.exp_m1()).max(0.0) as f32,
        ];
        Self {
            route: path,
            start,
            duration,
            ease: pacing.ease,
            carry,
            horizon,
            overshoot,
        }
    }

    fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= self.duration
    }

    /// The camera `tau` seconds in.
    fn at(&self, tau: f64) -> Camera {
        let d = self.duration.as_secs_f64();
        if tau >= d {
            return self.route.end();
        }
        let tau = tau.max(0.0);
        let camera = self.route.at_t((self.ease)(tau / d));
        let (h, _) = self.response(tau);
        let w = camera.w * (self.carry.2 * h).exp();
        let pan = w / self.route.start().w.max(1e-12) * h;
        Camera {
            x: camera.x + self.carry.0 * pan,
            y: camera.y + self.carry.1 * pan,
            w,
        }
    }

    /// Bounded carry and its exact slope. Differentiate this fast
    /// response analytically; a fixed finite-difference step would erase
    /// the derivative of an extreme input's very short carry horizon.
    fn response(&self, tau: f64) -> (f64, f64) {
        let d = self.duration.as_secs_f64();
        let u = tau / d;
        let remaining = 1.0 - u;
        let fade = remaining.powi(3) * (1.0 + 3.0 * u + 6.0 * u * u);
        let fade_speed = -30.0 * u * u * remaining * remaining / d;
        let response = self.horizon * -(-tau / self.horizon).exp_m1();
        let speed = (-tau / self.horizon).exp();
        (response * fade, speed * fade + response * fade_speed)
    }

    fn sample(&self, now: Instant) -> Camera {
        self.at(now.saturating_duration_since(self.start).as_secs_f64())
    }

    /// (dx/dt, dy/dt, d ln w / dt) at `now`. The smooth base path
    /// uses a central difference; the bounded carry has an exact derivative.
    fn velocity(&self, now: Instant) -> (f64, f64, f64) {
        let tau = now.saturating_duration_since(self.start).as_secs_f64();
        let d = self.duration.as_secs_f64();
        if tau >= d || d <= 0.0 {
            return (0.0, 0.0, 0.0);
        }
        if tau <= 0.0 {
            return self.carry;
        }
        let h = 1e-5_f64.min(d / 2.0).min(tau / 2.0).min((d - tau) / 2.0);
        let (lo, hi) = ((tau - h).max(0.0), (tau + h).min(d));
        let (a, b) = (
            self.route.at_t((self.ease)(lo / d)),
            self.route.at_t((self.ease)(hi / d)),
        );
        let base = self.route.at_t((self.ease)(tau / d));
        let span = hi - lo;
        let (carry, carry_speed) = self.response(tau);
        let zoom = (b.w.ln() - a.w.ln()) / span + self.carry.2 * carry_speed;
        let w = base.w * (self.carry.2 * carry).exp();
        let pan_speed = w / self.route.start().w.max(1e-12) * (carry_speed + carry * zoom);
        (
            (b.x - a.x) / span + self.carry.0 * pan_speed,
            (b.y - a.y) / span + self.carry.1 * pan_speed,
            zoom,
        )
    }
}

/// Plans a flight from `current` (moving at `velocity`) to `target`.
fn plan(
    current: Camera,
    velocity: (f64, f64, f64),
    target: Camera,
    now: Instant,
    pacing: Pacing,
) -> Trip {
    let path = Path::new(current, target);
    let fresh = Trip::new(path, now, (0.0, 0.0, 0.0), pacing);
    let start = fresh.velocity(now);
    let carry = (
        velocity.0 - start.0,
        velocity.1 - start.1,
        velocity.2 - start.2,
    );
    Trip::new(path, now, carry, pacing)
}

fn plan_travel(
    current: Camera,
    velocity: (f64, f64, f64),
    target: Camera,
    now: Instant,
    pacing: Pacing,
    travel: Travel,
) -> Trip {
    Trip::along(
        Route::Graph(GraphPath::new(current, target, travel)),
        now,
        velocity,
        pacing,
    )
}

#[derive(Clone, Copy, Debug)]
enum State {
    Still(Camera),
    Flying(Trip),
    Fading {
        from: Camera,
        to: Camera,
        start: Instant,
    },
}

/// A frame of a flight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot {
    /// The camera to draw the world with.
    pub camera: Camera,
    /// Reduced motion: the framing being crossfaded out, drawn at
    /// `1 - fade` under the new one.
    pub from: Option<Camera>,
    /// The new framing's opacity (1 except during a reduced-motion
    /// crossfade).
    pub fade: f32,
    /// Whether it still moves.
    pub live: bool,
}

/// Keyed, interruptible camera flights (see the [module docs](self)).
/// Cloning shares the store.
#[derive(Clone, Default)]
pub struct Flights {
    store: Rc<RefCell<HashMap<ElementId, State>>>,
    pacing: Pacing,
}

impl Flights {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The store registered under `scope`, shared by every view that asks.
    pub fn scoped(scope: impl Into<SharedString>, cx: &mut App) -> Self {
        cx.default_global::<Scopes>()
            .0
            .entry(scope.into())
            .or_default()
            .clone()
    }

    /// The same store paced otherwise ([`Pacing::GRAPH`] for the graph).
    #[must_use]
    pub fn paced(mut self, pacing: Pacing) -> Self {
        self.pacing = pacing;
        self
    }

    /// Flies `key` towards `target` and returns this frame's shot. A key's
    /// first sighting is not animated; a changed target re-plans from where
    /// the camera is, keeping its velocity.
    pub fn fly(
        &self,
        key: impl Into<ElementId>,
        target: Camera,
        window: &mut Window,
        cx: &mut App,
    ) -> Shot {
        self.fly_impl(key.into(), target, None, window, cx)
    }

    /// Fly the graph along a contextual route without changing generic flight behavior.
    pub fn fly_travel(
        &self,
        key: impl Into<ElementId>,
        target: Camera,
        travel: Travel,
        window: &mut Window,
        cx: &mut App,
    ) -> Shot {
        self.fly_impl(key.into(), target, Some(travel), window, cx)
    }

    fn fly_impl(
        &self,
        key: ElementId,
        target: Camera,
        travel: Option<Travel>,
        window: &mut Window,
        cx: &mut App,
    ) -> Shot {
        let now = now(cx);
        let reduced = reduced(cx);
        let (shot, trip) = {
            let mut store = self.store.borrow_mut();
            let state = store.entry(key.clone()).or_insert(State::Still(target));
            if let Some(travel) = travel {
                step_with(state, target, now, reduced, self.pacing, Some(travel))
            } else {
                step(state, target, now, reduced, self.pacing)
            }
        };
        if shot.live {
            request_frame(window, cx);
        }
        if probe::enabled(cx) {
            publish(cx, &key, &shot, trip.as_ref(), target, now);
        }
        shot
    }

    /// Starts a flight from a directly manipulated camera and its current
    /// velocity (world units/s and log-width/s). Like an interrupted flight,
    /// it starts at that exact camera and carries the velocity into the path.
    pub fn fly_from(
        &self,
        key: impl Into<ElementId>,
        camera: Camera,
        velocity: (f64, f64, f64),
        target: Camera,
        window: &mut Window,
        cx: &mut App,
    ) -> Shot {
        let key = key.into();
        let trip = plan(camera, velocity, target, now(cx), self.pacing);
        let state = if reduced(cx) {
            State::Still(camera)
        } else {
            State::Flying(trip)
        };
        self.store.borrow_mut().insert(key.clone(), state);
        self.fly(key, target, window, cx)
    }

    /// Seed contextual graph travel with a directly manipulated camera and velocity.
    pub fn fly_travel_from(
        &self,
        key: impl Into<ElementId>,
        camera: Camera,
        velocity: (f64, f64, f64),
        target: Camera,
        travel: Travel,
        window: &mut Window,
        cx: &mut App,
    ) -> Shot {
        let key = key.into();
        let trip = plan_travel(camera, velocity, target, now(cx), self.pacing, travel);
        let state = if reduced(cx) {
            State::Still(camera)
        } else {
            State::Flying(trip)
        };
        self.store.borrow_mut().insert(key.clone(), state);
        self.fly_travel(key, target, travel, window, cx)
    }

    /// Puts `key` at `camera` at rest, at once: safe mid-flight (the flight
    /// is dropped with its velocity). For direct manipulation (drag, wheel)
    /// jump every frame and pass the same camera to [`Flights::fly`], or do
    /// not fly that key while dragging; a later `fly` to another target
    /// flies from where the jump left it.
    pub fn jump(&self, key: impl Into<ElementId>, camera: Camera) {
        self.store
            .borrow_mut()
            .insert(key.into(), State::Still(camera));
    }

    /// Lands immediately, publishing the final probe sample without asking
    /// for a frame. Used when the view cannot paint a reduced-motion fade.
    pub fn snap(&self, key: impl Into<ElementId>, camera: Camera, cx: &mut App) {
        let key = key.into();
        let trip = self.store.borrow().get(&key).and_then(|state| match state {
            State::Flying(trip) => Some(*trip),
            _ => None,
        });
        self.jump(key.clone(), camera);
        if probe::enabled(cx) {
            let shot = Shot {
                camera,
                from: None,
                fade: 1.0,
                live: false,
            };
            publish(cx, &key, &shot, trip.as_ref(), camera, now(cx));
        }
    }

    /// Where `key`'s camera is now, without stepping it (picking, input
    /// between frames). `None` for a key never flown or jumped.
    #[must_use]
    pub fn camera(&self, key: impl Into<ElementId>, cx: &App) -> Option<Camera> {
        let now = now(cx);
        self.store
            .borrow()
            .get(&key.into())
            .map(|state| match *state {
                State::Still(camera) => camera,
                State::Flying(trip) => trip.sample(now),
                State::Fading { to, .. } => to,
            })
    }

    /// Where `key`'s camera is heading. `None` for an unknown key.
    #[must_use]
    pub fn target(&self, key: impl Into<ElementId>) -> Option<Camera> {
        self.store.borrow().get(&key.into()).map(target_of)
    }

    /// Whether every flight has landed.
    #[must_use]
    pub fn is_settled(&self, cx: &App) -> bool {
        let now = now(cx);
        self.store.borrow().values().all(|state| match state {
            State::Still(_) => true,
            State::Flying(trip) => trip.done(now),
            State::Fading { start, .. } => now.saturating_duration_since(*start) >= CROSSFADE,
        })
    }
}

#[derive(Default)]
struct Scopes(HashMap<SharedString, Flights>);

impl Global for Scopes {}

fn target_of(state: &State) -> Camera {
    match state {
        State::Still(camera) => *camera,
        State::Flying(trip) => trip.route.end(),
        State::Fading { to, .. } => *to,
    }
}

/// Advances one keyed flight to `now` towards `target`.
fn step(
    state: &mut State,
    target: Camera,
    now: Instant,
    reduced: bool,
    pacing: Pacing,
) -> (Shot, Option<Trip>) {
    step_with(state, target, now, reduced, pacing, None)
}

fn step_with(
    state: &mut State,
    target: Camera,
    now: Instant,
    reduced: bool,
    pacing: Pacing,
    travel: Option<Travel>,
) -> (Shot, Option<Trip>) {
    if target_of(state) != target
        || matches!(state, State::Flying(trip) if trip.route.travel() != travel)
        || (reduced && matches!(state, State::Flying(_)))
    {
        // Only a changed destination needs the inherited derivative. An
        // ordinary frame samples its route once below, with no unused
        // finite-difference samples or duplicate momentum response.
        let (current, velocity) = match *state {
            State::Still(camera) => (camera, (0.0, 0.0, 0.0)),
            State::Flying(trip) if trip.done(now) => (trip.route.end(), (0.0, 0.0, 0.0)),
            State::Flying(trip) => (trip.sample(now), trip.velocity(now)),
            State::Fading { to, .. } => (to, (0.0, 0.0, 0.0)),
        };
        *state = if reduced {
            State::Fading {
                from: current,
                to: target,
                start: now,
            }
        } else {
            let trip = travel.map_or_else(
                || plan(current, velocity, target, now, pacing),
                |travel| plan_travel(current, velocity, target, now, pacing, travel),
            );
            if trip.duration.is_zero() {
                State::Still(target)
            } else {
                State::Flying(trip)
            }
        };
    }
    match *state {
        State::Still(camera) => (
            Shot {
                camera,
                from: None,
                fade: 1.0,
                live: false,
            },
            None,
        ),
        State::Flying(trip) if trip.done(now) => {
            *state = State::Still(trip.route.end());
            (
                Shot {
                    camera: trip.route.end(),
                    from: None,
                    fade: 1.0,
                    live: false,
                },
                Some(trip),
            )
        }
        State::Flying(trip) => (
            Shot {
                camera: trip.sample(now),
                from: None,
                fade: 1.0,
                live: true,
            },
            Some(trip),
        ),
        State::Fading { from, to, start } => {
            let run = now.saturating_duration_since(start);
            if run >= CROSSFADE {
                *state = State::Still(to);
                return (
                    Shot {
                        camera: to,
                        from: None,
                        fade: 1.0,
                        live: false,
                    },
                    None,
                );
            }
            #[allow(clippy::cast_possible_truncation)]
            let fade = (run.as_secs_f64() / CROSSFADE.as_secs_f64()) as f32;
            (
                Shot {
                    camera: to,
                    from: Some(from),
                    fade,
                    live: true,
                },
                None,
            )
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn publish(
    cx: &mut App,
    key: &ElementId,
    shot: &Shot,
    trip: Option<&Trip>,
    target: Camera,
    now: Instant,
) {
    let epoch = motion_epoch(cx);
    let millis = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.0;
    let (started_ms, budget_ms) = trip.map_or((0.0, 0.0), |trip| {
        (millis(trip.start), trip.duration.as_secs_f64() * 1000.0)
    });
    let velocity = trip
        .filter(|_| shot.live)
        .map_or((0.0, 0.0, 0.0), |trip| trip.velocity(now));
    let group = probe::current_group();
    let at_ms = millis(now);
    for (index, (axis, value, goal, speed)) in [
        ("x", shot.camera.x, target.x, velocity.0),
        ("y", shot.camera.y, target.y, velocity.1),
        ("w", shot.camera.w, target.w, velocity.2 * shot.camera.w),
    ]
    .into_iter()
    .enumerate()
    {
        let overshoot = trip.map_or(0.0, |trip| trip.overshoot[index]);
        probe::record_track(cx, || TrackSample {
            key: format!("{key}.{axis}"),
            kind: TrackKind::Tween,
            value: value as f32,
            target: goal as f32,
            velocity: speed as f32,
            started_ms,
            budget_ms,
            at_ms,
            live: shot.live,
            overshoot_ratio: 0.0,
            overshoot_absolute: overshoot,
            group: group.clone(),
        });
    }
}

#[cfg(test)]
#[path = "flight/properties.rs"]
mod properties;

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::{Camera, Pacing, Path, State, Trip, ease, plan, step};
    use std::time::{Duration, Instant};

    fn close(a: Camera, b: Camera, tolerance: f64) -> bool {
        (a.x - b.x).abs() <= tolerance
            && (a.y - b.y).abs() <= tolerance
            && (a.w - b.w).abs() <= tolerance
    }

    #[test]
    fn endpoints_are_exact_and_the_formula_reaches_them() {
        let (p0, p1) = (Camera::new(-3.0, 2.0, 4.0), Camera::new(40.0, -7.5, 1.5));
        let path = Path::new(p0, p1);
        assert_eq!(path.at_t(0.0), p0);
        assert_eq!(path.at_t(1.0), p1);
        // Not just the clamp: the formula itself lands on both ends.
        assert!(close(path.analytic(0.0), p0, 1e-12));
        assert!(
            close(path.analytic(path.length()), p1, 1e-9),
            "{:?}",
            path.analytic(path.length())
        );
        let zoom = Path::new(Camera::new(1.0, 1.0, 8.0), Camera::new(1.0, 1.0, 0.5));
        assert!(close(zoom.analytic(zoom.length()), zoom.end(), 1e-12));
    }

    /// The pan distance going to zero, on both sides of the branch at
    /// d² = 1e-12 (d = 1e-6): the general formula just above it must agree
    /// with the zoom-only branch (1e-9 alone would only test that branch).
    #[test]
    fn the_zero_pan_limit_is_continuous() {
        for (w0, w1) in [(2.0, 9.0), (9.0, 2.0), (3.0, 3.0 + 1e-6)] {
            let zero = Path::new(Camera::new(0.0, 0.0, w0), Camera::new(0.0, 0.0, w1));
            for d in [1e-9, 0.999e-6, 1.001e-6, 3e-6, 1e-5] {
                let tiny = Path::new(Camera::new(0.0, 0.0, w0), Camera::new(d, 0.0, w1));
                assert!(
                    (zero.length() - tiny.length()).abs() < 1e-5,
                    "d={d}: S {} vs {}",
                    zero.length(),
                    tiny.length()
                );
                for i in 0..=20 {
                    let t = f64::from(i) / 20.0;
                    let (a, b) = (zero.at_t(t), tiny.at_t(t));
                    assert!(
                        close(a, b, 1e-5 * w0.max(w1)),
                        "d={d} t={t}: {a:?} vs {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn path_length_is_symmetric() {
        for (p0, p1) in [
            (Camera::new(0.0, 0.0, 1.0), Camera::new(10.0, 0.0, 1.0)),
            (Camera::new(-5.0, 3.0, 0.2), Camera::new(8.0, -1.0, 6.0)),
            (Camera::new(2.0, 2.0, 5.0), Camera::new(2.0, 2.0, 0.5)),
        ] {
            let (there, back) = (Path::new(p0, p1).length(), Path::new(p1, p0).length());
            assert!(there >= 0.0);
            assert!(
                (there - back).abs() < 1e-9 * there.max(1.0),
                "{there} vs {back}"
            );
        }
    }

    #[test]
    fn a_long_pan_zooms_out_once_and_back_in() {
        let path = Path::new(Camera::new(0.0, 0.0, 1.0), Camera::new(50.0, 0.0, 1.0));
        let widths: Vec<f64> = (0..=200)
            .map(|i| path.at_t(f64::from(i) / 200.0).w)
            .collect();
        let peak = widths
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(i, _)| i);
        assert!(widths[peak] > 10.0, "it zooms well out: {}", widths[peak]);
        assert!(
            peak > 50 && peak < 150,
            "the peak is between the ends: {peak}"
        );
        assert!(
            widths[..=peak].windows(2).all(|w| w[1] >= w[0] - 1e-12),
            "rises"
        );
        assert!(
            widths[peak..].windows(2).all(|w| w[1] <= w[0] + 1e-12),
            "then falls"
        );
        // And it travels monotonically along the pan.
        let xs: Vec<f64> = (0..=200)
            .map(|i| path.at_t(f64::from(i) / 200.0).x)
            .collect();
        assert!(xs.windows(2).all(|x| x[1] >= x[0] - 1e-12));
    }

    #[test]
    fn duration_follows_length_within_bounds() {
        let short = Path::new(Camera::new(0.0, 0.0, 1.0), Camera::new(0.01, 0.0, 1.0));
        let long = Path::new(Camera::new(0.0, 0.0, 1.0), Camera::new(1e6, 0.0, 1.0));
        assert_eq!(short.duration(), super::MIN_FLIGHT);
        assert_eq!(long.duration(), super::MAX_FLIGHT);
        let mid = Path::new(Camera::new(0.0, 0.0, 1.0), Camera::new(0.0, 0.0, 2.0));
        let expected = (2.0_f64.ln() / std::f64::consts::SQRT_2).max(0.26);
        assert!((mid.duration().as_secs_f64() - expected).abs() < 1e-6);
    }

    #[test]
    fn the_ease_leaves_and_lands_at_rest_and_is_smooth() {
        assert_eq!(ease(0.0), 0.0);
        assert!((ease(1.0) - 1.0).abs() < 1e-12);
        let slope = |t: f64| (ease(t + 1e-6) - ease(t - 1e-6)) / 2e-6;
        assert!(slope(1e-6) < 1e-3 && slope(1.0 - 1e-6) < 1e-3);
        for t in [0.18, 0.82] {
            assert!(
                (slope(t - 1e-4) - slope(t + 1e-4)).abs() < 1e-3,
                "C1 at {t}"
            );
        }
        assert!((slope(0.5) - 1.0 / 0.82).abs() < 1e-6);
    }

    fn velocity(trip: &Trip, now: Instant) -> (f64, f64, f64) {
        trip.velocity(now)
    }

    /// Re-planning at an instant: the camera and its velocity (sampled by
    /// central differences on both sides) do not jump, and it still lands
    /// exactly on the new target.
    #[test]
    fn replanning_mid_flight_keeps_position_and_velocity() {
        for pacing in [Pacing::DEFAULT, Pacing::GRAPH] {
            replan_under(pacing);
        }
    }

    fn replan_under(pacing: Pacing) {
        let t0 = Instant::now();
        let first = plan(
            Camera::new(0.0, 0.0, 2.0),
            (0.0, 0.0, 0.0),
            Camera::new(30.0, 10.0, 1.0),
            t0,
            pacing,
        );
        let mid = t0 + first.duration.mul_f64(0.4);
        let (at, v) = (first.sample(mid), velocity(&first, mid));
        assert!(v.0.abs() > 1.0, "it was moving: {v:?}");
        let second = plan(at, v, Camera::new(-20.0, 5.0, 4.0), mid, pacing);
        assert!(close(second.sample(mid), at, 1e-9), "no position jump");
        let w = velocity(&second, mid + Duration::from_nanos(20_000));
        let before = velocity(&first, mid - Duration::from_nanos(20_000));
        for (a, b) in [(before.0, w.0), (before.1, w.1), (before.2, w.2)] {
            assert!(
                (a - b).abs() < 1e-2 * a.abs().max(1.0),
                "velocity jump: {before:?} -> {w:?}"
            );
        }
        let end = second.start + second.duration;
        assert_eq!(second.sample(end), Camera::new(-20.0, 5.0, 4.0));
        assert_eq!(velocity(&second, end), (0.0, 0.0, 0.0));
    }

    /// Retargets many times per second through the store: sampled every
    /// millisecond, the camera never jumps (step bounded by the fastest
    /// speed), velocities stay finite, and a neutral tail lands exactly.
    #[test]
    fn a_retarget_storm_is_continuous_and_lands_exactly() {
        for pacing in [Pacing::DEFAULT, Pacing::GRAPH] {
            storm_under(pacing);
        }
    }

    fn storm_under(pacing: Pacing) {
        let t0 = Instant::now();
        let mut state = State::Still(Camera::new(0.0, 0.0, 1.0));
        let mut rng = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            rng ^= rng >> 12;
            rng ^= rng << 25;
            rng ^= rng >> 27;
            rng.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        #[allow(clippy::cast_precision_loss)]
        let mut unit = move || (next() % 10_000) as f64 / 10_000.0;
        let mut target = Camera::new(0.0, 0.0, 1.0);
        let mut last = Camera::new(0.0, 0.0, 1.0);
        for ms in 0..4_000_u64 {
            let now = t0 + Duration::from_millis(ms);
            if ms < 3_000 && unit() < 0.02 {
                target = Camera::new(
                    unit() * 100.0 - 50.0,
                    unit() * 60.0 - 30.0,
                    0.5 + unit() * 20.0,
                );
            }
            let (shot, _) = step(&mut state, target, now, false, pacing);
            let c = shot.camera;
            assert!(c.x.is_finite() && c.y.is_finite() && c.w > 0.0, "{c:?}");
            let jump = ((c.x - last.x).powi(2) + (c.y - last.y).powi(2)).sqrt();
            // Nothing crosses more than a view and a half in one millisecond.
            assert!(
                jump < 1.5 * last.w.max(c.w),
                "ms {ms}: jumped {jump} at w {}",
                c.w
            );
            last = c;
        }
        let (shot, _) = step(
            &mut state,
            target,
            t0 + Duration::from_secs(9),
            false,
            pacing,
        );
        assert_eq!(shot.camera, target);
        assert!(!shot.live);
    }

    #[test]
    fn reduced_motion_crossfades_instead_of_flying() {
        let t0 = Instant::now();
        let start = Camera::new(0.0, 0.0, 1.0);
        let goal = Camera::new(40.0, 0.0, 3.0);
        let mut state = State::Still(start);
        let (shot, _) = step(&mut state, goal, t0, true, Pacing::DEFAULT);
        assert_eq!(shot.camera, goal, "no flight: the new framing at once");
        assert_eq!(shot.from, Some(start));
        assert_eq!(shot.fade, 0.0);
        let (half, _) = step(
            &mut state,
            goal,
            t0 + Duration::from_millis(60),
            true,
            Pacing::DEFAULT,
        );
        assert!((half.fade - 0.5).abs() < 1e-3);
        let (done, _) = step(
            &mut state,
            goal,
            t0 + Duration::from_millis(120),
            true,
            Pacing::DEFAULT,
        );
        assert_eq!((done.from, done.fade, done.live), (None, 1.0, false));
    }

    #[test]
    fn a_direct_motion_seed_has_its_full_velocity_at_the_first_sample() {
        let start = Instant::now();
        let camera = Camera::new(17.0, -11.0, 200.0);
        let velocity = (130.0, -75.0, -2.3);
        let trip = plan(
            camera,
            velocity,
            Camera::new(500.0, 300.0, 40.0),
            start,
            Pacing::GRAPH,
        );
        assert_eq!(trip.sample(start), camera);
        assert_eq!(trip.velocity(start), velocity, "no half-speed first sample");
        let delta = Duration::from_nanos(100);
        let after = trip.sample(start + delta);
        let derivative = (
            (after.x - camera.x) / delta.as_secs_f64(),
            (after.y - camera.y) / delta.as_secs_f64(),
            (after.w.ln() - camera.w.ln()) / delta.as_secs_f64(),
        );
        for (got, want) in [
            (derivative.0, velocity.0),
            (derivative.1, velocity.1),
            (derivative.2, velocity.2),
        ] {
            assert!((got - want).abs() < 0.002, "{derivative:?} vs {velocity:?}");
        }
        assert_eq!(
            trip.sample(start + trip.duration),
            Camera::new(500.0, 300.0, 40.0)
        );
        assert_eq!(trip.velocity(start + trip.duration), (0.0, 0.0, 0.0));
    }

    #[test]
    fn reduced_motion_mid_flight_starts_a_crossfade_and_lands() {
        let start = Instant::now();
        let from = Camera::new(0.0, 0.0, 400.0);
        let to = Camera::new(500.0, 300.0, 40.0);
        let mut state = State::Still(from);
        step(&mut state, to, start, false, Pacing::GRAPH);
        let at = start + Duration::from_millis(80);
        let (before, _) = step(&mut state, to, at, false, Pacing::GRAPH);
        let (reduced, _) = step(&mut state, to, at, true, Pacing::GRAPH);
        assert_eq!(reduced.camera, to);
        assert_eq!(reduced.from, Some(before.camera));
        let (landed, _) = step(&mut state, to, at + super::CROSSFADE, true, Pacing::GRAPH);
        assert_eq!(landed.camera, to);
        assert!(!landed.live);
    }

    #[test]
    fn equal_width_flights_publish_a_finite_geometric_envelope() {
        let at = Instant::now();
        let path = Path::new(Camera::new(0.0, 0.0, 2.0), Camera::new(30.0, 0.0, 2.0));
        let trip = Trip::new(path, at, (0.0, 0.0, 0.0), Pacing::GRAPH);
        // For equal widths and ρ²=2, the midpoint width is √(w²+d²).
        let expected = 904.0_f64.sqrt() - 2.0;
        assert!((f64::from(trip.overshoot[2]) - expected).abs() < 1e-5);
        assert!(trip.overshoot[2].is_finite() && trip.overshoot[2] < 100.0);
        assert_eq!(trip.overshoot[0], 0.0);
        assert_eq!(trip.overshoot[1], 0.0);
        assert!((trip.sample(at + trip.duration / 2).w - 904.0_f64.sqrt()).abs() < 1e-8);

        // A same-position target with inherited drag/zoom velocity must be
        // checked too: it leaves the framing, then returns exactly.
        let loop_trip = plan(
            path.start(),
            (130.0, -75.0, -2.3),
            path.start(),
            at,
            Pacing::GRAPH,
        );
        for ms in 0..=320 {
            let c = loop_trip.sample(at + Duration::from_millis(ms));
            for (value, base, allowance) in [
                (c.x, 0.0, loop_trip.overshoot[0]),
                (c.y, 0.0, loop_trip.overshoot[1]),
                (c.w, 2.0, loop_trip.overshoot[2]),
            ] {
                assert!(
                    (value - base).abs() <= f64::from(allowance) + 1e-6,
                    "outside cached envelope: {c:?}"
                );
            }
        }
        assert_eq!(loop_trip.sample(at + loop_trip.duration), path.start());
    }

    #[test]
    fn graph_pacing_matches_the_prototype() {
        // 210·S + 260 ms in [320, 1500], sine ease.
        assert_eq!((Pacing::GRAPH.duration)(0.0), Duration::from_millis(320));
        assert_eq!((Pacing::GRAPH.duration)(2.0), Duration::from_millis(680));
        assert_eq!((Pacing::GRAPH.duration)(100.0), Duration::from_millis(1500));
        assert!(((Pacing::GRAPH.ease)(0.5) - 0.5).abs() < 1e-12);
        assert!(
            ((Pacing::GRAPH.ease)(0.25) - (1.0 - (std::f64::consts::PI / 4.0).cos()) / 2.0).abs()
                < 1e-12
        );
        assert_eq!((Pacing::GRAPH.ease)(0.0), 0.0);
        assert_eq!((Pacing::GRAPH.ease)(1.0), 1.0);
    }
}
