//! The symbol page's anatomy (gui-plan §8.3): a structured, language-neutral
//! picture of a symbol in place of a code block.
//!
//! One visual grammar for every language, drawn from the page model in
//! [`semantics::page`](crate::semantics):
//!
//! - [`fork`]: one of — a rail with a branch per variant;
//! - [`holds`]: a bracket of fields, noting how many are private;
//! - [`pipe`]: inputs → output, "or fails with" as its own exit, generic
//!   bounds as sentences;
//! - [`contract`]: "you write" (a dashed bracket) and "you get";
//! - [`can`]: capabilities in words, marked derived / written / via;
//! - [`prism`]: the static prism, curves from each row to the gem (one
//!   column in narrow rooms);
//! - [`does`]: members by what they do to it, look-alikes folded;
//! - [`in_use`]: real statements from callers' bodies.
//!
//! Every element takes the [`Measure`](crate::Measure) of the width it
//! gets, sets its text through the measure (density and text scale come
//! from the `Facet` global), and writes mixed text as one `StyledText` with
//! runs (never a div per token). Every named type and member is a link: a
//! click dispatches [`Open`] (the shell routes it), resting on it raises the
//! same peek card as the graph through the float layer. Holding ⌥
//! (`Reveal::xray`) spells each plain-word type's exact source beside it.

pub mod can;
pub mod contract;
pub mod does;
pub mod fork;
#[cfg(feature = "gallery")]
pub(crate) mod gallery;
pub mod holds;
pub mod in_use;
pub mod pipe;
pub mod prism;
pub mod text;

pub use can::{Can, can};
pub use contract::{ContractView, contract};
pub use does::{DoesView, does};
pub use fork::{ForkView, fork};
pub use holds::{HoldsView, holds};
pub use in_use::{InUse, in_use};
pub use pipe::{PipeView, pipe};
pub use prism::{PrismView, ToGraph, prism};
pub use text::{Line, Links, Open};

use crate::measure::{Measure, Set, Space};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{App, Div, ParentElement, Pixels, SharedString, Styled, div, px};

/// The anatomy's type roles, at 100 % text and comfortable density.
pub mod roles {
    use crate::tokens::{Face, TypeRole};

    const fn role(face: Face, weight: f32, size: f32, line: f32, tracking: f32) -> TypeRole {
        TypeRole { face, weight, size, line, tracking, italic: matches!(face, Face::Serif) }
    }

    /// A part's heading (`one of`, `holds, all private`, `can`).
    pub const HEAD: TypeRole = role(Face::Ui, 500.0, 11.5, 16.0, 0.02);
    /// A variant's or field's name.
    pub const NAME: TypeRole = role(Face::Mono, 600.0, 13.5, 20.0, 0.0);
    /// A private field's name.
    pub const NAME_QUIET: TypeRole = role(Face::Mono, 500.0, 13.5, 20.0, 0.0);
    /// A type in plain words.
    pub const TYPE: TypeRole = role(Face::Mono, 400.0, 13.0, 20.0, 0.0);
    /// The one sentence a row may carry.
    pub const SAY: TypeRole = role(Face::Serif, 400.0, 14.0, 20.0, 0.0);
    /// A pipe input's name.
    pub const INPUT: TypeRole = role(Face::Mono, 500.0, 12.5, 18.0, 0.0);
    /// A pipe's output.
    pub const OUTPUT: TypeRole = role(Face::Mono, 500.0, 14.5, 20.0, 0.0);
    /// Quiet UI words (flags, the contract's foot).
    pub const QUIET: TypeRole = role(Face::Ui, 400.0, 12.5, 18.0, 0.0);
    /// A capability.
    pub const CAP: TypeRole = role(Face::Ui, 500.0, 12.5, 18.0, 0.0);
    /// A prism row.
    pub const PRISM_ROW: TypeRole = role(Face::Mono, 500.0, 12.5, 18.0, 0.0);
    /// A section's title (`Does`, `In use`).
    pub const SECTION: TypeRole = role(Face::Display, 620.0, 20.0, 24.0, -0.02);
    /// A Does group's heading.
    pub const GROUP: TypeRole = role(Face::Ui, 500.0, 11.0, 14.0, 0.0);
    /// A member row's name and signature.
    pub const ROW: TypeRole = role(Face::Mono, 500.0, 13.0, 20.0, 0.0);
    /// An In use caption.
    pub const CAPTION: TypeRole = role(Face::Mono, 500.0, 12.5, 18.0, 0.0);
    /// Code in an In use excerpt.
    pub const CODE: TypeRole = role(Face::Mono, 400.0, 12.5, 20.5, 0.0);
    /// A quiet mono note (`serde_json`, `render::text`, `extension-qdrant · response.rs:30`).
    pub const NOTE: TypeRole = role(Face::Mono, 400.0, 11.0, 16.0, 0.0);

    /// The UI face at `base`'s size (plain words inside a mono line).
    #[must_use]
    pub const fn words(base: TypeRole) -> TypeRole {
        TypeRole { face: Face::Ui, weight: 400.0, italic: false, tracking: 0.0, ..base }
    }

    /// `base` in italic (generic variables).
    #[must_use]
    pub const fn italic(base: TypeRole) -> TypeRole {
        TypeRole { italic: true, ..base }
    }
}

/// Below this effective width (px) an anatomy stacks each row's name, type
/// and sentence (the prototype's reader-width query at 560, less the folio's
/// padding).
pub const STACK_BELOW: f32 = 520.0;

/// Whether rows stack in this room.
#[must_use]
pub fn stacked(measure: &Measure) -> bool {
    measure.effective() < STACK_BELOW
}

/// A length that follows text scale and density: `value` px at 100 %.
#[must_use]
pub fn k(measure: &Measure, value: f32) -> Pixels {
    px(value * measure.scale() * measure.density().space())
}

/// A row's vertical padding and minimum height follow the density's row
/// factor.
#[must_use]
pub fn row_pad(measure: &Measure, value: f32) -> Pixels {
    px(value * measure.scale() * measure.density().row())
}

/// A part's heading: quiet UI words above the part.
#[must_use]
pub fn heading(text: impl Into<SharedString>, measure: &Measure, palette: &Palette) -> Div {
    div()
        .set(roles::HEAD, measure)
        .text_color(palette.ink4.hsla())
        .mb(k(measure, 6.0))
        .child(text.into())
}

/// A section's title in the display face (`Does`, `In use`).
#[must_use]
pub fn section_title(text: &'static str, measure: &Measure, palette: &Palette) -> Div {
    div()
        .set(roles::SECTION, measure)
        .text_color(palette.ink0.hsla())
        .mb(measure.space(Space::Snug))
        .child(text)
}

/// The active palette.
#[must_use]
pub fn palette(cx: &App) -> &'static Palette {
    cx.facet().palette()
}

/// Whether a role's face is the display face (for tests of the calm rules).
#[must_use]
pub const fn is_serif(role: TypeRole) -> bool {
    matches!(role.face, Face::Serif)
}

/// The kind mark for a world kind.
#[must_use]
pub const fn icon_kind(kind: crate::graph::Kind) -> crate::icons::Kind {
    use crate::graph::Kind as W;
    use crate::icons::Kind as I;
    match kind {
        W::Struct => I::Struct,
        W::Enum => I::Enum,
        W::Union => I::Union,
        W::Trait => I::Trait,
        W::Type => I::Type,
        W::Function => I::Function,
        W::Method => I::Method,
        W::Macro => I::Macro,
        W::Constant => I::Constant,
        W::Field => I::Field,
        W::Variant => I::Variant,
        W::Other => I::Unknown,
    }
}
