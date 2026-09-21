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
    App, Div, ElementId, HighlightStyle, InteractiveText, ParentElement, SharedString, Styled,
    StyledText, TextAlign, UniformListScrollHandle, Window, div, px, uniform_list,
};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// Width of the line-number gutter, in pixels.
const GUTTER: f32 = 44.0;

/// One highlighted line, ready to draw.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    number_label: SharedString,
    text: SharedString,
    highlights: Arc<[(Range<usize>, HighlightStyle)]>,
    links: Arc<[Range<usize>]>,
    targets: Arc<[SymbolKey]>,
    current: bool,
}

/// One declaration's source, highlighted and linked.
#[derive(Clone, Debug)]
pub(crate) struct SourceView {
    lines: Arc<[Line]>,
    truncated: bool,
    doors: usize,
    current_line: Option<usize>,
}

/// The result of searching one already-built source view.
///
/// Search is deliberately a projection over the captured lines rather than a
/// second source fetch or a substring copied into view state. That keeps the
/// source sheet honest when the producer returned a partial excerpt and lets
/// the caller report an exact count while preserving line numbers and links.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SourceSearch {
    matches: usize,
    first_line: Option<usize>,
    highlights: Arc<[Arc<[(Range<usize>, HighlightStyle)]>]>,
    locations: Arc<[(usize, Range<usize>)]>,
}

impl SourceSearch {
    /// Returns the number of case-insensitive occurrences in the excerpt.
    pub(crate) const fn matches(&self) -> usize {
        self.matches
    }

    /// Returns the zero-based line containing the first occurrence.
    pub(crate) const fn first_line(&self) -> Option<usize> {
        self.first_line
    }

    /// Returns the line and byte span for one zero-based match.
    pub(crate) fn location(&self, index: usize) -> Option<(usize, Range<usize>)> {
        self.locations.get(index).cloned()
    }
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
        let current_line = lines
            .iter()
            .position(|line| line.number().get() == current);
        let lines: Arc<[Line]> = lines
            .iter()
            .map(|line| {
                let built = build_line(theme, language, line, current, resolve);
                doors = doors.saturating_add(built.targets.len());
                built
            })
            .collect::<Vec<_>>()
            .into();
        Self {
            lines,
            truncated: truncation == Truncation::Truncated,
            doors,
            current_line,
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

    /// Returns the zero-based retained line matching the declaration anchor.
    pub(crate) const fn current_line(&self) -> Option<usize> {
        self.current_line
    }

    /// Searches the captured excerpt without changing its token or link data.
    ///
    /// Empty input is intentionally a no-op. A query that changes case or
    /// contains Unicode is still compared by the shared range helper, which
    /// projects folded text back to valid UTF-8 byte ranges.
    pub(crate) fn search(&self, theme: &Theme, needle: &str) -> SourceSearch {
        if needle.trim().is_empty() {
            return SourceSearch::default();
        }
        let mut result = SourceSearch::default();
        let mut highlights = Vec::with_capacity(self.lines.len());
        let mut locations = Vec::new();
        let query_style = HighlightStyle {
            background_color: Some(theme.paint(Paint::GiltWash)),
            color: Some(theme.paint(Paint::TextStrong)),
            ..HighlightStyle::default()
        };
        for (line, entry) in self.lines.iter().enumerate() {
            let line_ranges = text::match_ranges(entry.text.as_ref(), needle);
            let count = line_ranges.len();
            if count == 0 {
                highlights.push(Arc::from(entry.highlights.as_ref()));
                continue;
            }
            if result.first_line.is_none() {
                result.first_line = Some(line);
            }
            result.matches = result.matches.saturating_add(count);
            locations.extend(line_ranges.iter().cloned().map(|range| (line, range)));
            let mut line_highlights = entry.highlights.to_vec();
            line_highlights.extend(
                line_ranges
                    .iter()
                    .cloned()
                    .map(|range| (range, query_style)),
            );
            highlights.push(line_highlights.into());
        }
        result.highlights = highlights.into();
        result.locations = locations.into();
        result
    }
}

/// Returns the source block using a cached query projection and a persistent
/// uniform-list scroll handle. Only visible source lines are cloned and
/// lowered into GPUI elements.
pub(crate) fn block_with_search(
    theme: &Theme,
    view: &SourceView,
    prefix: &str,
    search: &SourceSearch,
    scroll: &UniformListScrollHandle,
    active_match: usize,
    open: impl Fn(SymbolKey, &mut Window, &mut App) + 'static,
) -> Div {
    let open = Rc::new(open);
    let lines = Arc::clone(&view.lines);
    let active = search.location(active_match);
    let query_highlights = Arc::clone(&search.highlights);
    let theme_for_rows = theme.clone();
    let prefix_for_rows = prefix.to_owned();
    div()
        .w_full()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Small))
        .line_height(line_height(TypeScale::Interface))
        .child(
            uniform_list(
                ElementId::Name(SharedString::from(format!("{prefix}-lines"))),
                lines.len(),
                move |range, _window, _cx| {
                    lines
                        .get(range.clone())
                        .unwrap_or_default()
                        .iter()
                        .enumerate()
                        .map(|(offset, line)| {
                            let at = range.start.saturating_add(offset);
                            let highlight_runs = query_highlights
                                .get(at)
                                .map_or(line.highlights.as_ref(), Arc::as_ref);
                            let active_range = active.as_ref().and_then(|(line_at, active_range)| {
                                (*line_at == at).then_some(active_range)
                            });
                            draw_line(
                                &theme_for_rows,
                                line,
                                &prefix_for_rows,
                                at,
                                highlight_runs,
                                active_range,
                                Rc::clone(&open),
                            )
                        })
                        .collect()
                },
            )
            .track_scroll(scroll)
            .flex_1()
            .min_h(px(0.0)),
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
            number_label: SharedString::from(number.to_string()),
            text: SharedString::from(text.to_owned()),
            highlights: Arc::from([(0..text.len(), comment_style(theme))]),
            links: Arc::from([]),
            targets: Arc::from([]),
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
        highlights.push((
            range.clone(),
            style_for(theme, token.kind(), door.is_some()),
        ));
        if let Some(symbol) = door {
            links.push(range);
            targets.push(symbol);
        }
    }
    Line {
        number_label: SharedString::from(number.to_string()),
        text: SharedString::from(spelled),
        highlights: highlights.into(),
        links: links.into(),
        targets: targets.into(),
        current: number == current,
    }
}

/// Returns whether a token could name a declaration worth resolving.
fn linkable(kind: TokenKind, text: &str) -> bool {
    matches!(
        kind,
        TokenKind::Type | TokenKind::Name | TokenKind::Text | TokenKind::Binding
    ) && text
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
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
    highlight_runs: &[(Range<usize>, HighlightStyle)],
    active_range: Option<&Range<usize>>,
    open: Rc<impl Fn(SymbolKey, &mut Window, &mut App) + 'static>,
) -> Div {
    let active_style = HighlightStyle {
        background_color: Some(theme.paint(Paint::Gilt)),
        color: Some(theme.paint(Paint::TextStrong)),
        ..HighlightStyle::default()
    };
    let highlights = highlight_runs
        .iter()
        .cloned()
        .chain(active_range.map(|range| (range.clone(), active_style)));
    let styled = StyledText::new(line.text.clone()).with_highlights(highlights);
    let targets = Arc::clone(&line.targets);
    let body = InteractiveText::new(
        ElementId::Name(SharedString::from(format!("{prefix}-line-{at}"))),
        styled,
    )
    .on_click(line.links.as_ref().to_vec(), move |index, window, cx| {
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
                .child(line.number_label.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use backend_present::{Language, LineNumber, SourceLine, Truncation};

    fn line(number: u32, text: &str) -> SourceLine {
        SourceLine::new(LineNumber::new(number).expect("positive line"), text)
    }

    #[test]
    fn source_find_counts_case_insensitive_occurrences_and_keeps_first_line() {
        let lines = [
            line(10, "pub fn ferris() {"),
            line(11, "    let fellow = ferris();"),
            line(12, "}"),
        ];
        let view = SourceView::build(
            &Theme::default(),
            Language::Rust,
            &lines,
            10,
            Truncation::Complete,
            &|_| None,
        );

        let theme = Theme::default();
        assert_eq!(view.search(&theme, "FERRIS").matches(), 2);
        assert_eq!(view.search(&theme, "FERRIS").first_line(), Some(0));
        assert_eq!(view.search(&theme, "FERRIS").location(1), Some((1, 17..23)));
        assert_eq!(view.search(&theme, "").matches(), 0);
    }

    #[test]
    fn source_find_reports_no_match_without_touching_link_count() {
        let lines = [line(4, "struct Beacon;")];
        let view = SourceView::build(
            &Theme::default(),
            Language::Rust,
            &lines,
            4,
            Truncation::Truncated,
            &|_| None,
        );

        let theme = Theme::default();
        assert_eq!(view.search(&theme, "missing").matches(), 0);
        assert_eq!(view.search(&theme, "missing").first_line(), None);
        assert_eq!(view.doors(), 0, "the own declaration is not a source door");
        assert_eq!(view.search(&theme, "Beacon").matches(), 1);
    }

    #[test]
    fn source_anchor_resolves_to_the_retained_zero_based_line() {
        let lines = [line(40, "before"), line(41, "pub fn ferris() {}"), line(42, "after")];
        let view = SourceView::build(
            &Theme::default(),
            Language::Rust,
            &lines,
            41,
            Truncation::Complete,
            &|_| None,
        );
        assert_eq!(view.current_line(), Some(1));
    }

    #[test]
    fn source_anchor_is_absent_when_capture_starts_after_the_declaration() {
        let lines = [line(40, "before"), line(42, "after")];
        let view = SourceView::build(
            &Theme::default(),
            Language::Rust,
            &lines,
            41,
            Truncation::Truncated,
            &|_| None,
        );
        assert_eq!(view.current_line(), None);
    }
}
