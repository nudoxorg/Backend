//! Two-dimensional spring motion for 2-D positions and pan offsets.
//!
//! `Motion2` wraps two independent [`Motion`] scalars that share a single
//! `tick` call. This is the right abstraction for:
//!
//! - **Graph node positions** (`graph.settle`, §5.2): layout iterations arrive
//!   from the background executor; the canvas springs each node toward the
//!   latest position. The two axes decouple naturally.
//! - **Pan offsets** (§18.4): pan springs on wheel/drag fling release. X and
//!   Y share the same time step so the path curves smoothly, not in two
//!   separate straight lines.
//!
//! The physics tier carries no GPUI dependency, so `Motion2` is testable
//! without a window.

use std::time::Instant;

use crate::motion::spring::{Motion, Spring};

/// A spring-animated 2-D point sharing one `tick`.
///
/// Each axis uses the same [`Spring`] parameters (isotropic). If you need
/// different springs per axis, compose two independent [`Motion`]s manually
/// and tick them in sequence — that is just as efficient; this wrapper is
/// convenience only.
#[derive(Clone, Debug)]
pub struct Motion2 {
    /// Horizontal axis.
    pub x: Motion,
    /// Vertical axis.
    pub y: Motion,
}

impl Motion2 {
    /// Construct a resting `Motion2` at `(x, y)` with the given `spring`.
    pub fn new(x: f32, y: f32, spring: Spring) -> Self {
        Self {
            x: Motion::new(x, spring),
            y: Motion::new(y, spring),
        }
    }

    /// Retarget both axes without touching velocity.
    ///
    /// This is the standard call for streaming layout positions: a background
    /// iteration pushes new node coordinates and we retarget; the springs
    /// smoothly chase the cursor without resetting.
    pub fn animate_to(&mut self, x: f32, y: f32) {
        self.x.animate_to(x);
        self.y.animate_to(y);
    }

    /// Snap both axes instantly (reduced motion or initialisation).
    pub fn snap_to(&mut self, x: f32, y: f32) {
        self.x.snap_to(x);
        self.y.snap_to(y);
    }

    /// Current animated position.
    pub fn value(&self) -> (f32, f32) {
        (self.x.value(), self.y.value())
    }

    /// Target position.
    pub fn target(&self) -> (f32, f32) {
        (self.x.target(), self.y.target())
    }

    /// `true` when both axes have settled.
    pub fn is_settled(&self) -> bool {
        self.x.is_settled() && self.y.is_settled()
    }

    /// Advance both axes to `now`.
    ///
    /// Returns `true` while at least one axis is still moving. The caller
    /// must call `window.request_animation_frame()` in that case and must
    /// *not* call `tick` again in the same render pass.
    ///
    /// Time is a parameter (not `Instant::now()`) so tests remain
    /// deterministic.
    pub fn tick(&mut self, now: Instant) -> bool {
        // Tick both axes unconditionally so they stay time-synchronised. If
        // we short-circuit on the first settled axis, the other axis would
        // advance by a slightly different `dt` on its first tick after the
        // other settled — producing an imperceptible but diagnostically
        // confusing asymmetry.
        let x_moving = self.x.tick(now);
        let y_moving = self.y.tick(now);
        x_moving || y_moving
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn motion2_both_axes_settle() {
        let mut m = Motion2::new(0.0, 0.0, Spring::GENTLE);
        m.animate_to(100.0, 200.0);

        let start = Instant::now();
        let frame = Duration::from_millis(8);
        let mut settled = false;

        for i in 0..200u32 {
            let t = start + frame * i;
            if !m.tick(t) {
                settled = true;
                break;
            }
        }

        assert!(settled, "Motion2 with GENTLE spring should settle within ~1.6 s at 120 Hz");
        assert!((m.x.value() - 100.0).abs() < 0.01);
        assert!((m.y.value() - 200.0).abs() < 0.01);
    }

    #[test]
    fn snap_to_is_immediate_2d() {
        let mut m = Motion2::new(50.0, 75.0, Spring::DEFAULT);
        m.snap_to(0.0, 0.0);
        let (x, y) = m.value();
        assert_eq!(x, 0.0);
        assert_eq!(y, 0.0);
        assert!(m.is_settled());
    }

    #[test]
    fn retarget_preserves_velocity() {
        let mut m = Motion2::new(0.0, 0.0, Spring::SNAPPY);
        m.animate_to(200.0, 400.0);

        let start = Instant::now();
        let frame = Duration::from_millis(8);

        // Build up velocity over 80 ms.
        for i in 0..10u32 {
            m.tick(start + frame * i);
        }

        let (vx, vy) = (m.x.velocity(), m.y.velocity());
        assert!(vx > 0.0);
        assert!(vy > 0.0);

        // Retarget — velocity must be unchanged.
        m.animate_to(0.0, 0.0);
        assert_eq!(m.x.velocity(), vx);
        assert_eq!(m.y.velocity(), vy);
    }
}
