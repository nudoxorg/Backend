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
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let layout_id = if let Some(id) = id {
            let wrapped_child_id = self.child.element_id();
            window.with_inert_subtree_boundary(id, wrapped_child_id, |window| {
                self.child.request_layout(window, cx)
            })
        } else {
            window.with_inert_subtree(|window| self.child.request_layout(window, cx))
        };
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if let Some(id) = id {
            let wrapped_child_id = self.child.element_id();
            window.with_inert_subtree_boundary(id, wrapped_child_id, |window| {
                // This boundary shares the child's LayoutId; rebasing to
                // its absolute bounds would apply its layout origin twice.
                self.child.prepaint(window, cx)
            });
        } else {
            window.with_inert_subtree(|window| self.child.prepaint(window, cx));
        }
        // The wrapper node itself was created before the child scope began. Mark it after the
        // child has had its chance to append synthetic descendants or an active-descendant edge.
        if let Some(id) = id {
            window.mark_a11y_node_inert(id.accesskit_node_id());
        }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(id) = id {
            let wrapped_child_id = self.child.element_id();
            window.with_inert_subtree_boundary(id, wrapped_child_id, |window| {
                self.child.paint(window, cx)
            });
        } else {
            window.with_inert_subtree(|window| self.child.paint(window, cx));
        }
    }

    fn a11y_role(&self) -> Option<accesskit::Role> {
        Some(accesskit::Role::Pane)
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        node.set_description(self.accessible_description.to_string());
        node.set_disabled();
        node.clear_actions();
        node.clear_child_actions();
        node.clear_custom_actions();
        node.clear_active_descendant();
    }
}

#[cfg(test)]
mod tests {
    use super::inert;
    use crate::util::FluentBuilder;
    use crate::{
        self as gpui, AnyElement, App, AppContext as _, Bounds, Context, Element, ElementId,
        Entity, FocusHandle, HitboxBehavior, HitboxId, InputHandler, InteractiveElement,
        IntoElement, LayoutId, MouseButton, MouseMoveEvent, ParentElement, Pixels, PlatformWindow,
        Point, Render, StatefulInteractiveElement, Style, StyleRefinement, TestAppContext,
        UTF16Selection, VisualContext as _, Window, accesskit, deferred, div, point, px, size,
        styled::Styled,
    };
    use std::{
        cell::{Cell, RefCell},
        ops::Range,
        rc::Rc,
    };

    actions!(test_only, [InertTestAction]);

    #[derive(Default)]
    struct Events {
        clicks: Cell<usize>,
        keys: Cell<usize>,
        actions: Cell<usize>,
        a11y_actions: Cell<usize>,
        ime_insertions: Cell<usize>,
        input_handoff: RefCell<Option<FocusHandle>>,
        animation_frames: Cell<usize>,
        callback_order: RefCell<Vec<&'static str>>,
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
                .w(px(100.))
                .h(px(32.))
                .role(accesskit::Role::Button)
                .aria_label("cached control")
                .aria_active_descendant()
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    clicks.clicks.set(clicks.clicks.get() + 1)
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
            let control = div()
                .id("inert-test-control")
                .w(px(100.))
                .h(px(32.))
                .track_focus(&self.focus)
                .tab_stop(true)
                .role(accesskit::Role::Group)
                .child(button)
                .on_key_down(move |_, _, _| keys.keys.set(keys.keys.get() + 1))
                .on_action(move |_: &InertTestAction, _, _| {
                    actions.actions.set(actions.actions.get() + 1)
                });

            let nested_clicks = self.events.clone();
            let nested = inert(
                "nested-inert-control",
                "Nested content is unavailable",
                div()
                    .id("nested-inert-button")
                    .w(px(100.))
                    .h(px(20.))
                    .track_focus(&self.nested_focus)
                    .tab_stop(true)
                    .role(accesskit::Role::Button)
                    .aria_label("nested control")
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        nested_clicks
                            .nested_clicks
                            .set(nested_clicks.nested_clicks.get() + 1)
                    }),
            );

            div()
                .size_full()
                .flex()
                .flex_col()
                .child(control)
                .child(nested)
                .child(InputProbe {
                    focus: self.focus.clone(),
                    events: self.events.clone(),
                })
                .child(FrameProbe {
                    events: self.events.clone(),
                    once: None,
                    invalidate_on_frame: false,
                    callback_label: None,
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
                .cached(StyleRefinement::default().w(px(100.)).h(px(60.)));
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
                .w(px(240.))
                .h(px(80.))
                .flex()
                .flex_row()
                .child(child)
                .child(
                    div()
                        .id("active-sibling")
                        .debug_selector(|| "active-sibling".to_string())
                        .w(px(100.))
                        .h(px(60.))
                        .track_focus(&self.sibling_focus)
                        .tab_stop(true)
                        .role(accesskit::Role::Button)
                        .aria_label("active sibling")
                        .on_mouse_down(MouseButton::Left, move |_, _, _| {
                            sibling_clicks
                                .sibling_clicks
                                .set(sibling_clicks.sibling_clicks.get() + 1)
                        }),
                )
        }
    }

    struct PointerCaptureTargetView {
        hitbox: Rc<Cell<Option<HitboxId>>>,
    }

    impl Render for PointerCaptureTargetView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            PointerCaptureProbe {
                id: None,
                hitbox: self.hitbox.clone(),
                moves: None,
            }
        }
    }

    struct PointerCaptureRoot {
        child: Entity<PointerCaptureTargetView>,
        sibling: Entity<PointerCaptureTargetView>,
        inert: Rc<Cell<bool>>,
        replace_child: Rc<Cell<bool>>,
        child_hitbox: Rc<Cell<Option<HitboxId>>>,
    }

    impl Render for PointerCaptureRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let child: AnyElement = if self.replace_child.get() {
                PointerCaptureProbe {
                    id: Some("replacement-capture-owner"),
                    hitbox: self.child_hitbox.clone(),
                    moves: None,
                }
                .into_any_element()
            } else {
                let cached = self
                    .child
                    .clone()
                    .cached(StyleRefinement::default().w(px(100.)).h(px(60.)));
                if self.inert.get() {
                    inert(
                        "captured-child-inert-boundary",
                        "Captured child is unavailable",
                        cached,
                    )
                    .into_any_element()
                } else {
                    cached.into_any_element()
                }
            };

            let sibling = self
                .sibling
                .clone()
                .cached(StyleRefinement::default().w(px(100.)).h(px(60.)));

            div()
                .w(px(220.))
                .h(px(70.))
                .flex()
                .flex_row()
                .child(child)
                .child(sibling)
        }
    }

    struct KeyedCaptureRoot {
        inert: Rc<Cell<bool>>,
        reordered: Rc<Cell<bool>>,
        target_hitbox: Rc<Cell<Option<HitboxId>>>,
        sibling_hitbox: Rc<Cell<Option<HitboxId>>>,
        target_moves: Rc<Cell<usize>>,
    }

    struct FreshIdlessCaptureRoot {
        inert: Rc<Cell<bool>>,
        child_hitbox: Rc<Cell<Option<HitboxId>>>,
        sibling_hitbox: Rc<Cell<Option<HitboxId>>>,
    }

    impl Render for FreshIdlessCaptureRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let child = PointerCaptureProbe {
                id: None,
                hitbox: self.child_hitbox.clone(),
                moves: None,
            };
            let child: AnyElement = if self.inert.get() {
                inert("fresh-idless-boundary", "Retained idless child", child).into_any_element()
            } else {
                child.into_any_element()
            };
            div()
                .w(px(220.))
                .h(px(70.))
                .flex()
                .flex_row()
                .child(child)
                .child(PointerCaptureProbe {
                    id: None,
                    hitbox: self.sibling_hitbox.clone(),
                    moves: None,
                })
        }
    }

    impl Render for KeyedCaptureRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let target = div()
                .id("keyed-capture-target")
                .w(px(100.))
                .h(px(60.))
                .child(PointerCaptureProbe {
                    id: None,
                    hitbox: self.target_hitbox.clone(),
                    moves: Some(self.target_moves.clone()),
                });
            let target: AnyElement = if self.inert.get() {
                inert(
                    "keyed-capture-inert-wrapper",
                    "Keyed capture target is retained",
                    target,
                )
                .into_any_element()
            } else {
                target.into_any_element()
            };
            let sibling = div()
                .id("keyed-capture-sibling")
                .w(px(100.))
                .h(px(60.))
                .child(PointerCaptureProbe {
                    id: None,
                    hitbox: self.sibling_hitbox.clone(),
                    moves: None,
                })
                .into_any_element();
            let children = if self.reordered.get() {
                [sibling, target]
            } else {
                [target, sibling]
            };
            div()
                .id("keyed-capture-row")
                .w(px(220.))
                .h(px(70.))
                .flex()
                .flex_row()
                .children(children)
        }
    }

    struct PointerCaptureProbe {
        id: Option<&'static str>,
        hitbox: Rc<Cell<Option<HitboxId>>>,
        moves: Option<Rc<Cell<usize>>>,
    }

    impl IntoElement for PointerCaptureProbe {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl Element for PointerCaptureProbe {
        type RequestLayoutState = ();
        type PrepaintState = ();

        fn id(&self) -> Option<ElementId> {
            self.id.map(Into::into)
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
                window.request_layout(
                    Style {
                        size: size(px(100.).into(), px(60.).into()),
                        ..Style::default()
                    },
                    [],
                    cx,
                ),
                (),
            )
        }

        fn prepaint(
            &mut self,
            _: Option<&crate::GlobalElementId>,
            _: Option<&crate::InspectorElementId>,
            bounds: Bounds<Pixels>,
            _: &mut Self::RequestLayoutState,
            window: &mut Window,
            _: &mut App,
        ) -> Self::PrepaintState {
            let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
            self.hitbox.set(Some(hitbox.id));
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
            if let (Some(moves), Some(hitbox_id)) = (self.moves.clone(), self.hitbox.get()) {
                window.on_mouse_event::<MouseMoveEvent>(move |_, _, window, _| {
                    if window.captured_hitbox() == Some(hitbox_id) {
                        moves.set(moves.get() + 1);
                    }
                });
            }
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
                window.request_layout(
                    Style {
                        size: size(px(0.).into(), px(0.).into()),
                        ..Style::default()
                    },
                    [],
                    cx,
                ),
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

    struct PaintedInputView {
        focus: FocusHandle,
        events: Rc<Events>,
        renders: Rc<Cell<usize>>,
    }

    impl Render for PaintedInputView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            div().track_focus(&self.focus).child(InputProbe {
                focus: self.focus.clone(),
                events: self.events.clone(),
            })
        }
    }

    struct PaintedInputRoot {
        child: Entity<PaintedInputView>,
        mounted: Rc<Cell<bool>>,
        covered: Rc<Cell<bool>>,
        sibling: FocusHandle,
    }

    impl Render for PaintedInputRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let child = self
                .child
                .clone()
                .cached(StyleRefinement::default().w(px(100.)).h(px(30.)));
            let child: AnyElement = if !self.mounted.get() {
                div().into_any_element()
            } else if self.covered.get() {
                inert("covered-input", "The input is covered", child).into_any_element()
            } else {
                child.into_any_element()
            };
            div().child(child).child(div().track_focus(&self.sibling))
        }
    }

    /// A handler belongs to the last painted input owner, even while a native
    /// callback temporarily extracts the platform object. A new paint retires
    /// that ownership when the focused input is covered or leaves the tree.
    #[gpui::test]
    fn painted_input_ownership_survives_extraction_and_follows_mount(cx: &mut TestAppContext) {
        let events = Rc::new(Events::default());
        let renders = Rc::new(Cell::new(0));
        let mounted = Rc::new(Cell::new(true));
        let covered = Rc::new(Cell::new(false));
        let input_focus = Rc::new(RefCell::new(None));
        let sibling_focus = Rc::new(RefCell::new(None));

        let (_root, cx) = cx.add_window_view({
            let events = events.clone();
            let renders = renders.clone();
            let mounted = mounted.clone();
            let covered = covered.clone();
            let input_focus = input_focus.clone();
            let sibling_focus = sibling_focus.clone();
            move |_, cx| {
                let input = cx.focus_handle();
                let sibling = cx.focus_handle();
                *input_focus.borrow_mut() = Some(input.clone());
                *sibling_focus.borrow_mut() = Some(sibling.clone());
                PaintedInputRoot {
                    child: cx.new(|_| PaintedInputView {
                        focus: input,
                        events,
                        renders,
                    }),
                    mounted,
                    covered,
                    sibling,
                }
            }
        });
        let input_focus = input_focus.borrow().clone().unwrap();
        let sibling_focus = sibling_focus.borrow().clone().unwrap();

        cx.update(|window, cx| {
            assert!(
                !window.has_input_handler(),
                "the initial unfocused paint has no owner"
            );
            window.focus(&input_focus, cx);
            assert!(
                !window.has_input_handler(),
                "focus alone is not a painted handler"
            );
            window.draw(cx).clear(cx);
            assert!(
                window.has_input_handler(),
                "the focused input registered in paint"
            );

            let rendered = renders.get();
            window.draw(cx).clear(cx);
            assert_eq!(renders.get(), rendered, "unchanged child paint was cached");
            assert!(
                window.has_input_handler(),
                "cached paint retained its handler"
            );
        });

        let mut platform = cx.test_window(cx.window_handle());
        let handler = platform
            .take_input_handler()
            .expect("paint installed a handler");
        cx.update(|window, _| {
            assert!(
                window.has_input_handler(),
                "native extraction does not release ownership"
            );
        });
        let rendered = renders.get();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(
                window.has_input_handler(),
                "a cached paint during native extraction retains its focused handler"
            );
        });
        assert_eq!(renders.get(), rendered, "the extracted handler was cached");
        platform.set_input_handler(handler);

        cx.update(|window, cx| {
            window.focus(&sibling_focus, cx);
            assert!(
                window.has_input_handler(),
                "focus handoff awaits the next paint"
            );
            window.draw(cx).clear(cx);
            assert!(
                !window.has_input_handler(),
                "the sibling has no input handler"
            );

            window.focus(&input_focus, cx);
            window.draw(cx).clear(cx);
            assert!(window.has_input_handler());
        });

        covered.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(
                !window.has_input_handler(),
                "an inert cover retires the handler"
            );
        });

        covered.set(false);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(!window.has_input_handler());
            window.focus(&input_focus, cx);
            window.draw(cx).clear(cx);
            assert!(window.has_input_handler());
        });

        let mut platform = cx.test_window(cx.window_handle());
        let stale_after_unmount = platform.take_input_handler().unwrap();
        mounted.set(false);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert!(!window.has_input_handler(), "unmount retires the handler");
        });
        platform.set_input_handler(stale_after_unmount);
        let mut stale_after_unmount = platform.take_input_handler().unwrap();
        stale_after_unmount.replace_text_in_range(None, "z");
        assert_eq!(events.ime_insertions.get(), 0, "retired input cannot edit");
    }

    struct TwoPaintedInputs {
        first: Entity<PaintedInputView>,
        second: Entity<PaintedInputView>,
    }

    impl Render for TwoPaintedInputs {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.first.clone()).child(self.second.clone())
        }
    }

    /// Native backends restore the wrapper they took before calling into GPUI.
    /// A text callback can repaint and install another focused editor before
    /// that restore; the old wrapper must then delegate to the new owner.
    #[gpui::test]
    fn stale_native_wrapper_delivers_to_reentrant_painted_owner(cx: &mut TestAppContext) {
        let first_events = Rc::new(Events::default());
        let second_events = Rc::new(Events::default());
        let first_focus = Rc::new(RefCell::new(None));
        let second_focus = Rc::new(RefCell::new(None));
        let (_root, cx) = cx.add_window_view({
            let first_events = first_events.clone();
            let second_events = second_events.clone();
            let first_focus = first_focus.clone();
            let second_focus = second_focus.clone();
            move |_, cx| {
                let first = cx.focus_handle();
                let second = cx.focus_handle();
                *first_focus.borrow_mut() = Some(first.clone());
                *second_focus.borrow_mut() = Some(second.clone());
                TwoPaintedInputs {
                    first: cx.new(|_| PaintedInputView {
                        focus: first,
                        events: first_events,
                        renders: Rc::new(Cell::new(0)),
                    }),
                    second: cx.new(|_| PaintedInputView {
                        focus: second,
                        events: second_events,
                        renders: Rc::new(Cell::new(0)),
                    }),
                }
            }
        });
        let first_focus = first_focus.borrow().clone().unwrap();
        let second_focus = second_focus.borrow().clone().unwrap();
        cx.update(|window, cx| {
            window.focus(&first_focus, cx);
            window.draw(cx).clear(cx);
            assert!(window.has_input_handler());
        });

        first_events
            .input_handoff
            .replace(Some(second_focus.clone()));
        let mut platform = cx.test_window(cx.window_handle());
        let mut old_wrapper = platform.take_input_handler().unwrap();
        old_wrapper.replace_text_in_range(None, "a");
        cx.update(|window, _| {
            assert!(second_focus.is_focused(window));
            assert!(window.has_input_handler());
        });
        // This is what Mac, Linux, and Windows native callbacks do after
        // returning from GPUI: the held old wrapper overwrites the new one.
        platform.set_input_handler(old_wrapper);
        let mut restored_old_wrapper = platform.take_input_handler().unwrap();
        restored_old_wrapper.replace_text_in_range(None, "b");
        platform.set_input_handler(restored_old_wrapper);
        assert_eq!(first_events.ime_insertions.get(), 1);
        assert_eq!(second_events.ime_insertions.get(), 1);
    }

    struct MixedCachedInputRoot {
        focus: FocusHandle,
        earlier_events: Rc<Events>,
        later_cached: Entity<PaintedInputView>,
    }

    impl Render for MixedCachedInputRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .child(InputProbe {
                    focus: self.focus.clone(),
                    events: self.earlier_events.clone(),
                })
                .child(
                    self.later_cached
                        .clone()
                        .cached(StyleRefinement::default().w(px(100.)).h(px(30.))),
                )
        }
    }

    /// Both registrations use one focus. During native extraction the first
    /// handler paints fresh, but the later, selected handler is cached and its
    /// wrapper is still held by the callback. Paint order still chooses later.
    #[gpui::test]
    fn later_cached_input_beats_earlier_fresh_input_during_extraction(cx: &mut TestAppContext) {
        let earlier_events = Rc::new(Events::default());
        let later_events = Rc::new(Events::default());
        let later_renders = Rc::new(Cell::new(0));
        let focus_slot = Rc::new(RefCell::new(None));
        let (_root, cx) = cx.add_window_view({
            let earlier_events = earlier_events.clone();
            let later_events = later_events.clone();
            let later_renders = later_renders.clone();
            let focus_slot = focus_slot.clone();
            move |_, cx| {
                let focus = cx.focus_handle();
                *focus_slot.borrow_mut() = Some(focus.clone());
                MixedCachedInputRoot {
                    focus: focus.clone(),
                    earlier_events,
                    later_cached: cx.new(|_| PaintedInputView {
                        focus,
                        events: later_events,
                        renders: later_renders,
                    }),
                }
            }
        });
        let focus = focus_slot.borrow().clone().unwrap();
        cx.update(|window, cx| {
            window.focus(&focus, cx);
            window.draw(cx).clear(cx);
            assert!(window.has_input_handler());
        });

        let mut platform = cx.test_window(cx.window_handle());
        let held_later_wrapper = platform.take_input_handler().unwrap();
        let rendered = later_renders.get();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(window.has_input_handler());
        });
        assert_eq!(
            later_renders.get(),
            rendered,
            "later input reused cached paint"
        );
        platform.set_input_handler(held_later_wrapper);

        let mut restored_later_wrapper = platform.take_input_handler().unwrap();
        restored_later_wrapper.replace_text_in_range(None, "x");
        platform.set_input_handler(restored_later_wrapper);
        assert_eq!(earlier_events.ime_insertions.get(), 0);
        assert_eq!(later_events.ime_insertions.get(), 1);
    }

    struct FrameProbe {
        events: Rc<Events>,
        once: Option<Rc<Cell<bool>>>,
        invalidate_on_frame: bool,
        callback_label: Option<&'static str>,
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
                window.request_layout(
                    Style {
                        size: size(px(0.).into(), px(0.).into()),
                        ..Style::default()
                    },
                    [],
                    cx,
                ),
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
            if self
                .once
                .as_ref()
                .is_none_or(|scheduled| !scheduled.replace(true))
            {
                let events = self.events.clone();
                let invalidate_on_frame = self.invalidate_on_frame;
                let callback_label = self.callback_label;
                window.on_next_frame(move |window, _| {
                    if let Some(label) = callback_label {
                        events.callback_order.borrow_mut().push(label);
                    }
                    events
                        .animation_frames
                        .set(events.animation_frames.get() + 1);
                    if invalidate_on_frame {
                        window.refresh();
                    }
                });
            }
        }
    }

    struct QueuedCallbackView {
        events: Rc<Events>,
        scheduled: Rc<Cell<bool>>,
        deferred: bool,
        callback_label: &'static str,
    }

    impl Render for QueuedCallbackView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let probe = FrameProbe {
                events: self.events.clone(),
                once: Some(self.scheduled.clone()),
                invalidate_on_frame: true,
                callback_label: Some(self.callback_label),
            };
            if self.deferred {
                deferred(probe).into_any_element()
            } else {
                probe.into_any_element()
            }
        }
    }

    struct QueuedCallbackRoot {
        child: Entity<QueuedCallbackView>,
        sibling: Entity<QueuedCallbackView>,
        inert: Rc<Cell<bool>>,
        draws: Rc<Cell<usize>>,
        sibling_events: Rc<Events>,
    }

    impl Render for QueuedCallbackRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.draws.set(self.draws.get() + 1);
            let child = self
                .child
                .clone()
                .cached(StyleRefinement::default().w(px(100.)).h(px(30.)));
            let child: AnyElement = if self.inert.get() {
                let nested = inert("nested-queued-boundary", "Nested retained child", child);
                inert(
                    "queued-callback-boundary",
                    "Retained callback child",
                    nested,
                )
                .into_any_element()
            } else {
                child.into_any_element()
            };
            let sibling = self
                .sibling
                .clone()
                .cached(StyleRefinement::default().w(px(100.)).h(px(30.)));
            div().child(child).child(sibling).when(
                self.sibling_events.animation_frames.get() > 0,
                |root| {
                    root.child(
                        div()
                            .id("sibling-callback-updated")
                            .debug_selector(|| "sibling-callback-updated".to_string())
                            .w(px(10.))
                            .h(px(10.)),
                    )
                },
            )
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
            window: &mut Window,
            cx: &mut App,
        ) {
            self.insertions
                .ime_insertions
                .set(self.insertions.ime_insertions.get() + 1);
            let next_focus = self.insertions.input_handoff.borrow_mut().take();
            if let Some(next_focus) = next_focus {
                window.focus(&next_focus, cx);
                window.draw(cx).clear(cx);
            }
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
        let active_key_events = events.keys.get();
        assert!(
            active_key_events > 0,
            "the focused control receives key input"
        );
        assert_eq!(events.actions.get(), 1, "one explicit action is dispatched");
        assert_eq!(events.a11y_actions.get(), 1);
        let active_ime_insertions = events.ime_insertions.get();
        assert!(
            active_ime_insertions > 0,
            "the active input handler receives text input"
        );
        cx.simulate_click(point(px(10.), px(40.)), Default::default());
        assert_eq!(events.nested_clicks.get(), 0);
        cx.update(|window, cx| {
            window.focus(&nested_focus, cx);
            assert!(!nested_focus.is_focused(window));
            window.focus(&focus, cx);
            assert!(focus.is_focused(window));
        });

        let animation_frames_before_drain = events.animation_frames.get();
        let delivered_active_frames = cx.update(|window, cx| window.simulate_next_frame(cx));
        assert!(
            delivered_active_frames > 0,
            "the active subtree has queued frame callbacks"
        );
        let active_animation_frames = events.animation_frames.get();
        assert_eq!(
            active_animation_frames,
            animation_frames_before_drain + delivered_active_frames
        );

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
            let sibling = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("active sibling"))
                .map(|(_, node)| node)
                .unwrap();
            assert!(!sibling.is_disabled());

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
        assert_eq!(events.keys.get(), active_key_events);
        assert_eq!(events.actions.get(), 1);
        assert_eq!(events.a11y_actions.get(), 1);
        assert_eq!(events.ime_insertions.get(), active_ime_insertions);
        assert_eq!(events.animation_frames.get(), active_animation_frames);
        assert_eq!(events.nested_clicks.get(), 0);

        cx.update(|window, cx| {
            window.focus(&sibling_focus, cx);
            assert!(sibling_focus.is_focused(window));
            window.focus_prev(cx);
            assert!(sibling_focus.is_focused(window));
            assert!(!focus.is_focused(window));
        });
        let sibling_bounds = cx.debug_bounds("active-sibling").unwrap();
        let sibling_click_point = sibling_bounds.center();
        assert!(sibling_bounds.contains(&sibling_click_point));
        cx.simulate_click(sibling_click_point, Default::default());
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
        assert_eq!(events.keys.get(), active_key_events + 1);
    }

    #[gpui::test]
    fn freshly_mounted_inert_child_never_registers_interactions(cx: &mut TestAppContext) {
        let events = Rc::new(Events::default());
        let child_focus = Rc::new(std::cell::RefCell::new(None));
        let nested_focus = Rc::new(std::cell::RefCell::new(None));
        let sibling_focus = Rc::new(std::cell::RefCell::new(None));
        let (_root, cx) = cx.add_window_view({
            let events = events.clone();
            let child_focus = child_focus.clone();
            let nested_focus = nested_focus.clone();
            move |_, cx| {
                let focus = cx.focus_handle();
                let nested = cx.focus_handle();
                let sibling = cx.focus_handle();
                *child_focus.borrow_mut() = Some(focus.clone());
                *nested_focus.borrow_mut() = Some(nested.clone());
                *sibling_focus.borrow_mut() = Some(sibling.clone());
                RootView {
                    child: cx.new(|_| InertInteractiveView {
                        focus,
                        nested_focus: nested,
                        events: events.clone(),
                    }),
                    inert: Rc::new(Cell::new(true)),
                    sibling_focus: sibling,
                    events,
                }
            }
        });
        let child_focus = child_focus.borrow().clone().unwrap();
        let nested_focus = nested_focus.borrow().clone().unwrap();

        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.focus(&child_focus, cx);
            window.focus(&nested_focus, cx);
            assert!(!child_focus.is_focused(window));
            assert!(!nested_focus.is_focused(window));
            assert_eq!(window.simulate_next_frame(cx), 0);
        });
        cx.simulate_click(point(px(10.), px(10.)), Default::default());
        assert_eq!(events.clicks.get(), 0);
        assert_eq!(events.nested_clicks.get(), 0);

        let sibling_bounds = cx.debug_bounds("active-sibling").unwrap();
        cx.simulate_click(sibling_bounds.center(), Default::default());
        assert_eq!(events.sibling_clicks.get(), 1);
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

    #[gpui::test]
    fn inert_subtree_revokes_only_its_own_pointer_capture(cx: &mut TestAppContext) {
        let inert_state = Rc::new(Cell::new(false));
        let replace_child = Rc::new(Cell::new(false));
        let child_hitbox = Rc::new(Cell::new(None));
        let sibling_hitbox = Rc::new(Cell::new(None));
        let captured_sibling = Rc::new(Cell::new(None));
        let (root, cx) = cx.add_window_view({
            let inert_state = inert_state.clone();
            let replace_child = replace_child.clone();
            let child_hitbox = child_hitbox.clone();
            let sibling_hitbox = sibling_hitbox.clone();
            move |_, cx| {
                let child = cx.new(|_| PointerCaptureTargetView {
                    hitbox: child_hitbox.clone(),
                });
                let sibling = cx.new(|_| PointerCaptureTargetView {
                    hitbox: sibling_hitbox,
                });
                PointerCaptureRoot {
                    child,
                    sibling,
                    inert: inert_state,
                    replace_child,
                    child_hitbox,
                }
            }
        });

        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let sibling = sibling_hitbox.get().expect("sibling hitbox was painted");
            window.capture_pointer(sibling);
            captured_sibling.set(Some(sibling));
            assert_eq!(window.captured_hitbox(), Some(sibling));
        });

        // Redrawing an inert sibling must not cancel capture held by this active sibling.
        inert_state.set(true);
        root.update(cx, |_, root_cx| root_cx.notify());
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let owner_work = window.element_owner_path_work();
            assert!(owner_work.nodes_created > 0);
            assert!(owner_work.cached_handles_reused > 0);
            assert!(owner_work.node_storage_growths <= owner_work.nodes_created);
            assert_eq!(
                window.captured_hitbox(),
                captured_sibling.get(),
                "inert redraw revoked an unrelated live sibling's capture"
            );
        });

        // The same capture is revoked when its own previously active cached child becomes inert.
        inert_state.set(false);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let child = child_hitbox.get().expect("child hitbox was painted");
            window.capture_pointer(child);
            assert_eq!(window.captured_hitbox(), Some(child));
        });
        inert_state.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(
                window.captured_hitbox(),
                None,
                "capture remained attached to a subtree after it became inert"
            );
        });

        // A removed/replaced active owner no longer has a structural capture target either.
        inert_state.set(false);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let child = child_hitbox.get().expect("child hitbox was painted");
            window.capture_pointer(child);
            assert_eq!(window.captured_hitbox(), Some(child));
        });
        replace_child.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(
                window.captured_hitbox(),
                None,
                "capture remained attached after its identified owner was replaced"
            );
        });
    }

    #[gpui::test]
    fn fresh_idless_inert_child_is_revoked_without_touching_idless_sibling(
        cx: &mut TestAppContext,
    ) {
        let inert_state = Rc::new(Cell::new(false));
        let child_hitbox = Rc::new(Cell::new(None));
        let sibling_hitbox = Rc::new(Cell::new(None));
        let (_root, cx) = cx.add_window_view({
            let inert_state = inert_state.clone();
            let child_hitbox = child_hitbox.clone();
            let sibling_hitbox = sibling_hitbox.clone();
            move |_, _| FreshIdlessCaptureRoot {
                inert: inert_state,
                child_hitbox,
                sibling_hitbox,
            }
        });

        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let child = child_hitbox.get().expect("fresh child hitbox was painted");
            window.capture_pointer(child);
            assert_eq!(window.captured_hitbox(), Some(child));
        });
        inert_state.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(
                window.captured_hitbox(),
                None,
                "a fresh idless child retained capture after becoming inert"
            );
        });

        inert_state.set(false);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let sibling = sibling_hitbox.get().expect("idless sibling was painted");
            window.capture_pointer(sibling);
            assert_eq!(window.captured_hitbox(), Some(sibling));
        });
        inert_state.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(
                window.captured_hitbox(),
                sibling_hitbox.get(),
                "the active idless sibling lost capture when its sibling became inert"
            );
        });
    }

    #[gpui::test]
    fn keyed_reorder_and_inert_wrapper_only_revoke_the_wrapped_owner(cx: &mut TestAppContext) {
        let inert_state = Rc::new(Cell::new(false));
        let reordered = Rc::new(Cell::new(false));
        let target_hitbox = Rc::new(Cell::new(None));
        let sibling_hitbox = Rc::new(Cell::new(None));
        let target_moves = Rc::new(Cell::new(0));
        let (_root, cx) = cx.add_window_view({
            let inert_state = inert_state.clone();
            let reordered = reordered.clone();
            let target_hitbox = target_hitbox.clone();
            let sibling_hitbox = sibling_hitbox.clone();
            let target_moves = target_moves.clone();
            move |_, _| KeyedCaptureRoot {
                inert: inert_state,
                reordered,
                target_hitbox,
                sibling_hitbox,
                target_moves,
            }
        });

        let target = cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let target = target_hitbox
                .get()
                .expect("the keyed target hitbox was painted");
            window.capture_pointer(target);
            assert_eq!(window.captured_hitbox(), Some(target));
            target
        });

        // A keyed target survives active reorder. Its new frame hitbox has a fresh ID, so the
        // captured ID must be rebound through the exact owner path for listeners to keep working.
        reordered.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let rebound = target_hitbox
                .get()
                .expect("the reordered keyed target hitbox was painted");
            assert_ne!(rebound, target);
            assert_eq!(window.captured_hitbox(), Some(rebound));
        });
        cx.simulate_mouse_move(point(px(150.), px(30.)), None, Default::default());
        assert!(
            target_moves.get() > 0,
            "a keyed capture must route movement to the newly painted hitbox"
        );

        // Adding an inert wrapper around the same keyed owner revokes the capture.
        inert_state.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(window.captured_hitbox(), None);
        });

        // A live keyed sibling moved into the old target slot remains independently captured.
        inert_state.set(false);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            let sibling = sibling_hitbox
                .get()
                .expect("the keyed sibling hitbox was painted");
            window.capture_pointer(sibling);
            assert_eq!(window.captured_hitbox(), Some(sibling));
        });
        inert_state.set(true);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(
                window.captured_hitbox(),
                sibling_hitbox.get(),
                "the keyed sibling lost capture when its sibling became inert"
            );
        });
    }

    #[gpui::test]
    fn queued_frame_callback_is_skipped_for_newly_inert_owner(cx: &mut TestAppContext) {
        let child_events = Rc::new(Events::default());
        let sibling_events = Rc::new(Events::default());
        let inert_state = Rc::new(Cell::new(false));
        let child_scheduled = Rc::new(Cell::new(false));
        let sibling_scheduled = Rc::new(Cell::new(false));
        let draws = Rc::new(Cell::new(0));

        let (_root, cx) = cx.add_window_view({
            let child_events = child_events.clone();
            let sibling_events = sibling_events.clone();
            let inert_state = inert_state.clone();
            let child_scheduled = child_scheduled.clone();
            let sibling_scheduled = sibling_scheduled.clone();
            let draws = draws.clone();
            move |_, cx| {
                let sibling_view_events = sibling_events.clone();
                let child = cx.new(|_| QueuedCallbackView {
                    events: child_events,
                    scheduled: child_scheduled,
                    deferred: true,
                    callback_label: "child",
                });
                let sibling = cx.new(|_| QueuedCallbackView {
                    events: sibling_view_events,
                    scheduled: sibling_scheduled,
                    deferred: false,
                    callback_label: "sibling",
                });
                QueuedCallbackRoot {
                    child,
                    sibling,
                    inert: inert_state,
                    draws,
                    sibling_events: sibling_events.clone(),
                }
            }
        });

        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        assert!(child_scheduled.get());
        assert!(sibling_scheduled.get());
        let initial_draw_count = draws.get();
        assert_eq!(child_events.animation_frames.get(), 0);
        assert_eq!(sibling_events.animation_frames.get(), 0);
        cx.update(|window, _| {
            let order = sibling_events.clone();
            // This is a true window-level callback registered after the element callbacks. The
            // dirty-frame path must not partition it ahead of the earlier scoped sibling.
            window.on_next_frame(move |_, _| {
                order.callback_order.borrow_mut().push("window");
            });
        });

        inert_state.set(true);
        cx.update(|window, cx| {
            window.refresh();
            // Commit the transition before delivering callbacks queued by the prior active
            // frame, matching the dirty-frame production path.
            window.draw(cx).clear(cx);
            // The child callback is skipped, while the sibling and true window-level callbacks
            // both run in their original order; only the sibling dirties a reconciliation draw.
            assert_eq!(window.simulate_reconciled_next_frame(cx), (2, 1));
        });

        assert_eq!(child_events.animation_frames.get(), 0);
        assert_eq!(sibling_events.animation_frames.get(), 1);
        assert_eq!(
            sibling_events.callback_order.borrow().as_slice(),
            &["sibling", "window"],
            "mixed callback registrations keep order while the newly inert child is skipped"
        );
        assert!(
            cx.debug_bounds("sibling-callback-updated").is_some(),
            "callback state must be rendered before the same presentation"
        );
        assert_eq!(
            draws.get(),
            initial_draw_count + 2,
            "the transition and one live callback update each draw once"
        );
        cx.update(|window, cx| {
            assert_eq!(window.simulate_reconciled_next_frame(cx), (0, 0));
        });
        assert_eq!(
            draws.get(),
            initial_draw_count + 2,
            "idle callback delivery must not add a draw"
        );
    }

    struct PositionedInertControl {
        clicks: Rc<Cell<usize>>,
    }

    impl Render for PositionedInertControl {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.clicks.clone();
            div()
                .id("positioned-inert-control")
                .debug_selector(|| "positioned-inert-control".to_string())
                .w(px(100.))
                .h(px(40.))
                .role(accesskit::Role::Button)
                .aria_label("positioned inert control")
                .on_click(move |_, _, _| clicks.set(clicks.get() + 1))
        }
    }

    struct PositionedInertHost {
        child: Entity<PositionedInertControl>,
    }

    impl Render for PositionedInertHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let child = self
                .child
                .clone()
                .cached(StyleRefinement::default().w(px(100.)).h(px(40.)));
            let child = inert(
                "inner-positioned-inert",
                "Inner positioned content is unavailable",
                child,
            );
            let child = inert(
                "outer-positioned-inert",
                "Positioned content is unavailable",
                child,
            );
            div()
                .id("translated-inert-parent")
                .debug_selector(|| "translated-inert-parent".to_string())
                .size_full()
                .pl(px(80.))
                .pt(px(60.))
                .child(child)
        }
    }

    #[gpui::test]
    fn nested_inert_scopes_preserve_translated_visual_and_accessibility_bounds(
        cx: &mut TestAppContext,
    ) {
        let clicks = Rc::new(Cell::new(0));
        let (_host, visual) = cx.add_window_view({
            let clicks = clicks.clone();
            move |_, cx| PositionedInertHost {
                child: cx.new(|_| PositionedInertControl { clicks }),
            }
        });
        let accessibility_bounds = visual.update(|window, cx| {
            window.set_a11y_forced(true);
            window.draw(cx).clear(cx);

            let tree = window.a11y_tree().expect("accessibility is enabled");
            let (control_id, control) = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("positioned inert control"))
                .map(|(id, node)| (*id, node))
                .expect("the retained control remains accessible");
            assert!(control.is_disabled());
            assert!(
                !control.supports_action(accesskit::Action::Click),
                "inert controls do not expose an accessibility click"
            );
            let accessibility_bounds = window
                .a11y_node_bounds(control_id)
                .expect("the retained control has accessibility bounds");
            window.handle_a11y_action(
                accesskit::ActionRequest {
                    action: accesskit::Action::Click,
                    target_tree: accesskit::TreeId::ROOT,
                    target_node: control_id,
                    data: None,
                },
                cx,
            );
            accessibility_bounds
        });
        let visual_bounds = visual
            .debug_bounds("positioned-inert-control")
            .expect("the retained control has visual bounds");
        assert_eq!(visual_bounds, accessibility_bounds);
        assert_eq!(visual_bounds.size.width, px(100.));
        assert_eq!(visual_bounds.size.height, px(40.));
        let parent_bounds = visual
            .debug_bounds("translated-inert-parent")
            .expect("the padded parent has rendered bounds");
        assert_eq!(visual_bounds.origin.x, parent_bounds.origin.x + px(80.));
        assert_eq!(visual_bounds.origin.y, parent_bounds.origin.y + px(60.));

        visual.simulate_click(visual_bounds.center(), Default::default());
        assert_eq!(
            clicks.get(),
            0,
            "the disabled visual control cannot activate"
        );
    }
}
