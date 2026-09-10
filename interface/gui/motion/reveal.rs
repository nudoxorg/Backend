//! Defines the timed reveal for `interface-gui`.
//! This module owns progress for transitions a spring would over-serve.
//! Its narrow surface keeps easing curves out of element builders.

use core::time::Duration;

use crate::motion::tokens::{MotionPreference, Timing};

/// Which way a reveal is running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// Fully hidden and still.
    Closed,
    /// Opening.
    Opening,
    /// Fully shown and still.
    Open,
    /// Closing.
    Closing,
}

impl Phase {
    /// Whether the revealed content should be built at all this frame.
    #[must_use]
    pub const fn is_present(self) -> bool {
        !matches!(self, Self::Closed)
    }

    /// Whether this phase is moving and therefore needs a frame.
    #[must_use]
    pub const fn is_moving(self) -> bool {
        matches!(self, Self::Opening | Self::Closing)
    }
}

/// One transition between hidden and shown, expressed as progress in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reveal {
    progress: f32,
    phase: Phase,
    timing: Timing,
}

impl Reveal {
    /// A reveal that starts hidden.
    #[must_use]
    pub const fn closed(timing: Timing) -> Self {
        Self {
            progress: 0.0,
            phase: Phase::Closed,
            timing,
        }
    }

    /// A reveal that starts shown, for state restored from preferences.
    #[must_use]
    pub const fn opened(timing: Timing) -> Self {
        Self {
            progress: 1.0,
            phase: Phase::Open,
            timing,
        }
    }

    /// Linear progress from hidden to shown.
    #[must_use]
    pub const fn progress(self) -> f32 {
        self.progress
    }

    /// Progress eased for opacity and offset, which is what element builders want.
    #[must_use]
    pub fn eased(self) -> f32 {
        let clamped = self.progress.clamp(0.0, 1.0);
        // Cubic ease-out: fast to visible, slow to settle, so a surface feels caught rather than
        // launched.
        let inverted = 1.0 - clamped;
        1.0 - inverted * inverted * inverted
    }

    /// The current phase.
    #[must_use]
    pub const fn phase(self) -> Phase {
        self.phase
    }

    /// Whether the revealed content is on screen at all.
    #[must_use]
    pub const fn is_present(self) -> bool {
        self.phase.is_present()
    }

    /// Whether the reader has asked for this to be shown.
    #[must_use]
    pub const fn is_opening(self) -> bool {
        matches!(self.phase, Phase::Opening | Phase::Open)
    }

    /// Asks for the content to be shown or hidden.
    pub fn set(&mut self, open: bool, motion: MotionPreference) {
        if !motion.animates() {
            self.progress = if open { 1.0 } else { 0.0 };
            self.phase = if open { Phase::Open } else { Phase::Closed };
            return;
        }
        self.phase = match (open, self.phase) {
            (true, Phase::Open) => Phase::Open,
            (true, _) => Phase::Opening,
            (false, Phase::Closed) => Phase::Closed,
            (false, _) => Phase::Closing,
        };
    }

    /// Flips the reveal.
    pub fn toggle(&mut self, motion: MotionPreference) {
        self.set(!self.is_opening(), motion);
    }

    /// Advances one real interval, returning whether another frame is needed.
    pub fn advance(&mut self, elapsed: Duration) -> bool {
        let span = self.timing.duration().as_secs_f32().max(f32::MIN_POSITIVE);
        let delta = elapsed.as_secs_f32().min(0.032) / span;
        match self.phase {
            Phase::Opening => {
                self.progress = (self.progress + delta).min(1.0);
                if self.progress >= 1.0 {
                    self.phase = Phase::Open;
                    return false;
                }
                true
            }
            Phase::Closing => {
                self.progress = (self.progress - delta).max(0.0);
                if self.progress <= 0.0 {
                    self.phase = Phase::Closed;
                    return false;
                }
                true
            }
            Phase::Closed | Phase::Open => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_motion_lands_open_without_ever_requesting_a_frame() {
        let mut reveal = Reveal::closed(Timing::Reveal);
        reveal.set(true, MotionPreference::Reduced);
        assert_eq!(reveal.phase(), Phase::Open);
        assert!(!reveal.advance(Duration::from_millis(16)));
        assert!((reveal.eased() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_reveal_reverses_from_where_it_got_to() {
        let mut reveal = Reveal::closed(Timing::Disclosure);
        reveal.set(true, MotionPreference::Full);
        assert!(reveal.advance(Duration::from_millis(32)));
        let caught = reveal.progress();
        assert!(caught > 0.0 && caught < 1.0);
        reveal.set(false, MotionPreference::Full);
        assert_eq!(reveal.phase(), Phase::Closing);
        assert!((reveal.progress() - caught).abs() < f32::EPSILON);
    }

    #[test]
    fn a_closed_reveal_is_absent_and_still() {
        let mut reveal = Reveal::closed(Timing::Reveal);
        assert!(!reveal.is_present());
        assert!(!reveal.advance(Duration::from_millis(16)));
    }
}
