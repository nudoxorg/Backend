//! Exact Facet colour vocabulary.
//!
//! The design system uses named abyss, silver, signal and state steps instead
//! of a generated ramp. Keeping those names in the paint API makes it hard for
//! a view to accidentally turn a semantic colour into decoration. The two
//! appearances are Abyss (dark) and Glacier (daylight); preference migration
//! accepts the old serialized names at this boundary only.

use super::ramp::Hue;
use gpui::{Hsla, hsla};

/// Contrast treatment applied after the design-system values are selected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Contrast {
    /// The reference palette.
    #[default]
    Normal,
    /// Stronger text, rules and focus affordances.
    High,
}

impl Contrast {
    pub(crate) const fn is_high(self) -> bool {
        matches!(self, Self::High)
    }
}

/// The two Facet appearances.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Appearance {
    /// The dark blue abyss board.
    #[default]
    Abyss,
    /// The daylight glacier board.
    Glacier,
}

impl Appearance {
    /// Compatibility constants for preferences written by older builds. They
    /// are intentionally kept at the serialization boundary; all GUI code
    /// uses `Abyss` and `Glacier` directly.
    #[allow(non_upper_case_globals)]
    pub(crate) const Ink: Self = Self::Abyss;
    #[allow(non_upper_case_globals)]
    pub(crate) const Vellum: Self = Self::Glacier;

    /// Returns the stable preference name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Abyss => "abyss",
            Self::Glacier => "glacier",
        }
    }

    /// Parses canonical names and the pre-Facet migration names.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "abyss" | "ink" => Some(Self::Abyss),
            "glacier" | "vellum" => Some(Self::Glacier),
            _ => None,
        }
    }

    pub(crate) const fn flipped(self) -> Self {
        match self {
            Self::Abyss => Self::Glacier,
            Self::Glacier => Self::Abyss,
        }
    }

    pub(crate) const fn is_dark(self) -> bool {
        matches!(self, Self::Abyss)
    }
}

/// Semantic colour roles from the Facet boards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Paint {
    // Abyss surface steps.
    Abyss0,
    Abyss1,
    Abyss2,
    Abyss3,
    Abyss4,
    Abyss5,
    Abyss6,
    // Silver ink steps.
    Silver0,
    Silver1,
    Silver2,
    Silver3,
    Silver4,
    // Rules and overlays.
    Veil,
    Rule1,
    Rule2,
    Rule3,
    Tint,
    // Identity and focus.
    Mint,
    MintInk,
    Teal,
    Leaf,
    MintSoft,
    MintLine,
    Periwinkle,
    PeriwinkleHi,
    PeriwinkleSoft,
    PeriwinkleLine,
    // State voices.
    Amber,
    AmberSoft,
    AmberLine,
    Coral,
    CoralSoft,
    CoralLine,
    // Five fixed family hues: hue is family, shape is kind.
    FamilyNs,
    FamilyType,
    FamilyConcept,
    FamilyCall,
    FamilyValue,
    // Product-level semantic names.
    Focus,
    Action,
    Waiting,
    Stopped,
}

#[cfg(test)]
impl Paint {
    // Existing state tests exercise the old public paint surface. Keep those
    // names test-only while production views use the Facet vocabulary above.
    #[allow(non_upper_case_globals)]
    pub(crate) const Ground: Self = Self::Abyss0;
    #[allow(non_upper_case_globals)]
    pub(crate) const Text: Self = Self::Silver1;
    #[allow(non_upper_case_globals)]
    pub(crate) const Gilt: Self = Self::Mint;
}

/// Exact Facet palette. Dynamic ramps are used only for family mark geometry;
/// every UI semantic role below comes from the board's fixed hex values.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Palette {
    appearance: Appearance,
    contrast: Contrast,
}

fn hex_hsla(hex: u32, alpha: f32) -> Hsla {
    let red = ((hex >> 16) & 0xff) as f32 / 255.0;
    let green = ((hex >> 8) & 0xff) as f32 / 255.0;
    let blue = (hex & 0xff) as f32 / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let lightness = (max + min) * 0.5;
    let delta = max - min;
    let (hue, saturation) = if delta <= f32::EPSILON {
        (0.0, 0.0)
    } else {
        let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
        let hue = if (max - red).abs() <= f32::EPSILON {
            ((green - blue) / delta).rem_euclid(6.0)
        } else if (max - green).abs() <= f32::EPSILON {
            (blue - red) / delta + 2.0
        } else {
            (red - green) / delta + 4.0
        } / 6.0;
        (hue.rem_euclid(1.0), saturation)
    };
    hsla(hue, saturation, lightness, alpha)
}

impl Palette {
    pub(crate) fn new(appearance: Appearance) -> Self {
        Self::new_with_contrast(appearance, Contrast::Normal)
    }

    pub(crate) fn new_with_contrast(appearance: Appearance, contrast: Contrast) -> Self {
        Self {
            appearance,
            contrast,
        }
    }

    pub(crate) const fn appearance(self) -> Appearance {
        self.appearance
    }

    pub(crate) fn paint(self, role: Paint) -> Hsla {
        self.contrast_adjust(role, self.canonical(role))
    }

    /// Returns the exact dark/light token for a role.
    fn canonical(self, role: Paint) -> Hsla {
        let dark = self.appearance.is_dark();
        let value = match role {
            Paint::Abyss0 => (if dark { 0x030814 } else { 0xdfe5ee }, 1.0),
            Paint::Abyss1 => (if dark { 0x060d1b } else { 0xeef2f7 }, 1.0),
            Paint::Abyss2 => (if dark { 0x0a1424 } else { 0xf6f8fb }, 1.0),
            Paint::Abyss3 => (if dark { 0x0f1b2f } else { 0xffffff }, 1.0),
            Paint::Abyss4 => (if dark { 0x15243c } else { 0xeef2f8 }, 1.0),
            Paint::Abyss5 => (if dark { 0x1c304f } else { 0xe2e9f3 }, 1.0),
            Paint::Abyss6 => (if dark { 0x25406a } else { 0xcfdaea }, 1.0),
            Paint::Silver0 => (if dark { 0xf5f7fb } else { 0x0a1222 }, 1.0),
            Paint::Silver1 => (if dark { 0xd2d9e5 } else { 0x1d2940 }, 1.0),
            Paint::Silver2 => (if dark { 0x9aa6ba } else { 0x4b5a75 }, 1.0),
            Paint::Silver3 => (if dark { 0x74819a } else { 0x66758f }, 1.0),
            Paint::Silver4 => (if dark { 0x4c5870 } else { 0xa3aec2 }, 1.0),
            Paint::Veil => (
                if dark { 0x030814 } else { 0xc8d2e4 },
                if dark { 0.62 } else { 0.60 },
            ),
            Paint::Rule1 => (
                if dark { 0x9eb0ff } else { 0x1e326e },
                if dark { 0.07 } else { 0.08 },
            ),
            Paint::Rule2 => (
                if dark { 0x9eb0ff } else { 0x1e326e },
                if dark { 0.12 } else { 0.14 },
            ),
            Paint::Rule3 => (
                if dark { 0x9eb0ff } else { 0x1e326e },
                if dark { 0.22 } else { 0.26 },
            ),
            Paint::Tint => (
                if dark { 0xbecdff } else { 0x1e326e },
                if dark { 0.07 } else { 0.05 },
            ),
            Paint::PeriwinkleSoft => (
                if dark { 0x93a2fa } else { 0x4b5bd6 },
                if dark { 0.13 } else { 0.10 },
            ),
            Paint::Mint => (if dark { 0x6cebad } else { 0x0f9d6a }, 1.0),
            Paint::MintInk => (if dark { 0x03231a } else { 0xffffff }, 1.0),
            Paint::Teal => (if dark { 0x3fcdc6 } else { 0x0d8f8f }, 1.0),
            Paint::Leaf => (if dark { 0x2fb96c } else { 0x0c8a4e }, 1.0),
            Paint::MintSoft => (
                if dark { 0x62e6a6 } else { 0x0f9d6a },
                if dark { 0.12 } else { 0.10 },
            ),
            Paint::MintLine => (
                if dark { 0x62e6a6 } else { 0x0f9d6a },
                if dark { 0.34 } else { 0.36 },
            ),
            Paint::Periwinkle | Paint::Focus => (if dark { 0x93a2fa } else { 0x4b5bd6 }, 1.0),
            Paint::PeriwinkleHi => (if dark { 0xbcc6ff } else { 0x3443b8 }, 1.0),
            Paint::PeriwinkleLine => (if dark { 0x93a2fa } else { 0x4b5bd6 }, 0.40),
            Paint::Amber | Paint::Waiting => (if dark { 0xf4bb6a } else { 0xa8650a }, 1.0),
            Paint::AmberSoft => (
                if dark { 0xf4bb6a } else { 0xa8650a },
                if dark { 0.13 } else { 0.10 },
            ),
            Paint::AmberLine => (
                if dark { 0xf4bb6a } else { 0xa8650a },
                if dark { 0.38 } else { 0.36 },
            ),
            Paint::Coral | Paint::Stopped => (if dark { 0xff7a8a } else { 0xc8324a }, 1.0),
            Paint::CoralSoft => (
                if dark { 0xff7a8a } else { 0xc8324a },
                if dark { 0.13 } else { 0.09 },
            ),
            Paint::CoralLine => (
                if dark { 0xff7a8a } else { 0xc8324a },
                if dark { 0.40 } else { 0.36 },
            ),
            Paint::FamilyNs => (if dark { 0xa9b6cc } else { 0x55647e }, 1.0),
            Paint::FamilyType => (if dark { 0x5fe0b4 } else { 0x0b8f68 }, 1.0),
            Paint::FamilyConcept => (if dark { 0xd59cf5 } else { 0x8b3fc0 }, 1.0),
            Paint::FamilyCall => (if dark { 0x8fa6ff } else { 0x3d52d0 }, 1.0),
            Paint::FamilyValue => (if dark { 0xf3c06e } else { 0xa2650c }, 1.0),
            Paint::Action => (if dark { 0x6cebad } else { 0x0f9d6a }, 1.0),
        };
        hex_hsla(value.0, value.1)
    }

    fn contrast_adjust(self, role: Paint, mut color: Hsla) -> Hsla {
        if !self.contrast.is_high() {
            return color;
        }
        let dark = self.appearance.is_dark();
        match role {
            Paint::Silver0 => color.color.lightness = if dark { 0.98 } else { 0.08 },
            Paint::Silver1 => color.color.lightness = if dark { 0.87 } else { 0.16 },
            Paint::Silver2 => color.color.lightness = if dark { 0.74 } else { 0.30 },
            Paint::Silver3 => color.color.lightness = if dark { 0.62 } else { 0.42 },
            Paint::Rule1 | Paint::Rule2 | Paint::Rule3 => color.alpha = 1.0,
            Paint::Periwinkle | Paint::Focus => {
                color.alpha = 1.0;
                color.color.saturation = color.color.saturation.max(0.82);
                color.color.lightness = if dark { 0.82 } else { 0.28 };
            }
            Paint::MintSoft | Paint::PeriwinkleSoft => {
                color.alpha = if dark { 0.25 } else { 0.36 };
            }
            Paint::Tint => color.alpha = if dark { 0.42 } else { 0.24 },
            _ => {}
        }
        color
    }

    pub(crate) const fn contrast(self) -> Contrast {
        self.contrast
    }

    /// Projects arbitrary kind/language hues into the five fixed family hues
    /// in the Facet language board. Shape remains the declaration's meaning.
    pub(crate) fn on_plane(self, hue: Hue) -> Hsla {
        let degrees = hue.degrees_value().rem_euclid(360.0);
        let role = match degrees {
            220.0..=245.0 => Paint::FamilyNs,
            90.0..=180.0 => Paint::FamilyType,
            180.0..=250.0 => Paint::FamilyCall,
            250.0..=360.0 => Paint::FamilyConcept,
            0.0..=90.0 => Paint::FamilyValue,
            _ => Paint::FamilyNs,
        };
        self.paint(role)
    }

    pub(crate) fn plane_wash(self, hue: Hue, alpha: f32) -> Hsla {
        let mut tone = self.on_plane(hue);
        tone.alpha = alpha;
        tone
    }
}

#[cfg(test)]
mod tests {
    use super::{Appearance, Contrast, Paint, Palette};

    #[test]
    fn high_contrast_increases_text_and_edge_separation_in_both_appearances() {
        for appearance in [Appearance::Abyss, Appearance::Glacier] {
            let normal = Palette::new(appearance);
            let high = Palette::new_with_contrast(appearance, Contrast::High);
            let text_delta = (high.paint(Paint::Silver1).color.lightness
                - high.paint(Paint::Abyss0).color.lightness)
                .abs();
            let normal_delta = (normal.paint(Paint::Silver1).color.lightness
                - normal.paint(Paint::Abyss0).color.lightness)
                .abs();
            assert!(text_delta >= normal_delta);
            assert!(high.paint(Paint::Rule1).alpha >= normal.paint(Paint::Rule1).alpha);
            assert_eq!(high.contrast(), Contrast::High);
        }
    }

    #[test]
    fn high_contrast_focus_is_opaque_and_distinct_from_body_text() {
        for appearance in [Appearance::Abyss, Appearance::Glacier] {
            let palette = Palette::new_with_contrast(appearance, Contrast::High);
            let focus = palette.paint(Paint::Focus);
            let text = palette.paint(Paint::Silver1);
            assert_eq!(focus.alpha, 1.0);
            assert!(
                (focus.color.hue.into_degrees() - text.color.hue.into_degrees()).abs() > 0.01
                    || (focus.color.lightness - text.color.lightness).abs() > 0.2
            );
        }
    }
}
