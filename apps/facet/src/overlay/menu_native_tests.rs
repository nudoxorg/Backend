//! Native popup semantics and exact retained-owner admission.
use super::*;
use gpui::{Context, Render, TestAppContext, VisualTestContext, point, size};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Board;
impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(float::layer(window, cx))
    }
}
fn tree(cx: &mut VisualTestContext) -> TestResult<serde_json::Value> {
    cx.update(|window, cx| -> TestResult<_> {
        window.refresh();
        window.draw(cx).clear(cx);
        Ok(serde_json::from_str(
            &window
                .debug_a11y_tree_json()
                .ok_or("forced native menu tree")?,
        )?)
    })
}
fn request(key: &ElementId, menu: Menu) -> (FloatRequest, Rc<Owner>) {
    let owner = Rc::new(Owner::default());
    let anchor = Bounds::new(point(px(20.), px(20.)), size(px(120.), px(24.)));
    let request = FloatRequest::new(
        key.clone(),
        anchor,
        FloatKind::Menu,
        content(key.clone(), menu, owner.clone()),
    );
    assert!(owner.content.set(Rc::downgrade(&request.content)).is_ok());
    (request, owner)
}
#[gpui::test]
fn native_menu_retained_owner_cannot_close_focus_or_mutate_same_key_replacement(
    cx: &mut TestAppContext,
) {
    let result = check_native_menu_retained_owner_cannot_close_focus_or_mutate_same_key_replacement(cx);
    assert!(result.is_ok(), "fixture failed: {result:?}");
}

fn check_native_menu_retained_owner_cannot_close_focus_or_mutate_same_key_replacement(
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
    let (_, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Board
    });
    cx.simulate_resize(size(px(900.), px(700.)));
    tree(cx)?;
    let key: ElementId = "package-menu".into();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let count = calls.clone();
    let menu = Menu::new(
        vec![
            MenuItem::new("all 2 packages"),
            MenuItem::new("café_🧭"),
            MenuItem::new("disabled").disabled(),
        ],
        move |index, _, _| count.borrow_mut().push(index),
    );
    let (old_request, old_owner) = request(&key, menu.clone());
    cx.update(|window, cx| float::open(old_request.clone(), window, cx));
    let native = tree(cx)?;
    let nodes = native["nodes"].as_object().ok_or("native menu node map")?;
    assert!(nodes.values().any(|node| node["aria"]["role"] == "Menu"));
    for words in ["all 2 packages", "café_🧭", "disabled"] {
        assert!(
            nodes
                .values()
                .any(|node| node["aria"]["role"] == "MenuItem" && node["aria"]["label"] == words)
        );
    }
    let focus = cx.update(|window, cx| old_owner.focus(&key, window, cx))
        .ok_or("open prior menu focus")?;
    assert!(cx.update(|window, _| focus.is_focused(window)));
    cx.update(|window, cx| float::close(&key, window, cx));
    let (new_request, new_owner) = request(&key, menu.clone());
    cx.update(|window, cx| float::open(new_request.clone(), window, cx));
    tree(cx)?;
    let stale_state = Rc::new(RefCell::new(State {
        active: Some(0),
        ..State::default()
    }));
    let down = Keystroke::parse("down")?;
    cx.update(|window, cx| -> TestResult {
        let current = new_owner
            .focus(&key, window, cx)
            .ok_or("replacement menu is open")?;
        choose(&old_owner, &menu, &key, 1, window, cx);
        assert!(calls.borrow().is_empty(), "denial precedes action");
        assert!(
            new_owner.focus(&key, window, cx).is_some(),
            "denial precedes close"
        );
        assert!(current.is_focused(window), "denial precedes focus restore");
        assert!(!handle(
            &old_owner,
            &stale_state,
            &menu,
            &key,
            &[false, false, true],
            &down,
            window,
            cx
        ));
        assert_eq!(
            stale_state.borrow().active,
            Some(0),
            "denial precedes selection mutation"
        );
        Ok(())
    })?;
    cx.simulate_keystrokes("down enter");
    assert_eq!(
        *calls.borrow(),
        vec![1],
        "current native menu chooses the actual package once"
    );
    tree(cx)?;
    assert!(cx.update(|window, cx| new_owner.focus(&key, window, cx).is_none()));
    let (ax_request, _) = request(&key, menu);
    cx.update(|window, cx| float::open(ax_request, window, cx));
    tree(cx)?;
    cx.update(|window, cx| -> TestResult {
        let node = window
            .a11y_tree()
            .ok_or("forced native menu action tree")?
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == gpui::Role::MenuItem && node.label() == Some("all 2 packages")
            })
            .map(|(id, _)| *id)
            .ok_or("current all-packages MenuItem")?;
        window.simulate_a11y_action(
            gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click,
                target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: node,
                data: None,
            },
            cx,
        );
        Ok(())
    })?;
    assert_eq!(
        *calls.borrow(),
        vec![1, 0],
        "native AX chooses one actual row once"
    );
    Ok(())
}
