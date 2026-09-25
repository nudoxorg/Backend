//! Icon scenes: `marks` (the Language board's kind, modifier, capability
//! and language marks), `icons` (all 50 UI icons at 14/18/24 and the
//! chevron), `logo` (the lockup and the chevron diamond at their sizes).

#![allow(clippy::too_many_lines)]

use super::{
    Cap, CapState, Icon, IconSize, Kind, KindSize, Lang, Mod, cap_mark, chevron, kind_mark,
    lang_mark, logo, mod_mark, ui,
};
use crate::gallery::Scene;
use crate::paint::gallery::{board, canvas, doc, role, section, text};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, Family, TypeRole};
use gpui::{AnyElement, AnyView, App, Hsla, IntoElement, ParentElement, Styled, Window, div, px};

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "marks",
        title: "Language board: kind marks by family, modifier marks, capability marks, language marks",
        size: (1440, 1000),
        build: marks_scene,
    },
    Scene {
        id: "icons",
        title: "All 50 UI icons at 14, 18 and 24 px, and the chevron",
        size: (1440, 560),
        build: icons_scene,
    },
    Scene {
        id: "logo",
        title: "Brand board: the owner's lockup from 48 px, the chevron diamond below",
        size: (900, 420),
        build: logo_scene,
    },
];

const KCAP: TypeRole = role(Face::Mono, 400.0, 10.5, 13.0, 0.0);
const FAM: TypeRole = role(Face::Ui, 600.0, 14.0, 18.0, 0.0);
const FAMSUB: TypeRole = role(Face::Serif, 400.0, 12.0, 16.0, 0.0);
const FACT: TypeRole = role(Face::Ui, 400.0, 12.5, 16.0, 0.0);
const LANGCAP: TypeRole = role(Face::Ui, 400.0, 10.5, 13.0, 0.0);

fn marks_scene(window: &mut Window, cx: &mut App) -> AnyView {
    board(None, window, cx, marks)
}

fn marks(_window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let palette = cx.palette();
    let families: [(Family, &str, &str); 5] = [
        (Family::Namespace, "Namespaces", "frames that hold things"),
        (Family::Type, "Types", "cut boxes with storage"),
        (Family::Contract, "Contracts", "hollow diamonds: promises"),
        (Family::Callable, "Callables", "chevrons: things that run"),
        (Family::Value, "Values", "stones and lines: things that are"),
    ];
    let mut kinds = div().flex().flex_col().w(px(670.0));
    for (family, name, sub) in families {
        let hue = palette.family(family).hue;
        let mut row = div()
            .flex()
            .items_center()
            .border_t_1()
            .border_color(Hsla::from(palette.line1))
            .py(px(12.0))
            .child(
                div()
                    .w(px(180.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(text(FAM, facet, hue, name))
                    .child(text(FAMSUB, facet, palette.ink3, sub)),
            );
        for kind in Kind::ALL.iter().copied().filter(|k| k.family() == family) {
            row = row.child(
                div()
                    .w(px(82.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .child(kind_mark(kind, KindSize::Lg, palette))
                    .child(text(KCAP, facet, palette.ink3, kind.name())),
            );
        }
        kinds = kinds.child(row);
    }
    // Every kind at the three mark sizes, too.
    let mut ladder = div().flex().flex_col().gap(px(8.0)).pt(px(14.0));
    for size in [KindSize::Sm, KindSize::Md, KindSize::Lg] {
        let mut row = div().flex().items_center().gap(px(6.0));
        for kind in Kind::ALL {
            row = row.child(kind_mark(kind, size, palette));
        }
        ladder = ladder.child(row);
    }
    kinds = kinds.child(ladder);

    let mut facts = div().flex().flex_wrap().w(px(560.0)).gap_y(px(14.0));
    for m in Mod::ALL {
        facts = facts.child(
            div()
                .w(px(280.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(mod_mark(m, 18.0, palette).text_color(Hsla::from(palette.ink1)))
                .child(text(FACT, facet, palette.ink2, m.tooltip())),
        );
    }
    let mut small_mods = div().flex().items_center().gap(px(8.0));
    for m in Mod::ALL {
        small_mods = small_mods.child(mod_mark(m, 15.0, palette));
    }

    let mut caps = div().flex().items_start().gap(px(2.0));
    for (i, cap) in Cap::ALL.into_iter().enumerate() {
        let state = match i {
            0..=6 => CapState::On,
            7 | 8 => CapState::Via,
            _ => CapState::Off,
        };
        caps = caps.child(
            div()
                .w(px(42.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.0))
                .child(cap_mark(cap, state, palette))
                .child(text(KCAP, facet, palette.ink3, cap.name()).whitespace_nowrap()),
        );
    }

    let mut langs = div().flex().gap(px(22.0));
    for lang in Lang::ALL {
        langs = langs.child(
            div()
                .w(px(64.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(7.0))
                .child(lang_mark(lang, 14.0))
                .child(text(LANGCAP, facet, palette.ink3, lang.ecosystem())),
        );
    }
    let mut big_langs = div().flex().gap(px(18.0));
    for lang in Lang::ALL {
        big_langs = big_langs.child(lang_mark(lang, 28.0));
    }

    let column = doc(cx, "FACET 02", "The information language").child(
            div()
                .flex()
                .gap(px(60.0))
                .child(
                    section(
                        cx,
                        "What a thing is",
                        "Hue is the family, shape is the kind. Twenty marks, no letters, no tiles.",
                    )
                    .child(kinds),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(28.0))
                        .child(
                            section(
                                cx,
                                "What is true of it",
                                "Every uppercase label the old design wore is now a mark with a tooltip.",
                            )
                            .child(facts)
                            .child(small_mods),
                        )
                        .child(
                            section(
                                cx,
                                "What you can do with it",
                                "Lit is written or derived, dashed arrives through a blanket or auto impl, dim is a closed door.",
                            )
                            .child(caps),
                        )
                        .child(
                            section(cx, "Language marks", "One mark per ecosystem, coloured, never captioned twice.")
                                .child(langs)
                                .child(big_langs),
                        ),
                ),
        );
    canvas(cx, column)
}

fn icons_scene(window: &mut Window, cx: &mut App) -> AnyView {
    board(None, window, cx, |_window, cx| {
        let palette = cx.palette();
        let mut rows = div().flex().flex_col().gap(px(18.0));
        for size in [IconSize::S14, IconSize::S18, IconSize::S24] {
            let mut row = div().flex().flex_wrap().gap(px(18.0)).w(px(1300.0));
            for icon in Icon::ALL {
                row = row.child(ui(icon, size, palette.ink1));
            }
            row = row.child(chevron(size, palette.mint.base));
            rows = rows.child(row);
        }
        let mut ladder = div().flex().items_end().gap(px(16.0));
        for size in IconSize::ALL {
            ladder = ladder.child(ui(Icon::Search, size, palette.ink1));
            ladder = ladder.child(ui(Icon::Package, size, palette.ink1));
        }
        canvas(
            cx,
            doc(cx, "FACET 02", "Icons are cut, not drawn")
                .child(rows)
                .child(ladder),
        )
    })
}

fn logo_scene(window: &mut Window, cx: &mut App) -> AnyView {
    board(None, window, cx, |_window, cx| {
        let palette = cx.palette();
        let mut row = div().flex().items_end().gap(px(28.0));
        for size in [240.0, 96.0, 64.0, 56.0, 48.0, 40.0, 28.0, 20.0] {
            row = row.child(logo(size));
        }
        div()
            .size_full()
            .bg(Hsla::from(palette.g1))
            .p(px(40.0))
            .child(row)
            .into_any_element()
    })
}
