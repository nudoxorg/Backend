//! Current Cargo file bytes with no inferred indexed-symbol links.

use super::{Pager, PagingState, SourceCursor, SourcePage, initial_cursor, pager_controls};
use crate::model::pages::{CargoSourceKey, PageKey, PackageRef, SourceText};
use crate::navigation::CargoSourceRoute;
use crate::shell::bodies::state::{Shown, not_ready, shown};
use crate::shell::bodies::{Ctx, Leaf, Pages};
use crate::shell::focus::Target;
use crate::shell::kit::{quiet, text};
use crate::shell::reader::Reader;
use facet::{Space, tokens::ty};
use gpui::{
    App, AppContext as _, ClickEvent, Context, ElementId, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledText, Window, div, px,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::rc::Rc;

pub(super) fn body(
    route: &CargoSourceRoute,
    _store: &Pages,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let Ok(package) = PackageRef::parse(route.package.as_str()) else {
        return vec![Leaf::new(quiet("This Cargo source address is invalid.", &ctx.measure, ctx.palette))];
    };
    let key = CargoSourceKey { project: route.project.clone(), package, file: route.file.clone() };
    let page_key = PageKey::CargoSource(key.clone());
    // Transition plates may retain a captured Pages snapshot. Source bytes
    // must always come from the live store after its owner revocation fence.
    let live = ctx.links.store.read(cx);
    let resource = live.cargo_source(&key);
    let checking = live.is_loading(&page_key);
    drop(live);
    // A formerly valid file cannot be painted as current while the owner is
    // checking a newer observation or a forced revalidation is in flight.
    if checking
        || resource.value_root().is_some_and(|root| root != ctx.links.snapshot(cx).key())
    {
        let status = ctx.say("Checking the current Cargo source and file bytes…");
        return vec![Leaf::new(quiet(status, &ctx.measure, ctx.palette))];
    }
    let page = match shown(&resource) {
        Shown::Ready(page) => page.clone(),
        other => return not_ready(&other, &page_key, route.file.as_str(), ctx, cx),
    };
    let heading = ctx.say(format!("{}  ›  {}", route.package.as_str(), route.file.as_str()));
    let mut leaves = vec![Leaf::new(text(ty::MONO_ROW, &ctx.measure, ctx.palette.ink2).child(heading))];
    let status = ctx.say("Current Cargo file · source bytes checked by the owner. Declaration links are unavailable until this exact file is indexed.");
    leaves.push(Leaf::new(quiet(status, &ctx.measure, ctx.palette)));
    leaves.push(Leaf::new(code(&page.source, route, ctx, window, cx)));
    leaves
}

fn code(
    source: &SourceText,
    route: &CargoSourceRoute,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> gpui::AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let Some(range) = source.line_range() else {
        return quiet("This source file has invalid line numbers.", &measure, palette).into_any_element();
    };
    let requested = route.line.filter(|line| (range.first..=range.last).contains(line));
    let initial = requested.map_or(SourceCursor { line: range.first, byte: 0 }, |line| initial_cursor(source, line, 0));
    let pager_key: ElementId = format!(
        "cargo-source-pager-{}-{}-{:?}-{:?}",
        ctx.place_key, route.file.as_str(), route.line, ctx.source_generation
    ).into();
    let paging = Rc::clone(&ctx.source_paging);
    {
        let mut saved = paging.borrow_mut();
        saved.get_or_insert_with(|| PagingState::new(initial));
    }
    let recall = ctx.targets.recall();
    let reveal = Rc::clone(&ctx.reader_reveal);
    let memory = Rc::clone(&paging);
    let mut hasher = Sha256::new();
    hasher.update(route.package.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(route.file.as_str().as_bytes());
    let digest = hasher.finalize();
    let digest = digest[..16].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let prefix: Rc<str> = Rc::from(format!("cargo-source-line-{digest}"));
    let row_id: Rc<dyn Fn(u32) -> SharedString> = Rc::new(move |line| format!("{prefix}-{line}").into());
    let painted_row_id = Rc::clone(&row_id);
    let first = range.first;
    let last = range.last;
    let pager = window.use_keyed_state(pager_key, cx, move |window, cx| {
        Pager::new(memory, first, last, recall, reveal, row_id, window, cx)
    });
    let cursor = paging.borrow().as_ref().map_or(initial, |state| state.cursor);
    let page = SourcePage::at(source, cursor);
    let mut shown = String::new();
    let mut numbers = Vec::with_capacity(page.lines.len());
    for line in &page.lines {
        let visible = source.text().get(line.span.range()).unwrap_or_default();
        numbers.push(line.number);
        shown.push_str(visible);
        shown.push('\n');
    }
    if !shown.is_empty() { shown.pop(); }
    let shown: SharedString = shown.into();
    ctx.say(shown.clone());
    let role = measure.role(ty::CODE);
    let digits = numbers.last().map_or(1, |number| number.to_string().len());
    let gap = measure.space(Space::Gutter);
    let number_width = crate::shell::text_fit::text_width(&"0".repeat(digits), &role, cx);
    let columns = crate::shell::text_fit::columns(measure.width() - number_width - gap, &role, cx);
    let highlights = route.file.as_str().ends_with(".rs")
        .then(|| facet::code::highlight(facet::code::Lang::Rust, shown.clone(), window, cx))
        .flatten()
        .map(|highlighted| highlighted.styles(&palette));
    let visual = crate::shell::text_fit::wrap_code(&shown, highlights.as_deref().unwrap_or(&[]), columns);
    let mut grouped = BTreeMap::<usize, Vec<crate::shell::text_fit::CodeLine>>::new();
    for line in visual { grouped.entry(line.source).or_default().push(line); }
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base));
    column = column.child(pager_controls("top", &pager, cursor, &page, source, ctx, cx));
    if cursor.line > range.first || cursor.byte > 0 {
        column = column.child(quiet(format!("{} earlier lines", cursor.line - range.first), &measure, palette));
    }
    let mut rows = div().flex().flex_col();
    for (source_index, number) in numbers.iter().copied().enumerate() {
        let id = painted_row_id(number);
        let focus_key = (ctx.place_key, number, ctx.source_generation);
        if requested == Some(number) && ctx.active && ctx.source_focus_applied.get() != Some(focus_key) {
            ctx.targets.focus(id.clone());
            ctx.reader_reveal.set(true);
            ctx.source_focus_applied.set(Some(focus_key));
        }
        let copy_source = source.clone();
        let span = page.lines[source_index].span;
        let copy: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            let words = copy_source.text().get(span.range()).unwrap_or_default();
            app.write_to_clipboard(gpui::ClipboardItem::new_string(words.to_owned()));
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: format!("Copy visible part of source line {number}").into(),
            act: copy.clone(),
            peek: None,
            source: None,
        });
        let mut pieces = div().flex().flex_col();
        for (piece_index, piece) in grouped.remove(&source_index).unwrap_or_default().into_iter().enumerate() {
            let label = if piece_index == 0 {
                if page.lines[source_index].continued { format!("{number}+") } else { number.to_string() }
            } else { String::new() };
            let gutter_copy = copy.clone();
            let gutter = div()
                .id(format!("cargo-source-copy-line-{number}-{piece_index}"))
                .flex_none().w(number_width).flex().justify_end()
                .child(text(ty::CODE, &measure, if requested == Some(number) { palette.peri.base } else { palette.ink3 }).child(label))
                .cursor_pointer()
                .on_click(move |_: &ClickEvent, window, app| gutter_copy(window, app));
            pieces = pieces.child(div().flex().items_start().gap(gap).child(gutter).child(
                div().set(ty::CODE, &measure).text_color(palette.ink1.hsla()).min_w(px(0.0))
                    .whitespace_nowrap().child(StyledText::new(SharedString::from(piece.text)).with_highlights(piece.runs))
            ));
        }
        let row = div().flex().items_start().border_l_2()
            .border_color(if requested == Some(number) { palette.peri.line.hsla() } else { palette.line1.hsla() })
            .child(pieces);
        let shared = facet::motion::shared::shared(ElementId::Name(id.clone()), row);
        rows = rows.child(ctx.targets.track(id, shared));
    }
    column = column.child(rows);
    if let Some(line) = route.line.filter(|line| !(range.first..=range.last).contains(line)) {
        column = column.child(quiet(format!("Line {line} is outside this file."), &measure, palette));
    }
    if page.next.is_some() {
        column = column.child(quiet(format!("{} later lines", range.last.saturating_sub(numbers.last().copied().unwrap_or(cursor.line))), &measure, palette));
        column = column.child(pager_controls("bottom", &pager, cursor, &page, source, ctx, cx));
    }
    column.into_any_element()
}
