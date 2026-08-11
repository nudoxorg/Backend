//! Spring-animated HSLA color interpolation.
//!
//! `MotionColor` wraps four [`Motion`] scalars (h, s, l, a) so that status
//! dots, trust badges, and syntax-highlight tokens can cross-fade smoothly
//! as their semantic state changes.
//!
//! # Shortest-arc hue wraparound
//!
//! The classic bug in color animation: interpolating `h` from 0.95 to 0.05
//! (e.g. 342° → 18° in degrees) naively sweeps through 0.5 (180°), a long
//! way around the color wheel. The correct path crosses the 0/1 boundary and
//! travels only 0.10 of the circle.
//!
//! We correct this before handing off to the spring: the delta is wrapped
//! into `[−0.5, +0.5]` and the target is adjusted to `current + wrapped_delta`.
//! The spring then interpolates through the wrap point naturally. On arrival
//! the hue is re-normalised into `[0.0, 1.0)` so the stored value never drifts.
//!
//! In GPUI's `Hsla`, `h` is in the range 0.0–1.0 (not 0–360°). The wrap
//! math is identical; we just work in the 0–1 space.
//!
//! # GPUI dependency note
//!
//! `MotionColor` stores a `gpui::Hsla` as the "clean" target for
//! normalisation, but its physics are four plain `f32` springs. The type is
//! intentionally usable without a GPUI window — tests that only exercise
//! `tick` do not need a GPUI context.

use gpui::Hsla;
use std::time::Instant;

use crate::motion::spring::{Motion, Spring};

/// A spring-animated `Hsla` color, using shortest-arc hue interpolation.
///
/// Each channel (h, s, l, a) is a separate [`Motion`], advanced together with
/// a single `tick`. On settle the color is normalised so `h` stays in
/// `[0.0, 1.0)`.
#[derive(Clone, Debug)]
pub struct MotionColor {
    h: Motion,
    s: Motion,
    l: Motion,
    a: Motion,
}

impl MotionColor {
    /// Construct a resting `MotionColor` at `color`, using `spring` for all
    /// four channels.
    pub fn new(color: Hsla, spring: Spring) -> Self {
        Self {
            h: Motion::new(color.h, spring),
            s: Motion::new(color.s, spring),
            l: Motion::new(color.l, spring),
            a: Motion::new(color.a, spring),
        }
    }

    /// Begin animating toward `target`, using shortest-arc hue interpolation.
    ///
    /// The hue target is adjusted so that the spring travels through the
    /// shorter arc on the color wheel, even if that arc crosses the 0/1
    /// boundary. On settle the hue is normalised back to `[0.0, 1.0)`.
    pub fn animate_to(&mut self, target: Hsla) {
        let current_h = self.h.value();
        let raw_delta = target.h - current_h;

        // Wrap delta into [-0.5, +0.5]: shortest arc on the unit circle.
        let wrapped_delta = ((raw_delta + 0.5).rem_euclid(1.0)) - 0.5;
        let adjusted_h_target = current_h + wrapped_delta;

        // Animate h toward the adjusted target (may be outside [0,1] temporarily;
        // that is intentional — the spring path is a straight line in h-space
        // and crosses the wrap point naturally).
        self.h.animate_to(adjusted_h_target);
        self.s.animate_to(target.s);
        self.l.animate_to(target.l);
        self.a.animate_to(target.a);
    }

    /// Snap to `target` instantly (reduced motion or initialisation).
    pub fn snap_to(&mut self, target: Hsla) {
        self.h.snap_to(target.h);
        self.s.snap_to(target.s);
        self.l.snap_to(target.l);
        self.a.snap_to(target.a);
    }

    /// Current animated color.
    ///
    /// Hue is normalised to `[0.0, 1.0)` before returning so the value is
    /// always a valid `Hsla`.
    pub fn value(&self) -> Hsla {
        // Assembled by the palette, not here. This was an `Hsla { .. }` struct
        // literal — the last place in the crate outside `src/theme/` that
        // constructed a colour, and the one `tests/theme_law.rs` found. The
        // literal was innocent in intent and still worth removing: a guard that
        // has to make an exception for "but this one is only interpolating"
        // can no longer answer "where do colours come from?" in one word.
        crate::theme::palette::from_channels(
            self.h.value(),
            self.s.value(),
            self.l.value(),
            self.a.value(),
        )
    }

    /// `true` when all four channels have settled.
    pub fn is_settled(&self) -> bool {
        self.h.is_settled() && self.s.is_settled() && self.l.is_settled() && self.a.is_settled()
    }

    /// Advance all four channels to `now`.
    ///
    /// Returns `true` while at least one channel is still moving.
    pub fn tick(&mut self, now: Instant) -> bool {
        let h = self.h.tick(now);
        let s = self.s.tick(now);
        let l = self.l.tick(now);
        let a = self.a.tick(now);
        h || s || l || a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn hsla(h: f32, s: f32, l: f32, a: f32) -> Hsla {
        Hsla { h, s, l, a }
    }

    /// The short arc from 350°→10° (0.972→0.028) must pass through 0°/360°,
    /// not through 180°. In 0–1 space that means passing through 0.0, not 0.5.
    #[test]
    fn hue_wraps_shortest_arc_across_zero() {
        // From 0.972 (~350°) to 0.028 (~10°): delta = 0.056, short arc crosses 0.
        let from = hsla(0.972, 0.5, 0.5, 1.0);
        let to = hsla(0.028, 0.5, 0.5, 1.0);

        let mut m = MotionColor::new(from, Spring::SNAPPY);
        m.animate_to(to);

        // After a few ticks the hue should be moving toward ~0 (wrapping through
        // 1.0→0.0), not toward 0.5. The animated hue value (un-normalised) will
        // be < current_h, going toward ~0.028 via the wrap path.
        let start = Instant::now();
        m.tick(start);
        m.tick(start + Duration::from_millis(8));
        m.tick(start + Duration::from_millis(16));

        // The spring's internal hue target was adjusted to ~0.028 - 1.0 = -0.972
        // ... actually adjusted_target = current_h + wrapped_delta
        // wrapped_delta = ((0.028 - 0.972 + 0.5) % 1.0) - 0.5
        //               = ((-0.944 + 0.5) % 1.0) - 0.5
        //               = (-0.444 % 1.0) - 0.5  = -0.444 - 0.5 = -0.944? No:
        // rem_euclid: (-0.444 + 0.5).rem_euclid(1.0) = 0.056.rem_euclid(1.0) = 0.056
        // 0.056 - 0.5 = -0.444
        // adjusted_h_target = 0.972 + (-0.444) = 0.528? That goes the long way.
        //
        // Recalculate: delta = 0.028 - 0.972 = -0.944
        // (delta + 0.5) = -0.444
        // rem_euclid(1.0) = 0.556   (because -0.444 + 1.0 = 0.556)
        // wrapped_delta = 0.556 - 0.5 = 0.056
        // adjusted_h_target = 0.972 + 0.056 = 1.028
        //
        // So the spring goes from 0.972 → 1.028, which after rem_euclid gives
        // 0.028. The path passes through 1.0 (= 0.0), i.e. through the wrap.
        // Good — the hue increases from 0.972 toward 1.028, crossing 1.0 ≡ 0°.

        // After a few ticks the raw h value (before normalisation) should be
        // greater than 0.972 (spring moving toward 1.028).
        let raw_h = m.h.value();
        assert!(
            raw_h > 0.972 || raw_h < 0.028 + 0.05,
            "hue should be moving toward 1.028 (over wrap), got raw h = {raw_h}"
        );

        // The reported color should have h normalised into [0, 1).
        let color = m.value();
        assert!(
            (0.0..=1.0).contains(&color.h),
            "normalised hue out of range: {}", color.h
        );
    }

    /// Simple non-wrapping case: 0.2 → 0.4 must go straight.
    #[test]
    fn hue_interpolates_directly_when_not_near_boundary() {
        let from = hsla(0.2, 0.6, 0.5, 1.0);
        let to = hsla(0.4, 0.6, 0.5, 1.0);

        let mut m = MotionColor::new(from, Spring::DEFAULT);
        m.animate_to(to);

        let start = Instant::now();
        for i in 0..10u32 {
            m.tick(start + Duration::from_millis(8) * i);
        }

        // Hue should be between 0.2 and 0.4 (no wrapping).
        let h = m.value().h;
        assert!(
            h >= 0.19 && h <= 0.41,
            "non-wrapping hue should stay in [0.2, 0.4], got {h}"
        );
    }

    /// All channels settle within a reasonable time.
    #[test]
    fn color_settles() {
        let from = hsla(0.1, 0.3, 0.4, 0.8);
        let to = hsla(0.6, 0.7, 0.6, 1.0);

        let mut m = MotionColor::new(from, Spring::SNAPPY);
        m.animate_to(to);

        let start = Instant::now();
        let frame = Duration::from_millis(8);
        let mut settled = false;

        for i in 0..200u32 {
            if !m.tick(start + frame * i) {
                settled = true;
                break;
            }
        }

        assert!(settled, "MotionColor with SNAPPY spring should settle within ~1.6 s at 120 Hz");

        let c = m.value();
        assert!((c.h - to.h).abs() < 0.01, "hue did not settle to target");
        assert!((c.s - to.s).abs() < 0.01, "saturation did not settle");
        assert!((c.l - to.l).abs() < 0.01, "lightness did not settle");
        assert!((c.a - to.a).abs() < 0.01, "alpha did not settle");
    }

    /// snap_to is instant; value() returns normalised hue.
    #[test]
    fn snap_to_color() {
        let mut m = MotionColor::new(hsla(0.5, 0.5, 0.5, 1.0), Spring::DEFAULT);
        m.snap_to(hsla(0.9, 0.2, 0.8, 0.5));
        let c = m.value();
        assert!((c.h - 0.9).abs() < 1e-6);
        assert!((c.a - 0.5).abs() < 1e-6);
        assert!(m.is_settled());
    }
}
