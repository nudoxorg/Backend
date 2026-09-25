//! Overlay scenes beyond the peek: the tip, the menu, toasts, the dialog and
//! hint mode, each still and as a film. States are scripted on the executor
//! clock.

#![allow(clippy::too_many_lines)]

use super::dialog::{self, Dialog, DialogButton};
use super::float::{self, FloatKind, FloatRequest, Side};
use super::menu::{self, Menu, MenuItem};
use super::toast::{self, Toast};
use super::tooltip::{self, TipText, Tipped};
use crate::controls::button;
use crate::gallery::Scene;
use crate::icons::{Icon, Kind};
use crate::measure::Set;
use crate::paint::{gem, ground};
use crate::theme::ActiveFacet;
use crate::tokens::{Voice, ty};
use gpui::{
    AnyView, App, AppContext, Bounds, Context, ElementId, InteractiveElement, IntoElement,
    Keystroke, ParentElement, Pixels, Render, SharedString, Styled, Window, div, px,
};
use std::time::Duration;

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "overlays",
        title: "Tip, menu (second row selected) and the toast stack",
        size: (1100, 620),
        build: overlays,
    },
    Scene {
        id: "dialog",
        title: "Dialog over its scrim",
        size: (900, 560),
        build: dialog_still,
    },
    Scene {
        id: "hint-mode",
        title: "Hint mode: home-row codes over every trigger",
        size: (1100, 620),
        build: hints,
    },
    Scene {
        id: "hint-mode-typed",
        title: "Hint mode after typing S: only the S codes remain, S dropped",
        size: (1100, 620),
        build: hints_typed,
    },
    Scene {
        id: "menu-film",
        title: "Film: menu opens at 1, ↓ at 150 and 300 (the plate glides), ↵ at 500",
        size: (700, 420),
        build: menu_film,
    },
    Scene {
        id: "toast-film",
        title: "Film: a toast at 1, a second at 250, the first undone at 900",
        size: (700, 420),
        build: toast_film,
    },
    Scene {
        id: "float-follow",
        title: "Film: a node flies across (a camera pan) with its peek open; the card tracks it rigidly, then exits when the node leaves the window",
        size: (900, 480),
        build: follow_film,
    },
    Scene {
        id: "dialog-film",
        title: "Film: dialog opens at 1, closed mid-entrance at 150, opened again at 260",
        size: (900, 560),
        build: dialog_film,
    },
];

type Step = Box<dyn FnOnce(&mut Window, &mut App)>;

fn script(steps: Vec<(u64, Step)>, window: &mut Window, cx: &mut App) {
    for (at, step) in steps {
        window
            .spawn(cx, async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(at))
                    .await;
                let _ = cx.update(|window, cx| step(window, cx));
            })
            .detach();
    }
}

fn key(name: &str) -> ElementId {
    ElementId::Name(SharedString::from(format!("overlays:{name}")))
}

fn anchor(name: &str, window: &Window, cx: &mut App) -> Bounds<Pixels> {
    float::reported(&key(name), window, cx).unwrap_or_default()
}

fn press(keystroke: &'static str) -> Step {
    Box::new(move |window, cx| {
        if let Ok(keystroke) = Keystroke::parse(keystroke) {
            window.dispatch_keystroke(keystroke, cx);
        }
    })
}

fn the_menu() -> Menu {
    Menu::new(
        vec![
            MenuItem::new("Open").icon(Icon::Peek).chord(&["↵"]),
            MenuItem::new("Open beside").icon(Icon::Split),
            MenuItem::new("Copy path").icon(Icon::Copy).chord(&["⌘", "C"]),
            MenuItem::new("Pin").icon(Icon::Pin).chord(&["Space"]).separated(),
            MenuItem::new("Rename").disabled(),
            MenuItem::new("Remove from shelf").danger(),
        ],
        |_, _, _| {},
    )
}

fn open_menu() -> Step {
    Box::new(|window, cx| {
        let at = anchor("more", window, cx);
        menu::open(key("more-menu"), at, Side::Below, the_menu(), window, cx);
    })
}

/// A quiet page: a title row with a gem (tipped), a "More" button (menu),
/// some rows of rest-able names, and the layer.
struct Stage {
    grid: bool,
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let measure = facet.measure(window.viewport_size().width);
        let more = key("more");
        let more_button = float::trigger(
            more.clone(),
            move |bounds| {
                let menu_key = more.clone();
                FloatRequest::new(menu_key.clone(), bounds, FloatKind::Tip, tooltip::content(TipText {
                    title: None,
                    body: "More actions".into(),
                    chord: Vec::new(),
                }))
                .side(Side::Above)
            },
            button("more-button", "More", &measure).icon(Icon::More),
        );
        let gem_tip = div()
            .id("gem")
            .child(gem(Kind::Enum).size(40.0 * measure.scale()))
            .tip_rich("Enum", "A closed set of variants.", &["⌥"]);
        let names = [
            "SemanticLinkKind",
            "RelationDirection",
            "Neighbourhood",
            "RelationLabel",
            "Page::relations",
            "method_edge",
        ];
        let mut rows = div().flex().flex_col().gap(px(10.0 * measure.scale()));
        let count = if self.grid { names.len() * 2 } else { names.len() };
        for index in 0..count {
            let name = names[index % names.len()];
            let k = key(&format!("row{index}"));
            let label: SharedString = name.into();
            rows = rows.child(float::trigger(
                k.clone(),
                move |bounds| {
                    let label = label.clone();
                    FloatRequest::new(k.clone(), bounds, FloatKind::Tip, move |measure, window, cx| {
                        tooltip::content(TipText {
                            title: None,
                            body: label.clone(),
                            chord: Vec::new(),
                        })(measure, window, cx)
                    })
                },
                div()
                    .set(ty::MONO_ROW, &measure)
                    .text_color(palette.ink1.hsla())
                    .child(name),
            ));
        }
        let rows = if self.grid {
            rows.flex_row().flex_wrap().w(px(560.0 * measure.scale()))
        } else {
            rows
        };
        div()
            .relative()
            .size_full()
            .bg(palette.g1.hsla())
            .child(ground())
            .child(
                div()
                    .absolute()
                    .left(px(48.0))
                    .top(px(40.0))
                    .flex()
                    .flex_col()
                    .gap(px(28.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(16.0))
                            .child(gem_tip)
                            .child(
                                div()
                                    .set(ty::TITLE, &measure)
                                    .text_color(palette.ink0.hsla())
                                    .child("SemanticLinkKind"),
                            )
                            .child(more_button),
                    )
                    .child(rows),
            )
            .child(float::layer(window, cx))
    }
}

fn stage(grid: bool, steps: Vec<(u64, Step)>, window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|_| Stage { grid });
    script(steps, window, cx);
    view.into()
}

fn toasts() -> Vec<(u64, Step)> {
    vec![
        (
            20,
            Box::new(|window: &mut Window, cx: &mut App| {
                toast::show(Toast::new("Pinned SemanticLinkKind").undo(|_, _| {}), window, cx);
            }) as Step,
        ),
        (
            40,
            Box::new(|window: &mut Window, cx: &mut App| {
                toast::show(
                    Toast::new("serde is two releases behind what you pin").voice(Voice::Amber),
                    window,
                    cx,
                );
            }),
        ),
    ]
}

fn overlays(window: &mut Window, cx: &mut App) -> AnyView {
    let mut steps: Vec<(u64, Step)> = vec![
        (1, open_menu()),
        (60, press("down")),
        (
            80,
            Box::new(|window, cx| {
                let at = float::reported(&ElementId::NamedChild(std::sync::Arc::new(ElementId::Name("gem".into())), "tip".into()), window, cx)
                    .unwrap_or_default();
                let request = tooltip::request(key("gem-tip"), at, TipText {
                    title: Some("Enum".into()),
                    body: "A closed set of variants.".into(),
                    chord: vec!["⌥".into()],
                });
                float::rest(request, window, cx);
            }),
        ),
    ];
    steps.extend(toasts());
    stage(false, steps, window, cx)
}

fn remove_dialog() -> Dialog {
    Dialog {
        title: "Remove serde from the shelf?".into(),
        body: "Its pages stay in the index; `serde` just stops showing in your shelf and on the rings.".into(),
        buttons: vec![
            DialogButton::new("Keep", |_, _| {}),
            DialogButton::new("Remove", |_, _| {}).primary().danger(),
        ],
        dismissible: true,
    }
}

fn open_dialog() -> Step {
    Box::new(|window, cx| dialog::open(remove_dialog(), window, cx))
}

fn dialog_still(window: &mut Window, cx: &mut App) -> AnyView {
    stage(false, vec![(1, open_dialog())], window, cx)
}

fn hints(window: &mut Window, cx: &mut App) -> AnyView {
    stage(true, vec![(1, Box::new(|window, cx| super::hint::enter(window, cx)))], window, cx)
}

fn hints_typed(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        true,
        vec![
            (1, Box::new(|window, cx| super::hint::enter(window, cx))),
            (
                60,
                Box::new(|window, cx| {
                    if let Ok(keystroke) = Keystroke::parse("s") {
                        super::hint::handle_key(&keystroke, window, cx);
                    }
                }),
            ),
        ],
        window,
        cx,
    )
}

fn menu_film(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        vec![
            (1, open_menu()),
            (150, press("down")),
            (300, press("down")),
            (500, press("enter")),
        ],
        window,
        cx,
    )
}

fn toast_film(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        vec![
            (
                1,
                Box::new(|window: &mut Window, cx: &mut App| {
                    let id = toast::show(Toast::new("Pinned SemanticLinkKind").undo(|_, _| {}), window, cx);
                    // The undo is pressed at 900 ms.
                    window
                        .spawn(cx, async move |cx| {
                            cx.background_executor().timer(Duration::from_millis(899)).await;
                            let _ = cx.update(|window, cx| toast::dismiss(id, window, cx));
                        })
                        .detach();
                }),
            ),
            (
                250,
                Box::new(|window: &mut Window, cx: &mut App| {
                    toast::show(Toast::new("Removed serde from the shelf").voice(Voice::Coral), window, cx);
                }),
            ),
        ],
        window,
        cx,
    )
}

fn dialog_film(window: &mut Window, cx: &mut App) -> AnyView {
    stage(
        false,
        vec![
            (1, open_dialog()),
            (150, Box::new(|window, cx| dialog::close(window, cx))),
            (260, open_dialog()),
        ],
        window,
        cx,
    )
}


/// A node whose position is a function of time (a camera flying over a
/// graph), with a tracked trigger on it.
struct Flight {
    start: Option<std::time::Instant>,
}

impl Render for Flight {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let now = crate::motion::now(cx);
        let start = *self.start.get_or_insert(now);
        let t = now.saturating_duration_since(start).as_secs_f32();
        // Drift right, then accelerate off the right edge after 0.6 s.
        let x = 120.0 + 260.0 * t + if t > 0.6 { 2400.0 * (t - 0.6) * (t - 0.6) } else { 0.0 };
        let y = 140.0 + 40.0 * (t * 3.0).sin();
        if x < 1400.0 {
            crate::motion::request_frame(window, cx);
        }
        let node = key("node");
        div()
            .relative()
            .size_full()
            .bg(palette.g1.hsla())
            .child(ground())
            .child(
                div().absolute().left(px(x)).top(px(y)).child(float::trigger(
                    node.clone(),
                    move |bounds| {
                        super::peek::request(
                            node.clone(),
                            bounds,
                            super::peek::Peek::Symbol(super::peek::SymbolPeek {
                                kind: Some(Kind::Enum),
                                name: "SemanticLinkKind".into(),
                                place: "enum in `present::relation`".into(),
                                path: "present::relation".into(),
                                sentence: Some("The closed vocabulary of how one symbol touches another.".into()),
                                uses: Some(14),
                                ..super::peek::SymbolPeek::default()
                            }),
                        )
                    },
                    gem(Kind::Enum).size(28.0),
                )),
            )
            .child(float::layer(window, cx))
    }
}

fn follow_film(window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|_| Flight { start: None });
    script(
        vec![
            (
                1,
                Box::new(|window: &mut Window, cx: &mut App| {
                    let at = anchor("node", window, cx);
                    let request = super::peek::request(
                        key("node"),
                        at,
                        super::peek::Peek::Symbol(super::peek::SymbolPeek {
                            kind: Some(Kind::Enum),
                            name: "SemanticLinkKind".into(),
                            place: "enum in `present::relation`".into(),
                            path: "present::relation".into(),
                            sentence: Some("The closed vocabulary of how one symbol touches another.".into()),
                            uses: Some(14),
                            ..super::peek::SymbolPeek::default()
                        }),
                    );
                    float::open(request, window, cx);
                    float::settle_now(window, cx);
                }) as Step,
            ),
            (2, Box::new(|window: &mut Window, cx: &mut App| float::settle_now(window, cx))),
        ],
        window,
        cx,
    );
    view.into()
}
