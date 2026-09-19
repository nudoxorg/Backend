//! Text primitives: the type ladder, the specimen face, and navigable text.
//! Every string drawn in this application passes through one of these.
//! None of them takes a colour; they take a role and read the lit palette.
//!
//! Navigable text is the one place a reader needs to distinguish "this is a
//! word" from "this is a door". It is set in the same vellum as body text, so
//! a page is not a field of blue, and carries a hairline underline that goes
//! to full strength under the pointer. The hue that says *what kind of thing*
//! is behind the door lives in the glyph beside it, never in the word itself.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{TypeScale, line_height, type_size};
use gpui::{Div, FontWeight, Hsla, SharedString, Styled, div};

/// Returns a text block at one rung of the type ladder.
pub(crate) fn text_at(theme: &Theme, scale: TypeScale, role: Paint) -> Div {
    div()
        .text_size(type_size(scale))
        .line_height(line_height(scale))
        .text_color(theme.paint(role))
}

/// Returns a heading block, set in the strongest ink at a semibold weight.
pub(crate) fn heading(theme: &Theme, scale: TypeScale) -> Div {
    text_at(theme, scale, Paint::TextStrong).font_weight(FontWeight::SEMIBOLD)
}

/// Returns a body text block.
pub(crate) fn body(theme: &Theme) -> Div {
    text_at(theme, TypeScale::Body, Paint::Text)
}

/// Returns an interface-sized text block.
pub(crate) fn label(theme: &Theme) -> Div {
    text_at(theme, TypeScale::Interface, Paint::Text)
}

/// Returns a secondary text block.
pub(crate) fn dim(theme: &Theme) -> Div {
    text_at(theme, TypeScale::Small, Paint::TextDim)
}

/// Returns a tertiary text block, for counts and hints.
pub(crate) fn faint(theme: &Theme) -> Div {
    text_at(theme, TypeScale::Tiny, Paint::TextFaint)
}

/// Returns text the reader can follow to another declaration.
///
/// The underline is drawn at a fraction of the text's own colour so the page
/// reads as prose rather than as a list of links; `hovered` takes it to full
/// strength, which is the only state change a link ever has.
pub(crate) fn navigable(theme: &Theme, scale: TypeScale, hovered: bool) -> Div {
    let ink = theme.paint(if hovered {
        Paint::TextStrong
    } else {
        Paint::Text
    });
    text_at(theme, scale, Paint::Text)
        .text_color(ink)
        .text_decoration_1()
        .text_decoration_color(underline_ink(theme, hovered))
        .cursor_pointer()
}

/// Returns the colour a navigable underline is drawn in.
pub(crate) fn underline_ink(theme: &Theme, hovered: bool) -> Hsla {
    let mut ink = theme.paint(Paint::GiltDim);
    ink.a = if hovered { 0.95 } else { 0.42 };
    ink
}

/// Returns the one text builder allowed to use the gilt accent.
///
/// Gilt marks identity the reader can copy — a crumb, a key tag, the palette
/// nib — and nothing else. Keeping it to one builder is what keeps the accent
/// under a few percent of the pixels on a page.
pub(crate) fn identity_text(theme: &Theme, scale: TypeScale) -> Div {
    text_at(theme, scale, Paint::Gilt).font_weight(FontWeight::MEDIUM)
}

/// Returns a single-line block that truncates with a trailing ellipsis.
pub(crate) fn single_line(block: Div) -> Div {
    block.whitespace_nowrap().overflow_hidden().text_ellipsis()
}

/// Shortens a path for display, eliding its middle and never its ends.
pub(crate) fn elide(text: &str, budget: usize) -> SharedString {
    SharedString::from(crate::presentation::crumb::elide_middle(text, budget))
}
