//! A critically-damped spring integrated with semi-implicit Euler.
//! Panels, sheets, and the palette nib move on springs rather than on eased
//! tweens so that an interrupted motion keeps its velocity instead of jumping.
//!
//! Three details make this feel right rather than merely animated. The damping
//! ratio is 0.97 — just under critical — so a panel arrives with a trace of
//! weight instead of decelerating into nothing. The integrator sub-steps at
//! 240 Hz regardless of frame rate, so a dropped frame does not change the
//! trajectory. And the frame delta is clamped at 32 ms, so a stalled window
//! resumes where it paused rather than teleporting to the target.

use std::time::Duration;

/// Fixed integration step: 240 Hz.
const SUBSTEP: f32 = 1.0 / 240.0;

/// Longest frame delta the integrator will honour, in seconds.
const STALL_CLAMP: f32 = 0.032;

/// Distance below which a spring is considered arrived.
const EPSILON_VALUE: f32 = 0.004;

/// Speed below which a spring is considered stopped.
const EPSILON_VELOCITY: f32 = 0.02;

/// How eagerly a spring pulls toward its target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stiffness(f32);

impl Stiffness {
    /// A panel width or a sheet height: firm, arrives in about 240 ms.
    pub(crate) const PANEL: Self = Self(220.0);

    /// Returns the stiffness constant.
    pub(crate) const fn get(self) -> f32 {
        self.0
    }
}

/// One animated scalar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    stiffness: Stiffness,
}

impl Spring {
    /// Damping ratio, just under critical.
    const DAMPING_RATIO: f32 = 0.97;

    /// Creates a spring already at rest on a value.
    pub(crate) const fn at(value: f32, stiffness: Stiffness) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            stiffness,
        }
    }

    /// Returns the current value.
    pub(crate) const fn value(self) -> f32 {
        self.value
    }

    /// Returns the value the spring is travelling toward.
    pub(crate) const fn target(self) -> f32 {
        self.target
    }

    /// Returns the current velocity, useful to prove interrupted motion does
    /// not reset its momentum when a target reverses.
    pub(crate) const fn velocity(self) -> f32 {
        self.velocity
    }

    /// Returns whether the spring has arrived and needs no further frames.
    pub(crate) fn settled(self) -> bool {
        (self.value - self.target).abs() <= EPSILON_VALUE
            && self.velocity.abs() <= EPSILON_VELOCITY
    }

    /// Aims the spring at a new target, preserving its current velocity.
    ///
    /// Retargeting mid-flight is the common case — a reader drags a panel open
    /// and then changes their mind — and discarding velocity there is what
    /// makes an interface feel like it is fighting the pointer.
    pub(crate) const fn retarget(&mut self, target: f32) {
        self.target = target;
    }

    /// Places the spring on a value immediately, with no motion.
    pub(crate) const fn snap(&mut self, target: f32) {
        self.value = target;
        self.target = target;
        self.velocity = 0.0;
    }

    /// Advances the spring by one frame; returns whether it is still moving.
    pub(crate) fn advance(&mut self, delta: Duration) -> bool {
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        let mut remaining = delta.as_secs_f32().min(STALL_CLAMP);
        let stiffness = self.stiffness.get();
        let damping = 2.0 * Self::DAMPING_RATIO * stiffness.max(0.0).sqrt();
        while remaining > 0.0 {
            let step = remaining.min(SUBSTEP);
            let acceleration = -stiffness * (self.value - self.target) - damping * self.velocity;
            self.velocity += acceleration * step;
            self.value += self.velocity * step;
            remaining -= step;
        }
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}
