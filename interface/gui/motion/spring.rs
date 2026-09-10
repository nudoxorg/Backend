//! Defines the spring integrator for `interface-gui`.
//! This module owns the only place a value moves over time.
//! Its narrow surface keeps frame requests tied to real motion.

use core::time::Duration;

/// How stiff and how damped one spring is.
///
/// Damping is stated absolutely rather than as a ratio because the ratio is a *result*: every
/// preset here lands at ζ ≈ 0.97, just under critical, which settles fast and overshoots by an
/// amount a reader registers as confidence rather than as bounce.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringParams {
    stiffness: f32,
    damping: f32,
}

impl SpringParams {
    /// Composes a spring from its two constants.
    #[must_use]
    pub fn new(stiffness: f32, damping: f32) -> Self {
        Self {
            stiffness: stiffness.max(f32::MIN_POSITIVE),
            damping: damping.max(0.0),
        }
    }

    /// The restoring constant.
    #[must_use]
    pub const fn stiffness(self) -> f32 {
        self.stiffness
    }

    /// The dissipating constant.
    #[must_use]
    pub const fn damping(self) -> f32 {
        self.damping
    }

    /// The damping ratio this pair produces, for unit mass.
    #[must_use]
    pub fn ratio(self) -> f32 {
        self.damping / (2.0 * self.stiffness.sqrt())
    }
}

/// The fixed substep the integrator advances by, so motion is frame-rate independent.
const SUBSTEP: f32 = 1.0 / 240.0;

/// The longest real interval one advance will integrate.
///
/// A stalled main thread must not fling a panel across the window when it wakes; past this point
/// the spring simply loses the time.
const STALL_CLAMP: f32 = 0.032;

/// Rest is judged against the distance this spring was asked to travel, never against an absolute
/// epsilon, so a two-pixel nudge and a four-hundred-pixel panel both settle in the same feel.
const REST_DISPLACEMENT: f32 = 0.002;

/// Rest also requires the spring to have stopped, at the same relative scale.
const REST_VELOCITY: f32 = 0.04;

/// One scalar under spring control.
///
/// Retargeting mutates only the target: the spring keeps its position *and* its velocity, so a
/// panel caught mid-open and sent back closed reverses smoothly instead of restarting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    position: f32,
    velocity: f32,
    target: f32,
    travel: f32,
    params: SpringParams,
}

impl Spring {
    /// A spring at rest at one value.
    #[must_use]
    pub const fn resting(value: f32, params: SpringParams) -> Self {
        Self {
            position: value,
            velocity: 0.0,
            target: value,
            travel: 0.0,
            params,
        }
    }

    /// The current value.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.position
    }

    /// The value the spring is heading for.
    #[must_use]
    pub const fn target(self) -> f32 {
        self.target
    }

    /// The current rate of change.
    #[must_use]
    pub const fn velocity(self) -> f32 {
        self.velocity
    }

    /// Whether this spring has stopped and needs no further frames.
    #[must_use]
    pub fn at_rest(self) -> bool {
        let scale = self.travel.abs().max(1.0);
        (self.position - self.target).abs() <= scale * REST_DISPLACEMENT
            && self.velocity.abs() <= scale * REST_VELOCITY
    }

    /// Sends the spring toward a new value, preserving position and velocity.
    pub fn animate_to(&mut self, target: f32) {
        if (self.target - target).abs() <= f32::EPSILON {
            return;
        }
        self.travel = target - self.position;
        self.target = target;
    }

    /// Places the spring at a value with no motion, for reduced-motion readers and for load.
    pub fn snap_to(&mut self, value: f32) {
        self.position = value;
        self.target = value;
        self.velocity = 0.0;
        self.travel = 0.0;
    }

    /// Integrates one real interval, returning whether the spring still needs frames.
    ///
    /// Returning `false` is what lets an idle window request no frames at all: the shell asks every
    /// spring, and only asks the window for another frame when one says yes.
    pub fn advance(&mut self, elapsed: Duration) -> bool {
        if self.at_rest() {
            self.position = self.target;
            self.velocity = 0.0;
            return false;
        }
        let mut remaining = elapsed.as_secs_f32().min(STALL_CLAMP);
        while remaining > 0.0 {
            let step = remaining.min(SUBSTEP);
            let acceleration = self.params.stiffness.mul_add(
                self.target - self.position,
                -(self.params.damping * self.velocity),
            );
            self.velocity = acceleration.mul_add(step, self.velocity);
            self.position = self.velocity.mul_add(step, self.position);
            remaining -= step;
        }
        if self.at_rest() {
            self.position = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::tokens::Preset;

    fn settle(spring: &mut Spring) -> u32 {
        let mut frames = 0_u32;
        while spring.advance(Duration::from_millis(16)) {
            frames += 1;
            assert!(frames < 600, "a spring failed to settle in ten seconds");
        }
        frames
    }

    #[test]
    fn every_preset_sits_just_under_critical() {
        for preset in Preset::ALL {
            let ratio = preset.params().ratio();
            assert!(
                (0.93..=1.0).contains(&ratio),
                "{preset:?} damps at {ratio}, outside the confident band"
            );
        }
    }

    #[test]
    fn retargeting_preserves_velocity_and_does_not_move_the_value() {
        let mut spring = Spring::resting(0.0, Preset::Default.params());
        spring.animate_to(300.0);
        assert!(spring.advance(Duration::from_millis(48)));
        let (position, velocity) = (spring.value(), spring.velocity());
        assert!(velocity > 0.0, "the spring never got moving");
        spring.animate_to(0.0);
        assert!((spring.value() - position).abs() < f32::EPSILON);
        assert!((spring.velocity() - velocity).abs() < f32::EPSILON);
        assert!((spring.target() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_stall_cannot_fling_a_spring_past_its_target() {
        let mut spring = Spring::resting(0.0, Preset::Snappy.params());
        spring.animate_to(400.0);
        spring.advance(Duration::from_secs(4));
        assert!(spring.value() < 400.0, "a four-second stall teleported the spring");
    }

    #[test]
    fn snapping_settles_immediately_and_requests_no_frame() {
        let mut spring = Spring::resting(0.0, Preset::Gentle.params());
        spring.animate_to(240.0);
        spring.snap_to(240.0);
        assert!(spring.at_rest());
        assert!(!spring.advance(Duration::from_millis(16)));
    }

    #[test]
    fn snappy_settles_sooner_than_gentle_over_the_same_travel() {
        let mut snappy = Spring::resting(0.0, Preset::Snappy.params());
        let mut gentle = Spring::resting(0.0, Preset::Gentle.params());
        snappy.animate_to(300.0);
        gentle.animate_to(300.0);
        assert!(settle(&mut snappy) < settle(&mut gentle));
    }
}
