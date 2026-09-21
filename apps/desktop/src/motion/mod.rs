//! Motion tokens and the spring integrator every animated value uses.
//! Three durations cover the whole application, and reduced motion collapses
//! all of them to zero rather than merely shortening them.
//!
//! The rule this module exists to enforce: an idle window schedules no frames.
//! Every animated value here is either settled — in which case nothing asks for
//! another frame — or moving because the reader just did something. There is no
//! ambient animation, no pulse, no shimmer.

pub(crate) mod spring;
pub(crate) mod clock;

use gpui::{Animation, ease_out_quint, linear};
use std::time::Duration;

/// How long a transition takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Beat {
    /// 90 ms — hover, selection, a chip lighting up.
    Touch,
    /// 140 ms — a reveal, a tab change, a page swap.
    Reveal,
    /// 220 ms — a disclosure opening, a sheet dropping.
    Unfold,
}

impl Beat {
    /// Returns this beat's duration, or zero when motion is reduced.
    pub(crate) const fn duration(self, reduced: bool) -> Duration {
        if reduced {
            return Duration::from_millis(0);
        }
        Duration::from_millis(match self {
            Self::Touch => 90,
            Self::Reveal => 140,
            Self::Unfold => 220,
        })
    }
}

/// Builds a one-shot animation for one beat.
///
/// Under reduced motion the animation still exists but runs for zero time, so
/// call sites do not need two code paths and the element still lands in its
/// final state on the very first frame.
pub(crate) fn once(beat: Beat, reduced: bool) -> Animation {
    let animation = Animation::new(beat.duration(reduced));
    if reduced {
        return animation.with_easing(linear);
    }
    animation.with_easing(ease_out_quint())
}

/// Returns the opacity an entering element should have at animation delta.
///
/// Entering content starts at a third rather than at nothing: a page that
/// fades from invisible reads as a flash, while one that fades from dim reads
/// as arriving.
pub(crate) fn entering_opacity(delta: f32) -> f32 {
    0.65_f32.mul_add(delta, 0.35).clamp(0.0, 1.0)
}
