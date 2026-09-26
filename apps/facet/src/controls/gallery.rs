//! Controls scenes.
//!
//! - `controls`: the Controls board, section by section (plus the calm
//!   Settings rows), at the board's width;
//! - `controls-states`: every control × every state in one grid;
//! - `controls-live`: live controls at fixed positions with a default input
//!   script — hover (sweep), press, keyboard focus, a switch, a check, a seg
//!   glide, a version-comb scrub, a splitter drag past both limits and a
//!   release, ⌘ held — for filmstrips and motion reports;
//! - `version-comb`: the version comb target's five states in shelf books
//!   (17 and ~400 releases) plus a three-release package, scrubbed by pointer
//!   and keys;
//! - `controls-storm`: a seeded storm over the live controls, then a settle,
//!   then a remount of the same state under fresh ids (settle == fresh).
//!
//! Glacier, density and contrast come from the capture flags (`--theme
//! glacier`, `--density dense`, `--contrast high`).

#![allow(clippy::too_many_lines)]

use super::glyph::Glyph;
use super::seg::Swatch;
use super::state::Look;
use super::sweep::sweep;
use super::toggle::Tri;
use super::{
    Intent, KbdVoice, PanelSide, Release, ReleaseId, ReleaseStep, SplitEvent, SplitModel, Step,
    button, check, density_toggle, field, icon_button, kbd, keys, radio, seg, select, split_width,
    splitter, step_release, switch, theme_toggle, version_comb,
};
use crate::Set;
use crate::gallery::{Scene, declare_script};
use crate::icons::{Icon, Lang};
use crate::measure::{Control, Density, Measure, Space};
use crate::paint::gallery::{canvas, doc, role, section, text};
use crate::paint::{Bevel, Chamfer, Plate, cut, ground};
use crate::probe;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, TypeRole, ty};
use gpui::{
    AnyElement, AnyView, App, AppContext, Context, Div, ElementId, Entity, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Render, ScrollHandle,
    SharedString, Size, StatefulInteractiveElement, Styled, WeakEntity, Window, div, px,
};
use gpui_component::input::InputState;
use std::rc::Rc;

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "controls",
        title: "Controls board: buttons and icon buttons (every state, the sweep, every intent), \
                segmented stones, diamond switch/check/radio, the version comb, fields and the \
                select trigger, key caps, the calm Settings rows",
        size: (1440, 1720),
        build: |window, cx| Board::view(Mode::Board, window, cx),
    },
    Scene {
        id: "controls-states",
        title: "Every control in every state: rest, hover, press, keyboard focus, on/selected, \
                busy, fault, disabled",
        size: (1440, 900),
        build: |window, cx| Board::view(Mode::States, window, cx),
    },
    Scene {
        id: "controls-live",
        title: "Live controls driven by the default script: sweep, press, focus arrival, switch, \
                check, seg glide, version-comb scrub, splitter drag + release, ⌘ caps rising",
        size: (1200, 640),
        build: |window, cx| {
            declare_script(LIVE_SCRIPT, cx);
            Board::view(Mode::Live, window, cx)
        },
    },
    Scene {
        id: "version-comb",
        title: "The version comb (VersionComb.png): at rest, hovering, scrubbed, 400 releases at \
                rest and under the lens, 3 releases; the default script scrubs by pointer and keys",
        size: (1000, 900),
        build: |window, cx| {
            declare_script(COMB_SCRIPT, cx);
            Board::view(Mode::Comb, window, cx)
        },
    },
    Scene {
        id: "controls-storm",
        title: "The live controls under a storm (`facet-gallery storm --scene controls-storm`): \
                every live control tells the probe what it shows; the storm aims at them, \
                holds ⌘, steps the comb, opens the select, and checks settle == fresh",
        size: (1200, 640),
        build: |window, cx| {
            crate::gallery::storm::declare_keys(&["home", "end", "pageup", "pagedown", "[", "]"], cx);
            Board::view(Mode::Storm, window, cx)
        },
    },
];

/// The live scene's script: every motion in turn, at fixed coordinates (see
/// [`live`] for the layout; `probe` bounds confirm them).
const LIVE_SCRIPT: &str = "
move 20,20 @0
# the sweep: one crossing on hover-enter, the lift
move 120,95 @200
# press and release
down left 120,95 @800
up left 120,95 @960
# leave, then keyboard focus arrives on the first control
move 20,20 @1200
key tab @1400
# a switch, a check, a radio
click left 418,95 @1800
click left 580,95 @2300
# the seg glides two stops, then back one
click left 1020,95 @2800
click left 836,95 @3300
# the version comb: press on a tick, scrub back through history, release
down left 286,222 @3700
move 266,222 @3760
move 236,222 @3820
move 206,222 @3880
move 186,222 @3940
up left 186,222 @4000
key right @4200
key escape @4400
# the splitter: grab, past the maximum, back, below the minimum, over the collapse line, out, release
move 865,420 @4600
down left 865,420 @4700
move 1000,420 @4780
move 1100,420 @4860
move 940,420 @4960
move 760,420 @5060
move 700,420 @5160
move 820,420 @5260
up left 820,420 @5360
# hold cmd: every control's key cap rises, left to right
move 20,20 @5800
hold cmd @6000
release cmd @6700
";

/// The version-comb scene's script (see [`comb_scene`] for the layout).
const COMB_SCRIPT: &str = "
move 10,10 @0
# present, at rest: hover along the comb, rest on 0.3.0 (its tip opens below)
move 120,200 @200
move 195,200 @500
# press on 0.3.0 and drag back through history, release on 0.2.1
down left 195,200 @1400
move 170,200 @1480
move 150,200 @1560
move 136,200 @1640
up left 136,200 @1720
# keys: one release, one minor, the newest, then Esc back to the pin
key right @2000
key pageup @2300
key end @2600
key escape @2900
# tokio, 400 releases: the lens opens under the pointer, slides at its edge
move 70,460 @3300
move 100,460 @3500
move 130,460 @3700
move 160,460 @3900
move 240,460 @4100
down left 240,460 @4300
move 260,460 @4400
up left 260,460 @4500
# three releases: hover one, press another
move 804,460 @4900
click left 686,460 @5300
key escape @5700
move 10,10 @6100
# the page, not a comb: `]` twice and `[` once step toml; its comb echoes
click left 960,880 @6300
key ] @6500
key ] @7300
key [ @8100
move 10,10 @9800
";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Board,
    States,
    Live,
    Comb,
    /// The live controls in a flow that wraps and scrolls at any window
    /// size, keyboard focus kept on screen (the storm's scene).
    Storm,
}

/// Every scene's state: what the controls show and change.
struct Board {
    mode: Mode,
    inputs: Vec<Entity<InputState>>,
    switch_on: bool,
    check: Tri,
    radio: usize,
    lens: usize,
    density: usize,
    theme: usize,
    histories: [Rc<[Release]>; 4],
    pins: [usize; 4],
    viewing: [usize; 4],
    /// The toml history's releases that touch your code.
    toml_touches: Vec<usize>,
    split: SplitModel,
    /// The live selects' language (an index into [`LANGS`]).
    language: usize,
    /// The page itself: `[` / `]` step the page's package (toml) from
    /// anywhere a control does not take them.
    page: FocusHandle,
    /// The storm flow's cells (each holds one control), its scroll, and the
    /// (cell, window size) it last scrolled keyboard focus into view for.
    cells: Vec<FocusHandle>,
    scroll: ScrollHandle,
    followed: Option<(usize, Size<Pixels>)>,
}

impl Board {
    fn view(mode: Mode, window: &mut Window, cx: &mut App) -> AnyView {
        let inputs = [
            ("Filter, or find any package", ""),
            ("", "axum"),
            ("Registry", "ssh://not-a-registry"),
            ("Filter", ""),
        ]
        .into_iter()
        .map(|(placeholder, value)| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        })
        .collect();
        // The target's tokio pins 1.38.1.
        let tokio = tokio();
        let toml = toml();
        let tokio_pin = tokio
            .iter()
            .position(|release| release.version.as_ref() == "1.38.1")
            .unwrap_or(0);
        let board = cx.new(|cx| Self {
            mode,
            inputs,
            switch_on: false,
            check: Tri::Off,
            radio: 0,
            lens: 0,
            density: 0,
            theme: 1,
            histories: [present(), young(), tokio.clone(), toml.0.clone()],
            pins: [16, 1, tokio_pin, toml.1],
            viewing: [16, 1, tokio_pin, toml.1],
            toml_touches: toml.2.clone(),
            split: SplitModel::shelf(264.0, false),
            language: 0,
            page: cx.focus_handle(),
            cells: (0..STORM_CELLS).map(|_| cx.focus_handle()).collect(),
            scroll: ScrollHandle::new(),
            followed: None,
        });
        if mode == Mode::Comb {
            let page = board.read(cx).page.clone();
            window.focus(&page, cx);
        }
        board.into()
    }
}

impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let content = match self.mode {
            Mode::Board => controls_board(self, &me, window, cx),
            Mode::States => states(self, window, cx),
            Mode::Live => live(self, &me, window, cx),
            Mode::Comb => comb_scene(self, &me, cx),
            Mode::Storm => storm_flow(self, &me, window, cx),
        };
        let page = cx.entity().downgrade();
        // The float layer (tips, menus) is the root's last child.
        div()
            .size_full()
            .track_focus(&self.page)
            // `[` / `]` from anywhere: the page's package steps through its
            // releases by the comb's own steps; its comb echoes the move.
            .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                let step = match event.keystroke.key.as_str() {
                    "[" => ReleaseStep::Older,
                    "]" => ReleaseStep::Newer,
                    _ => return,
                };
                let _ = page.update(cx, |board, cx| {
                    if board.mode != Mode::Comb {
                        return;
                    }
                    if let Some(next) = step_release(&board.histories[3], board.viewing[3], step) {
                        board.viewing[3] = next;
                        cx.notify();
                    }
                });
                cx.stop_propagation();
            })
            .child(content)
            .child(crate::overlay::float::layer(window, cx))
    }
}

/// A handler that updates the board and repaints.
fn with(
    me: &WeakEntity<Board>,
    f: impl Fn(&mut Board) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let me = me.clone();
    move |_window, cx| {
        let _ = me.update(cx, |board, cx| {
            f(board);
            cx.notify();
        });
    }
}

/// The ecosystems a live select chooses among.
const LANGS: [(Lang, &str); 7] = [
    (Lang::Rust, "Rust"),
    (Lang::Typescript, "TypeScript"),
    (Lang::Python, "Python"),
    (Lang::Go, "Go"),
    (Lang::Java, "Java"),
    (Lang::Csharp, "C#"),
    (Lang::Cpp, "C++"),
];

/// A live select over [`LANGS`]: it opens its own menu.
fn language_select(id: &'static str, board: &Board, me: &WeakEntity<Board>, measure: &Measure) -> super::Select {
    let (lang, label) = LANGS[board.language.min(LANGS.len() - 1)];
    select(id, label, measure)
        .lang(lang)
        .options(LANGS.map(|(_, label)| label), with_index(me, |board, index| board.language = index))
}

/// A handler taking an index.
fn with_index(
    me: &WeakEntity<Board>,
    f: impl Fn(&mut Board, usize) + 'static,
) -> impl Fn(usize, &mut Window, &mut App) + 'static {
    let me = me.clone();
    move |index, _window, cx| {
        let _ = me.update(cx, |board, cx| {
            f(board, index);
            cx.notify();
        });
    }
}

/// Releases from their versions (oldest first), one every two weeks.
fn releases_of(versions: &[String]) -> Rc<[Release]> {
    let n = versions.len();
    versions
        .iter()
        .enumerate()
        .map(|(i, version)| {
            let weeks = (n - 1 - i) * 2;
            let age = match weeks {
                0 => "this week".to_owned(),
                2..=8 => format!("{weeks} weeks ago"),
                9..=103 => format!("{} months ago", weeks * 7 / 30),
                _ => format!("{} years ago", weeks / 52),
            };
            Release {
                id: ReleaseId(version.clone().into()),
                version: version.clone().into(),
                step: Step::of(version),
                age: age.into(),
            }
        })
        .collect()
}

/// `present`'s seventeen releases (the target's).
fn present() -> Rc<[Release]> {
    let versions = [
        "0.1.0", "0.1.1", "0.1.2", "0.1.3", "0.2.0", "0.2.1", "0.2.2", "0.2.3", "0.2.4", "0.3.0",
        "0.3.1", "0.3.2", "0.3.3", "0.3.4", "0.4.0", "0.4.1", "0.4.2",
    ];
    releases_of(&versions.map(str::to_owned))
}

/// `toml` from W-Data's release fixture (120 releases, your pin 0.8.23):
/// the releases, the pin, and the releases whose move from the pin touches
/// your uses (`Crate::impact(pinned, v)` not empty).
fn toml() -> (Rc<[Release]>, usize, Vec<usize>) {
    let Some(krate) = crate::data::release::fixture::get("toml") else {
        return (Rc::from(Vec::new()), 0, Vec::new());
    };
    let date = |at: &str| {
        let mut parts = at.get(..10).unwrap_or("0-0-0").split('-').map(|p| p.parse::<i64>().unwrap_or(0));
        let (y, m, d) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
        y * 372 + m * 31 + d
    };
    let newest = krate.versions.last().map_or(0, |v| date(&v.at));
    let releases: Rc<[Release]> = krate
        .versions
        .iter()
        .map(|v| {
            let days = newest - date(&v.at);
            let age = match days {
                0..=6 => "this week".to_owned(),
                7..=59 => format!("{} weeks ago", days / 7),
                60..=729 => format!("{} months ago", days / 31),
                _ => format!("{} years ago", days / 372),
            };
            Release {
                id: ReleaseId(v.v.clone()),
                version: v.v.clone(),
                step: Step::of(&v.v),
                age: age.into(),
            }
        })
        .collect();
    let pin = krate.versions.iter().position(|v| v.v == krate.pinned).unwrap_or(0);
    let touches = krate
        .versions
        .iter()
        .enumerate()
        .filter(|(_, v)| !krate.impact(&krate.pinned, &v.v).is_empty())
        .map(|(i, _)| i)
        .collect();
    (releases, pin, touches)
}

/// A young package: three releases.
fn young() -> Rc<[Release]> {
    releases_of(&["1.0.0", "1.0.1", "1.1.0"].map(str::to_owned))
}

/// A long history (the target's `tokio`): 1.0.0 to 1.40.x, about 400.
fn tokio() -> Rc<[Release]> {
    let mut versions = Vec::new();
    for minor in 0..41_usize {
        for patch in 0..[9, 12, 6, 14, 8, 11][minor % 6] {
            versions.push(format!("1.{minor}.{patch}"));
        }
    }
    releases_of(&versions)
}

/// A handler for comb `slot` that moves the board's viewed release.
fn on_version(
    me: &WeakEntity<Board>,
    slot: usize,
) -> impl Fn(&super::VersionSelected, &mut Window, &mut App) + 'static {
    let me = me.clone();
    move |event, _window, cx| {
        let _ = me.update(cx, |board, cx| {
            if let Some(index) = board.histories[slot]
                .iter()
                .position(|release| release.id == event.0)
            {
                board.viewing[slot] = index;
                cx.notify();
            }
        });
    }
}

const CELL: TypeRole = role(Face::Ui, 500.0, 12.0, 16.0, 0.0);
const TITLE: TypeRole = role(Face::Display, 700.0, 30.0, 34.0, -0.02);
const CLABEL: TypeRole = role(Face::Mono, 400.0, 10.5, 12.0, 0.0);
const CCAP: TypeRole = role(Face::Serif, 400.0, 12.5, 18.0, 0.0);
const SLAB: TypeRole = role(Face::Ui, 500.0, 14.0, 18.0, 0.0);

/// A control over its mono state label (`.bstep`).
fn step(control: impl IntoElement, label: &'static str, cx: &App) -> Div {
    let palette = cx.palette();
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap(px(9.0))
        .child(control)
        .child(text(CLABEL, cx.facet(), palette.ink3, label))
}

fn caption(body: &'static str, cx: &App) -> Div {
    text(CCAP, cx.facet(), cx.palette().ink3, body)
}

/// The facet sweep frozen at three phases of its crossing, on default plates.
fn sweep_strip(measure: &Measure, cx: &App) -> Div {
    let palette = cx.palette();
    let height = measure.control(Control::Medium);
    let chamfer = f32::from(height) * 8.0 / 30.0;
    let light = super::with_alpha(palette.bevel_hi.into(), 0.4);
    let plate = |t: f32| {
        cut()
            .chamfer(Chamfer::Px(chamfer))
            .bevel(Bevel::Rest)
            .plate(Plate::Flat)
            .fill(palette.plate2)
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .w(px(120.0) * measure.scale())
            .h(height)
            .child(
                div()
                    .set(ty::BUTTON, measure)
                    .text_color(palette.ink0.hsla())
                    .child("Compare"),
            )
            .child(div().absolute().inset_0().child(sweep(t, chamfer, light).size_full()))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(9.0))
        .items_start()
        .child(caption("The facet sweep: three phases of one crossing.", cx))
        .child(
            div()
                .flex()
                .gap(px(3.0))
                .child(plate(0.18))
                .child(plate(0.42))
                .child(plate(0.66)),
        )
}

fn buttons_section(board: &Board, me: &WeakEntity<Board>, measure: &Measure, cx: &App) -> Div {
    let m = measure;
    let add = |id: &'static str| button(id, "Add package", m).glyph(Glyph::Plus);
    let states = div()
        .flex()
        .flex_wrap()
        .gap_x(px(28.0))
        .gap_y(px(18.0))
        .items_start()
        .child(step(add("b-rest"), "rest", cx))
        .child(step(add("b-hover").look(Look::HOVER), "hover", cx))
        .child(step(add("b-press").look(Look::PRESS), "press", cx))
        .child(step(add("b-focus").look(Look::FOCUS), "focus", cx))
        .child(step(add("b-busy").busy(true), "busy", cx))
        .child(step(add("b-off").disabled(true), "disabled", cx));
    let family = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(12.0))
        .child(button("f-filter", "Filter", m))
        .child(button("f-compare", "Compare", m).intent(Intent::Edge))
        .child(button("f-cancel", "Cancel", m).intent(Intent::Ghost))
        .child(button("f-remove", "Remove package", m).danger().glyph(Glyph::Cross))
        .child(button("f-add", "Add", m).primary().size(Control::Small).glyph(Glyph::Plus))
        .child(
            button("f-resolve", "Resolve dependencies", m)
                .primary()
                .size(Control::Large)
                .icon(Icon::Play),
        )
        .child(button("f-seal", "Seal now", m).primary().icon(Icon::Seal));
    let icons = div()
        .flex()
        .items_center()
        .gap(px(14.0))
        .child(icon_button("i-search", Icon::Search, "Search", m))
        .child(icon_button("i-filter", Icon::Filter, "Filter", m).look(Look::HOVER))
        .child(icon_button("i-shelf", Icon::SideL, "Toggle shelf", m).on(true))
        .child(icon_button("i-focus", Icon::Inbox, "Inbox", m).look(Look::FOCUS))
        .child(icon_button("i-press", Icon::Trail, "Trail map", m).look(Look::PRESS))
        .child(
            seg("lens-well", m)
                .well()
                .label("Map")
                .label("Readme")
                .label("Depends")
                .selected(board.lens.min(2))
                .on_select(with_index(me, |board, index| board.lens = index)),
        );
    section(
        cx,
        "Buttons and icon buttons",
        "Every plate that clicks is the same flat surface as a card, cut small. Colour marks \
         intent; the bevel marks what is happening to it right now.",
    )
    .child(
        div()
            .flex()
            .justify_between()
            .items_start()
            .gap(px(40.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(caption(
                        "One control, its whole life. The bevel is the only thing that ever changes.",
                        cx,
                    ))
                    .child(states),
            )
            .child(sweep_strip(m, cx)),
    )
    .child(family)
    .child(icons)
}

fn toggles_section(board: &Board, me: &WeakEntity<Board>, measure: &Measure, cx: &App) -> Div {
    let m = measure;
    let switches = div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(switch("s-std", true, m).label("std").note("on by default"))
        .child(switch("s-unstable", false, m).label("unstable"))
        .child(switch("s-derive", true, m).label("derive").note("focused").look(Look::FOCUS))
        .child(switch("s-rc", false, m).label("rc").note("disabled: implied by std").disabled(true));
    let checks = div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(check("c-ser", Tri::On, m).label("Serialize"))
        .child(check("c-de", Tri::Off, m).label("Deserializer"))
        .child(check("c-debug", Tri::On, m).label("Debug").note("focused").look(Look::FOCUS))
        .child(check("c-some", Tri::Mixed, m).label("Derives").note("some"))
        .child(radio("r-latest", true, m).label("Latest"))
        .child(radio("r-pinned", false, m).label("Your pin"));
    let combs = div()
        .flex()
        .flex_col()
        .gap(px(18.0))
        .child(
            version_comb("board-comb", board.histories[0].clone(), &m.within(px(236.0) * m.scale()))
                .pinned(board.pins[0])
                .selected(board.viewing[0])
                .on_select(on_version(me, 0)),
        )
        .child(
            version_comb("board-comb-far", board.histories[0].clone(), &m.within(px(236.0) * m.scale()))
                .pinned(board.pins[0])
                .selected(9),
        );
    section(
        cx,
        "Switches, checkboxes, the version comb",
        "No pills, no rounded pucks. One geometry, the diamond, for anything with two states, \
         and for the value that sits between them.",
    )
    .child(
        div()
            .flex()
            .gap(px(60.0))
            .items_start()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(caption(
                        "A diamond that fills mint when it is on and slides along a short track.",
                        cx,
                    ))
                    .child(switches),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(caption(
                        "A checkbox is the same diamond, standing still: it fills, it does not travel.",
                        cx,
                    ))
                    .child(checks),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(caption(
                        "Releases are a comb: tall for minors, mint for your pin, the ring is the one you read.",
                        cx,
                    ))
                    .child(combs),
            ),
    )
}

fn inputs_section(board: &Board, me: &WeakEntity<Board>, measure: &Measure, cx: &App) -> Div {
    let m = measure;
    let column = |el: AnyElement, label: &'static str| {
        step(div().w(px(216.0) * m.scale()).child(el), label, cx)
    };
    section(
        cx,
        "Inputs and the select trigger",
        "A text field is a cut plate with a caret in it. Focus turns the bevel periwinkle; a field \
         that refuses your input turns it coral. Nothing else moves.",
    )
    .child(
        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap(px(26.0))
            .child(column(
                field("in-rest", &board.inputs[0], m).icon(Icon::Filter).into_any_element(),
                "rest",
            ))
            .child(column(
                field("in-focus", &board.inputs[1], m)
                    .icon(Icon::Search)
                    .look(Look::FOCUS)
                    .into_any_element(),
                "focus · periwinkle bevel",
            ))
            .child(column(
                field("in-bad", &board.inputs[2], m)
                    .fault("not a registry: use https://")
                    .into_any_element(),
                "bad · coral bevel",
            ))
            .child(step(
                select("sel-rest", "Rust", m).lang(Lang::Rust),
                "select · rest",
                cx,
            ))
            .child(step(
                select("sel-open", "Python", m).lang(Lang::Python).open(true),
                "select · open",
                cx,
            ))
            .child(step(
                language_select("sel-live", board, me, m),
                "select · live",
                cx,
            )),
    )
}

fn keys_section(measure: &Measure, cx: &App) -> Div {
    let m = measure;
    let palette = cx.palette();
    let demo = |chord: &[&str], label: &'static str| {
        cut()
            .chamfer(Chamfer::Sm)
            .bevel(Bevel::Rest)
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(14.0))
            .py(px(10.0))
            .child(keys(chord, KbdVoice::Plain, m))
            .child(
                div()
                    .set(ty::ROW, m)
                    .text_color(palette.ink2.hsla())
                    .child(SharedString::from(label)),
            )
    };
    section(cx, "Key caps", "Quiet until asked.").child(
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(30.0))
            .child(demo(&["⌘", "K"], "jump to anything"))
            .child(demo(&["⌥", "→"], "follow, in a peek"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(kbd("S", m).hot())
                    .child(kbd("F", m).hot())
                    .child(kbd("AS", m).hint())
                    .child(kbd("GJ", m).hint())
                    .child(kbd("Space", m)),
            ),
    )
}

/// One Settings row: the label left, the control right, hairline between.
fn settings_row(label: &'static str, control: impl IntoElement, first: bool, cx: &App) -> Div {
    let palette = cx.palette();
    let row = div()
        .flex()
        .items_center()
        .gap(px(24.0))
        .min_h(px(64.0))
        .child(
            div()
                .w(px(150.0))
                .flex_none()
                .child(text(SLAB, cx.facet(), palette.ink1, label)),
        )
        .child(div().flex_1().flex().justify_end().child(control));
    if first {
        row
    } else {
        row.border_t_1().border_color(palette.line1.hsla())
    }
}

fn settings_section(board: &Board, me: &WeakEntity<Board>, measure: &Measure, cx: &App) -> Div {
    let m = measure;
    let rows = div()
        .flex()
        .flex_col()
        .w(px(600.0))
        .child(settings_row(
            "Theme",
            theme_toggle(
                "set-theme",
                [Swatch::System, Swatch::Abyss, Swatch::Glacier][board.theme.min(2)],
                m,
            )
            .on_select(with_index(me, |board, index| board.theme = index)),
            true,
            cx,
        ))
        .child(settings_row(
            "Contrast",
            seg("set-contrast", m).label("Normal").label("High").selected(0),
            false,
            cx,
        ))
        .child(settings_row(
            "Density",
            density_toggle(
                "set-density",
                [Density::Comfortable, Density::Compact, Density::Dense][board.density.min(2)],
                m,
            )
            .on_select(with_index(me, |board, index| board.density = index)),
            false,
            cx,
        ))
        .child(settings_row(
            "Motion",
            seg("set-motion", m).label("System").label("Full").label("Reduced").selected(0),
            false,
            cx,
        ));
    section(
        cx,
        "Settings rows",
        "Rows of cut stones: the chosen one is the only plate.",
    )
    .child(rows)
}

fn controls_board(
    board: &Board,
    me: &WeakEntity<Board>,
    _window: &mut Window,
    cx: &mut Context<Board>,
) -> AnyElement {
    let measure = cx.facet().measure(px(1440.0 - 128.0));
    let palette = cx.palette();
    let column = doc(cx, "FACET 10", "Controls")
        .gap(px(34.0))
        .child(
            text(
                ty::LEDE,
                cx.facet(),
                palette.ink2,
                "Buttons, toggles and fields are the same cut plate as everything else in Nudox, \
                 only small enough to click. Colour says what a control means; the bevel says \
                 what state it is in.",
            )
            .max_w(px(640.0)),
        )
        .child(buttons_section(board, me, &measure, cx))
        .child(toggles_section(board, me, &measure, cx))
        .child(inputs_section(board, me, &measure, cx))
        .child(keys_section(&measure, cx))
        .child(settings_section(board, me, &measure, cx));
    canvas(cx, column)
}

/// The states grid: one row per control, one column per state.
fn states(board: &Board, _window: &mut Window, cx: &mut Context<Board>) -> AnyElement {
    let m = cx.facet().measure(px(1440.0 - 96.0));
    let palette = cx.palette();
    let head = |label: &'static str| {
        div()
            .w(px(170.0))
            .flex_none()
            .child(text(CLABEL, cx.facet(), palette.ink3, label))
    };
    let cell = |el: AnyElement| div().w(px(170.0)).flex_none().flex().child(el);
    let looks = [
        ("rest", Look::LIVE, false, false),
        ("hover", Look::HOVER, false, false),
        ("press", Look::PRESS, false, false),
        ("focus", Look::FOCUS, false, false),
        ("on / busy / fault", Look::LIVE, true, false),
        ("disabled", Look::LIVE, false, true),
    ];
    let mut rows = div().flex().flex_col().gap(px(22.0)).child(
        div()
            .flex()
            .child(head(""))
            .children(looks.iter().map(|(name, ..)| head(name))),
    );
    type Make<'a> = &'a dyn Fn(String, Look, bool, bool) -> AnyElement;
    let row = |name: &'static str, make: Make<'_>| {
        div()
            .flex()
            .items_center()
            .min_h(px(40.0))
            .child(head(name))
            .children(looks.iter().enumerate().map(|(i, (_, look, alt, off))| {
                cell(make(format!("{name}-{i}"), *look, *alt, *off))
            }))
    };
    rows = rows
        .child(row("button", &|id, look, alt, off| {
            button(ElementId::from(id), "Add package", &m)
                .glyph(Glyph::Plus)
                .look(look)
                .busy(alt)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("primary", &|id, look, alt, off| {
            button(ElementId::from(id), "Resolve", &m)
                .primary()
                .icon(Icon::Play)
                .look(look)
                .busy(alt)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("ghost", &|id, look, alt, off| {
            button(ElementId::from(id), "Cancel", &m)
                .ghost()
                .look(look)
                .busy(alt)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("danger", &|id, look, alt, off| {
            button(ElementId::from(id), "Remove", &m)
                .danger()
                .glyph(Glyph::Cross)
                .look(look)
                .busy(alt)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("icon button", &|id, look, alt, off| {
            icon_button(ElementId::from(id), Icon::SideL, "Shelf", &m)
                .look(look)
                .on(alt)
                .disabled(off)
                .key("\\")
                .into_any_element()
        }))
        .child(row("switch", &|id, look, alt, off| {
            switch(ElementId::from(id), alt, &m)
                .label("derive")
                .look(look)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("check", &|id, look, alt, off| {
            check(ElementId::from(id), if alt { Tri::On } else { Tri::Off }, &m)
                .label("Debug")
                .look(look)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("radio", &|id, look, alt, off| {
            radio(ElementId::from(id), alt, &m)
                .label("Latest")
                .look(look)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("stones", &|id, look, alt, off| {
            seg(ElementId::from(id), &m)
                .label("Map")
                .label("List")
                .selected(usize::from(alt))
                .look(look)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("comb", &|id, look, alt, _off| {
            let comb = version_comb(ElementId::from(id), board.histories[0].clone(), &m.within(px(150.0)))
                .pinned(board.pins[0])
                .selected(if alt { 9 } else { board.pins[0] });
            let comb = if look.focus { comb.focus_look() } else { comb };
            div().w(px(150.0)).child(comb).into_any_element()
        }))
        .child(row("select", &|id, look, alt, off| {
            select(ElementId::from(id), "Rust", &m)
                .lang(Lang::Rust)
                .look(look)
                .open(alt)
                .disabled(off)
                .into_any_element()
        }))
        .child(row("field", &|id, look, alt, off| {
            let mut f = field(ElementId::from(id), &board.inputs[3], &m)
                .icon(Icon::Filter)
                .look(look)
                .disabled(off);
            if alt {
                f = f.fault("not a package name");
            }
            div().w(px(160.0)).child(f).into_any_element()
        }));
    div()
        .size_full()
        .relative()
        .bg(palette.g1)
        .child(ground())
        .child(div().p(px(48.0)).child(rows))
        .into_any_element()
}

/// Live controls at fixed positions (the script's coordinates).
fn live(board: &Board, me: &WeakEntity<Board>, window: &mut Window, cx: &mut Context<Board>) -> AnyElement {
    let facet = cx.facet();
    let m = facet.measure(px(1200.0));
    let palette = cx.palette();
    let at = |x: f32, y: f32, key: &'static str, el: AnyElement| {
        div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .child(probe::measure(key, el))
    };
    let panel = split_panel(board, me, &m, window, cx)
        .absolute()
        .left(px(600.0))
        .top(px(300.0))
        .w(px(560.0))
        .h(px(240.0));
    div()
        .size_full()
        .relative()
        .bg(palette.g1)
        .child(ground())
        .child(at(
            60.0,
            80.0,
            "live-button",
            button("live-add", "Add package", &m)
                .glyph(Glyph::Plus)
                .key("A")
                .into_any_element(),
        ))
        .child(at(
            300.0,
            80.0,
            "live-icon",
            icon_button("live-shelf", Icon::SideL, "Shelf", &m)
                .key("\\")
                .into_any_element(),
        ))
        .child(at(400.0, 80.0, "live-switch", live_switch(board, me, &m)))
        .child(at(560.0, 80.0, "live-check", live_check(board, me, &m)))
        .child(at(680.0, 80.0, "live-radio", live_radio(board, me, &m)))
        .child(at(800.0, 80.0, "live-seg", live_seg(board, me, &m)))
        .child(at(60.0, 210.0, "live-comb", live_comb(board, me, &m.within(px(236.0)))))
        .child(at(
            60.0,
            300.0,
            "live-field",
            div()
                .w(px(280.0))
                .child(field("live-field", &board.inputs[0], &m).icon(Icon::Filter))
                .into_any_element(),
        ))
        .child(at(
            60.0,
            380.0,
            "live-select",
            language_select("live-select", board, me, &m).into_any_element(),
        ))
        .child(panel)
        .into_any_element()
}

/// How many cells the storm flow holds.
const STORM_CELLS: usize = 10;

/// The live controls as a flow that wraps to any window and scrolls, for
/// storms: whatever holds keyboard focus is scrolled into view when focus
/// moves or the window changes size, so focus is never left off screen.
fn storm_flow(board: &mut Board, me: &WeakEntity<Board>, window: &mut Window, cx: &mut Context<Board>) -> AnyElement {
    let facet = cx.facet();
    let palette = cx.palette();
    let viewport = window.viewport_size();
    let m = facet.measure(viewport.width);
    let s = m.scale();
    if let Some(cell) = board.cells.iter().position(|cell| cell.contains_focused(window, cx))
        && board.followed != Some((cell, viewport))
    {
        board.scroll.scroll_to_item(cell);
        board.followed = Some((cell, viewport));
    }
    let controls: [(&'static str, AnyElement); STORM_CELLS] = [
        (
            "live-button",
            button("live-add", "Add package", &m).glyph(Glyph::Plus).key("A").into_any_element(),
        ),
        (
            "live-icon",
            icon_button("live-shelf", Icon::SideL, "Shelf", &m).key("\\").into_any_element(),
        ),
        ("live-switch", live_switch(board, me, &m)),
        ("live-check", live_check(board, me, &m)),
        ("live-radio", live_radio(board, me, &m)),
        ("live-seg", live_seg(board, me, &m)),
        ("live-comb", live_comb(board, me, &m.within(px(236.0) * s))),
        (
            "live-field",
            div()
                .w(px(280.0) * s)
                .child(field("live-field", &board.inputs[0], &m).icon(Icon::Filter))
                .into_any_element(),
        ),
        ("live-select", language_select("live-select", board, me, &m).into_any_element()),
        (
            "split-row",
            split_panel(board, me, &m, window, cx)
                .w(px(480.0) * s)
                .h(px(160.0) * s)
                .into_any_element(),
        ),
    ];
    let gap = m.space(Space::Roomy) * 2.0;
    div()
        .id("storm-flow")
        .size_full()
        .overflow_y_scroll()
        .track_scroll(&board.scroll)
        .bg(palette.g1)
        .p(gap)
        .flex()
        .flex_wrap()
        .content_start()
        .items_center()
        .gap(gap)
        .children(controls.into_iter().zip(&board.cells).map(|((key, control), cell)| {
            div().track_focus(cell).child(probe::measure(key, control))
        }))
        .into_any_element()
}

fn live_switch(board: &Board, me: &WeakEntity<Board>, m: &Measure) -> AnyElement {
    switch("live-switch", board.switch_on, m)
        .label("derive")
        .on_toggle(with(me, |board| board.switch_on = !board.switch_on))
        .into_any_element()
}

fn live_check(board: &Board, me: &WeakEntity<Board>, m: &Measure) -> AnyElement {
    check("live-check", board.check, m)
        .label("Debug")
        .on_toggle(with(me, |board| {
            board.check = if board.check == Tri::On { Tri::Off } else { Tri::On };
        }))
        .into_any_element()
}

fn live_radio(board: &Board, me: &WeakEntity<Board>, m: &Measure) -> AnyElement {
    radio("live-radio", board.radio == 1, m)
        .label("Pin")
        .on_toggle(with(me, |board| board.radio = 1))
        .into_any_element()
}

fn live_seg(board: &Board, me: &WeakEntity<Board>, m: &Measure) -> AnyElement {
    seg("live-seg", m)
        .label("Reference")
        .key("1")
        .label("Relations")
        .key("2")
        .label("Usage")
        .key("3")
        .selected(board.lens)
        .on_select(with_index(me, |board, index| board.lens = index))
        .into_any_element()
}

fn live_comb(board: &Board, me: &WeakEntity<Board>, m: &Measure) -> AnyElement {
    version_comb("live-comb", board.histories[0].clone(), m)
        .pinned(board.pins[0])
        .selected(board.viewing[0])
        .on_select(on_version(me, 0))
        .into_any_element()
}

/// A pane beside the shelf, its width the splitter's.
fn split_panel(board: &Board, me: &WeakEntity<Board>, m: &Measure, window: &mut Window, cx: &mut Context<Board>) -> Div {
    let palette = cx.palette();
    let split_id = ElementId::from("live-split");
    let width = split_width(&split_id, &board.split, PanelSide::Left, window, cx);
    div()
        .flex()
        .overflow_hidden()
        .border_1()
        .border_color(palette.line1.hsla())
        .child(
            probe::measure(
                "split-panel",
                div()
                    .h_full()
                    .w(width)
                    .flex_none()
                    .bg(palette.pane)
                    .border_r_1()
                    .border_color(palette.line1.hsla()),
            ),
        )
        .child(splitter(split_id, board.split, m).on_change({
            let me = me.clone();
            move |event, _window, cx| {
                let _ = me.update(cx, |board, cx| {
                    board.split = match event {
                        SplitEvent::Resize(width) => SplitModel {
                            width,
                            is_collapsed: false,
                            ..board.split
                        },
                        SplitEvent::Collapse => SplitModel {
                            is_collapsed: true,
                            ..board.split
                        },
                        SplitEvent::Expand => SplitModel {
                            is_collapsed: false,
                            ..board.split
                        },
                        SplitEvent::Reset => SplitModel {
                            width: board.split.default,
                            is_collapsed: false,
                            ..board.split
                        },
                    };
                    cx.notify();
                });
            }
        }))
        .child(div().flex_1())
}

/// The version comb as the target draws it: a shelf's book at 264 px in
/// each cell — at rest, hovering, scrubbed, 400 releases at rest, 400 under
/// the pointer — and a young package with three releases.
fn comb_scene(board: &Board, me: &WeakEntity<Board>, cx: &mut Context<Board>) -> AnyElement {
    let facet = cx.facet();
    let palette = cx.palette();
    let s = facet.text_scale;
    let shelf = facet.measure(px(264.0) * s);
    let cell = |label: &'static str, book: crate::chrome::Book, key: &'static str, slot: Option<usize>, x: f32, y: f32| {
        let handler = slot.map(|slot| {
            let handler: Rc<dyn Fn(&super::VersionSelected, &mut Window, &mut App)> =
                Rc::new(on_version(me, slot));
            handler
        });
        div()
            .absolute()
            .left(px(x) * s)
            .top(px(y) * s)
            .flex()
            .flex_col()
            .gap(px(10.0) * s)
            .child(text(CELL, facet, palette.ink3, label))
            .child(probe::measure(
                key,
                div()
                    .w(px(264.0) * s)
                    .pt(px(8.0) * s)
                    .pb(px(2.0) * s)
                    .bg(palette.pane)
                    .border_r_1()
                    .border_color(palette.line1.hsla())
                    .child(crate::chrome::book(key, &book, &shelf, handler, cx)),
            ))
    };
    let book = |slot: usize, name: &'static str| {
        let releases = board.histories[slot].clone();
        let pin = board.pins[slot];
        crate::chrome::Book::new(crate::icons::Kind::Package, name, releases[pin].version.clone())
            .releases(releases, pin, board.viewing[slot])
    };
    // The target's still states (hover, scrubbed, lens) are pinned looks.
    let sheet = |slot: usize, name: &'static str, view: Option<usize>, hot: Option<usize>, lens: Option<f32>| {
        let mut b = book(slot, name);
        // A still state does not follow the live cells' scrubbing.
        b.viewing = Some(view.unwrap_or(board.pins[slot]));
        b.hover_look = hot;
        b.lens_look = lens;
        b
    };
    let over_121 = board.histories[2]
        .iter()
        .position(|release| release.version.as_ref() == "1.21.0")
        .unwrap_or(0);
    div()
        .size_full()
        .relative()
        .bg(palette.g1)
        .child(
            div()
                .absolute()
                .left(px(48.0) * s)
                .top(px(40.0) * s)
                .child(text(TITLE, facet, palette.ink0, "The version comb")),
        )
        .child(cell("At rest", book(0, "present"), "comb-rest", Some(0), 48.0, 110.0))
        .child(cell(
            "Hovering a release",
            sheet(0, "present", None, Some(9), None),
            "comb-hover",
            None,
            360.0,
            110.0,
        ))
        .child(cell(
            "Scrubbed to an older release",
            sheet(0, "present", Some(9), None, None),
            "comb-scrub",
            None,
            672.0,
            110.0,
        ))
        .child(cell("400 releases at rest", book(2, "tokio"), "comb-400", Some(2), 48.0, 370.0))
        .child(cell(
            "400 releases, pointer over 1.21",
            sheet(2, "tokio", None, Some(over_121), Some(118.0)),
            "comb-400-lens",
            None,
            360.0,
            370.0,
        ))
        .child(cell("3 releases", book(1, "fresh"), "comb-3", Some(1), 672.0, 370.0))
        .child(cell(
            "toml: mint moves touch your code · [ ] step",
            {
                let mut b = book(3, "toml");
                b.touches = board.toml_touches.clone();
                b
            },
            "comb-toml",
            Some(3),
            48.0,
            630.0,
        ))
        .into_any_element()
}
