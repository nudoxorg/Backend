//! Fluid values: a token is a ramp between end points, not a number.
//!
//! `GUTTER = 16 px at 320 → 40 px at 1440` reads at any room: 16 below 320,
//! 40 above 1440, and in between it glides with the width, the way CSS
//! `clamp()` does with container units. A region asks its own room what the
//! token is (`GUTTER.at(room)`); nothing else in the app turns a width into a
//! length.

use super::room::{Design, Room};
use gpui::{Pixels, px};
use std::marker::PhantomData;

/// One end point of a token: a region `at` design px wide holds `value`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    at: Design,
    value: f32,
}

impl Stop {
    /// The room (in design px) this end point is written for.
    #[must_use]
    pub const fn at(&self) -> Design {
        self.at
    }

    /// The token's value there.
    #[must_use]
    pub const fn value(&self) -> f32 {
        self.value
    }
}

/// An end point: at `at` design px the token is `value`.
#[must_use]
pub const fn stop(at: f32, value: f32) -> Stop {
    Stop {
        at: Design::px(at),
        value,
    }
}

/// What a token's numbers are measured in, and what reading one gives back.
pub trait Unit: Copy {
    /// What [`Fluid::at`] returns.
    type Out;

    /// The value as this unit, at text scale `scale`.
    fn out(value: f32, scale: f32) -> Self::Out;
}

/// Lengths: written in px at 100 % text, read in real px (times the text
/// scale).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Px;

impl Unit for Px {
    type Out = Pixels;

    fn out(value: f32, scale: f32) -> Pixels {
        px(value * scale)
    }
}

/// Unitless factors (a type step's share of its role, a spacing's breathing
/// room): read as written, whatever the text scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ratio;

impl Unit for Ratio {
    type Out = f32;

    fn out(value: f32, _scale: f32) -> f32 {
        value
    }
}

/// How a token moves between two end points.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Curve {
    /// Straight, like CSS `clamp()`.
    Linear,
    /// Eased at both end points (smoothstep), so a value settles into its
    /// plateau with no kink.
    Smooth,
}

impl Curve {
    fn ease(self, t: f32) -> f32 {
        match self {
            Self::Linear => t,
            Self::Smooth => t * t * (3.0 - 2.0 * t),
        }
    }
}

/// A fluid token: a value that glides with the room between its end points
/// and holds the first below them and the last above them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fluid<U: Unit> {
    stops: &'static [Stop],
    curve: Curve,
    unit: PhantomData<fn() -> U>,
}

/// A length token.
pub type Length = Fluid<Px>;
/// A factor token.
pub type Blend = Fluid<Ratio>;

impl<U: Unit> Fluid<U> {
    /// A token through `stops` (at least two, strictly widening), straight
    /// between them. Only design tokens are built: this is `pub(crate)` so a
    /// value can only come from `facet::tokens::fluid`.
    ///
    /// # Panics
    /// At compile time (in a `const`), if `stops` has fewer than two end
    /// points or they do not strictly widen.
    #[must_use]
    pub(crate) const fn new(stops: &'static [Stop]) -> Self {
        assert!(stops.len() >= 2, "a fluid token needs two end points");
        let mut i = 1;
        while i < stops.len() {
            assert!(
                stops[i - 1].at.get() < stops[i].at.get(),
                "a fluid token's end points must strictly widen"
            );
            i += 1;
        }
        Self {
            stops,
            curve: Curve::Linear,
            unit: PhantomData,
        }
    }

    /// The same token, eased at every end point.
    #[must_use]
    pub(crate) const fn smooth(self) -> Self {
        Self {
            curve: Curve::Smooth,
            ..self
        }
    }

    /// The token's end points, narrowest first.
    #[must_use]
    pub const fn stops(&self) -> &'static [Stop] {
        self.stops
    }

    /// The token at `room`: clamped to its end points, interpolated between.
    #[must_use]
    pub fn at(&self, room: Room) -> U::Out {
        U::out(self.raw(room), room.scale())
    }

    /// The token's number at `room` before the unit reads it (design px for
    /// lengths).
    #[must_use]
    pub fn raw(&self, room: Room) -> f32 {
        let x = room.design().get();
        let (first, last) = (self.stops[0], self.stops[self.stops.len() - 1]);
        if x <= first.at.get() {
            return first.value;
        }
        if x >= last.at.get() {
            return last.value;
        }
        for pair in self.stops.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            if x <= to.at.get() {
                let t = (x - from.at.get()) / (to.at.get() - from.at.get());
                return from.value + (to.value - from.value) * self.curve.ease(t);
            }
        }
        last.value
    }
}
