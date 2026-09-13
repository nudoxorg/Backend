//! The two palettes: Ink (dark, default) and Vellum (light).
//!
//! The identity is a cool ink ground carrying warm vellum text, with exactly
//! one accent — gilt — reserved for identity the reader can copy: the crumb
//! trail, key tags, the focus ring, and the palette nib. Nothing decorative
//! is ever gilt, which is what keeps it under about five percent of the
//! pixels on a page and therefore still meaningful when it appears.
//!
//! Views never see a colour literal. They ask for a [`Paint`] role and the
//! palette answers with a tone generated from a ramp, so the light appearance
//! is the same design re-lit rather than a second set of hand-picked hexes.

use super::ramp::{Chroma, Hue, Ramp, Step};
use gpui::Hsla;

/// Which of the two palettes is lit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Appearance {
    /// Cool ink ground, warm vellum text. The default.
    #[default]
    Ink,
    /// Warm paper ground, cool ink text.
    Vellum,
}

impl Appearance {
    /// Returns the stable name used by the preferences file and the palette.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Ink => "ink",
            Self::Vellum => "vellum",
        }
    }

    /// Parses a name written by the preferences file.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "ink" => Some(Self::Ink),
            "vellum" => Some(Self::Vellum),
            _ => None,
        }
    }

    /// Returns the other appearance.
    pub(crate) const fn flipped(self) -> Self {
        match self {
            Self::Ink => Self::Vellum,
            Self::Vellum => Self::Ink,
        }
    }

    /// Returns whether this appearance is the dark one.
    pub(crate) const fn is_dark(self) -> bool {
        matches!(self, Self::Ink)
    }
}

/// One semantic colour role. This is the whole colour vocabulary of the app.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Paint {
    /// The window ground behind every panel.
    Ground,
    /// A panel resting on the ground.
    Panel,
    /// A surface floating above the page: sheets, hover cards, menus.
    Raised,
    /// A recessed well: source blocks, signature specimens.
    Sunken,
    /// The scrim drawn behind a floating surface.
    Scrim,
    /// The hairline that separates two regions.
    Hairline,
    /// A hairline that must read as a real edge.
    HairlineStrong,
    /// Headings and the declaration currently being read.
    TextStrong,
    /// Body text.
    Text,
    /// Secondary text: signatures in lists, summaries.
    TextDim,
    /// Tertiary text: counts, hints, disabled affordances.
    TextFaint,
    /// The accent, reserved for copyable identity.
    Gilt,
    /// A muted accent for the trail's separators.
    GiltDim,
    /// A translucent accent wash behind a selected identity.
    GiltWash,
    /// The wash under a hovered row.
    Hover,
    /// The wash under a selected row.
    Selected,
    /// The focus ring.
    Focus,
    /// Ready, complete, healthy.
    Ok,
    /// Partial, in progress, degraded.
    Caution,
    /// Failed, unavailable, rejected.
    Fault,
    /// A translucent wash behind a fault block.
    FaultWash,
    /// Informational, unconfigured, not applicable.
    Info,
}

/// A generated palette.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Palette {
    appearance: Appearance,
    ink: Ramp,
    vellum: Ramp,
    gilt: Ramp,
    ok: Ramp,
    caution: Ramp,
    fault: Ramp,
    info: Ramp,
}

/// Hue of the cool neutral ground.
const INK_HUE: f32 = 232.0;
/// Hue of the warm neutral text.
const VELLUM_HUE: f32 = 40.0;
/// Hue of the single accent.
const GILT_HUE: f32 = 42.0;

impl Palette {
    /// Generates the palette for one appearance.
    pub(crate) fn new(appearance: Appearance) -> Self {
        Self {
            appearance,
            ink: Ramp::new(Hue::degrees(INK_HUE), Chroma::new(0.10)),
            vellum: Ramp::new(Hue::degrees(VELLUM_HUE), Chroma::new(0.06)),
            gilt: Ramp::new(Hue::degrees(GILT_HUE), Chroma::new(0.74)),
            ok: Ramp::new(Hue::degrees(152.0), Chroma::new(0.46)),
            caution: Ramp::new(Hue::degrees(46.0), Chroma::new(0.62)),
            fault: Ramp::new(Hue::degrees(12.0), Chroma::new(0.62)),
            info: Ramp::new(Hue::degrees(208.0), Chroma::new(0.42)),
        }
    }

    /// Returns which appearance generated this palette.
    pub(crate) const fn appearance(self) -> Appearance {
        self.appearance
    }

    /// Returns the colour for one semantic role.
    pub(crate) fn paint(self, role: Paint) -> Hsla {
        match role {
            Paint::Ground | Paint::Panel | Paint::Raised | Paint::Sunken | Paint::Scrim => {
                self.surface(role)
            }
            Paint::Hairline | Paint::HairlineStrong => self.edge(role),
            Paint::TextStrong | Paint::Text | Paint::TextDim | Paint::TextFaint => self.type_ink(role),
            Paint::Gilt | Paint::GiltDim | Paint::GiltWash | Paint::Focus => self.accent(role),
            Paint::Hover | Paint::Selected => self.wash(role),
            Paint::Ok | Paint::Caution | Paint::Fault | Paint::FaultWash | Paint::Info => {
                self.signal(role)
            }
        }
    }

    fn surface(self, role: Paint) -> Hsla {
        let dark = self.appearance.is_dark();
        match role {
            Paint::Panel if dark => self.ink.tone(Step::S1),
            Paint::Panel => self.vellum.plane(0.955, Chroma::new(0.09)),
            Paint::Raised if dark => self.ink.tone(Step::S2),
            Paint::Raised => self.vellum.plane(0.995, Chroma::new(0.04)),
            Paint::Sunken if dark => self.ink.plane(0.038, Chroma::new(0.14)),
            Paint::Sunken => self.vellum.plane(0.930, Chroma::new(0.12)),
            Paint::Scrim if dark => self.ink.wash(Step::S0, 0.62),
            Paint::Scrim => self.ink.wash(Step::S3, 0.24),
            _ if dark => self.ink.tone(Step::S0),
            _ => self.vellum.plane(0.975, Chroma::new(0.07)),
        }
    }

    fn edge(self, role: Paint) -> Hsla {
        let dark = self.appearance.is_dark();
        match (role, dark) {
            (Paint::HairlineStrong, true) => self.ink.tone(Step::S5),
            (Paint::HairlineStrong, false) => self.ink.wash(Step::S7, 0.34),
            (_, true) => self.ink.tone(Step::S3),
            (_, false) => self.ink.wash(Step::S8, 0.20),
        }
    }

    fn type_ink(self, role: Paint) -> Hsla {
        let dark = self.appearance.is_dark();
        match (role, dark) {
            (Paint::TextStrong, true) => self.vellum.tone(Step::S11),
            (Paint::TextStrong, false) => self.ink.plane(0.115, Chroma::new(0.22)),
            (Paint::Text, true) => self.vellum.tone(Step::S10),
            (Paint::Text, false) => self.ink.plane(0.210, Chroma::new(0.16)),
            (Paint::TextDim, true) => self.vellum.tone(Step::S8),
            (Paint::TextDim, false) => self.ink.plane(0.420, Chroma::new(0.12)),
            (_, true) => self.vellum.tone(Step::S7),
            (_, false) => self.ink.plane(0.540, Chroma::new(0.10)),
        }
    }

    fn accent(self, role: Paint) -> Hsla {
        let dark = self.appearance.is_dark();
        let solid = if dark {
            self.gilt.plane(0.660, Chroma::new(0.74))
        } else {
            self.gilt.plane(0.440, Chroma::new(0.74))
        };
        match role {
            Paint::GiltDim if dark => self.gilt.plane(0.470, Chroma::new(0.40)),
            Paint::GiltDim => self.gilt.plane(0.620, Chroma::new(0.38)),
            Paint::GiltWash if dark => self.gilt.wash(Step::S9, 0.14),
            Paint::GiltWash => self.gilt.wash(Step::S10, 0.26),
            Paint::Focus => solid,
            _ => solid,
        }
    }

    fn wash(self, role: Paint) -> Hsla {
        let dark = self.appearance.is_dark();
        match (role, dark) {
            (Paint::Selected, true) => self.ink.tone(Step::S3),
            (Paint::Selected, false) => self.ink.wash(Step::S9, 0.16),
            (_, true) => self.ink.tone(Step::S2),
            (_, false) => self.ink.wash(Step::S10, 0.10),
        }
    }

    fn signal(self, role: Paint) -> Hsla {
        let lightness = if self.appearance.is_dark() { 0.620 } else { 0.400 };
        match role {
            Paint::Ok => self.ok.plane(lightness, Chroma::new(0.52)),
            Paint::Caution => self.caution.plane(lightness, Chroma::new(0.68)),
            Paint::FaultWash if self.appearance.is_dark() => self.fault.wash(Step::S6, 0.16),
            Paint::FaultWash => self.fault.wash(Step::S10, 0.22),
            Paint::Fault => self.fault.plane(lightness, Chroma::new(0.66)),
            _ => self.info.plane(lightness, Chroma::new(0.40)),
        }
    }

    /// Returns a hue rendered on this appearance's single chromatic plane.
    ///
    /// Declaration-kind and language glyphs use only this: they differ in hue
    /// and never in lightness, so a member list reads as one texture in
    /// greyscale and as a sorted set of kinds in colour.
    pub(crate) fn on_plane(self, hue: Hue) -> Hsla {
        let ramp = Ramp::new(hue, Chroma::new(0.0));
        if self.appearance.is_dark() {
            ramp.plane(0.620, Chroma::new(0.76))
        } else {
            ramp.plane(0.380, Chroma::new(0.72))
        }
    }

    /// Returns a translucent wash of one hue on the chromatic plane.
    pub(crate) fn plane_wash(self, hue: Hue, alpha: f32) -> Hsla {
        let mut tone = self.on_plane(hue);
        tone.a = alpha;
        tone
    }

    /// Returns the lightness of the chromatic plane, for the ramp tests.
    pub(crate) fn plane_lightness(self) -> f32 {
        if self.appearance.is_dark() { 0.620 } else { 0.380 }
    }
}
