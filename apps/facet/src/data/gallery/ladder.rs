//! The ladder for data marks (calm `v4/shots/Ladder.png`): the same datum
//! at four rungs, and ⌥ x-ray spelling the marks in place.

use super::super::{
    Directions, Stage, StageState, Tab, TickTone, comb, compass, compass_bar, compass_row,
    gem_progress, lens_bar, territory,
};
use super::lenses::release_lens;
use crate::icons::Kind;
use crate::measure::{Reveal, Rung};
use crate::overlay::float::{self, FloatKind};
use crate::overlay::lens::lens_card;
use crate::theme::{ActiveFacet, Facet};
use crate::tokens::{Face, TypeRole, ty};
use crate::{Measure, Set, Typeset};
use gpui::{AnyElement, App, Div, IntoElement, ParentElement, Styled, Window, div, px};

const H1: TypeRole = TypeRole {
    face: Face::Display,
    weight: 720.0,
    size: 30.0,
    line: 32.0,
    tracking: -0.03,
    italic: false,
};
const HEAD: TypeRole = TypeRole {
    weight: 500.0,
    size: 11.0,
    line: 14.0,
    ..ty::SMALL
};
const TITLE: TypeRole = TypeRole {
    weight: 500.0,
    size: 12.5,
    line: 16.0,
    ..ty::SMALL
};

pub(crate) fn board(_width: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let s = facet.text_scale;
    let m = |w: f32| facet.measure(px(w * s));
    let xray = |w: f32| {
        Measure::new(
            px(w * s),
            &Facet {
                reveal: Reveal {
                    xray: true,
                    keys: false,
                },
                ..facet
            },
        )
    };
    let words = |role: TypeRole, ink: crate::tokens::Tone, t: &'static str| {
        div().typeset(role, &facet).text_color(ink.hsla()).child(t)
    };
    let grid = |cells: [AnyElement; 5]| -> Div {
        let widths = [110.0, 90.0, 220.0, 360.0, 380.0];
        let mut row = div().flex().items_center().gap(px(24.0 * s));
        for (cell, w) in cells.into_iter().zip(widths) {
            row = row.child(div().w(px(w * s)).flex().items_center().child(cell));
        }
        row
    };
    let head = grid([
        div().into_any_element(),
        words(HEAD, palette.ink4, "Mark").into_any_element(),
        words(HEAD, palette.ink4, "Tag").into_any_element(),
        words(HEAD, palette.ink4, "Row").into_any_element(),
        words(HEAD, palette.ink4, "Card").into_any_element(),
    ]);

    // A symbol's relations: compass → spelled → row → bar.
    let dirs = Directions::new(5, 11, 26, 0);
    let relations = grid([
        words(TITLE, palette.ink3, "Its relations").into_any_element(),
        compass(dirs, &m(90.0)).into_any_element(),
        compass(dirs, &m(220.0)).spell(true).into_any_element(),
        compass_row(dirs, &m(360.0), palette).into_any_element(),
        div().w(px(360.0 * s)).child(compass_bar(dirs, &m(360.0))).into_any_element(),
    ]);

    // A release history: whisper → tag → comb → comb with its caption.
    let ticks = super::releases();
    let history = grid([
        words(TITLE, palette.ink3, "Its releases").into_any_element(),
        comb("l-mark", ticks.clone(), &m(90.0)).rung(Rung::Mark).into_any_element(),
        comb("l-tag", ticks.clone(), &m(220.0)).rung(Rung::Tag).into_any_element(),
        comb("l-row", ticks.clone(), &m(360.0)).rung(Rung::Row).into_any_element(),
        comb("l-card", ticks.clone(), &m(380.0))
            .rung(Rung::Card)
            .caption([("19 since your pin, ", false), ("2", true), (" touch your code", false)])
            .into_any_element(),
    ]);

    // One release: its tick → its tick and name → its line → its lens.
    let one = vec![super::super::Tick::new(18.0).tone(TickTone::Current).label("1.0.200")];
    let release = grid([
        words(TITLE, palette.ink3, "A release").into_any_element(),
        comb("r1-mark", one.clone(), &m(12.0)).rung(Rung::Row).thickness(22.0).into_any_element(),
        div()
            .flex()
            .items_center()
            .gap(px(8.0 * s))
            .child(comb("r1-tag", one.clone(), &m(4.0)).rung(Rung::Row).thickness(16.0))
            .child(div().set(ty::MONO_ROW, &m(200.0)).text_color(palette.ink0.hsla()).child("1.0.200"))
            .into_any_element(),
        div()
            .flex()
            .items_baseline()
            .gap(px(10.0 * s))
            .child(div().set(ty::MONO_ROW, &m(360.0)).text_color(palette.ink0.hsla()).child("1.0.200"))
            .child(
                div()
                    .set(ty::CAPTION, &m(360.0))
                    .text_color(palette.ink3.hsla())
                    .child("3 weeks ago · touches from_str"),
            )
            .into_any_element(),
        float::plate(FloatKind::Lens, false, palette)
            .child(lens_card(&release_lens(40), &facet.measure(px(340.0 * s)), None, cx))
            .into_any_element(),
    ]);

    // X-ray: the same marks, at rest and while ⌥ is held.
    let regions = super::package::regions();
    let stages = vec![
        Stage::new("resolve", StageState::Done),
        Stage::new("fetch", StageState::Done),
        Stage::new("index", StageState::Now).done(0.6),
        Stage::new("seal", StageState::Todo),
    ];
    let pane = |held: bool| {
        let mm = |w: f32| if held { xray(w) } else { m(w) };
        div()
            .flex()
            .flex_col()
            .gap(px(16.0 * s))
            .child(words(HEAD, palette.ink4, if held { "Holding ⌥" } else { "At rest" }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(18.0 * s))
                    .child(compass(dirs, &mm(300.0)))
                    .child(gem_progress(if held { "x-gem-h" } else { "x-gem" }, Kind::Package, stages.clone(), &mm(300.0)).size(24.0)),
            )
            .child(lens_bar(
                if held { "x-tabs-h" } else { "x-tabs" },
                vec![
                    Tab::new("Reference"),
                    Tab::new("Relations").count("19"),
                    Tab::new("Usage").count("9"),
                    Tab::new("History").count("3"),
                ],
                0,
                &mm(420.0),
            ))
            .child(comb(if held { "x-comb-h" } else { "x-comb" }, ticks.clone(), &mm(420.0)).caption([
                ("19 since your pin, ", false),
                ("2", true),
                (" touch your code", false),
            ]))
            .child(territory(if held { "x-terr-h" } else { "x-terr" }, regions.clone(), &mm(420.0)))
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .px(px(56.0))
        .pt(px(44.0))
        .gap(px(30.0 * s))
        .child(div().set(H1, &facet.measure(px(1440.0))).text_color(palette.ink0.hsla()).child("One thing, four rungs"))
        .child(head)
        .child(relations)
        .child(history)
        .child(release)
        .child(div().flex().gap(px(40.0 * s)).child(pane(false)).child(pane(true)))
        .into_any_element()
}
