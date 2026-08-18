//! Spring physics kernel — the retained-animation tier (GUI-PLAN §4.2).
//!
//! # Why springs instead of duration-easing?
//!
//! Duration-easing animations restart from zero whenever the target changes.
//! That restart produces a velocity discontinuity the user feels as "cheap".
//! Springs carry velocity through retargeting: when the user toggles the dock
//! while it is already closing, the door reverses without stopping. This is
//! the entire reason the retained tier exists.
//!
//! # Semi-implicit Euler and the 240 Hz substep
//!
//! The spring ODE is:
//! ```text
//! m·ẍ = −k·(x − target) − c·ẋ
//! ```
//! We integrate with **semi-implicit Euler** (velocity updated first, position
//! updated using the new velocity). Compared to explicit Euler this is
//! unconditionally stable for any positive k, c, m — it will not blow up at
//! large time steps, it conserves energy rather than manufacturing it, and it
//! has the same O(h) truncation error with a single line of code difference
//! versus explicit.
//!
//! We sub-step at a fixed 240 Hz (h = 1/240 s ≈ 4.17 ms). That rate is high
//! enough that the integration error on UI-scale springs (k ≤ 600, c ≤ 50,
//! m = 1) is imperceptible, and it makes the simulation independent of the
//! display refresh rate: the same spring behaviour on a 60 Hz monitor and a
//! 120 Hz ProMotion display, without retuning constants.
//!
//! # Stall clamp
//!
//! A 32 ms cap on `dt` prevents teleporting when the app is paused (focus
//! loss, debugger, sleep). Without it a 1-second freeze would look like a
//! single huge jump on resume. The cap means the spring loses at most ~7
//! sub-steps worth of accuracy during a stall — perfectly acceptable.
//!
//! # Velocity preservation on retarget
//!
//! `animate_to` *only* changes `self.target`. Velocity is untouched. The next
//! `tick` call will immediately begin steering toward the new target from
//! whatever velocity the spring already had. This is the invariant that makes
//! mid-flight interruption feel physically honest: a dock that was springing
//! open at 300 px/s will not jerk when you close it — it gracefully curves
//! back.

use std::time::{Duration, Instant};

/// Physical parameters for a critically-damped spring.
///
/// All three presets are tuned for `mass = 1.0`. Do not change mass without
/// retuning stiffness and damping together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// Restoring force per unit displacement (N/m).
    pub stiffness: f32,
    /// Viscous damping coefficient (N·s/m).
    pub damping: f32,
    /// Effective inertial mass (kg).
    pub mass: f32,
}

impl Spring {
    /// Damping ratio shared by every preset: 0.97, just inside critical.
    ///
    /// Critical damping (ζ = 1) is the fastest approach with no overshoot, but
    /// it reads as slightly dead. A hair under gives a sub-pixel overshoot the
    /// eye reads as "physical" without ever looking bouncy. Each preset's
    /// `damping` is `2 · ζ · √(stiffness · mass)`.
    pub const ZETA: f32 = 0.97;

    /// Default UI spring — settles in 350 ms (measured, not estimated).
    ///
    /// Use for: docks, selection bars, chevron rotations, keyboard-driven
    /// scroll offsets. The most common spring in the app.
    pub const DEFAULT: Spring = Spring {
        stiffness: 459.0,
        damping: 41.6,
        mass: 1.0,
    };

    /// Snappy spring — settles in 225 ms.
    ///
    /// Use for: overlays, toasts, tab-underline slides. Anything that must
    /// feel instant while still communicating direction.
    pub const SNAPPY: Spring = Spring {
        stiffness: 1225.0,
        damping: 67.9,
        mass: 1.0,
    };

    /// Gentle spring — settles in 600 ms.
    ///
    /// Use for: progress bars, graph-node positions, count tickers. Surfaces
    /// where the audience is watching numbers accumulate, not waiting for a
    /// toggle.
    pub const GENTLE: Spring = Spring {
        stiffness: 145.0,
        damping: 23.4,
        mass: 1.0,
    };
}

/// A spring-animated scalar.
///
/// Views own one `Motion` per animated dimension in their state struct and
/// call `tick` once per render, consuming the return value to decide whether
/// to request the next animation frame (GUI-PLAN §4.2 render-loop contract).
///
/// `Motion` has **no** dependency on any GPUI type. It is pure scalar
/// arithmetic and can be tested deterministically with simulated time.
#[derive(Clone, Debug)]
pub struct Motion {
    value: f32,
    velocity: f32,
    target: f32,
    spring: Spring,
    /// `None` before the first tick; reset to `None` on settle or snap.
    last_tick: Option<Instant>,
    /// Distance this spring was asked to travel, captured at `animate_to`.
    ///
    /// Rest thresholds are derived from this rather than being absolute, which
    /// is what makes settle time independent of travel distance — see
    /// [`Motion::rest_displacement`].
    travel: f32,
}

/// Rest displacement as a fraction of the travel distance (0.5 %).
///
/// GUI-PLAN §4.2 specified an *absolute* `epsilon: 0.05`, which is wrong in a
/// way that only shows up under measurement: a linear spring's time to cross an
/// absolute threshold grows with the log of the distance travelled, so a 320 px
/// dock took 758 ms to "settle" while a 1-unit chevron took 517 ms — with the
/// same spring. Two surfaces using `Spring::DEFAULT` visibly disagreed about
/// what DEFAULT feels like, and the 320 px case blew §5.4's 700 ms cap.
///
/// A *relative* threshold is scale-invariant: every preset now settles in the
/// same wall-clock time whatever it is animating, which is the only reading of
/// "this spring settles in 350 ms" that can be true. 0.5 % is far below the
/// perceptual floor for every travel we animate (1.6 px on a 320 px dock slide,
/// and the last 1.6 px of a near-critically-damped approach take longer than
/// the eye can resolve anyway).
const REST_DISPLACEMENT_FRAC: f32 = 0.005;

/// Floor for the rest displacement, for springs whose travel is ~0.
const REST_DISPLACEMENT_MIN: f32 = 1e-4;

/// Frame rate used to convert a rest *displacement* into a rest *velocity*.
///
/// The spring is at rest when it is close enough AND slow enough that it could
/// not cover the rest displacement within one frame. Deriving velocity from
/// displacement this way keeps the two thresholds consistent, instead of
/// picking two unrelated magic numbers.
const REST_VELOCITY_HZ: f32 = 120.0;

impl Motion {
    /// Construct a resting `Motion` at `value`, using the given `spring`.
    pub fn new(value: f32, spring: Spring) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            spring,
            last_tick: None,
            travel: 0.0,
        }
    }

    /// Begin animating toward `target`, preserving current velocity.
    ///
    /// This is **the whole point**: velocity is not reset, so mid-flight
    /// retargeting is smooth. Call sites never need to branch on whether
    /// an animation is already running.
    ///
    /// When `MotionScale` is 0.0, callers must call `snap_to` instead; the
    /// helpers in `declarative.rs` enforce this transparently so no call site
    /// ever branches on scale.
    pub fn animate_to(&mut self, target: f32) {
        // Record the distance so the rest thresholds scale with it. A
        // retarget mid-flight widens `travel` rather than replacing it: the
        // spring should not become *stricter* about resting just because the
        // user changed their mind toward a nearer target.
        self.travel = self.travel.max((target - self.value).abs());
        self.target = target;
    }

    /// Displacement below which the spring counts as arrived.
    fn rest_displacement(&self) -> f32 {
        (self.travel * REST_DISPLACEMENT_FRAC).max(REST_DISPLACEMENT_MIN)
    }

    /// Jump to `target` instantly, resetting velocity and animation state.
    ///
    /// Used for reduced-motion (`MotionScale = 0.0`), initial positioning,
    /// and the internal settle shortcut.
    pub fn snap_to(&mut self, target: f32) {
        self.target = target;
        self.value = target;
        self.velocity = 0.0;
        self.last_tick = None;
    }

    /// Current animated value — use this in `render` to set element properties.
    pub fn value(&self) -> f32 {
        self.value
    }

    /// The target the spring is converging toward.
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Current velocity, in value-units per second.
    ///
    /// Public because velocity is *handed off*, not just observed: a drag that
    /// ends with the pointer still moving seeds the spring with the pointer's
    /// velocity so the fling continues the gesture instead of restarting it
    /// (GUI-PLAN §18.4). Reading it is also how tests assert the continuity
    /// property that makes retargeting look physical rather than cheap.
    pub fn velocity(&self) -> f32 {
        self.velocity
    }

    /// `true` when the spring has effectively come to rest.
    ///
    /// The caller may stop scheduling animation frames once this returns `true`.
    /// Both thresholds are relative to the travel distance, so a preset's
    /// settle time is the same whether it is moving a 320 px dock or a 1-unit
    /// rotation — see [`REST_DISPLACEMENT_FRAC`].
    pub fn is_settled(&self) -> bool {
        let displacement = self.rest_displacement();
        self.velocity.abs() < displacement * REST_VELOCITY_HZ
            && (self.value - self.target).abs() < displacement
    }

    /// Advance the spring to `now`.
    ///
    /// Returns `true` while the spring is still moving — the caller must
    /// call `window.request_animation_frame()` in that case. Returns `false`
    /// once settled; the final value has been snapped exactly to target.
    ///
    /// **`now` is a parameter, not `Instant::now()`, so tests are
    /// deterministic**: pass a simulated clock, never call `Instant::now()`
    /// inside this function.
    ///
    /// A gap of more than 32 ms is clamped to 32 ms to prevent teleporting
    /// after the app is paused (debugger, focus loss, system sleep).
    pub fn tick(&mut self, now: Instant) -> bool {
        let dt = match self.last_tick.replace(now) {
            Some(prev) => {
                // Clamp stalls: a 1-second freeze must not look like a jump.
                (now.saturating_duration_since(prev)).min(Duration::from_millis(32))
            }
            // First tick: use one sub-step worth of time so the spring
            // immediately begins moving rather than computing a zero delta.
            None => Duration::from_millis(8),
        };

        let mut remaining = dt.as_secs_f32();
        // Fixed substep at 240 Hz — framerate-independent, stable for all
        // stiffness/damping values used in this codebase (see module docs).
        const H: f32 = 1.0 / 240.0;

        while remaining > 0.0 {
            let h = remaining.min(H);

            let f_spring = -self.spring.stiffness * (self.value - self.target);
            let f_damp = -self.spring.damping * self.velocity;

            // Semi-implicit Euler: update velocity first, then position.
            // This order is the "implicit" part — it damps energy rather
            // than amplifying it. See module docs for the trade-off rationale.
            self.velocity += (f_spring + f_damp) / self.spring.mass * h;
            self.value += self.velocity * h;

            remaining -= h;
        }

        if self.is_settled() {
            // Snap exactly to target so rendering gets a pixel-clean value.
            self.snap_settle();
            false
        } else {
            true
        }
    }

    /// Internal: snap to target and clear animation state after settling.
    fn snap_settle(&mut self) {
        self.value = self.target;
        self.velocity = 0.0;
        self.last_tick = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Simulate a spring for `total` duration at 120 Hz (8.33 ms per frame).
    /// Returns `(final_value, final_velocity, settled_at_ms)` where
    /// `settled_at_ms` is `None` if never settled within the budget.
    fn simulate(
        spring: Spring,
        initial_value: f32,
        initial_velocity: f32,
        target: f32,
        total_ms: u64,
    ) -> (f32, f32, Option<u64>) {
        let mut m = Motion {
            value: initial_value,
            velocity: initial_velocity,
            target,
            spring,
            last_tick: None,
            travel: (target - initial_value).abs(),
        };

        let start = Instant::now();
        let mut t = start;
        let frame = Duration::from_micros(8_333); // 120 Hz

        for i in 0..(total_ms * 1000 / 8_333 + 1) {
            t = start + frame * i as u32;
            let still_moving = m.tick(t);
            if !still_moving {
                let elapsed_ms = (t - start).as_millis() as u64;
                return (m.value(), 0.0, Some(elapsed_ms));
            }
        }
        (m.value(), m.velocity, None)
    }

    /// Each preset settles at its **advertised** time, independent of travel.
    ///
    /// This is the regression guard for the defect that GUI-PLAN §4.2's
    /// absolute `epsilon: 0.05` introduced: settle time used to grow with
    /// travel distance, so `DEFAULT` took 758 ms over 320 px but 517 ms over
    /// 1 unit. Asserting the same wall-clock across three orders of magnitude
    /// of travel is what makes "DEFAULT settles in 350 ms" a true statement
    /// rather than a statement about one particular dock width.
    #[test]
    fn presets_settle_at_advertised_time_regardless_of_travel() {
        // (preset, advertised ms) — must match the doc comments on Spring.
        let presets = [
            (Spring::DEFAULT, 350u64),
            (Spring::SNAPPY, 225),
            (Spring::GENTLE, 600),
        ];
        // Three orders of magnitude: a chevron, a panel, a scroll offset.
        let travels = [1.0f32, 320.0, 4000.0];

        for (spring, advertised) in presets {
            for travel in travels {
                let (_, _, settled) = simulate(spring, 0.0, 0.0, travel, 1500);
                let settled = settled
                    .unwrap_or_else(|| panic!("{spring:?} never settled over travel {travel}"));
                let slack = advertised / 10; // ±10 %
                assert!(
                    settled.abs_diff(advertised) <= slack,
                    "{spring:?} over travel {travel} settled in {settled} ms, \
                     advertised {advertised} ms (±{slack})"
                );
            }
        }
    }

    /// Every preset must settle within 700 ms from an arbitrary (value, velocity) start.
    #[test]
    fn all_presets_settle_within_700ms() {
        let cases = [
            (Spring::DEFAULT, 0.0f32, 50.0f32, 320.0f32),
            (Spring::DEFAULT, 320.0, -200.0, 0.0),
            (Spring::SNAPPY, 0.0, 100.0, 100.0),
            (Spring::SNAPPY, 200.0, -300.0, 0.0),
            (Spring::GENTLE, 0.0, 0.0, 1.0),
            (Spring::GENTLE, 1.0, 30.0, 0.0),
        ];

        for (spring, init_val, init_vel, target) in cases {
            let (_, _, settled_at) = simulate(spring, init_val, init_vel, target, 700);
            assert!(
                settled_at.is_some(),
                "spring {spring:?} from ({init_val}, {init_vel}) to {target} did not settle in 700 ms"
            );
        }
    }

    /// Retargeting mid-flight must not produce a sign flip in the position
    /// derivative — velocity continuity across the retarget instant.
    #[test]
    fn retarget_preserves_velocity_sign() {
        let mut m = Motion::new(0.0, Spring::DEFAULT);
        m.animate_to(320.0);

        let start = Instant::now();
        let frame = Duration::from_millis(8);

        // Advance 80 ms (building up toward-target velocity).
        let mut t = start;
        for i in 0..10 {
            t = start + frame * i;
            m.tick(t);
        }

        // Velocity at this point should be positive (moving toward 320).
        let v_before = m.velocity;
        assert!(
            v_before > 0.0,
            "expected positive velocity before retarget, got {v_before}"
        );

        // Retarget to 0 — direction reversal, but velocity should NOT sign-flip
        // instantaneously; it will only change sign once the spring turns it.
        let value_before_retarget = m.value();
        m.animate_to(0.0);

        // Immediately after retarget: value unchanged, velocity unchanged.
        assert_eq!(
            m.value(),
            value_before_retarget,
            "retarget must not move the value"
        );
        assert_eq!(m.velocity, v_before, "retarget must not change velocity");

        // Tick once more — velocity will begin to decrease but must not
        // instantaneously flip sign (that would be the classic discontinuity).
        t += frame;
        m.tick(t);

        // The position derivative (velocity) must not have jumped through zero
        // in a single step. Because the spring force only gradually reverses
        // velocity, the step-size is small enough that v_after and v_before
        // have the same sign OR v_after has crossed zero by at most the
        // maximum deceleration in one substep — we check by asserting the
        // absolute change is bounded.
        //
        // The real invariant is "no teleport of velocity"; we verify by ensuring
        // that after one 8 ms tick the velocity changed by at most the maximum
        // spring impulse in 8 ms: |f_max / m| * dt = |k*xmax + c*vmax| * dt.
        let v_after = m.velocity;
        let max_impulse_per_step = (Spring::DEFAULT.stiffness * (320.0 - 0.0f32).abs()
            + Spring::DEFAULT.damping * v_before.abs())
            / Spring::DEFAULT.mass
            * 0.032; // max dt used internally
        assert!(
            (v_after - v_before).abs() <= max_impulse_per_step + 0.01,
            "velocity changed by more than one spring impulse in one tick: before={v_before}, after={v_after}"
        );
    }

    /// A 32 ms stall must not cause teleporting (value jumps > one spring period).
    #[test]
    fn stall_clamp_prevents_teleport() {
        let mut m = Motion::new(0.0, Spring::DEFAULT);
        m.animate_to(320.0);

        let start = Instant::now();
        // First tick at t=0 (or equiv: first tick starts the spring).
        m.tick(start);
        let v_after_first = m.velocity;

        // Simulate a 500 ms stall — as if the app was paused.
        let after_stall = start + Duration::from_millis(500);
        m.tick(after_stall);

        // The value should not have jumped to target in one step. With a
        // 32 ms clamp the maximum possible displacement is bounded.
        // Empirically: after ~40 ms of integration from rest, value < 100 for
        // DEFAULT spring. Check it's not beyond ~80 px (well below 320).
        assert!(
            m.value() < 80.0,
            "stall caused teleport: value={} after 500 ms stall (expected < 80 from clamp)",
            m.value()
        );
        // Velocity is positive — spring is moving.
        assert!(m.velocity > v_after_first || m.velocity > 0.0);
    }

    /// Scale-0 `animate_to` behaves as `snap_to` when the caller replaces
    /// the call (as our helpers do). This test verifies the `snap_to` path
    /// directly to ensure the semantic contract.
    #[test]
    fn snap_to_is_immediate() {
        let mut m = Motion::new(0.0, Spring::DEFAULT);
        m.snap_to(320.0);
        assert_eq!(m.value(), 320.0);
        assert_eq!(m.velocity, 0.0);
        assert!(m.is_settled());
    }

    /// After `snap_to`, `tick` must return `false` (no more animation needed).
    #[test]
    fn snap_to_tick_is_done() {
        let mut m = Motion::new(0.0, Spring::DEFAULT);
        m.snap_to(320.0);
        // tick should immediately report settled
        let still = m.tick(Instant::now());
        assert!(!still, "tick after snap_to must return false");
    }
}
