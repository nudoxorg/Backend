//! Cubic-bezier easing, solved the way browsers solve `cubic-bezier()`.
//!
//! The polynomial form and the Newton-then-bisection solver follow `WebKit`'s
//! `UnitBezier`, evaluated in `f64` so a curve's output is stable to 1e-7 of
//! the progress axis. Outputs above 1 or below 0 are returned as-is: `BOUNCE`
//! and `SPRING` overshoot on purpose.

use crate::tokens::motion::Bezier;

/// Straight-line progress.
pub const LINEAR: Bezier = Bezier {
    x1: 0.0,
    y1: 0.0,
    x2: 1.0,
    y2: 1.0,
};

/// CSS `ease`, for parity checks against browser references.
pub const EASE: Bezier = Bezier {
    x1: 0.25,
    y1: 0.1,
    x2: 0.25,
    y2: 1.0,
};

const EPSILON: f64 = 1e-7;

#[derive(Clone, Copy)]
struct Unit {
    ax: f64,
    bx: f64,
    cx: f64,
    ay: f64,
    by: f64,
    cy: f64,
}

impl Unit {
    fn new(curve: Bezier) -> Self {
        let (x1, y1, x2, y2) = (
            f64::from(curve.x1),
            f64::from(curve.y1),
            f64::from(curve.x2),
            f64::from(curve.y2),
        );
        let cx = 3.0 * x1;
        let bx = 3.0 * (x2 - x1) - cx;
        let cy = 3.0 * y1;
        let by = 3.0 * (y2 - y1) - cy;
        Self {
            ax: 1.0 - cx - bx,
            bx,
            cx,
            ay: 1.0 - cy - by,
            by,
            cy,
        }
    }

    fn x(self, t: f64) -> f64 {
        ((self.ax * t + self.bx) * t + self.cx) * t
    }

    fn y(self, t: f64) -> f64 {
        ((self.ay * t + self.by) * t + self.cy) * t
    }

    fn dx(self, t: f64) -> f64 {
        (3.0 * self.ax * t + 2.0 * self.bx) * t + self.cx
    }

    fn dy(self, t: f64) -> f64 {
        (3.0 * self.ay * t + 2.0 * self.by) * t + self.cy
    }

    /// The curve parameter whose x is `x` (x in 0..=1).
    fn solve_t(self, x: f64) -> f64 {
        let mut t = x;
        for _ in 0..8 {
            let error = self.x(t) - x;
            if error.abs() < EPSILON {
                return t;
            }
            let slope = self.dx(t);
            if slope.abs() < 1e-6 {
                break;
            }
            t -= error / slope;
        }
        let (mut low, mut high) = (0.0_f64, 1.0_f64);
        t = x;
        for _ in 0..64 {
            let value = self.x(t);
            if (value - x).abs() < EPSILON {
                return t;
            }
            if x > value {
                low = t;
            } else {
                high = t;
            }
            t = low + (high - low) * 0.5;
        }
        t
    }
}

impl Bezier {
    /// Eased progress for linear progress `x` (clamped to 0..=1).
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn ease(self, x: f32) -> f32 {
        let x = f64::from(x.clamp(0.0, 1.0));
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        let unit = Unit::new(self);
        unit.y(unit.solve_t(x)) as f32
    }

    /// The curve's slope `dy/dx` at linear progress `x`: eased units per unit
    /// of progress (multiply by `span / duration` for a velocity).
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn slope(self, x: f32) -> f32 {
        let x = f64::from(x.clamp(0.0, 1.0));
        let unit = Unit::new(self);
        let t = unit.solve_t(x);
        let dx = unit.dx(t);
        if dx.abs() < 1e-12 {
            return 0.0;
        }
        (unit.dy(t) / dx) as f32
    }

    /// How far this curve travels past its own settled value (1.0) or before
    /// its start (0.0) at its peak, as a fraction of the eased span. `glide`
    /// and `snap` never leave `0.0..=1.0` and report `0.0`; `spring` and
    /// `bounce` peak above `1.0` and report that peak's excess. Used by the
    /// motion alignment report to size the allowed overshoot for a segment
    /// without hand-tagging which named curve animated it.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn overshoot(self) -> f32 {
        let samples: u32 = 200;
        let mut peak = 0.0_f32;
        for i in 0..=samples {
            let x = i as f32 / samples as f32;
            let y = self.ease(x);
            peak = peak.max(y - 1.0).max(-y);
        }
        peak.max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{EASE, LINEAR};
    use crate::tokens::motion::{BOUNCE, Bezier, DROP, GLIDE, SNAP, SPRING};

    /// Reference points solved independently by 200-step bisection on the
    /// Bernstein form (see the checkpoint notes); tolerance 1e-5.
    /// A named curve and its `(x, y)` reference points.
    type Reference = (&'static str, Bezier, [(f32, f32); 5]);

    const REFERENCE: [Reference; 6] = [
        (
            "glide",
            GLIDE,
            [
                (0.1, 0.401_096_9),
                (0.25, 0.764_864_7),
                (0.5, 0.961_382_5),
                (0.75, 0.996_894_2),
                (0.9, 0.999_839_5),
            ],
        ),
        (
            "snap",
            SNAP,
            [
                (0.1, 0.059_332_73),
                (0.25, 0.534_076_6),
                (0.5, 0.870_362_7),
                (0.75, 0.974_958_1),
                (0.9, 0.996_433_2),
            ],
        ),
        (
            "spring",
            SPRING,
            [
                (0.1, 0.433_028_8),
                (0.25, 0.842_713_1),
                (0.5, 1.035_810_7),
                (0.75, 1.045_614_6),
                (0.9, 1.022_076_2),
            ],
        ),
        (
            "bounce",
            BOUNCE,
            [
                (0.1, 0.403_933_04),
                (0.25, 0.816_289_2),
                (0.5, 1.087_400_7),
                (0.75, 1.059_646_9),
                (0.9, 1.012_615_6),
            ],
        ),
        (
            "drop",
            DROP,
            [
                (0.1, 0.007_983_655),
                (0.25, 0.049_935_82),
                (0.5, 0.202_723_15),
                (0.75, 0.476_885_3),
                (0.9, 0.730_682_3),
            ],
        ),
        (
            "ease",
            EASE,
            [
                (0.1, 0.094_796_31),
                (0.25, 0.408_510_6),
                (0.5, 0.802_403_4),
                (0.75, 0.960_459),
                (0.9, 0.994_316_5),
            ],
        ),
    ];

    #[test]
    fn curves_match_independent_reference_points() {
        for (name, curve, points) in REFERENCE {
            for (x, expected) in points {
                let actual = curve.ease(x);
                assert!(
                    (actual - expected).abs() < 1e-5,
                    "{name}({x}) = {actual}, expected {expected}"
                );
            }
        }
    }

    #[test]
    fn endpoints_are_exact_and_input_is_clamped() {
        for curve in [GLIDE, SNAP, SPRING, BOUNCE, DROP, EASE, LINEAR] {
            assert_eq!(curve.ease(0.0).to_bits(), 0.0_f32.to_bits());
            assert_eq!(curve.ease(1.0).to_bits(), 1.0_f32.to_bits());
            assert_eq!(curve.ease(-3.0).to_bits(), 0.0_f32.to_bits());
            assert_eq!(curve.ease(7.0).to_bits(), 1.0_f32.to_bits());
        }
        assert!((LINEAR.ease(0.37) - 0.37).abs() < 1e-6);
    }

    #[test]
    fn overshooting_curves_leave_the_unit_range() {
        let peak = |curve: Bezier| {
            (0..=1000_u16)
                .map(|i| curve.ease(f32::from(i) / 1000.0))
                .fold(f32::MIN, f32::max)
        };
        assert!(
            (peak(BOUNCE) - 1.097_804).abs() < 1e-3,
            "bounce peak {}",
            peak(BOUNCE)
        );
        assert!(
            (peak(SPRING) - 1.051_685).abs() < 1e-3,
            "spring peak {}",
            peak(SPRING)
        );
        assert!(peak(GLIDE) <= 1.0 + 1e-6);
    }

    #[test]
    fn slope_matches_finite_differences() {
        for curve in [GLIDE, SNAP, BOUNCE, EASE] {
            for x in [0.2_f32, 0.5, 0.8] {
                let h = 1e-3;
                let numeric = (curve.ease(x + h) - curve.ease(x - h)) / (2.0 * h);
                let analytic = curve.slope(x);
                assert!(
                    (numeric - analytic).abs() < 2e-2 * analytic.abs().max(1.0),
                    "{curve:?} at {x}: {analytic} vs {numeric}"
                );
            }
        }
    }
}
