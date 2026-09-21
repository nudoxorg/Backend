//! Text primitives: the type ladder, the specimen face, and navigable text.
//! Every string drawn in this application passes through one of these.
//! None of them takes a colour; they take a role and read the lit palette.
//!
//! Navigable text is the one place a reader needs to distinguish "this is a
//! word" from "this is a door". It is set in the same silver as body text, so
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
        .font_family(theme.ui_face())
        .text_size(type_size(scale))
        .line_height(line_height(scale))
        .text_color(theme.paint(role))
}

/// Returns a heading block, set in the strongest ink at a semibold weight.
pub(crate) fn heading(theme: &Theme, scale: TypeScale) -> Div {
    text_at(theme, scale, Paint::Silver0)
        .font_family(theme.display_face())
        .font_weight(FontWeight::SEMIBOLD)
}

/// Returns a display heading in the bundled Archivo face. The hero rung is
/// reserved for the design-system showcase and first-run identity surface.
pub(crate) fn display(theme: &Theme, scale: TypeScale) -> Div {
    text_at(theme, scale, Paint::Silver0)
        .font_family(theme.display_face())
        .font_weight(FontWeight::SEMIBOLD)
}

/// Returns a body text block.
pub(crate) fn body(theme: &Theme) -> Div {
    text_at(theme, TypeScale::Body, Paint::Silver1)
}

/// Returns an interface-sized text block.
pub(crate) fn label(theme: &Theme) -> Div {
    text_at(theme, TypeScale::Interface, Paint::Silver1)
}

/// Returns a secondary text block.
pub(crate) fn dim(theme: &Theme) -> Div {
    text_at(theme, TypeScale::Small, Paint::Silver2)
}

/// Returns a tertiary text block, for counts and hints.
pub(crate) fn faint(theme: &Theme) -> Div {
    text_at(theme, TypeScale::Tiny, Paint::Silver3)
}

/// Returns text the reader can follow to another declaration.
///
/// The underline is drawn at a fraction of the text's own colour so the page
/// reads as prose rather than as a list of links; `hovered` takes it to full
/// strength, which is the only state change a link ever has.
pub(crate) fn navigable(theme: &Theme, scale: TypeScale, hovered: bool) -> Div {
    let ink = theme.paint(if hovered {
        Paint::Silver0
    } else {
        Paint::Silver1
    });
    text_at(theme, scale, Paint::Silver1)
        .text_color(ink)
        .text_decoration_1()
        .text_decoration_color(underline_ink(theme, hovered))
        .cursor_pointer()
}

/// Returns the colour a navigable underline is drawn in.
pub(crate) fn underline_ink(theme: &Theme, hovered: bool) -> Hsla {
    let mut ink = theme.paint(Paint::Leaf);
    ink.alpha = if hovered { 0.95 } else { 0.42 };
    ink
}

/// Returns the one text builder allowed to use the mint identity accent.
///
/// Mint marks identity the reader can copy — a crumb, a key tag, the palette
/// nib — and nothing else. Keeping it to one builder is what keeps the accent
/// under a few percent of the pixels on a page.
pub(crate) fn identity_text(theme: &Theme, scale: TypeScale) -> Div {
    text_at(theme, scale, Paint::Mint).font_weight(FontWeight::MEDIUM)
}

/// Returns a single-line block that truncates with a trailing ellipsis.
pub(crate) fn single_line(block: Div) -> Div {
    block.whitespace_nowrap().overflow_hidden().text_ellipsis()
}

/// Shortens a path for display, eliding its middle and never its ends.
pub(crate) fn elide(text: &str, budget: usize) -> SharedString {
    SharedString::from(crate::presentation::crumb::elide_middle(text, budget))
}

/// Returns one line of text with the matched spans emphasised.
///
/// The emphasis is a weight and an ink step, never a background: a result
/// list that boxes every match reads as a form, while one that strengthens
/// the matched letters reads as an answer.
pub(crate) fn highlighted(
    theme: &Theme,
    text: &str,
    needle: &str,
    scale: TypeScale,
) -> gpui::StyledText {
    let ranges = match_ranges(text, needle);
    let style = gpui::HighlightStyle {
        color: Some(theme.paint(Paint::Silver0)),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..gpui::HighlightStyle::default()
    };
    let _ = scale;
    gpui::StyledText::new(SharedString::from(text.to_owned()))
        .with_highlights(ranges.into_iter().map(|range| (range, style)))
}

/// Returns the byte ranges of every case-insensitive occurrence of each word.
///
/// Matching is projected back onto the original string. Rust's Unicode lower
/// casing can expand one scalar into several scalars (`İ`, for example), so a
/// lower-cased byte offset cannot be used as a `StyledText` range directly.
/// The boundary map keeps every returned range valid UTF-8 while preserving
/// the useful behaviour of highlighting the complete source scalar.
pub(crate) fn match_ranges(text: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    let (lower, starts, ends) = lower_with_boundaries(text);
    let mut ranges = Vec::new();
    for word in needle.split_whitespace() {
        let word = word.to_lowercase();
        if word.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(at) = lower.get(from..).and_then(|rest| rest.find(&word)) {
            let folded_start = from.saturating_add(at);
            let folded_end = folded_start.saturating_add(word.len());
            let Some(&start) = starts.get(folded_start) else {
                break;
            };
            let Some(&end) = ends.get(folded_end) else {
                break;
            };
            ranges.push(start..end);
            from = folded_end;
        }
    }
    ranges.sort_by_key(|range| range.start);
    ranges.dedup();
    ranges
}

fn lower_with_boundaries(text: &str) -> (String, Vec<usize>, Vec<usize>) {
    let mut lower = String::with_capacity(text.len());
    let mut starts = vec![0];
    let mut ends = vec![0];
    for (start, scalar) in text.char_indices() {
        let end = start + scalar.len_utf8();
        let folded = scalar.to_lowercase().collect::<String>();
        lower.push_str(&folded);
        for _ in 1..folded.len() {
            starts.push(start);
            ends.push(end);
        }
        starts.push(end);
        ends.push(end);
    }
    (lower, starts, ends)
}

#[cfg(test)]
mod tests {
    use super::match_ranges;

    #[test]
    fn match_ranges_keeps_expanded_unicode_ranges_on_boundaries() {
        assert_eq!(match_ranges("İstanbul", "i"), vec![0..2]);
        assert_eq!(match_ranges("Δelta δELTA", "δelta"), vec![0..6, 7..13]);
    }

    #[test]
    fn match_ranges_handles_long_rtl_text_without_invalid_offsets() {
        let text = "שלום العالم ".repeat(2_048);
        let ranges = match_ranges(&text, "العالم");
        assert_eq!(ranges.len(), 2_048);
        assert!(ranges
            .iter()
            .all(|range| text.is_char_boundary(range.start) && text.is_char_boundary(range.end)));
    }
}
