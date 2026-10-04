//! Real native pointer/focus/AX dispatch against a guard changed without redraw.
use super::*;
use gpui::{AppContext as _, Context, Render, TestAppContext, VisualTestContext, point};
use std::cell::Cell;

type TestResult<T = ()> = Result<T, &'static str>;

struct Fixture {
    current: Rc<Cell<bool>>,
    calls: Rc<Cell<usize>>,
    owned_focus: FocusHandle,
    reader_focus: FocusHandle,
}
impl Render for Fixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measure = Measure::new(px(480.), &cx.facet());
        let current = self.current.clone();
        let calls = self.calls.clone();
        div()
            .flex()
            .flex_col()
            .child(
                button("current-source-control", "Open source", &measure)
                    .focus_handle(self.owned_focus.clone())
                    .when_current(Rc::new(move |_| current.get()))
                    .on_click(move |_, _| calls.set(calls.get() + 1)),
            )
            .child(
                div()
                    .id("current-reader-focus")
                    .role(gpui::Role::Label)
                    .aria_label("Current Reader")
                    .track_focus(&self.reader_focus)
                    .w(px(220.))
                    .h(px(32.))
                    .child("Current Reader"),
            )
    }
}
fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}
fn click_ax(cx: &mut VisualTestContext, node: gpui::accesskit::NodeId) {
    cx.update(|window, cx| {
        window.simulate_a11y_action(
            gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click,
                target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: node,
                data: None,
            },
            cx,
        )
    });
}
#[gpui::test]
fn stale_live_guard_denies_pointer_focus_and_ax_before_redraw_but_current_owner_activates(
    cx: &mut TestAppContext,
) {
    let result = check_stale_live_guard_denies_pointer_focus_and_ax_before_redraw_but_current_owner_activates(cx);
    assert!(result.is_ok(), "fixture failed: {result:?}");
}

fn check_stale_live_guard_denies_pointer_focus_and_ax_before_redraw_but_current_owner_activates(
    cx: &mut TestAppContext,
) -> TestResult {
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = crate::fonts::install(cx);
        crate::set_facet(
            crate::Facet {
                reduced_motion: true,
                ..crate::Facet::default()
            },
            cx,
        );
    });
    let current = Rc::new(Cell::new(true));
    let calls = Rc::new(Cell::new(0));
    let (fixture, cx) = cx.add_window_view(|window, cx| {
        window.set_a11y_forced(true);
        Fixture {
            current: current.clone(),
            calls: calls.clone(),
            owned_focus: cx.focus_handle(),
            reader_focus: cx.focus_handle(),
        }
    });
    draw(cx);
    let (owned_focus, reader_focus) = fixture.read_with(cx, |fixture, _| {
        (fixture.owned_focus.clone(), fixture.reader_focus.clone())
    });
    cx.update(|window, cx| window.focus(&reader_focus, cx));
    draw(cx);
    let node = cx.update(|window, _| -> TestResult<_> {
        window
            .a11y_tree()
            .ok_or("forced native button tree")?
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == gpui::Role::Button && node.label() == Some("Open source")
            })
            .map(|(id, _)| *id)
            .ok_or("current Open source button")
    })?;
    let at = cx.update(|window, _| window.a11y_node_bounds(node))
        .ok_or("current native button bounds")?.center();
    // The committed visible control and listeners are still the current frame.
    // Only the producer predicate changes; admission cannot rely on rerender.
    current.set(false);
    cx.simulate_mouse_down(at, MouseButton::Left, gpui::Modifiers::none());
    assert!(
        cx.update(|window, _| reader_focus.is_focused(window)),
        "stale press cannot steal current Reader focus"
    );
    assert!(!cx.update(|window, _| owned_focus.is_focused(window)));
    cx.simulate_mouse_up(at, MouseButton::Left, gpui::Modifiers::none());
    click_ax(cx, node);
    assert_eq!(
        calls.get(),
        0,
        "stale pointer and actual AX cannot invoke the old producer"
    );
    assert!(
        cx.update(|window, _| reader_focus.is_focused(window)),
        "denied AX does not mutate focus"
    );
    current.set(true);
    cx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(calls.get(), 1);
    assert!(
        cx.update(|window, _| owned_focus.is_focused(window)),
        "fresh pointer retains native automatic focus"
    );
    draw(cx);
    click_ax(cx, node);
    assert_eq!(calls.get(), 2, "fresh native AX activates once");
    crate::test_input::native_press(cx, "enter");
    crate::test_input::native_press(cx, "space");
    assert_eq!(
        calls.get(),
        4,
        "fresh native key releases activate once each"
    );
    // The owner may change between a valid down and its release.
    cx.simulate_mouse_down(at, MouseButton::Left, gpui::Modifiers::none());
    current.set(false);
    cx.simulate_mouse_up(at, MouseButton::Left, gpui::Modifiers::none());
    click_ax(cx, node);
    crate::test_input::native_press(cx, "enter");
    crate::test_input::native_press(cx, "space");
    assert_eq!(
        calls.get(),
        4,
        "release and AX/key callbacks recheck current ownership"
    );
    Ok(())
}

#[gpui::test]
fn shared_capture_guard_precedes_focus_on_an_unstyled_native_target(cx: &mut TestAppContext) {
    struct Target {
        current: Rc<Cell<bool>>,
        target: FocusHandle,
        reader: FocusHandle,
    }
    impl Render for Target {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let current = self.current.clone();
            let target = native_button(
                div().id("raw-native-target").w(px(160.)).h(px(40.)),
                &self.target,
                |_, _| {},
            );
            div()
                .child(capture_activation_admission(
                    target,
                    Rc::new(move |_| current.get()),
                ))
                .child(div().track_focus(&self.reader).w(px(160.)).h(px(40.)))
        }
    }
    let current = Rc::new(Cell::new(true));
    let (target, cx) = cx.add_window_view(|_, cx| Target {
        current: current.clone(),
        target: cx.focus_handle(),
        reader: cx.focus_handle(),
    });
    draw(cx);
    let reader = target.read_with(cx, |target, _| target.reader.clone());
    cx.update(|window, cx| window.focus(&reader, cx));
    current.set(false);
    cx.simulate_mouse_down(
        point(px(20.), px(20.)),
        MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert!(cx.update(|window, _| reader.is_focused(window)));
}
