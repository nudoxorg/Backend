//! The small states the folio's components are told, as types: nothing in
//! the page's public surface is a bare `bool` or a bare `f32` of pixels.

use crate::controls::state::Touch;
use crate::motion::{Spec, spec};
use crate::overlay::float::QUICK_REST;
use gpui::{App, ElementId, Pixels, Window, px};

/// The shared open state for a disclosure card. Children remain in ordinary
/// GPUI flow, so their measured height moves every following sibling as the
/// card opens, resizes, or responds to a text-scale change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DisclosureFlow {
    /// Animated progress, including the plate's small entrance overshoot.
    pub(crate) progress: f32,
}

/// Whether a disclosure's complete words may be painted. This is a semantic
/// cut, separate from the spring that changes the plate's geometry: copy
/// never reflows through progressively narrower widths on the way out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InkPhase {
    Retained,
    Retired,
}

/// A disclosure whose words require the full measured plate width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DisclosureCopy {
    /// Physical expansion of the card and its children.
    pub(crate) geometry: f32,
    /// Whether the complete words are still on the card.
    pub(crate) ink: InkPhase,
}

impl DisclosureCopy {
    /// Retire whole words before closing the plate. On the way back, expand
    /// the plate before restoring them. Separate keyed tracks preserve the
    /// physical spring's value and velocity through a rapid reversal.
    #[must_use]
    pub(crate) fn read(id: &ElementId, touch: &Touch, pose: Pose, window: &mut Window, cx: &mut App) -> Self {
        const INK_END: f32 = 0.03;
        const FULL_WIDTH: f32 = 0.999;
        let wanted = touch.hovered || touch.focused || pose == Pose::Held;
        let ink_progress = touch.motion.animate(
            crate::controls::state::track(id, "copy"),
            if wanted { 1.0 } else { 0.0 },
            plate(wanted),
            window,
            cx,
        );
        let geometry = touch.motion.animate(
            crate::controls::state::track(id, "open"),
            if ink_progress > INK_END { 1.0 } else { 0.0 },
            spec::FOLLOW,
            window,
            cx,
        ).clamp(0.0, 1.05);
        let ink = if ink_progress > INK_END && geometry >= FULL_WIDTH {
            InkPhase::Retained
        } else {
            InkPhase::Retired
        };
        Self { geometry, ink }
    }
}

impl DisclosureFlow {
    /// Preserve the design minimum at its 100% reference size while letting
    /// larger or wrapped content determine the card's actual height.
    #[must_use]
    pub(crate) fn rest_height(minimum: Nominal) -> Pixels {
        minimum.at(1.0)
    }

    /// Read the common hover/focus/held state and animate its open track.
    #[must_use]
    pub(crate) fn read(
        id: &ElementId,
        touch: &Touch,
        pose: Pose,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let wanted = touch.hovered || touch.focused || pose == Pose::Held;
        let progress = touch
            .motion
            .animate(
                crate::controls::state::track(id, "open"),
                if wanted { 1.0 } else { 0.0 },
                plate(wanted),
                window,
                cx,
            )
            .clamp(0.0, 1.05);
        Self { progress }
    }
}

/// How a plate that answers a pointer's *rest* opens or shuts: it opens after
/// the house rest ([`QUICK_REST`]), so a pointer only passing over it opens
/// nothing, and it shuts at once when the pointer goes.
#[must_use]
pub fn plate(wanted: bool) -> Spec {
    if wanted { spec::LIFT.delayed(QUICK_REST) } else { spec::LEAVE }
}

/// Whether your project reaches a name (the mint mark on a shingle or card).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Use {
    /// Your code names it.
    Yours,
    /// It is only there.
    #[default]
    Elsewhere,
}

impl Use {
    /// Whether it is yours.
    #[must_use]
    pub const fn is_yours(self) -> bool {
        matches!(self, Self::Yours)
    }
}

/// Which release a view is of.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Time {
    /// The release you pin.
    #[default]
    Now,
    /// Another release: everything is drawn a little warmer.
    Past,
}

/// How a module is shown.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Extent {
    /// Its cards open in place.
    #[default]
    Inline,
    /// It has too many names for that: it is a page of its own.
    Page,
}

/// The weight berg.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Fold {
    /// A glyph in the crest.
    #[default]
    Folded,
    /// The whole berg is out.
    Open,
}

/// Whether a component is shown in the pose a pointer would give it (scenes
/// and tests), or left to the pointer.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Pose {
    /// The pointer decides.
    #[default]
    Live,
    /// Held open whatever the pointer does.
    Held,
}

/// Whether a card is picked out (a name asked for, or the one a click left).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Pick {
    /// At rest.
    #[default]
    Rest,
    /// Picked out.
    Lit,
}

/// Whether a release can still be built.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Standing {
    /// It can.
    #[default]
    Available,
    /// It was pulled.
    Yanked,
}

/// Whether the page has read a release's names.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Names {
    /// Only its date and size are known.
    #[default]
    Unread,
    /// Its names have been read.
    Read,
}

/// Whether a crate says it will never use `unsafe`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Unsafe {
    /// It may.
    #[default]
    Allowed,
    /// `#![forbid(unsafe_code)]`.
    Forbidden,
}

/// Whether building a crate runs its own code.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Build {
    /// It does not.
    #[default]
    Plain,
    /// A build script runs.
    Script,
}

/// What a crate is.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Library {
    /// An ordinary library.
    #[default]
    Plain,
    /// It runs inside the compiler.
    ProcMacro,
}

/// Whether a licence option is the one judged against your project.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Fit {
    /// This is the option that was judged.
    Judged,
    /// A quieter alternative.
    Aside,
}

/// A length in pixels at 100 % text: what a design number is before the
/// reader's text size scales it.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Nominal(f32);

impl Nominal {
    /// `value` px at 100 % text.
    #[must_use]
    pub const fn px(value: f32) -> Self {
        Self(value)
    }

    /// The length at text scale `scale`.
    #[must_use]
    pub fn at(self, scale: f32) -> Pixels {
        px(self.0 * scale)
    }

    /// The bare number, px at 100 % text.
    #[must_use]
    pub const fn value(self) -> f32 {
        self.0
    }
}
