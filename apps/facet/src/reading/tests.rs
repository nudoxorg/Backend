//! Actual GPUI native trees and ActionRequest delivery, without a probe oracle.
use super::{ControlKind, Intent, ReadingRole, control, styled, text};
use gpui::{
    AppContext as _, Context, FocusHandle, HighlightStyle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _,
    StyledText, TestAppContext, VisualTestContext, Window, div, px,
};
use std::{cell::Cell, rc::Rc, sync::Arc};

type TestResult<T = ()> = Result<T, &'static str>;

struct Document {
    words: SharedString,
    role: ReadingRole,
    lines: Option<usize>,
    intent: Option<Intent>,
    covered: bool,
    owner: u32,
    focus: FocusHandle,
    calls: Rc<Cell<usize>>,
    current: Rc<Cell<bool>>,
}

impl Render for Document {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let owner = gpui::ElementId::Name(format!("reading-owner-{}", self.owner).into());
        let key =
            |part: &'static str| gpui::ElementId::NamedChild(Arc::new(owner.clone()), part.into());
        let intent = self.intent.unwrap_or(Intent::Reading(self.role));
        let highlighted = StyledText::new(self.words.clone())
            .with_highlights([(0..self.words.len(), HighlightStyle::default())]);
        let mut body = div()
            .w(px(220.))
            .min_w_0()
            .text_size(px(18.))
            .line_height(px(24.))
            .child(styled(key("words"), highlighted, intent));
        if let Some(lines) = self.lines {
            body = body.overflow_hidden().text_ellipsis().line_clamp(lines);
        }
        let body = if intent == Intent::NameOfExistingControl {
            let calls = self.calls.clone();
            let current = self.current.clone();
            crate::controls::button::native_button(
                control(
                    self.words.clone(),
                    ControlKind::Button,
                    body.id(key("control")),
                ),
                &self.focus,
                move |_, _| {
                    if current.get() {
                        calls.set(calls.get() + 1);
                    }
                },
            )
            .into_any_element()
        } else {
            body.into_any_element()
        };
        let body = if self.covered {
            gpui::inert(key("covered"), "The current surface owns input", body).into_any_element()
        } else {
            body
        };
        div().size_full().child(body)
    }
}

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::fonts::install(cx).expect("production font cuts install");
        cx.set_global(gpui::TextTrace);
    });
}

fn mount(
    cx: &mut TestAppContext,
    words: SharedString,
) -> (gpui::Entity<Document>, &mut VisualTestContext) {
    cx.add_window_view(|window, cx| {
        window.set_a11y_forced(true);
        Document {
            words,
            role: ReadingRole::Paragraph,
            lines: None,
            intent: None,
            covered: false,
            owner: 1,
            focus: cx.focus_handle(),
            calls: Rc::new(Cell::new(0)),
            current: Rc::new(Cell::new(true)),
        }
    })
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

fn named_nodes(
    cx: &mut VisualTestContext,
) -> Vec<(gpui::accesskit::NodeId, gpui::accesskit::Node)> {
    cx.update(|window, _| {
        window
            .a11y_tree()
            .expect("forced native tree")
            .nodes
            .iter()
            .filter(|(_, node)| node.label().is_some())
            .cloned()
            .collect()
    })
}

fn action(
    cx: &mut VisualTestContext,
    node: gpui::accesskit::NodeId,
    action: gpui::AccessibleAction,
    data: Option<gpui::accesskit::ActionData>,
) {
    cx.update(|window, cx| {
        window.simulate_a11y_action(
            gpui::accesskit::ActionRequest {
                action,
                target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: node,
                data,
            },
            cx,
        );
    });
}

#[gpui::test]
fn reading_roles_share_native_paint_and_have_no_editable_actions(cx: &mut TestAppContext) {
    init(cx);
    let words: SharedString = "café Ελληνικά 日本語 🧭 current words".into();
    let (document, cx) = mount(cx, words.clone());
    for role in [
        ReadingRole::Paragraph,
        ReadingRole::Heading,
        ReadingRole::Code,
        ReadingRole::Fact,
        ReadingRole::Status,
    ] {
        document.update(cx, |document, cx| {
            document.role = role;
            cx.notify();
        });
        draw(cx);
        let nodes = named_nodes(cx);
        let matching: Vec<_> = nodes
            .iter()
            .filter(|(_, node)| node.label() == Some(words.as_ref()))
            .collect();
        assert_eq!(matching.len(), 1, "one native node for the one text layout");
        let (id, node) = matching[0];
        assert_eq!(Some(node.role()), Intent::Reading(role).native_role());
        for action in [
            gpui::AccessibleAction::SetValue,
            gpui::AccessibleAction::Click,
            gpui::AccessibleAction::Focus,
        ] {
            assert!(!node.supports_action(action));
        }
        assert!(cx.update(|window, _| {
            window
                .painted_texts()
                .iter()
                .any(|run| run.text == words && run.alpha > 0.)
        }));
        action(
            cx,
            *id,
            gpui::AccessibleAction::SetValue,
            Some(gpui::accesskit::ActionData::Value("forged edit".into())),
        );
        draw(cx);
        assert_eq!(
            document.read_with(cx, |document, _| document.words.clone()),
            words
        );
        assert!(
            named_nodes(cx)
                .iter()
                .any(|(_, node)| node.label() == Some(words.as_ref()))
        );
    }
}

#[gpui::test]
fn native_reading_names_only_the_actual_clamped_text(cx: &mut TestAppContext) {
    let result = check_native_reading_names_only_the_actual_clamped_text(cx);
    assert!(result.is_ok(), "fixture failed: {result:?}");
}

fn check_native_reading_names_only_the_actual_clamped_text(cx: &mut TestAppContext) -> TestResult {
    init(cx);
    let words: SharedString =
        format!("{} HIDDEN_SOURCE_TAIL", "café visible words 🧭 ".repeat(24)).into();
    let (document, cx) = mount(cx, words);
    document.update(cx, |document, cx| {
        document.lines = Some(2);
        cx.notify();
    });
    draw(cx);
    let nodes = named_nodes(cx);
    let native: Vec<_> = nodes
        .iter()
        .filter(|(_, node)| node.role() == gpui::Role::Label)
        .collect();
    assert_eq!(native.len(), 1);
    let read = native[0].1.label().ok_or("current clamped reading label")?;
    assert!(!read.contains("HIDDEN_SOURCE_TAIL"));
    let painted = cx.update(|window, _| {
        window
            .painted_texts()
            .iter()
            .map(|run| run.text.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    });
    assert_eq!(
        read, painted,
        "native words come from the very layout that paints"
    );
    Ok(())
}

#[gpui::test]
fn existing_control_names_are_not_duplicated_and_covered_actions_stay_inert(
    cx: &mut TestAppContext,
) {
    let result = check_existing_control_names_are_not_duplicated_and_covered_actions_stay_inert(cx);
    assert!(result.is_ok(), "fixture failed: {result:?}");
}

fn check_existing_control_names_are_not_duplicated_and_covered_actions_stay_inert(
    cx: &mut TestAppContext,
) -> TestResult {
    init(cx);
    let words: SharedString = "Open the current package".into();
    let (document, cx) = mount(cx, words.clone());
    document.update(cx, |document, cx| {
        document.intent = Some(Intent::NameOfExistingControl);
        cx.notify();
    });
    draw(cx);
    let nodes = named_nodes(cx);
    let named: Vec<_> = nodes
        .iter()
        .filter(|(_, node)| node.label() == Some(words.as_ref()))
        .collect();
    assert_eq!(named.len(), 1, "the existing control owns its name");
    let (id, node) = named[0];
    assert_eq!(node.role(), gpui::Role::Button);
    assert!(node.supports_action(gpui::AccessibleAction::Click));
    assert!(!node.supports_action(gpui::AccessibleAction::SetValue));
    let calls = document.read_with(cx, |document, _| document.calls.clone());
    action(cx, *id, gpui::AccessibleAction::Click, None);
    assert_eq!(calls.get(), 1);
    // This is the component host's live admission model, not a real producer
    // attachment. A separate desktop gate must establish owner acceptance.
    document.read_with(cx, |document, _| document.current.set(false));
    action(cx, *id, gpui::AccessibleAction::Click, None);
    assert_eq!(
        calls.get(),
        1,
        "mounted callback still rechecks host admission"
    );
    document.update(cx, |document, cx| {
        document.owner = 2;
        document.current.set(true);
        document.covered = true;
        cx.notify();
    });
    draw(cx);
    let nodes = named_nodes(cx);
    let (covered_id, covered) = nodes
        .iter()
        .find(|(_, node)| node.label() == Some(words.as_ref()))
        .ok_or("covered current control")?;
    assert_ne!(
        *covered_id, *id,
        "the host's actual key owns native identity"
    );
    assert!(!covered.supports_action(gpui::AccessibleAction::Click));
    action(cx, *covered_id, gpui::AccessibleAction::Click, None);
    assert_eq!(
        calls.get(),
        1,
        "forged Click cannot revive the covered subtree"
    );
    document.update(cx, |document, cx| {
        document.intent = Some(Intent::Decoration);
        document.covered = false;
        cx.notify();
    });
    draw(cx);
    assert!(
        !named_nodes(cx)
            .iter()
            .any(|(_, node)| node.label() == Some(words.as_ref()))
    );
    assert!(cx.update(|window, _| window.painted_texts().iter().any(|run| run.text == words)));
    Ok(())
}

#[test]
fn decoration_and_existing_control_names_have_no_native_reading_identity() {
    use gpui::Element as _;
    for intent in [Intent::NameOfExistingControl, Intent::Decoration] {
        let leaf = text("existing-owner-key", "already painted", intent);
        assert!(leaf.id().is_none());
        assert!(leaf.a11y_role().is_none());
    }
}
