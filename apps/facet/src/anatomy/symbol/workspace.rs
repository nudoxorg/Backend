//! In your workspace: every place your packages name the symbol, by
//! package, most places first. One filter row (a package picker, verb chips,
//! "include tests"), one group per package, one row per place: a click opens
//! that file at that line in the editor.

use super::host::{Act, Change, Ui};
use super::ink::{G, mark};
use super::key::{Key, Part, Sec};
use super::kit::{Chosen, Ellipsis, Env, Voice, chip, head, ink, roles, said, truncated, wrapped};
use super::view::{Ctx, Use, Uses, Verb};
use crate::measure::Set;
use crate::probe::{self, TextOverflow};
use gpui::{
    AnyElement, FontWeight, HighlightStyle, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledText, UnderlineStyle, deferred, div, px,
};
use std::collections::BTreeMap;

/// Five places show per package before "N more".
const FIVE: usize = 5;

fn visible<'a>(uses: &'a Uses, ui: &Ui) -> Vec<&'a Use> {
    uses.all
        .iter()
        .filter(|u| {
            (ui.imports || u.verb != Verb::Imports)
                && ui.package.as_ref().is_none_or(|p| *p == u.package)
                && ui.verb.is_none_or(|v| v == u.verb)
                && ui.fill.as_ref().is_none_or(|f| u.fill.as_ref() == Some(f))
                && (ui.tests || u.ctx != Ctx::Test)
        })
        .collect()
}

fn verb_marks(env: &Env<'_>, verbs: &[Verb]) -> AnyElement {
    div().flex().items_center().gap(env.k(8.0)).children(verbs.iter().map(|verb| mark(G::Verb(*verb), env.p, 14.0 * env.m.scale()))).into_any_element()
}

/// The code line with the name marked.
fn code_line(env: &Env<'_>, key: &Key, place: &Use) -> AnyElement {
    let i = ink(env.p);
    let mut highlights = Vec::new();
    if let Some((start, end)) = place.mark {
        highlights.push((
            start..end,
            HighlightStyle { color: Some(i.ink0), font_weight: Some(FontWeight(600.0)), underline: Some(UnderlineStyle { thickness: px(1.5), color: Some(i.peri), wavy: false }), ..HighlightStyle::default() },
        ));
    }
    let shared = SharedString::from(place.text.clone());
    let styled = StyledText::new(shared.clone()).with_highlights(highlights);
    probe::text(
        key.id(),
        shared,
        env.m.role(roles::CODE),
        1.0,
        TextOverflow::Ellipsis,
        div().set(roles::CODE, &env.m).text_color(i.ink2).whitespace_nowrap().overflow_hidden().text_ellipsis().min_w_0().w_full().child(styled),
    )
    .into_any_element()
}

fn row(env: &Env<'_>, place: &Use, key: &Key, open: Act) -> AnyElement {
    let i = ink(env.p);
    let glyph = div().w(env.s(22.0)).flex_none().flex().items_center().child(div().opacity(if place.approx { 0.75 } else { 1.0 }).child(mark(G::Verb(place.verb), env.p, 14.0 * env.m.scale())));
    let loc = div().w(env.s(env.lay.place)).flex_none().child(truncated(env, &key.field("place"), format!("{}:{}", place.file, place.line), roles::PLACE, i.ink3, Ellipsis::Start));
    let mut end = div().flex().items_center().gap(env.k(8.0)).flex_none();
    if let Some(fill) = &place.fill {
        end = end.child(div().px(env.k(6.0)).border_1().border_color(with_alpha(i.violet, 0.35)).child(said(env, &key.field("fill"), format!("T = {fill}"), roles::WRITTEN, i.violet)));
    }
    let group = key.field("group").text();
    end = end.child(
        div()
            .flex()
            .items_center()
            .gap(env.k(4.0))
            .invisible()
            .group_hover(group.clone(), |style| style.visible())
            .child(said(env, &key.field("open"), "open", roles::CHIP, i.peri))
            .child(mark(G::Open, env.p, 11.0 * env.m.scale())),
    );
    let act = open.clone();
    let line = div()
        .id(key.id())
        .group(group)
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(env.k(10.0))
        .min_h(env.s(30.0))
        .pl(env.s(2.0))
        .pr(env.s(6.0))
        .cursor_pointer()
        .hover(|style| style.bg(i.g2))
        .on_click(move |_, window, cx| act(window, cx))
        .child(glyph)
        .child(loc)
        .child(div().min_w(env.s(200.0)).flex_1().child(code_line(env, &key.field("code"), place)))
        .child(end)
        .into_any_element();
    let label = SharedString::from(format!("{}:{}", place.file, place.line));
    env.host.target(key, label, open, line)
}

fn with_alpha(mut color: gpui::Hsla, alpha: f32) -> gpui::Hsla {
    color.alpha *= alpha;
    color
}

fn package_menu(env: &Env<'_>, uses: &Uses, ui: &Ui) -> AnyElement {
    let i = ink(env.p);
    let packages = uses.packages(ui.imports, ui.tests);
    let base = Key::of(Part::Menu);
    let mut menu = div()
        .id(base.id())
        .absolute()
        .top(env.s(32.0))
        .left_0()
        .w(env.s(300.0).min(env.m.width()))
        .max_h(env.s(320.0))
        .overflow_y_scroll()
        .p(env.k(6.0))
        .flex()
        .flex_col()
        .bg(i.plate3)
        .border_1()
        .border_color(i.line3)
        .occlude()
        .on_mouse_down_out({
            let close = env.host.change(Change::Menu(false));
            move |_, window, cx| close(window, cx)
        });
    for (n, entry) in packages.iter().enumerate() {
        let key = base.at(n);
        let verbs: Vec<Verb> = entry.verbs.iter().copied().filter(|v| *v != Verb::Imports).collect();
        let pick = env.host.change(Change::Package(Some(entry.package.clone())));
        menu = menu.child(
            div()
                .id(key.id())
                .flex()
                .items_center()
                .gap(env.k(10.0))
                .h(env.s(28.0))
                .px(env.k(8.0))
                .cursor_pointer()
                .bg(if ui.package.as_deref() == Some(entry.package.as_str()) { i.g2 } else { i.plate3 })
                .hover(|style| style.bg(i.g2))
                .on_click(move |_, window, cx| pick(window, cx))
                .child(div().min_w_0().flex_1().child(truncated(env, &key.field("name"), entry.package.clone(), roles::PACKAGE, i.mint, Ellipsis::End)))
                .child(verb_marks(env, &verbs))
                .child(div().w(env.s(34.0)).flex().justify_end().child(said(env, &key.field("count"), entry.count.to_string(), roles::COUNT, i.ink3))),
        );
    }
    deferred(menu).with_priority(4).into_any_element()
}

fn picker(env: &Env<'_>, uses: &Uses, ui: &Ui) -> AnyElement {
    let i = ink(env.p);
    let base = Key::of(Part::Picker);
    let packages = uses.packages(ui.imports, ui.tests);
    let total: usize = packages.iter().map(|p| p.count).sum();
    let (words, count, tone, colour) = match &ui.package {
        Some(package) => (package.clone(), packages.iter().find(|p| p.package == *package).map_or(0, |p| p.count), i.mint, i.mint),
        None => (format!("all {} package{}", packages.len(), if packages.len() == 1 { "" } else { "s" }), total, i.line2, i.ink1),
    };
    let label = div()
        .flex()
        .items_center()
        .gap(env.k(6.0))
        .child(said(env, &base.field("name"), words, if ui.package.is_some() { roles::PACKAGE } else { roles::CHIP }, colour))
        .child(said(env, &base.field("count"), count.to_string(), roles::COUNT, i.ink3))
        .child(said(env, &base.field("caret"), "▾", roles::COUNT, i.ink3))
        .into_any_element();
    let chosen = if ui.package.is_some() { Chosen::On } else { Chosen::Off };
    let mut out = div().relative().flex().items_center().gap(env.k(6.0)).child(chip(env, &base, label, chosen, Voice::Plain, tone, env.host.change(Change::Menu(!ui.menu))));
    if ui.package.is_some() {
        let all = said(env, &base.field("all-words"), "× all", roles::CHIP, i.ink1);
        out = out.child(chip(env, &base.field("all"), all, Chosen::Off, Voice::Plain, i.line2, env.host.change(Change::Package(None))));
    }
    if ui.menu {
        out = out.child(package_menu(env, uses, ui));
    }
    out.into_any_element()
}

fn filters(env: &Env<'_>, uses: &Uses, ui: &Ui) -> AnyElement {
    let i = ink(env.p);
    let mut row = div().flex().flex_wrap().items_center().gap(env.k(6.0)).mb(env.k(8.0)).child(picker(env, uses, ui)).child(div().w(px(1.0)).h(env.s(18.0)).mx(env.k(4.0)).bg(i.line2));
    let mut counts: BTreeMap<Verb, usize> = BTreeMap::new();
    for place in &uses.all {
        if ui.package.as_ref().is_none_or(|p| *p == place.package) && (ui.tests || place.ctx != Ctx::Test) {
            *counts.entry(place.verb).or_default() += 1;
        }
    }
    for (n, verb) in Verb::ALL.into_iter().enumerate() {
        let Some(count) = counts.get(&verb) else { continue };
        let key = Key::of(Part::Chip).at(n);
        let quiet = verb == Verb::Imports && !ui.imports;
        let label = div()
            .flex()
            .items_center()
            .gap(env.k(6.0))
            .child(mark(G::Verb(verb), env.p, 14.0 * env.m.scale()))
            .child(said(env, &key.field("word"), verb.word(), roles::CHIP, if quiet { i.ink3 } else { i.ink1 }))
            .child(said(env, &key.field("count"), count.to_string(), roles::COUNT, i.ink3))
            .into_any_element();
        let chosen = if ui.verb == Some(verb) { Chosen::On } else { Chosen::Off };
        row = row.child(chip(env, &key, label, chosen, if quiet { Voice::Quiet } else { Voice::Plain }, i.peri, env.host.change(Change::Verb(verb))));
    }
    let tests = uses.all.iter().filter(|u| u.ctx == Ctx::Test).count();
    if tests > 0 {
        let key = Key::of(Part::Chip).field("tests");
        let label = div()
            .flex()
            .items_center()
            .gap(env.k(6.0))
            .child(said(env, &key.field("word"), "include tests", roles::CHIP, i.ink1))
            .child(said(env, &key.field("count"), tests.to_string(), roles::COUNT, i.ink3))
            .into_any_element();
        row = row.child(chip(env, &key, label, if ui.tests { Chosen::On } else { Chosen::Off }, Voice::Plain, i.peri, env.host.change(Change::Tests)));
    }
    row.into_any_element()
}

/// "In your workspace".
pub(super) fn workspace(env: &Env<'_>, uses: &Uses, ui: &Ui) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Sec(Sec::Uses));
    let section = div().id(key.id()).mt(env.s(env.lay.section)).flex().flex_col();
    if uses.all.is_empty() {
        let words = uses.elsewhere.clone().unwrap_or_else(|| "Nothing in your workspace names it.".to_owned());
        return section.child(head(env, Sec::Uses, "In your workspace", None, None)).child(wrapped(env, &key.field("none"), words, roles::BODY, i.ink3)).into_any_element();
    }
    let shown = visible(uses, ui);
    let aside = ui.fill.as_ref().map(|fill| {
        let clear = env.host.change(Change::Fill(None));
        div()
            .flex()
            .items_center()
            .gap(env.k(6.0))
            .child(said(env, &key.field("fill-words"), "where T is", roles::ASIDE, i.ink3))
            .child(said(env, &key.field("fill"), fill.clone(), roles::WRITTEN, i.violet))
            .child(div().id(key.field("fill-clear").id()).cursor_pointer().on_click(move |_, window, cx| clear(window, cx)).child(said(env, &key.field("fill-clear-words"), "clear", roles::ASIDE, i.peri)))
            .into_any_element()
    });
    let places = format!("{} place{}", shown.len(), if shown.len() == 1 { "" } else { "s" });
    // The groups.
    let mut groups: Vec<(String, Vec<&Use>)> = Vec::new();
    for place in &shown {
        match groups.iter_mut().find(|(package, _)| *package == place.package) {
            Some((_, list)) => list.push(place),
            None => groups.push((place.package.clone(), vec![place])),
        }
    }
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    let mut list = div().mt(env.k(10.0)).flex().flex_col();
    for (g, (package, places)) in groups.iter().enumerate() {
        let mut verbs: Vec<Verb> = places.iter().map(|p| p.verb).collect();
        verbs.sort();
        verbs.dedup();
        let group_key = Key::of(Part::Package).at(g);
        let group_head = div()
            .flex()
            .items_center()
            .gap(env.k(10.0))
            .pt(env.k(4.0))
            .pb(env.k(6.0))
            .border_b_1()
            .border_color(i.line1)
            .child(said(env, &group_key.field("name"), package.clone(), roles::PACKAGE, i.mint))
            .child(said(env, &group_key.field("count"), places.len().to_string(), roles::COUNT, i.ink3))
            .child(div().ml(env.k(6.0)).child(verb_marks(env, &verbs)));
        let cap = if ui.expanded.contains(package) { places.len() } else { FIVE };
        let rows: Vec<AnyElement> = places.iter().take(cap).enumerate().map(|(n, place)| row(env, place, &group_key.then(Part::Place, n), env.host.open_source(&place.path, place.line))).collect();
        let mut column = div().mt(env.k(12.0)).flex().flex_col().child(group_head).children(rows);
        if places.len() > cap {
            let more = env.host.change(Change::Expand(package.clone()));
            column = column.child(
                div()
                    .id(group_key.field("more").id())
                    .cursor_pointer()
                    .pl(env.s(30.0))
                    .py(env.k(6.0))
                    .on_click(move |_, window, cx| more(window, cx))
                    .child(said(env, &group_key.field("more-words"), format!("{} more in {package}", places.len() - cap), roles::CHIP, i.peri)),
            );
        }
        list = list.child(column);
    }
    if shown.is_empty() {
        list = list.child(wrapped(env, &key.field("empty"), "No place matches.", roles::BODY, i.ink3));
    }
    let mut out = section.child(head(env, Sec::Uses, "In your workspace", Some(places), aside)).child(filters(env, uses, ui)).child(list);
    if uses.all.iter().any(|u| u.approx) {
        out = out.child(div().mt(env.k(10.0)).child(wrapped(env, &key.field("foot"), "Places marked lighter are matched by name in the files that import it, not resolved.", roles::ASIDE, i.ink3)));
    }
    out.into_any_element()
}
