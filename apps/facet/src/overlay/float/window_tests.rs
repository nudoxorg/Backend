//! Actual GPUI events and executor timers, including the virtual trigger
//! reporting pattern used by the graph canvas.

use super::{FloatKind, FloatRequest};
use crate::theme::{Facet, set_facet};
use gpui::{
    Bounds, Context, DispatchPhase, InteractiveElement, IntoElement, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, ScrollWheelEvent,
    StatefulInteractiveElement, Styled, TestAppContext, VisualTestContext, Window, canvas, div,
    point, px, size,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

const KEY: &str = "virtual-symbol";

fn anchor() -> Bounds<Pixels> {
    Bounds::new(point(px(100.0), px(100.0)), size(px(30.0), px(20.0)))
}

struct Board {
    page_presses: Rc<Cell<usize>>,
    page_scrolls: Rc<Cell<usize>>,
    actions: Rc<Cell<usize>>,
}

impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let presses = self.page_presses.clone();
        let scrolls = self.page_scrolls.clone();
        let actions = self.actions.clone();
        div()
            .size_full()
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        // Like the graph, only a currently picked symbol reports. On
                        // transit to the card the old trigger emits no mouse report.
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase != DispatchPhase::Capture
                                || !anchor().contains(&event.position)
                            {
                                return;
                            }
                            let actions = actions.clone();
                            super::report(
                                &KEY.into(),
                                anchor(),
                                true,
                                move || {
                                    FloatRequest::new(
                                        KEY,
                                        anchor(),
                                        FloatKind::Peek,
                                        move |_, _, _| {
                                            let actions = actions.clone();
                                            div()
                                                .id("native-card-action")
                                                .w(px(220.0))
                                                .h(px(80.0))
                                                .on_click(move |_, _, _| {
                                                    actions.set(actions.get() + 1)
                                                })
                                                .into_any_element()
                                        },
                                    )
                                },
                                window,
                                cx,
                            );
                        });
                        window.on_mouse_event({
                            let presses = presses.clone();
                            move |_: &MouseDownEvent, phase, _, _| {
                                if phase == DispatchPhase::Bubble {
                                    presses.set(presses.get() + 1);
                                }
                            }
                        });
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, _| {
                            if phase == DispatchPhase::Bubble {
                                presses.set(presses.get() + 1);
                            }
                        });
                        window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, _| {
                            if phase == DispatchPhase::Bubble {
                                scrolls.set(scrolls.get() + 1);
                            }
                        });
                    },
                )
                .size_full(),
            )
            .child(super::layer(window, cx))
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
}

fn advance(cx: &mut VisualTestContext, millis: u64) {
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
}

#[gpui::test]
fn timer_opens_a_jittered_virtual_trigger_and_pointer_transit_keeps_the_native_card(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        set_facet(
            Facet {
                reduced_motion: true,
                ..Facet::default()
            },
            cx,
        );
        crate::probe::enable(cx);
    });
    let presses = Rc::new(Cell::new(0));
    let scrolls = Rc::new(Cell::new(0));
    let actions = Rc::new(Cell::new(0));
    let (view, cx) = cx.add_window_view(|_, _| Board {
        page_presses: presses.clone(),
        page_scrolls: scrolls.clone(),
        actions: actions.clone(),
    });
    cx.simulate_resize(size(px(900.0), px(700.0)));
    draw(cx);
    let notifications = Rc::new(Cell::new(0));
    let observe = notifications.clone();
    let _subscription =
        cx.update(|_, cx| cx.observe(&view, move |_, _| observe.set(observe.get() + 1)));

    cx.simulate_mouse_move(anchor().center(), None, gpui::Modifiers::none());
    advance(cx, 200);
    cx.update(|window, cx| {
        for offset in [1.0, -1.0] {
            window.dispatch_event(
                gpui::PlatformInput::MouseMove(MouseMoveEvent {
                    position: anchor().center() + point(px(offset), px(0.0)),
                    pressed_button: None,
                    modifiers: gpui::Modifiers::none(),
                }),
                cx,
            );
        }
    });
    advance(cx, 149);
    assert!(!cx.update(|window, cx| super::is_open(&KEY.into(), window, cx)));
    let before_timer = notifications.get();
    advance(cx, 1);
    assert!(
        cx.update(|window, cx| super::is_open(&KEY.into(), window, cx)),
        "cold intent must open on its original deadline without another event or draw"
    );
    assert!(
        notifications.get() > before_timer,
        "the real timer must notify the layer's host"
    );
    draw(cx);
    let bounds = cx.update(|window, cx| {
        super::state(window, cx)
            .borrow()
            .model
            .top()
            .expect("real card")
            .painted
            .expect("painted card")
    });
    let ledger = cx.update(|_, cx| crate::probe::take(cx));
    assert!(
        ledger.stacks.iter().any(|stack| stack
            .entries
            .iter()
            .any(|entry| entry.key == KEY && entry.kind == "peek" && entry.bounds.is_some())),
        "the probe must describe the real painted card"
    );

    cx.update(|window, cx| {
        for offset in [0.0, 1.0] {
            window.dispatch_event(
                gpui::PlatformInput::MouseMove(MouseMoveEvent {
                    position: bounds.center() + point(px(offset), px(0.0)),
                    pressed_button: None,
                    modifiers: gpui::Modifiers::none(),
                }),
                cx,
            );
        }
    });
    advance(cx, 200);
    assert!(
        cx.update(|window, cx| super::is_open(&KEY.into(), window, cx)),
        "missing virtual trigger reports mean leave, not physical unmount of a card being hovered"
    );
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    assert_eq!(
        actions.get(),
        1,
        "the native child action must receive the click"
    );
    assert_eq!(
        presses.get(),
        0,
        "neither press nor release may reach window-wide canvas listeners beneath the card"
    );
    cx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(16.0))),
        modifiers: gpui::Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    assert_eq!(
        scrolls.get(),
        0,
        "a scroll over the card must not move the underlying map"
    );
    cx.simulate_mouse_move(point(px(800.0), px(600.0)), None, gpui::Modifiers::none());
    advance(cx, 160);
    assert!(
        !cx.update(|window, cx| super::is_open(&KEY.into(), window, cx)),
        "ordinary leave still closes after the existing grace"
    );
    cx.simulate_mouse_move(anchor().center(), None, gpui::Modifiers::none());
    assert!(
        cx.update(|window, cx| super::is_open(&KEY.into(), window, cx)),
        "the warm trigger should reopen immediately"
    );
    cx.simulate_event(gpui::MouseExitEvent {
        position: point(px(-1.0), px(110.0)),
        pressed_button: None,
        modifiers: gpui::Modifiers::none(),
    });
    advance(cx, 160);
    assert!(
        !cx.update(|window, cx| super::is_open(&KEY.into(), window, cx)),
        "leaving the native window must release trigger hover too"
    );
}

#[gpui::test]
fn the_executor_timer_publishes_the_original_exit_segment_terminal_before_culling(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        set_facet(Facet::default(), cx);
        crate::probe::enable(cx);
    });
    let (_, cx) = cx.add_window_view(|_, _| Board {
        page_presses: Rc::new(Cell::new(0)),
        page_scrolls: Rc::new(Cell::new(0)),
        actions: Rc::new(Cell::new(0)),
    });
    cx.simulate_resize(size(px(900.0), px(700.0)));
    draw(cx);
    cx.update(|window, cx| {
        super::open(
            FloatRequest::new("exit-symbol", anchor(), FloatKind::Peek, |_, _, _| {
                div().w(px(220.0)).h(px(80.0)).into_any_element()
            }),
            window,
            cx,
        )
    });
    advance(cx, 200);
    draw(cx);
    cx.update(|window, cx| {
        super::close_all(window, cx);
    });
    advance(cx, 149);
    draw(cx);
    let before = cx
        .update(|_, cx| crate::probe::take(cx))
        .tracks
        .into_iter()
        .rev()
        .find(|track| track.key == "float-1.presence" && track.target == 0.0)
        .expect("actual leaving presence sample");
    assert!(before.live && before.value > 0.0);
    advance(cx, 1);
    // No draw, refresh, or manually published sample: the exit timer owns
    // retirement and must terminate the segment observed by the harness.
    let terminal = cx
        .update(|_, cx| crate::probe::take(cx))
        .tracks
        .into_iter()
        .find(|track| track.key == before.key)
        .expect("executor retirement must publish a terminal sample");
    assert_eq!(
        (terminal.value, terminal.target, terminal.live),
        (0.0, 0.0, false)
    );
    assert_eq!(
        (terminal.started_ms, terminal.budget_ms),
        (before.started_ms, before.budget_ms)
    );
    assert!(terminal.at_ms >= terminal.started_ms + terminal.budget_ms);
}
