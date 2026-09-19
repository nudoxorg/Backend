//! Glyph tiles: declaration kinds, source languages, and readiness marks.
//! A tile is a small rounded square holding one drawn mark on the chromatic plane.
//! Its hue is the only colour a dense list carries, and it means exactly one thing.
//!
//! Marks rather than letters. Nineteen distinguishable letters at fourteen
//! pixels collide — `C` for class beside `c` for the C language was the
//! complaint that retired them — while nineteen small shapes on nineteen hues
//! at one luminance are read preattentively and still say something true in
//! greyscale. The tile is washed rather than filled so a column of them reads
//! as texture, and the mark carries the contrast. A language is drawn as its
//! logo, never as two letters, for the same reason.
//!
//! One glyph deliberately differs from the shared model's. `backend packages`
//! prints `●` for a ready project because a terminal cannot draw weight; this
//! window draws `✓`, which is what the same state looks like beside a `◐` and
//! an `✗` in colour. The *state* is [`backend_present::Readiness`] in both
//! places; only the mark is a drawing decision.

use crate::presentation::project::Standing;
use crate::theme::kind::{kind_glyph, mark_path, package_glyph, untyped_glyph};
use crate::theme::language::hue as language_hue;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, TypeScale, radius, type_size};
use crate::theme::{Theme, ramp::Hue};
use crate::ui::icon::{self, Logo};
use backend_library::DeclarationKind;
use backend_present::Language;
use gpui::{Div, ParentElement, Styled, div, px, svg};

/// Side length of a glyph tile, in pixels.
const TILE: f32 = 16.0;

/// Side length of the larger tile used in page headers.
const TILE_LARGE: f32 = 24.0;

/// Size of the mark inside a tile, in pixels.
const MARK: f32 = 11.0;

/// Size of the mark inside a large tile, in pixels.
const MARK_LARGE: f32 = 16.0;

/// Returns the tile for one declaration kind.
pub(crate) fn kind_tile(theme: &Theme, kind: Option<DeclarationKind>, large: bool) -> Div {
    let glyph = kind.map_or_else(untyped_glyph, kind_glyph);
    tile(theme, glyph.hue(), large).child(mark(theme, mark_path(kind), glyph.hue(), large))
}

/// Returns the bare mark for one kind, at an explicit size and in its hue.
///
/// For places too dense for a tile: a section head, a tree row, a chip.
pub(crate) fn kind_mark(theme: &Theme, kind: Option<DeclarationKind>, side: f32) -> gpui::Svg {
    let glyph = kind.map_or_else(untyped_glyph, kind_glyph);
    svg()
        .path(mark_path(kind))
        .w(px(side))
        .h(px(side))
        .flex_none()
        .text_color(theme.on_plane(glyph.hue()))
}

/// Returns the tile for a package or project row.
pub(crate) fn package_tile(theme: &Theme, large: bool) -> Div {
    let glyph = package_glyph();
    tile(theme, glyph.hue(), large).child(mark(theme, icon::PACKAGE_PATH, glyph.hue(), large))
}

/// Returns the tile for one source language: its logo in its hue.
pub(crate) fn language_tag(theme: &Theme, language: Language) -> Div {
    let hue = language_hue(language);
    let ink = theme.on_plane(hue);
    let shell = tile(theme, hue, false);
    match Logo::of(language) {
        Some(logo) => shell.child(icon::logo(logo, MARK, ink)),
        None => shell.child(
            div()
                .text_size(type_size(TypeScale::Micro))
                .text_color(ink)
                .child("··"),
        ),
    }
}

/// Returns the readable name of one declaration kind.
pub(crate) fn kind_label(kind: Option<DeclarationKind>) -> &'static str {
    kind.map_or_else(|| untyped_glyph().label(), |kind| kind_glyph(kind).label())
}

fn tile(theme: &Theme, hue: Hue, large: bool) -> Div {
    let side = if large { TILE_LARGE } else { TILE };
    div()
        .flex_none()
        .w(px(side))
        .h(px(side))
        .rounded(radius(if large { Radius::Small } else { Radius::Hair }))
        .bg(theme.plane_wash(hue, 0.16))
        .flex()
        .items_center()
        .justify_center()
}

fn mark(theme: &Theme, path: &'static str, hue: Hue, large: bool) -> gpui::Svg {
    let side = if large { MARK_LARGE } else { MARK };
    svg()
        .path(path)
        .w(px(side))
        .h(px(side))
        .flex_none()
        .text_color(theme.on_plane(hue))
}

/// Returns the mark this window draws for one standing.
pub(crate) const fn standing_glyph(standing: Standing) -> &'static str {
    match standing {
        Standing::Readable => "✓",
        Standing::Indexing => "◐",
        Standing::Failed => "✗",
        Standing::Requested | Standing::Empty => "○",
    }
}

/// Returns the paint role one standing is drawn in.
pub(crate) const fn standing_paint(standing: Standing) -> Paint {
    match standing {
        Standing::Readable => Paint::Ok,
        Standing::Indexing => Paint::Caution,
        Standing::Failed => Paint::Fault,
        Standing::Requested | Standing::Empty => Paint::Info,
    }
}

/// Returns the standing mark drawn on a shelf row.
pub(crate) fn standing_mark(theme: &Theme, standing: Standing) -> Div {
    div()
        .flex_none()
        .w(px(TILE))
        .flex()
        .items_center()
        .justify_center()
        .text_size(type_size(TypeScale::Small))
        .text_color(theme.paint(standing_paint(standing)))
        .child(standing_glyph(standing))
}
