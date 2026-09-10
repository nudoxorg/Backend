//! Defines the seven ecosystem marks for `interface-gui`.
//! This module owns the glyph and hue drawn beside every package coordinate.
//! Its narrow surface keeps ecosystem identity out of element builders.

use interface_core::PackageEcosystem;
use interface_identity::{ALL_ECOSYSTEMS, ecosystem_tag};

use crate::theme::tokens::{Appearance, Chroma, Color, Hue, Lightness};

/// One ecosystem's mark: a glyph from a distinct shape family and a hue of its own.
///
/// The glyphs deliberately do not share a family — a half-square, a hatched square, a half-circle,
/// a hatched circle, and two triangles read apart at eleven pixels in a way that seven variations
/// on one square never do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EcosystemMark {
    glyph: &'static str,
    hue: u16,
}

impl EcosystemMark {
    /// The glyph drawn at the head of a package row.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        self.glyph
    }

    /// The colour that glyph is drawn in.
    #[must_use]
    pub fn color(self, appearance: Appearance) -> Color {
        let (lightness, chroma) = match appearance {
            Appearance::Ink => (0.64, 0.58),
            Appearance::Vellum => (0.40, 0.62),
        };
        Color::opaque(
            Hue::degrees(f32::from(self.hue)),
            Chroma::new(chroma),
            Lightness::new(lightness),
        )
    }
}

const MARKS: [EcosystemMark; 7] = [
    EcosystemMark { glyph: "◧", hue: 24 },
    EcosystemMark { glyph: "▩", hue: 356 },
    EcosystemMark { glyph: "◑", hue: 218 },
    EcosystemMark { glyph: "◍", hue: 190 },
    EcosystemMark { glyph: "△", hue: 340 },
    EcosystemMark { glyph: "▽", hue: 262 },
    EcosystemMark { glyph: "◈", hue: 96 },
];

/// The mark for one ecosystem.
#[must_use]
pub fn ecosystem_mark(ecosystem: PackageEcosystem) -> EcosystemMark {
    ALL_ECOSYSTEMS
        .into_iter()
        .zip(MARKS)
        .find_map(|(candidate, mark)| (candidate == ecosystem).then_some(mark))
        .unwrap_or(EcosystemMark { glyph: "◌", hue: 0 })
}

/// The tag a reader types and the interface echoes, for one ecosystem.
#[must_use]
pub fn ecosystem_label(ecosystem: PackageEcosystem) -> &'static str {
    ecosystem_tag(ecosystem).as_str()
}

/// The three coordinates a first run offers, one per ecosystem a reader is most likely to hold.
pub const EXAMPLE_COORDINATES: [&str; 3] = [
    "cargo:serde@1.0.196",
    "npm:@types/node@20.11.0",
    "pypi:httpx@0.27.0",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ecosystem_has_a_distinct_mark() {
        for (index, ecosystem) in ALL_ECOSYSTEMS.into_iter().enumerate() {
            let mark = ecosystem_mark(ecosystem);
            assert_ne!(mark.glyph(), "◌", "{ecosystem:?} fell through to the fallback");
            for other in ALL_ECOSYSTEMS.into_iter().skip(index + 1) {
                assert_ne!(mark.glyph(), ecosystem_mark(other).glyph());
            }
        }
    }

    #[test]
    fn every_example_coordinate_parses() {
        for text in EXAMPLE_COORDINATES {
            assert!(
                interface_identity::PackageCoordinate::parse(text).is_ok(),
                "{text} is offered on the first-run card but does not parse"
            );
        }
    }
}
