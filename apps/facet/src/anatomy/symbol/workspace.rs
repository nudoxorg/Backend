//! In your workspace: every place your packages name the symbol, by
//! package, most places first. One filter row (a package picker, verb chips,
//! "include tests"), one group per package, one row per place: a click opens
//! that file at that line in the editor.

use super::host::{Act, Change, Ui};
use super::ink::{G, mark};
use super::key::{Key, Part, Sec, Slot};
use super::kit::{
    Chip, Chosen, Ellipsis, Env, Fires, Voice, action, chip, faded, head, ink, roles, said, spot,
    truncated, wrapped,
};
use super::view::{Ctx, Use, Uses, Verb};
use crate::measure::Set;
use crate::overlay::float::Side;
use crate::overlay::menu::{self, Menu, MenuItem};
use crate::probe::{self, TextOverflow};
use gpui::{
    AnyElement, FontWeight, HighlightStyle, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, StyledText, UnderlineStyle, div, px,
};
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

/// Five places show per package before "N more".
const FIVE: usize = 5;

fn visible<'a>(uses: &'a Uses, ui: &Ui) -> Vec<&'a Use> {
    uses.all
        .iter()
        .filter(|u| {
            ui.listed().lists(u)
                && ui.package.as_ref().is_none_or(|p| *p == u.package)
                && ui.verb.is_none_or(|v| v == u.verb)
                && ui.fill.as_ref().is_none_or(|f| u.fill.as_ref() == Some(f))
        })
        .collect()
}

/// The line as it is when it fits `capacity` characters; otherwise, with its
/// start elided when more than a third of `capacity` comes before the mark, so
/// a cut line still shows the name it marks: the text and the mark in it.
fn windowed(
    text: &str,
    mark: Option<Range<usize>>,
    capacity: usize,
) -> (String, Option<Range<usize>>) {
    let Some(mark) =
        mark.filter(|mark| mark.end <= text.len() && text.is_char_boundary(mark.start))
    else {
        return (text.to_owned(), None);
    };
    if text.chars().count() <= capacity {
        return (text.to_owned(), Some(mark));
    }
    let lead = (capacity / 3).max(4);
    let before = text[..mark.start].chars().count();
    if before <= lead {
        return (text.to_owned(), Some(mark));
    }
    let cut = text[..mark.start]
        .char_indices()
        .nth(before - lead)
        .map_or(0, |(at, _)| at);
    let ellipsis = "…";
    (
        format!("{ellipsis}{}", &text[cut..]),
        Some(mark.start - cut + ellipsis.len()..mark.end - cut + ellipsis.len()),
    )
}

/// The code line with the name marked.
fn code_line(env: &Env<'_>, key: &Key, place: &Use) -> AnyElement {
    let i = ink(env.p);
    let mut highlights = Vec::new();
    let (text, mark) = windowed(&place.text, place.mark.clone(), env.lay.code);
    if let Some(mark) = mark {
        highlights.push((
            mark,
            HighlightStyle {
                color: Some(i.ink0),
                font_weight: Some(FontWeight(600.0)),
                underline: Some(UnderlineStyle {
                    thickness: px(1.5),
                    color: Some(i.peri),
                    wavy: false,
                }),
                ..HighlightStyle::default()
            },
        ));
    }
    let shared = SharedString::from(text);
    let styled = StyledText::new(shared.clone()).with_highlights(highlights);
    probe::text(
        key.id(),
        shared,
        env.m.role(roles::CODE),
        1.0,
        TextOverflow::Ellipsis,
        div()
            .set(roles::CODE, &env.m)
            .text_color(i.ink2)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .min_w_0()
            .w_full()
            .child(styled),
    )
    .into_any_element()
}

fn row(env: &Env<'_>, place: &Use, key: &Key, open: Act) -> AnyElement {
    let i = ink(env.p);
    let glyph = div()
        .w(env.s(22.0))
        .flex_none()
        .flex()
        .items_center()
        .child(
            div()
                .opacity(if place.approx { 0.75 } else { 1.0 })
                .child(mark(G::Verb(place.verb), env.p, 14.0 * env.m.scale())),
        );
    let loc = div().w(env.s(env.lay.place)).flex_none().child(truncated(
        env,
        &key.field(Slot::Place),
        format!("{}:{}", place.file, place.line),
        roles::PLACE,
        i.ink3,
        Ellipsis::Start,
    ));
    let mut end = div().flex().items_center().gap(env.k(8.0)).flex_none();
    if let Some(fill) = &place.fill {
        end = end.child(
            div()
                .px(env.k(6.0))
                .border_1()
                .border_color(faded(i.violet, 0.35))
                .child(said(
                    env,
                    &key.field(Slot::Fill),
                    format!("T = {fill}"),
                    roles::WRITTEN,
                    i.violet,
                )),
        );
    }
    let group = key.field(Slot::Group).text();
    end = end.child(
        div()
            .flex()
            .items_center()
            .gap(env.k(4.0))
            .invisible()
            .group_hover(group.clone(), |style| style.visible())
            .child(said(
                env,
                &key.field(Slot::Open),
                "open",
                roles::CHIP,
                i.peri,
            ))
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
        .hover(|style| style.bg(i.g3))
        .on_click(move |_, window, cx| act(window, cx))
        .child(glyph)
        .child(loc)
        .child(div().min_w(env.s(200.0)).flex_1().child(code_line(
            env,
            &key.field(Slot::Code),
            place,
        )))
        .child(end)
        .into_any_element();
    let label = SharedString::from(format!("{}:{}", place.file, place.line));
    env.host.target(key, label, open, line)
}

fn verb_marks(env: &Env<'_>, verbs: &[Verb]) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(env.k(8.0))
        .children(
            verbs
                .iter()
                .map(|verb| mark(G::Verb(*verb), env.p, 14.0 * env.m.scale())),
        )
        .into_any_element()
}

/// Opens the package menu on the overlay's menu layer: arrows walk it, Enter
/// chooses, Escape and an outside press close it, and pressing the chip again
/// closes it too. The first row is "all packages".
fn open_menu(env: &Env<'_>, uses: &Uses, ui: &Ui) -> Act {
    let packages = uses.packages(ui.listed());
    let mut items = vec![
        MenuItem::new(format!("all {} packages", packages.len())).chord(&[&packages
            .iter()
            .map(|p| p.count)
            .sum::<usize>()
            .to_string()]),
    ];
    let mut picks: Vec<Act> = vec![env.host.change(Change::Package(None))];
    for entry in &packages {
        items.push(MenuItem::new(entry.package.clone()).chord(&[&entry.count.to_string()]));
        picks.push(
            env.host
                .change(Change::Package(Some(entry.package.clone()))),
        );
    }
    let menu = Menu::new(items, move |choice, window, cx| {
        if let Some(pick) = picks.get(choice) {
            pick(window, cx);
        }
    });
    let spots = env.host.spots();
    let key = Key::of(Part::Menu).id();
    Rc::new(move |window, cx| {
        let Some(anchor) = spots.get(Sec::Picker) else {
            return;
        };
        menu::open(key.clone(), anchor, Side::Below, menu.clone(), window, cx);
    })
}

fn picker(env: &Env<'_>, uses: &Uses, ui: &Ui) -> AnyElement {
    let i = ink(env.p);
    let base = Key::of(Part::Picker);
    let packages = uses.packages(ui.listed());
    let total: usize = packages.iter().map(|p| p.count).sum();
    let (words, count, tone, colour) = match &ui.package {
        Some(package) => (
            package.clone(),
            packages
                .iter()
                .find(|p| p.package == *package)
                .map_or(0, |p| p.count),
            i.mint,
            i.mint,
        ),
        None => (
            format!(
                "all {} package{}",
                packages.len(),
                if packages.len() == 1 { "" } else { "s" }
            ),
            total,
            i.line2,
            i.ink1,
        ),
    };
    let label = div()
        .flex()
        .items_center()
        .gap(env.k(6.0))
        .child(said(
            env,
            &base.field(Slot::Name),
            words,
            if ui.package.is_some() {
                roles::PACKAGE
            } else {
                roles::CHIP
            },
            colour,
        ))
        .child(said(
            env,
            &base.field(Slot::Count),
            count.to_string(),
            roles::COUNT,
            i.ink3,
        ))
        .child(said(
            env,
            &base.field(Slot::Caret),
            "▾",
            roles::COUNT,
            i.ink3,
        ))
        .into_any_element();
    let chosen = if ui.package.is_some() {
        Chosen::On
    } else {
        Chosen::Off
    };
    let opener = chip(
        env,
        Chip {
            key: &base,
            label,
            chosen,
            voice: Voice::Plain,
            tone,
            fires: Fires::Press,
        },
        open_menu(env, uses, ui),
    );
    let mut out = div().flex().items_center().gap(env.k(6.0)).child(spot(
        Sec::Picker,
        &env.host.spots(),
        opener,
    ));
    if ui.package.is_some() {
        let all = said(
            env,
            &base.field(Slot::AllWords),
            "× all",
            roles::CHIP,
            i.ink1,
        );
        out = out.child(chip(
            env,
            Chip {
                key: &base.field(Slot::All),
                label: all,
                chosen: Chosen::Off,
                voice: Voice::Plain,
                tone: i.line2,
                fires: Fires::Click,
            },
            env.host.change(Change::Package(None)),
        ));
    }
    out.into_any_element()
}

fn filters(env: &Env<'_>, uses: &Uses, ui: &Ui) -> AnyElement {
    let i = ink(env.p);
    let mut row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(env.k(6.0))
        .mb(env.k(8.0))
        .child(picker(env, uses, ui))
        .child(div().w(px(1.0)).h(env.s(18.0)).mx(env.k(4.0)).bg(i.line2));
    let mut counts: BTreeMap<Verb, usize> = BTreeMap::new();
    for place in &uses.all {
        if ui.package.as_ref().is_none_or(|p| *p == place.package)
            && (ui.tests || place.ctx != Ctx::Test)
        {
            *counts.entry(place.verb).or_default() += 1;
        }
    }
    for (n, verb) in Verb::ALL.into_iter().enumerate() {
        let Some(count) = counts.get(&verb) else {
            continue;
        };
        let key = Key::of(Part::Chip).at(n);
        let quiet = verb == Verb::Imports && !ui.imports;
        let label = div()
            .flex()
            .items_center()
            .gap(env.k(6.0))
            .child(mark(G::Verb(verb), env.p, 14.0 * env.m.scale()))
            .child(said(
                env,
                &key.field(Slot::Word),
                verb.word(),
                roles::CHIP,
                if quiet { i.ink3 } else { i.ink1 },
            ))
            .child(said(
                env,
                &key.field(Slot::Count),
                count.to_string(),
                roles::COUNT,
                i.ink3,
            ))
            .into_any_element();
        let chosen = if ui.verb == Some(verb) {
            Chosen::On
        } else {
            Chosen::Off
        };
        row = row.child(chip(
            env,
            Chip {
                key: &key,
                label,
                chosen,
                voice: if quiet { Voice::Quiet } else { Voice::Plain },
                tone: i.peri,
                fires: Fires::Click,
            },
            env.host.change(Change::Verb(verb)),
        ));
    }
    let tests = uses.all.iter().filter(|u| u.ctx == Ctx::Test).count();
    if tests > 0 {
        let key = Key::of(Part::Chip).field(Slot::Tests);
        let label = div()
            .flex()
            .items_center()
            .gap(env.k(6.0))
            .child(said(
                env,
                &key.field(Slot::Word),
                "include tests",
                roles::CHIP,
                i.ink1,
            ))
            .child(said(
                env,
                &key.field(Slot::Count),
                tests.to_string(),
                roles::COUNT,
                i.ink3,
            ))
            .into_any_element();
        row = row.child(chip(
            env,
            Chip {
                key: &key,
                label,
                chosen: if ui.tests { Chosen::On } else { Chosen::Off },
                voice: Voice::Plain,
                tone: i.peri,
                fires: Fires::Click,
            },
            env.host.change(Change::Tests),
        ));
    }
    row.into_any_element()
}

/// "In your workspace".
pub(super) fn workspace(env: &Env<'_>, uses: &Uses, ui: &Ui) -> AnyElement {
    let i = ink(env.p);
    let key = Key::of(Part::Sec(Sec::Uses));
    let section = div()
        .id(key.id())
        .mt(env.s(env.lay.section))
        .flex()
        .flex_col();
    if uses.all.is_empty() {
        let words = uses
            .elsewhere
            .clone()
            .unwrap_or_else(|| "Nothing in your workspace names it.".to_owned());
        return section
            .child(head(env, Sec::Uses, "In your workspace", None, None))
            .child(wrapped(
                env,
                &key.field(Slot::None),
                words,
                roles::BODY,
                i.ink3,
            ))
            .into_any_element();
    }
    let shown = visible(uses, ui);
    let aside = ui.fill.as_ref().map(|fill| {
        let clear = env.host.change(Change::Fill(None));
        div()
            .flex()
            .items_center()
            .gap(env.k(6.0))
            .child(said(
                env,
                &key.field(Slot::FillWords),
                "where T is",
                roles::ASIDE,
                i.ink3,
            ))
            .child(said(
                env,
                &key.field(Slot::Fill),
                fill.clone(),
                roles::WRITTEN,
                i.violet,
            ))
            .child(action(
                env,
                &key.field(Slot::FillClear),
                "clear the filter",
                clear,
                said(
                    env,
                    &key.field(Slot::FillClearWords),
                    "clear",
                    roles::ASIDE,
                    i.peri,
                ),
            ))
            .into_any_element()
    });
    let places = format!(
        "{} place{}",
        shown.len(),
        if shown.len() == 1 { "" } else { "s" }
    );
    // The groups.
    let mut groups: Vec<(String, Vec<&Use>)> = Vec::new();
    for place in &shown {
        match groups
            .iter_mut()
            .find(|(package, _)| *package == place.package)
        {
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
            .child(said(
                env,
                &group_key.field(Slot::Name),
                package.clone(),
                roles::PACKAGE,
                i.mint,
            ))
            .child(said(
                env,
                &group_key.field(Slot::Count),
                places.len().to_string(),
                roles::COUNT,
                i.ink3,
            ))
            .child(div().ml(env.k(6.0)).child(verb_marks(env, &verbs)));
        let cap = if ui.expanded.contains(package) {
            places.len()
        } else {
            FIVE
        };
        let rows: Vec<AnyElement> = places
            .iter()
            .take(cap)
            .enumerate()
            .map(|(n, place)| {
                row(
                    env,
                    place,
                    &group_key.then(Part::Place, n),
                    env.host.open_source(&place.path, place.line),
                )
            })
            .collect();
        let mut column = div()
            .mt(env.k(12.0))
            .flex()
            .flex_col()
            .child(group_head)
            .children(rows);
        if places.len() > cap {
            let more = env.host.change(Change::Expand(package.clone()));
            let words = format!("{} more in {package}", places.len() - cap);
            column = column.child(div().pl(env.s(30.0)).py(env.k(4.0)).child(action(
                env,
                &group_key.field(Slot::More),
                words.clone(),
                more,
                said(
                    env,
                    &group_key.field(Slot::MoreWords),
                    words,
                    roles::CHIP,
                    i.peri,
                ),
            )));
        }
        list = list.child(column);
    }
    if shown.is_empty() {
        list = list.child(wrapped(
            env,
            &key.field(Slot::Empty),
            "No place matches.",
            roles::BODY,
            i.ink3,
        ));
    }
    let mut out = section
        .child(head(
            env,
            Sec::Uses,
            "In your workspace",
            Some(places),
            aside,
        ))
        .child(filters(env, uses, ui))
        .child(list);
    if uses.all.iter().any(|u| u.approx) {
        out = out.child(div().mt(env.k(10.0)).child(wrapped(
            env,
            &key.field(Slot::Foot),
            "Places marked lighter are matched by name in the files that import it, not resolved.",
            roles::ASIDE,
            i.ink3,
        )));
    }
    out.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::windowed;

    #[test]
    fn a_long_lead_is_cut_so_the_marked_name_stays_in_view() {
        let text = "let response = match serde_json::from_str(&body) {";
        let mark = text.find("from_str").map(|at| at..at + 8);
        let (shown, at) = windowed(text, mark.clone(), 30);
        let at = at.expect("still marked");
        assert!(shown.starts_with('…') && shown.len() < text.len() + 3);
        assert_eq!(&shown[at], "from_str", "the mark moved with the text");
        // A line that fits, or no mark, is left alone.
        assert_eq!(
            windowed(text, mark, 60),
            (text.to_owned(), text.find("from_str").map(|at| at..at + 8))
        );
        assert_eq!(windowed(text, None, 10), (text.to_owned(), None));
        // Multi-byte text before the mark is cut on a character.
        let wide = "let déjà_vu = «x» + Value::Null;";
        let mark = wide.find("Value").map(|at| at..at + 5);
        let (shown, at) = windowed(wide, mark, 12);
        assert_eq!(&shown[at.expect("marked")], "Value");
    }
}
