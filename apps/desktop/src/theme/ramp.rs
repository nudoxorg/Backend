//! Twelve-step tonal ramps generated from one hue and one chroma.
//!
//! Every colour in the application is a tone drawn from a ramp, so a palette
//! is a small table of hue/chroma pairs rather than a list of hex literals.
//! Two properties follow from generating tones instead of picking them: every
//! ramp shares one lightness ladder, so text contrast is uniform across hues,
//! and a family of hues can be pinned to a single ladder step, which is how
//! declaration kinds stay preattentively distinct in colour yet collapse to
//! one shade in greyscale.

use gpui::{Hsla, hsla};

/// Number of tones on every ramp.
pub(crate) const RAMP_STEPS: usize = 12;

/// The shared lightness ladder, dark end first.
///
/// The spacing is perceptual rather than linear: closely spaced at the dark
/// end where surfaces stack, widely spaced at the light end where text sits.
const LADDER: [f32; RAMP_STEPS] = [
    0.055, 0.082, 0.112, 0.150, 0.196, 0.258, 0.336, 0.430, 0.540, 0.660, 0.790, 0.945,
];

/// A hue angle in degrees on the colour wheel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hue(f32);

impl Hue {
    /// Names a hue by its angle in degrees.
    pub(crate) const fn degrees(value: f32) -> Self {
        Self(value)
    }

    /// Returns the angle as the 0..1 turn fraction GPUI expects.
    pub(crate) fn turns(self) -> f32 {
        (self.0 / 360.0).rem_euclid(1.0)
    }
}

/// Saturation applied uniformly to every tone on a ramp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Chroma(f32);

impl Chroma {
    /// Names a saturation in the 0..1 range.
    pub(crate) const fn new(value: f32) -> Self {
        Self(value)
    }

    /// Returns the saturation value.
    pub(crate) const fn get(self) -> f32 {
        self.0
    }
}

/// One tone position on a ramp. [`Step::S0`] is the darkest tone.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Step(u8);

impl Step {
    /// Darkest tone; the window ground in the dark appearance.
    pub(crate) const S0: Self = Self(0);
    /// Panel ground.
    pub(crate) const S1: Self = Self(1);
    /// Raised surface.
    pub(crate) const S2: Self = Self(2);
    /// Hover wash.
    pub(crate) const S3: Self = Self(3);
    /// Strong hairline.
    pub(crate) const S5: Self = Self(5);
    /// Dim text.
    pub(crate) const S7: Self = Self(7);
    /// Secondary text.
    pub(crate) const S8: Self = Self(8);
    /// Chromatic plane for the dark appearance.
    pub(crate) const S9: Self = Self(9);
    /// Body text.
    pub(crate) const S10: Self = Self(10);
    /// Brightest tone; headings.
    pub(crate) const S11: Self = Self(11);

    /// Returns the ladder index of this tone.
    pub(crate) fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// A hue and chroma bound to the shared lightness ladder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ramp {
    hue: Hue,
    chroma: Chroma,
}

impl Ramp {
    /// Binds a hue and chroma to the shared ladder.
    pub(crate) const fn new(hue: Hue, chroma: Chroma) -> Self {
        Self { hue, chroma }
    }

    /// Returns the opaque tone at one ladder step.
    pub(crate) fn tone(self, step: Step) -> Hsla {
        hsla(
            self.hue.turns(),
            self.chroma.get(),
            Self::lightness(step),
            1.0,
        )
    }

    /// Returns a tone at one ladder step with an explicit alpha.
    pub(crate) fn wash(self, step: Step, alpha: f32) -> Hsla {
        hsla(
            self.hue.turns(),
            self.chroma.get(),
            Self::lightness(step),
            alpha,
        )
    }

    /// Returns a colour off the ladder, on an exact luminance plane.
    ///
    /// Kind and language hues use this so that every glyph in a member list
    /// differs only in hue, never in lightness or chroma.
    pub(crate) fn plane(self, lightness: f32, chroma: Chroma) -> Hsla {
        hsla(self.hue.turns(), chroma.get(), lightness, 1.0)
    }

    /// Returns the ladder lightness of one step.
    fn lightness(step: Step) -> f32 {
        LADDER.get(step.index()).copied().unwrap_or(0.5)
    }
}
