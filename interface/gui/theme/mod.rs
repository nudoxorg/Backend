//! Defines the complete visual token system for `interface-gui`.
//! This module owns the theme value every view reads and nothing else writes.
//! Its narrow surface keeps colour, type, and space decisions out of views.
//!
//! # The one accent
//!
//! Read the gilt rule on [`Palette`] before adding a colour to anything. It is the single
//! constraint that keeps a page from becoming decoration: gilt marks an identity a reader can
//! copy, navigable text is vellum with a kind-hued glyph and a dotted underline, and the two are
//! never confused.

mod ecosystem;
mod kind;
mod palette;
mod ramp;
mod tokens;

pub use ecosystem::{EXAMPLE_COORDINATES, EcosystemMark, ecosystem_label, ecosystem_mark};
pub use kind::{Status, kind_color, kind_ground};
pub use palette::{Palette, Surface};
pub use ramp::{Ramp, RampRecipe, Step};
pub use tokens::{
    Alpha, Appearance, Chroma, Color, ELEVATION_ALPHA, ELEVATION_BLUR_PIXELS,
    ELEVATION_OFFSET_PIXELS, Face, HAIRLINE_PIXELS, Hue, Lightness, MEASURE_PIXELS, Radius, Rems,
    Role, Space, TypeStyle, Weight,
};

/// How large the reader has asked the interface to be.
///
/// This drives one call to `Window::set_rem_size`, and every length in the token system is stated
/// in rems, so the entire interface scales as one piece rather than as forty independent controls.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum InterfaceSize {
    /// Fourteen-pixel root: more of the page, tighter chrome.
    Compact,
    /// Sixteen-pixel root, the design measurement.
    #[default]
    Regular,
    /// Eighteen-pixel root.
    Large,
    /// Twenty-pixel root.
    Larger,
}

impl InterfaceSize {
    /// Every size in ascending order.
    pub const ALL: [Self; 4] = [Self::Compact, Self::Regular, Self::Large, Self::Larger];

    /// The root size in pixels.
    #[must_use]
    pub const fn root_pixels(self) -> f32 {
        match self {
            Self::Compact => 14.0,
            Self::Regular => 16.0,
            Self::Large => 18.0,
            Self::Larger => 20.0,
        }
    }

    /// The word shown beside the size control.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Regular => "Regular",
            Self::Large => "Large",
            Self::Larger => "Larger",
        }
    }

    /// The next size up, saturating at the largest.
    #[must_use]
    pub const fn larger(self) -> Self {
        match self {
            Self::Compact => Self::Regular,
            Self::Regular => Self::Large,
            Self::Large | Self::Larger => Self::Larger,
        }
    }

    /// The next size down, saturating at the smallest.
    #[must_use]
    pub const fn smaller(self) -> Self {
        match self {
            Self::Compact | Self::Regular => Self::Compact,
            Self::Large => Self::Regular,
            Self::Larger => Self::Large,
        }
    }
}

/// The resolved theme: one palette, one root size, and the two faces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    palette: Palette,
    size: InterfaceSize,
}

impl Theme {
    /// Resolves a theme from the two preferences that determine it.
    #[must_use]
    pub fn resolve(appearance: Appearance, size: InterfaceSize) -> Self {
        Self {
            palette: Palette::resolve(appearance),
            size,
        }
    }

    /// The resolved colours.
    #[must_use]
    pub const fn palette(&self) -> &Palette {
        &self.palette
    }

    /// The appearance in force.
    #[must_use]
    pub const fn appearance(&self) -> Appearance {
        self.palette.appearance()
    }

    /// The interface size in force.
    #[must_use]
    pub const fn size(&self) -> InterfaceSize {
        self.size
    }

    /// The root size in pixels, which is the one number handed to the window.
    #[must_use]
    pub const fn root_pixels(&self) -> f32 {
        self.size.root_pixels()
    }

    /// Resolves one length against the root size.
    #[must_use]
    pub const fn pixels(&self, length: Rems) -> f32 {
        length.pixels(self.root_pixels())
    }

    /// The family name for one face.
    #[must_use]
    pub const fn family(&self, face: Face) -> &'static str {
        match face {
            Face::Text => TEXT_FAMILY,
            Face::Mono => MONO_FAMILY,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::resolve(Appearance::default(), InterfaceSize::default())
    }
}

/// The reading face. GPUI resolves this through the platform's font fallback chain.
pub const TEXT_FAMILY: &str = ".SystemUIFont";

/// The code face, which carries every identity, signature, and key tag.
pub const MONO_FAMILY: &str = "SF Mono";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_size_saturates_at_both_ends() {
        assert_eq!(InterfaceSize::Compact.smaller(), InterfaceSize::Compact);
        assert_eq!(InterfaceSize::Larger.larger(), InterfaceSize::Larger);
        assert_eq!(InterfaceSize::Regular.larger(), InterfaceSize::Large);
    }

    #[test]
    fn one_rem_length_scales_with_the_root() {
        let compact = Theme::resolve(Appearance::Ink, InterfaceSize::Compact);
        let larger = Theme::resolve(Appearance::Ink, InterfaceSize::Larger);
        let title = Role::Title.style().size();
        assert!(compact.pixels(title) < larger.pixels(title));
    }
}
