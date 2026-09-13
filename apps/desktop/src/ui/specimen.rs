//! Signatures and source, set as one shaped text run with typed colour.
//! Type names inside a signature are links; everything else is coloured text.
//! One text element, not a row of word-shaped boxes, so the monospace grid holds.
//!
//! GPUI has no inline-flow element, so rich text is built the other way round:
//! one string, a list of byte ranges with highlight styles, and a parallel list
//! of clickable ranges. That is exactly what a tokenized signature already is,
//! and it keeps ligatures, wrapping, and caret geometry correct in a way that
//! a flex row of per-token divs never could.

use crate::presentation::signature::{Resolved, Signature, TokenKind};
use crate::theme::Theme;
use crate::theme::kind::kind_glyph;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, line_height, space, type_size};
use backend_library::{DeclarationKind, SymbolKey};
use gpui::{
    App, Div, ElementId, HighlightStyle, InteractiveText, ParentElement, SharedString, StyledText,
    UnderlineStyle, Window, div, px,
};
use gpui::{FontWeight, Styled};
use std::ops::Range;

/// The pieces an interactive run of text is assembled from.
struct Runs {
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    links: Vec<Range<usize>>,
    targets: Vec<SymbolKey>,
}

/// Returns the signature specimen block, with every resolved type linked.
pub(crate) fn signature_block(
    theme: &Theme,
    signature: &Signature,
    id: impl Into<SharedString>,
    open: impl Fn(SymbolKey, &mut Window, &mut App) + 'static,
) -> Div {
    let runs = signature_runs(theme, signature);
    super::surface::sunken(theme)
        .w_full()
        .px(space(Space::Room))
        .py(space(Space::Base))
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Small))
        .line_height(line_height(TypeScale::Body))
        .text_color(theme.paint(Paint::Text))
        .child(interactive(runs, id, open))
}

/// Returns a one-line signature preview, coloured but not interactive.
pub(crate) fn signature_line(theme: &Theme, signature: &Signature, scale: TypeScale) -> Div {
    let runs = signature_runs(theme, signature);
    div()
        .font_family(theme.specimen())
        .text_size(type_size(scale))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .text_color(theme.paint(Paint::TextDim))
        .child(StyledText::new(runs.text).with_highlights(runs.highlights))
}

fn interactive(
    runs: Runs,
    id: impl Into<SharedString>,
    open: impl Fn(SymbolKey, &mut Window, &mut App) + 'static,
) -> InteractiveText {
    let targets = runs.targets;
    let styled = StyledText::new(runs.text).with_highlights(runs.highlights);
    InteractiveText::new(ElementId::Name(id.into()), styled).on_click(
        runs.links,
        move |index, window, cx| {
            if let Some(symbol) = targets.get(index).copied() {
                open(symbol, window, cx);
            }
        },
    )
}

fn signature_runs(theme: &Theme, signature: &Signature) -> Runs {
    let mut text = String::with_capacity(signature.text().len());
    let mut highlights = Vec::new();
    let mut links = Vec::new();
    let mut targets = Vec::new();
    for token in signature.tokens() {
        let start = text.len();
        text.push_str(token.text());
        let range = start..text.len();
        let linked = matches!(token.resolved(), Resolved::Declaration(_));
        highlights.push((range.clone(), style_for(theme, token.kind(), linked)));
        if let Resolved::Declaration(symbol) = token.resolved() {
            links.push(range);
            targets.push(symbol);
        }
    }
    Runs {
        text: SharedString::from(text),
        highlights,
        links,
        targets,
    }
}

fn style_for(theme: &Theme, kind: TokenKind, linked: bool) -> HighlightStyle {
    let mut style = HighlightStyle {
        color: Some(ink_for(theme, kind)),
        ..HighlightStyle::default()
    };
    if matches!(kind, TokenKind::Name) {
        style.font_weight = Some(FontWeight::SEMIBOLD);
    }
    if linked {
        style.underline = Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(super::text::underline_ink(theme, false)),
            wavy: false,
        });
    }
    style
}

fn ink_for(theme: &Theme, kind: TokenKind) -> gpui::Hsla {
    match kind {
        TokenKind::Keyword => theme.on_plane(kind_glyph(DeclarationKind::Module).hue()),
        TokenKind::Name => theme.paint(Paint::TextStrong),
        TokenKind::Type => theme.on_plane(kind_glyph(DeclarationKind::Struct).hue()),
        TokenKind::Parameter => theme.on_plane(kind_glyph(DeclarationKind::Field).hue()),
        TokenKind::Literal => theme.on_plane(kind_glyph(DeclarationKind::Constant).hue()),
        TokenKind::Marker => theme.on_plane(kind_glyph(DeclarationKind::Macro).hue()),
        TokenKind::Punctuation => theme.paint(Paint::TextFaint),
        TokenKind::Space | TokenKind::Plain => theme.paint(Paint::Text),
    }
}
