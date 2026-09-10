//! Defines the resolved palette for `interface-gui`.
//! This module owns the three ramps and the rule that governs the accent.
//! Its narrow surface means every element asks for a role, never a colour.

use crate::theme::{
    ramp::{Ramp, RampRecipe, Step},
    tokens::{Alpha, Appearance, Color},
};

/// Which ramp a role draws from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Surface {
    /// Cool near-neutral: every panel, border, control, and scrim.
    Ground,
    /// Warm near-neutral: every glyph of running text.
    Vellum,
    /// The single accent. See the rule on [`Palette`].
    Gilt,
}

/// The three ramps resolved against one appearance, plus the ladders that sit outside them.
///
/// # The gilt rule
///
/// Gilt marks **an identity you can copy** and nothing else. In practice that is exactly four
/// things: the crumb trail address, the key tag, the focus ring, and the palette selection nib.
///
/// Navigable text is *not* gilt. A type token, a member name, a prose link, and a relation row are
/// all vellum text carrying their kind glyph in the kind hue, a dotted hairline underline, and a
/// pointer cursor; the underline goes solid on hover. That is what makes them navigable, and it
/// leaves the accent free to mean one thing.
///
/// The rule is enforced structurally: `crate::ui::identity_text` is the only builder permitted to
/// paint gilt, and it is the builder used by the crumb trail, the key tag, and nothing else. A
/// typical page therefore holds gilt on well under five percent of its lit pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    appearance: Appearance,
    ground: Ramp,
    vellum: Ramp,
    gilt: Ramp,
}

impl Palette {
    /// Resolves both neutrals and the accent for one appearance.
    #[must_use]
    pub fn resolve(appearance: Appearance) -> Self {
        Self {
            appearance,
            ground: Ramp::resolve(RampRecipe::neutral(232.0, 0.10, 0.45, 0.40), appearance),
            vellum: Ramp::resolve(RampRecipe::neutral(40.0, 0.06, 0.50, 0.42), appearance),
            gilt: Ramp::resolve(RampRecipe::chromatic(42.0, 0.74, 0.66, 0.44), appearance),
        }
    }

    /// Which appearance produced this palette.
    #[must_use]
    pub const fn appearance(self) -> Appearance {
        self.appearance
    }

    /// One step of one ramp.
    #[must_use]
    pub fn color(&self, surface: Surface, step: Step) -> Color {
        match surface {
            Surface::Ground => self.ground.step(step),
            Surface::Vellum => self.vellum.step(step),
            Surface::Gilt => self.gilt.step(step),
        }
    }

    /// The window ground.
    #[must_use]
    pub fn app_background(&self) -> Color {
        self.ground.step(Step::AppBg)
    }

    /// A panel resting on the ground.
    #[must_use]
    pub fn panel(&self) -> Color {
        self.ground.step(Step::SubtleBg)
    }

    /// A row or control at rest.
    #[must_use]
    pub fn element(&self) -> Color {
        self.ground.step(Step::ElementBg)
    }

    /// A row or control under the pointer.
    #[must_use]
    pub fn element_hover(&self) -> Color {
        self.ground.step(Step::ElementHover)
    }

    /// A selected row.
    #[must_use]
    pub fn element_active(&self) -> Color {
        self.ground.step(Step::ElementActive)
    }

    /// A separator inside a panel.
    #[must_use]
    pub fn divider(&self) -> Color {
        self.ground.step(Step::BorderSubtle)
    }

    /// The edge of a control.
    #[must_use]
    pub fn border(&self) -> Color {
        self.ground.step(Step::Border)
    }

    /// The edge of an emphasized control.
    #[must_use]
    pub fn border_strong(&self) -> Color {
        self.ground.step(Step::BorderStrong)
    }

    /// Primary running text.
    #[must_use]
    pub fn text(&self) -> Color {
        self.vellum.step(Step::TextHigh)
    }

    /// Secondary text: counts, summaries, timestamps.
    #[must_use]
    pub fn text_low(&self) -> Color {
        self.vellum.step(Step::TextLow)
    }

    /// Text that is present but inert, such as an unresolved target.
    #[must_use]
    pub fn text_inert(&self) -> Color {
        self.ground.step(Step::BorderStrong)
    }

    /// The accent fill. See the gilt rule on [`Palette`] before reaching for this.
    #[must_use]
    pub fn accent(&self) -> Color {
        self.gilt.step(Step::Solid)
    }

    /// The accent fill under the pointer.
    #[must_use]
    pub fn accent_hover(&self) -> Color {
        self.gilt.step(Step::SolidHover)
    }

    /// The tinted ground behind a selected identity.
    #[must_use]
    pub fn accent_ground(&self) -> Color {
        self.gilt.step(Step::ElementActive)
    }

    /// Text drawn on top of an accent fill.
    #[must_use]
    pub fn on_accent(&self) -> Color {
        match self.appearance {
            Appearance::Ink => self.gilt.step(Step::AppBg),
            Appearance::Vellum => self.gilt.step(Step::ElementHover),
        }
    }

    /// The scrim behind a modal surface.
    #[must_use]
    pub fn scrim(&self) -> Color {
        self.ground.step(Step::AppBg).with_alpha(Alpha::SCRIM_HARD)
    }

    /// The scrim behind an inline sheet.
    #[must_use]
    pub fn scrim_soft(&self) -> Color {
        self.ground.step(Step::AppBg).with_alpha(Alpha::SCRIM_SOFT)
    }

    /// The shadow colour a floating surface casts.
    #[must_use]
    pub fn elevation(&self) -> Color {
        self.ground
            .step(Step::AppBg)
            .with_alpha(crate::theme::tokens::ELEVATION_ALPHA)
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::resolve(Appearance::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_clears_its_own_ground_in_both_appearances() {
        for appearance in Appearance::ALL {
            let palette = Palette::resolve(appearance);
            let ground = palette.app_background().lightness().get();
            let text = palette.text().lightness().get();
            assert!(
                (ground - text).abs() > 0.60,
                "{appearance:?} sets text {text} against ground {ground}"
            );
            let low = palette.text_low().lightness().get();
            assert!(
                (ground - low).abs() > 0.30,
                "{appearance:?} sets secondary text too close to its ground"
            );
        }
    }

    #[test]
    fn accent_clears_the_panel_it_sits_on() {
        for appearance in Appearance::ALL {
            let palette = Palette::resolve(appearance);
            assert!(
                (palette.accent().lightness().get() - palette.panel().lightness().get()).abs()
                    > 0.25
            );
        }
    }
}
