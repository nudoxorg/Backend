//! Source, plain: the file's crumb, the text around the declaration with its
//! lines numbered (the declaration's numbers lit mint), and the
//! declaration's one sentence and callers in the margin.

pub(crate) mod paging;

#[cfg(test)]
use paging::{MAX_SOURCE_BYTES, MAX_SOURCE_LINE_BYTES, MAX_SOURCE_LINES, previous_cursor};
use paging::{PagingState, SourceCursor, SourcePage, initial_cursor, verified_link};

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
#[cfg(test)]
use crate::model::pages::ByteSpan;
use crate::model::pages::{
    DocFragment, PageKey, SourceCoverage, SourceOrigin, SourceText, SourceView, SymbolRef,
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
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

const CONTEXT_BEFORE: u32 = 24;
const MAX_PAGE_REFERENCES: usize = 32;

struct Pager {
    state: Rc<RefCell<Option<PagingState>>>,
    first: u32,
    last: u32,
    input: Entity<InputState>,
    error: Option<SharedString>,
    recall: Recall,
    reveal: Rc<Cell<bool>>,
    _subscription: Subscription,
}

impl Pager {
    fn new(
        state: Rc<RefCell<Option<PagingState>>>,
        first: u32,
        last: u32,
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
            state,
            first,
            last,
            input,
            error: None,
            recall,
            reveal,
            _subscription: subscription,
        }
    }

    fn focus_line(&mut self, line: u32, cx: &mut Context<Self>) {
        self.error = None;
        self.recall
            .focus(crate::shell::reader::source_line_shared_id(line));
        self.reveal.set(true);
        cx.notify();
    }

    fn next(&mut self, fallback: SourceCursor, cx: &mut Context<Self>) {
        let line = self
            .state
            .borrow_mut()
            .as_mut()
            .map(|state| state.next(fallback).line);
        if let Some(line) = line {
            self.focus_line(line, cx);
        }
    }

    fn previous(&mut self, fallback: SourceCursor, cx: &mut Context<Self>) {
        let line = self
            .state
            .borrow_mut()
            .as_mut()
            .map(|state| state.previous(fallback).line);
        if let Some(line) = line {
            self.focus_line(line, cx);
        }
    }

    fn jump(&mut self, text: &str, cx: &mut Context<Self>) {
        match text.trim().parse::<u32>() {
            Ok(line) if (self.first..=self.last).contains(&line) => {
                let changed = self.state.borrow_mut().as_mut().is_some_and(|state| {
                    state.jump(line);
                    true
                });
                if changed {
                    self.focus_line(line, cx);
                }
            }
            _ => {
                self.error =
                    Some(format!("Enter a line from {} to {}", self.first, self.last).into());
                cx.notify();
            }
        }
    }

    fn show_references(&mut self, page: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.state.borrow_mut().as_mut() {
            state.reference_page = page;
        }
        self.recall.focus(format!(
            "source-reference-{}",
            page.saturating_mul(MAX_PAGE_REFERENCES)
        ));
        self.reveal.set(true);
        cx.notify();
    }
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
    let Some(range) = source.line_range() else {
        let words = if source.line_count() == 0 {
            "No source lines were recorded".to_owned()
        } else {
            "Source line numbers are invalid; this text cannot be navigated safely".to_owned()
        };
        return quiet(ctx.say(words), &measure, palette).into_any_element();
    };
    let first_line = range.first;
    let last_line = range.last;
    let total = last_line - first_line + 1;
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
    let paging = Rc::clone(&ctx.source_paging);
    {
        let mut saved = paging.borrow_mut();
        let state = saved.get_or_insert_with(|| PagingState::new(initial));
        if restore_target.is_some() && state.restored_focus_place != Some(ctx.place_key) {
            state.reference_page = restored_reference_page;
        }
    }
    let recall = ctx.targets.recall();
    let reveal = Rc::clone(&ctx.reader_reveal);
    let pager_memory = Rc::clone(&paging);
    let pager = window.use_keyed_state(pager_key, cx, move |window, cx| {
        Pager::new(
            pager_memory,
            first_line,
            last_line,
            recall,
            reveal,
            window,
            cx,
        )
    });
    let cursor = paging
        .borrow()
        .as_ref()
        .map_or(initial, |state| state.cursor);
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
            label: if page.lines[source_index].continued || page.lines[source_index].more_in_line {
                format!("Copy visible part of source line {number}")
            } else {
                format!("Copy source line {number}")
            }
            .into(),
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
                .id(format!("source-copy-line-{number}-{piece_index}"))
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
        let reference_page = paging
            .borrow()
            .as_ref()
            .map_or(0, |state| state.reference_page)
            .min(last_reference_page);
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
                let apply = paging.borrow_mut().as_mut().is_some_and(|state| {
                    if state.restored_focus_place == Some(ctx.place_key) {
                        return false;
                    }
                    state.restored_focus_place = Some(ctx.place_key);
                    true
                });
                if apply {
                    ctx.targets.focus(id.clone());
                }
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
        label: "Copy visible source page".into(),
        act: copy.clone(),
        peek: None,
        source: None,
    });
    let copy_button = div()
        .id(copy_id.clone())
        .cursor_pointer()
        .text_color(palette.peri.base.hsla())
        .child(text(ty::SMALL, &measure, palette.peri.base).child("Copy visible page"))
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
    let total_last = source.line_range().map_or(cursor.line, |range| range.last);
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

    let memory = Rc::clone(&pager.read(cx).state);
    let previous = memory
        .borrow()
        .as_ref()
        .and_then(|state| state.previous_target(source));
    if let Some(previous) = previous {
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
    let next = memory
        .borrow()
        .as_ref()
        .and_then(|state| state.next_target(page));
    if let Some(next) = next {
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
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::model::pages::{
        IdentifierSpan, Known, LineSpan, PageValue, Provenance, ReadFailure, SymbolLink,
    };
    use crate::navigation::View;
    use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
    use gpui::TestAppContext;

    fn text(words: String) -> SourceText {
        SourceText::new(words.into(), 1, SourceOrigin::LocalFile, true)
            .expect("valid test source lines")
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

    #[test]
    fn mutated_invalid_line_range_never_publishes_duplicate_row_ids() {
        let boundary = SourceText::new(Arc::from("last"), u32::MAX, SourceOrigin::Excerpt, true)
            .expect("one final line is representable");
        let last = SourcePage::at(
            &boundary,
            SourceCursor {
                line: u32::MAX,
                byte: 0,
            },
        );
        assert_eq!(last.lines.len(), 1);
        assert_eq!(last.lines[0].number, u32::MAX);
        assert!(last.next.is_none());

        let mut source = text("one\ntwo".to_owned());
        for invalid_first in [0, u32::MAX] {
            source.first_line = invalid_first;
            assert!(source.line_range().is_none());
            let cursor = SourceCursor {
                line: invalid_first,
                byte: 0,
            };
            assert!(SourcePage::at(&source, cursor).lines.is_empty());
            assert!(previous_cursor(&source, cursor).is_none());
        }
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
                let words: String = (1..=600)
                    .map(|line| {
                        if line == 565 {
                            "RelationDirection\n".to_owned()
                        } else {
                            format!("line {line}\n")
                        }
                    })
                    .collect();
                let source = text(words);
                let span = source.line_span(565).expect("verified reference line");
                view.text = Known::Known(source.with_verified_local_excerpt(span));
                view.declaration = Known::Known(LineSpan {
                    first: 565,
                    last: 565,
                });
                view.identifiers = Known::Known(Arc::from([IdentifierSpan {
                    span,
                    link: SymbolLink {
                        target: crate::shell::tests::symbol("RelationDirection"),
                        provenance: Provenance::ByName,
                    },
                }]));
            }
            Ok(value)
        }
    }

    fn activate_reader_target(rig: &mut crate::shell::tests::Rig, id: &str) {
        let target = rig.shell.read_with(rig.cx, |shell, cx| {
            let targets = shell.reader_targets(cx);
            targets.focus(id);
            targets.current().expect("visible source target")
        });
        rig.cx.update(|window, cx| (target.act)(window, cx));
        rig.settle();
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

    #[gpui::test]
    fn native_jump_next_link_and_back_restore_the_same_page(cx: &mut TestAppContext) {
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
        let mut rig =
            crate::shell::tests::rig_with_reads(cx, Some(route.clone()), 320.0, 700.0, pool);

        activate_reader_target(&mut rig, "source-jump-field");
        rig.keys("5 0 0 enter");
        assert!(
            rig.said().iter().any(|word| word.contains("Lines 500")),
            "Enter must jump to the exact line"
        );
        activate_reader_target(&mut rig, "source-page-top-next");
        let later = rig.said();
        assert!(
            later.iter().any(|word| word.contains("Lines 564")),
            "later page missing: {later:#?}"
        );
        assert!(
            later.iter().any(|word| word.contains("RelationDirection")),
            "verified link line missing: {later:#?}"
        );

        rig.shell.update(rig.cx, |shell, cx| {
            shell.set_source_reader_scroll_offset(gpui::point(px(0.0), px(-96.0)), cx)
        });
        rig.repaint();
        let before = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx));
        activate_reader_target(&mut rig, "source-reference-0");
        assert_ne!(rig.route(), route, "the verified link must navigate");
        rig.keys("cmd-[");
        assert_eq!(rig.route(), route);
        assert!(
            rig.said().iter().any(|word| word.contains("Lines 564")),
            "Back reset the source page"
        );
        let (_, focus) = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.focus_state(cx));
        assert_eq!(
            focus.as_deref(),
            Some("source-reference-0"),
            "Back lost the exact link focus"
        );
        let restored = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.source_reader_scroll_offset(cx));
        assert_eq!(
            restored, before,
            "Back lost the source page's scroll offset"
        );
    }

    struct LongWrappedLine;

    impl PageReader for LongWrappedLine {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            let mut fixture = crate::shell::tests::Fixture;
            let mut value = fixture.read(request, context)?;
            if let PageValue::Source(view) = &mut value {
                view.text = Known::Known(text(format!(
                    "{}{}{}\ntail",
                    "α".repeat(1_024),
                    "β".repeat(1_024),
                    "γ".repeat(1_024)
                )));
                view.declaration = Known::Known(LineSpan { first: 1, last: 1 });
            }
            Ok(value)
        }
    }

    #[gpui::test]
    fn native_wrapped_line_continues_without_duplicate_row_ids(cx: &mut TestAppContext) {
        let route = crate::shell::tests::view_route("RelationLabel", View::Code);
        let pool = ReadPool::start(2, |_| LongWrappedLine).expect("wrapped source pool");
        let mut rig = crate::shell::tests::rig_with_reads(cx, Some(route), 260.0, 700.0, pool);
        assert!(
            rig.said()
                .iter()
                .any(|word| word.contains("Line 1 in parts"))
        );
        let before = rig.said();
        activate_reader_target(&mut rig, "source-page-top-next");
        let after = rig.said();
        assert_ne!(
            before, after,
            "the next page must advance within the same long line"
        );
        assert!(after.iter().any(|word| word.contains("Line 1 in parts")));
    }
}
