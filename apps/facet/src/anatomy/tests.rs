//! The anatomy's text: one line of runs per row, links where the names are,
//! the ⌥ source only under x-ray, and a click on a link dispatching `Open`.

use super::text::{Deco, Line, Links, Open, TypeInk};
use super::roles;
use crate::measure::{Measure, Reveal};
use crate::semantics::types::{Nowhere, Scope, Target};
use crate::theme::{Facet, set_facet};
use crate::tokens::ABYSS;
use gpui::{
    Context, IntoElement, Modifiers, ParentElement, Render, SharedString, Styled, TestAppContext, Window, div, point,
    px,
};
use std::cell::RefCell;
use std::rc::Rc;

fn scope() -> Scope<'static> {
    Scope::new(&Nowhere).owner(Target::Node(7), "RelationGroup")
}

#[test]
fn a_spelled_type_is_one_line_with_a_link_per_name() {
    let spelled = scope().spell_text("Result<Vec<Relation>, fmt::Error>");
    let mut line = Line::new();
    line.spelled(&spelled, &TypeInk::new(roles::TYPE, &ABYSS), &Links::plain(), false);
    assert_eq!(line.text(), "list of Relation or fails with Error");
    let links: Vec<(&str, &Target)> = line.links().iter().map(|(r, t)| (&line.text()[r.clone()], t)).collect();
    assert_eq!(
        links,
        [
            ("Relation", &Target::Path(SharedString::from("Relation"))),
            ("Error", &Target::Path(SharedString::from("fmt::Error"))),
        ]
    );
}

#[test]
fn xray_spells_the_exact_source_beside_the_words_and_only_then() {
    let spelled = scope().spell_text("&mut fmt::Formatter<'_>");
    let ink = TypeInk::new(roles::TYPE, &ABYSS);
    let mut plain = Line::new();
    plain.spelled(&spelled, &ink, &Links::plain(), false);
    assert_eq!(plain.text(), "mutable Formatter");
    let mut xray = Line::new();
    xray.spelled(&spelled, &ink, &Links::plain(), true);
    assert_eq!(xray.text(), "mutable Formatter  &mut fmt::Formatter<'_>");
    // A type whose words are its source says it once.
    let mut same = Line::new();
    same.spelled(&scope().spell_text("u8"), &ink, &Links::plain(), true);
    assert_eq!(same.text(), "u8");
}

#[test]
fn generics_are_italic_and_never_links() {
    let spelled = Scope::new(&Nowhere).generics(["T"]).spell_text("Option<T>");
    let mut line = Line::new();
    line.spelled(&spelled, &TypeInk::new(roles::TYPE, &ABYSS), &Links::plain(), false);
    assert_eq!(line.text(), "maybe T");
    assert!(line.links().is_empty());
}

struct OneLine {
    measure: Measure,
}

impl Render for OneLine {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut line = Line::new();
        line.link("RelationLabel", roles::TYPE, ABYSS.ink1.hsla(), Target::Node(21_727));
        line.push(" and some words", roles::TYPE, ABYSS.ink3.hsla());
        div().size_full().child(line.element("one-line", roles::TYPE, &self.measure, &Links::plain(), &ABYSS))
    }
}

#[gpui::test]
fn clicking_a_link_dispatches_open_and_clicking_words_does_not(cx: &mut TestAppContext) {
    let opened: Rc<RefCell<Vec<Target>>> = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        crate::fonts::install(cx).ok();
        set_facet(Facet { reveal: Reveal::default(), ..Facet::default() }, cx);
        let opened = Rc::clone(&opened);
        cx.on_action(move |open: &Open, _| opened.borrow_mut().push(open.target.clone()));
    });
    let measure = Facet::default().measure(px(600.0));
    let (_view, cx) = cx.add_window_view(|_, _| OneLine { measure });
    cx.run_until_parked();
    // The link is the line's first word, at its left edge.
    cx.simulate_click(point(px(6.0), px(8.0)), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(*opened.borrow(), [Target::Node(21_727)]);
    // Far along the line: plain words, no action.
    cx.simulate_click(point(px(560.0), px(8.0)), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(opened.borrow().len(), 1);
}

#[test]
fn only_the_one_sentence_per_row_is_serif() {
    // §6.2: serif is the lede and one sentence per row, never a heading,
    // a count, a name or a label.
    let not_serif = [
        roles::HEAD,
        roles::NAME,
        roles::NAME_QUIET,
        roles::TYPE,
        roles::INPUT,
        roles::OUTPUT,
        roles::QUIET,
        roles::CAP,
        roles::PRISM_ROW,
        roles::SECTION,
        roles::GROUP,
        roles::ROW,
        roles::CAPTION,
        roles::CODE,
        roles::NOTE,
    ];
    assert!(not_serif.iter().all(|r| !super::is_serif(*r)));
    assert!(super::is_serif(roles::SAY));
    // A generic variable is set as a mathematical variable: italic serif.
    assert!(super::is_serif(roles::italic(roles::TYPE)));
}

#[test]
fn decorating_a_range_splits_its_runs_and_marks_only_that_text() {
    let mut line = Line::new();
    line.push("list of ", roles::TYPE, ABYSS.ink3.hsla());
    let name = line.link("Relation", roles::TYPE, ABYSS.ink1.hsla(), Target::Node(1));
    let tail = line.push(" or fails", roles::TYPE, ABYSS.ink3.hsla());
    // "of Rel" crosses the first run's end and cuts the link's run.
    line.decorate(5..11, Deco::Strike(ABYSS.coral.base.hsla()));
    line.decorate(name.clone(), Deco::Underline(ABYSS.mint.base.hsla()));
    let runs = line.runs_for_test();
    let total: usize = runs.iter().map(|r| r.len).sum();
    assert_eq!(total, line.text().len());
    let struck: String = spans(&line, |r| r.strikethrough.is_some());
    let under: String = spans(&line, |r| r.underline.is_some());
    assert_eq!(struck, "of Rel");
    assert_eq!(under, "Relation");
    // Text pushed after a decorated run starts its own undecorated run.
    line.decorate(tail, Deco::Underline(ABYSS.mint.base.hsla()));
    line.push("!", roles::TYPE, ABYSS.ink3.hsla());
    assert!(line.runs_for_test().last().is_some_and(|r| r.underline.is_none() && r.len == 1));
}

fn spans(line: &Line, keep: impl Fn(&gpui::TextRun) -> bool) -> String {
    let mut at = 0;
    let mut out = String::new();
    for run in line.runs_for_test() {
        if keep(run) {
            out.push_str(&line.text()[at..at + run.len]);
        }
        at += run.len;
    }
    out
}

#[test]
fn the_lint_sees_gpui_s_break_opportunities_in_code() {
    use super::text::wrap_units;
    // gpui may wrap before `(` and `&`; `.`, `_` and `::` hold a word together.
    let units = wrap_units(".with_prose(Prose::from_fragments(&document.fragments))");
    let widest = units.split_whitespace().max_by_key(|u| u.len()).unwrap_or_default();
    assert_eq!(widest, "(Prose::from_fragments");
    assert_eq!(wrap_units("list of Relation"), "list of Relation");
}
