//! Room: the width a region has, and the one place a width becomes a number
//! a token can be read at.

use gpui::{Pixels, px};

/// The smallest and largest text scale a room is set in (the settings' 50 %
/// and 400 %); anything outside would divide a width into nonsense.
const SCALE_MIN: f32 = 0.5;
const SCALE_MAX: f32 = 4.0;

/// A width in px at 100 % text: the unit every fluid token is written in.
///
/// A window 1440 px wide at 200 % text is 720 design px wide, and lays out
/// exactly as a 720 px window at 100 %: nothing ever overlaps because the
/// text got bigger.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Design(f32);

impl Design {
    /// `value` design px.
    #[must_use]
    pub const fn px(value: f32) -> Self {
        Self(value)
    }

    /// The number of design px.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// The width a region actually has: not the window's, the region's own, as
/// measured in the frame it is laid out in. Every fluid value and every mode
/// is read from a room, never from a bare width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Room {
    width: Pixels,
    scale: f32,
}

impl Room {
    /// A region `width` wide, set at text scale `scale` (1.0 = 100 %).
    #[must_use]
    pub fn new(width: Pixels, scale: f32) -> Self {
        Self {
            width: width.max(px(0.0)),
            scale: scale.clamp(SCALE_MIN, SCALE_MAX),
        }
    }

    /// The region's width in real px.
    #[must_use]
    pub const fn width(self) -> Pixels {
        self.width
    }

    /// The text scale (1.0 = 100 %).
    #[must_use]
    pub const fn scale(self) -> f32 {
        self.scale
    }

    /// The region's width in design px (width ÷ text scale).
    #[must_use]
    pub fn design(self) -> Design {
        Design(f32::from(self.width) / self.scale)
    }

    /// A child region `width` wide, in the same text scale.
    #[must_use]
    pub fn within(self, width: Pixels) -> Self {
        Self::new(width, self.scale)
    }

    /// A child region inset by `by` on both sides.
    #[must_use]
    pub fn inset(self, by: Pixels) -> Self {
        self.within(self.width - by * 2.0)
    }

    /// The region left when a column `taken` wide sits beside it.
    #[must_use]
    pub fn beside(self, taken: Pixels) -> Self {
        self.within(self.width - taken)
    }
}
