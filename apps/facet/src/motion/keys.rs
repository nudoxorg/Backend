//! Keyframe tracks for compound motion (drop-in squash and stretch, make
//! room, pop), with CSS `@keyframes` semantics: the timing function eases
//! each interval between two keyframes, not the whole animation.

use crate::tokens::motion::{Bezier, EMPH, GLIDE, QUICK, SCENE, SPRING, STD};
use std::borrow::Cow;
use std::time::Duration;

/// A value keyframes can interpolate. `Default` is the value of an empty
/// track.
pub trait Mix: Copy + Default + 'static {
    /// `self` blended towards `to` by `t` (t may leave 0..=1 for overshoot).
    #[must_use]
    fn mix(self, to: Self, t: f32) -> Self;
}

impl Mix for f32 {
    fn mix(self, to: Self, t: f32) -> Self {
        self + (to - self) * t
    }
}

/// A 2D pose: translation (px), scale about the centre, rotation (degrees)
/// and opacity. [`Pose::REST`] is the laid-out state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Horizontal translation in px.
    pub x: f32,
    /// Vertical translation in px.
    pub y: f32,
    /// Horizontal scale.
    pub sx: f32,
    /// Vertical scale.
    pub sy: f32,
    /// Rotation in degrees.
    pub rotate: f32,
    /// Opacity.
    pub opacity: f32,
}

impl Pose {
    /// Identity: exactly the laid-out position.
    pub const REST: Self = Self {
        x: 0.0,
        y: 0.0,
        sx: 1.0,
        sy: 1.0,
        rotate: 0.0,
        opacity: 1.0,
    };

    /// Whether this pose is the rest pose (within `1e-4`).
    #[must_use]
    pub fn is_rest(&self) -> bool {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        near(self.x, 0.0)
            && near(self.y, 0.0)
            && near(self.sx, 1.0)
            && near(self.sy, 1.0)
            && near(self.rotate, 0.0)
            && near(self.opacity, 1.0)
    }
}

impl Default for Pose {
    fn default() -> Self {
        Self::REST
    }
}

impl Mix for Pose {
    fn mix(self, to: Self, t: f32) -> Self {
        Self {
            x: self.x.mix(to.x, t),
            y: self.y.mix(to.y, t),
            sx: self.sx.mix(to.sx, t),
            sy: self.sy.mix(to.sy, t),
            rotate: self.rotate.mix(to.rotate, t),
            // Opacity never leaves 0..=1, even under an overshooting curve.
            opacity: self.opacity.mix(to.opacity, t).clamp(0.0, 1.0),
        }
    }
}

/// A keyframe track: offsets in 0..=1 (ascending; repeat an offset's value to
/// hold, like CSS `0%,4%{…}`), a duration, and the per-interval curve.
#[derive(Clone, Debug, PartialEq)]
pub struct Keys<T: Mix> {
    /// Total duration.
    pub duration: Duration,
    /// `(offset, value)` pairs; the first offset is 0 and the last 1.
    pub frames: Cow<'static, [(f32, T)]>,
    /// The curve applied inside every interval.
    pub ease: Bezier,
}

impl<T: Mix> Keys<T> {
    /// A track over borrowed static keyframes.
    #[must_use]
    pub const fn new(duration: Duration, frames: &'static [(f32, T)], ease: Bezier) -> Self {
        Self {
            duration,
            frames: Cow::Borrowed(frames),
            ease,
        }
    }

    /// A track over owned keyframes (for values known only at runtime).
    #[must_use]
    pub fn owned(duration: Duration, frames: Vec<(f32, T)>, ease: Bezier) -> Self {
        Self {
            duration,
            frames: Cow::Owned(frames),
            ease,
        }
    }

    /// The same track with another duration.
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    /// The value at `progress` (0..=1 of the duration).
    #[must_use]
    pub fn at(&self, progress: f32) -> T {
        let frames = self.frames.as_ref();
        let Some(&(_, first)) = frames.first() else {
            return T::default();
        };
        let progress = progress.clamp(0.0, 1.0);
        let mut previous = (0.0, first);
        for &(offset, value) in frames {
            if progress <= offset {
                let span = offset - previous.0;
                if span <= f32::EPSILON {
                    return value;
                }
                let local = (progress - previous.0) / span;
                return previous.1.mix(value, self.ease.ease(local));
            }
            previous = (offset, value);
        }
        previous.1
    }

    /// The value `elapsed` after the start.
    #[must_use]
    pub fn sample(&self, elapsed: Duration) -> T {
        self.at(self.progress(elapsed))
    }

    /// Linear progress `elapsed` into the track.
    #[must_use]
    pub fn progress(&self, elapsed: Duration) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// The final value.
    #[must_use]
    pub fn end(&self) -> T {
        self.at(1.0)
    }

    /// The first value.
    #[must_use]
    pub fn start(&self) -> T {
        self.at(0.0)
    }
}

const fn pose(x: f32, y: f32, sx: f32, sy: f32, opacity: f32) -> Pose {
    Pose {
        x,
        y,
        sx,
        sy,
        rotate: 0.0,
        opacity,
    }
}

/// `drop-in`: falls from above squashed tall, lands wide (45 %), rebounds
/// (62 %), settles (78 %), rests. Board keyframes, per-interval glide.
pub const DROP_IN: Keys<Pose> = Keys::new(
    SCENE,
    &[
        (0.0, pose(0.0, -140.0, 0.6, 1.3, 0.0)),
        (0.45, pose(0.0, 0.0, 1.18, 0.78, 1.0)),
        (0.62, pose(0.0, -16.0, 0.94, 1.08, 1.0)),
        (0.78, pose(0.0, 0.0, 1.06, 0.94, 1.0)),
        (1.0, Pose::REST),
    ],
    GLIDE,
);

/// `pop`: from 60 % and transparent, overshoot to 108 % at 70 %, rest.
pub const POP: Keys<Pose> = Keys::new(
    STD,
    &[
        (0.0, pose(0.0, 0.0, 0.6, 0.6, 0.0)),
        (0.7, pose(0.0, 0.0, 1.08, 1.08, 1.0)),
        (1.0, Pose::REST),
    ],
    SPRING,
);

/// `peek`: a small lift into place (popovers, peek cards).
pub const PEEK: Keys<Pose> = Keys::new(
    QUICK,
    &[(0.0, pose(0.0, 4.0, 0.95, 0.95, 0.0)), (1.0, Pose::REST)],
    GLIDE,
);

/// `rise` (v3): 10 px up into place with a fade.
pub const RISE: Keys<Pose> = Keys::new(
    STD,
    &[(0.0, pose(0.0, 10.0, 1.0, 1.0, 0.0)), (1.0, Pose::REST)],
    GLIDE,
);

/// `toast`: up 14 px from 94 %.
pub const TOAST: Keys<Pose> = Keys::new(
    EMPH,
    &[(0.0, pose(0.0, 14.0, 0.94, 0.94, 0.0)), (1.0, Pose::REST)],
    GLIDE,
);

/// `unfold`: disclosure content drops 5 px into place.
pub const UNFOLD: Keys<Pose> = Keys::new(
    QUICK,
    &[(0.0, pose(0.0, -5.0, 1.0, 1.0, 0.0)), (1.0, Pose::REST)],
    GLIDE,
);

/// `make-room`: a neighbour pushed down by `shift` px starts at its old
/// place, overshoots 5 px past the new one at 55 %, and rests.
#[must_use]
pub fn make_room(shift: f32) -> Keys<Pose> {
    Keys::owned(
        EMPH,
        vec![
            (0.0, pose(0.0, -shift, 1.0, 1.0, 1.0)),
            (0.55, pose(0.0, 5.0_f32.copysign(shift), 1.0, 1.0, 1.0)),
            (1.0, Pose::REST),
        ],
        GLIDE,
    )
}

#[cfg(test)]
mod tests {
    use super::{DROP_IN, Keys, Pose, make_room};
    use crate::tokens::motion::GLIDE;
    use std::time::Duration;

    #[test]
    fn keyframes_hit_their_values_at_their_offsets() {
        for &(offset, value) in DROP_IN.frames.iter() {
            assert_eq!(DROP_IN.at(offset), value, "at {offset}");
        }
        assert!(DROP_IN.end().is_rest());
        assert!(!DROP_IN.start().is_rest());
    }

    #[test]
    fn each_interval_is_eased_separately() {
        // Halfway through the first interval (0..0.45) glide is at 0.961.
        let halfway = DROP_IN.at(0.225);
        let expected = -140.0 + 140.0 * GLIDE.ease(0.5);
        assert!(
            (halfway.y - expected).abs() < 1e-3,
            "{} vs {expected}",
            halfway.y
        );
    }

    #[test]
    fn held_keyframes_hold() {
        static HOLD: [(f32, f32); 4] = [(0.0, 5.0), (0.4, 5.0), (0.4, 9.0), (1.0, 9.0)];
        let keys = Keys::new(Duration::from_millis(100), &HOLD, GLIDE);
        assert!((keys.at(0.2) - 5.0).abs() < 1e-6);
        assert!((keys.at(0.7) - 9.0).abs() < 1e-6);
    }

    #[test]
    fn make_room_starts_at_the_old_place_and_rests() {
        let keys = make_room(34.0);
        assert!((keys.start().y + 34.0).abs() < 1e-6);
        assert!((keys.at(0.55).y - 5.0).abs() < 1e-6);
        assert_eq!(keys.end(), Pose::REST);
        assert_eq!(keys.sample(Duration::from_secs(5)), Pose::REST);
    }
}
