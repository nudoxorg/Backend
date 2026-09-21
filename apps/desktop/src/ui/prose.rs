//! Documentation prose, with the compiler's own links made clickable.
//! A paragraph is one shaped text run; a link is a byte range inside it.
//! Code examples drop into the same specimen well signatures use.
//!
//! The links here are not scraped out of text. The producer emitted a
//! [`backend_library::Fragment::Link`] carrying a stable declaration key, so
//! following one is an exact navigation rather than a guess — which is the
//! whole reason this product's documentation can be hyperlinked end to end.
//!
//! One reconstruction happens here, and it is worth stating exactly.
//! [`backend_present::Prose`] is a flat run of blocks because that is what a
//! terminal and a Markdown renderer both want: a link is its own block. A
//! window wants the link back *inside* the sentence it was written in. The
//! inverse is exact rather than heuristic, because of how the shared fold
//! works: a paragraph break emits two adjacent `Text` blocks, while a link
//! emits `Text`, `Link`, `Text`. So a pair of adjacent blocks belongs to one
//! flow if and only if one of the two is a `Link`, and grouping on that rule
//! recovers the author's paragraphs with no guessing and no text matching.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, line_height, space, type_size};
use backend_library::SymbolKey;
use backend_present::Prose;
use gpui::{
    AnyElement, App, Div, ElementId, HighlightStyle, InteractiveText, IntoElement, ParentElement,
    SharedString, Styled, StyledText, UnderlineStyle, Window, div, px,
};
use std::ops::Range;
use std::rc::Rc;

/// Returns the whole prose region for one page.
pub(crate) fn blocks(
    theme: &Theme,
    prose: &[Prose],
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
            flows(prose)
                .into_iter()
                .enumerate()
                .map(|(at, flow)| one(theme, &flow, prefix, at, Rc::clone(&open))),
        )
}

/// Returns the plain text of one prose run, for summaries and tooltips.
pub(crate) fn plain(prose: &[Prose]) -> String {
    let mut out = String::new();
    for block in prose {
        match block {
            Prose::Text(text) | Prose::Code(text) => out.push_str(text),
            Prose::Link { label, .. } => out.push_str(label),
        }
        out.push(' ');
    }
    out.trim().to_owned()
}

/// Returns the first sentence of the first paragraph.
pub(crate) fn summary(prose: &[Prose]) -> Option<String> {
    let first = flows(prose).into_iter().find(|flow| {
        flow.iter()
            .any(|block| matches!(block, Prose::Text(_) | Prose::Link { .. }))
    })?;
    let text = plain(&first);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed
        .find(". ")
        .map_or(trimmed.len(), |at| at.saturating_add(1));
    Some(trimmed.get(..end).unwrap_or(trimmed).trim().to_owned())
}

/// Groups the flat shared blocks back into the paragraphs they were written as.
fn flows(prose: &[Prose]) -> Vec<Vec<Prose>> {
    let mut flows: Vec<Vec<Prose>> = Vec::new();
    for block in prose {
        let joins = match (flows.last().and_then(|flow| flow.last()), block) {
            (Some(Prose::Link { .. }), _) | (Some(Prose::Text(_)), Prose::Link { .. }) => true,
            (None | Some(Prose::Code(_) | Prose::Text(_)), _) => false,
        };
        match flows.last_mut() {
            Some(flow) if joins => flow.push(block.clone()),
            _ => flows.push(vec![block.clone()]),
        }
    }
    flows
}

fn one(
    theme: &Theme,
    flow: &[Prose],
    prefix: &str,
    at: usize,
    open: Rc<impl Fn(SymbolKey, &mut Window, &mut App) + 'static>,
) -> AnyElement {
    if let [Prose::Code(code)] = flow {
        return super::surface::sunken(theme)
            .w_full()
            .px(space(Space::Room))
            .py(space(Space::Snug))
            .font_family(theme.specimen())
            .text_size(type_size(TypeScale::Small))
            .line_height(line_height(TypeScale::Body))
            .text_color(theme.paint(Paint::Silver1))
            .child(code.clone())
            .into_any_element();
    }
    paragraph(theme, flow, prefix, at, open).into_any_element()
}

fn paragraph(
    theme: &Theme,
    flow: &[Prose],
    prefix: &str,
    at: usize,
    open: Rc<impl Fn(SymbolKey, &mut Window, &mut App) + 'static>,
) -> Div {
    let mut text = String::new();
    let mut highlights = Vec::new();
    let mut links: Vec<Range<usize>> = Vec::new();
    let mut targets = Vec::new();
    for block in flow {
        match block {
            Prose::Text(body) | Prose::Code(body) => {
                pad(&mut text);
                text.push_str(body);
            }
            Prose::Link { label, target } => {
                pad(&mut text);
                let start = text.len();
                text.push_str(label);
                let range = start..text.len();
                highlights.push((range.clone(), link_style(theme)));
                links.push(range);
                targets.push(*target);
            }
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
        .font_family(theme.serif_face())
        .text_size(type_size(TypeScale::Body))
        .line_height(line_height(TypeScale::Body))
        .text_color(theme.paint(Paint::Silver1))
        .child(body)
}

/// Restores the one space the shared fold trimmed off each block's edges.
fn pad(text: &mut String) {
    if !text.is_empty() && !text.ends_with(' ') {
        text.push(' ');
    }
}

fn link_style(theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: Some(theme.paint(Paint::Silver0)),
        underline: Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(super::text::underline_ink(theme, false)),
            wavy: false,
        }),
        ..HighlightStyle::default()
    }
}

/// Returns whether a summary only restates the identity it belongs to.
///
/// The producer writes `struct in src/main.cpp:3` for a declaration with no
/// documentation. That sentence is true and says nothing the header does not
/// already say, so no surface should set it as prose.
pub(crate) fn tautological(summary: &str, identity: &backend_present::Identity) -> bool {
    let Some((_, site)) = summary.trim().split_once(" in ") else {
        return false;
    };
    let path = identity
        .path()
        .map(backend_present::PackagePath::as_str)
        .unwrap_or_default();
    let spelled = identity
        .line()
        .map_or_else(|| path.to_owned(), |line| format!("{path}:{}", line.get()));
    !path.is_empty() && site.trim() == spelled
}
