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
//! starting velocity is carried by a correction `Δv·τ·(1 − τ/D)²` (in log-w
//! for the zoom) that starts with exactly that velocity and is exactly zero,
//! with zero velocity, when the new flight ends. No position or velocity
//! jumps, and the flight still lands exactly on its target.

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
        Duration::from_secs_f64(self.length.max(0.0))
            .clamp(MIN_FLIGHT, MAX_FLIGHT)
    }

    /// Pan distance between the ends.
    #[must_use]
    pub const fn distance(&self) -> f64 {
        self.d1
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
    let ramp = |t: f64| speed * (t / 2.0 - RAMP / (2.0 * std::f64::consts::PI) * (std::f64::consts::PI * t / RAMP).sin());
    if t < RAMP {
        ramp(t)
    } else if t <= 1.0 - RAMP {
        speed * (RAMP / 2.0 + (t - RAMP))
    } else {
        1.0 - ramp(1.0 - t)
    }
}

/// One flight in progress: an eased path plus the velocity correction it
/// inherited from an interrupted flight.
#[derive(Clone, Copy, Debug)]
struct Trip {
    path: Path,
    start: Instant,
    duration: Duration,
    /// Velocity (x, y, ln w per second) the path lacks at its start.
    carry: (f64, f64, f64),
}

impl Trip {
    fn new(path: Path, start: Instant, carry: (f64, f64, f64)) -> Self {
        let moving = carry.0.abs() + carry.1.abs() + carry.2.abs() > 1e-9;
        let duration = if path.length() <= 1e-12 && !moving {
            Duration::ZERO
        } else {
            path.duration()
        };
        Self {
            path,
            start,
            duration,
            carry,
        }
    }

    fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= self.duration
    }

    /// The camera `tau` seconds in.
    fn at(&self, tau: f64) -> Camera {
        let d = self.duration.as_secs_f64();
        if tau >= d {
            return self.path.end();
        }
        let tau = tau.max(0.0);
        let camera = self.path.at_t(ease(tau / d));
        // τ(1 − τ/D)²: slope 1 at 0, zero value and slope at D.
        let h = tau * (1.0 - tau / d).powi(2);
        Camera {
            x: camera.x + self.carry.0 * h,
            y: camera.y + self.carry.1 * h,
            w: camera.w * (self.carry.2 * h).exp(),
        }
    }

    fn sample(&self, now: Instant) -> Camera {
        self.at(now.saturating_duration_since(self.start).as_secs_f64())
    }

    /// (dx/dt, dy/dt, d ln w / dt) at `now`, by central difference.
    fn velocity(&self, now: Instant) -> (f64, f64, f64) {
        let tau = now.saturating_duration_since(self.start).as_secs_f64();
        let d = self.duration.as_secs_f64();
        if tau >= d || d <= 0.0 {
            return (0.0, 0.0, 0.0);
        }
        let h = 1e-5_f64.min(tau.max(1e-9)).min((d - tau).max(1e-9));
        let (a, b) = (self.at(tau - h), self.at(tau + h));
        let span = 2.0 * h;
        (
            (b.x - a.x) / span,
            (b.y - a.y) / span,
            (b.w.ln() - a.w.ln()) / span,
        )
    }
}

/// Plans a flight from `current` (moving at `velocity`) to `target`.
fn plan(current: Camera, velocity: (f64, f64, f64), target: Camera, now: Instant) -> Trip {
    let path = Path::new(current, target);
    let fresh = Trip::new(path, now, (0.0, 0.0, 0.0));
    let start = fresh.velocity(now);
    let carry = (velocity.0 - start.0, velocity.1 - start.1, velocity.2 - start.2);
    Trip::new(path, now, carry)
}

#[derive(Clone, Copy, Debug)]
enum State {
    Still(Camera),
    Flying(Trip),
    Fading { from: Camera, to: Camera, start: Instant },
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
        let key = key.into();
        let now = now(cx);
        let reduced = reduced(cx);
        let (shot, trip) = {
            let mut store = self.store.borrow_mut();
            let state = store.entry(key.clone()).or_insert(State::Still(target));
            step(state, target, now, reduced)
        };
        if shot.live {
            request_frame(window, cx);
        }
        if probe::enabled(cx) {
            publish(cx, &key, &shot, trip.as_ref(), target, now);
        }
        shot
    }

    /// Puts `key` at `camera` without flying.
    pub fn jump(&self, key: impl Into<ElementId>, camera: Camera) {
        self.store
            .borrow_mut()
            .insert(key.into(), State::Still(camera));
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
        State::Flying(trip) => trip.path.end(),
        State::Fading { to, .. } => *to,
    }
}

/// Advances one keyed flight to `now` towards `target`.
fn step(state: &mut State, target: Camera, now: Instant, reduced: bool) -> (Shot, Option<Trip>) {
    // Where it is now, and how fast.
    let (current, velocity) = match *state {
        State::Still(camera) => (camera, (0.0, 0.0, 0.0)),
        State::Flying(trip) if trip.done(now) => (trip.path.end(), (0.0, 0.0, 0.0)),
        State::Flying(trip) => (trip.sample(now), trip.velocity(now)),
        State::Fading { to, .. } => (to, (0.0, 0.0, 0.0)),
    };
    if target_of(state) != target {
        *state = if reduced {
            State::Fading {
                from: current,
                to: target,
                start: now,
            }
        } else {
            let trip = plan(current, velocity, target, now);
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
            *state = State::Still(trip.path.end());
            (
                Shot {
                    camera: trip.path.end(),
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
fn publish(cx: &mut App, key: &ElementId, shot: &Shot, trip: Option<&Trip>, target: Camera, now: Instant) {
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
    for (axis, value, goal, speed) in [
        ("x", shot.camera.x, target.x, velocity.0),
        ("y", shot.camera.y, target.y, velocity.1),
        ("w", shot.camera.w, target.w, velocity.2 * shot.camera.w),
    ] {
        // The zoom-out between the ends is the path, not an overshoot: allow
        // the path's own peak width, as a share of the span.
        let overshoot = trip.map_or(0.0, |trip| {
            if axis != "w" {
                return 0.0;
            }
            let (w0, w1) = (trip.path.start().w, trip.path.end().w);
            let peak = (0..=64)
                .map(|i| trip.path.at_t(f64::from(i) / 64.0).w)
                .fold(0.0, f64::max);
            ((peak - w0.max(w1)) / (w1 - w0).abs().max(1e-9)).max(0.0) as f32
        });
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
            overshoot_ratio: overshoot,
            group: group.clone(),
        });
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::{Camera, Path, State, Trip, ease, plan, step};
    use std::time::{Duration, Instant};

    fn close(a: Camera, b: Camera, tolerance: f64) -> bool {
        (a.x - b.x).abs() <= tolerance && (a.y - b.y).abs() <= tolerance && (a.w - b.w).abs() <= tolerance
    }

    #[test]
    fn endpoints_are_exact_and_the_formula_reaches_them() {
        let (p0, p1) = (Camera::new(-3.0, 2.0, 4.0), Camera::new(40.0, -7.5, 1.5));
        let path = Path::new(p0, p1);
        assert_eq!(path.at_t(0.0), p0);
        assert_eq!(path.at_t(1.0), p1);
        // Not just the clamp: the formula itself lands on both ends.
        assert!(close(path.analytic(0.0), p0, 1e-12));
        assert!(close(path.analytic(path.length()), p1, 1e-9), "{:?}", path.analytic(path.length()));
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
                    assert!(close(a, b, 1e-5 * w0.max(w1)), "d={d} t={t}: {a:?} vs {b:?}");
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
            assert!((there - back).abs() < 1e-9 * there.max(1.0), "{there} vs {back}");
        }
    }

    #[test]
    fn a_long_pan_zooms_out_once_and_back_in() {
        let path = Path::new(Camera::new(0.0, 0.0, 1.0), Camera::new(50.0, 0.0, 1.0));
        let widths: Vec<f64> = (0..=200).map(|i| path.at_t(f64::from(i) / 200.0).w).collect();
        let peak = widths
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(i, _)| i);
        assert!(widths[peak] > 10.0, "it zooms well out: {}", widths[peak]);
        assert!(peak > 50 && peak < 150, "the peak is between the ends: {peak}");
        assert!(widths[..=peak].windows(2).all(|w| w[1] >= w[0] - 1e-12), "rises");
        assert!(widths[peak..].windows(2).all(|w| w[1] <= w[0] + 1e-12), "then falls");
        // And it travels monotonically along the pan.
        let xs: Vec<f64> = (0..=200).map(|i| path.at_t(f64::from(i) / 200.0).x).collect();
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
            assert!((slope(t - 1e-4) - slope(t + 1e-4)).abs() < 1e-3, "C1 at {t}");
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
        let t0 = Instant::now();
        let first = plan(
            Camera::new(0.0, 0.0, 2.0),
            (0.0, 0.0, 0.0),
            Camera::new(30.0, 10.0, 1.0),
            t0,
        );
        let mid = t0 + first.duration.mul_f64(0.4);
        let (at, v) = (first.sample(mid), velocity(&first, mid));
        assert!(v.0.abs() > 1.0, "it was moving: {v:?}");
        let second = plan(at, v, Camera::new(-20.0, 5.0, 4.0), mid);
        assert!(close(second.sample(mid), at, 1e-9), "no position jump");
        let w = velocity(&second, mid + Duration::from_nanos(20_000));
        let before = velocity(&first, mid - Duration::from_nanos(20_000));
        for (a, b) in [(before.0, w.0), (before.1, w.1), (before.2, w.2)] {
            assert!((a - b).abs() < 1e-2 * a.abs().max(1.0), "velocity jump: {before:?} -> {w:?}");
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
                target = Camera::new(unit() * 100.0 - 50.0, unit() * 60.0 - 30.0, 0.5 + unit() * 20.0);
            }
            let (shot, _) = step(&mut state, target, now, false);
            let c = shot.camera;
            assert!(c.x.is_finite() && c.y.is_finite() && c.w > 0.0, "{c:?}");
            let jump = ((c.x - last.x).powi(2) + (c.y - last.y).powi(2)).sqrt();
            // Nothing crosses more than a view and a half in one millisecond.
            assert!(jump < 1.5 * last.w.max(c.w), "ms {ms}: jumped {jump} at w {}", c.w);
            last = c;
        }
        let (shot, _) = step(&mut state, target, t0 + Duration::from_secs(9), false);
        assert_eq!(shot.camera, target);
        assert!(!shot.live);
    }

    #[test]
    fn reduced_motion_crossfades_instead_of_flying() {
        let t0 = Instant::now();
        let start = Camera::new(0.0, 0.0, 1.0);
        let goal = Camera::new(40.0, 0.0, 3.0);
        let mut state = State::Still(start);
        let (shot, _) = step(&mut state, goal, t0, true);
        assert_eq!(shot.camera, goal, "no flight: the new framing at once");
        assert_eq!(shot.from, Some(start));
        assert_eq!(shot.fade, 0.0);
        let (half, _) = step(&mut state, goal, t0 + Duration::from_millis(60), true);
        assert!((half.fade - 0.5).abs() < 1e-3);
        let (done, _) = step(&mut state, goal, t0 + Duration::from_millis(120), true);
        assert_eq!((done.from, done.fade, done.live), (None, 1.0, false));
    }
}
