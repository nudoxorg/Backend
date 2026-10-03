//! Real GPUI input over the shared FACET button path, with a competing host
//! shortcut. No direct invocation of a control callback substitutes for keys.
#![allow(clippy::expect_used)]
use super::{button, icon_button, switch};
use crate::{
    Measure,
    icons::Icon,
    theme::{ActiveFacet, Facet, set_facet},
};
use gpui::{
    AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, KeyBinding,
    KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ParentElement, Render, StatefulInteractiveElement, Styled, TestAppContext,
    VisualTestContext, Window, actions, div, point, px,
};
use std::{cell::Cell, rc::Rc};

actions!(facet_activation_fixture, [BackgroundActivate]);
#[derive(Clone, Copy)]
enum Kind {
    Button,
    Icon,
    Toggle,
    Chrome,
}
struct Fixture {
    kind: Kind,
    disabled: bool,
    busy: bool,
    covered: bool,
    retired: bool,
    clicks: Rc<Cell<usize>>,
    background: Rc<Cell<usize>>,
    spare: FocusHandle,
}
impl Render for Fixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measure = Measure::new(window.viewport_size().width, &cx.facet());
        let clicks = Rc::clone(&self.clicks);
        let action = move |_: &mut Window, _: &mut gpui::App| clicks.set(clicks.get() + 1);
        let control = match self.kind {
            Kind::Button => button("control", "Advance", &measure)
                .disabled(self.disabled)
                .busy(self.busy)
                .on_click(action)
                .into_any_element(),
            Kind::Icon => icon_button("control", Icon::SideL, "Toggle shelf", &measure)
                .disabled(self.disabled)
                .on_click(action)
                .into_any_element(),
            Kind::Toggle => switch("control", false, &measure)
                .label("Enable")
                .disabled(self.disabled)
                .on_toggle(action)
                .into_any_element(),
            Kind::Chrome => {
                let focus = window
                    .use_keyed_state("chrome-focus", cx, |_, cx| cx.focus_handle().tab_stop(true));
                super::button::native_button(
                    div().id("control").role(gpui::Role::Button).size(px(40.0)),
                    focus.read(cx),
                    action,
                )
                .into_any_element()
            }
        };
        let control = if self.retired { div().into_any_element() } else { control };
        let control = if self.covered {
            gpui::inert("covered-control", "A modal owns input", control).into_any_element()
        } else {
            control
        };
        let raw_parent = Rc::clone(&self.background);
        let background = Rc::clone(&self.background);
        div()
            .size_full()
            .key_context("ActivationHost")
            .on_action(move |_: &BackgroundActivate, _, _| background.set(background.get() + 1))
            .on_key_down(move |event: &KeyDownEvent, _, _| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    raw_parent.set(raw_parent.get() + 1);
                }
            })
            .child(div().absolute().left(px(40.0)).top(px(40.0)).child(control))
    }
}
fn fixture(cx: &mut TestAppContext, kind: Kind) -> (Entity<Fixture>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        set_facet(
            Facet {
                reduced_motion: true,
                ..Facet::default()
            },
            cx,
        );
        cx.bind_keys([
            KeyBinding::new(
                "enter",
                BackgroundActivate,
                Some("ActivationHost && !NativeControl"),
            ),
            KeyBinding::new(
                "space",
                BackgroundActivate,
                Some("ActivationHost && !NativeControl"),
            ),
        ]);
    });
    let (view, cx) = cx.add_window_view(move |_, cx| Fixture {
        kind,
        disabled: false,
        busy: false,
        covered: false,
        retired: false,
        clicks: Rc::new(Cell::new(0)),
        background: Rc::new(Cell::new(0)),
        spare: cx.focus_handle(),
    });
    draw(cx);
    (view, cx)
}
fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}
fn down(cx: &mut VisualTestContext, key: &str, held: bool) {
    cx.simulate_event(KeyDownEvent {
        keystroke: Keystroke::parse(key).expect("key"),
        is_held: held,
        prefer_character_input: false,
    });
}
fn up(cx: &mut VisualTestContext, key: &str) {
    cx.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse(key).expect("key"),
    });
}
#[gpui::test]
fn pointer_enter_and_space_activate_each_shared_control_exactly_once(cx: &mut TestAppContext) {
    for kind in [Kind::Button, Kind::Icon, Kind::Toggle, Kind::Chrome] {
        let (view, cx) = fixture(cx, kind);
        cx.simulate_click(point(px(50.0), px(50.0)), Modifiers::none());
        assert_eq!(view.read_with(cx, |view, _| view.clicks.get()), 1);
        cx.update(|window, cx| window.focus_next(cx));
        draw(cx);
        for key in ["enter", "space"] {
            down(cx, key, false);
            down(cx, key, true);
            assert_eq!(
                view.read_with(cx, |view, _| view.clicks.get()),
                if key == "enter" { 1 } else { 2 },
                "activation waits for a clean native release"
            );
            up(cx, key);
            draw(cx);
        }
        assert_eq!(view.read_with(cx, |view, _| view.clicks.get()), 3);
        assert_eq!(view.read_with(cx, |view, _| view.background.get()), 0);
    }
}
#[gpui::test]
fn interrupted_focus_and_disabled_or_busy_owner_cannot_activate_on_release(
    cx: &mut TestAppContext,
) {
    for key in ["enter", "space"] {
        let (view, cx) = fixture(cx, Kind::Button);
        cx.update(|window, cx| window.focus_next(cx));
        draw(cx);
        let before = cx
            .update(|window, cx| window.focused(cx))
            .expect("control focus");
        down(cx, key, false);
        let spare = view.read_with(cx, |view, _| view.spare.clone());
        cx.update(|window, cx| {
            spare.focus(window, cx);
            before.focus(window, cx);
        });
        draw(cx);
        up(cx, key);
        assert_eq!(
            view.read_with(cx, |view, _| view.clicks.get()),
            0,
            "focus ABA invalidates the native press"
        );
        for busy in [false, true] {
            down(cx, key, false);
            view.update(cx, |view, cx| {
                view.busy = busy;
                view.disabled = !busy;
                cx.notify();
            });
            draw(cx);
            up(cx, key);
            cx.simulate_click(point(px(50.0), px(50.0)), Modifiers::none());
            assert_eq!(view.read_with(cx, |view, _| view.clicks.get()), 0);
            view.update(cx, |view, cx| {
                view.busy = false;
                view.disabled = false;
                cx.notify();
            });
            draw(cx);
            cx.update(|window, cx| before.focus(window, cx));
            draw(cx);
        }
    }
}

#[gpui::test]
fn a_covered_or_retired_shared_button_cannot_finish_an_old_native_press(
    cx: &mut TestAppContext,
) {
    let (view, cx) = fixture(cx, Kind::Button);
    cx.update(|window, cx| window.focus_next(cx));
    draw(cx);
    down(cx, "enter", false);
    view.update(cx, |view, cx| {
        view.covered = true;
        cx.notify();
    });
    draw(cx);
    up(cx, "enter");
    assert_eq!(view.read_with(cx, |view, _| view.clicks.get()), 0,
        "a modal's inert underlay cannot complete the button press");

    view.update(cx, |view, cx| {
        view.covered = false;
        cx.notify();
    });
    draw(cx);
    cx.update(|window, cx| window.focus_next(cx));
    draw(cx);
    down(cx, "space", false);
    view.update(cx, |view, cx| {
        view.retired = true;
        cx.notify();
    });
    draw(cx);
    up(cx, "space");
    view.update(cx, |view, cx| {
        view.retired = false;
        cx.notify();
    });
    draw(cx);
    assert_eq!(view.read_with(cx, |view, _| view.clicks.get()), 0,
        "a remounted button cannot inherit a retired press");
    assert_eq!(view.read_with(cx, |view, _| view.background.get()), 0);
}
