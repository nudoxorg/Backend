//! Marks scenes: the four boards (every state of every mark, cards shown
//! in place), the package hero at any size (cards opened through the real
//! float layer, one at a time), three films (a card unfurling and sweeping,
//! a copy, a scrub) and the mono ligature check. All data is the pinned
//! fixture (`fixture.json`); the boards reproduce `v4/marks/Marks.html` and
//! the heroes `v4/marks/Hero.html`.

#![allow(clippy::too_many_lines)]

use super::card;
use super::deps::{self, DepKind, dep_line, dep_link};
use super::eco::{self, ecosystem_mark};
use super::fixture;
use super::license::{self, license_mark};
use super::version::{self, version_mark};
use crate::fluid::Modes;
use crate::gallery::Scene;
use crate::icons::Kind;
use crate::measure::{Measure, Set};
use crate::overlay::float;
use crate::paint::{gem, ground};
use crate::theme::ActiveFacet;
use crate::tokens::fluid::{DOCK, Dock, HERO_DEPS, MARK_GEM, MOCK_SHELF, Split};
use crate::tokens::{Face, TypeRole, geo, ty};
use gpui::{
    AnyElement, AnyView, App, AppContext, Context, ElementId, FontFeatures, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px,
};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "marks-eco",
        title: "Ecosystem marks: at rest, in rows (quiet), and every card (in place)",
        size: (1440, 1254),
        build: eco_board,
    },
    Scene {
        id: "marks-license",
        title: "License marks: the families, choices and links, and their cards (in place)",
        size: (1440, 1300),
        build: license_board,
    },
    Scene {
        id: "marks-version",
        title: "Version: Rider, Baseline and Band on toml's 120 releases, and the cards",
        size: (1440, 1500),
        build: version_board,
    },
    Scene {
        id: "marks-deps",
        title: "Depends on: the folded line, ⌥, and every card (in place)",
        size: (1440, 1250),
        build: deps_board,
    },
    Scene {
        id: "marks-hero-toml",
        title: "Hero: toml (registry crate, two copies in the tree)",
        size: (1440, 900),
        build: hero_toml,
    },
    Scene {
        id: "marks-hero-serde",
        title: "Hero: serde (up to date, 316 releases, size unknown)",
        size: (1440, 900),
        build: hero_serde,
    },
    Scene {
        id: "marks-hero-present",
        title: "Hero: present (your own project: local, the tree's licenses)",
        size: (1440, 900),
        build: hero_present,
    },
    Scene {
        id: "marks-hero-react",
        title: "Hero: @types/react (npm, not in your tree)",
        size: (1440, 900),
        build: hero_react,
    },
    Scene {
        id: "marks-hero-smallvec",
        title: "Hero: smallvec (one release behind, no API changes)",
        size: (1440, 900),
        build: hero_smallvec,
    },
    Scene {
        id: "marks-hero-toml-eco",
        title: "Hero toml, the ecosystem card open (float)",
        size: (1440, 900),
        build: hero_toml_eco,
    },
    Scene {
        id: "marks-hero-toml-lic",
        title: "Hero toml, the license card open (float)",
        size: (1440, 900),
        build: hero_toml_lic,
    },
    Scene {
        id: "marks-hero-toml-ver",
        title: "Hero toml, the number's card open (float)",
        size: (1440, 900),
        build: hero_toml_ver,
    },
    Scene {
        id: "marks-hero-toml-also",
        title: "Hero toml, the 1.1.5 tooth's card open (float)",
        size: (1440, 900),
        build: hero_toml_also,
    },
    Scene {
        id: "marks-hero-toml-dep",
        title: "Hero toml, toml_edit's card open (float)",
        size: (1440, 900),
        build: hero_toml_dep,
    },
    Scene {
        id: "marks-card-film",
        title: "Film: rest on toml_edit at 100 (open 450), sweep to serde at 1150, leave at 1750",
        size: (1440, 900),
        build: card_film,
    },
    Scene {
        id: "marks-copy-film",
        title: "Film: the card rests open (350..670), the install line is copied at 900",
        size: (1440, 900),
        build: copy_film,
    },
    Scene {
        id: "marks-scrub-film",
        title: "Film: the rider walks from 0.8.23 to 1.1.6 at 100, and home at 1500",
        size: (1440, 900),
        build: scrub_film,
    },
    Scene {
        id: "marks-lens-film",
        title: "Film: the pointer sweeps serde's 316 releases; the lens opens under the Rider and the number holds",
        size: (1440, 900),
        build: lens_film,
    },
    Scene {
        id: "marks-calt",
        title: "Geist Mono: contextual alternates on (default) and off, `--version`",
        size: (760, 780),
        build: calt,
    },
];

pub(crate) type Build = fn(&Measure, &mut Window, &mut App) -> AnyElement;

pub(crate) struct Stage {
    build: Build,
    focus: Option<gpui::FocusHandle>,
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let measure = facet.measure(window.viewport_size().width);
        // The stage holds focus until Tab walks into the marks (a scene
        // plays the shell's part: Tab moves focus, shift-Tab back).
        let focus = self.focus.get_or_insert_with(|| {
            let handle = cx.focus_handle();
            window.focus(&handle, cx);
            handle
        });
        let content = (self.build)(&measure, window, cx);
        div()
            .track_focus(focus)
            .on_key_down(|event: &gpui::KeyDownEvent, window, cx| {
                match (
                    event.keystroke.modifiers.shift,
                    event.keystroke.key.as_str(),
                ) {
                    (false, "tab") => window.focus_next(cx),
                    (true, "tab") => window.focus_prev(cx),
                    _ => {}
                }
            })
            .relative()
            .size_full()
            .bg(palette.g1.hsla())
            .child(ground())
            .child(content)
            .child(float::layer(window, cx))
    }
}

pub(crate) fn stage(build: Build, cx: &mut App) -> AnyView {
    cx.new(|_| Stage { build, focus: None }).into()
}

const BOARD_TITLE: TypeRole = TypeRole {
    face: Face::Display,
    weight: 640.0,
    size: 32.0,
    line: 38.0,
    tracking: -0.02,
    italic: false,
};
const SECTION: TypeRole = TypeRole {
    face: Face::Display,
    weight: 620.0,
    size: 19.0,
    line: 24.0,
    tracking: -0.01,
    italic: false,
};
const LABEL: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 400.0,
    size: 12.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

fn board(
    title: &'static str,
    section: &'static str,
    rows: Vec<AnyElement>,
    measure: &Measure,
    cx: &App,
) -> AnyElement {
    let palette = cx.facet().palette();
    div()
        .absolute()
        .left(px(52.0))
        .top(px(40.0))
        .w(px(1336.0))
        .flex()
        .flex_col()
        .gap(px(28.0))
        .child(
            div()
                .set(BOARD_TITLE, measure)
                .text_color(palette.ink0.hsla())
                .child(title),
        )
        .child(
            div()
                .set(SECTION, measure)
                .text_color(palette.ink0.hsla())
                .child(section),
        )
        .children(rows)
        .into_any_element()
}

fn row(cells: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(40.0))
        .children(cells)
        .into_any_element()
}

fn cell(
    label: impl Into<SharedString>,
    width: f32,
    body: impl IntoElement,
    measure: &Measure,
    cx: &App,
) -> AnyElement {
    let palette = cx.facet().palette();
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(14.0))
        .child(
            div()
                .set(LABEL, measure)
                .text_color(palette.ink3.hsla())
                .child(label.into()),
        )
        .child(body)
        .into_any_element()
}

fn line(children: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(30.0))
        .gap_y(px(18.0))
        .children(children)
        .into_any_element()
}

// ------------------------------------------------------------------ boards

const ECOS: [&str; 7] = [
    "toml",
    "@types/react",
    "tomli",
    "github.com/BurntSushi/toml",
    "joda-time",
    "Polyfill",
    "tomlplusplus",
];

fn eco_board(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, window, cx| {
            let palette = cx.facet().palette();
            let m = measure.within(px(400.0));
            let rest = line(
                ECOS.iter()
                    .enumerate()
                    .map(|(i, p)| {
                        ecosystem_mark(("eco-rest", i), fixture::eco(p), &m).into_any_element()
                    })
                    .collect(),
            );
            let rows = div()
                .flex()
                .flex_col()
                .children(ECOS.iter().enumerate().map(|(i, p)| {
                    let pkg = fixture::package(p);
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .h(px(30.0))
                        .child(ecosystem_mark(("eco-row", i), fixture::eco(p), &m).quiet())
                        .child(
                            div()
                                .set(ty::MONO_ROW, &m)
                                .text_color(palette.ink1.hsla())
                                .child(pkg.name.clone()),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .set(ty::MONO_SMALL, &m)
                                .text_color(palette.ink4.hsla())
                                .child(pkg.version.clone().unwrap_or_default()),
                        )
                }));
            let open = |i: usize, p: &str, window: &mut Window, cx: &mut App| {
                let facts = fixture::eco(p);
                let content = eco::board_card(&facts);
                div()
                    .flex()
                    .flex_col()
                    .child(ecosystem_mark(("eco-open", i), facts, &m))
                    .child(card::in_place(&content, window, cx))
                    .into_any_element()
            };
            let mut cards = Vec::new();
            for (i, p) in [
                "toml",
                "present",
                "@types/react",
                "tomli",
                "github.com/BurntSushi/toml",
                "joda-time",
                "Polyfill",
                "tomlplusplus",
            ]
            .iter()
            .enumerate()
            {
                let label = match *p {
                    "toml" => "Hover · crates.io".to_owned(),
                    "present" => "A workspace package".to_owned(),
                    _ => format!("Hover · {}", fixture::package(p).name),
                };
                cards.push(cell(label, 420.0, open(i, p, window, cx), measure, cx));
            }
            let mut rows_out = vec![row(vec![
                cell("At rest", 640.0, rest, measure, cx),
                cell(
                    "In a row, the stone alone (quiet: no card)",
                    420.0,
                    rows,
                    measure,
                    cx,
                ),
            ])];
            let mut it = cards.into_iter();
            loop {
                let chunk: Vec<AnyElement> = it.by_ref().take(3).collect();
                if chunk.is_empty() {
                    break;
                }
                rows_out.push(row(chunk));
            }
            board("Identity marks", "Ecosystem", rows_out, measure, cx)
        },
        cx,
    )
}

fn license_board(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, window, cx| {
            let palette = cx.facet().palette();
            let m = measure.within(px(420.0));
            let who = |w: &str| {
                div()
                    .set(ty::MONO_SMALL, &m)
                    .text_color(palette.ink4.hsla())
                    .child(w.to_owned())
            };
            let fam = |i: usize, facts: license::LicenseFacts, w: &str| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(license_mark(("lic-fam", i), facts, &m))
                    .child(who(w))
                    .into_any_element()
            };
            let families = line(vec![
                fam(0, fixture::named_license("tokio"), "tokio"),
                fam(1, fixture::named_license("option-ext"), "option-ext"),
                fam(
                    2,
                    fixture::license(Some("GPL-2.0-only"), None),
                    "self_cell's other choice",
                ),
                fam(3, fixture::named_license("notify"), "notify"),
                fam(4, fixture::named_license("cfg_block"), "cfg_block"),
                fam(5, fixture::license(None, None), "no license field, no file"),
            ]);
            let compound = line(vec![
                fam(
                    10,
                    fixture::license(fixture::package("toml").license.as_deref(), Some("toml")),
                    "toml",
                ),
                fam(11, fixture::named_license("memchr"), "memchr"),
                fam(12, fixture::named_license("self_cell"), "self_cell"),
                fam(13, fixture::named_license("unicode-ident"), "unicode-ident"),
                fam(14, fixture::named_license("r-efi"), "r-efi"),
            ]);
            let open =
                |i: usize, facts: license::LicenseFacts, window: &mut Window, cx: &mut App| {
                    let content = license::board_card(&facts);
                    div()
                        .flex()
                        .flex_col()
                        .child(license_mark(("lic-open", i), facts, &m))
                        .child(card::in_place(&content, window, cx))
                        .into_any_element()
                };
            let mut own = fixture::license(
                fixture::package("present").license.as_deref(),
                Some("present"),
            );
            own.own = true;
            let cards = vec![
                (
                    "Hover · toml MIT OR Apache-2.0",
                    fixture::license(fixture::package("toml").license.as_deref(), Some("toml")),
                ),
                (
                    "Hover · self_cell Apache-2.0 OR GPL-2.0-only",
                    fixture::named_license("self_cell"),
                ),
                (
                    "Hover · option-ext MPL-2.0",
                    fixture::named_license("option-ext"),
                ),
                (
                    "Hover · cfg_block license-file",
                    fixture::named_license("cfg_block"),
                ),
                (
                    "Hover · unicode-ident AND",
                    fixture::named_license("unicode-ident"),
                ),
                (
                    "Hover · present, your own project: the mark knows the tree",
                    own,
                ),
            ];
            let mut rows_out = vec![row(vec![
                cell("The shape is the family", 640.0, families, measure, cx),
                cell(
                    "OR is a choice of rings; AND links them",
                    640.0,
                    compound,
                    measure,
                    cx,
                ),
            ])];
            let mut built: Vec<AnyElement> = Vec::new();
            for (i, (label, facts)) in cards.into_iter().enumerate() {
                built.push(cell(label, 420.0, open(i, facts, window, cx), measure, cx));
            }
            let mut it = built.into_iter();
            loop {
                let chunk: Vec<AnyElement> = it.by_ref().take(3).collect();
                if chunk.is_empty() {
                    break;
                }
                rows_out.push(row(chunk));
            }
            board("Identity marks", "License", rows_out, measure, cx)
        },
        cx,
    )
}

fn version_board(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, window, cx| {
            let wide = measure.within(px(640.0));
            let narrow = measure.within(px(220.0));
            let toml = fixture::version("toml");
            let variants = row(vec![
                cell(
                    "Rider: the number is the pin's own tooth; later releases trail off, breaking ones stand taller",
                    640.0,
                    version_mark("v-rider", toml.clone(), &wide),
                    measure,
                    cx,
                ),
                cell(
                    "Baseline (rows): the number sits at the comb's start, the pin is a mint tick inside it",
                    640.0,
                    version_mark("v-base", toml.clone(), &wide).row(),
                    measure,
                    cx,
                ),
            ]);
            let band = row(vec![
                cell(
                    "Band (below 240 px): one hairline; breaking releases, the pin and the also teeth stand out",
                    640.0,
                    version_mark("v-band", toml.clone(), &narrow),
                    measure,
                    cx,
                ),
                cell(
                    "Scrubbed to 1.0.0: the number rode there; the pin waits home",
                    640.0,
                    version_mark("v-scrub", toml.clone(), &wide).scrub_at(1, "1.0.0+spec-1.1.0"),
                    measure,
                    cx,
                ),
            ]);
            let open = |label: &str,
                        facts: &version::VersionFacts,
                        content: card::Content,
                        window: &mut Window,
                        cx: &mut App| {
                cell(
                    label.to_owned(),
                    640.0,
                    div()
                        .flex()
                        .flex_col()
                        .child(version_mark(
                            SharedString::from(format!("v-open-{label}")),
                            facts.clone(),
                            &wide,
                        ))
                        .child(card::in_place(&content, window, cx)),
                    measure,
                    cx,
                )
            };
            let smallvec = fixture::version("smallvec");
            let syn = fixture::version("syn");
            let serde = fixture::version("serde");
            let react = fixture::version("@types/react");
            let present = fixture::version("present");
            let also =
                version::board_also(&toml, "1.1.5").unwrap_or_else(|| version::board_number(&toml));
            let syn_also =
                version::board_also(&syn, "2.0.119").unwrap_or_else(|| version::board_number(&syn));
            let rows_out = vec![
                variants,
                band,
                row(vec![
                    open(
                        "The number's card: the semver reading",
                        &toml,
                        version::board_number(&toml),
                        window,
                        cx,
                    ),
                    open(
                        "The second pin: two copies of toml compile",
                        &toml,
                        also,
                        window,
                        cx,
                    ),
                ]),
                row(vec![
                    open(
                        "smallvec: behind, and honestly no API changes",
                        &smallvec,
                        version::board_number(&smallvec),
                        window,
                        cx,
                    ),
                    open(
                        "syn: two majors in your tree, neither yours",
                        &syn,
                        syn_also,
                        window,
                        cx,
                    ),
                ]),
                row(vec![
                    open(
                        "serde: up to date",
                        &serde,
                        version::board_number(&serde),
                        window,
                        cx,
                    ),
                    open(
                        "@types/react: not in your tree",
                        &react,
                        version::board_number(&react),
                        window,
                        cx,
                    ),
                ]),
                row(vec![cell(
                    "present: never published",
                    640.0,
                    version_mark("v-local", present, &wide).number_look(),
                    measure,
                    cx,
                )]),
            ];
            board("Identity marks", "Version", rows_out, measure, cx)
        },
        cx,
    )
}

fn deps_board(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, window, cx| {
            let m = measure.within(px(640.0));
            let small = measure.within(px(420.0));
            let toml = fixture::deps("toml");
            let rest = dep_line("deps-rest", toml.clone(), "toml", &m);
            let tight = dep_line(
                "deps-tight",
                toml.clone(),
                "toml",
                &measure.within(px(380.0)),
            );
            let open = |i: usize,
                        label: &str,
                        pkg: &str,
                        dep: &str,
                        kind: DepKind,
                        window: &mut Window,
                        cx: &mut App| {
                let facts = fixture::dep(pkg, dep, kind);
                let content = deps::board_card(&facts, pkg);
                cell(
                    label.to_owned(),
                    420.0,
                    div()
                        .flex()
                        .flex_col()
                        .child(dep_link(("dep-open", i), facts, pkg.to_owned(), &small))
                        .child(card::in_place(&content, window, cx)),
                    measure,
                    cx,
                )
            };
            let rows_out = vec![
                row(vec![
                    cell(
                        "toml, at rest: as many as fit, then the fold",
                        640.0,
                        rest,
                        measure,
                        cx,
                    ),
                    cell(
                        "toml in 380 px: fewer fit, the fold counts the rest",
                        640.0,
                        tight,
                        measure,
                        cx,
                    ),
                ]),
                row(vec![
                    open(
                        0,
                        "toml → toml_edit",
                        "toml",
                        "toml_edit",
                        DepKind::Normal,
                        window,
                        cx,
                    ),
                    open(
                        1,
                        "toml → indexmap, optional and off in your tree",
                        "toml",
                        "indexmap",
                        DepKind::Normal,
                        window,
                        cx,
                    ),
                    open(
                        2,
                        "toml → serde",
                        "toml",
                        "serde",
                        DepKind::Normal,
                        window,
                        cx,
                    ),
                ]),
                row(vec![
                    open(
                        3,
                        "serde_json → itoa (no usage phrase: the items used)",
                        "serde_json",
                        "itoa",
                        DepKind::Normal,
                        window,
                        cx,
                    ),
                    open(
                        4,
                        "serde_json → serde_core",
                        "serde_json",
                        "serde_core",
                        DepKind::Normal,
                        window,
                        cx,
                    ),
                    open(
                        5,
                        "serde_json → memchr",
                        "serde_json",
                        "memchr",
                        DepKind::Normal,
                        window,
                        cx,
                    ),
                ]),
                row(vec![
                    open(
                        6,
                        "present → backend-library, yours",
                        "present",
                        "backend-library",
                        DepKind::Normal,
                        window,
                        cx,
                    ),
                    open(
                        7,
                        "@types/react → csstype",
                        "@types/react",
                        "csstype",
                        DepKind::Normal,
                        window,
                        cx,
                    ),
                    open(
                        8,
                        "toml → snapbox, a dev-dependency: uses unknown (⌥)",
                        "toml",
                        "snapbox",
                        DepKind::Dev,
                        window,
                        cx,
                    ),
                ]),
            ];
            board("Identity marks", "Depends on", rows_out, measure, cx)
        },
        cx,
    )
}

// ------------------------------------------------------------------ heroes

/// Which card a hero shows open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Rest,
    Eco,
    Lic,
    Ver,
    Also,
    Dep(&'static str),
    CardFilm,
    CopyFilm,
    ScrubFilm,
}

const LEDE: TypeRole = TypeRole {
    face: Face::Serif,
    weight: 400.0,
    size: 17.0,
    line: 24.0,
    tracking: 0.0,
    italic: true,
};
const TAB: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 500.0,
    size: 13.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

fn first_sentence(text: &str) -> String {
    match text.find(". ") {
        Some(end) => text[..=end].trim().to_owned(),
        None => text.trim().to_owned(),
    }
}

fn hero(
    pkg: &'static str,
    look: Look,
    measure: &Measure,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let palette = cx.facet().palette();
    let s = measure.scale();
    let room = measure.fluid_room();
    let modes = Modes::keyed(
        ElementId::Name(format!("marks-hero-{pkg}").into()),
        window,
        cx,
    );
    // The window around the reader, as the prototype draws it: a titlebar
    // strip and the shelf (a spine below 900 px, gone below 640 px): the
    // product's own modes, held through their hysteresis bands.
    let shelf = match modes.settle(&DOCK, room).mode {
        Dock::Shelf => f32::from(MOCK_SHELF.at(room)),
        Dock::Spine => f32::from(geo::KSPINE) * s,
        Dock::Drawer => 0.0,
    };
    let reader_w = f32::from(measure.width()) - shelf;
    let cqi = reader_w / 100.0;
    let pad_x = (4.0 * cqi).clamp(16.0 * s, 48.0 * s);
    let pad_top = (5.0 * cqi).clamp(22.0 * s, 64.0 * s);
    let column_w = (reader_w - pad_x * 2.0).min(880.0 * s - pad_x * 2.0);
    let column = measure.within(px(column_w));
    let p = fixture::package(pkg);
    let local = p.local;
    let name = p.name.clone();
    let lede = p.description.as_deref().map(|d| {
        if local {
            d.to_owned()
        } else {
            first_sentence(d)
        }
    });
    let mut eco = ecosystem_mark("hero-eco", fixture::eco(pkg), &column);
    let mut lic_facts = fixture::license(p.license.as_deref(), Some(&p.name));
    lic_facts.own = local;
    let mut lic = license_mark("hero-lic", lic_facts, &column);
    let mut ver = version_mark("hero-ver", fixture::version(pkg), &column);
    match look {
        Look::Eco => eco = eco.open_look(),
        Look::CopyFilm => eco = eco.press_look(900),
        Look::Lic => lic = lic.open_look(),
        Look::Ver => ver = ver.number_look(),
        Look::Also => ver = ver.also_look("1.1.5"),
        Look::ScrubFilm => {
            ver = ver
                .scrub_at(100, "1.1.6+spec-1.1.0")
                .scrub_at(1500, p.pin.clone().unwrap_or_default())
        }
        _ => {}
    }
    let deps = fixture::deps(pkg);
    let wrap = modes.settle(&HERO_DEPS, column.fluid_room()).mode == Split::Stacked;
    // Beside the eco and license marks, the deps get what the line has left.
    let marks_w = 16.0 * s * 2.0 + 180.0 * s + 22.0 * s * 2.0;
    let deps_measure = if wrap {
        column
    } else {
        column.within(px((column_w - marks_w).max(120.0 * s)))
    };
    let mut dep_el =
        (!deps.is_empty()).then(|| dep_line("hero-deps", deps, name.clone(), &deps_measure));
    if let Some(line) = dep_el.take() {
        dep_el = Some(match look {
            Look::Dep(dep) => line.card_look(dep),
            // Rested at 100, open at 450 (the peek's rest delay): the
            // board's t=0. The sweep comes 700 ms later, the leave at 1300.
            Look::CardFilm => line.card_at("toml_edit", 100).card_at("serde", 1150),
            _ => line,
        });
    }
    let gem_px = f32::from(MARK_GEM.at(room));
    let head = div()
        .flex()
        .items_center()
        .gap(px((2.0 * cqi).clamp(14.0 * s, 22.0 * s)))
        .child(gem(Kind::Package).size(gem_px))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .set(ty::HERO, &column)
                        .text_color(palette.ink0.hsla())
                        .child(name.clone()),
                )
                .children(lede.map(|l| {
                    div()
                        .mt(px(6.0 * s))
                        .set(LEDE, &column)
                        .text_color(palette.ink2.hsla())
                        .child(l)
                })),
        );
    let mut marks = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(22.0 * s))
        .gap_y(px(10.0 * s))
        .child(eco)
        .child(lic);
    let mut below = None;
    if let Some(dep_el) = dep_el {
        if wrap {
            below = Some(dep_el);
        } else {
            marks = marks.child(dep_el);
        }
    }
    let tabs = div()
        .flex()
        .gap(px((2.4 * cqi).clamp(14.0 * s, 28.0 * s)))
        .border_b_1()
        .border_color(palette.line1.hsla())
        .children(
            ["Map", "Readme", "Depends", "Used by", "Changes"]
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let mut tab = div()
                        .pb(px(10.0 * s))
                        .set(TAB, &column)
                        .text_color(if i == 0 {
                            palette.ink0.hsla()
                        } else {
                            palette.ink3.hsla()
                        })
                        .child(*t);
                    if i == 0 {
                        tab = tab.border_b_2().border_color(palette.mint.base.hsla());
                    }
                    tab
                }),
        );
    let body = div()
        .flex()
        .flex_col()
        .w(px(column_w))
        .child(head)
        .child(div().mt(px(18.0 * s)).child(marks))
        .child(div().mt(px(20.0 * s)).child(ver))
        .children(below.map(|b| div().mt(px(14.0 * s)).child(b)))
        .child(div().mt(px(26.0 * s)).child(tabs));
    let reader = div()
        .absolute()
        .left(px(shelf))
        .top(px(50.0 * s))
        .right_0()
        .bottom_0()
        .flex()
        .justify_center()
        .pt(px(pad_top))
        .child(body);
    let chrome = div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(50.0 * s))
        .border_b_1()
        .border_color(palette.line1.hsla());
    let mut root = div().absolute().inset_0().child(chrome).child(reader);
    if shelf > 0.0 {
        root = root.child(
            div()
                .absolute()
                .left_0()
                .top(px(50.0 * s))
                .bottom_0()
                .w(px(shelf))
                .border_r_1()
                .border_color(palette.line1.hsla()),
        );
    }
    if look == Look::CardFilm {
        schedule_close(1750, window, cx);
    }
    root.into_any_element()
}

/// Closes every card `after` ms into the scene (once per scene).
fn schedule_close(after: u64, window: &mut Window, cx: &mut App) {
    #[derive(Default)]
    struct Scheduled(bool);
    impl gpui::Global for Scheduled {}
    if std::mem::replace(&mut cx.default_global::<Scheduled>().0, true) {
        return;
    }
    window
        .spawn(cx, async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(after))
                .await;
            let _ = cx.update(|window, cx| {
                float::close_all(window, cx);
            });
        })
        .detach();
}

macro_rules! hero_scene {
    ($name:ident, $pkg:literal, $look:expr) => {
        fn $name(_: &mut Window, cx: &mut App) -> AnyView {
            stage(
                |measure, window, cx| hero($pkg, $look, measure, window, cx),
                cx,
            )
        }
    };
}

hero_scene!(hero_toml, "toml", Look::Rest);
hero_scene!(hero_serde, "serde", Look::Rest);
hero_scene!(hero_present, "present", Look::Rest);
hero_scene!(hero_react, "@types/react", Look::Rest);
hero_scene!(hero_smallvec, "smallvec", Look::Rest);
hero_scene!(hero_toml_eco, "toml", Look::Eco);
hero_scene!(hero_toml_lic, "toml", Look::Lic);
hero_scene!(hero_toml_ver, "toml", Look::Ver);
hero_scene!(hero_toml_also, "toml", Look::Also);
hero_scene!(hero_toml_dep, "toml", Look::Dep("toml_edit"));
hero_scene!(card_film, "toml", Look::CardFilm);
hero_scene!(copy_film, "toml", Look::CopyFilm);
hero_scene!(scrub_film, "toml", Look::ScrubFilm);

/// The lens under the Rider: serde's comb is too dense to hit one release
/// by pointer, so the lens opens where the pointer enters and follows it.
fn lens_film(window: &mut Window, cx: &mut App) -> AnyView {
    crate::gallery::declare_script(
        "move 620,248 @100; move 700,248 @250; move 780,248 @400; move 860,248 @550; move 940,248 @700; move 1000,248 @850; leave @1300",
        cx,
    );
    hero_serde(window, cx)
}

// ------------------------------------------------------------------ ligatures

/// The install line whose `--` Geist Mono's contextual alternates join.
pub(crate) const CALT_LINE: &str = "dotnet add package Polyfill --version 9.9.0";

/// A control line: sequences Geist Mono joins when its ligatures are on.
pub(crate) const CONTROL_LINE: &str = "a -> b => c != d >= e <= f === g";

fn calt(_: &mut Window, cx: &mut App) -> AnyView {
    stage(
        |measure, _window, cx| {
            let palette = cx.facet().palette();
            let row = |label: &'static str, features: Option<FontFeatures>, line: &'static str| {
                // On a flat plate, so rows compare pixel for pixel.
                let mut text = div()
                    .h(px(40.0))
                    .w(px(680.0))
                    .flex()
                    .items_center()
                    .bg(palette.table.hsla())
                    .set(card::LINE, measure)
                    .text_color(palette.ink0.hsla());
                if let Some(features) = features {
                    text.text_style().font_features = Some(features);
                }
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .set(LABEL, measure)
                            .text_color(palette.ink3.hsla())
                            .child(label),
                    )
                    .child(text.child(line))
            };
            let off = FontFeatures(Arc::new(vec![
                ("calt".to_owned(), 0),
                ("liga".to_owned(), 0),
                ("zero".to_owned(), 1),
            ]));
            let on = FontFeatures(Arc::new(vec![
                ("calt".to_owned(), 1),
                ("liga".to_owned(), 0),
                ("zero".to_owned(), 1),
            ]));
            let all_on = FontFeatures(Arc::new(vec![
                ("calt".to_owned(), 1),
                ("liga".to_owned(), 1),
                ("dlig".to_owned(), 1),
            ]));
            let all_off = FontFeatures(Arc::new(vec![
                ("calt".to_owned(), 0),
                ("liga".to_owned(), 0),
                ("dlig".to_owned(), 0),
            ]));
            let calt_only = FontFeatures(Arc::new(vec![
                ("calt".to_owned(), 1),
                ("liga".to_owned(), 0),
                ("dlig".to_owned(), 0),
            ]));
            let liga_only = FontFeatures(Arc::new(vec![
                ("calt".to_owned(), 0),
                ("liga".to_owned(), 1),
                ("dlig".to_owned(), 0),
            ]));
            div()
                .absolute()
                .left(px(40.0))
                .top(px(30.0))
                .flex()
                .flex_col()
                .gap(px(20.0))
                .child(row("mono role (fonts::features)", None, CALT_LINE))
                .child(row("calt on", Some(on), CALT_LINE))
                .child(row("calt off", Some(off), CALT_LINE))
                .child(row(
                    "control, every feature on: arrows and !=",
                    Some(all_on.clone()),
                    CONTROL_LINE,
                ))
                .child(row(
                    "control, every feature off",
                    Some(all_off),
                    CONTROL_LINE,
                ))
                .child(row(
                    "control, mono role (fonts::features)",
                    None,
                    CONTROL_LINE,
                ))
                .child(row("control, calt on only", Some(calt_only), CONTROL_LINE))
                .child(row("control, liga on only", Some(liga_only), CONTROL_LINE))
                .child(row(
                    "install line, every feature on",
                    Some(all_on.clone()),
                    CALT_LINE,
                ))
                .into_any_element()
        },
        cx,
    )
}
