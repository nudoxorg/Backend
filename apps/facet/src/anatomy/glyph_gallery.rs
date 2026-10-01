//! W-Glyph's scenes: the sigils, the symbol page's new parts, and the whole
//! page as the board (`Nudox-Design-System/v6/moments`, `?v=symbol`) draws it.

use super::sigil::{Form, Sigil, sigil};
use super::plan::Fam;
use crate::gallery::Scene;
use super::page::gallery::{GalleryDoors, PageOf, draw, pinned, view_of, view_with_doors};
use super::plan::{Cap, DeclKind, Effect, Sibling};
use super::plan::{Source, compile};
use super::reach::Reached;
use std::rc::Rc;

mod serde_json_from_str;
mod serde_serialize;
mod toml_as_str;
mod toml_history;
mod toml_value;
use crate::theme::ActiveFacet;
use crate::tokens::scale;
use gpui::{AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, div, px};

pub(crate) const SCENES: &[Scene] = &[
    Scene { id: "glyph-value", title: "toml::Value with your code and it: the reach bar, five crates, their decks", size: (1176, 1500), build: |_, cx| {
        let (mut source, kind) = pinned(PageOf::Value);
        source.module = "toml::value".to_owned();
        source.caps = Cap::of_traits(["Clone", "PartialEq", "Display", "Debug", "Serialize", "Deserialize", "Default", "Index"]);
        source.reach = toml_value::toml_value();
        source.history = toml_history::toml_history();
        source.reach.reached = vec![
            Reached { name: "as_str".to_owned(), effect: Effect::Reads, gives: Some("maybe text".to_owned()) },
            Reached { name: "as_table".to_owned(), effect: Effect::Reads, gives: Some("maybe Table".to_owned()) },
            Reached { name: "as_bool".to_owned(), effect: Effect::Reads, gives: Some("maybe bool".to_owned()) },
            Reached { name: "as_array".to_owned(), effect: Effect::Reads, gives: Some("maybe list of Value".to_owned()) },
        ];
        view_of(source, kind, cx)
    } },
    Scene { id: "glyph-value-world", title: "toml::Value with the reach read from the fixture world itself (what the desktop draws): its crates, counts, members and mined lines", size: (1176, 1500), build: |_, cx| {
        let (world, _) = super::gallery::world();
        let node = super::gallery::find(world, "toml::value::Value").or_else(|| super::gallery::find(world, "toml::Value"));
        let (mut source, kind) = pinned(PageOf::Value);
        source.module = "toml::value".to_owned();
        if let Some(node) = node {
            source.reach = super::reach_world::reach_of(world, node, &mut super::gallery::read);
        }
        view_of(source, kind, cx)
    } },
    Scene { id: "glyph-serialize", title: "serde::Serialize: a socket, eighteen crates of yours name it", size: (1176, 1500), build: |_, cx| {
        let (mut source, kind) = pinned(PageOf::Serialize);
        source.module = "serde::ser".to_owned();
        source.scope = "serde::ser".to_owned();
        let sibling = |name: &str, kind: DeclKind, current: bool| Sibling { name: name.to_owned(), kind, link: (!current).then(|| name.to_owned()), current };
        source.siblings = vec![
            sibling("Error", DeclKind::Trait, false),
            sibling("Serialize", DeclKind::Trait, true),
            sibling("SerializeMap", DeclKind::Trait, false),
            sibling("SerializeSeq", DeclKind::Trait, false),
            sibling("SerializeStruct", DeclKind::Trait, false),
            sibling("Serializer", DeclKind::Trait, false),
        ];
        source.reach = serde_serialize::serde_serialize();
        view_of(source, kind, cx)
    } },
    Scene { id: "glyph-as-str", title: "toml::Value::as_str: a method, the pipe on its receiver, the crates that call it", size: (1176, 1200), build: |_, cx| {
        let (mut source, kind) = pinned(PageOf::FromStr);
        source.name = "as_str".to_owned();
        source.owner = Some("Value".to_owned());
        source.kind = DeclKind::Method;
        source.signature = Some("pub fn as_str(&self) -> Option<&str>".to_owned());
        source.lede = Some("Extracts the string of this value if it is a string.".to_owned());
        source.failures = Vec::new();
        source.uses = Default::default();
        source.sections = Vec::new();
        source.module = "toml::value".to_owned();
        source.scope = "toml::value::Value".to_owned();
        let sibling = |name: &str, current: bool| Sibling { name: name.to_owned(), kind: DeclKind::Method, link: (!current).then(|| name.to_owned()), current };
        source.siblings = vec![sibling("as_array", false), sibling("as_bool", false), sibling("as_float", false), sibling("as_integer", false), sibling("as_str", true), sibling("as_table", false), sibling("get", false)];
        source.reach = toml_as_str::toml_as_str();
        view_of(source, kind, cx)
    } },
    Scene { id: "glyph-hop-film", title: "A hop, filmed: click the from_str chip on Value's strip; Value's name and mark travel to its chip on from_str's strip, ringed 'from'", size: (1176, 760), build: |_, cx| hop(cx) },
    Scene { id: "glyph-hop", title: "serde_json::from_str reached by a hop from from_slice: its chip on the strip is ringed 'from'", size: (1176, 700), build: |_, cx| {
        let (mut source, kind) = pinned(PageOf::FromStr);
        source.module = "serde_json::de".to_owned();
        source.scope = "serde_json::de".to_owned();
        let sibling = |name: &str, kind: DeclKind, current: bool| Sibling { name: name.to_owned(), kind, link: (!current).then(|| name.to_owned()), current };
        source.siblings = vec![
            sibling("Deserializer", DeclKind::Struct, false),
            sibling("from_reader", DeclKind::Function, false),
            sibling("from_slice", DeclKind::Function, false),
            sibling("from_str", DeclKind::Function, true),
        ];
        source.reach = serde_json_from_str::serde_json_from_str();
        view_with_doors(source, kind, Some("from_slice"), cx)
    } },
    Scene { id: "glyph-from-str", title: "serde_json::from_str: a pipe, its sibling strip, fifteen crates as a bar, folded to seven", size: (1176, 1400), build: |_, cx| {
        let (mut source, kind) = pinned(PageOf::FromStr);
        source.module = "serde_json::de".to_owned();
        source.scope = "serde_json::de".to_owned();
        let sibling = |name: &str, kind: DeclKind, current: bool| Sibling { name: name.to_owned(), kind, link: (!current).then(|| name.to_owned()), current };
        source.siblings = vec![
            sibling("Deserializer", DeclKind::Struct, false),
            sibling("StreamDeserializer", DeclKind::Struct, false),
            sibling("from_reader", DeclKind::Function, false),
            sibling("from_slice", DeclKind::Function, false),
            sibling("from_str", DeclKind::Function, true),
        ];
        source.reach = serde_json_from_str::serde_json_from_str();
        view_of(source, kind, cx)
    } },
    Scene { id: "sigils", title: "The sigils: one drawing per kind, carrying its facts, at 64, 24 and 16 px", size: (1100, 640), build: |_, cx| cx.new(|_: &mut Context<Board>| Board).into() },
];

struct Board;

fn specimens() -> Vec<(&'static str, Sigil)> {
    let call = |form: Form| Sigil::new(form, Fam::Callable);
    vec![
        ("fn, no input", Sigil { gives: true, ..call(Form::Fn) }),
        ("fn(1) -> T", Sigil { ins: 1, gives: true, ..call(Form::Fn) }),
        ("fn(3) -> T, fails", Sigil { ins: 3, gives: true, fails: true, ..call(Form::Fn) }),
        ("fn(5), async", Sigil { ins: 5, gives: true, is_async: true, ..call(Form::Fn) }),
        ("method, &self", Sigil { ins: 1, gives: true, recv: true, ..call(Form::Method) }),
        ("unsafe fn", Sigil { ins: 2, is_unsafe: true, ..call(Form::Fn) }),
        ("macro", Sigil { ins: 1, ..call(Form::Macro) }),
        ("enum, 7 cases", Sigil { cases: 7, yours: 5, ..Sigil::new(Form::Enum, Fam::Type) }),
        ("enum, 3 cases", Sigil { cases: 3, ..Sigil::new(Form::Enum, Fam::Type) }),
        ("struct, 4 fields", Sigil { fields: 4, yours: 2, ..Sigil::new(Form::Struct, Fam::Type) }),
        ("struct, marker", Sigil { marker: true, ..Sigil::new(Form::Struct, Fam::Type) }),
        ("alias", Sigil::new(Form::Alias, Fam::Type)),
        ("trait, 2 write", Sigil { write: 2, get: 0, ..Sigil::new(Form::Trait, Fam::Contract) }),
        ("trait, 3 write 4 get", Sigil { write: 3, get: 4, yours: 3, ..Sigil::new(Form::Trait, Fam::Contract) }),
        ("constant", Sigil::new(Form::Value, Fam::Value)),
    ]
}

impl Render for Board {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let measure = facet.measure(px(1060.0));
        let mut grid = div().flex().flex_wrap().gap_x(px(28.0)).gap_y(px(22.0));
        for (label, facts) in specimens() {
            grid = grid.child(
                div().w(px(120.0)).flex().flex_col().items_center().gap(px(8.0))
                    .child(div().h(px(78.0)).flex().items_center().justify_center().child(sigil(facts, 64.0, palette)))
                    .child(div().flex().items_center().gap(px(14.0)).child(sigil(facts, 24.0, palette)).child(sigil(facts, 16.0, palette)))
                    .child(crate::anatomy::page::said(format!("sigil-{label}"), label, scale::LABEL, palette.ink3, &measure)),
            );
        }
        div().size_full().bg(palette.g1.hsla()).p(px(24.0)).child(grid)
    }
}


// ------------------------------------------------------------------ the hop

/// Two pages, siblings in one module, each with a door to the other: a click
/// on a strip chip swaps the page, and the shared name and mark of the page
/// you leave glide to its chip on the page you reach.
struct Hop {
    pages: Vec<(String, Source)>,
    at: usize,
    from: Option<usize>,
}

fn hop_pages() -> Vec<(String, Source)> {
    let sibling = |name: &str, kind: DeclKind, current: bool| Sibling { name: name.to_owned(), kind, link: Some(name.to_owned()).filter(|_| !current), current };
    let strip = |at: usize| vec![sibling("Value", DeclKind::Enum, at == 0), sibling("from_str", DeclKind::Function, at == 1)];
    let (mut value, _) = pinned(PageOf::Value);
    value.module = "toml::value".to_owned();
    value.scope = "toml::value".to_owned();
    value.siblings = strip(0);
    value.caps = Cap::of_traits(["Clone", "PartialEq", "Display", "Debug", "Serialize", "Deserialize", "Default", "Index"]);
    value.reach = toml_value::toml_value();
    value.history = toml_history::toml_history();
    let (mut parse, _) = pinned(PageOf::FromStr);
    parse.module = "toml::value".to_owned();
    parse.scope = "toml::value".to_owned();
    parse.siblings = strip(1);
    parse.reach = serde_json_from_str::serde_json_from_str();
    vec![("Value".to_owned(), value), ("from_str".to_owned(), parse)]
}

fn hop(cx: &mut gpui::App) -> gpui::AnyView {
    cx.new(|_: &mut Context<Hop>| Hop { pages: hop_pages(), at: 0, from: None }).into()
}

impl Render for Hop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let source = &self.pages[self.at].1;
        let plan = compile(source);
        let this = cx.entity().downgrade();
        let open: Rc<dyn Fn(&str, &mut Window, &mut gpui::App)> = Rc::new(move |link, _, cx| {
            let link = link.to_owned();
            let _ = this.update(cx, |scene, cx| {
                if let Some(to) = scene.pages.iter().position(|(name, _)| *name == link) {
                    scene.from = Some(scene.at);
                    scene.at = to;
                    cx.notify();
                }
            });
        });
        let doors = GalleryDoors { from: self.from.map(|at| self.pages[at].0.clone()), open: Some(open), marks: true };
        draw(source, &plan, Some(&doors), Some(&self.pages[self.at].0), window, cx)
    }
}

#[cfg(test)]
mod tests {
    //! The page's own interactions against GPUI's test platform: the same
    //! element trees the desktop mounts (no shell), pinned to real lines of
    //! this repository's crates, every assertion read off what was painted.

    use super::*;
    use crate::probe::{Ledger, TextSample};
    use crate::theme::{Facet, set_facet};
    use gpui::{Bounds, Modifiers, Pixels, TestAppContext, VisualTestContext, point, size};
    use std::time::Duration;

    fn mount<V: Render + 'static>(cx: &mut TestAppContext, build: impl FnOnce(&mut Window, &mut Context<V>) -> V) -> (gpui::Entity<V>, &mut VisualTestContext) {
        cx.update(|cx| {
            set_facet(Facet::default(), cx);
            let _ = crate::fonts::install(cx);
            crate::probe::enable(cx);
        });
        let (view, cx) = cx.add_window_view(build);
        cx.simulate_resize(size(px(1176.0), px(1400.0)));
        (view, cx)
    }

    /// The frame the platform would draw `ms` from now.
    fn frame(cx: &mut VisualTestContext, ms: u64) -> Ledger {
        if ms > 0 {
            cx.executor().advance_clock(Duration::from_millis(ms));
        }
        cx.run_until_parked();
        let _ = cx.update(|_, cx| crate::probe::take(cx));
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
        cx.update(|_, cx| crate::probe::take(cx))
    }

    fn text<'a>(ledger: &'a Ledger, key: &str) -> Option<&'a TextSample> {
        ledger.texts.iter().find(|text| text.key == key)
    }

    fn at<'a>(ledger: &'a Ledger, key: &str) -> &'a str {
        text(ledger, key).map_or("", |text| text.content.as_str())
    }

    fn bounds(sample: &crate::probe::BoundsSample) -> Bounds<Pixels> {
        Bounds::new(point(px(sample.x), px(sample.y)), size(px(sample.width), px(sample.height)))
    }

    fn centre(sample: &TextSample) -> gpui::Point<Pixels> {
        let b = bounds(&sample.bounds);
        point(b.left() + b.size.width / 2.0, b.top() + b.size.height / 2.0)
    }

    fn value_page(cx: &mut TestAppContext) -> &mut VisualTestContext {
        let (mut source, kind) = pinned(PageOf::Value);
        source.reach = toml_value::toml_value();
        source.reach.reached = vec![
            Reached { name: "as_str".to_owned(), effect: Effect::Reads, gives: Some("maybe text".to_owned()) },
            Reached { name: "as_table".to_owned(), effect: Effect::Reads, gives: Some("maybe Table".to_owned()) },
        ];
        let plan = compile(&source);
        let _ = kind;
        struct Page(Source, super::super::plan::PagePlan);
        impl Render for Page {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                draw(&self.0, &self.1, None, None, window, cx)
            }
        }
        mount(cx, |_, _| Page(source, plan)).1
    }

    #[gpui::test]
    fn scrubbing_the_reach_bar_names_the_member_and_brings_its_lines_forward_in_every_deck(cx: &mut TestAppContext) {
        let cx = value_page(cx);
        let rest = frame(cx, 0);
        assert_eq!(at(&rest, "page-yours-caption"), "What your code reaches through Value", "at rest the caption says what the bar is");
        assert_eq!(at(&rest, "page-yours-0-line-0-place"), "harness.rs:607", "desktop's deck shows its first line");
        // Point at the `as_table` segment.
        let segment = text(&rest, "page-yours-seg-1-label").expect("the second segment is as_table");
        assert_eq!(segment.content, "as_table");
        let width = bounds(&segment.bounds).size.width;
        cx.simulate_mouse_move(centre(segment), None, Modifiers::none());
        let scrubbed = frame(cx, 0);
        let caption = at(&scrubbed, "page-yours-caption");
        assert_eq!(caption, "as_table · reads it · gives maybe Table · desktop 2  local-service 10  engine 3  advisory 2", "the member, what it does, and who reaches it");
        assert_eq!(at(&scrubbed, "page-yours-0-line-0-place"), "harness.rs:615", "desktop's deck brings its as_table line to the front");
        assert_eq!(at(&scrubbed, "page-yours-1-line-0-place"), "builtin/browse.rs:188", "and so does local-service's");
        let grown = bounds(&text(&scrubbed, "page-yours-seg-1-label").expect("still there").bounds).size.width;
        assert!(grown >= width, "the pointed-at segment keeps its room: {width} → {grown}");
        // The pointer leaves: everything lets go in the frame that answers.
        cx.simulate_mouse_move(point(px(2.0), px(2.0)), None, Modifiers::none());
        let after = frame(cx, 0);
        assert_eq!(at(&after, "page-yours-0-line-0-place"), "harness.rs:607");
    }

    #[gpui::test]
    fn resting_on_a_deck_fans_it_in_place_and_a_click_opens_every_line_it_kept(cx: &mut TestAppContext) {
        let cx = value_page(cx);
        let rest = frame(cx, 0);
        assert!(text(&rest, "page-yours-0-line-1-code").is_none(), "stacked: one line shows");
        let top = text(&rest, "page-yours-0-line-0-code").expect("desktop's top line");
        let pointer = point(bounds(&top.bounds).left() + px(30.0), bounds(&top.bounds).top() + px(4.0));
        cx.simulate_mouse_move(pointer, None, Modifiers::none());
        let fanned = frame(cx, 0);
        for n in 0..5 {
            assert!(text(&fanned, &format!("page-yours-0-line-{n}-code")).is_some(), "fanned: line {n} shows");
        }
        assert!(text(&fanned, "page-yours-0-line-5-code").is_none(), "a fan is five lines");
        let (first, second) = (bounds(&text(&fanned, "page-yours-0-line-0-code").expect("0").bounds), bounds(&text(&fanned, "page-yours-0-line-1-code").expect("1").bounds));
        assert!(second.top() > first.top() + px(20.0), "the lines stack one under the other");
        // The row grew in place: the next crate moved down.
        assert!(bounds(&text(&fanned, "page-yours-1-name").expect("next crate").bounds).top() > bounds(&text(&rest, "page-yours-1-name").expect("next crate").bounds).top() + px(60.0));
        // A click opens the whole list: every kept line, and what was counted but not kept.
        cx.simulate_click(pointer, Modifiers::none());
        cx.simulate_mouse_move(point(px(2.0), px(2.0)), None, Modifiers::none());
        let open = frame(cx, 0);
        assert!(text(&open, "page-yours-0-line-6-code").is_some(), "open: the seventh line shows");
        assert_eq!(at(&open, "page-yours-0-more"), "and 36 more counted, lines not kept");
    }

    #[gpui::test]
    fn the_symbol_you_leave_lands_in_its_chip_on_the_next_page_ringed_from(cx: &mut TestAppContext) {
        let (_hop, cx) = mount(cx, |_, _| Hop { pages: hop_pages(), at: 0, from: None });
        let rest = frame(cx, 0);
        let title = |cx: &mut VisualTestContext, address: &str| cx.update(|window, cx| crate::motion::shared::last_bounds(super::super::page::title_key(address), window, cx));
        let before = title(cx, "Value").expect("Value's title is painted, and shared");
        assert!(text(&rest, "page-from-tag").is_none(), "nothing is ringed on the page you start on");
        // Click the chip of from_str.
        let chip = text(&rest, "page-strip-1-name").expect("from_str's chip");
        assert_eq!(chip.content, "from_str");
        cx.simulate_click(centre(chip), Modifiers::none());
        let _ = frame(cx, 0);
        let start = title(cx, "Value").expect("Value's name is painted on the first frame of the hop");
        // The name starts centred on the title it was and as tall as it (the morph scales it to fit).
        let mid = |b: Bounds<Pixels>| (f32::from(b.left() + b.size.width / 2.0), f32::from(b.top() + b.size.height / 2.0));
        let near = |a: Bounds<Pixels>, b: Bounds<Pixels>, by: f32| (mid(a).0 - mid(b).0).abs() <= by && (mid(a).1 - mid(b).1).abs() <= by;
        assert!(near(start, before, 4.0), "it starts over the title it was: {start:?} vs {before:?}");
        assert!((f32::from(start.size.height) - f32::from(before.size.height)).abs() < 2.0, "as tall as the title it was");
        let mut far = vec![mid(start)];
        for ms in [60_u64, 60, 120, 240, 240] {
            let _ = frame(cx, ms);
            let now = title(cx, "Value").expect("still painted in flight");
            far.push(mid(now));
        }
        let landed = frame(cx, 800);
        let chip = bounds(&text(&landed, "page-strip-0-name").expect("Value's chip on from_str's strip").bounds);
        let end = title(cx, "Value").expect("Value's name at rest");
        assert!(near(end, chip, 3.0), "it lands on its chip: {end:?} vs {chip:?}");
        assert!((f32::from(end.size.height) - f32::from(chip.size.height)).abs() < 2.0, "at the chip's own size");
        let (cx_, cy_) = mid(chip);
        let away = |(x, y): (f32, f32)| ((x - cx_).powi(2) + (y - cy_).powi(2)).sqrt();
        let distances: Vec<f32> = far.iter().copied().map(away).collect();
        assert!(distances[0] > 60.0, "the hop is a real move (from {:.0} px away): {distances:?}", distances[0]);
        assert!(distances.windows(2).all(|pair| pair[1] <= pair[0] + 1.0), "it only ever closes in: {distances:?}");
        // Ringed "from", on the chip.
        let tag = bounds(&text(&landed, "page-from-tag").expect("the chip is ringed 'from'").bounds);
        assert!(tag.top() < chip.top() && tag.left() > chip.left() - px(80.0) && tag.left() < chip.right() + px(80.0), "the tag rides the chip's ring: {tag:?} on {chip:?}");
        assert_eq!(landed.texts.iter().filter(|text| text.key == "page-from-tag").count(), 1, "one ring");
    }

    #[gpui::test]
    fn its_mark_travels_with_its_name_and_lands_small_on_the_chip(cx: &mut TestAppContext) {
        let (_hop, cx) = mount(cx, |_, _| Hop { pages: hop_pages(), at: 0, from: None });
        let rest = frame(cx, 0);
        let mark = |cx: &mut VisualTestContext| cx.update(|window, cx| crate::motion::shared::last_bounds(gpui::ElementId::Name("mark:Value".into()), window, cx));
        let hero = mark(cx).expect("the hero mark is shared");
        let chip = text(&rest, "page-strip-1-name").expect("from_str's chip");
        cx.simulate_click(centre(chip), Modifiers::none());
        let _ = frame(cx, 0);
        let start = mark(cx).expect("the mark is painted on the first frame");
        assert!((f32::from(start.size.height) - f32::from(hero.size.height)).abs() < 4.0, "it starts as big as the hero mark: {start:?} vs {hero:?}");
        let _ = frame(cx, 800);
        let end = mark(cx).expect("the mark at rest");
        assert!(f32::from(end.size.height) < f32::from(hero.size.height) * 0.5, "it shrank into the chip: {} px → {} px", hero.size.height, end.size.height);
    }

    #[gpui::test]
    fn resting_on_a_stack_spreads_its_operations_in_place_and_leaving_it_gathers_them(cx: &mut TestAppContext) {
        let cx = value_page(cx);
        let rest = frame(cx, 0);
        let stack = text(&rest, "page-does-reads it").expect("the reads-it stack");
        assert!(text(&rest, "page-does-reads it-stack-0-name").is_none(), "gathered: no names");
        let below = bounds(&text(&rest, "page-yours-0-name").expect("a row below").bounds).top();
        cx.simulate_mouse_move(centre(stack), None, Modifiers::none());
        let spread = frame(cx, 0);
        assert!(text(&spread, "page-does-reads it-stack-0-name").is_some(), "spread: its first operation is named");
        let _ = below;
        cx.simulate_mouse_move(point(px(2.0), px(2.0)), None, Modifiers::none());
        assert!(text(&frame(cx, 0), "page-does-reads it-stack-0-name").is_none(), "gathered again");
    }
}
