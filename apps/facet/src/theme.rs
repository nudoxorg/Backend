//! The active FACET theme: appearance, contrast, density, text scale, motion
//! and the held reveal modes.
//!
//! One GPUI global. Components read it at render time through
//! [`ActiveFacet`]; changing it must be followed by `cx.refresh_windows()`
//! because cached views read the palette imperatively.

use crate::measure::{Density, Reveal};
use crate::tokens::{Appearance, Palette, TypeRole};
use gpui::{App, Global, Pixels, px};
use std::sync::LazyLock;

/// Contrast treatment layered over an appearance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Contrast {
    /// The boards' palette.
    #[default]
    Normal,
    /// Every ink one step stronger, lines and bevels firmer, the ground quieter.
    High,
}

static ABYSS_HIGH: LazyLock<Palette> = LazyLock::new(|| strengthen(Appearance::Abyss.palette()));
static GLACIER_HIGH: LazyLock<Palette> =
    LazyLock::new(|| strengthen(Appearance::Glacier.palette()));

fn strengthen(base: &Palette) -> Palette {
    Palette {
        ink1: base.ink0,
        ink2: base.ink1,
        ink3: base.ink2,
        ink4: base.ink3,
        line1: base.line1.alpha(1.8),
        line2: base.line2.alpha(1.8),
        line3: base.line3.alpha(1.6),
        bevel_hi: base.bevel_hi.alpha(1.6),
        bevel_lo: base.bevel_lo.alpha(1.3),
        ground: base.ground.alpha(0.5),
        ..*base
    }
}

/// The window-independent visual settings every component reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Facet {
    /// Abyss or Glacier.
    pub appearance: Appearance,
    /// Text scale, 1.0 = 100 %. Clamped to 0.85..=2.0.
    pub text_scale: f32,
    /// Reduced motion: springs snap, ambient motion stops.
    pub reduced_motion: bool,
    /// Normal or high contrast.
    pub contrast: Contrast,
    /// Comfortable, compact or dense.
    pub density: Density,
    /// Held reveal modes (⌘ keys, ⌥ x-ray).
    pub reveal: Reveal,
}

impl Default for Facet {
    fn default() -> Self {
        Self {
            appearance: Appearance::Abyss,
            text_scale: 1.0,
            reduced_motion: false,
            contrast: Contrast::Normal,
            density: Density::Comfortable,
            reveal: Reveal::default(),
        }
    }
}

impl Global for Facet {}

impl Facet {
    /// The active palette (contrast applied).
    #[must_use]
    pub fn palette(&self) -> &'static Palette {
        match (self.appearance, self.contrast) {
            (appearance, Contrast::Normal) => appearance.palette(),
            (Appearance::Abyss, Contrast::High) => &ABYSS_HIGH,
            (Appearance::Glacier, Contrast::High) => &GLACIER_HIGH,
        }
    }

    /// A measure for a container `width` wide under this theme.
    #[must_use]
    pub fn measure(&self, width: Pixels) -> crate::measure::Measure {
        crate::measure::Measure::new(width, self)
    }

    /// A token size scaled by the text scale.
    #[must_use]
    pub fn px(&self, value: f32) -> Pixels {
        px(value * self.text_scale)
    }

    /// A type role's size at the current text scale.
    #[must_use]
    pub fn size(&self, role: TypeRole) -> Pixels {
        self.px(role.size)
    }

    /// A type role's line height at the current text scale.
    #[must_use]
    pub fn line(&self, role: TypeRole) -> Pixels {
        self.px(role.line)
    }
}

/// Read access to the active [`Facet`] from any context that derefs to `App`.
pub trait ActiveFacet {
    /// The active theme (the default if none was installed).
    fn facet(&self) -> Facet;
    /// The active palette.
    fn palette(&self) -> &'static Palette {
        self.facet().palette()
    }
}

impl ActiveFacet for App {
    fn facet(&self) -> Facet {
        self.try_global::<Facet>().copied().unwrap_or_default()
    }
}

/// Installs or replaces the active theme and repaints every window.
pub fn set_facet(facet: Facet, cx: &mut App) {
    let facet = Facet {
        text_scale: facet.text_scale.clamp(0.85, 2.0),
        ..facet
    };
    if cx.try_global::<Facet>() == Some(&facet) {
        return;
    }
    cx.set_global(facet);
    cx.refresh_windows();
}
