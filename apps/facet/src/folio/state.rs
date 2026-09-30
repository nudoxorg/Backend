//! The small states the folio's components are told, as types: nothing in
//! the page's public surface is a bare `bool` or a bare `f32` of pixels.

use crate::motion::{Spec, spec};
use crate::overlay::float::QUICK_REST;
use gpui::{Pixels, px};

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
