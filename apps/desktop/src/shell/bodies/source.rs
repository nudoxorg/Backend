//! Source, plain: the file's crumb, the text around the declaration with its
//! lines numbered (the declaration's numbers lit mint), and the
//! declaration's one sentence and callers in the margin.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::pages::{
    ByteSpan, DocFragment, PageKey, SourceCoverage, SourceOrigin, SourceText, SourceView, SymbolRef,
};
use crate::navigation::{Intent, Route, SymbolRoute};
use crate::shell::focus::{Recall, Target};
use crate::shell::kit::{gap_words, quiet, symbol_route, text};
use crate::shell::reader::Reader;
use facet::tokens::ty;
use facet::{Control, Set as _, Space};
use gpui::{
    App, AppContext as _, ClickEvent, Context, ElementId, Entity, InteractiveElement,
    InteractiveText, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    StyledText, Subscription, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

/// Bounded source pages keep wrapping, syntax work and keyboard targets small
/// while every byte remains reachable, including a long minified line.
const CONTEXT_BEFORE: u32 = 24;
const MAX_SOURCE_LINES: usize = 64;
const MAX_SOURCE_BYTES: usize = 8 * 1024;
const MAX_SOURCE_LINE_BYTES: usize = 2 * 1024;
const MAX_PAGE_REFERENCES: usize = 32;
const MAX_PAGE_HISTORY: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceCursor {
    line: u32,
    byte: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VisibleLine {
    number: u32,
    span: ByteSpan,
    continued: bool,
    more_in_line: bool,
}

struct SourcePage {
    lines: Vec<VisibleLine>,
    next: Option<SourceCursor>,
}

impl SourcePage {
    fn at(source: &SourceText, cursor: SourceCursor) -> Self {
        let Some(first_index) = cursor
            .line
            .checked_sub(source.first_line)
            .and_then(|index| usize::try_from(index).ok())
        else {
            return Self {
                lines: Vec::new(),
                next: None,
            };
        };
        let spans = source.line_spans_in(first_index, MAX_SOURCE_LINES);
        let mut lines = Vec::with_capacity(spans.len());
        let mut used = 0_usize;
        let mut next = None;
        for (index, full_span) in spans.into_iter().enumerate() {
            let number = cursor
                .line
                .saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
            let Some(full) = source.text().get(full_span.range()) else {
                break;
            };
            let offset = if index == 0 {
                cursor.byte.min(full.len())
            } else {
                0
            };
            let Some(rest) = full.get(offset..) else {
                break;
            };
            let allowed = MAX_SOURCE_BYTES
                .saturating_sub(used)
                .min(MAX_SOURCE_LINE_BYTES);
            let visible = utf8_prefix(rest, allowed);
            if visible.is_empty() && !rest.is_empty() {
                next = Some(SourceCursor {
                    line: number,
                    byte: offset,
                });
                break;
            }
            let start = full_span.start as usize + offset;
            let end = start.saturating_add(visible.len());
            let (Ok(start_byte), Ok(end_byte)) = (u32::try_from(start), u32::try_from(end)) else {
                break;
            };
            let more_in_line = visible.len() < rest.len();
            lines.push(VisibleLine {
                number,
                span: ByteSpan {
                    start: start_byte,
                    end: end_byte,
                },
                continued: offset > 0,
                more_in_line,
            });
            used = used.saturating_add(visible.len()).saturating_add(1);
            if more_in_line {
                next = Some(SourceCursor {
                    line: number,
                    byte: offset + visible.len(),
                });
                break;
            }
            if used >= MAX_SOURCE_BYTES || lines.len() >= MAX_SOURCE_LINES {
                let next_line = number.saturating_add(1);
                if usize::try_from(next_line.saturating_sub(source.first_line))
                    .is_ok_and(|index| index < source.line_count())
                {
                    next = Some(SourceCursor {
                        line: next_line,
                        byte: 0,
                    });
                }
                break;
            }
        }
        if next.is_none() {
            let following = lines.last().map(|line| line.number.saturating_add(1));
            if let Some(line) = following.filter(|line| {
                usize::try_from(line.saturating_sub(source.first_line))
                    .is_ok_and(|index| index < source.line_count())
            }) {
                next = Some(SourceCursor { line, byte: 0 });
            }
        }
        Self { lines, next }
    }
}

fn previous_cursor(source: &SourceText, cursor: SourceCursor) -> Option<SourceCursor> {
    if cursor.byte > 0 {
        let span = source.line_span(cursor.line)?;
        let line = source.text().get(span.range())?;
        let mut byte = cursor
            .byte
            .min(line.len())
            .saturating_sub(MAX_SOURCE_LINE_BYTES);
        while !line.is_char_boundary(byte) {
            byte += 1;
        }
        return Some(SourceCursor {
            line: cursor.line,
            byte,
        });
    }
    let before = cursor.line.checked_sub(source.first_line)? as usize;
    if before == 0 {
        return None;
    }
    let first_index = before.saturating_sub(MAX_SOURCE_LINES);
    let spans = source.line_spans_in(first_index, before - first_index);
    let mut used = 0_usize;
    let mut start = None;
    for (index, span) in spans.iter().enumerate().rev() {
        let line = source.text().get(span.range())?;
        if line.len() > MAX_SOURCE_LINE_BYTES {
            if start.is_none() {
                let mut byte = line.len().saturating_sub(MAX_SOURCE_LINE_BYTES);
                while !line.is_char_boundary(byte) {
                    byte += 1;
                }
                let number = source
                    .first_line
                    .saturating_add(u32::try_from(first_index + index).ok()?);
                return Some(SourceCursor { line: number, byte });
            }
            break;
        }
        if used.saturating_add(line.len()).saturating_add(1) > MAX_SOURCE_BYTES {
            break;
        }
        used = used.saturating_add(line.len()).saturating_add(1);
        let number = source
            .first_line
            .saturating_add(u32::try_from(first_index + index).ok()?);
        start = Some(SourceCursor {
            line: number,
            byte: 0,
        });
    }
    start
}

fn initial_cursor(source: &SourceText, line: u32, context: u32) -> SourceCursor {
    let target = line.max(source.first_line);
    let Some(target_index) = target
        .checked_sub(source.first_line)
        .map(|index| index as usize)
    else {
        return SourceCursor {
            line: source.first_line,
            byte: 0,
        };
    };
    let first_index = target_index.saturating_sub(context as usize);
    let spans = source.line_spans_in(first_index, target_index - first_index);
    let mut start = target;
    let mut bytes = 0_usize;
    for (index, span) in spans.iter().enumerate().rev() {
        let Some(line) = source.text().get(span.range()) else {
            break;
        };
        if line.len() > MAX_SOURCE_LINE_BYTES || bytes + line.len() + 1 > MAX_SOURCE_BYTES / 4 {
            break;
        }
        bytes += line.len() + 1;
        start = source
            .first_line
            .saturating_add(u32::try_from(first_index + index).unwrap_or(u32::MAX));
    }
    SourceCursor {
        line: start,
        byte: 0,
    }
}

fn verified_link(coverage: SourceCoverage, identifier: ByteSpan, visible: ByteSpan) -> bool {
    let within = identifier.start >= visible.start
        && identifier.end <= visible.end
        && identifier.start < identifier.end;
    within
        && match coverage {
            SourceCoverage::CapturedExcerpt => true,
            SourceCoverage::LiveFileExcerptVerified { bytes } => {
                identifier.start >= bytes.start && identifier.end <= bytes.end
            }
            SourceCoverage::Unverified => false,
        }
}

struct Pager {
    cursor: SourceCursor,
    back: Vec<SourceCursor>,
    forward: Vec<SourceCursor>,
    first: u32,
    last: u32,
    input: Entity<InputState>,
    error: Option<SharedString>,
    reference_page: usize,
    recall: Recall,
    reveal: Rc<Cell<bool>>,
    _subscription: Subscription,
}

impl Pager {
    fn new(
        cursor: SourceCursor,
        first: u32,
        last: u32,
        reference_page: usize,
        recall: Recall,
        reveal: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Line number"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |pager, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    pager.jump(&input.read(cx).value().to_string(), cx);
                }
            },
        );
        Self {
            cursor,
            back: Vec::new(),
            forward: Vec::new(),
            first,
            last,
            input,
            error: None,
            reference_page,
            recall,
            reveal,
            _subscription: subscription,
        }
    }

    fn show(&mut self, cursor: SourceCursor, focus: u32, cx: &mut Context<Self>) {
        self.cursor = cursor;
        self.back.clear();
        self.forward.clear();
        self.error = None;
        self.reference_page = 0;
        self.recall
            .focus(crate::shell::reader::source_line_shared_id(focus));
        self.reveal.set(true);
        cx.notify();
    }

    fn next(&mut self, fallback: SourceCursor, cx: &mut Context<Self>) {
        let next = self.forward.pop().unwrap_or(fallback);
        push_history(&mut self.back, self.cursor);
        self.cursor = next;
        self.reference_page = 0;
        self.recall
            .focus(crate::shell::reader::source_line_shared_id(next.line));
        self.reveal.set(true);
        cx.notify();
    }

    fn previous(&mut self, fallback: SourceCursor, cx: &mut Context<Self>) {
        let previous = self.back.pop().unwrap_or(fallback);
        push_history(&mut self.forward, self.cursor);
        self.cursor = previous;
        self.reference_page = 0;
        self.recall
            .focus(crate::shell::reader::source_line_shared_id(previous.line));
        self.reveal.set(true);
        cx.notify();
    }

    fn jump(&mut self, text: &str, cx: &mut Context<Self>) {
        match text.trim().parse::<u32>() {
            Ok(line) if (self.first..=self.last).contains(&line) => {
                self.show(SourceCursor { line, byte: 0 }, line, cx);
            }
            _ => {
                self.error =
                    Some(format!("Enter a line from {} to {}", self.first, self.last).into());
                cx.notify();
            }
        }
    }

    fn show_references(&mut self, page: usize, cx: &mut Context<Self>) {
        self.reference_page = page;
        self.recall.focus(format!(
            "source-reference-{}",
            page.saturating_mul(MAX_PAGE_REFERENCES)
        ));
        self.reveal.set(true);
        cx.notify();
    }
}

fn push_history(history: &mut Vec<SourceCursor>, cursor: SourceCursor) {
    if history.len() == MAX_PAGE_HISTORY {
        history.remove(0);
    }
    history.push(cursor);
}

pub(super) fn body(
    place: &Route,
    route: &SymbolRoute,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let symbol = match crate::runtime::store::route_declaration(place) {
        Ok(symbol) => symbol,
        Err(unread) => return ctx.unread(&unread),
    };
    let resource = store.source(&symbol);
    let view = match shown(&resource) {
        Shown::Ready(view) => view.clone(),
        other => {
            let name = symbol.identity().name().to_owned();
            return not_ready(&other, &PageKey::Source(symbol), &name, ctx, cx);
        }
    };
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut leaves = Vec::new();
    // The crumb: package › file › declaration.
    let identity = symbol.identity();
    let mut crumb = Vec::new();
    if let Some(project) = identity.project() {
        crumb.push(project.name().to_owned());
    }
    if let Some(file) = view.file.known() {
        crumb.push(file.to_string());
    }
    crumb.push(view.symbol.name.to_string());
    let crumb = ctx.say(crumb.join("  ›  "));
    leaves.push(Leaf::new(
        text(ty::MONO_ROW, &measure, palette.ink2).child(crumb),
    ));
    match view.text.known() {
        Some(source) => {
            if source.origin == SourceOrigin::LocalFile
                || source.coverage() == SourceCoverage::Unverified
            {
                let status = match source.coverage() {
                    SourceCoverage::LiveFileExcerptVerified { .. } => {
                        "Current local file · only the declaration excerpt matches indexed source; surrounding bytes may have changed."
                    }
                    SourceCoverage::Unverified => {
                        "Saved source text · these bytes have not been revalidated; source links are disabled."
                    }
                    SourceCoverage::CapturedExcerpt => {
                        "Local source · indexed source coverage is not established."
                    }
                };
                let words = ctx.say(status);
                leaves.push(Leaf::new(quiet(words, &measure, palette)));
            }
            let note = margin(&view, store, &symbol, route, ctx);
            let code = code(&view, source, route, ctx, window, cx);
            let leaf = Leaf::new(code);
            leaves.push(match note {
                Some(note) => leaf.with_note(note),
                None => leaf,
            });
            if let Some(gap) = view.identifiers.gap() {
                let words = ctx.say(format!("Source links aren't available: {}", gap.detail));
                leaves.push(Leaf::new(quiet(words, &ctx.measure, ctx.palette)));
            }
        }
        None => {
            if let Some(gap) = view.text.gap() {
                let words = ctx.say(gap_words(gap));
                leaves.push(Leaf::new(quiet(words, &measure, palette)));
            }
        }
    }
    leaves
}

fn code(
    view: &SourceView,
    source: &SourceText,
    route: &SymbolRoute,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let declaration = view.declaration.known().copied();
    let first_line = source.first_line;
    let total = u32::try_from(source.line_count()).unwrap_or(u32::MAX);
    let last_line = first_line.saturating_add(total.saturating_sub(1));
    let requested_line = route
        .line
        .filter(|line| *line >= first_line && *line <= last_line);
    let initial = requested_line
        .or_else(|| declaration.map(|span| span.first))
        .filter(|line| *line >= first_line && *line <= last_line)
        .map_or(
            SourceCursor {
                line: first_line,
                byte: 0,
            },
            |line| {
                initial_cursor(
                    source,
                    line,
                    if requested_line.is_some() {
                        0
                    } else {
                        CONTEXT_BEFORE
                    },
                )
            },
        );
    let pager_key: ElementId = format!(
        "source-pager-{}-{}-{}-{:?}-{:?}",
        ctx.place_key, first_line, total, route.line, ctx.source_generation
    )
    .into();
    let leaving = Route::Symbol(route.clone());
    let restore_target = ctx.targets.left_by(&leaving);
    let restored_reference_page = restore_target
        .as_ref()
        .and_then(|id| id.strip_prefix("source-reference-"))
        .and_then(|index| index.parse::<usize>().ok())
        .map_or(0, |index| index / MAX_PAGE_REFERENCES);
    let recall = ctx.targets.recall();
    let reveal = Rc::clone(&ctx.reader_reveal);
    let pager = window.use_keyed_state(pager_key, cx, move |window, cx| {
        Pager::new(
            initial,
            first_line,
            last_line,
            restored_reference_page,
            recall,
            reveal,
            window,
            cx,
        )
    });
    let cursor = pager.read(cx).cursor;
    let page = SourcePage::at(source, cursor);
    let from = cursor.line;
    let mut shown = String::new();
    let mut numbers = Vec::new();
    let mut raw_lines = Vec::new();
    let mut source_starts = Vec::new();
    for line in &page.lines {
        let visible = source.text().get(line.span.range()).unwrap_or_default();
        source_starts.push(line.span.start as usize);
        numbers.push(line.number);
        raw_lines.push(visible);
        shown.push_str(visible);
        shown.push('\n');
    }
    if !shown.is_empty() {
        shown.pop();
    }
    let shown: SharedString = shown.into();
    ctx.say(shown.clone());
    let language = code_language(view);
    let highlights = language
        .and_then(|language| facet::code::highlight(language, shown.clone(), window, cx))
        .map(|highlighted| highlighted.styles(&palette));
    // The number column holds the widest number; the code gets the rest and
    // soft-wraps at token boundaries — continuation lines carry no number.
    let role = measure.role(ty::CODE);
    let digits = numbers.last().map_or(1, |number| number.to_string().len());
    let gap = measure.space(Space::Gutter);
    let number_width = crate::shell::text_fit::text_width(&"0".repeat(digits), &role, cx);
    let columns = crate::shell::text_fit::columns(measure.width() - number_width - gap, &role, cx);
    let lines =
        crate::shell::text_fit::wrap_code(&shown, highlights.as_deref().unwrap_or(&[]), columns);
    let mut grouped = BTreeMap::<usize, Vec<crate::shell::text_fit::CodeLine>>::new();
    for line in lines {
        grouped.entry(line.source).or_default().push(line);
    }
    let above = from.saturating_sub(first_line);
    let rendered_to = numbers.last().copied().unwrap_or(from);
    let below = last_line.saturating_sub(rendered_to);
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base));
    column = column.child(pager_controls(
        "top", &pager, cursor, &page, source, ctx, cx,
    ));
    if above > 0 || cursor.byte > 0 {
        let label = if cursor.byte > 0 {
            format!(
                "{above} earlier lines · line {from} continues from byte {}",
                cursor.byte
            )
        } else {
            format!("{above} earlier lines")
        };
        column = column.child(quiet(label, &measure, palette));
    }
    let mut rows = div().flex().flex_col();
    let mut visible_references = Vec::<(SymbolRef, Route)>::new();
    let mut reference_indices = BTreeMap::<SymbolRef, usize>::new();
    // The producer emits identifier spans in byte order. Select the visible
    // spans once for each source row, then use a binary interval search per
    // wrapped visual row. This avoids rescanning the entire file's identifier
    // table for every line (and every wrapped line) on each render frame.
    let mut identifiers_by_line =
        vec![Vec::<&crate::model::pages::IdentifierSpan>::new(); numbers.len()];
    let identifiers = match source.coverage() {
        SourceCoverage::CapturedExcerpt | SourceCoverage::LiveFileExcerptVerified { .. } => {
            view.identifiers.known()
        }
        SourceCoverage::Unverified => None,
    };
    if let Some(identifiers) = identifiers {
        let visible_start = source_starts.first().copied().unwrap_or(usize::MAX);
        let mut cursor = identifiers.partition_point(|identifier| {
            usize::try_from(identifier.span.end).unwrap_or(usize::MAX) <= visible_start
        });
        for (source_index, line_start) in source_starts.iter().copied().enumerate() {
            let line_end = line_start.saturating_add(raw_lines[source_index].len());
            while cursor < identifiers.len()
                && usize::try_from(identifiers[cursor].span.end).unwrap_or(usize::MAX) <= line_start
            {
                cursor += 1;
            }
            let mut end = cursor;
            while end < identifiers.len()
                && usize::try_from(identifiers[end].span.start).unwrap_or(usize::MAX) < line_end
            {
                identifiers_by_line[source_index].push(&identifiers[end]);
                end += 1;
            }
            cursor = end;
        }
    }
    for (source_index, number) in numbers.iter().copied().enumerate() {
        let pieces = grouped.remove(&source_index).unwrap_or_default();
        let id = crate::shell::reader::source_line_shared_id(number);
        let focus_key = (ctx.place_key, number, ctx.source_generation);
        if requested_line == Some(number)
            && ctx.active
            && ctx.source_focus_applied.get() != Some(focus_key)
        {
            ctx.targets.focus(id.clone());
            ctx.reader_reveal.set(true);
            ctx.source_focus_applied.set(Some(focus_key));
        }
        let copy_source = source.clone();
        let copy_span = page.lines[source_index].span;
        let copy_line: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            let words = copy_source
                .text()
                .get(copy_span.range())
                .unwrap_or_default();
            app.write_to_clipboard(gpui::ClipboardItem::new_string(words.to_owned()));
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: format!("Copy source line {number}").into(),
            act: copy_line.clone(),
            peek: None,
            source: None,
        });

        let mut visual_lines = div().flex().flex_col();
        for (piece_index, line) in pieces.into_iter().enumerate() {
            let mut link_ranges = Vec::new();
            let mut link_routes = Vec::new();
            let mut link_ids = Vec::new();
            let line_start = source_starts[source_index];
            let piece_start = line_start.saturating_add(line.source_range.start);
            let piece_end = line_start.saturating_add(line.source_range.end);
            {
                let line_identifiers = &identifiers_by_line[source_index];
                let first = line_identifiers.partition_point(|identifier| {
                    usize::try_from(identifier.span.end).unwrap_or(usize::MAX) <= piece_start
                });
                let rest = &line_identifiers[first..];
                let count = rest.partition_point(|identifier| {
                    usize::try_from(identifier.span.start).unwrap_or(usize::MAX) < piece_end
                });
                for identifier in &rest[..count] {
                    let start = usize::try_from(identifier.span.start).unwrap_or(usize::MAX);
                    let end = usize::try_from(identifier.span.end).unwrap_or(usize::MAX);
                    if !verified_link(
                        source.coverage(),
                        identifier.span,
                        page.lines[source_index].span,
                    ) {
                        continue;
                    }
                    let source_from = start.max(piece_start).saturating_sub(line_start);
                    let source_to = end.min(piece_end).saturating_sub(line_start);
                    if source_from >= source_to {
                        continue;
                    }
                    let text_from = line
                        .text_offset
                        .saturating_add(source_from.saturating_sub(line.source_range.start));
                    let text_to = line
                        .text_offset
                        .saturating_add(source_to.saturating_sub(line.source_range.start));
                    if line.text.get(text_from..text_to).is_none() {
                        continue;
                    }
                    let target = identifier.link.target.clone();
                    let Some(target_route) = symbol_route(route.package.as_str(), &target) else {
                        continue;
                    };
                    let target_index =
                        *reference_indices.entry(target.clone()).or_insert_with(|| {
                            let index = visible_references.len();
                            visible_references.push((target.clone(), target_route.clone()));
                            index
                        });
                    link_ranges.push(text_from..text_to);
                    link_routes.push(target_route.clone());
                    link_ids.push(SharedString::from(format!(
                        "source-reference-{target_index}"
                    )));
                }
            }
            let label = if piece_index == 0 {
                if page.lines[source_index].continued {
                    format!("{number}+")
                } else {
                    number.to_string()
                }
            } else {
                String::new()
            };
            let declared =
                declaration.is_some_and(|span| span.first <= number && number <= span.last);
            let requested = requested_line == Some(number);
            let copy_gutter = copy_line.clone();
            let line_number = div()
                .id(format!("source-copy-line-{number}"))
                .flex_none()
                .w(number_width)
                .flex()
                .justify_end()
                .child(
                    text(
                        ty::CODE,
                        &measure,
                        if declared {
                            palette.mint.base
                        } else if requested {
                            palette.peri.base
                        } else {
                            palette.ink3
                        },
                    )
                    .child(label),
                )
                .cursor_pointer()
                .on_click(move |_: &ClickEvent, window, app| copy_gutter(window, app));
            let shared_text: SharedString = line.text.clone().into();
            let styled = StyledText::new(shared_text.clone()).with_highlights(line.runs);
            let body: gpui::AnyElement = if link_ranges.is_empty() {
                styled.into_any_element()
            } else {
                let click_routes = link_routes;
                let click_ids = link_ids;
                let click_links = ctx.links.clone();
                let click_recall = ctx.targets.recall();
                let click_leaving = leaving.clone();
                InteractiveText::new(
                    ElementId::Name(SharedString::from(format!(
                        "source-links-{number}-{piece_index}"
                    ))),
                    styled,
                )
                .on_click(link_ranges, move |which, _, app| {
                    if let (Some(target_route), Some(id)) =
                        (click_routes.get(which), click_ids.get(which))
                    {
                        click_recall.focus(id.clone());
                        click_recall.remember_leave(click_leaving.clone(), id.clone());
                        click_links.dispatch(Intent::Navigate(target_route.clone()), app);
                    }
                })
                .into_any_element()
            };
            visual_lines = visual_lines.child(
                div()
                    .flex()
                    .items_start()
                    .gap(gap)
                    .child(line_number)
                    .child(
                        div()
                            .set(ty::CODE, &measure)
                            .text_color(palette.ink1.hsla())
                            .min_w(px(0.0))
                            .whitespace_nowrap()
                            .child(body),
                    ),
            );
        }
        let requested = requested_line == Some(number);
        let source_row = div()
            .flex()
            .items_start()
            .border_l_2()
            .border_color(if requested {
                palette.peri.line.hsla()
            } else {
                palette.line1.hsla()
            })
            .child(visual_lines);
        let shared_row = facet::motion::shared::shared(ElementId::Name(id.clone()), source_row);
        rows = rows.child(ctx.targets.track(id, shared_row));
        if page.lines[source_index].more_in_line {
            rows = rows.child(quiet(
                format!("Line {number} continues on the next source page"),
                &measure,
                palette,
            ));
        }
    }
    column = column.child(rows);
    if route.line.is_some() && requested_line.is_none() {
        column = column.child(quiet(
            format!(
                "Line {} is outside the source text this reader has",
                route.line.unwrap_or_default()
            ),
            &measure,
            palette,
        ));
    }
    if page.next.is_some() {
        let continuation = if page.next.is_some_and(|next| next.line == rendered_to) {
            format!("Line {rendered_to} continues")
        } else {
            format!("{below} later lines")
        };
        column = column.child(quiet(continuation, &measure, palette));
        column = column.child(pager_controls(
            "bottom", &pager, cursor, &page, source, ctx, cx,
        ));
    }
    if let Some(editor_path) = view.editor_path.known() {
        let path: Arc<str> = Arc::clone(editor_path);
        let line = route
            .line
            .or_else(|| declaration.map(|span| span.first))
            .unwrap_or(first_line);
        let links = ctx.links.clone();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            links.dispatch(
                Intent::OpenSource {
                    path: Arc::clone(&path),
                    line,
                },
                app,
            );
        });
        let id: SharedString = "source-open-editor".into();
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Open source file in editor".into(),
            act: act.clone(),
            peek: None,
            source: None,
        });
        let button = div()
            .id(id.clone())
            .cursor_pointer()
            .text_color(palette.peri.base.hsla())
            .child(text(ty::SMALL, &measure, palette.peri.base).child("Open in editor"))
            .on_click(move |_: &ClickEvent, window, app| act(window, app));
        column = column.child(ctx.targets.track(id, button));
    }
    if !visible_references.is_empty() {
        let reference_count = visible_references.len();
        let last_reference_page = (reference_count - 1) / MAX_PAGE_REFERENCES;
        let reference_page = pager.read(cx).reference_page.min(last_reference_page);
        let reference_start = reference_page * MAX_PAGE_REFERENCES;
        column = column.child(text(ty::MONO_SMALL, &measure, palette.ink3).child(ctx.say(
            format!(
                "References on these lines · {}–{} of {reference_count}",
                reference_start + 1,
                (reference_start + MAX_PAGE_REFERENCES).min(reference_count)
            ),
        )));
        let mut reference_controls = div().flex().flex_wrap().gap(measure.space(Space::Base));
        for (direction, destination, label) in [
            (
                "previous",
                reference_page.checked_sub(1),
                "Previous references",
            ),
            (
                "next",
                (reference_page < last_reference_page).then_some(reference_page + 1),
                "Next references",
            ),
        ] {
            if let Some(destination) = destination {
                let id: SharedString = format!("source-references-{direction}").into();
                let state = pager.clone();
                let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
                    state.update(app, |pager, cx| pager.show_references(destination, cx));
                });
                ctx.targets.push(Target {
                    id: id.clone(),
                    label: label.into(),
                    act: act.clone(),
                    peek: None,
                    source: None,
                });
                reference_controls = reference_controls.child(
                    ctx.targets.track(
                        id.clone(),
                        facet::controls::button(id, label, &measure)
                            .ghost()
                            .size(Control::Small)
                            .on_click(move |window, app| act(window, app)),
                    ),
                );
            }
        }
        column = column.child(reference_controls);
        for (index, (symbol, target_route)) in visible_references
            .into_iter()
            .enumerate()
            .skip(reference_start)
            .take(MAX_PAGE_REFERENCES)
        {
            let id: SharedString = format!("source-reference-{index}").into();
            let label: SharedString = symbol.identity().name().to_owned().into();
            let links = ctx.links.clone();
            let recall = ctx.targets.recall();
            let leaving = leaving.clone();
            let destination = target_route.clone();
            let target_id = id.clone();
            let target = Target {
                id: id.clone(),
                label: label.clone(),
                act: Rc::new(move |_, app| {
                    recall.focus(target_id.clone());
                    recall.remember_leave(leaving.clone(), target_id.clone());
                    links.dispatch(Intent::Navigate(destination.clone()), app);
                }),
                peek: Some(PageKey::Symbol(symbol.clone())),
                source: Some(symbol),
            };
            if restore_target.as_ref() == Some(&id) {
                ctx.targets.focus(id.clone());
            }
            let act = target.act.clone();
            ctx.targets.push(target);
            let row = div()
                .id(id.clone())
                .cursor_pointer()
                .text_color(palette.peri.base.hsla())
                .child(text(ty::PROSE, &measure, palette.peri.base).child(label))
                .on_click(move |_: &ClickEvent, window, app| act(window, app));
            column = column.child(ctx.targets.track(id, row));
        }
    }
    let snippet = shown.clone();
    let copy_id: SharedString = "source-copy-excerpt".into();
    let copy: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
        app.write_to_clipboard(gpui::ClipboardItem::new_string(snippet.to_string()));
    });
    ctx.targets.push(Target {
        id: copy_id.clone(),
        label: "Copy visible source excerpt".into(),
        act: copy.clone(),
        peek: None,
        source: None,
    });
    let copy_button = div()
        .id(copy_id.clone())
        .cursor_pointer()
        .text_color(palette.peri.base.hsla())
        .child(text(ty::SMALL, &measure, palette.peri.base).child("Copy excerpt"))
        .on_click(move |_: &ClickEvent, window, app| copy(window, app));
    column = column.child(ctx.targets.track(copy_id, copy_button));
    column.into_any_element()
}

fn pager_controls(
    position: &str,
    pager: &Entity<Pager>,
    cursor: SourceCursor,
    page: &SourcePage,
    source: &SourceText,
    ctx: &mut Ctx<'_>,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let first = page.lines.first().map_or(cursor.line, |line| line.number);
    let last = page.lines.last().map_or(cursor.line, |line| line.number);
    let total_last = source
        .first_line
        .saturating_add(source.line_count().saturating_sub(1) as u32);
    let range = if source.line_count() == 0 {
        "No source lines were recorded".to_owned()
    } else if first == last
        && page
            .lines
            .first()
            .is_some_and(|line| line.continued || line.more_in_line)
    {
        format!("Line {first} in parts · last line {total_last}")
    } else {
        format!("Lines {first}–{last} of {total_last}")
    };
    let mut controls = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(measure.space(Space::Base))
        .min_w_0()
        .child(quiet(ctx.say(range), &measure, palette));

    if let Some(previous) = pager
        .read(cx)
        .back
        .last()
        .copied()
        .or_else(|| previous_cursor(source, cursor))
    {
        let id: SharedString = format!("source-page-{position}-previous").into();
        let state = pager.clone();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            state.update(app, |pager, cx| pager.previous(previous, cx));
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Previous source lines".into(),
            act: act.clone(),
            peek: None,
            source: None,
        });
        controls = controls.child(
            ctx.targets.track(
                id.clone(),
                facet::controls::button(id, "Previous lines", &measure)
                    .ghost()
                    .size(Control::Small)
                    .on_click(move |window, app| act(window, app)),
            ),
        );
    }
    if let Some(next) = pager.read(cx).forward.last().copied().or(page.next) {
        let id: SharedString = format!("source-page-{position}-next").into();
        let state = pager.clone();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            state.update(app, |pager, cx| pager.next(next, cx));
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Next source lines".into(),
            act: act.clone(),
            peek: None,
            source: None,
        });
        controls = controls.child(
            ctx.targets.track(
                id.clone(),
                facet::controls::button(id, "Next lines", &measure)
                    .ghost()
                    .size(Control::Small)
                    .on_click(move |window, app| act(window, app)),
            ),
        );
    }
    if position == "top" && source.line_count() > 0 {
        let input = pager.read(cx).input.clone();
        let error = pager.read(cx).error.clone();
        let field_id: SharedString = "source-jump-field".into();
        let focus_input = input.clone();
        let focus: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, app| {
            focus_input.update(app, |input, cx| input.focus(window, cx));
        });
        ctx.targets.push(Target {
            id: field_id.clone(),
            label: "Enter a source line number".into(),
            act: focus.clone(),
            peek: None,
            source: None,
        });
        let mut field = facet::controls::field(field_id.clone(), &input, &measure).quiet();
        if let Some(error) = error {
            field = field.fault(error);
        }
        controls = controls.child(
            ctx.targets.track(
                field_id.clone(),
                div()
                    .id(field_id)
                    .w(px(112.0 * measure.scale()))
                    .min_w_0()
                    .on_click(move |_: &ClickEvent, window, app| focus(window, app))
                    .child(field),
            ),
        );
        let id: SharedString = "source-jump-go".into();
        let state = pager.clone();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            let typed = input.read(app).value().to_string();
            state.update(app, |pager, cx| pager.jump(&typed, cx));
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Go to source line".into(),
            act: act.clone(),
            peek: None,
            source: None,
        });
        controls = controls.child(
            ctx.targets.track(
                id.clone(),
                facet::controls::button(id, "Go to line", &measure)
                    .ghost()
                    .size(Control::Small)
                    .on_click(move |window, app| act(window, app)),
            ),
        );
    }
    controls.into_any_element()
}

fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &text[..end]
}

/// The producer's language is authoritative. JavaScript is the TypeScript
/// frontend's dialect, while C has its own grammar; unknown stays unstyled.
fn code_language(view: &SourceView) -> Option<facet::code::Lang> {
    use backend_present::Language;
    use facet::code::Lang;

    Some(match view.symbol.language {
        Language::Rust => Lang::Rust,
        Language::TypeScript => {
            let javascript = view.symbol.path.as_deref().is_some_and(|path| {
                [".js", ".jsx", ".mjs", ".cjs"]
                    .iter()
                    .any(|suffix| path.ends_with(suffix))
            });
            if javascript {
                Lang::JavaScript
            } else {
                Lang::TypeScript
            }
        }
        Language::Python => Lang::Python,
        Language::Go => Lang::Go,
        Language::Java => Lang::Java,
        Language::CSharp => Lang::CSharp,
        Language::C => Lang::C,
        Language::Cxx => Lang::Cpp,
        Language::Unknown => return None,
    })
}

fn margin(
    view: &SourceView,
    store: &super::Pages,
    symbol: &SymbolRef,
    route: &SymbolRoute,
    ctx: &mut Ctx<'_>,
) -> Option<gpui::AnyElement> {
    let page = store.symbol(symbol);
    let page = page.loaded_value()?;
    let measure = ctx.note;
    let palette = ctx.palette;
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base));
    let name = ctx.say(view.symbol.name.to_string());
    column = column.child(text(ty::MONO_ROW, &measure, palette.ink0).child(name));
    let prose = DocFragment::plain_text(&page.docs);
    if let Some(sentence) = prose
        .split_terminator(['.', '\n'])
        .map(str::trim)
        .find(|line| !line.is_empty())
    {
        let sentence = ctx.say(format!("{sentence}."));
        column = column.child(text(ty::MARGIN, &measure, palette.ink2).child(sentence));
    }
    if let Some(callers) = page.rose.left.known().filter(|callers| !callers.is_empty()) {
        column = column.child(
            text(ty::SMALL, &measure, palette.ink3)
                .pt(measure.space(Space::Base))
                .child("Called by"),
        );
        for caller in callers.iter().take(5) {
            let name: SharedString = ctx.say(caller.decl.name.to_string());
            let target = symbol_route(route.package.as_str(), &caller.decl.coordinate);
            let links = ctx.links.clone();
            column = column.child(
                div()
                    .id(SharedString::from(format!(
                        "caller-{}",
                        caller.decl.coordinate
                    )))
                    .child(text(ty::MONO_SMALL, &measure, palette.ink1).child(name))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        if let Some(route) = target.clone() {
                            links.dispatch(Intent::Navigate(route), cx);
                        }
                    }),
            );
        }
        if callers.len() > 5 {
            let more = ctx.say(format!("and {} more", callers.len() - 5));
            column = column.child(quiet(more, &measure, palette));
        }
    }
    Some(column.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pages::{Known, LineSpan, PageValue, ReadFailure};
    use crate::navigation::View;
    use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
    use gpui::TestAppContext;

    fn text(words: String) -> SourceText {
        SourceText::new(words.into(), 1, SourceOrigin::LocalFile, true)
    }

    #[test]
    fn every_line_is_reachable_without_growing_a_page() {
        let source = text((1..=600).map(|line| format!("line {line}\n")).collect());
        let mut cursor = SourceCursor { line: 1, byte: 0 };
        let mut visited = Vec::new();
        loop {
            let page = SourcePage::at(&source, cursor);
            assert!(!page.lines.is_empty());
            assert!(page.lines.len() <= MAX_SOURCE_LINES);
            assert!(
                page.lines
                    .iter()
                    .map(|line| line.span.end - line.span.start)
                    .sum::<u32>()
                    <= MAX_SOURCE_BYTES as u32
            );
            visited.extend(page.lines.iter().map(|line| line.number));
            let Some(next) = page.next else { break };
            assert!(next.line > cursor.line || next.line == cursor.line && next.byte > cursor.byte);
            cursor = next;
        }
        assert_eq!(visited, (1..=600).collect::<Vec<_>>());
        assert_eq!(cursor.line, 577);
        let mut backward = cursor;
        let mut steps = 0;
        while let Some(previous) = previous_cursor(&source, backward) {
            assert!(
                previous.line < backward.line
                    || previous.line == backward.line && previous.byte < backward.byte
            );
            backward = previous;
            steps += 1;
            assert!(steps < 600);
        }
        assert_eq!(backward, SourceCursor { line: 1, byte: 0 });
    }

    #[test]
    fn a_long_utf8_line_is_paged_without_omitting_a_character() {
        let original = format!("{}\ntail", "é".repeat(80_000));
        let source = text(original);
        let mut cursor = SourceCursor { line: 1, byte: 0 };
        let mut reconstructed = String::new();
        let mut pages = 0;
        loop {
            let page = SourcePage::at(&source, cursor);
            assert!(!page.lines.is_empty());
            for line in &page.lines {
                assert!(
                    usize::try_from(line.span.end - line.span.start).unwrap()
                        <= MAX_SOURCE_LINE_BYTES
                );
                reconstructed.push_str(source.text().get(line.span.range()).unwrap());
                if !line.more_in_line && line.number == 1 {
                    reconstructed.push('\n');
                }
            }
            pages += 1;
            assert!(pages < 100);
            let Some(next) = page.next else { break };
            assert!(next.line > cursor.line || next.line == cursor.line && next.byte > cursor.byte);
            cursor = next;
        }
        assert_eq!(reconstructed, source.text());
    }

    #[test]
    fn an_exact_deep_route_survives_an_oversized_predecessor() {
        let source = text(format!(
            "{}\n{}",
            "x".repeat(100_000),
            (2..=500)
                .map(|line| format!("line {line}\n"))
                .collect::<String>()
        ));
        let cursor = initial_cursor(&source, 450, 0);
        assert_eq!(cursor, SourceCursor { line: 450, byte: 0 });
        assert_eq!(
            SourcePage::at(&source, cursor)
                .lines
                .first()
                .map(|line| line.number),
            Some(450)
        );
    }

    #[test]
    fn only_verified_excerpt_bytes_admit_links() {
        let span = ByteSpan { start: 10, end: 15 };
        let visible = ByteSpan { start: 0, end: 20 };
        assert!(!verified_link(SourceCoverage::Unverified, span, visible));
        assert!(verified_link(
            SourceCoverage::CapturedExcerpt,
            span,
            visible
        ));
        assert!(!verified_link(
            SourceCoverage::CapturedExcerpt,
            span,
            ByteSpan { start: 12, end: 20 }
        ));
        assert!(verified_link(
            SourceCoverage::LiveFileExcerptVerified {
                bytes: ByteSpan { start: 8, end: 17 }
            },
            span,
            visible
        ));
        assert!(!verified_link(
            SourceCoverage::LiveFileExcerptVerified {
                bytes: ByteSpan { start: 11, end: 17 }
            },
            span,
            visible
        ));
    }

    struct LongSource;

    impl PageReader for LongSource {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            let mut fixture = crate::shell::tests::Fixture;
            let mut value = fixture.read(request, context)?;
            if let PageValue::Source(view) = &mut value {
                view.text = Known::Known(text(
                    (1..=600).map(|line| format!("line {line}\n")).collect(),
                ));
                view.declaration = Known::Known(LineSpan {
                    first: 500,
                    last: 500,
                });
            }
            Ok(value)
        }
    }

    #[gpui::test]
    fn narrow_native_reader_opens_an_exact_deep_line(cx: &mut TestAppContext) {
        let Route::Symbol(mut route) = crate::shell::tests::view_route("RelationLabel", View::Code)
        else {
            unreachable!()
        };
        route.line = Some(500);
        let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
        let mut rig =
            crate::shell::tests::rig_with_reads(cx, Some(Route::Symbol(route)), 260.0, 700.0, pool);
        let words = rig.said();
        assert!(
            words.iter().any(|word| word.contains("line 500")),
            "deep route not visible: {words:#?}"
        );
        assert!(
            words.iter().any(|word| word.contains("Lines 500")),
            "pager not visible: {words:#?}"
        );
        let (zone, focus) = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        assert_eq!(zone, crate::shell::focus::Zone::Reader);
        assert_eq!(focus.as_deref(), Some("source-line-500"));
        rig.keys("j");
        let (_, walked) = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        assert_eq!(walked.as_deref(), Some("source-line-501"));
    }
}
