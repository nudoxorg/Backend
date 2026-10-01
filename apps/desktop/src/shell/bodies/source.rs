//! Source, plain: the file's crumb, the text around the declaration with its
//! lines numbered (the declaration's numbers lit mint), and the
//! declaration's one sentence and callers in the margin.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::pages::{DocFragment, PageKey, SourceView, SymbolRef};
use crate::navigation::{Intent, Route, SymbolRoute};
use crate::shell::kit::{gap_words, quiet, symbol_route, text};
use crate::shell::reader::Reader;
use facet::tokens::ty;
use facet::Space;
use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div,
};

/// Lines shown before and after the declaration.
const CONTEXT_BEFORE: u32 = 24;
const CONTEXT_AFTER: u32 = 240;

pub(super) fn body(
    place: &Route,
    route: &SymbolRoute,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
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
    leaves.push(Leaf::new(text(ty::MONO_ROW, &measure, palette.ink2).child(crumb)));
    match view.text.known() {
        Some(source) => {
            let note = margin(&view, store, &symbol, route, ctx);
            let code = code(&view, source, ctx, cx);
            let leaf = Leaf::new(code);
            leaves.push(match note {
                Some(note) => leaf.with_note(note),
                None => leaf,
            });
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
    ctx: &mut Ctx<'_>,
    cx: &gpui::App,
) -> gpui::AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let declaration = view.declaration.known().copied();
    let first_line = source.first_line;
    let total = u32::try_from(source.line_count()).unwrap_or(u32::MAX);
    let last_line = first_line.saturating_add(total.saturating_sub(1));
    let (from, to) = declaration.map_or((first_line, last_line.min(first_line + CONTEXT_AFTER)), |span| {
        (
            span.first.saturating_sub(CONTEXT_BEFORE).max(first_line),
            span.last.saturating_add(CONTEXT_AFTER).min(last_line),
        )
    });
    let mut shown = String::new();
    let mut numbers = Vec::new();
    for (index, line) in source.text.lines().enumerate() {
        let number = first_line + u32::try_from(index).unwrap_or(u32::MAX);
        if number < from || number > to {
            continue;
        }
        numbers.push(number);
        shown.push_str(line);
        shown.push('\n');
    }
    let shown = shown.trim_end_matches('\n').to_owned();
    ctx.say(shown.clone());
    // The number column holds the widest number; the code gets the rest and
    // soft-wraps at token boundaries — continuation lines carry no number.
    let role = measure.role(ty::CODE);
    let digits = numbers.last().map_or(1, |number| number.to_string().len());
    let gap = measure.space(Space::Gutter);
    let number_width = crate::shell::text_fit::text_width(&"0".repeat(digits), &role, cx);
    let columns = crate::shell::text_fit::columns(measure.width() - number_width - gap, &role, cx);
    let lines = crate::shell::text_fit::wrap_code(&shown, &[], columns);
    let above = from.saturating_sub(first_line);
    let below = last_line.saturating_sub(to);
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base));
    if above > 0 {
        column = column.child(quiet(format!("{above} lines above"), &measure, palette));
    }
    let mut rows = div().flex().flex_col();
    for line in lines {
        let number = numbers.get(line.source).copied();
        let lit = number.is_some_and(|number| declaration.is_some_and(|span| span.first <= number && number <= span.last));
        let label = if line.continued { String::new() } else { number.map(|number| number.to_string()).unwrap_or_default() };
        rows = rows.child(
            div()
                .flex()
                .gap(gap)
                .child(
                    text(ty::CODE, &measure, if lit { palette.mint.base } else { palette.ink3 })
                        .flex_none()
                        .w(number_width)
                        .flex()
                        .justify_end()
                        .child(label),
                )
                .child(text(ty::CODE, &measure, palette.ink1).whitespace_nowrap().child(line.text)),
        );
    }
    column = column.child(rows);
    if below > 0 {
        column = column.child(quiet(format!("{below} lines below"), &measure, palette));
    }
    column.into_any_element()
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
    if let Some(sentence) = prose.split_terminator(['.', '\n']).map(str::trim).find(|line| !line.is_empty()) {
        let sentence = ctx.say(format!("{sentence}."));
        column = column.child(text(ty::MARGIN, &measure, palette.ink2).child(sentence));
    }
    if let Some(callers) = page.rose.left.known().filter(|callers| !callers.is_empty()) {
        column = column.child(text(ty::SMALL, &measure, palette.ink3).pt(measure.space(Space::Base)).child("Called by"));
        for caller in callers.iter().take(5) {
            let name: SharedString = ctx.say(caller.decl.name.to_string());
            let target = symbol_route(route.package.as_str(), &caller.decl.coordinate);
            let links = ctx.links.clone();
            column = column.child(
                div()
                    .id(SharedString::from(format!("caller-{}", caller.decl.coordinate)))
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
