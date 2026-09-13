//! Documentation prose, with the compiler's own links made clickable.
//! A paragraph is one shaped text run; a link is a byte range inside it.
//! Code examples drop into the same specimen well signatures use.
//!
//! The links here are not scraped out of text. The producer emitted a
//! `Fragment::Link` carrying a stable declaration key, so following one is an
//! exact navigation rather than a guess — which is the whole reason this
//! product's documentation can be hyperlinked end to end.

use crate::presentation::prose::{Block, Span};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, line_height, space, type_size};
use backend_library::SymbolKey;
use gpui::{
    AnyElement, App, Div, ElementId, HighlightStyle, InteractiveText, IntoElement, ParentElement,
    SharedString, Styled, StyledText, UnderlineStyle, Window, div, px,
};
use std::ops::Range;
use std::rc::Rc;

/// Returns the whole prose region for one page.
pub(crate) fn blocks(
    theme: &Theme,
    blocks: &[Block],
    prefix: &str,
    open: impl Fn(SymbolKey, &mut Window, &mut App) + 'static,
) -> Div {
    let open = Rc::new(open);
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .children(
            blocks
                .iter()
                .enumerate()
                .map(|(at, block)| one(theme, block, prefix, at, Rc::clone(&open))),
        )
}

/// Returns a single paragraph of prose, for summaries and hover cards.
pub(crate) fn line(theme: &Theme, text: &str, scale: TypeScale) -> Div {
    div()
        .text_size(type_size(scale))
        .line_height(line_height(scale))
        .text_color(theme.paint(Paint::TextDim))
        .child(text.to_owned())
}

fn one(
    theme: &Theme,
    block: &Block,
    prefix: &str,
    at: usize,
    open: Rc<impl Fn(SymbolKey, &mut Window, &mut App) + 'static>,
) -> AnyElement {
    match block {
        Block::Code(code) => super::surface::sunken(theme)
            .w_full()
            .px(space(Space::Room))
            .py(space(Space::Snug))
            .font_family(theme.specimen())
            .text_size(type_size(TypeScale::Small))
            .line_height(line_height(TypeScale::Body))
            .text_color(theme.paint(Paint::Text))
            .child(code.clone())
            .into_any_element(),
        Block::Paragraph(spans) => paragraph(theme, spans, prefix, at, open).into_any_element(),
    }
}

fn paragraph(
    theme: &Theme,
    spans: &[Span],
    prefix: &str,
    at: usize,
    open: Rc<impl Fn(SymbolKey, &mut Window, &mut App) + 'static>,
) -> Div {
    let mut text = String::new();
    let mut highlights = Vec::new();
    let mut links: Vec<Range<usize>> = Vec::new();
    let mut targets = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(span.text());
        let range = start..text.len();
        if let Some(symbol) = span.link() {
            highlights.push((range.clone(), link_style(theme)));
            links.push(range);
            targets.push(symbol);
        }
    }
    let styled = StyledText::new(SharedString::from(text)).with_highlights(highlights);
    let body = InteractiveText::new(
        ElementId::Name(SharedString::from(format!("{prefix}-prose-{at}"))),
        styled,
    )
    .on_click(links, move |index, window, cx| {
        if let Some(symbol) = targets.get(index).copied() {
            open(symbol, window, cx);
        }
    });
    div()
        .w_full()
        .text_size(type_size(TypeScale::Body))
        .line_height(line_height(TypeScale::Body))
        .text_color(theme.paint(Paint::Text))
        .child(body)
}

fn link_style(theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: Some(theme.paint(Paint::TextStrong)),
        underline: Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(super::text::underline_ink(theme, false)),
            wavy: false,
        }),
        ..HighlightStyle::default()
    }
}
