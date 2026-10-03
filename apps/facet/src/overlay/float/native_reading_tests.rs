//! Mounted float admission uses the existing current/hidden/leaving decisions.
//! TestSupport trees and delivery do not establish native platform acceptance.
use super::{FloatKind, FloatRequest};
use crate::reading::{self, ControlKind, Intent, ReadingRole};
use gpui::{
    Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, TestAppContext, VisualTestContext, Window, div, point, px, size,
};
use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

struct Board;

impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(super::layer(window, cx))
    }
}

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::fonts::install(cx).expect("production font cuts install");
        crate::set_facet(crate::Facet::default(), cx);
        cx.set_global(gpui::TextTrace);
    });
}

fn request(
    key: &'static str,
    label: &'static str,
    anchor: gpui::Bounds<gpui::Pixels>,
    calls: Rc<Cell<usize>>,
) -> FloatRequest {
    FloatRequest::new(key, anchor, FloatKind::Peek, move |_, _, _| {
        let owner: ElementId = key.into();
        let key = |part: &'static str| ElementId::NamedChild(Arc::new(owner.clone()), part.into());
        let calls = calls.clone();
        let name: SharedString = format!("Open {label}").into();
        let control = reading::control(
            name.clone(),
            ControlKind::Button,
            div().id(key("control")).h(px(28.)).child(reading::text(
                key("control-name"),
                name,
                Intent::NameOfExistingControl,
            )),
        )
        .on_click(move |_, _, _| calls.set(calls.get() + 1));
        div()
            .w(px(320.))
            .px(px(12.))
            .py(px(12.))
            .flex()
            .flex_col()
            .text_size(px(18.))
            .line_height(px(24.))
            .child(reading::text(
                key("words"),
                label,
                Intent::Reading(ReadingRole::Heading),
            ))
            .child(control)
            .into_any_element()
    })
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

fn advance(cx: &mut VisualTestContext, millis: u64) {
    cx.executor().advance_clock(Duration::from_millis(millis));
    draw(cx);
}

fn nodes(cx: &mut VisualTestContext) -> Vec<(gpui::accesskit::NodeId, gpui::accesskit::Node)> {
    cx.update(|window, _| {
        window
            .a11y_tree()
            .expect("forced native tree")
            .nodes
            .clone()
    })
}

fn click(cx: &mut VisualTestContext, id: gpui::accesskit::NodeId) {
    cx.update(|window, cx| {
        window.simulate_a11y_action(
            gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click,
                target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: id,
                data: None,
            },
            cx,
        )
    });
}

fn button(cx: &mut VisualTestContext, label: &str) -> gpui::accesskit::NodeId {
    nodes(cx)
        .iter()
        .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some(label))
        .map(|(id, _)| *id)
        .expect("the actual current native control")
}

fn has(cx: &mut VisualTestContext, label: &str) -> bool {
    nodes(cx)
        .iter()
        .any(|(_, node)| node.label() == Some(label))
}

#[gpui::test]
fn retiring_float_keeps_paint_but_releases_its_native_reading_subtree(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Board
    });
    cx.simulate_resize(size(px(900.), px(700.)));
    let calls = Rc::new(Cell::new(0));
    let anchor = gpui::Bounds::new(point(px(100.), px(100.)), size(px(30.), px(20.)));
    cx.update(|window, cx| {
        super::open(
            request(
                "retiring-owner",
                "Earlier current reading",
                anchor,
                calls.clone(),
            ),
            window,
            cx,
        )
    });
    advance(cx, 400);
    let old = button(cx, "Open Earlier current reading");
    click(cx, old);
    assert_eq!(calls.get(), 1);
    cx.update(|window, cx| {
        super::close_all(window, cx);
    });
    advance(cx, 40);
    assert!(
        cx.update(|window, _| window
            .painted_texts()
            .iter()
            .any(|run| run.text.as_ref() == "Earlier current reading" && run.alpha > 0.)),
        "the exiting visual copy still paints"
    );
    assert!(!has(cx, "Earlier current reading"));
    assert!(!has(cx, "Open Earlier current reading"));
    click(cx, old);
    assert_eq!(
        calls.get(),
        1,
        "the retained ActionRequest cannot activate its old copy"
    );
    assert!(
        cx.update(|window, cx| super::state(window, cx)
            .borrow()
            .model
            .cards()
            .any(|card| !card.is_open())),
        "the reading subtree was suppressed before visual retirement"
    );
}

#[gpui::test]
fn a_warm_swap_names_only_current_content_while_the_prior_copy_wipes_out(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Board
    });
    cx.simulate_resize(size(px(900.), px(700.)));
    let old_calls = Rc::new(Cell::new(0));
    let new_calls = Rc::new(Cell::new(0));
    let anchor = gpui::Bounds::new(point(px(100.), px(100.)), size(px(30.), px(20.)));
    cx.update(|window, cx| {
        super::open(
            request("prior-owner", "Prior reading", anchor, old_calls.clone()),
            window,
            cx,
        )
    });
    advance(cx, 400);
    let old = button(cx, "Open Prior reading");
    let next = gpui::Bounds::new(point(px(700.), px(100.)), size(px(30.), px(20.)));
    cx.update(|window, cx| {
        super::open(
            request("current-owner", "Current reading", next, new_calls.clone()),
            window,
            cx,
        )
    });
    advance(cx, 80);
    assert!(
        cx.update(|window, cx| super::state(window, cx)
            .borrow()
            .model
            .top()
            .unwrap()
            .previous
            .is_some()),
        "the old content is genuinely retained during the existing wipe"
    );
    assert!(has(cx, "Current reading"));
    assert!(!has(cx, "Prior reading"));
    assert!(!has(cx, "Open Prior reading"));
    click(cx, old);
    assert_eq!(old_calls.get(), 0);
    advance(cx, 160);
    let current = button(cx, "Open Current reading");
    click(cx, current);
    assert_eq!(
        new_calls.get(),
        1,
        "the existing live overlay stays actionable"
    );
}

#[gpui::test]
fn a_parent_hidden_behind_the_current_sheet_has_no_native_reading_or_control(
    cx: &mut TestAppContext,
) {
    init(cx);
    let (_, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Board
    });
    cx.simulate_resize(size(px(900.), px(700.)));
    let calls = Rc::new(Cell::new(0));
    let anchor = gpui::Bounds::new(point(px(100.), px(100.)), size(px(30.), px(20.)));
    cx.update(|window, cx| {
        super::open(
            request("parent-owner", "Parent reading", anchor, calls.clone()),
            window,
            cx,
        )
    });
    advance(cx, 400);
    let parent = button(cx, "Open Parent reading");
    let child_anchor = cx.update(|window, cx| {
        let painted = super::state(window, cx)
            .borrow()
            .model
            .top()
            .unwrap()
            .painted
            .unwrap();
        gpui::Bounds::new(painted.center(), size(px(1.), px(1.)))
    });
    cx.update(|window, cx| {
        super::open(
            request(
                "child-owner",
                "Current child reading",
                child_anchor,
                calls.clone(),
            ),
            window,
            cx,
        )
    });
    advance(cx, 400);
    cx.simulate_resize(size(px(400.), px(700.)));
    draw(cx);
    assert!(has(cx, "Current child reading"));
    assert!(!has(cx, "Parent reading"));
    assert!(!has(cx, "Open Parent reading"));
    click(cx, parent);
    assert_eq!(calls.get(), 0);
    let current = button(cx, "Open Current child reading");
    click(cx, current);
    assert_eq!(calls.get(), 1);
}
