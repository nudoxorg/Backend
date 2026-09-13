//! Glyph tiles: declaration kinds, source languages, and readiness marks.
//! A tile is a small rounded square holding one letter on the chromatic plane.
//! Its hue is the only colour a dense list carries, and it means exactly one thing.
//!
//! Letters rather than pictograms, because eighteen distinguishable pictograms
//! do not exist at fourteen pixels, while eighteen letters at eighteen hues on
//! one luminance plane are read preattentively and still say something true in
//! greyscale. The tile is washed rather than filled so a column of them reads
//! as texture, and the letter carries the contrast.

use crate::presentation::shelf::Readiness;
use crate::theme::kind::{KindGlyph, kind_glyph, package_glyph, untyped_glyph};
use crate::theme::language::Language;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, TypeScale, radius, type_size};
use crate::theme::{Theme, ramp::Hue};
use backend_library::DeclarationKind;
use gpui::{Div, FontWeight, ParentElement, Styled, div, px};

/// Side length of a glyph tile, in pixels.
const TILE: f32 = 16.0;

/// Side length of the larger tile used in page headers.
const TILE_LARGE: f32 = 22.0;

/// Returns the tile for one declaration kind.
pub(crate) fn kind_tile(theme: &Theme, kind: Option<DeclarationKind>, large: bool) -> Div {
    let glyph = kind.map_or_else(untyped_glyph, kind_glyph);
    tile(theme, glyph.hue(), &glyph.letter().to_string(), large)
}

/// Returns the tile for a package or project row.
pub(crate) fn package_tile(theme: &Theme, large: bool) -> Div {
    let glyph = package_glyph();
    tile(theme, glyph.hue(), &glyph.letter().to_string(), large)
}

/// Returns the two-letter tag for one source language.
pub(crate) fn language_tag(theme: &Theme, language: Language) -> Div {
    tile(theme, language.hue(), language.tag(), false)
}

/// Returns the readable name of one declaration kind.
pub(crate) fn kind_label(kind: Option<DeclarationKind>) -> &'static str {
    kind.map_or_else(|| untyped_glyph().label(), |kind| kind_glyph(kind).label())
}

/// Returns the mark for one kind, for tooltips and group headers.
pub(crate) fn glyph_for(kind: Option<DeclarationKind>) -> KindGlyph {
    kind.map_or_else(untyped_glyph, kind_glyph)
}

fn tile(theme: &Theme, hue: Hue, letter: &str, large: bool) -> Div {
    let side = if large { TILE_LARGE } else { TILE };
    let scale = if large {
        TypeScale::Small
    } else {
        TypeScale::Micro
    };
    div()
        .flex_none()
        .w(px(side))
        .h(px(side))
        .rounded(radius(Radius::Hair))
        .bg(theme.plane_wash(hue, 0.16))
        .flex()
        .items_center()
        .justify_center()
        .text_size(type_size(scale))
        .font_weight(FontWeight::SEMIBOLD)
        .font_family(theme.specimen())
        .text_color(theme.on_plane(hue))
        .child(letter.to_owned())
}

/// Returns the readiness mark drawn on a shelf row.
pub(crate) fn readiness_mark(theme: &Theme, readiness: &Readiness) -> Div {
    let role = match readiness {
        Readiness::Ready => Paint::Ok,
        Readiness::Indexing { .. } => Paint::Caution,
        Readiness::Failed(_) => Paint::Fault,
        Readiness::Requested => Paint::TextFaint,
    };
    div()
        .flex_none()
        .w(px(TILE))
        .flex()
        .items_center()
        .justify_center()
        .text_size(type_size(TypeScale::Small))
        .text_color(theme.paint(role))
        .child(readiness.glyph().to_string())
}

/// Returns a hue-coded bar showing one project's language mix.
///
/// A bar rather than a sentence: the mix is a proportion, and a proportion is
/// a length. The bar is the width of the row, so two projects can be compared
/// at a glance without reading a single number.
pub(crate) fn language_bar(
    theme: &Theme,
    counts: &[crate::presentation::shelf::LanguageCount],
    total: usize,
) -> Div {
    let total = total.max(1);
    div()
        .h(px(3.0))
        .w_full()
        .flex()
        .gap(px(1.0))
        .children(counts.iter().map(|count| {
            let share = ratio(count.declarations(), total);
            div()
                .h_full()
                .rounded_full()
                .bg(theme.on_plane(count.language().hue()))
                .flex_basis(px(0.0))
                .flex_grow(share)
                .flex_shrink(1.0)
        }))
}

fn ratio(part: usize, total: usize) -> f32 {
    let part = u32::try_from(part).unwrap_or(u32::MAX);
    let total = u32::try_from(total).unwrap_or(u32::MAX).max(1);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a declaration count above sixteen million cannot change a bar's width"
    )]
    let share = part as f32 / total as f32;
    share.clamp(0.0, 1.0)
}
