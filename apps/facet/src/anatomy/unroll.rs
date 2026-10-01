//! A disclosure reserves only its visible height and uncovers its content
//! in place. Retargeting reverses from the painted extent; idle is silent.

use crate::motion::presence::{Act, Entry, Extent, Presence};
use crate::motion::{Keys, Pose};
use crate::tokens::motion::{DROP, GLIDE};
use gpui::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, Styled,
    Window, div,
};
use std::time::Duration;

#[derive(IntoElement)]
pub struct Unroll {
    id: ElementId,
    open: bool,
    presence: Presence,
    body: AnyElement,
}

#[must_use]
pub fn unroll(
    id: impl Into<ElementId>,
    open: bool,
    presence: Presence,
    body: impl IntoElement,
) -> Unroll {
    Unroll {
        id: id.into(),
        open,
        presence,
        body: body.into_any_element(),
    }
}

fn act(opening: bool) -> Act {
    let duration = Duration::from_millis(if opening { 180 } else { 120 });
    let (from, to) = if opening {
        (Extent::NONE, Extent::FULL)
    } else {
        (Extent::FULL, Extent::NONE)
    };
    Act {
        duration,
        pose: Keys::owned(duration, vec![(0.0, Pose::REST), (1.0, Pose::REST)], GLIDE),
        room: Keys::owned(
            duration,
            vec![(0.0, from), (1.0, to)],
            if opening { GLIDE } else { DROP },
        ),
    }
}

impl RenderOnce for Unroll {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let entries = self
            .open
            .then(|| Entry::new(self.id).enter(act(true)).exit(act(false)));
        let items = self.presence.sync_entries(entries, window, cx);
        let mut root = div().overflow_hidden();
        if let Some(item) = items.first() {
            // The parent clips to the slot's reserved height. Slot itself
            // reserves room but deliberately does not install a mask.
            let child = div().child(self.body);
            let child = if item.is_leaving() {
                child
                    .capture_any_mouse_down(|_, _, cx| cx.stop_propagation())
                    .capture_any_mouse_up(|_, _, cx| cx.stop_propagation())
                    .capture_key_down(|_, _, cx| cx.stop_propagation())
                    .capture_action::<super::Open>(|_, _, cx| cx.stop_propagation())
            } else {
                child
            };
            root = root.child(item.slot(child));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disclosure_motion_changes_only_room_never_text_pose() {
        let opening = act(true);
        assert_eq!(opening.at(0.0), (Pose::REST, Extent::NONE));
        assert_eq!(opening.at(1.0), (Pose::REST, Extent::FULL));
        assert_eq!(act(false).at(1.0), (Pose::REST, Extent::NONE));
        assert_eq!(opening.at(0.5).0, Pose::REST);
    }
    struct Clickable {
        open: bool,
        presence: Presence,
        focus: gpui::FocusHandle,
        opens: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl gpui::Render for Clickable {
        fn render(
            &mut self,
            _window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            use gpui::StatefulInteractiveElement;
            let opens = self.opens.clone();
            let keys = self.opens.clone();
            let child = div()
                .id("disclosed-action")
                .track_focus(&self.focus)
                .w(gpui::px(200.0))
                .h(gpui::px(40.0))
                .on_click(move |_, _, _| opens.set(opens.get() + 1))
                .on_key_down(move |event, _, _| {
                    if event.keystroke.key == "enter" {
                        keys.set(keys.get() + 1);
                    }
                });
            let _ = cx;
            div().child(unroll(
                "disclosure",
                self.open,
                self.presence.clone(),
                child,
            ))
        }
    }

    fn frame(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    #[gpui::test]
    fn collapsing_content_cannot_activate_by_mouse_or_key_and_rests(cx: &mut gpui::TestAppContext) {
        let opens = std::rc::Rc::new(std::cell::Cell::new(0));
        let (view, cx) = cx.add_window_view(|_, cx| Clickable {
            open: true,
            presence: Presence::new("regression-disclosure"),
            focus: cx.focus_handle(),
            opens: opens.clone(),
        });
        cx.update(|_, cx| {
            crate::motion::reset_epoch(cx);
        });
        frame(cx);
        cx.simulate_click(
            gpui::point(gpui::px(6.0), gpui::px(8.0)),
            gpui::Modifiers::none(),
        );
        assert_eq!(
            opens.get(),
            1,
            "the active content has a real click handler"
        );
        cx.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        view.update(cx, |view, cx| {
            view.open = false;
            cx.notify();
        });
        frame(cx);
        assert!(
            !view.read_with(cx, |view, _| view.presence.is_empty()),
            "the leaving content is still painted"
        );
        cx.simulate_click(
            gpui::point(gpui::px(6.0), gpui::px(8.0)),
            gpui::Modifiers::none(),
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(opens.get(), 1, "outgoing visual content cannot navigate");
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        frame(cx);
        let requested = cx.update(|_, cx| crate::motion::frames_requested(cx));
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(40));
            cx.run_until_parked();
            frame(cx);
        }
        assert_eq!(
            cx.update(|_, cx| crate::motion::frames_requested(cx)),
            requested,
            "idle disclosure schedules zero frames"
        );
    }
}
