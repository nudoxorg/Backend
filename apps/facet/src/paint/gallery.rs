//! Paint scenes: `plates` (the Plates board's bevel states and sizes),
//! `gems` (families, kinds and the `DataMarks` "stone is progress" strip),
//! `ground` (the field alone), `cut-aa` (the antialiasing probe), and the
//! Glacier twins of `plates` and `gems`.

#![allow(clippy::too_many_lines)]

use super::{Bevel, Chamfer, Edge, GemState, Hatch, Plate, cut, gem, ground, hatch_fill};
use crate::fonts::Typeset;
use crate::gallery::Scene;
use crate::icons::{self, IconSize, Kind};
use crate::theme::{ActiveFacet, Facet, set_facet};
use crate::tokens::{Appearance, Face, Palette, Tone, TypeRole};
use gpui::{
    AnyElement, AnyView, App, AppContext, Context, Div, Hsla, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px,
};

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "plates",
        title: "Plates board: the cut plate's bevel states, sizes, lift, and floating plates",
        size: (1440, 760),
        build: plates_abyss,
    },
    Scene {
        id: "plates-glacier",
        title: "Plates, Glacier",
        size: (1440, 760),
        build: plates_glacier,
    },
    Scene {
        id: "gems",
        title: "Gems: every family, a size ramp 14-96, DataMarks 'the stone is progress', working and glint loops",
        size: (1440, 980),
        build: gems_abyss,
    },
    Scene {
        id: "gems-glacier",
        title: "Gems, Glacier",
        size: (1440, 980),
        build: gems_glacier,
    },
    Scene {
        id: "ground",
        title: "The faceted ground at 1440x900",
        size: (1440, 900),
        build: ground_scene,
    },
    Scene {
        id: "ambient",
        title: "Ambient motion on the pulse: ground twinkle, running bevels, working and glinting gems, running hatch (film it)",
        size: (960, 420),
        build: ambient,
    },
    Scene {
        id: "cut-aa",
        title: "Antialiasing probe: plates and a gem for zoom crops at 1x and 2x",
        size: (520, 160),
        build: cut_aa,
    },
];

/// A scene root that renders a plain function.
pub(crate) struct Board {
    content: fn(&mut Window, &mut App) -> AnyElement,
}

impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        (self.content)(window, cx)
    }
}

/// A scene view. `Some(appearance)` pins the appearance (the `-glacier`
/// twins); `None` keeps whatever the capture asked for (`--theme`).
pub(crate) fn board(
    appearance: Option<Appearance>,
    window: &mut Window,
    cx: &mut App,
    content: fn(&mut Window, &mut App) -> AnyElement,
) -> AnyView {
    let _ = window;
    if let Some(appearance) = appearance {
        set_facet(
            Facet {
                appearance,
                ..cx.facet()
            },
            cx,
        );
    }
    cx.new(|_| Board { content }).into()
}

/// A board-local type role.
pub(crate) const fn role(face: Face, weight: f32, size: f32, line: f32, tracking: f32) -> TypeRole {
    TypeRole {
        face,
        weight,
        size,
        line,
        tracking,
        italic: matches!(face, Face::Serif),
    }
}

const H1: TypeRole = role(Face::Display, 640.0, 46.0, 50.0, -0.035);
const H2: TypeRole = role(Face::Display, 620.0, 22.0, 28.0, -0.02);
const SUB: TypeRole = role(Face::Serif, 400.0, 14.5, 21.0, 0.0);
const DOC_NO: TypeRole = role(Face::Mono, 400.0, 12.0, 16.0, 0.06);
const PSTATE_B: TypeRole = role(Face::Display, 620.0, 13.0, 18.0, 0.0);
const PSTATE_SPAN: TypeRole = role(Face::Serif, 400.0, 11.5, 16.0, 0.0);
const CLABEL: TypeRole = role(Face::Mono, 400.0, 10.5, 10.5, 0.0);
const GEMCAP: TypeRole = role(Face::Mono, 400.0, 10.0, 10.0, 0.0);
const ROW: TypeRole = role(Face::Ui, 500.0, 13.0, 18.0, 0.0);
const FAMILY: TypeRole = role(Face::Ui, 600.0, 13.5, 18.0, 0.0);

pub(crate) fn text(
    role: TypeRole,
    facet: Facet,
    color: Tone,
    body: impl Into<SharedString>,
) -> Div {
    div()
        .typeset(role, &facet)
        .text_color(Hsla::from(color))
        .child(body.into())
}

/// The `.doc` column: padding 56/64, a doc number and a title. Wrap the
/// finished column in [`canvas`].
pub(crate) fn doc(cx: &App, number: &'static str, title: &'static str) -> Div {
    let facet = cx.facet();
    let palette = cx.palette();
    div()
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(28.0))
        .px(px(64.0))
        .py(px(56.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(text(DOC_NO, facet, palette.mint.base, number))
                .child(text(H1, facet, palette.ink0, title)),
        )
}

/// The window ground behind a finished `.doc` column.
pub(crate) fn canvas(cx: &App, column: Div) -> AnyElement {
    let palette = cx.palette();
    div()
        .size_full()
        .relative()
        .bg(Hsla::from(palette.g1))
        .child(ground())
        .child(column)
        .into_any_element()
}

pub(crate) fn section(cx: &App, title: &'static str, sub: &'static str) -> Div {
    let facet = cx.facet();
    let palette = cx.palette();
    div()
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(text(H2, facet, palette.ink0, title))
        .child(text(SUB, facet, palette.ink2, sub).max_w(px(640.0)))
}

fn plates_abyss(window: &mut Window, cx: &mut App) -> AnyView {
    board(None, window, cx, plates)
}

fn plates_glacier(window: &mut Window, cx: &mut App) -> AnyView {
    board(Some(Appearance::Glacier), window, cx, plates)
}

fn plates(_window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = cx.palette();
    let pstate = |bevel: Bevel, plate: Plate, title: &'static str, caption: &'static str| {
        cut()
            .chamfer(Chamfer::Sm)
            .bevel(bevel)
            .plate(plate)
            // `.cut.run` at the board capture's virtual time (2.5 s of 1.6 s).
            .phase(0.5625)
            .flex()
            .flex_col()
            .gap(px(5.0))
            .px(px(15.0))
            .py(px(13.0))
            .min_w(px(112.0))
            .child(text(PSTATE_B, facet, palette.ink0, title))
            .child(text(PSTATE_SPAN, facet, palette.ink2, caption))
    };
    let states = div()
        .flex()
        .flex_wrap()
        .gap(px(16.0))
        .child(pstate(
            Bevel::Rest,
            Plate::Flat,
            "Rest",
            "lit top-left, shaded bottom-right",
        ))
        .child(pstate(
            Bevel::Focus,
            Plate::Flat,
            "Focus",
            "doubled, periwinkle",
        ))
        .child(pstate(
            Bevel::Hot,
            Plate::Flat,
            "Yours",
            "mint: your code reaches this",
        ))
        .child(pstate(
            Bevel::Run,
            Plate::Flat,
            "Working",
            "light travels the edge",
        ))
        .child(pstate(
            Bevel::Amber,
            Plate::Flat,
            "Waiting",
            "amber, and still",
        ))
        .child(pstate(
            Bevel::Coral,
            Plate::Flat,
            "Stopped",
            "coral, with one reason",
        ))
        .child(pstate(
            Bevel::Ghost,
            Plate::Flat,
            "Pending",
            "hatched: sampled, not sealed",
        ))
        .child(pstate(
            Bevel::Rest,
            Plate::Two,
            "Two",
            "a header tone over a body tone",
        ))
        .child(pstate(
            Bevel::Rest,
            Plate::Deep,
            "Deep",
            "recessed to table level",
        ));

    let size_box = |chamfer: Chamfer, side: f32, lift: f32, label: &'static str| {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .child(cut().chamfer(chamfer).lift(lift).w(px(side)).h(px(side)))
            .child(text(CLABEL, facet, palette.ink3, label))
    };
    let sizes = div()
        .flex()
        .items_end()
        .gap(px(22.0))
        .child(size_box(Chamfer::Sm, 64.0, 0.0, "sm · 9px cut"))
        .child(size_box(Chamfer::Md, 80.0, 0.0, "default · 14px cut"))
        .child(size_box(Chamfer::Lg, 96.0, 0.0, "lg · 22px cut"))
        .child(size_box(Chamfer::Sm, 64.0, 1.0, ".lift, hovered"));

    // Beyond the board row: a mid-transition edge, weave, and floating plates.
    let rest = Edge::of(Bevel::Rest, palette);
    let focus = Edge::of(Bevel::Focus, palette);
    let blend = |t: f32, label: &'static str| {
        cut()
            .chamfer(Chamfer::Sm)
            .edge(rest.mix(focus, t))
            .w(px(96.0))
            .h(px(48.0))
            .flex()
            .items_center()
            .justify_center()
            .child(text(CLABEL, facet, palette.ink3, label))
    };
    let extras = div()
        .flex()
        .items_start()
        .gap(px(22.0))
        .child(blend(0.0, "rest"))
        .child(blend(0.35, "t .35"))
        .child(blend(0.7, "t .7"))
        .child(blend(1.0, "focus"))
        .child(
            cut()
                .chamfer(Chamfer::Sm)
                .plate(Plate::Weave)
                .w(px(140.0))
                .h(px(48.0))
                .flex()
                .items_center()
                .justify_center()
                .child(text(CLABEL, facet, palette.ink3, "weave")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(text(
                    CLABEL,
                    facet,
                    palette.ink3,
                    "hatch · pending, running",
                ))
                .child(
                    hatch_fill(Hatch::pending(), palette.ink2.into())
                        .w(px(120.0))
                        .h(px(10.0)),
                )
                .child(
                    hatch_fill(Hatch::seam().phase(0.5), palette.mint.base.into())
                        .w(px(120.0))
                        .h(px(2.0)),
                )
                .child(
                    hatch_fill(Hatch::diagonal(2.0, 4.0), palette.coral.base.into())
                        .chamfer(3.0)
                        .w(px(11.0))
                        .h(px(11.0)),
                ),
        )
        .child(
            cut()
                .chamfer(Chamfer::Float)
                .floating()
                .fill(palette.glass)
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(14.0))
                .py(px(12.0))
                .child(gem(Kind::Enum).size(24.0))
                .child(text(ROW, facet, palette.ink0, "RelationLabel")),
        )
        .child(
            cut()
                .chamfer(Chamfer::Float)
                .floating()
                .fill(palette.glass)
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(14.0))
                .py(px(10.0))
                .child(icons::ui(
                    icons::Icon::Diamond,
                    IconSize::S14,
                    palette.mint.base,
                ))
                .child(text(ROW, facet, palette.ink0, "Unpinned RelationLabel"))
                .child(text(ROW, facet, palette.peri.base, "Undo")),
        );

    let mut runs = div().flex().items_center().gap(px(12.0)).child(text(
        CLABEL,
        facet,
        palette.ink3,
        "run, 8 phases of 1.6 s",
    ));
    for step in 0..8u8 {
        runs = runs.child(
            cut()
                .chamfer(Chamfer::Sm)
                .bevel(Bevel::Run)
                .phase(f32::from(step) / 8.0)
                .w(px(96.0))
                .h(px(40.0)),
        );
    }
    let column = doc(cx, "FACET 11", "Plates").child(
            section(
                cx,
                "The cut plate: sizes, lift, and what each bevel means",
                "One surface, one family of edges. Everything raised in Nudox is this same chamfer, only the corner size and the bevel colour change.",
            )
            .child(states)
            .child(sizes)
            .child(extras)
            .child(runs),
    );
    canvas(cx, column)
}

fn gems_abyss(window: &mut Window, cx: &mut App) -> AnyView {
    board(None, window, cx, gems)
}

fn gems_glacier(window: &mut Window, cx: &mut App) -> AnyView {
    board(Some(Appearance::Glacier), window, cx, gems)
}

fn gem_cell(
    facet: Facet,
    palette: &Palette,
    stone: impl IntoElement,
    caption: &'static str,
) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(7.0))
        .child(stone)
        .child(text(GEMCAP, facet, palette.ink3, caption))
}

fn gems(_window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = cx.palette();

    let families: [(&str, &[Kind]); 5] = [
        (
            "Namespaces",
            &[Kind::Module, Kind::Package, Kind::Import, Kind::Unknown],
        ),
        (
            "Types",
            &[
                Kind::Struct,
                Kind::Class,
                Kind::Enum,
                Kind::Union,
                Kind::Type,
            ],
        ),
        ("Contracts", &[Kind::Trait, Kind::Interface]),
        (
            "Callables",
            &[Kind::Function, Kind::Method, Kind::Constructor, Kind::Macro],
        ),
        (
            "Values",
            &[
                Kind::Constant,
                Kind::Field,
                Kind::Property,
                Kind::Variable,
                Kind::Variant,
            ],
        ),
    ];
    let mut family_rows = div().flex().flex_col().gap(px(14.0));
    for (name, kinds) in families {
        let hue = kinds[0].hue(palette);
        let mut row = div().flex().items_center().gap(px(20.0)).child(
            div()
                .w(px(120.0))
                .typeset(FAMILY, &facet)
                .text_color(hue)
                .child(name),
        );
        for &kind in kinds {
            row = row.child(gem_cell(facet, palette, gem(kind).size(44.0), kind.name()));
        }
        family_rows = family_rows.child(row);
    }

    let strip = div()
        .flex()
        .items_end()
        .gap(px(20.0))
        .child(gem_cell(
            facet,
            palette,
            gem(Kind::Package).state(GemState::Hollow),
            "hollow",
        ))
        .child(gem_cell(
            facet,
            palette,
            gem(Kind::Package).progress(0.0),
            "0 of 12",
        ))
        .child(gem_cell(
            facet,
            palette,
            gem(Kind::Package).progress(4.0),
            "4 of 12",
        ))
        .child(gem_cell(
            facet,
            palette,
            gem(Kind::Package)
                .progress(8.0)
                .state(GemState::Working)
                .phase(0.0),
            "working",
        ))
        .child(gem_cell(facet, palette, gem(Kind::Package), "12 of 12"))
        .child(gem_cell(
            facet,
            palette,
            gem(Kind::Package).state(GemState::Stalled),
            "stalled",
        ))
        .child(gem_cell(
            facet,
            palette,
            gem(Kind::Package).state(GemState::Cracked),
            "cracked",
        ))
        .child(gem_cell(
            facet,
            palette,
            gem(Kind::Package).state(GemState::Glint).phase(0.87),
            "glint",
        ));

    let mut ramp = div().flex().items_end().gap(px(20.0));
    for (size, label) in [
        (14.0, "14"),
        (18.0, "18"),
        (24.0, "24"),
        (32.0, "32"),
        (48.0, "48"),
        (64.0, "64"),
        (96.0, "96"),
    ] {
        ramp = ramp.child(gem_cell(
            facet,
            palette,
            gem(Kind::Struct).size(size),
            label,
        ));
    }
    let mut small_states = div().flex().items_end().gap(px(14.0));
    for (state, progress) in [
        (GemState::Hollow, 12.0),
        (GemState::Normal, 0.0),
        (GemState::Normal, 4.0),
        (GemState::Normal, 12.0),
        (GemState::Stalled, 12.0),
        (GemState::Cracked, 12.0),
    ] {
        small_states =
            small_states.child(gem(Kind::Trait).size(18.0).state(state).progress(progress));
    }
    ramp = ramp.child(gem_cell(facet, palette, small_states, "18 px states"));

    let mut working = div().flex().items_end().gap(px(12.0));
    for step in 0..12u8 {
        let phase = f32::from(step) / 12.0 + 0.02;
        working = working.child(
            gem(Kind::Function)
                .size(36.0)
                .progress(10.0)
                .state(GemState::Working)
                .phase(phase),
        );
    }
    let mut glint = div().flex().items_end().gap(px(12.0));
    for step in 0..8u8 {
        let phase = 0.8 + f32::from(step) * 0.025;
        glint = glint.child(
            gem(Kind::Constant)
                .size(36.0)
                .state(GemState::Glint)
                .phase(phase),
        );
    }
    let loops = div()
        .flex()
        .gap(px(40.0))
        .child(gem_cell(
            facet,
            palette,
            working,
            "working, 12 phases of 2.4 s (10 of 12 lit)",
        ))
        .child(gem_cell(
            facet,
            palette,
            glint,
            "glint, 80 %..97.5 % of 5 s",
        ));

    let column = doc(cx, "FACET 12", "Gems")
        .child(
            section(
                cx,
                "Hue is the family, shape is the kind",
                "Twelve flat facets lit from the top-left, a table at table level, the kind's glyph at its centre.",
            )
            .child(family_rows),
        )
        .child(
            section(
                cx,
                "The stone is progress",
                "A gem does not sit beside a progress bar, the gem is the progress bar, facet by facet.",
            )
            .child(strip)
            .child(ramp)
            .child(loops),
        );
    canvas(cx, column)
}

fn ground_scene(window: &mut Window, cx: &mut App) -> AnyView {
    board(None, window, cx, |_window, cx| {
        let palette = cx.palette();
        div()
            .size_full()
            .relative()
            .bg(Hsla::from(palette.g1))
            .child(ground())
            .into_any_element()
    })
}

fn cut_aa(window: &mut Window, cx: &mut App) -> AnyView {
    board(None, window, cx, |_window, cx| {
        let palette = cx.palette();
        let plate = |bevel: Bevel| {
            cut()
                .chamfer(Chamfer::Md)
                .bevel(bevel)
                .w(px(140.0))
                .h(px(64.0))
        };
        div()
            .size_full()
            .bg(Hsla::from(palette.g1))
            .flex()
            .items_center()
            .gap(px(20.0))
            .px(px(20.0))
            .child(plate(Bevel::Rest))
            .child(plate(Bevel::Focus))
            .child(plate(Bevel::Hot))
            .child(gem(Kind::Trait).size(64.0))
            .into_any_element()
    })
}

fn ambient(window: &mut Window, cx: &mut App) -> AnyView {
    crate::motion::pulse::thaw(cx);
    board(None, window, cx, |window, cx| {
        let pulse = crate::motion::pulse::lease(window, cx);
        let facet = cx.facet();
        let palette = cx.palette();
        let run = pulse.phase(1.6);
        let plate = |w: f32, h: f32, chamfer: Chamfer| {
            cut()
                .chamfer(chamfer)
                .bevel(Bevel::Run)
                .phase(run)
                .w(px(w))
                .h(px(h))
        };
        div()
            .size_full()
            .relative()
            .bg(Hsla::from(palette.g1))
            .child(ground().phase(pulse.phase(7.0)))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(28.0))
                    .p(px(40.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(24.0))
                            .child(plate(188.0, 64.0, Chamfer::Sm))
                            .child(plate(96.0, 96.0, Chamfer::Lg))
                            .child(plate(140.0, 30.0, Chamfer::Button)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(24.0))
                            .child(
                                gem(Kind::Package)
                                    .progress(8.0)
                                    .state(GemState::Working)
                                    .phase(pulse.phase(2.4)),
                            )
                            .child(
                                gem(Kind::Trait)
                                    .state(GemState::Glint)
                                    .phase(pulse.phase(5.0)),
                            )
                            .child(
                                gem(Kind::Function)
                                    .size(24.0)
                                    .progress(12.0)
                                    .state(GemState::Working)
                                    .phase(pulse.phase(2.4)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(text(CLABEL, facet, palette.ink3, "seam-run and hatch-run"))
                            .child(
                                hatch_fill(
                                    Hatch::seam().phase(pulse.phase(0.5)),
                                    palette.mint.base.into(),
                                )
                                .w(px(240.0))
                                .h(px(2.0)),
                            )
                            .child(
                                hatch_fill(
                                    Hatch::pending().phase(pulse.phase(0.25)),
                                    palette.ink2.into(),
                                )
                                .w(px(240.0))
                                .h(px(12.0)),
                            ),
                    ),
            )
            .into_any_element()
    })
}
