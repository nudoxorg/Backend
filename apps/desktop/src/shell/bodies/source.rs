//! Source, plain: the file's crumb, the text around the declaration with its
//! lines numbered (the declaration's numbers lit mint), and the
//! declaration's one sentence and callers in the margin.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::pages::{DocFragment, PageKey, SourceCoverage, SourceOrigin, SourceView, SymbolRef};
use crate::navigation::{Intent, Route, SymbolRoute};
use crate::shell::focus::Target;
use crate::shell::kit::{gap_words, quiet, symbol_route, text};
use crate::shell::reader::Reader;
use facet::tokens::ty;
use facet::{Set as _, Space};
use gpui::{
    App, AppContext as _, ClickEvent, Context, ElementId, InteractiveElement, InteractiveText,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledText,
    Window, div, px,
};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

/// Lines shown before and after the declaration.
const CONTEXT_BEFORE: u32 = 24;
const CONTEXT_AFTER: u32 = 240;
const ROUTE_LINE_CONTEXT: u32 = 72;
const MAX_SOURCE_LINES: usize = 256;
const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_SOURCE_LINE_BYTES: usize = 8 * 1024;

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
    source: &crate::model::pages::SourceText,
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
    let (from, to) = requested_line.map_or_else(
        || {
            declaration.map_or(
                (
                    first_line,
                    last_line.min(first_line.saturating_add(CONTEXT_AFTER)),
                ),
                |span| {
                    (
                        span.first.saturating_sub(CONTEXT_BEFORE).max(first_line),
                        span.last.saturating_add(CONTEXT_AFTER).min(last_line),
                    )
                },
            )
        },
        |line| {
            (
                line.saturating_sub(ROUTE_LINE_CONTEXT).max(first_line),
                line.saturating_add(ROUTE_LINE_CONTEXT).min(last_line),
            )
        },
    );
    let mut shown = String::new();
    let mut numbers = Vec::new();
    let mut raw_lines = Vec::new();
    let mut source_starts = Vec::new();
    let mut clipped = BTreeSet::new();
    let first_index = usize::try_from(from.saturating_sub(first_line)).unwrap_or(usize::MAX);
    let wanted_lines = usize::try_from(to.saturating_sub(from).saturating_add(1))
        .unwrap_or(MAX_SOURCE_LINES)
        .min(MAX_SOURCE_LINES);
    let line_spans = source.line_spans_in(first_index, wanted_lines);
    for (source_index, span) in line_spans.iter().copied().enumerate() {
        if numbers.len() >= MAX_SOURCE_LINES {
            break;
        }
        let number = from.saturating_add(u32::try_from(source_index).unwrap_or(u32::MAX));
        let line = source.text().get(span.range()).unwrap_or_default();
        let visible = utf8_prefix(line, MAX_SOURCE_LINE_BYTES);
        if shown.len().saturating_add(visible.len()).saturating_add(1) > MAX_SOURCE_BYTES {
            break;
        }
        if visible.len() < line.len() {
            clipped.insert(numbers.len());
        }
        source_starts.push(usize::try_from(span.start).unwrap_or_default());
        numbers.push(number);
        // Line copy follows the bounded visible excerpt, so a single enormous
        // source line cannot be retained again by a keyboard target.
        raw_lines.push(visible.to_owned());
        shown.push_str(visible);
        shown.push('\n');
    }
    let shown = shown.trim_end_matches('\n').to_owned();
    ctx.say(shown.clone());
    let language = code_language(view);
    let shown: SharedString = shown.into();
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
    if above > 0 {
        column = column.child(quiet(format!("{above} lines above"), &measure, palette));
    }
    let mut rows = div().flex().flex_col();
    let mut visible_references = Vec::<(SymbolRef, Route)>::new();
    let leaving = Route::Symbol(route.clone());
    let restore_target = ctx.targets.left_by(&leaving);
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
        let raw_line: Arc<str> = Arc::from(raw_lines[source_index].clone());
        let copy_line_text = Arc::clone(&raw_line);
        let copy_line: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            app.write_to_clipboard(gpui::ClipboardItem::new_string(copy_line_text.to_string()));
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
                    if clipped.contains(&source_index)
                        && end > line_start.saturating_add(raw_lines[source_index].len())
                    {
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
                    let target_index = visible_references
                        .iter()
                        .position(|(seen, _)| seen == &target)
                        .unwrap_or_else(|| {
                            visible_references.push((target.clone(), target_route.clone()));
                            visible_references.len() - 1
                        });
                    link_ranges.push(text_from..text_to);
                    link_routes.push(target_route.clone());
                    link_ids.push(SharedString::from(format!(
                        "source-reference-{target_index}"
                    )));
                }
            }
            let label = if piece_index == 0 {
                number.to_string()
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
        if clipped.contains(&source_index) {
            rows = rows.child(quiet(
                format!("Line {number} clipped after {MAX_SOURCE_LINE_BYTES} bytes"),
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
    if rendered_to < to {
        column = column.child(quiet(
            format!("Source display is bounded at line {rendered_to}"),
            &measure,
            palette,
        ));
    }
    if below > 0 {
        column = column.child(quiet(format!("{below} lines below"), &measure, palette));
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
        column = column.child(
            text(ty::MONO_SMALL, &measure, palette.ink3).child("References in this excerpt"),
        );
        for (index, (symbol, target_route)) in visible_references.into_iter().take(32).enumerate() {
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
