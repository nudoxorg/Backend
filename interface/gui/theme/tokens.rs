//! Defines colour, type, space, and radius tokens for `interface-gui`.
//! This module owns the scalar vocabulary every other token module composes.
//! Its narrow surface keeps raw floats from leaking into element builders.

use core::fmt;

/// Which of the two shipped themes is in force.
///
/// The reader picks one; nothing in the interface reads the operating system's preference, because
/// a document reader that repaints itself at dusk is a document reader that lost the reader's place.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Appearance {
    /// Ink: near-black warm-neutral surfaces, the default.
    #[default]
    Ink,
    /// Vellum: warm paper surfaces under an off-white ground.
    Vellum,
}

impl Appearance {
    /// Both appearances in presentation order.
    pub const ALL: [Self; 2] = [Self::Ink, Self::Vellum];

    /// The word shown beside the appearance control.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ink => "Ink",
            Self::Vellum => "Vellum",
        }
    }

    /// The other appearance, for a two-state toggle.
    #[must_use]
    pub const fn flipped(self) -> Self {
        match self {
            Self::Ink => Self::Vellum,
            Self::Vellum => Self::Ink,
        }
    }

    /// Whether this appearance paints light marks on dark ground.
    #[must_use]
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Ink)
    }
}

/// One hue angle in degrees, normalized to `[0, 360)` on construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hue(f32);

impl Hue {
    /// Wraps any angle onto the colour wheel.
    #[must_use]
    pub fn degrees(angle: f32) -> Self {
        let wrapped = angle % 360.0;
        Self(if wrapped < 0.0 { wrapped + 360.0 } else { wrapped })
    }

    /// The angle in degrees.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// The angle as a turn fraction, which is how GPUI spells hue.
    #[must_use]
    pub fn turns(self) -> f32 {
        self.0 / 360.0
    }
}

/// Saturation in `[0, 1]`, clamped on construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chroma(f32);

impl Chroma {
    /// A fully desaturated axis.
    pub const NONE: Self = Self(0.0);

    /// Clamps a requested saturation into range.
    #[must_use]
    pub fn new(value: f32) -> Self {
        Self(value.clamp(0.0, 1.0))
    }

    /// The saturation value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// The same axis scaled toward grey, for surfaces and borders that only tint.
    #[must_use]
    pub fn scaled(self, factor: f32) -> Self {
        Self::new(self.0 * factor)
    }
}

/// Lightness in `[0, 1]`, clamped on construction.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Lightness(f32);

impl Lightness {
    /// Clamps a requested lightness into range.
    #[must_use]
    pub fn new(value: f32) -> Self {
        Self(value.clamp(0.0, 1.0))
    }

    /// The lightness value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// The same plane shifted, staying inside range.
    #[must_use]
    pub fn shifted(self, delta: f32) -> Self {
        Self::new(self.0 + delta)
    }
}

/// Opacity in `[0, 1]`, clamped on construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Alpha(f32);

impl Alpha {
    /// Fully opaque.
    pub const OPAQUE: Self = Self(1.0);

    /// The six-rung ladder every translucent fill in the interface draws from.
    ///
    /// Nothing outside this ladder is allowed: an interface with eleven opacities reads as
    /// eleven accidents rather than one system.
    pub const LADDER: [Self; 6] = [
        Self(0.06),
        Self(0.12),
        Self(0.22),
        Self(0.38),
        Self(0.55),
        Self(0.72),
    ];

    /// The scrim behind an inline sheet.
    pub const SCRIM_SOFT: Self = Self(0.42);

    /// The scrim behind a modal palette.
    pub const SCRIM_HARD: Self = Self(0.62);

    /// Clamps a requested opacity into range.
    #[must_use]
    pub fn new(value: f32) -> Self {
        Self(value.clamp(0.0, 1.0))
    }

    /// The opacity value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// One resolved colour: hue, saturation, lightness, opacity.
///
/// The interface never writes a hex literal. Every colour in every element comes from a
/// [`crate::theme::Ramp`] step, a kind hue, or a status hue, so a theme change is a data change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    hue: Hue,
    chroma: Chroma,
    lightness: Lightness,
    alpha: Alpha,
}

impl Color {
    /// Composes one opaque colour.
    #[must_use]
    pub const fn opaque(hue: Hue, chroma: Chroma, lightness: Lightness) -> Self {
        Self {
            hue,
            chroma,
            lightness,
            alpha: Alpha::OPAQUE,
        }
    }

    /// The same colour at a different opacity.
    #[must_use]
    pub const fn with_alpha(self, alpha: Alpha) -> Self {
        Self { alpha, ..self }
    }

    /// The same colour on a different luminance plane.
    #[must_use]
    pub const fn with_lightness(self, lightness: Lightness) -> Self {
        Self { lightness, ..self }
    }

    /// The hue angle.
    #[must_use]
    pub const fn hue(self) -> Hue {
        self.hue
    }

    /// The saturation.
    #[must_use]
    pub const fn chroma(self) -> Chroma {
        self.chroma
    }

    /// The lightness.
    #[must_use]
    pub const fn lightness(self) -> Lightness {
        self.lightness
    }

    /// The opacity.
    #[must_use]
    pub const fn alpha(self) -> Alpha {
        self.alpha
    }
}

impl fmt::Display for Color {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "hsla({:.0}, {:.0}%, {:.1}%, {:.2})",
            self.hue.get(),
            self.chroma.get() * 100.0,
            self.lightness.get() * 100.0,
            self.alpha.get()
        )
    }
}

/// A length in root-em units, so one interface-size preference scales the whole reader.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Rems(f32);

impl Rems {
    /// Builds a length from a design measurement taken at a sixteen-pixel root.
    #[must_use]
    pub const fn at_sixteen(pixels: f32) -> Self {
        Self(pixels / 16.0)
    }

    /// The length in rem units.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// The length resolved against one root size, in pixels.
    #[must_use]
    pub const fn pixels(self, root: f32) -> f32 {
        self.0 * root
    }
}

/// The four weights the interface draws with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Weight {
    /// Prose and long-form reading.
    Regular,
    /// Interface chrome, which needs a touch more presence at small sizes.
    Medium,
    /// Small capitals and section labels.
    Book,
    /// Titles and identity.
    Semibold,
}

impl Weight {
    /// The numeric weight this rung asks the font for.
    #[must_use]
    pub const fn value(self) -> u16 {
        match self {
            Self::Regular => 400,
            Self::Medium => 450,
            Self::Book => 500,
            Self::Semibold => 600,
        }
    }
}

/// Whether a role is set in the reading face or the code face.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Face {
    /// The user-interface and prose face.
    Text,
    /// The fixed-pitch face used for every identity, signature, and code span.
    Mono,
}

/// One complete text role: size, leading, weight, tracking, and face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeStyle {
    size: Rems,
    leading: Rems,
    weight: Weight,
    tracking: Rems,
    face: Face,
}

impl TypeStyle {
    /// The type size.
    #[must_use]
    pub const fn size(self) -> Rems {
        self.size
    }

    /// The line box height.
    #[must_use]
    pub const fn leading(self) -> Rems {
        self.leading
    }

    /// The weight.
    #[must_use]
    pub const fn weight(self) -> Weight {
        self.weight
    }

    /// Letter tracking, which is non-zero only for small capitals.
    #[must_use]
    pub const fn tracking(self) -> Rems {
        self.tracking
    }

    /// Which face carries this role.
    #[must_use]
    pub const fn face(self) -> Face {
        self.face
    }
}

const fn role(pixels: f32, leading: f32, weight: Weight, face: Face, tracking: f32) -> TypeStyle {
    TypeStyle {
        size: Rems::at_sixteen(pixels),
        leading: Rems::at_sixteen(leading),
        weight,
        tracking: Rems::at_sixteen(tracking),
        face,
    }
}

/// The eight text roles. There is no ninth: a size that is not on this list is a mistake.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Role {
    /// The one large title on a page.
    Display,
    /// Section titles and panel headers.
    Title,
    /// Documentation prose.
    Body,
    /// Interface chrome: buttons, rows, chips.
    Ui,
    /// Dense secondary chrome: counts, timestamps.
    Dense,
    /// Small capitals for group labels.
    Caption,
    /// Inline code spans and key tags.
    Mono,
    /// The signature specimen block at the head of a page.
    Specimen,
}

impl Role {
    /// Every role in presentation order.
    pub const ALL: [Self; 8] = [
        Self::Display,
        Self::Title,
        Self::Body,
        Self::Ui,
        Self::Dense,
        Self::Caption,
        Self::Mono,
        Self::Specimen,
    ];

    /// The resolved style for this role.
    #[must_use]
    pub const fn style(self) -> TypeStyle {
        match self {
            Self::Display => role(20.0, 28.0, Weight::Semibold, Face::Text, 0.0),
            Self::Title => role(15.0, 22.0, Weight::Semibold, Face::Text, 0.0),
            Self::Body => role(14.5, 23.0, Weight::Regular, Face::Text, 0.0),
            Self::Ui => role(13.0, 20.0, Weight::Medium, Face::Text, 0.0),
            Self::Dense => role(12.0, 16.0, Weight::Regular, Face::Text, 0.0),
            Self::Caption => role(11.0, 16.0, Weight::Book, Face::Text, 0.2),
            Self::Mono => role(12.5, 19.0, Weight::Regular, Face::Mono, 0.0),
            Self::Specimen => role(13.5, 22.0, Weight::Regular, Face::Mono, 0.0),
        }
    }
}

/// The eight spacing steps. Gaps and padding come from here or they are wrong.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Space {
    /// Four pixels: inside a chip.
    Hair,
    /// Eight pixels: between a glyph and its label.
    Tight,
    /// Twelve pixels: between rows.
    Snug,
    /// Sixteen pixels: panel padding.
    Base,
    /// Twenty pixels: between a heading and its body.
    Loose,
    /// Twenty-four pixels: between sections.
    Wide,
    /// Thirty-two pixels: reader gutters.
    Gutter,
    /// Forty pixels: above a page title.
    Chapter,
}

impl Space {
    /// The step in rem units.
    #[must_use]
    pub const fn rems(self) -> Rems {
        Rems::at_sixteen(match self {
            Self::Hair => 4.0,
            Self::Tight => 8.0,
            Self::Snug => 12.0,
            Self::Base => 16.0,
            Self::Loose => 20.0,
            Self::Wide => 24.0,
            Self::Gutter => 32.0,
            Self::Chapter => 40.0,
        })
    }
}

/// The four corner radii.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Radius {
    /// Chips and tags.
    Chip,
    /// Rows and small controls.
    Control,
    /// Panels and cards.
    Panel,
    /// Floating surfaces: the palette, hover cards.
    Floating,
}

impl Radius {
    /// The radius in rem units.
    #[must_use]
    pub const fn rems(self) -> Rems {
        Rems::at_sixteen(match self {
            Self::Chip => 4.0,
            Self::Control => 6.0,
            Self::Panel => 10.0,
            Self::Floating => 14.0,
        })
    }
}

/// The reading measure: prose never runs wider than this, whatever the window does.
pub const MEASURE_PIXELS: f32 = 640.0;

/// Border width, in pixels, for every border in the interface.
pub const HAIRLINE_PIXELS: f32 = 1.0;

/// How far a floating surface's shadow falls.
pub const ELEVATION_OFFSET_PIXELS: f32 = 4.0;

/// How soft a floating surface's shadow is.
pub const ELEVATION_BLUR_PIXELS: f32 = 16.0;

/// How dark a floating surface's shadow is.
pub const ELEVATION_ALPHA: Alpha = Alpha(0.18);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_clamp_and_hues_wrap() {
        assert!((Hue::degrees(-30.0).get() - 330.0).abs() < f32::EPSILON);
        assert!((Hue::degrees(400.0).get() - 40.0).abs() < f32::EPSILON);
        assert!((Chroma::new(2.0).get() - 1.0).abs() < f32::EPSILON);
        assert!(Lightness::new(-1.0).get().abs() < f32::EPSILON);
        assert!((Rems::at_sixteen(20.0).pixels(16.0) - 20.0).abs() < f32::EPSILON);
        assert!((Rems::at_sixteen(20.0).pixels(18.0) - 22.5).abs() < f32::EPSILON);
    }

    #[test]
    fn every_role_has_leading_at_least_its_size() {
        for role in Role::ALL {
            let style = role.style();
            assert!(
                style.leading().get() >= style.size().get(),
                "{role:?} sets leading tighter than its size"
            );
        }
    }
}
