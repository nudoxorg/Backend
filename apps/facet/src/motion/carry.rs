//! CARRY: the one driver of a place change (W-Motion PLAN §2a, MOTION
//! "deeper").
//!
//! A whole transition — the plate opening, the title riding its edge, the
//! old page drifting, the new one printing — is driven by one value `p` on
//! one critically damped spring, and every pose is a [`band`] of it. Bands
//! stay linear because the driver is already a spring. An interruption
//! (Back while a page is still opening) retargets that one spring from its
//! painted value and velocity, so nothing restarts and nothing jumps.
//!
//! [`CARRY`]'s response is the lead's ruling (2026-09-28): 0.28 s, 97 %
//! settled at 240 ms, the top of the 120–240 ms budget (at MOTION's first
//! 0.34 s it was only 93 % there).

use super::spring::{Phase, Spring};
use std::time::{Duration, Instant};

/// The place-change spring: response 0.28 s, critically damped. From rest,
/// `p` is 23 % at 40 ms, 54 % at 80, 75 % at 120, 87 % at 160 and 97 % at
/// 240, with no overshoot.
pub const CARRY: Spring = Spring {
    response: 0.28,
    damping: 1.0,
};

/// One transition's driver: closed form in the time since its last
/// retarget, so the value at any virtual time is the same however many
/// frames were drawn in between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Carry {
    target: f32,
    origin: Phase,
    /// When the segment starts; before it the value holds at its origin.
    start: Instant,
}

impl Carry {
    /// From `from` (at rest) towards `to`, starting at `start` (a start in
    /// the future holds `from` until then: the Close waits for its fold).
    #[must_use]
    pub fn new(from: f32, to: f32, start: Instant) -> Self {
        Self {
            target: to,
            origin: Phase {
                offset: f64::from(from - to),
                velocity: 0.0,
            },
            start,
        }
    }

    /// The value and its velocity (units per second) at `now`.
    #[must_use]
    pub fn sample(&self, now: Instant) -> (f32, f32) {
        let dt = now.saturating_duration_since(self.start).as_secs_f64();
        let phase = CARRY.step(self.origin, dt);
        #[allow(clippy::cast_possible_truncation)]
        let sample = ((phase.offset as f32) + self.target, phase.velocity as f32);
        sample
    }

    /// The value at `now`.
    #[must_use]
    pub fn value(&self, now: Instant) -> f32 {
        self.sample(now).0
    }

    /// Where it is heading.
    #[must_use]
    pub const fn target(&self) -> f32 {
        self.target
    }

    /// When the current segment started (or starts).
    #[must_use]
    pub const fn start(&self) -> Instant {
        self.start
    }

    /// Heads for `to` from wherever it is painted at `now`, keeping its
    /// velocity.
    pub fn retarget(&mut self, to: f32, now: Instant) {
        let (value, velocity) = self.sample(now);
        *self = Self {
            target: to,
            origin: Phase {
                offset: f64::from(value - to),
                velocity: f64::from(velocity),
            },
            start: now,
        };
    }

    /// Whether it is within `within` of its target and nearly still.
    #[must_use]
    pub fn settled(&self, now: Instant, within: f32) -> bool {
        let (value, velocity) = self.sample(now);
        (value - self.target).abs() <= within && velocity.abs() <= within * 20.0
    }

    /// How long after its start the segment first comes within `within`
    /// of its target and stays (for probe budgets).
    #[must_use]
    pub fn budget(&self, within: f32) -> Duration {
        Duration::from_secs_f64(CARRY.settle_time(self.origin, f64::from(within)))
    }
}

/// `p` mapped onto the band `[a, b]` and clamped: 0 before `a`, 1 after `b`.
#[must_use]
pub fn band(p: f32, a: f32, b: f32) -> f32 {
    if b <= a {
        return if p >= b { 1.0 } else { 0.0 };
    }
    ((p - a) / (b - a)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{Carry, band};
    use std::time::{Duration, Instant};

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    /// The ruling's numbers, from rest: 23 / 54 / 75 / 87 / 97 %.
    #[test]
    fn carry_lands_97_percent_by_240_ms_without_overshoot() {
        let start = Instant::now();
        let carry = Carry::new(0.0, 1.0, start);
        let p = |ms| carry.value(at(start, ms));
        for (ms, want) in [(40, 0.23), (80, 0.54), (120, 0.75), (160, 0.87), (240, 0.97)] {
            assert!((p(ms) - want).abs() < 0.01, "p({ms}) = {:.3}, want {want}", p(ms));
        }
        let peak = (0..2000).map(p).fold(0.0_f32, f32::max);
        assert!(peak <= 1.0 + 1e-6, "critically damped: never past 1 ({peak})");
    }

    /// Back at 120 ms: the value is continuous, the velocity carries over,
    /// and the driver then heads home without a jump.
    #[test]
    fn a_retarget_starts_from_the_painted_value_and_velocity() {
        let start = Instant::now();
        let mut carry = Carry::new(0.0, 1.0, start);
        let (value, velocity) = carry.sample(at(start, 120));
        assert!(velocity > 0.0);
        carry.retarget(0.0, at(start, 120));
        let (after, after_velocity) = carry.sample(at(start, 120));
        assert!((after - value).abs() < 1e-5 && (after_velocity - velocity).abs() < 1e-3);
        // Still moving forward for a moment (it has momentum), then home.
        assert!(carry.value(at(start, 130)) > value - 1e-3);
        assert!(carry.value(at(start, 600)) < 0.01);
    }

    /// A start in the future holds the origin until then.
    #[test]
    fn a_delayed_carry_holds_until_its_start() {
        let start = Instant::now();
        let carry = Carry::new(1.0, 0.0, at(start, 64));
        assert!((carry.value(start) - 1.0).abs() < 1e-6);
        assert!((carry.value(at(start, 64)) - 1.0).abs() < 1e-6);
        assert!(carry.value(at(start, 64 + 240)) < 0.04);
    }

    #[test]
    fn bands_clamp_and_map_linearly() {
        assert_eq!(band(0.1, 0.2, 0.6), 0.0);
        assert!((band(0.4, 0.2, 0.6) - 0.5).abs() < 1e-6);
        assert_eq!(band(0.9, 0.2, 0.6), 1.0);
    }
}
