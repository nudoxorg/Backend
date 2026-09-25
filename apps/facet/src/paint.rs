//! Paint primitives: the cut plate and its bevel states, the hatch, the gem,
//! and the faceted ground. (Lane F2.)
//!
//! All four are flat, hard-edged, two-tone: no gradients, no blur except the
//! floating plate's shadow, no rounded blobs. Geometry is convex polygons
//! ([`geom`]) emitted as triangle fans; the renderer's MSAA antialiases the
//! straight edges. Animated inputs (`phase`, `lift`, blended bevel colours)
//! are plain numbers: the caller's motion engine owns the clock.

pub mod cut;
pub mod gem;
pub mod geom;
pub mod ground;
pub mod hatch;

#[cfg(feature = "gallery")]
pub(crate) mod gallery;
#[cfg(all(test, feature = "gallery"))]
mod headless;

pub use cut::{Bevel, Chamfer, Cut, CutPaint, Edge, Plate, cut, paint_cut};
pub use gem::{Gem, GemState, gem};
pub use ground::{Ground, ground};
pub use hatch::{Hatch, HatchFill, hatch_fill};

use gpui::{Hsla, Rgba, hsla_to_rgba, rgb_to_hsla};

/// A blend from `a` to `b` in premultiplied sRGB (`t` in `0..=1`), so a
/// fade to or from transparent never darkens. Hue-safe, unlike lerping HSL.
#[must_use]
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (hsla_to_rgba(a), hsla_to_rgba(b));
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    let alpha = lerp(a.alpha, b.alpha);
    if alpha <= 1e-6 {
        return clear();
    }
    let channel = |x: f32, y: f32| (lerp(x * a.alpha, y * b.alpha) / alpha).clamp(0.0, 1.0);
    rgb_to_hsla(Rgba::new(
        channel(a.red, b.red),
        channel(a.green, b.green),
        channel(a.blue, b.blue),
        alpha.clamp(0.0, 1.0),
    ))
}

/// Fully transparent.
#[must_use]
pub fn clear() -> Hsla {
    gpui::transparent_black()
}
