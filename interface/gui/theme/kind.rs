//! Defines the fifteen declaration-kind hues for `interface-gui`.
//! This module owns the one luminance plane every kind hue shares.
//! Its narrow surface keeps hue from ever encoding importance.

use compiler_ir_vocabulary::EntityKind;

use crate::theme::tokens::{Appearance, Chroma, Color, Hue, Lightness};

/// Kind hue angles, in `EntityKind::ALL` order.
///
/// The angles are spread so that no two kinds a reader sees together (functions beside types,
/// traits beside implementations) land within twenty degrees of each other.
const HUES: [f32; 15] = [
    158.0, 318.0, 206.0, 192.0, 220.0, 48.0, 276.0, 290.0, 26.0, 14.0, 338.0, 168.0, 238.0, 82.0,
    122.0,
];

/// Where every kind hue sits under Ink.
const INK_PLANE: (f32, f32) = (0.62, 0.76);

/// Where every kind hue sits under Vellum.
const VELLUM_PLANE: (f32, f32) = (0.38, 0.72);

/// The hue this kind is drawn in, on the shared luminance plane.
///
/// Every kind is exactly as loud as every other kind. A reader scanning a member list is reading
/// *which* kind, never *how important* — importance is the engine's business and it does not have
/// an opinion either.
#[must_use]
pub fn kind_color(kind: EntityKind, appearance: Appearance) -> Color {
    let (lightness, chroma) = plane(appearance);
    Color::opaque(
        Hue::degrees(hue_of(kind)),
        Chroma::new(chroma),
        Lightness::new(lightness),
    )
}

/// The same hue at a lower lightness, for the tinted ground behind a kind glyph.
#[must_use]
pub fn kind_ground(kind: EntityKind, appearance: Appearance) -> Color {
    let base = kind_color(kind, appearance);
    let lightness = match appearance {
        Appearance::Ink => 0.20,
        Appearance::Vellum => 0.92,
    };
    base.with_lightness(Lightness::new(lightness))
}

fn plane(appearance: Appearance) -> (f32, f32) {
    match appearance {
        Appearance::Ink => INK_PLANE,
        Appearance::Vellum => VELLUM_PLANE,
    }
}

fn hue_of(kind: EntityKind) -> f32 {
    EntityKind::ALL
        .into_iter()
        .zip(HUES)
        .find_map(|(candidate, hue)| (candidate == kind).then_some(hue))
        .unwrap_or(0.0)
}

/// The four status hues, which are the only colours in the interface that mean something.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Status {
    /// A capability is ready, a package compiled, a lane ran complete.
    Ok,
    /// A lane degraded, a projection truncated, a package is stale.
    Warn,
    /// A compile failed, a lane is unavailable, an address did not resolve.
    Danger,
    /// A job is running, an operand is being explained.
    Info,
}

impl Status {
    /// Every status in escalation order.
    pub const ALL: [Self; 4] = [Self::Ok, Self::Warn, Self::Danger, Self::Info];

    const fn hue(self) -> f32 {
        match self {
            Self::Ok => 142.0,
            Self::Warn => 38.0,
            Self::Danger => 0.0,
            Self::Info => 211.0,
        }
    }

    /// The colour this status is drawn in.
    #[must_use]
    pub fn color(self, appearance: Appearance) -> Color {
        let (lightness, chroma) = plane(appearance);
        Color::opaque(
            Hue::degrees(self.hue()),
            Chroma::new(chroma),
            Lightness::new(lightness),
        )
    }

    /// The tinted ground behind a status glyph.
    #[must_use]
    pub fn ground(self, appearance: Appearance) -> Color {
        let lightness = match appearance {
            Appearance::Ink => 0.19,
            Appearance::Vellum => 0.93,
        };
        self.color(appearance).with_lightness(Lightness::new(lightness))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_sits_on_one_luminance_plane() {
        for appearance in Appearance::ALL {
            let (lightness, chroma) = plane(appearance);
            for kind in EntityKind::ALL {
                let color = kind_color(kind, appearance);
                assert!(
                    (color.lightness().get() - lightness).abs() < f32::EPSILON,
                    "{kind:?} under {appearance:?} left the shared luminance plane"
                );
                assert!((color.chroma().get() - chroma).abs() < f32::EPSILON);
            }
            for status in Status::ALL {
                assert!(
                    (status.color(appearance).lightness().get() - lightness).abs() < f32::EPSILON
                );
            }
        }
    }

    #[test]
    fn kind_hues_are_distinct() {
        for (index, kind) in EntityKind::ALL.into_iter().enumerate() {
            for other in EntityKind::ALL.into_iter().skip(index + 1) {
                assert!(
                    (hue_of(kind) - hue_of(other)).abs() > f32::EPSILON,
                    "{kind:?} and {other:?} share a hue"
                );
            }
        }
    }
}
