use super::action_tree::ActionTree;
use super::semantic::{ActionMetadata, ActionState, SemanticBounds};
use gpui::{App, Bounds, FocusHandle, SharedString, Window};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Owns semantic snapshots and post-layout evidence for one application/window
/// owner.
///
/// A workspace keeps one instance and threads it through its `Theme`. Adapter
/// calls therefore carry an explicit window frame even when a CE popup renders
/// after the root element has been built. One window state record owns every
/// related value, so a reset or frame replacement cannot leave a tree paired
/// with measurements, focus requests, or pointer state from another revision.
#[derive(Clone, Debug, Default)]
pub(crate) struct ActionFrames {
    states: Rc<RefCell<HashMap<u64, WindowActionState>>>,
}

#[derive(Clone, Debug, Default)]
struct WindowActionState {
    tree: ActionTree,
    measured: MeasuredActionFrame,
    requested_focus: Option<String>,
    modal_restore: Option<String>,
    previous_native_focus: Option<String>,
    previous_pointer_states: HashMap<String, (bool, bool)>,
    pending_focus: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct MeasuredActionFrame {
    revision: u64,
    bounds: HashMap<String, SemanticBounds>,
    focus_order: HashMap<String, u32>,
    /// The owner reported by the concrete GPUI focus handle during render.
    /// This stays separate from the metadata-derived owner so capture cannot
    /// silently present declared focus as native evidence.
    native_focus_owner: Option<String>,
    focus_owner: Option<String>,
}

/// A registration capability for one rendered window frame. The revision is
/// checked on every write so a retained CE overlay from an older render cannot
/// leak actions into the next frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ActionFrameToken {
    window_id: u64,
    revision: u64,
}

impl ActionFrames {
    pub(crate) fn begin(
        &self,
        window: &Window,
        route: impl Into<SharedString>,
    ) -> ActionFrameToken {
        self.begin_id(window.window_handle().window_id().as_u64(), route)
    }

    pub(super) fn begin_id(
        &self,
        window_id: u64,
        route: impl Into<SharedString>,
    ) -> ActionFrameToken {
        let mut states = self.states.borrow_mut();
        let mut previous = states.remove(&window_id);
        let previous_native_focus = previous
            .as_ref()
            .and_then(|state| state.measured.native_focus_owner.clone());
        let previous_pointer_states = previous
            .as_ref()
            .map(|state| {
                state
                    .tree
                    .iter()
                    .filter(|action| action.is_hovered() || action.is_pressed())
                    .map(|action| {
                        (
                            action.id().to_string(),
                            (action.is_hovered(), action.is_pressed()),
                        )
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let revision = previous
            .as_ref()
            .map_or(1, |state| state.tree.revision().saturating_add(1));
        let requested_focus = previous
            .as_mut()
            .and_then(|state| state.requested_focus.take());
        let modal_restore = previous
            .as_mut()
            .and_then(|state| state.modal_restore.take());
        states.insert(
            window_id,
            WindowActionState {
                tree: ActionTree::new(route.into(), revision),
                measured: MeasuredActionFrame {
                    revision,
                    ..MeasuredActionFrame::default()
                },
                requested_focus,
                modal_restore,
                previous_native_focus,
                previous_pointer_states,
                pending_focus: None,
            },
        );
        ActionFrameToken {
            window_id,
            revision,
        }
    }

    pub(crate) fn register(&self, token: ActionFrameToken, action: ActionMetadata) {
        if let Some(state) = self
            .states
            .borrow_mut()
            .get_mut(&token.window_id)
            .filter(|state| state.tree.revision() == token.revision)
        {
            state.tree.registrar().record(action);
        }
    }

    pub(crate) fn snapshot(&self, window: &Window) -> ActionTree {
        self.states
            .borrow()
            .get(&window.window_handle().window_id().as_u64())
            .map(|state| state.tree.clone())
            .unwrap_or_default()
    }

    pub(crate) fn reset(&self, window: &Window) {
        self.states
            .borrow_mut()
            .remove(&window.window_handle().window_id().as_u64());
    }

    pub(crate) fn finalize(&self, window: &Window) {
        self.finalize_id(window.window_handle().window_id().as_u64());
    }

    pub(super) fn finalize_id(&self, window_id: u64) {
        let mut states = self.states.borrow_mut();
        let Some(state) = states.get_mut(&window_id) else {
            return;
        };
        state.tree.finalize_modal();
        let pointer_states = std::mem::take(&mut state.previous_pointer_states);
        state.tree.restore_pointer_states(&pointer_states);
        if let Some(restore) = state.modal_restore.as_ref() {
            if state.tree.modal_root().is_some() {
                state.tree.set_restore_focus(restore.clone());
            }
        }
        let preferred = state
            .requested_focus
            .take()
            .or_else(|| state.previous_native_focus.clone());
        if state.tree.modal_root().is_none() {
            if let Some(preferred) = preferred {
                state.tree.set_focus_owner(&preferred);
            }
        }
    }

    pub(super) fn snapshot_id(&self, window_id: u64) -> ActionTree {
        self.states
            .borrow()
            .get(&window_id)
            .map(|state| state.tree.clone())
            .unwrap_or_default()
    }

    pub(crate) fn record_bounds(
        &self,
        token: ActionFrameToken,
        id: SharedString,
        bounds: Bounds<gpui::Pixels>,
    ) {
        let mut states = self.states.borrow_mut();
        let Some(state) = states
            .get_mut(&token.window_id)
            .filter(|state| state.tree.revision() == token.revision)
        else {
            return;
        };
        let Some((focusable, order, focused)) = state.tree.get(id.as_ref()).map(|action| {
            (
                action.is_focusable(),
                action.focus_order(),
                action.is_focused(),
            )
        }) else {
            // A measurement for an unregistered element is stale or points at
            // the wrong semantic id. Never let it become capture evidence.
            return;
        };
        let id = id.to_string();
        state.measured.bounds.insert(
            id.clone(),
            SemanticBounds::Logical {
                x: u32::from(bounds.origin.x),
                y: u32::from(bounds.origin.y),
                width: u32::from(bounds.size.width),
                height: u32::from(bounds.size.height),
            },
        );
        if focusable {
            state.measured.focus_order.insert(id.clone(), order as u32);
            if focused {
                state.measured.focus_owner = Some(id);
            }
        }
    }

    /// Records the native GPUI focus state observed by a concrete CE control.
    /// The action must already belong to this frame, guarding against a
    /// retained element from a prior route claiming current ownership.
    pub(crate) fn record_native_focus(
        &self,
        token: ActionFrameToken,
        id: SharedString,
        handle: FocusHandle,
        focused: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        let id = id.to_string();
        let should_defer = {
            let mut states = self.states.borrow_mut();
            let Some(state) = states
                .get_mut(&token.window_id)
                .filter(|state| state.tree.revision() == token.revision)
            else {
                return;
            };
            let Some((focusable, declared_focused)) = state
                .tree
                .get(id.as_str())
                .map(|action| (action.is_focusable(), action.is_focused()))
            else {
                return;
            };
            if !focusable {
                return;
            }
            if focused {
                state.tree.set_focus_owner(&id);
                state.measured.native_focus_owner = Some(id.clone());
                state.measured.focus_owner = Some(id.clone());
                state.pending_focus = None;
                if state.requested_focus.as_deref() == Some(id.as_str()) {
                    state.requested_focus = None;
                }
                false
            } else {
                let requested_focus = state.requested_focus.as_deref() == Some(id.as_str());
                let should_focus = declared_focused || requested_focus;
                if state.measured.native_focus_owner.as_deref() == Some(id.as_str()) {
                    state.measured.native_focus_owner = None;
                }
                if should_focus && state.pending_focus.as_deref() != Some(id.as_str()) {
                    state.pending_focus = Some(id.clone());
                    true
                } else {
                    false
                }
            }
        };
        if should_defer {
            window.defer(cx, move |window, cx| handle.focus(window, cx));
        }
    }

    /// Records hover/pressed state from the concrete CE interaction callbacks
    /// in this frame. This keeps pointer state beside the same live action
    /// tree used for focus and measured bounds.
    pub(crate) fn record_pointer_state(
        &self,
        token: ActionFrameToken,
        id: SharedString,
        state: ActionState,
        active: bool,
    ) {
        if let Some(frame) = self
            .states
            .borrow_mut()
            .get_mut(&token.window_id)
            .filter(|frame| frame.tree.revision() == token.revision)
        {
            frame.tree.set_pointer_state(id.as_ref(), state, active);
        }
    }

    /// Requests focus restoration to a stable action id on the next concrete
    /// render of that control. The actual FocusHandle is supplied by that
    /// control's observer, so restoration never relies on a synthetic handle.
    pub(crate) fn request_focus(&self, window: &Window, id: impl Into<String>) {
        let window_id = window.window_handle().window_id().as_u64();
        self.states
            .borrow_mut()
            .entry(window_id)
            .or_default()
            .requested_focus = Some(id.into());
    }

    pub(crate) fn remember_modal_restore(&self, window: &Window) {
        let window_id = window.window_handle().window_id().as_u64();
        let mut states = self.states.borrow_mut();
        let Some(state) = states.get_mut(&window_id) else {
            return;
        };
        if let Some(owner) = state.measured.native_focus_owner.clone() {
            state.modal_restore = Some(owner);
        }
    }

    pub(crate) fn request_modal_restore(&self, window: &Window) {
        let window_id = window.window_handle().window_id().as_u64();
        let mut states = self.states.borrow_mut();
        let Some(state) = states.get_mut(&window_id) else {
            return;
        };
        if let Some(owner) = state.modal_restore.take() {
            state.requested_focus = Some(owner);
        }
    }

    pub(crate) fn snapshot_bounds(&self, window: &Window) -> HashMap<String, SemanticBounds> {
        self.snapshot_bounds_id(window.window_handle().window_id().as_u64())
    }

    pub(super) fn snapshot_bounds_id(&self, window_id: u64) -> HashMap<String, SemanticBounds> {
        self.states
            .borrow()
            .get(&window_id)
            .map(|state| state.measured.bounds.clone())
            .unwrap_or_default()
    }

    /// Returns focus order proven by the same measured controls that supplied
    /// the rectangles. A metadata-only action never enters this map.
    ///
    /// This reads the order straight from the finalized tree instead of a
    /// value cached at `record_bounds` time. A dialog's content is built
    /// lazily by the CE `Dialog` element, so its controls can register after
    /// the frame's modal reindex has already run; caching the order at
    /// measurement time would freeze that control's pre-reindex (raw
    /// registration) position instead of its true tab-stop ordinal.
    pub(crate) fn snapshot_focus_order(&self, window: &Window) -> HashMap<String, u32> {
        self.snapshot_focus_order_id(window.window_handle().window_id().as_u64())
    }

    pub(super) fn snapshot_focus_order_id(&self, window_id: u64) -> HashMap<String, u32> {
        self.states
            .borrow()
            .get(&window_id)
            .map(|state| {
                state
                    .measured
                    .bounds
                    .keys()
                    .filter_map(|id| {
                        state
                            .tree
                            .get(id)
                            .map(|action| (id.clone(), action.focus_order() as u32))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns the action owner reconciled with the concrete GPUI focus
    /// observer during this frame. Before that observer runs, this may carry
    /// the frame's declared entry owner and must not be exported as native
    /// evidence.
    pub(crate) fn snapshot_focus_owner(&self, window: &Window) -> Option<String> {
        self.snapshot_focus_owner_id(window.window_handle().window_id().as_u64())
    }

    pub(super) fn snapshot_focus_owner_id(&self, window_id: u64) -> Option<String> {
        self.states
            .borrow()
            .get(&window_id)
            .and_then(|state| state.measured.focus_owner.clone())
    }

    /// Returns the owner proven by a concrete GPUI focus handle during the
    /// render pass. It is intentionally distinct from the metadata owner.
    pub(crate) fn snapshot_native_focus_owner(&self, window: &Window) -> Option<String> {
        self.snapshot_native_focus_owner_id(window.window_handle().window_id().as_u64())
    }

    pub(super) fn snapshot_native_focus_owner_id(&self, window_id: u64) -> Option<String> {
        self.states
            .borrow()
            .get(&window_id)
            .and_then(|state| state.measured.native_focus_owner.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    #[test]
    fn replacing_a_window_frame_replaces_all_post_layout_state_atomically() {
        let frames = ActionFrames::default();
        let first = frames.begin_id(11, "home");
        frames.register(
            first,
            ActionMetadata::new("open", "Open", super::super::semantic::ActionRole::Button),
        );
        frames.record_bounds(
            first,
            "open".into(),
            Bounds::new(point(px(2.0), px(3.0)), size(px(80.0), px(24.0))),
        );
        assert_eq!(frames.snapshot_id(11).len(), 1);
        assert_eq!(frames.snapshot_bounds_id(11).len(), 1);

        let second = frames.begin_id(11, "settings");
        assert_ne!(first.revision, second.revision);
        assert_eq!(frames.snapshot_id(11).len(), 0);
        assert!(frames.snapshot_bounds_id(11).is_empty());
        assert!(frames.snapshot_focus_order_id(11).is_empty());
        assert!(frames.snapshot_native_focus_owner_id(11).is_none());

        frames.states.borrow_mut().remove(&11);
        assert_eq!(frames.snapshot_id(11), ActionTree::default());
        assert!(frames.snapshot_bounds_id(11).is_empty());
    }
}
