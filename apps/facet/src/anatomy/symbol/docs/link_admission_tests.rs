//! One painted UTF-8 body, actual native clipping/focus, live owner admission.
use super::{Link, rich_text};
use gpui::{
    AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, TestAppContext,
    VisualTestContext, Window, div, point, px,
};
use std::{cell::Cell, rc::Rc};

struct Paragraph {
    words: gpui::SharedString,
    current: Rc<Cell<bool>>,
    calls: Rc<Cell<usize>>,
    reader: FocusHandle,
    clip: Option<(f32, f32)>,
}
impl Render for Paragraph {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let calls = self.calls.clone();
        let current = self.current.clone();
        let body = rich_text(
            "owned-unicode-body",
            self.words.clone(),
            vec![],
            vec![Link {
                range: 0..self.words.len(),
                destination: "https://docs.rs/current/état".into(),
                activate: Rc::new(move |_, _| calls.set(calls.get() + 1)),
                admission: Some(Rc::new(move |_| current.get())),
            }],
        );
        let body = div()
            .w(px(260.))
            .text_size(px(24.))
            .line_height(px(32.))
            .child(body);
        let body = if let Some((offset, height)) = self.clip {
            div()
                .w(px(260.))
                .h(px(height))
                .overflow_hidden()
                .child(body.mt(px(-offset)))
                .into_any_element()
        } else {
            body.into_any_element()
        };
        div().flex().flex_col().child(body).child(
            div()
                .id("current-reading-focus")
                .role(gpui::Role::Label)
                .aria_label("Current Reader")
                .track_focus(&self.reader)
                .w(px(260.))
                .h(px(24.))
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
fn link(cx: &mut VisualTestContext, words: &str) -> gpui::accesskit::NodeId {
    cx.update(|window, _| {
        window
            .a11y_tree()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.role() == gpui::Role::Link && node.label() == Some(words))
            .map(|(id, _)| *id)
            .expect("the actual visible Unicode link")
    })
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
fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let _ = crate::fonts::install(cx);
        cx.set_global(gpui::TextTrace);
    });
}
fn words() -> gpui::SharedString {
    "café Ελληνικά 日本語 🧭 current observed tick documentation wraps across native rows and ends with 終わり_é".into()
}

#[gpui::test]
fn retained_link_rechecks_owner_before_focus_glyph_and_native_activation(cx: &mut TestAppContext) {
    init(cx);
    let current = Rc::new(Cell::new(true));
    let calls = Rc::new(Cell::new(0));
    let words = words();
    let (paragraph, cx) = cx.add_window_view(|window, cx| {
        window.set_a11y_forced(true);
        Paragraph {
            words: words.clone(),
            current: current.clone(),
            calls: calls.clone(),
            reader: cx.focus_handle(),
            clip: None,
        }
    });
    draw(cx);
    let node = link(cx, &words);
    let at = cx.update(|window, _| window.a11y_node_bounds(node).unwrap().center());
    let reader = paragraph.read_with(cx, |paragraph, _| paragraph.reader.clone());
    cx.update(|window, cx| window.focus(&reader, cx));
    draw(cx);
    // Keep the mounted body and its committed handlers. Only its host's
    // admission changes, as when the Reader's page/root/owner is replaced.
    current.set(false);
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert!(
        cx.update(|window, _| reader.is_focused(window)),
        "denial precedes native link auto focus"
    );
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    // The next wrapped row has no first-fragment native overlay. This must
    // exercise InteractiveText's actual glyph-range admission separately.
    cx.simulate_click(point(px(10.), px(44.)), gpui::Modifiers::none());
    click_ax(cx, node);
    assert_eq!(
        calls.get(),
        0,
        "stale glyph and AX callbacks cannot invoke the producer"
    );
    assert!(
        cx.update(|window, _| reader.is_focused(window)),
        "denied AX does not refocus"
    );
    current.set(true);
    cx.simulate_click(point(px(10.), px(44.)), gpui::Modifiers::none());
    assert_eq!(
        calls.get(),
        1,
        "the same painted glyph callback remains live for the current owner"
    );
    cx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(
        calls.get(),
        2,
        "first-fragment native overlay never duplicates glyph activation"
    );
    assert!(
        !cx.update(|window, _| reader.is_focused(window)),
        "fresh native pointer admission preserves automatic focus"
    );
    click_ax(cx, node);
    assert_eq!(calls.get(), 3);
    for key in ["enter", "space"] {
        crate::test_input::native_press(cx, key);
    }
    assert_eq!(calls.get(), 5);
    current.set(false);
    click_ax(cx, node);
    for key in ["enter", "space"] {
        crate::test_input::native_press(cx, key);
    }
    assert_eq!(
        calls.get(),
        5,
        "key and AX callback admission stays live after a focus handoff"
    );
}

#[gpui::test]
fn partially_visible_first_and_last_unicode_rows_keep_masked_native_link_bounds(
    cx: &mut TestAppContext,
) {
    init(cx);
    let calls = Rc::new(Cell::new(0));
    let words = words();
    let (paragraph, cx) = cx.add_window_view(|window, cx| {
        window.set_a11y_forced(true);
        Paragraph {
            words: words.clone(),
            current: Rc::new(Cell::new(true)),
            calls: calls.clone(),
            reader: cx.focus_handle(),
            clip: None,
        }
    });
    draw(cx);
    // Read the actual native painting height, rather than predict wrapping.
    let full_height = cx.update(|window, _| {
        f32::from(
            window
                .painted_texts()
                .iter()
                .find(|run| run.text == words)
                .unwrap()
                .bounds
                .size
                .height,
        )
    });
    assert!(
        full_height > 64.,
        "the real Unicode body wraps across several rows"
    );
    for (n, offset) in [12., full_height - 10.].into_iter().enumerate() {
        paragraph.update(cx, |paragraph, cx| {
            paragraph.clip = Some((offset, 8.));
            cx.notify();
        });
        draw(cx);
        let node = link(cx, &words);
        cx.update(|window, _| {
            let bounds = window.a11y_node_bounds(node).unwrap();
            assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
            assert!(
                bounds.top() >= px(0.) && bounds.bottom() <= px(8.),
                "semantic bounds intersect the actual clip, without a hidden full-row box"
            );
            assert!(
                window.painted_texts().iter().any(|run| run.text == words
                    && run.alpha > 0.
                    && run.bounds.size.height > px(0.)
                    && run.bounds.size.height <= px(8.)),
                "the same UTF-8 body is actually painted inside that mask"
            );
        });
        click_ax(cx, node);
        assert_eq!(
            calls.get(),
            n + 1,
            "partially visible native link remains reachable exactly once"
        );
    }
}
