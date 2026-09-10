//! Defines the twelve-step colour ramp for `interface-gui`.
//! This module owns the mapping from one hue recipe to a complete usable scale.
//! Its narrow surface means a new accent is three numbers, never a new palette.

use crate::theme::tokens::{Appearance, Chroma, Color, Hue, Lightness};

/// One rung of a ramp, in the order a surface stacks.
///
/// A step is a *role*, not a shade: `Border` is whatever separates two planes in this appearance,
/// which is lighter than the surface under Ink and darker than it under Vellum. Element builders
/// name roles, so the same builder is correct in both themes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Step {
    /// The window ground.
    AppBg,
    /// A panel resting on the ground.
    SubtleBg,
    /// A control or row at rest.
    ElementBg,
    /// The same control under the pointer.
    ElementHover,
    /// The same control while pressed or selected.
    ElementActive,
    /// A separator inside a panel.
    BorderSubtle,
    /// The edge of a control.
    Border,
    /// The edge of a focused or emphasized control.
    BorderStrong,
    /// The filled accent: a nib, a progress fill, a focus ring.
    Solid,
    /// The same fill under the pointer.
    SolidHover,
    /// Secondary text: counts, summaries, timestamps.
    TextLow,
    /// Primary text.
    TextHigh,
}

impl Step {
    /// Every step in stacking order.
    pub const ALL: [Self; 12] = [
        Self::AppBg,
        Self::SubtleBg,
        Self::ElementBg,
        Self::ElementHover,
        Self::ElementActive,
        Self::BorderSubtle,
        Self::Border,
        Self::BorderStrong,
        Self::Solid,
        Self::SolidHover,
        Self::TextLow,
        Self::TextHigh,
    ];

    /// How many steps a ramp carries.
    pub const COUNT: usize = 12;

    const fn index(self) -> usize {
        match self {
            Self::AppBg => 0,
            Self::SubtleBg => 1,
            Self::ElementBg => 2,
            Self::ElementHover => 3,
            Self::ElementActive => 4,
            Self::BorderSubtle => 5,
            Self::Border => 6,
            Self::BorderStrong => 7,
            Self::Solid => 8,
            Self::SolidHover => 9,
            Self::TextLow => 10,
            Self::TextHigh => 11,
        }
    }
}

/// A step's lightness plane and how much of the recipe's saturation it keeps.
#[derive(Clone, Copy, Debug)]
struct Shape {
    lightness: f32,
    tint: f32,
}

const INK: [Shape; Step::COUNT] = [
    Shape { lightness: 0.055, tint: 0.35 },
    Shape { lightness: 0.080, tint: 0.35 },
    Shape { lightness: 0.130, tint: 0.35 },
    Shape { lightness: 0.175, tint: 0.40 },
    Shape { lightness: 0.215, tint: 0.45 },
    Shape { lightness: 0.245, tint: 0.50 },
    Shape { lightness: 0.300, tint: 0.55 },
    Shape { lightness: 0.390, tint: 0.60 },
    Shape { lightness: 0.000, tint: 1.00 },
    Shape { lightness: 0.070, tint: 1.00 },
    Shape { lightness: 0.620, tint: 0.45 },
    Shape { lightness: 0.930, tint: 0.30 },
];

const VELLUM: [Shape; Step::COUNT] = [
    Shape { lightness: 0.900, tint: 0.35 },
    Shape { lightness: 0.970, tint: 0.30 },
    Shape { lightness: 0.935, tint: 0.35 },
    Shape { lightness: 1.000, tint: 0.30 },
    Shape { lightness: 0.915, tint: 0.45 },
    Shape { lightness: 0.880, tint: 0.50 },
    Shape { lightness: 0.830, tint: 0.55 },
    Shape { lightness: 0.745, tint: 0.60 },
    Shape { lightness: 0.000, tint: 1.00 },
    Shape { lightness: -0.060, tint: 1.00 },
    Shape { lightness: 0.430, tint: 0.45 },
    Shape { lightness: 0.150, tint: 0.30 },
];

/// The three numbers that define one ramp.
///
/// `solid` is stated per appearance because a filled accent has to clear its own ground: gilt sits
/// at `.66` on Ink and `.44` on Vellum, and no single number is legible on both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RampRecipe {
    hue: Hue,
    chroma: Chroma,
    solid_ink: Lightness,
    solid_vellum: Lightness,
}

impl RampRecipe {
    /// A ramp with no hue of its own, for surfaces and for text.
    #[must_use]
    pub fn neutral(hue: f32, chroma: f32, solid_ink: f32, solid_vellum: f32) -> Self {
        Self::chromatic(hue, chroma, solid_ink, solid_vellum)
    }

    /// A ramp that carries an accent hue at full saturation in its solid steps.
    #[must_use]
    pub fn chromatic(hue: f32, chroma: f32, solid_ink: f32, solid_vellum: f32) -> Self {
        Self {
            hue: Hue::degrees(hue),
            chroma: Chroma::new(chroma),
            solid_ink: Lightness::new(solid_ink),
            solid_vellum: Lightness::new(solid_vellum),
        }
    }

    /// The recipe's hue angle.
    #[must_use]
    pub const fn hue(self) -> Hue {
        self.hue
    }

    const fn solid(self, appearance: Appearance) -> Lightness {
        match appearance {
            Appearance::Ink => self.solid_ink,
            Appearance::Vellum => self.solid_vellum,
        }
    }
}

/// Twelve resolved colours, computed once when the appearance changes.
///
/// Ramps are cheap plain data. The reader holds one per role and every render reads them; nothing
/// recomputes a colour inside a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ramp {
    steps: [Color; Step::COUNT],
}

impl Ramp {
    /// Resolves one recipe against one appearance.
    #[must_use]
    pub fn resolve(recipe: RampRecipe, appearance: Appearance) -> Self {
        let shapes = match appearance {
            Appearance::Ink => &INK,
            Appearance::Vellum => &VELLUM,
        };
        let solid = recipe.solid(appearance);
        let mut steps = [Color::opaque(recipe.hue, Chroma::NONE, solid); Step::COUNT];
        for (slot, step) in steps.iter_mut().zip(Step::ALL) {
            let shape = shape_of(shapes, step);
            let lightness = match step {
                Step::Solid | Step::SolidHover => solid.shifted(shape.lightness),
                _ => Lightness::new(shape.lightness),
            };
            *slot = Color::opaque(recipe.hue, recipe.chroma.scaled(shape.tint), lightness);
        }
        Self { steps }
    }

    /// One resolved step.
    #[must_use]
    pub fn step(&self, step: Step) -> Color {
        // `Step::index` is total over a twelve-slot array, so the fallback is unreachable in
        // practice and still costs nothing.
        self.steps
            .get(step.index())
            .copied()
            .unwrap_or_else(|| Color::opaque(Hue::degrees(0.0), Chroma::NONE, Lightness::new(0.5)))
    }
}

fn shape_of(shapes: &[Shape; Step::COUNT], step: Step) -> Shape {
    shapes
        .get(step.index())
        .copied()
        .unwrap_or(Shape { lightness: 0.5, tint: 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gilt() -> RampRecipe {
        RampRecipe::chromatic(42.0, 0.74, 0.66, 0.44)
    }

    #[test]
    fn ink_surfaces_climb_and_vellum_borders_descend() {
        let ink = Ramp::resolve(gilt(), Appearance::Ink);
        assert!(ink.step(Step::AppBg).lightness() < ink.step(Step::ElementBg).lightness());
        assert!(ink.step(Step::TextLow).lightness() < ink.step(Step::TextHigh).lightness());

        let vellum = Ramp::resolve(gilt(), Appearance::Vellum);
        assert!(vellum.step(Step::BorderStrong).lightness() < vellum.step(Step::Border).lightness());
        assert!(vellum.step(Step::TextHigh).lightness() < vellum.step(Step::TextLow).lightness());
    }

    #[test]
    fn solid_hover_separates_from_solid_in_both_appearances() {
        for appearance in Appearance::ALL {
            let ramp = Ramp::resolve(gilt(), appearance);
            assert!(
                (ramp.step(Step::Solid).lightness().get()
                    - ramp.step(Step::SolidHover).lightness().get())
                .abs()
                    > 0.02,
                "{appearance:?} draws hover at the rest lightness"
            );
        }
    }
}
