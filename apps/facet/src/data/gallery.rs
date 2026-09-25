//! Data-mark scenes: `data-marks` (every mark, its rungs and its states),
//! and the Glacier / 200 % / dense twins.

#![allow(clippy::too_many_lines)]

mod films;
mod ladder;
mod lenses;
mod package;
mod stage;
mod symbol;

use super::{
    CombOrientation, Dir, Directions, Door, FileUses, Stone, StoneState, Tick, TickTone, comb,
    compass, compass_bar, compass_row, fcomb, mosaic,
};
use crate::gallery::Scene;
use crate::measure::Rung;
use crate::paint::gallery::{board, canvas, doc, text};
use crate::theme::ActiveFacet;
use crate::tokens::{Appearance, Face, Family, TypeRole, ty};
use gpui::{
    AnyElement, AnyView, App, Div, IntoElement, ParentElement, Styled, Window, div, px,
};

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "lenses",
        title: "Lenses (calm): a scripted rest on a release tick opens the real lens through the float layer; module and language lenses",
        size: (1440, 760),
        build: |_, cx| stage::stage(None, lenses::POKES, lenses::board, cx),
    },
    Scene {
        id: "lenses-glacier",
        title: "Lenses, Glacier",
        size: (1440, 760),
        build: |_, cx| stage::stage(Some(Appearance::Glacier), lenses::POKES, lenses::board, cx),
    },
    Scene {
        id: "data-dense",
        title: "Perf: a 600-tick comb and a 2 000-stone mosaic, one element each, filling a window",
        size: (1440, 900),
        build: |_, cx| stage::stage(None, &[], dense, cx),
    },
    Scene {
        id: "film-comb",
        title: "Film: the pointer sweeps a release comb tick by tick, then leaves (wave, then decay in place)",
        size: (1000, 200),
        build: |_, cx| stage::stage(None, &films::COMB_SWEEP, films::comb_scene, cx),
    },
    Scene {
        id: "film-comb-bare",
        title: "Film: the same sweep over a comb with no door (the comb's own tracks alone)",
        size: (1000, 200),
        build: |_, cx| stage::stage(None, &films::COMB_SWEEP, films::bare_comb_scene, cx),
    },
    Scene {
        id: "film-rose",
        title: "Film: rest on the rose's 'to' strands, then on group_head (peek), then leave",
        size: (1176, 1000),
        build: |_, cx| stage::stage(None, &films::ROSE_POKES, |w, _, cx| symbol::folio(w, None, cx), cx),
    },
    Scene {
        id: "film-mosaic",
        title: "Film: the pointer sweeps a row of stones",
        size: (400, 200),
        build: |_, cx| stage::stage(None, &films::MOSAIC_SWEEP, films::mosaic_scene, cx),
    },
    Scene {
        id: "film-gem",
        title: "Film: stages advance; the gem fills facet by facet, works, stalls amber, completes; the seam follows",
        size: (520, 200),
        build: |_, cx| stage::scripted(&[], &films::GEM_STEPS, true, films::gem_scene, cx),
    },
    Scene {
        id: "film-strands",
        title: "Film: written, via (dashed) and flowing (marching on the pulse) strands",
        size: (660, 340),
        build: |_, cx| stage::scripted(&[], &[], true, films::strands_scene, cx),
    },
    Scene {
        id: "film-rung",
        title: "Film: a comb's room shrinks step by step; its rung cross-fades card → row → tag → mark",
        size: (1000, 160),
        build: |_, cx| stage::scripted(&[], &films::RUNG_STEPS, false, films::rung_scene, cx),
    },
    Scene {
        id: "data-20k-mosaic",
        title: "Perf: a 20 000-stone mosaic of which the window shows about a tenth (culled to the visible rows)",
        size: (1440, 900),
        build: |_, cx| stage::stage(None, &[], dense_mosaic, cx),
    },
    Scene {
        id: "data-20k-map",
        title: "Perf: a territory of 400 regions and 20 000 stones on a 4400 x 3000 canvas; the window shows under a tenth",
        size: (1440, 900),
        build: |_, cx| stage::stage(None, &[], dense_map, cx),
    },
    Scene {
        id: "ladder",
        title: "Data marks at four rungs (relations, releases, one release) and x-ray: the same marks at rest and while ⌥ is held",
        size: (1440, 1400),
        build: |_, cx| stage::stage(None, &[], ladder::board, cx),
    },
    Scene {
        id: "symbol",
        title: "Symbol page head and rose band (calm): the reader column of a 1440 window (1176)",
        size: (1176, 1000),
        build: |_, cx| stage::stage(None, &[], |w, _, cx| symbol::folio(w, None, cx), cx),
    },
    Scene {
        id: "symbol-rose",
        title: "Rose with its 'to' direction rested: only that direction lights, in its hue, and names itself",
        size: (1176, 1000),
        build: |_, cx| stage::stage(None, &[], |w, _, cx| symbol::folio(w, Some(3), cx), cx),
    },
    Scene {
        id: "symbol-1100",
        title: "Symbol page at a 1100 window (reader 836 beside a 264 shelf)",
        size: (836, 1000),
        build: |_, cx| stage::stage(None, &[], |w, _, cx| symbol::folio(w, None, cx), cx),
    },
    Scene {
        id: "symbol-760",
        title: "Symbol page at a 760 window (reader 718 beside the kspine): the rose steps down",
        size: (718, 1000),
        build: |_, cx| stage::stage(None, &[], |w, _, cx| symbol::folio(w, None, cx), cx),
    },
    Scene {
        id: "symbol-480",
        title: "Symbol page at 480: the rose becomes four lines",
        size: (480, 1000),
        build: |_, cx| stage::stage(None, &[], |w, _, cx| symbol::folio(w, None, cx), cx),
    },
    Scene {
        id: "package",
        title: "Package folio (calm): facts, release comb + caption, lens bar, start line, territory; the reader column at 1176 (1440 window)",
        size: (1176, 1000),
        build: |_, cx| stage::stage(None, &[], |w, _, cx| package::folio(w, Some(super::Spot::Region(0)), cx), cx),
    },
    Scene {
        id: "package-900",
        title: "Package folio at a 900 window (reader 858)",
        size: (858, 1100),
        build: |_, cx| stage::stage(None, &[], |w, _, cx| package::folio(w, Some(super::Spot::Region(0)), cx), cx),
    },
    Scene {
        id: "data-marks",
        title: "Data marks: compass (mark/row/bar), comb at four rungs, file comb, mosaic, dense 600/2000",
        size: (1440, 1500),
        build: marks_abyss,
    },
    Scene {
        id: "data-marks-glacier",
        title: "Data marks, Glacier",
        size: (1440, 1500),
        build: marks_glacier,
    },
];

const H3: TypeRole = TypeRole {
    face: Face::Display,
    weight: 620.0,
    size: 16.0,
    line: 20.0,
    tracking: -0.01,
    italic: false,
};

fn marks_abyss(window: &mut Window, cx: &mut App) -> AnyView {
    board(None, window, cx, marks)
}

fn marks_glacier(window: &mut Window, cx: &mut App) -> AnyView {
    board(Some(Appearance::Glacier), window, cx, marks)
}

/// The release history the boards use: 64 releases, four majors, the pin
/// (mint), the one being read (peri), three sampled (hatched).
pub(crate) fn releases() -> Vec<Tick> {
    // The calm board's seed: 64 releases, your pin (44), the one you read
    // (46), two later releases that touch your code (52, 58).
    let mut rng = 7_u64;
    (0..64)
        .map(|i: usize| {
            rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            #[allow(clippy::cast_precision_loss)]
            let mut tick = Tick::new(6.0 + ((rng >> 33) % 10) as f32)
                .label(format!("1.0.{}", 147 + i))
                .split(1.0 + (i % 3) as f32, 1.0 + ((i * 5) % 4) as f32);
            match i {
                44 => {
                    tick = tick.tone(TickTone::Pin);
                    tick.height = 18.0;
                }
                46 => {
                    tick = tick.tone(TickTone::Current);
                    tick.height = 18.0;
                }
                52 | 58 => {
                    tick = tick.tone(TickTone::Touches);
                    tick.height = 14.0;
                }
                _ => {}
            }
            tick
        })
        .collect()
}

/// A package's stones, as the boards seed them.
pub(crate) fn stones(n: usize, seed: usize) -> Vec<Stone> {
    const CYCLE: [Family; 5] = [
        Family::Type,
        Family::Callable,
        Family::Callable,
        Family::Value,
        Family::Contract,
    ];
    (0..n)
        .map(|i| {
            let family = CYCLE[(i * 7 + seed) % CYCLE.len()];
            let r = (i * 13 + seed * 5) % 23;
            let state = match r {
                1..=7 => StoneState::Public,
                11 => StoneState::New,
                17 if seed % 2 == 0 => StoneState::Gated,
                19 if seed % 3 == 0 => StoneState::Gone,
                _ => StoneState::Private,
            };
            let stone = Stone::new(family, state);
            if r == 3 { stone.yours() } else { stone }
        })
        .collect()
}

fn heading(cx: &App, title: &'static str) -> Div {
    let facet = cx.facet();
    let palette = cx.palette();
    div().child(text(H3, facet, palette.ink0, title))
}

fn label(cx: &App, words: &'static str) -> Div {
    let facet = cx.facet();
    let palette = cx.palette();
    text(LABEL, facet, palette.ink4, words)
}

fn cell(cx: &App, words: &'static str, mark: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(div().min_h(px(34.0)).flex().items_center().child(mark))
        .child(label(cx, words))
}

const LABEL: TypeRole = TypeRole {
    weight: 500.0,
    size: 11.0,
    line: 14.0,
    ..ty::SMALL
};

/// Every data mark, calm at rest, with one rested or walked example each.
fn marks(_window: &mut Window, cx: &mut App) -> AnyElement {
    use super::{Has, Run, Stage, StageState as St, Tab, caps, facts, gem_progress, lens_bar, seam};
    use crate::icons::{Cap, Kind};
    let facet = cx.facet();
    let palette = cx.palette();
    let s = facet.text_scale;
    let m = |w: f32| facet.measure(px(w * s));
    let dirs = Directions::new(3, 3, 9, 4);
    let tip = Door::tip(|_, _, _, _| div().into_any_element());
    let row = || div().flex().flex_wrap().gap(px(36.0 * s)).items_end();

    let compasses = row()
        .child(cell(cx, "16", compass(dirs, &m(400.0)).size(super::CompassSize::S16)))
        .child(cell(cx, "18", compass(dirs, &m(400.0))))
        .child(cell(cx, "30", compass(dirs, &m(400.0)).size(super::CompassSize::S30)))
        .child(cell(
            cx,
            "rest: from",
            compass(dirs, &m(400.0))
                .size(super::CompassSize::S30)
                .id("c-rest")
                .door(tip.clone())
                .rest(Some(Dir::From)),
        ))
        .child(cell(cx, "⌥", compass(dirs, &m(400.0)).spell(true)))
        .child(cell(cx, "row", compass_row(dirs, &m(400.0), palette)))
        .child(cell(
            cx,
            "bar",
            div().w(px(340.0 * s)).child(
                compass_bar(Directions::new(5, 11, 26, 0), &m(340.0))
                    .id("bar")
                    .door(tip.clone())
                    .rest(Some(Dir::MadeOf)),
            ),
        ));

    let ticks = releases();
    let combs = div()
        .flex()
        .flex_col()
        .gap(px(24.0 * s))
        .child(
            row()
                .child(cell(cx, "mark", comb("r-mark", ticks.clone(), &m(80.0))))
                .child(cell(cx, "tag", comb("r-tag", ticks.clone(), &m(150.0))))
                .child(cell(cx, "row", comb("r-row", ticks.clone(), &m(300.0)).rest(Some(46))))
                .child(cell(
                    cx,
                    "files",
                    fcomb(
                        vec![
                            FileUses::new("render.rs", [9.0, 12.0, 8.0, 10.0, 7.0]).hot(),
                            FileUses::new("page.rs", [8.0, 11.0]),
                            FileUses::new("outline.rs", [9.0]),
                            FileUses::new("vendor.rs", [7.0]).theirs(),
                        ],
                        &m(200.0),
                    )
                    .id("fc")
                    .door(tip.clone())
                    .rest(Some(1)),
                )),
        )
        .child(cell(
            cx,
            "card",
            comb("r-card", ticks.clone(), &m(800.0))
                .caption([("19 releases since your pin, ", false), ("2", true), (" touch your code", false)])
                .door(tip.clone()),
        ));

    let mosaics = row()
        .child(cell(cx, "rest", mosaic("mo-a", stones(96, 3), &m(260.0)).door(tip.clone()).rest(Some(26))))
        .child(cell(cx, "⌥ only yours", mosaic("mo-b", stones(96, 4), &m(260.0)).dimmed(true)))
        .child(
            div()
                .h(px(120.0 * s))
                .child(
                    comb(
                        "spine",
                        (0..40usize)
                            .map(|i| {
                                #[allow(clippy::cast_precision_loss)]
                                Tick::new(6.0 + ((i * 11) % 14) as f32)
                            })
                            .collect::<Vec<_>>(),
                        &m(28.0),
                    )
                    .orientation(CombOrientation::Vertical)
                    .rung(Rung::Row)
                    .thickness(28.0)
                    .rest(Some(22)),
                ),
        );

    let st = |states: &[(St, f32)]| -> Vec<Stage> {
        ["resolve", "fetch", "index", "seal"]
            .iter()
            .zip(states)
            .map(|(n, (state, done))| Stage::new(*n, *state).done(*done))
            .collect()
    };
    let gems = row()
        .child(cell(cx, "to do", gem_progress("g0", Kind::Struct, st(&[(St::Todo, 0.0); 4]), &m(200.0)).size(40.0)))
        .child(cell(
            cx,
            "fetching",
            gem_progress("g1", Kind::Struct, st(&[(St::Done, 1.0), (St::Now, 0.5), (St::Todo, 0.0), (St::Todo, 0.0)]), &m(200.0)).size(40.0),
        ))
        .child(cell(
            cx,
            "stalled",
            gem_progress("g2", Kind::Struct, st(&[(St::Done, 1.0), (St::Done, 1.0), (St::Stall, 0.4), (St::Todo, 0.0)]), &m(200.0)).size(40.0),
        ))
        .child(cell(
            cx,
            "failed",
            gem_progress("g3", Kind::Struct, st(&[(St::Done, 1.0), (St::Bad, 1.0), (St::Todo, 0.0), (St::Todo, 0.0)]), &m(200.0)).size(40.0),
        ))
        .child(cell(
            cx,
            "done, rest: index",
            gem_progress("g4", Kind::Struct, st(&[(St::Done, 1.0); 4]), &m(200.0))
                .size(40.0)
                .door(tip.clone())
                .rest(Some(2)),
        ))
        .child(cell(
            cx,
            "seams",
            div()
                .w(px(300.0 * s))
                .flex()
                .flex_col()
                .gap(px(8.0 * s))
                .child(seam("s1", st(&[(St::Done, 1.0), (St::Now, 0.5), (St::Todo, 0.0), (St::Todo, 0.0)]), &m(300.0)))
                .child(seam("s2", st(&[(St::Done, 1.0), (St::Done, 1.0), (St::Stall, 0.4), (St::Todo, 0.0)]), &m(300.0)))
                .child(seam("s3", st(&[(St::Done, 1.0), (St::Bad, 1.0), (St::Todo, 0.0), (St::Todo, 0.0)]), &m(300.0))),
        ));

    let lines = row()
        .child(cell(
            cx,
            "caps, rest: hash",
            caps(
                "caps",
                vec![
                    (Cap::Clone, Has::On),
                    (Cap::Copy, Has::On),
                    (Cap::Debug, Has::On),
                    (Cap::Eq, Has::On),
                    (Cap::Hash, Has::On),
                    (Cap::Ord, Has::Off),
                    (Cap::Display, Has::Via),
                ],
                &m(300.0),
            )
            .door(tip.clone())
            .rest(Some(4)),
        ))
        .child(cell(
            cx,
            "facts, rest: since",
            facts(&m(420.0), palette)
                .fact([Run::Words("enum in ".into()), Run::Mono("present::glyph".into())])
                .fact([Run::Words("since ".into()), Run::Mono("0.3.0".into())])
                .fact([Run::Yours("9".into()), Run::Words(" uses in your code".into())])
                .door("facts", tip.clone())
                .rest(Some(1)),
        ))
        .child(cell(
            cx,
            "lens bar, rest: usage",
            lens_bar(
                "tabs",
                vec![Tab::new("Reference"), Tab::new("Relations").count("19"), Tab::new("Usage").count("9"), Tab::new("History").count("3")],
                0,
                &m(420.0),
            )
            .door(tip.clone())
            .rest(Some(2)),
        ));

    let column = doc(cx, "FACET V4 · DATA", "Data marks")
        .child(heading(cx, "Compass"))
        .child(compasses)
        .child(heading(cx, "Comb"))
        .child(combs)
        .child(heading(cx, "Mosaic and spine"))
        .child(mosaics)
        .child(heading(cx, "Progress"))
        .child(gems)
        .child(heading(cx, "Lines"))
        .child(lines);
    canvas(cx, column)
}

/// The perf scene: the densest marks, full width.
fn dense(width: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let w = (width - 80.0).max(200.0);
    let m = facet.measure(px(w));
    let ticks: Vec<Tick> = (0..600usize)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let mut t = Tick::new(10.0 + ((i * 29) % 31) as f32).split(1.0, 1.0);
            if i % 97 == 0 {
                t = t.major();
            }
            if i == 311 {
                t = t.tone(TickTone::Pin);
            }
            if i % 41 == 0 {
                t = t.tone(TickTone::Touches);
            }
            t
        })
        .collect();
    let tip = Door::tip(|_, _, _, _| div().into_any_element());
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(24.0))
        .p(px(40.0))
        .child(comb("dense-comb", ticks, &m).thickness(60.0).rung(Rung::Row).door(tip.clone()))
        .child(mosaic("dense-mosaic", stones(2000, 5), &m).door(tip))
        .into_any_element()
}

/// 20 000 stones in a column; about 9 % of it is on screen.
fn dense_mosaic(_width: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let m = facet.measure(px(460.0));
    div()
        .size_full()
        .p(px(40.0))
        .child(
            mosaic("mosaic-20k", stones(20_000, 5), &m)
                .door(Door::tip(|_, _, _, _| div().into_any_element())),
        )
        .into_any_element()
}

/// 400 modules, 20 000 stones, on a canvas three windows wide and tall.
fn dense_map(_width: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let regions: Vec<super::Region> = (0..400_usize)
        .map(|i| {
            let n = 10 + (i * 37) % 81;
            super::Region::new(format!("mod{i}"), n).reached((0..n).filter(|k| (k * 7 + i) % 23 == 0).collect::<Vec<_>>())
        })
        .collect();
    let total: usize = regions.iter().map(|r| r.items).sum();
    // Top the last region up so the map holds exactly 20 000 stones.
    let mut regions = regions;
    if let Some(last) = regions.last_mut() {
        last.items += 20_000_usize.saturating_sub(total);
    }
    div()
        .size_full()
        .child(
            super::territory("map-20k", regions, &facet.measure(px(4400.0)))
                .canvas(3000.0)
                .region_door(Door::tip(|_, _, _, _| div().into_any_element())),
        )
        .into_any_element()
}
