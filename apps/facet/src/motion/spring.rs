//! An analytic damped spring: frame-rate independent, retargetable, and
//! velocity-preserving.
//!
//! The state is closed-form in the time since the last retarget, so the value
//! at any virtual time is the same no matter how many frames were drawn in
//! between. Retargeting samples the current position and velocity and starts
//! a new closed-form segment from them, so velocity is continuous by
//! construction.

/// A spring described the way designers tune one: the period of the undamped
/// oscillation and the damping ratio (1 = critical, < 1 overshoots).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// Undamped period in seconds (stiffness = (2π / response)²).
    pub response: f32,
    /// Damping ratio ζ.
    pub damping: f32,
}

/// Crisp and nearly critical: plates, pointers, the focus bevel.
pub const SNAPPY: Spring = Spring {
    response: 0.28,
    damping: 0.86,
};
/// Critically damped and unhurried: panels, splitters, scroll follow.
pub const GENTLE: Spring = Spring {
    response: 0.45,
    damping: 1.0,
};
/// A visible overshoot (about 9 %, like the `bounce` curve).
pub const BOUNCY: Spring = Spring {
    response: 0.42,
    damping: 0.6,
};

/// Position (relative to the target) and velocity, in units and units/s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Phase {
    /// Displacement from the target.
    pub offset: f64,
    /// Velocity in units per second.
    pub velocity: f64,
}

impl Spring {
    /// The step response's peak overshoot as a fraction of the span
    /// (`exp(-ζπ / √(1-ζ²))` below critical damping, `0` at or above it).
    #[must_use]
    pub fn overshoot_ratio(self) -> f32 {
        let zeta = f64::from(self.damping);
        if zeta >= 1.0 {
            return 0.0;
        }
        #[allow(clippy::cast_possible_truncation)]
        let ratio = (-zeta * std::f64::consts::PI / (1.0 - zeta * zeta).sqrt()).exp() as f32;
        ratio
    }

    fn omega(self) -> f64 {
        std::f64::consts::TAU / f64::from(self.response.max(1e-3))
    }

    /// The phase `dt` seconds after `start`.
    #[must_use]
    pub fn step(self, start: Phase, dt: f64) -> Phase {
        let omega = self.omega();
        let zeta = f64::from(self.damping.max(0.0));
        let (x0, v0) = (start.offset, start.velocity);
        if dt <= 0.0 {
            return start;
        }
        if (zeta - 1.0).abs() < 1e-4 {
            // Critically damped: x = (a + b t) e^{-ωt}.
            let b = v0 + omega * x0;
            let decay = (-omega * dt).exp();
            return Phase {
                offset: (x0 + b * dt) * decay,
                velocity: (b - omega * (x0 + b * dt)) * decay,
            };
        }
        if zeta < 1.0 {
            let omega_d = omega * (1.0 - zeta * zeta).sqrt();
            let decay = (-zeta * omega * dt).exp();
            let b = (v0 + zeta * omega * x0) / omega_d;
            let (sin, cos) = (omega_d * dt).sin_cos();
            let offset = decay * (x0 * cos + b * sin);
            let velocity = decay
                * ((b * omega_d - zeta * omega * x0) * cos
                    - (x0 * omega_d + zeta * omega * b) * sin);
            return Phase { offset, velocity };
        }
        // Overdamped: two real roots.
        let root = (zeta * zeta - 1.0).sqrt();
        let r1 = -omega * (zeta - root);
        let r2 = -omega * (zeta + root);
        let c2 = (v0 - r1 * x0) / (r2 - r1);
        let c1 = x0 - c2;
        let (e1, e2) = ((r1 * dt).exp(), (r2 * dt).exp());
        Phase {
            offset: c1 * e1 + c2 * e2,
            velocity: c1 * r1 * e1 + c2 * r2 * e2,
        }
    }

    /// Whether a phase is at rest for a value that must land within `rest`.
    #[must_use]
    pub fn at_rest(self, phase: Phase, rest: f64) -> bool {
        phase.offset.abs() <= rest && phase.velocity.abs() <= rest * self.omega()
    }

    /// Seconds until the spring started at `start` comes to rest (at most 30).
    #[must_use]
    pub fn settle_time(self, start: Phase, rest: f64) -> f64 {
        let step = f64::from(self.response.max(1e-3)) / 60.0;
        let mut t = 0.0;
        while t < 30.0 {
            if self.at_rest(self.step(start, t), rest) {
                return t;
            }
            t += step;
        }
        30.0
    }
}

#[cfg(test)]
mod tests {
    use super::{BOUNCY, GENTLE, Phase, SNAPPY, Spring};

    fn run(spring: Spring, start: Phase, frames: usize, dt: f64) -> Phase {
        let mut phase = start;
        for _ in 0..frames {
            phase = spring.step(phase, dt);
        }
        phase
    }

    #[test]
    fn stepping_is_frame_rate_independent() {
        let start = Phase {
            offset: -100.0,
            velocity: 0.0,
        };
        for spring in [SNAPPY, GENTLE, BOUNCY] {
            let whole = spring.step(start, 0.24);
            let at_200 = run(spring, start, 48, 0.005);
            let at_30 = run(spring, start, 8, 0.03);
            assert!((whole.offset - at_200.offset).abs() < 1e-6, "{spring:?}");
            assert!((whole.offset - at_30.offset).abs() < 1e-6, "{spring:?}");
            assert!((whole.velocity - at_30.velocity).abs() < 1e-5, "{spring:?}");
        }
    }

    #[test]
    fn velocity_is_the_derivative_of_offset() {
        let start = Phase {
            offset: 40.0,
            velocity: -300.0,
        };
        for spring in [
            SNAPPY,
            GENTLE,
            BOUNCY,
            Spring {
                response: 0.3,
                damping: 1.6,
            },
        ] {
            for t in [0.01, 0.1, 0.25] {
                let h = 1e-6;
                let numeric = (spring.step(start, t + h).offset - spring.step(start, t - h).offset)
                    / (2.0 * h);
                let analytic = spring.step(start, t).velocity;
                assert!(
                    (numeric - analytic).abs() < 1e-3 * analytic.abs().max(1.0),
                    "{spring:?} t={t}"
                );
            }
        }
    }

    #[test]
    fn bouncy_overshoots_and_critical_does_not() {
        let start = Phase {
            offset: -1.0,
            velocity: 0.0,
        };
        let peak = |spring: Spring| {
            (1..400)
                .map(|i| spring.step(start, f64::from(i) / 400.0).offset)
                .fold(f64::MIN, f64::max)
        };
        assert!(peak(BOUNCY) > 0.05, "bouncy peak {}", peak(BOUNCY));
        assert!(peak(GENTLE) <= 1e-9, "critical peak {}", peak(GENTLE));
    }

    #[test]
    fn settle_time_lands_at_rest() {
        let start = Phase {
            offset: 120.0,
            velocity: 0.0,
        };
        for spring in [SNAPPY, GENTLE, BOUNCY] {
            let t = spring.settle_time(start, 0.05);
            assert!(t > 0.1 && t < 3.0, "{spring:?} settles in {t}");
            assert!(spring.at_rest(spring.step(start, t), 0.05));
        }
    }
}
