//! Native activation ownership without remounting element state.

use crate::{
    AnyElement, App, Bounds, Element, ElementId, EntityId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Window,
};

/// An exact native activation owner. The epoch is supplied by the owning UI
/// lifecycle; GPUI never allocates another counter or producer authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum NativeActivationScope {
    /// Pending presses may finish only under this same owner and epoch.
    Active { owner: EntityId, epoch: u64 },
    /// Checked lifecycle exhaustion or an unavailable input owner fails closed.
    Retired { owner: EntityId },
}
impl NativeActivationScope {
    /// Construct from an existing checked lifecycle epoch.
    pub fn new(owner: EntityId, epoch: Option<u64>) -> Self {
        match epoch {
            Some(epoch) => Self::Active { owner, epoch },
            None => Self::Retired { owner },
        }
    }
    /// Qualify the same existing receipt with an actual native field entity.
    pub fn for_owner(self, owner: EntityId) -> Self {
        match self {
            Self::Active { epoch, .. } => Self::Active { owner, epoch },
            Self::Retired { .. } => Self::Retired { owner },
        }
    }
    pub(crate) fn admits(scope: Option<Self>) -> bool {
        !matches!(scope, Some(Self::Retired { .. }))
    }
}

/// Scope native mouse/keyboard activation while preserving element ancestry,
/// keyed entities, native focus handles, scroll offsets and local expansion.
/// Nested scopes may name an independent local field/visit. An absent wrapper
/// retains GPUI's ordinary unscoped activation behavior.
pub fn native_activation_scope(
    scope: NativeActivationScope,
    child: impl IntoElement,
) -> NativeActivationScopeElement {
    NativeActivationScopeElement {
        scope,
        child: child.into_any_element(),
    }
}

/// An ancestry-transparent native activation boundary.
pub struct NativeActivationScopeElement {
    scope: NativeActivationScope,
    child: AnyElement,
}
impl IntoElement for NativeActivationScopeElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for NativeActivationScopeElement {
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
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let layout = window.with_native_activation_scope(Some(self.scope), |window| {
            self.child.request_layout(window, cx)
        });
        (layout, ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_native_activation_scope(Some(self.scope), |window| {
            // The wrapper returns the child's own LayoutId. Preserve the
            // parent's offset so the child applies its layout origin once.
            self.child.prepaint(window, cx)
        });
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window
            .with_native_activation_scope(Some(self.scope), |window| self.child.paint(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        self as gpui, AppContext as _, Context, Entity, FocusHandle, InteractiveElement,
        KeyDownEvent, KeyUpEvent, Keystroke, MouseButton, ParentElement, Render,
        StatefulInteractiveElement, StyleRefinement, Styled, TestAppContext, div, point, px,
    };
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    struct PressView {
        focus: FocusHandle,
        clicks: Rc<Cell<usize>>,
        state: Rc<RefCell<Option<Entity<String>>>>,
        renders: Rc<Cell<usize>>,
    }
    impl Render for PressView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            let draft = window.use_keyed_state("draft", cx, |_, _| "edited local draft".to_owned());
            *self.state.borrow_mut() = Some(draft);
            let clicks = self.clicks.clone();
            div()
                .id("same-control")
                .w(px(100.))
                .h(px(40.))
                .track_focus(&self.focus)
                .on_click(move |_, _, _| clicks.set(clicks.get() + 1))
        }
    }
    struct Host {
        child: Entity<PressView>,
        epoch: Option<u64>,
        local: bool,
        covered: bool,
        deferred: bool,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let cached = self
                .child
                .clone()
                .cached(StyleRefinement::default().w(px(100.)).h(px(40.)));
            let child: AnyElement = if self.local {
                native_activation_scope(
                    NativeActivationScope::new(
                        self.child.entity_id(),
                        (!self.covered).then_some(0),
                    ),
                    cached,
                )
                .into_any_element()
            } else {
                cached.into_any_element()
            };
            let child: AnyElement = if self.deferred {
                crate::deferred(child).into_any_element()
            } else {
                child
            };
            native_activation_scope(
                NativeActivationScope::new(cx.entity_id(), self.epoch),
                div().id("stable-host").size_full().child(child),
            )
        }
    }

    #[gpui::test]
    fn cached_and_deferred_native_presses_require_the_same_activation_owner(
        cx: &mut TestAppContext,
    ) {
        for deferred in [false, true] {
            let clicks = Rc::new(Cell::new(0));
            let state = Rc::new(RefCell::new(None));
            let renders = Rc::new(Cell::new(0));
            let (host, visual) = cx.add_window_view({
                let clicks = clicks.clone();
                let state = state.clone();
                let renders = renders.clone();
                move |window, cx| {
                    let child = cx.new(|cx| PressView {
                        focus: cx.focus_handle(),
                        clicks,
                        state,
                        renders,
                    });
                    child.update(cx, |child, cx| window.focus(&child.focus, cx));
                    Host {
                        child,
                        epoch: Some(1),
                        local: false,
                        covered: false,
                        deferred,
                    }
                }
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let original = state.borrow().as_ref().expect("mounted draft").entity_id();
            let focused = visual.update(|window, cx| window.focused(cx));
            let count = renders.get();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(
                renders.get(),
                count,
                "the unchanged subtree really uses its cache"
            );
            let at = point(px(10.), px(10.));
            visual.simulate_mouse_down(at, MouseButton::Left, Default::default());
            visual.update(|_, cx| {
                host.update(cx, |host, cx| {
                    host.epoch = Some(2);
                    cx.notify();
                })
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(
                renders.get() > count,
                "scope change refreshes cached listeners, including deferred draws"
            );
            visual.simulate_mouse_up(at, MouseButton::Left, Default::default());
            assert_eq!(
                clicks.get(),
                0,
                "new callbacks cannot borrow the old native down"
            );
            assert_eq!(
                state.borrow().as_ref().expect("draft").entity_id(),
                original,
                "scope changes preserve keyed entities"
            );
            assert_eq!(visual.update(|window, cx| window.focused(cx)), focused);
            visual.simulate_click(at, Default::default());
            assert_eq!(clicks.get(), 1);
            let key = Keystroke::parse("enter").expect("key");
            visual.simulate_event(KeyDownEvent {
                keystroke: key.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            visual.update(|_, cx| {
                host.update(cx, |host, cx| {
                    host.epoch = Some(3);
                    cx.notify();
                })
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.simulate_event(KeyUpEvent {
                keystroke: key.clone(),
            });
            assert_eq!(
                clicks.get(),
                1,
                "focus identity alone cannot authorize a covered activation"
            );
            visual.simulate_event(KeyDownEvent {
                keystroke: key.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            visual.simulate_event(KeyUpEvent { keystroke: key });
            assert_eq!(
                clicks.get(),
                2,
                "a fresh native keyboard gesture remains live"
            );
            visual.update(|_, cx| {
                host.update(cx, |host, cx| {
                    host.epoch = None;
                    cx.notify();
                })
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.simulate_click(at, Default::default());
            assert_eq!(
                clicks.get(),
                2,
                "retired scope cannot allocate a reusable activation"
            );
        }
    }

    #[gpui::test]
    fn local_field_owner_preserves_a_press_but_cover_and_unwind_retire_it(cx: &mut TestAppContext) {
        let clicks = Rc::new(Cell::new(0));
        let state = Rc::new(RefCell::new(None));
        let (host, visual) = cx.add_window_view({
            let clicks = clicks.clone();
            let state = state.clone();
            move |_, cx| {
                let child = cx.new(|cx| PressView {
                    focus: cx.focus_handle(),
                    clicks,
                    state,
                    renders: Rc::new(Cell::new(0)),
                });
                Host {
                    child,
                    epoch: Some(1),
                    local: true,
                    covered: false,
                    deferred: true,
                }
            }
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let original = state.borrow().as_ref().expect("mounted draft").entity_id();
        let at = point(px(10.), px(10.));
        visual.simulate_mouse_down(at, MouseButton::Left, Default::default());
        visual.update(|_, cx| {
            host.update(cx, |host, cx| {
                host.epoch = Some(2);
                cx.notify();
            })
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.simulate_mouse_up(at, MouseButton::Left, Default::default());
        assert_eq!(
            clicks.get(),
            1,
            "independent local field owns its held activation"
        );
        assert_eq!(
            state.borrow().as_ref().expect("draft").entity_id(),
            original
        );
        visual.simulate_mouse_down(at, MouseButton::Left, Default::default());
        for covered in [true, false] {
            visual.update(|_, cx| {
                host.update(cx, |host, cx| {
                    host.covered = covered;
                    cx.notify();
                })
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
        visual.simulate_mouse_up(at, MouseButton::Left, Default::default());
        assert_eq!(
            clicks.get(),
            1,
            "returning to a local owner cannot revive its covered press"
        );
        visual.simulate_click(at, Default::default());
        assert_eq!(clicks.get(), 2);
        visual.update(|window, _| {
            assert_eq!(window.native_activation_scope(), None);
            let outer = NativeActivationScope::new(host.entity_id(), Some(8));
            window.with_native_activation_scope(Some(outer), |window| {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    window.with_native_activation_scope(
                        Some(NativeActivationScope::new(host.entity_id(), None)),
                        |_| panic!("controlled scope unwind"),
                    );
                }));
                assert!(result.is_err());
                assert_eq!(window.native_activation_scope(), Some(outer));
            });
            assert_eq!(
                window.native_activation_scope(),
                None,
                "no ambient scope leaks beyond its subtree"
            );
        });
    }
}
