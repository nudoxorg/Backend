//! The application theme: one generated palette plus the geometry tokens.
//!
//! The theme is a GPUI global rather than a field threaded through views, so a
//! stateless element builder can ask for a colour without being handed one.
//! No view in this application may write a colour literal; every tone comes
//! from [`Theme::paint`] or from a hue placed on the chromatic plane.

pub(crate) mod kind;
pub(crate) mod language;
pub(crate) mod palette;
pub(crate) mod ramp;
pub(crate) mod tokens;

use gpui::{App, Global, Hsla, Pixels, SharedString};
use palette::{Appearance, Paint, Palette};
use ramp::Hue;
use tokens::InterfaceSize;

/// The lit palette and the reading preferences that scale it.
#[derive(Clone, Debug)]
pub(crate) struct Theme {
    palette: Palette,
    interface: InterfaceSize,
    reduced_motion: bool,
    specimen: SharedString,
}

impl Global for Theme {}

impl Theme {
    /// Lights one appearance at one interface size.
    pub(crate) fn new(appearance: Appearance, interface: InterfaceSize, reduced_motion: bool) -> Self {
        Self {
            palette: Palette::new(appearance),
            interface,
            reduced_motion,
            specimen: SharedString::new_static("SF Mono"),
        }
    }

    /// Returns the colour for one semantic role.
    pub(crate) fn paint(&self, role: Paint) -> Hsla {
        self.palette.paint(role)
    }

    /// Returns a hue rendered on the single chromatic plane.
    pub(crate) fn on_plane(&self, hue: Hue) -> Hsla {
        self.palette.on_plane(hue)
    }

    /// Returns a translucent wash of one hue on the chromatic plane.
    pub(crate) fn plane_wash(&self, hue: Hue, alpha: f32) -> Hsla {
        self.palette.plane_wash(hue, alpha)
    }

    /// Returns the root font size implied by the reading size.
    pub(crate) fn root_pixels(&self) -> Pixels {
        self.interface.root_pixels()
    }

    /// Returns whether motion is suppressed.
    pub(crate) const fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    /// Returns the monospace family used for signatures and source.
    pub(crate) fn specimen(&self) -> SharedString {
        self.specimen.clone()
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(Appearance::Ink, InterfaceSize::DEFAULT, false)
    }
}

/// Returns the lit theme, installing the default one if none is set.
pub(crate) fn theme(cx: &App) -> Theme {
    cx.try_global::<Theme>().cloned().unwrap_or_default()
}
