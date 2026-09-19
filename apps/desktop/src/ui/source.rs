//! Source text, highlighted and linked: every identifier the shelf knows is a door.
//! A view is built once per page from the captured excerpt and rendered many times.
//! Nothing is parsed twice: the signature lexer colours the lines, the index links them.
//!
//! The excerpt the producer retained is already the declaration's own text,
//! numbered from its capture site. This module runs each line through the
//! same tokenizer the signature specimen uses — one lexer, one colour table,
//! so a type name is the same hue in a signature and in the body below it —
//! and then asks the project index whether each identifier is a declaration
//! on the shelf. The ones that are become clickable, underlined with the
//! same dotted rule a signature link carries, because a name-match is an
//! honest offer to look, not a proven edge.
//!
//! Building is separate from rendering on purpose. Resolving every word of a
//! forty-line excerpt against the index is a few hundred hash lookups, cheap
//! once and wasteful on every frame; the [`SourceView`] is the cached result
//! and the render is a straight walk over it.

use super::specimen::style_for;
use super::text;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, line_height, space, type_size};
use backend_library::SymbolKey;
use backend_present::{Language, Signature, SourceLine, TokenKind, Truncation};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Div, ElementId, HighlightStyle, InteractiveText, ParentElement,
    SharedString, Styled, StyledText, TextAlign, Window, div, px,
};
use std::ops::Range;
use std::rc::Rc;

/// Width of the line-number gutter, in pixels.
const GUTTER: f32 = 44.0;

/// One highlighted line, ready to draw.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    number: u32,
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    links: Vec<Range<usize>>,
    targets: Vec<SymbolKey>,
    current: bool,
}

/// One declaration's source, highlighted and linked.
#[derive(Clone, Debug)]
pub(crate) struct SourceView {
    lines: Vec<Line>,
    truncated: bool,
    doors: usize,
}

impl SourceView {
    /// Builds the view for one excerpt.
    ///
    /// `resolve` answers whether an identifier names a declaration on the
    /// shelf; the page's own name should answer `None`, since a door back to
    /// the page a reader is on is not a door.
    pub(crate) fn build(
        theme: &Theme,
        language: Language,
        lines: &[SourceLine],
        current: u32,
        truncation: Truncation,
        resolve: &dyn Fn(&str) -> Option<SymbolKey>,
    ) -> Self {
        let mut doors = 0_usize;
        let lines = lines
            .iter()
            .map(|line| {
                let built = build_line(theme, language, line, current, resolve);
                doors = doors.saturating_add(built.targets.len());
                built
            })
            .collect();
        Self {
            lines,
            truncated: truncation == Truncation::Truncated,
            doors,
        }
    }

    /// Returns how many lines the view holds.
    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }

    /// Returns how many identifiers on these lines open a page.
    pub(crate) const fn doors(&self) -> usize {
        self.doors
    }
}

/// Returns the rendered block: gutter, lines, and the truncation note.
pub(crate) fn block(
    theme: &Theme,
    view: &SourceView,
    prefix: &str,
    open: impl Fn(SymbolKey, &mut Window, &mut App) + 'static,
) -> Div {
    let open = Rc::new(open);
    div()
        .w_full()
        .flex()
        .flex_col()
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Small))
        .line_height(line_height(TypeScale::Interface))
        .children(
            view.lines
                .iter()
                .enumerate()
                .map(|(at, line)| draw_line(theme, line, prefix, at, Rc::clone(&open))),
        )
        .when(view.truncated, |block| {
            block.child(
                text::faint(theme)
                    .pt(space(Space::Snug))
                    .pl(px(GUTTER))
                    .child("The producer retained only this prefix; the declaration continues past it."),
            )
        })
}

fn build_line(
    theme: &Theme,
    language: Language,
    line: &SourceLine,
    current: u32,
    resolve: &dyn Fn(&str) -> Option<SymbolKey>,
) -> Line {
    let text = line.text();
    let number = line.number().get();
    if is_comment(text) {
        return Line {
            number,
            text: SharedString::from(text.to_owned()),
            highlights: vec![(0..text.len(), comment_style(theme))],
            links: Vec::new(),
            targets: Vec::new(),
            current: number == current,
        };
    }
    let mut highlights = Vec::new();
    let mut links = Vec::new();
    let mut targets = Vec::new();
    let mut spelled = String::with_capacity(text.len());
    for token in Signature::tokenize(text, language).tokens() {
        let start = spelled.len();
        spelled.push_str(token.text());
        let range = start..spelled.len();
        let door = linkable(token.kind(), token.text())
            .then(|| resolve(token.text()))
            .flatten();
        highlights.push((range.clone(), style_for(theme, token.kind(), door.is_some())));
        if let Some(symbol) = door {
            links.push(range);
            targets.push(symbol);
        }
    }
    Line {
        number,
        text: SharedString::from(spelled),
        highlights,
        links,
        targets,
        current: number == current,
    }
}

/// Returns whether a token could name a declaration worth resolving.
fn linkable(kind: TokenKind, text: &str) -> bool {
    matches!(
        kind,
        TokenKind::Type | TokenKind::Name | TokenKind::Text | TokenKind::Binding
    ) && text.chars().next().is_some_and(|first| first.is_alphabetic() || first == '_')
        && text.chars().count() > 1
}

/// Returns whether a whole line is commentary, to be set faint as one run.
fn is_comment(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with("* ")
        || trimmed == "*"
        || trimmed.starts_with("*/")
        || trimmed.starts_with("--")
}

fn comment_style(theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: Some(theme.paint(Paint::TextFaint)),
        font_style: Some(gpui::FontStyle::Italic),
        ..HighlightStyle::default()
    }
}

fn draw_line(
    theme: &Theme,
    line: &Line,
    prefix: &str,
    at: usize,
    open: Rc<impl Fn(SymbolKey, &mut Window, &mut App) + 'static>,
) -> Div {
    let styled = StyledText::new(line.text.clone()).with_highlights(line.highlights.clone());
    let targets = line.targets.clone();
    let body = InteractiveText::new(
        ElementId::Name(SharedString::from(format!("{prefix}-line-{at}"))),
        styled,
    )
    .on_click(line.links.clone(), move |index, window, cx| {
        if let Some(symbol) = targets.get(index).copied() {
            open(symbol, window, cx);
        }
    });
    div()
        .w_full()
        .flex()
        .items_start()
        .gap(space(Space::Base))
        .px(space(Space::Snug))
        .when(line.current, |row| row.bg(theme.paint(Paint::GiltWash)))
        .child(
            div()
                .flex_none()
                .w(px(GUTTER))
                .text_align(TextAlign::Right)
                .text_size(type_size(TypeScale::Micro))
                .text_color(theme.paint(if line.current {
                    Paint::Gilt
                } else {
                    Paint::TextFaint
                }))
                .child(line.number.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_color(theme.paint(Paint::Text))
                .child(body),
        )
}
