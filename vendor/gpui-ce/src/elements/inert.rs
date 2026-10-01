//! A retained subtree whose contents are unavailable for user interaction.

use crate::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, Window,
};

/// Keep an element subtree laid out and painted while preventing interaction with it.
///
/// The subtree remains visible and its accessible contents remain readable, but GPUI does not
/// register its pointer, keyboard, focus, text-input, cursor, or accessibility actions. The
/// wrapper is exposed to assistive technology as a disabled pane with the supplied description.
///
/// Inertness also applies to retained `ViewElement` caches. A cached view inside this subtree is
/// rendered through the inert registration policy instead of replaying listeners and tab stops
/// captured while it was interactive.
pub fn inert(
    id: impl Into<ElementId>,
    accessible_description: impl Into<SharedString>,
    child: impl IntoElement,
) -> Inert {
    Inert {
        id: id.into(),
        accessible_description: accessible_description.into(),
        child: child.into_any_element(),
    }
}

/// The element returned by [`inert`].
pub struct Inert {
    id: ElementId,
    accessible_description: SharedString,
    child: AnyElement,
}

impl IntoElement for Inert {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Inert {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let layout_id = window.with_inert_subtree(|window| self.child.request_layout(window, cx));
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        // A capture from the previous interactive frame must not keep an inert child hovered or
        // receive its drag stream. This is a window-local interaction and is safely reset before
        // the committed inert frame replaces the old hit-test tree.
        window.release_pointer();
        window.with_inert_subtree(|window| self.child.prepaint_at(bounds.origin, window, cx));
        // The wrapper node itself was created before the child scope began. Mark it after the
        // child has had its chance to append synthetic descendants or an active-descendant edge.
        if let Some(id) = id {
            window.mark_a11y_node_inert(id.accesskit_node_id());
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_inert_subtree(|window| self.child.paint(window, cx));
    }

    fn a11y_role(&self) -> Option<accesskit::Role> {
        Some(accesskit::Role::Pane)
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        node.set_description(self.accessible_description.to_string());
        node.set_disabled(true);
        node.clear_actions();
        node.clear_child_actions();
        node.clear_custom_actions();
        node.clear_active_descendant();
    }
}

#[cfg(test)]
mod tests {
    use super::inert;
    use crate::{
        self as gpui, AnyElement, App, AppContext as _, Bounds, Context, Element, ElementId,
        FocusHandle, InputHandler, InteractiveElement, IntoElement, LayoutId, ParentElement,
        Pixels, Point, Render, StatefulInteractiveElement, Style, StyleRefinement, TestAppContext,
        UTF16Selection, Window, accesskit, actions, div, point, px, size,
    };
    use std::{cell::Cell, ops::Range, rc::Rc};

    actions!(test_only, [InertTestAction]);

    #[derive(Default)]
    struct Events {
        clicks: Cell<usize>,
        keys: Cell<usize>,
        actions: Cell<usize>,
        a11y_actions: Cell<usize>,
        ime_insertions: Cell<usize>,
        animation_frames: Cell<usize>,
        nested_clicks: Cell<usize>,
        sibling_clicks: Cell<usize>,
    }

    struct InertInteractiveView {
        focus: FocusHandle,
        nested_focus: FocusHandle,
        events: Rc<Events>,
    }

    impl Render for InertInteractiveView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.events.clone();
            let keys = self.events.clone();
            let actions = self.events.clone();
            let a11y_actions = self.events.clone();
            let synthetic_id = accesskit::NodeId(0x1e47);
            let button = div()
                .id("inert-test-button")
                .size(size(px(100.), px(32.)))
                .track_focus(&self.focus)
                .tab_stop(true)
                .role(accesskit::Role::Button)
                .aria_label("cached control")
                .aria_active_descendant()
                .on_mouse_down(move |_, _, _| clicks.clicks.set(clicks.clicks.get() + 1))
                .on_key_down(move |_, _, _| keys.keys.set(keys.keys.get() + 1))
                .on_action(move |_: &InertTestAction, _, _| {
                    actions.actions.set(actions.actions.get() + 1)
                })
                .on_a11y_action(accesskit::Action::Click, move |_, _, _| {
                    a11y_actions
                        .a11y_actions
                        .set(a11y_actions.a11y_actions.get() + 1)
                })
                .a11y_synthetic_children(move |builder| {
                    builder.parent_node().set_active_descendant(synthetic_id);
                    let mut node = accesskit::Node::new(accesskit::Role::TextRun);
                    node.set_label("retained text");
                    node.add_action(accesskit::Action::Click);
                    assert!(builder.push_child(synthetic_id, node));
                });

            let nested_clicks = self.events.clone();
            let nested = inert(
                "nested-inert-control",
                "Nested content is unavailable",
                div()
                    .id("nested-inert-button")
                    .size(size(px(100.), px(20.)))
                    .track_focus(&self.nested_focus)
                    .tab_stop(true)
                    .role(accesskit::Role::Button)
                    .aria_label("nested control")
                    .on_mouse_down(move |_, _, _| {
                        nested_clicks
                            .nested_clicks
                            .set(nested_clicks.nested_clicks.get() + 1)
                    }),
            );

            div()
                .size_full()
                .flex()
                .flex_col()
                .child(button)
                .child(nested)
                .child(InputProbe {
                    focus: self.focus.clone(),
                    events: self.events.clone(),
                })
                .child(FrameProbe {
                    events: self.events.clone(),
                })
        }
    }

    struct RootView {
        child: crate::Entity<InertInteractiveView>,
        inert: Rc<Cell<bool>>,
        sibling_focus: FocusHandle,
        events: Rc<Events>,
    }

    impl Render for RootView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let cached = self
                .child
                .clone()
                .cached(StyleRefinement::default().size(size(px(100.), px(60.))));
            let child: AnyElement = if self.inert.get() {
                inert(
                    "reader-loading",
                    "Previous content remains available",
                    cached,
                )
                .into_any_element()
            } else {
                cached.into_any_element()
            };

            let sibling_clicks = self.events.clone();
            div()
                .size(size(px(240.), px(80.)))
                .flex()
                .flex_row()
                .child(child)
                .child(
                    div()
                        .id("active-sibling")
                        .size(size(px(100.), px(60.)))
                        .track_focus(&self.sibling_focus)
                        .tab_stop(true)
                        .role(accesskit::Role::Button)
                        .aria_label("active sibling")
                        .on_mouse_down(move |_, _, _| {
                            sibling_clicks
                                .sibling_clicks
                                .set(sibling_clicks.sibling_clicks.get() + 1)
                        }),
                )
        }
    }

    struct InputProbe {
        focus: FocusHandle,
        events: Rc<Events>,
    }

    impl IntoElement for InputProbe {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl Element for InputProbe {
        type RequestLayoutState = ();
        type PrepaintState = ();

        fn id(&self) -> Option<ElementId> {
            None
        }

        fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
            None
        }

        fn request_layout(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            (
                window.request_layout(Style::default().size(size(px(0.), px(0.))), [], cx),
                (),
            )
        }

        fn prepaint(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            _: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            _: &mut Window,
            _: &mut App,
        ) -> Self::PrepaintState {
        }

        fn paint(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            _: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            _: &mut Self::PrepaintState,
            window: &mut Window,
            cx: &mut App,
        ) {
            window.handle_input(
                &self.focus,
                TestInputHandler {
                    insertions: self.events.clone(),
                },
                cx,
            );
        }
    }

    struct FrameProbe {
        events: Rc<Events>,
    }

    impl IntoElement for FrameProbe {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl Element for FrameProbe {
        type RequestLayoutState = ();
        type PrepaintState = ();

        fn id(&self) -> Option<ElementId> {
            None
        }

        fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
            None
        }

        fn request_layout(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            (
                window.request_layout(Style::default().size(size(px(0.), px(0.))), [], cx),
                (),
            )
        }

        fn prepaint(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            _: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            _: &mut Window,
            _: &mut App,
        ) -> Self::PrepaintState {
        }

        fn paint(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            _: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            _: &mut Self::PrepaintState,
            window: &mut Window,
            _: &mut App,
        ) {
            let events = self.events.clone();
            window.on_next_frame(move |_, _| {
                events
                    .animation_frames
                    .set(events.animation_frames.get() + 1)
            });
        }
    }

    struct TestInputHandler {
        insertions: Rc<Events>,
    }

    impl InputHandler for TestInputHandler {
        fn selected_text_range(
            &mut self,
            _: bool,
            _: &mut Window,
            _: &mut App,
        ) -> Option<UTF16Selection> {
            None
        }

        fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
            None
        }

        fn text_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Option<Range<usize>>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<String> {
            None
        }

        fn replace_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            _: &str,
            _: &mut Window,
            _: &mut App,
        ) {
            self.insertions
                .ime_insertions
                .set(self.insertions.ime_insertions.get() + 1);
        }

        fn replace_and_mark_text_in_range(
            &mut self,
            _: Option<Range<usize>>,
            _: &str,
            _: Option<Range<usize>>,
            _: &mut Window,
            _: &mut App,
        ) {
            self.insertions
                .ime_insertions
                .set(self.insertions.ime_insertions.get() + 1);
        }

        fn unmark_text(&mut self, _: &mut Window, _: &mut App) {}

        fn bounds_for_range(
            &mut self,
            _: Range<usize>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<Bounds<Pixels>> {
            None
        }

        fn character_index_for_point(
            &mut self,
            _: Point<Pixels>,
            _: &mut Window,
            _: &mut App,
        ) -> Option<usize> {
            None
        }
    }

    #[gpui::test]
    fn inert_cached_subtree_blocks_input_and_keeps_siblings_live(cx: &mut TestAppContext) {
        let events = Rc::new(Events::default());
        let inert_state = Rc::new(Cell::new(false));
        let focus_handle = Rc::new(std::cell::RefCell::new(None));
        let nested_focus_handle = Rc::new(std::cell::RefCell::new(None));
        let sibling_focus_handle = Rc::new(std::cell::RefCell::new(None));

        let (_root, cx) = cx.add_window_view({
            let events = events.clone();
            let inert_state = inert_state.clone();
            let focus_handle = focus_handle.clone();
            let nested_focus_handle = nested_focus_handle.clone();
            let sibling_focus_handle = sibling_focus_handle.clone();
            move |_, cx| {
                let focus = cx.focus_handle();
                let nested_focus = cx.focus_handle();
                let sibling_focus = cx.focus_handle();
                *focus_handle.borrow_mut() = Some(focus.clone());
                *nested_focus_handle.borrow_mut() = Some(nested_focus.clone());
                *sibling_focus_handle.borrow_mut() = Some(sibling_focus.clone());
                let child = cx.new(|_| InertInteractiveView {
                    focus,
                    nested_focus,
                    events: events.clone(),
                });
                RootView {
                    child,
                    inert: inert_state,
                    sibling_focus,
                    events,
                }
            }
        });
        let focus = focus_handle.borrow().clone().unwrap();
        let nested_focus = nested_focus_handle.borrow().clone().unwrap();
        let sibling_focus = sibling_focus_handle.borrow().clone().unwrap();

        cx.update(|window, cx| {
            window.set_a11y_forced(true);
            window.draw(cx).clear(cx);
            window.focus(&focus, cx);
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(focus.is_focused(window));
            assert!(window.simulate_next_frame(cx) > 0);
        });

        cx.simulate_click(point(px(10.), px(10.)), Default::default());
        cx.simulate_keystrokes("a");
        cx.simulate_input("x");
        cx.dispatch_action(InertTestAction);
        cx.update(|window, cx| {
            let button_id = window
                .a11y_tree()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("cached control"))
                .map(|(id, _)| *id)
                .unwrap();
            window.handle_a11y_action(
                accesskit::ActionRequest {
                    action: accesskit::Action::Click,
                    target_tree: accesskit::TreeId::ROOT,
                    target_node: button_id,
                    data: None,
                },
                cx,
            );
        });

        assert_eq!(events.clicks.get(), 1);
        assert_eq!(events.keys.get(), 1);
        assert_eq!(events.actions.get(), 1);
        assert_eq!(events.a11y_actions.get(), 1);
        assert_eq!(events.ime_insertions.get(), 1);
        assert_eq!(events.animation_frames.get(), 1);

        cx.simulate_click(point(px(10.), px(40.)), Default::default());
        assert_eq!(events.nested_clicks.get(), 0);
        cx.update(|window, cx| {
            window.focus(&nested_focus, cx);
            assert!(!nested_focus.is_focused(window));
            window.focus(&focus, cx);
            assert!(focus.is_focused(window));
        });

        inert_state.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(!focus.is_focused(window));
            assert!(!nested_focus.is_focused(window));
            window.focus(&focus, cx);
            window.focus(&nested_focus, cx);
            assert!(!focus.is_focused(window));
            assert!(!nested_focus.is_focused(window));

            let tree = window.a11y_tree().unwrap();
            let pane = tree
                .nodes
                .iter()
                .find(|(_, node)| node.description() == Some("Previous content remains available"))
                .map(|(_, node)| node)
                .unwrap();
            assert_eq!(pane.role(), accesskit::Role::Pane);
            assert!(pane.is_disabled());
            assert!(!pane.supports_action(accesskit::Action::Click));

            let control = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("cached control"))
                .map(|(_, node)| node)
                .unwrap();
            assert!(control.is_disabled());
            assert!(!control.supports_action(accesskit::Action::Click));
            assert!(!control.supports_action(accesskit::Action::Focus));
            assert_eq!(control.active_descendant(), None);
            assert_eq!(
                pane.active_descendant(),
                None,
                "the disabled pane does not name an interactive descendant"
            );

            let synthetic = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("retained text"))
                .map(|(_, node)| node)
                .unwrap();
            assert!(synthetic.is_disabled());
            assert!(!synthetic.supports_action(accesskit::Action::Click));

            assert_eq!(window.simulate_next_frame(cx), 0);
        });

        cx.simulate_click(point(px(10.), px(10.)), Default::default());
        cx.simulate_keystrokes("a");
        cx.simulate_input("y");
        cx.dispatch_action(InertTestAction);
        cx.update(|window, cx| {
            let button_id = window
                .a11y_tree()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("cached control"))
                .map(|(id, _)| *id)
                .unwrap();
            window.handle_a11y_action(
                accesskit::ActionRequest {
                    action: accesskit::Action::Click,
                    target_tree: accesskit::TreeId::ROOT,
                    target_node: button_id,
                    data: None,
                },
                cx,
            );
        });

        assert_eq!(events.clicks.get(), 1);
        assert_eq!(events.keys.get(), 1);
        assert_eq!(events.actions.get(), 1);
        assert_eq!(events.a11y_actions.get(), 1);
        assert_eq!(events.ime_insertions.get(), 1);
        assert_eq!(events.animation_frames.get(), 1);
        assert_eq!(events.nested_clicks.get(), 0);

        cx.update(|window, cx| {
            window.focus(&sibling_focus, cx);
            assert!(sibling_focus.is_focused(window));
            window.focus_prev(cx);
            assert!(sibling_focus.is_focused(window));
            assert!(!focus.is_focused(window));
        });
        cx.simulate_click(point(px(150.), px(20.)), Default::default());
        assert_eq!(events.sibling_clicks.get(), 1);

        inert_state.set(false);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            window.focus(&focus, cx);
            window.draw(cx).clear(cx);
            assert!(focus.is_focused(window));
        });
        cx.simulate_click(point(px(10.), px(10.)), Default::default());
        cx.simulate_keystrokes("a");
        assert_eq!(events.clicks.get(), 2);
        assert_eq!(events.keys.get(), 2);
    }

    #[gpui::test]
    fn inert_scope_is_restored_after_unwind(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, _| crate::EmptyView);
        cx.update_window(window.into(), |_, window, _| {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                window.with_inert_subtree(|window| {
                    assert!(window.is_inert_subtree());
                    panic!("simulated renderer panic");
                });
            }));

            assert!(panic.is_err());
            assert!(
                !window.is_inert_subtree(),
                "an unwound inert scope must not affect later siblings"
            );
        })
        .unwrap();
    }
}
