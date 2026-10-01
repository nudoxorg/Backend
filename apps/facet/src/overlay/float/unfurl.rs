//! The unfurl's schedule (PLAN §2c, `sig-peek.png`): how a card comes out
//! of the word it belongs to and goes back into it.
//!
//! | ms | enter | | ms | leave |
//! |---|---|---|---|---|
//! | 0–60 | the underline draws under the token | | 0–120 | the body rolls up |
//! | 60–140 | the underline travels to become the card's facing edge | | 120–210 | the edge shrinks back to the word |
//! | 100–220 | the body unrolls from that edge | | 210–300 | the underline undraws |
//!
//! Each band runs on its own window, so the leave is not the entrance
//! played backwards. A reversal (asked for again while leaving, or left
//! while entering) resumes the other schedule at the latest time none of
//! its bands is ahead of where the card is ([`resume`]): every band goes on
//! from where it is, at least one of them at once, and nothing jumps.

use crate::tokens::motion::{Bezier, DROP, GLIDE};

/// An entrance's length, ms.
pub const ENTER_MS: f32 = 220.0;
/// An exit's length, ms.
pub const EXIT_MS: f32 = 300.0;

/// Each band's window on the entrance, ms, in [`Bands`] order (line, edge,
/// body).
const ENTER: [(f32, f32); 3] = [(0.0, 60.0), (60.0, 140.0), (100.0, 220.0)];
/// Each band's window on the exit, ms (it runs 1 → 0): the body first.
const EXIT: [(f32, f32); 3] = [(210.0, 300.0), (120.0, 210.0), (0.0, 120.0)];

/// How far each part of an unfurl has run (0..1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bands {
    /// The underline under the anchor's token has drawn.
    pub line: f32,
    /// The underline has travelled to become the card's facing edge.
    pub edge: f32,
    /// The body has unrolled away from that edge.
    pub body: f32,
}

impl Bands {
    /// Nothing drawn.
    pub const CLOSED: Self = Self { line: 0.0, edge: 0.0, body: 0.0 };
    /// At rest, open.
    pub const OPEN: Self = Self { line: 1.0, edge: 1.0, body: 1.0 };

    /// Every band at `value` (a card that does not unfurl).
    #[must_use]
    pub const fn all(value: f32) -> Self {
        Self { line: value, edge: value, body: value }
    }

    const fn get(self) -> [f32; 3] {
        [self.line, self.edge, self.body]
    }

    const fn from_array([line, edge, body]: [f32; 3]) -> Self {
        Self { line, edge, body }
    }

    /// The three bands' mean: the card's scalar presence (0 only when all
    /// are closed, 1 only when all are open).
    #[must_use]
    pub fn mean(self) -> f32 {
        (self.line + self.edge + self.body) / 3.0
    }
}

/// The curve a band leaves on: the body rolls up accelerating (it lets go),
/// the lines glide.
const fn exit_curve(band: usize) -> Bezier {
    if band == 2 { DROP } else { GLIDE }
}

fn window_progress((from, to): (f32, f32), ms: f32) -> f32 {
    ((ms - from) / (to - from)).clamp(0.0, 1.0)
}

/// The entrance `ms` into it, from closed.
#[must_use]
pub fn entering(ms: f32) -> Bands {
    Bands::from_array(ENTER.map(|window| GLIDE.ease(window_progress(window, ms))))
}

/// The exit `ms` into it, from open.
#[must_use]
pub fn leaving(ms: f32) -> Bands {
    let mut bands = [0.0; 3];
    for (band, window) in EXIT.iter().enumerate() {
        bands[band] = 1.0 - exit_curve(band).ease(window_progress(*window, ms));
    }
    Bands::from_array(bands)
}

/// The latest linear progress at which `curve` is still below `value`
/// (0..1, monotone): a band placed there is never ahead of `value`.
fn inverse(curve: Bezier, value: f32) -> f32 {
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..40 {
        let mid = (low + high) * 0.5;
        if curve.ease(mid) < value {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

/// Which way an unfurl runs: the card opening (its bands rise) or closing
/// (they fall).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Run {
    /// Opening.
    Entering,
    /// Closing.
    Leaving,
}

impl Run {
    /// The way a presence that goes from `from` to `to` runs.
    #[must_use]
    pub fn toward(from: f32, to: f32) -> Self {
        if to >= from { Self::Entering } else { Self::Leaving }
    }

    /// The schedule's length, ms.
    #[must_use]
    pub const fn ms(self) -> f32 {
        match self {
            Self::Entering => ENTER_MS,
            Self::Leaving => EXIT_MS,
        }
    }
}

/// Where on its schedule a segment that starts from `from` begins, ms: the
/// latest time at which no band of the fresh schedule is ahead of `from`.
#[must_use]
pub fn resume(from: Bands, run: Run) -> f32 {
    let from = from.get();
    let latest = (0..3).map(|band| {
        let value = from[band];
        match run {
            Run::Entering => {
                let (start, end) = ENTER[band];
                if value <= 0.0 {
                    start
                } else if value >= 1.0 {
                    ENTER_MS
                } else {
                    start + (end - start) * inverse(GLIDE, value)
                }
            }
            Run::Leaving => {
                let (start, end) = EXIT[band];
                if value >= 1.0 {
                    start
                } else if value <= 0.0 {
                    EXIT_MS
                } else {
                    start + (end - start) * inverse(exit_curve(band), 1.0 - value)
                }
            }
        }
    });
    latest.fold(f32::INFINITY, f32::min).clamp(0.0, run.ms())
}

/// The bands `ms` into a schedule (entering or leaving) that a segment
/// joined at [`resume`]`(from, run)`: each band holds where it was until the
/// fresh schedule passes it.
#[must_use]
pub fn at(from: Bands, run: Run, ms: f32) -> Bands {
    let fresh = match run {
        Run::Entering => entering(ms),
        Run::Leaving => leaving(ms),
    }
    .get();
    let from = from.get();
    let mut bands = [0.0; 3];
    for band in 0..3 {
        bands[band] = match run {
            Run::Entering => from[band].max(fresh[band]),
            Run::Leaving => from[band].min(fresh[band]),
        };
    }
    Bands::from_array(bands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entrance_lands_each_band_on_its_storyboard_time() {
        let at60 = entering(60.0);
        assert!((at60.line - 1.0).abs() < 1e-6 && at60.edge == 0.0 && at60.body == 0.0, "{at60:?}");
        let at140 = entering(140.0);
        assert!((at140.edge - 1.0).abs() < 1e-6 && at140.body > 0.0 && at140.body < 1.0, "{at140:?}");
        assert_eq!(entering(ENTER_MS), Bands::OPEN);
        assert_eq!(entering(99.0).body, 0.0, "the body waits for the edge");
    }

    #[test]
    fn the_leave_rolls_the_body_up_before_the_edge_moves() {
        let at120 = leaving(120.0);
        assert!(at120.body == 0.0 && (at120.edge - 1.0).abs() < 1e-6 && (at120.line - 1.0).abs() < 1e-6, "{at120:?}");
        let at210 = leaving(210.0);
        assert!(at210.edge == 0.0 && (at210.line - 1.0).abs() < 1e-6, "{at210:?}");
        assert_eq!(leaving(EXIT_MS), Bands::CLOSED);
    }

    /// Two poses agree to the precision the bisection in `inverse` has (a
    /// reversal resumes from where the bands are, to within a hair).
    fn near(a: Bands, b: Bands) -> bool {
        a.get().iter().zip(b.get()).all(|(a, b)| (a - b).abs() < 1e-5)
    }

    #[test]
    fn a_reversal_goes_on_from_where_every_band_is() {
        for stop in [10.0, 50.0, 90.0, 110.0, 130.0, 180.0, 219.0] {
            let pose = entering(stop);
            let offset = resume(pose, Run::Leaving);
            let first = at(pose, Run::Leaving, offset);
            assert!(near(first, pose), "leaving at +{stop} jumped: {first:?} vs {pose:?}");
            // Something moves at once, nothing ever rises while leaving.
            let next = at(pose, Run::Leaving, offset + 4.0);
            assert!(next.mean() < pose.mean(), "leaving at +{stop} stalled: {pose:?} -> {next:?}");
        }
        for stop in [20.0, 100.0, 150.0, 250.0, 290.0] {
            let pose = leaving(stop);
            let offset = resume(pose, Run::Entering);
            let first = at(pose, Run::Entering, offset);
            assert!(near(first, pose), "re-entering at +{stop} jumped: {first:?} vs {pose:?}");
            let next = at(pose, Run::Entering, offset + 4.0);
            assert!(next.mean() > pose.mean(), "re-entering at +{stop} stalled: {pose:?} -> {next:?}");
        }
    }
}
