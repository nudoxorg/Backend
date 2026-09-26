//! Controls: the same cut plate as everything else, small enough to click.
//! Buttons, segmented controls, fields and selects are cut plates; toggles
//! are diamonds ("no pills, no rounded pucks"); a package's releases are a
//! comb you can scrub; key
//! caps are cut stones. Colour says what a control means; the bevel says
//! what state it is in.
//!
//! Every control is a plain `'static` builder (`RenderOnce`) that takes the
//! [`Measure`](crate::Measure) for the width it actually gets at
//! construction. Its one piece of per-instance memory (hover, press, focus,
//! a drag) lives in `state::Interact`, persisted by element id through
//! `Window::use_keyed_state`; its animated values live in a
//! [`Motion`](crate::Motion) store scoped to that state, so every track
//! publishes to the probe ledger as `<id>-<channel>`.

mod state;
mod sweep;
mod text;

pub mod button;
pub mod comb;
pub mod field;
pub mod glyph;
pub mod icon_button;
pub mod kbd;
pub mod seg;
pub mod splitter;
pub mod toggle;

#[cfg(feature = "gallery")]
pub mod gallery;

pub use button::{Button, Intent, button};
pub use comb::{Release, ReleaseId, ReleaseStep, Step, VersionComb, VersionSelected, step_release, version_comb};
pub use field::{Field, Select, field, select, select_menu_key, sync_text_engine};
pub use glyph::{Glyph, glyph};
pub use icon_button::{IconButton, IconButtonSize, icon_button};
pub use kbd::{Kbd, KbdSize, KbdVoice, KeyRise, kbd, keys};
pub use seg::{Seg, SegStyle, Swatch, density_toggle, seg, theme_toggle};
pub use splitter::{PanelSide, SplitEvent, SplitModel, Splitter, split_width, splitter};
pub use state::Look;
pub use toggle::{Toggle, Tri, check, radio, switch};

use crate::motion::Spring;
use gpui::Hsla;

/// A dragged thumb or seam: nearly critical and quick enough to stay under
/// the pointer (it smooths the pointer's discrete steps; it never trails
/// visibly behind a drag).
pub(crate) const GRIP: Spring = Spring {
    response: 0.09,
    damping: 0.92,
};

/// `color` at exactly `alpha`.
pub(crate) fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    let mut color = color;
    color.alpha = alpha;
    color
}

/// `color`, fully transparent (for fading a tone in or out without darkening).
pub(crate) fn clear(color: Hsla) -> Hsla {
    with_alpha(color, 0.0)
}

/// Desaturated the way the boards' `filter: saturate(.4)` does (disabled).
pub(crate) fn muted(color: Hsla) -> Hsla {
    let mut color = color;
    color.color.saturation *= 0.4;
    color
}
