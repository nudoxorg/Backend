//! Chrome scenes.
//!
//! - `chrome`: the frame every page sits in — titlebar, shelf (or spine, or
//!   nothing), a quiet reader, the pinned column at vast widths, the status
//!   bar — at whatever size it is captured (`--size 2560x1440`, `1440x900`,
//!   `1100x900`, `760x900`, `480x900`; `--text-scale 200`), degrading the way
//!   the flow targets do;
//! - `chrome-density`: the shelf and its titlebar at comfortable, compact
//!   and dense, side by side;
//! - `chrome-thread`: a titlebar whose trail descends three times and walks
//!   back once (film it: the thread makes room, older beads shrink);
//! - `chrome-resize`: the window dragged from 1440 down to 480 and back
//!   (film it: pieces leave and return, nothing pops).

#![allow(clippy::too_many_lines)]

use super::{
    Book, Bead, Here, Lights, RowTone, ShelfData, ShelfRow, TitleButton, TitlebarData, shelf, spine,
    status_bar, titlebar,
};
use crate::Set;
use crate::gallery::{Scene, declare_script};
use crate::icons::{Icon, Kind};
use crate::measure::{Density, Measure};
use crate::paint::{gem, ground};
use crate::theme::{ActiveFacet, Facet};
use crate::tokens::{Face, TypeRole, ty};
use gpui::{
    AnyElement, AnyView, App, AppContext, Context, ElementId, Entity, IntoElement, ParentElement,
    Render, SharedString, Styled, Window, div, px,
};
use gpui_component::input::InputState;
use std::time::Duration;

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "chrome",
        title: "The chrome at the capture's size: titlebar thread, shelf / spine / none, pinned \
                column at vast widths, status bar (capture at 2560/1440/1100/760/480 and 200 %)",
        size: (1440, 900),
        build: |window, cx| Chrome::view(Mode::Window, window, cx),
    },
    Scene {
        id: "chrome-density",
        title: "Titlebar and shelf at comfortable, compact and dense",
        size: (1500, 640),
        build: |window, cx| Chrome::view(Mode::Density, window, cx),
    },
    Scene {
        id: "chrome-thread",
        title: "The bead thread descending three times and walking back once (film 0..3200)",
        size: (1440, 120),
        build: |window, cx| Chrome::view(Mode::Thread, window, cx),
    },
    Scene {
        id: "chrome-resize",
        title: "The window dragged from 1440 to 480 and back: pieces leave and return (film it)",
        size: (1440, 900),
        build: |window, cx| {
            declare_script(RESIZE_SCRIPT, cx);
            Chrome::view(Mode::Window, window, cx)
        },
    },
];

const RESIZE_SCRIPT: &str = "
resize 1440x900 @0
resize 1300x900 @200
resize 1150x900 @300
resize 1050x900 @400
resize 900x900 @700
resize 800x900 @800
resize 700x900 @1100
resize 600x900 @1200
resize 480x900 @1500
resize 700x900 @2200
resize 1100x900 @2500
resize 1440x900 @2800
";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Window,
    Density,
    Thread,
}

struct Chrome {
    mode: Mode,
    filter: Entity<InputState>,
    step: usize,
}

impl Chrome {
    fn view(mode: Mode, window: &mut Window, cx: &mut App) -> AnyView {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        cx.new(|cx| {
            if mode == Mode::Thread {
                // The scene's own clock (virtual time headless): one step at
                // each of STEPS.
                cx.spawn(async move |this: gpui::WeakEntity<Chrome>, cx| {
                    let mut at = 0;
                    for step in STEPS {
                        let timer = cx
                            .background_executor()
                            .timer(Duration::from_millis(step - at));
                        timer.await;
                        at = step;
                        if this
                            .update(cx, |this: &mut Chrome, cx| {
                                this.step += 1;
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                })
                .detach();
            }
            Self {
                mode,
                filter,
                step: 0,
            }
        })
        .into()
    }
}

fn bead(key: &str, kind: Kind, name: &str) -> Bead {
    Bead {
        key: ElementId::from(SharedString::from(key.to_owned())),
        kind,
        name: SharedString::from(name.to_owned()),
    }
}

fn buttons() -> Vec<TitleButton> {
    vec![
        TitleButton {
            id: "trail".into(),
            icon: Icon::Trail,
            label: "Trail map".into(),
            key: Some("T".into()),
            on: false,
        },
        TitleButton {
            id: "inbox".into(),
            icon: Icon::Inbox,
            label: "Inbox".into(),
            key: Some("I".into()),
            on: false,
        },
    ]
}

/// The flow targets' trail: present › glyph › KindGlyph › RelationLabel,
/// with Typed ahead.
fn flow_trail(shelf_open: bool) -> TitlebarData {
    TitlebarData {
        lights: Lights::StandIn,
        shelf_open: Some(shelf_open),
        behind: vec![
            bead("present", Kind::Module, "present"),
            bead("glyph", Kind::Module, "glyph"),
            bead("KindGlyph", Kind::Struct, "KindGlyph"),
        ],
        here: Here::Page {
            kind: Kind::Enum,
            name: "RelationLabel".into(),
            path: "present › glyph".into(),
        },
        ahead: vec![bead("Typed", Kind::Variant, "Typed")],
        buttons: buttons(),
    }
}

fn flow_rows() -> Vec<ShelfRow> {
    vec![
        ShelfRow::new("identity", Kind::Module, "identity"),
        ShelfRow::new("page", Kind::Module, "page"),
        ShelfRow::new("signature", Kind::Module, "signature").tone(RowTone::Dim),
        ShelfRow::new("glyph", Kind::Module, "glyph").tone(RowTone::Open),
        ShelfRow::new("RelationLabel", Kind::Enum, "RelationLabel")
            .depth(1)
            .tone(RowTone::Current)
            .note("9 uses"),
        ShelfRow::new("RelationDirection", Kind::Enum, "RelationDirection")
            .depth(1)
            .note("4 uses"),
        ShelfRow::new("KindGlyph", Kind::Struct, "KindGlyph").depth(1).note("2 uses"),
        ShelfRow::new("GlyphSet", Kind::Trait, "GlyphSet").depth(1).tone(RowTone::Dim),
        ShelfRow::new("relation_label", Kind::Function, "relation_label").depth(1),
        ShelfRow::new("MAX_GLYPHS", Kind::Constant, "MAX_GLYPHS")
            .depth(1)
            .tone(RowTone::Dim),
        ShelfRow::new("fault", Kind::Module, "fault"),
        ShelfRow::new("outline", Kind::Module, "outline"),
    ]
}

fn shelf_data(filter: &Entity<InputState>) -> ShelfData {
    ShelfData {
        up: Some("backend".into()),
        book: Some(Book::new(Kind::Package, "present", "0.4.2")),
        filter: Some(filter.clone()),
        rows: flow_rows(),
        focused: None,
    }
}

const LEDE: TypeRole = TypeRole {
    face: Face::Serif,
    weight: 400.0,
    size: 18.0,
    line: 25.0,
    tracking: 0.0,
    italic: true,
};

/// A quiet reader: the page's hero only (the pages are another lane's).
fn reader(measure: &Measure, cx: &App) -> AnyElement {
    let palette = cx.palette();
    let s = measure.scale();
    div()
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .overflow_hidden()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(880.0 * s))
                .pt(measure.fluid(22.0, 64.0))
                .px(measure.fluid(16.0, 48.0))
                .flex()
                .items_center()
                .gap(measure.fluid(14.0, 22.0))
                .child(gem(Kind::Enum).size(f32::from(measure.fluid(44.0, 64.0))))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .set(ty::HERO, measure)
                                .text_color(palette.ink0.hsla())
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child("RelationLabel"),
                        )
                        .child(
                            div()
                                .set(LEDE, measure)
                                .text_color(palette.ink2.hsla())
                                .child("The readable label of one relation group."),
                        ),
                ),
        )
        .into_any_element()
}

/// The whole frame for a window `width` × `height` under `facet`.
fn window_frame(facet: &Facet, width: f32, height: f32, filter: &Entity<InputState>, window: &mut Window, cx: &mut App) -> AnyElement {
    let palette = cx.palette();
    let win = facet.measure(px(width));
    let s = win.scale();
    let effective = win.effective();
    let shelf_w = (effective * 0.18).clamp(220.0, 264.0) * s;
    // The flow targets' container queries: `max-width` is inclusive.
    let side: Option<AnyElement> = if effective > 900.0 {
        Some(
            shelf("shelf", shelf_data(filter), &facet.measure(px(shelf_w)))
                .into_any_element(),
        )
    } else if effective > 640.0 {
        Some(
            spine(
                "spine",
                flow_rows()
                    .into_iter()
                    .filter(|row| row.depth == 1)
                    .map(|row| (row.key, row.kind, row.name))
                    .collect(),
                Some(0),
                &win,
            )
            .into_any_element(),
        )
    } else {
        None
    };
    let pins_w = 280.0 * s;
    let pins = (effective >= 1900.0)
        .then(|| super::pins_frame(&facet.measure(px(pins_w)), window, cx));
    let reader_w = width
        - side.as_ref().map_or(0.0, |_| if effective > 900.0 { shelf_w } else { 42.0 * s })
        - pins.as_ref().map_or(0.0, |_| pins_w);
    div()
        .size_full()
        .relative()
        .bg(palette.g1)
        .child(ground())
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .child(titlebar("titlebar", flow_trail(side.is_some()), &win))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h(px(0.0))
                        .children(side)
                        .child(reader(&facet.measure(px(reader_w)), cx))
                        .children(pins),
                )
                .child(status_bar("nudox://present/glyph/RelationLabel", &win, cx)),
        )
        .h(px(height))
        .into_any_element()
}

/// The trail at `step` descents (then one walk back at step 4).
fn trail_at(step: usize) -> TitlebarData {
    let pages = [
        bead("present", Kind::Module, "present"),
        bead("glyph", Kind::Module, "glyph"),
        bead("KindGlyph", Kind::Struct, "KindGlyph"),
        bead("RelationLabel", Kind::Enum, "RelationLabel"),
        bead("Typed", Kind::Variant, "Typed"),
    ];
    let paths = [
        "backend",
        "present",
        "present › glyph",
        "present › glyph",
        "glyph › RelationLabel",
    ];
    // Where "here" is: one descent per step, then back one.
    let at = match step {
        0 => 1,
        1 => 2,
        2 => 3,
        3 => 4,
        _ => 3,
    };
    let here = &pages[at];
    TitlebarData {
        lights: Lights::StandIn,
        shelf_open: Some(true),
        behind: pages[..at].to_vec(),
        here: Here::Page {
            kind: here.kind,
            name: here.name.clone(),
            path: paths[at].into(),
        },
        ahead: pages[at + 1..].to_vec(),
        buttons: buttons(),
    }
}

/// When the thread scene takes each step (virtual ms after the first frame).
const STEPS: [u64; 4] = [400, 1200, 2000, 2800];

impl Render for Chrome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = cx.palette();
        let viewport = window.viewport_size();
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let content = match self.mode {
            Mode::Window => window_frame(&facet, width, height, &self.filter, window, cx),
            Mode::Density => {
                let column = |density: Density, label: &'static str, cx: &mut App| {
                    let facet = Facet { density, ..facet };
                    let m = facet.measure(px(480.0));
                    div()
                        .w(px(480.0))
                        .h_full()
                        .flex()
                        .flex_col()
                        .border_r_1()
                        .border_color(palette.line1.hsla())
                        .child(titlebar(
                            ElementId::from(SharedString::from(format!("tb-{label}"))),
                            flow_trail(true),
                            &facet.measure(px(1100.0)),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_h(px(0.0))
                                .child(
                                    shelf(
                                        ElementId::from(SharedString::from(format!("shelf-{label}"))),
                                        shelf_data(&self.filter),
                                        &facet.measure(px(264.0)),
                                    ),
                                )
                                .child(
                                    div().flex_1().p(px(16.0)).child(
                                        div()
                                            .set(ty::LABEL, &m)
                                            .text_color(palette.ink3.hsla())
                                            .child(label),
                                    ),
                                ),
                        )
                        .child(status_bar("nudox://present/glyph/RelationLabel", &m, cx))
                        .into_any_element()
                };
                div()
                    .size_full()
                    .relative()
                    .bg(palette.g1)
                    .child(ground())
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .gap(px(30.0))
                            .child(column(Density::Comfortable, "comfortable", cx))
                            .child(column(Density::Compact, "compact", cx))
                            .child(column(Density::Dense, "dense", cx)),
                    )
                    .into_any_element()
            }
            Mode::Thread => {
                let step = self.step;
                div()
                    .size_full()
                    .relative()
                    .bg(palette.g1)
                    .child(ground())
                    .child(titlebar("thread", trail_at(step), &facet.measure(px(width))))
                    .into_any_element()
            }
        };
        // The float layer (tips) is the root's last child.
        div()
            .size_full()
            .child(content)
            .child(crate::overlay::float::layer(window, cx))
    }
}
