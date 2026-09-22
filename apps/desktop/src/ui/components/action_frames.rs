use super::action_tree::ActionTree;
use super::semantic::{ActionMetadata, ActionState, SemanticBounds};
use gpui::{App, Bounds, FocusHandle, SharedString, Window};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Owns semantic snapshots for one application/window owner.
///
/// A workspace keeps one instance and threads it through its `Theme`. Adapter
/// calls therefore carry an explicit window frame even when a CE popup renders
/// after the root element has been built. There is no process-global lock or
/// thread-local "current window" that can misattribute a second window.
#[derive(Clone, Debug, Default)]
pub(crate) struct ActionFrames {
    frames: Rc<RefCell<HashMap<u64, ActionTree>>>,
    measured: Rc<RefCell<HashMap<u64, MeasuredActionFrame>>>,
    requested_focus: Rc<RefCell<HashMap<u64, String>>>,
    modal_restore: Rc<RefCell<HashMap<u64, String>>>,
    previous_native_focus: Rc<RefCell<HashMap<u64, String>>>,
    previous_pointer_states: Rc<RefCell<HashMap<u64, HashMap<String, (bool, bool)>>>>,
    pending_focus: Rc<RefCell<HashMap<u64, String>>>,
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
        if let Some(previous) = self
            .measured
            .borrow()
            .get(&window_id)
            .and_then(|frame| frame.native_focus_owner.clone())
        {
            self.previous_native_focus
                .borrow_mut()
                .insert(window_id, previous);
        } else {
            self.previous_native_focus.borrow_mut().remove(&window_id);
        }
        let pointer_states = self
            .frames
            .borrow()
            .get(&window_id)
            .map(|tree| {
                tree.iter()
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
        self.previous_pointer_states
            .borrow_mut()
            .insert(window_id, pointer_states);
        self.pending_focus.borrow_mut().remove(&window_id);
        let mut frames = self.frames.borrow_mut();
        let revision = frames
            .get(&window_id)
            .map_or(1, |frame| frame.revision().saturating_add(1));
        frames.insert(window_id, ActionTree::new(route.into(), revision));
        self.measured.borrow_mut().insert(
            window_id,
            MeasuredActionFrame {
                revision,
                bounds: HashMap::new(),
                focus_order: HashMap::new(),
                native_focus_owner: None,
                focus_owner: None,
            },
        );
        ActionFrameToken {
            window_id,
            revision,
        }
    }

    pub(crate) fn register(&self, token: ActionFrameToken, action: ActionMetadata) {
        if let Some(frame) = self
            .frames
            .borrow_mut()
            .get_mut(&token.window_id)
            .filter(|frame| frame.revision() == token.revision)
        {
            frame.registrar().record(action);
        }
    }

    pub(crate) fn snapshot(&self, window: &Window) -> ActionTree {
        self.frames
            .borrow()
            .get(&window.window_handle().window_id().as_u64())
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn reset(&self, window: &Window) {
        let window_id = window.window_handle().window_id().as_u64();
        self.frames.borrow_mut().remove(&window_id);
        self.measured.borrow_mut().remove(&window_id);
        self.requested_focus.borrow_mut().remove(&window_id);
        self.modal_restore.borrow_mut().remove(&window_id);
        self.previous_native_focus.borrow_mut().remove(&window_id);
        self.previous_pointer_states.borrow_mut().remove(&window_id);
        self.pending_focus.borrow_mut().remove(&window_id);
    }

    pub(crate) fn finalize(&self, window: &Window) {
        let window_id = window.window_handle().window_id().as_u64();
        if let Some(frame) = self.frames.borrow_mut().get_mut(&window_id) {
            frame.finalize_modal();
            if let Some(pointer_states) =
                self.previous_pointer_states.borrow_mut().remove(&window_id)
            {
                frame.restore_pointer_states(&pointer_states);
            }
            if let Some(restore) = self.modal_restore.borrow().get(&window_id) {
                if frame.modal_root().is_some() {
                    frame.set_restore_focus(restore.clone());
                }
            }
            let preferred = self
                .requested_focus
                .borrow_mut()
                .remove(&window_id)
                .or_else(|| self.previous_native_focus.borrow().get(&window_id).cloned());
            if frame.modal_root().is_none() {
                if let Some(preferred) = preferred {
                    frame.set_focus_owner(&preferred);
                }
            }
        }
    }

    pub(super) fn snapshot_id(&self, window_id: u64) -> ActionTree {
        self.frames
            .borrow()
            .get(&window_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn record_bounds(
        &self,
        token: ActionFrameToken,
        id: SharedString,
        bounds: Bounds<gpui::Pixels>,
    ) {
        let focus = self
            .frames
            .borrow()
            .get(&token.window_id)
            .filter(|tree| tree.revision() == token.revision)
            .and_then(|tree| tree.iter().find(|action| action.id() == &id))
            .map(|action| {
                (
                    action.is_focusable(),
                    action.focus_order(),
                    action.is_focused(),
                )
            });
        let Some(focus) = focus else {
            // A measurement for an unregistered element is stale or points at
            // the wrong semantic id. Never let it become capture evidence.
            return;
        };
        let mut measured = self.measured.borrow_mut();
        let Some(frame) = measured
            .get_mut(&token.window_id)
            .filter(|frame| frame.revision == token.revision)
        else {
            return;
        };
        let id = id.to_string();
        frame.bounds.insert(
            id.clone(),
            SemanticBounds::Logical {
                x: u32::from(bounds.origin.x),
                y: u32::from(bounds.origin.y),
                width: u32::from(bounds.size.width),
                height: u32::from(bounds.size.height),
            },
        );
        let (focusable, order, focused) = focus;
        if focusable {
            frame.focus_order.insert(id.clone(), order as u32);
            if focused {
                frame.focus_owner = Some(id);
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
        let action_state = self
            .frames
            .borrow()
            .get(&token.window_id)
            .filter(|tree| tree.revision() == token.revision)
            .and_then(|tree| tree.iter().find(|action| action.id() == &id))
            .map(|action| (action.is_focusable(), action.is_focused()));
        let Some((focusable, declared_focused)) = action_state else {
            return;
        };
        if !focusable {
            return;
        }
        let id = id.to_string();
        if focused {
            if let Some(tree) = self
                .frames
                .borrow_mut()
                .get_mut(&token.window_id)
                .filter(|tree| tree.revision() == token.revision)
            {
                tree.set_focus_owner(&id);
            }
            if let Some(frame) = self
                .measured
                .borrow_mut()
                .get_mut(&token.window_id)
                .filter(|frame| frame.revision == token.revision)
            {
                frame.native_focus_owner = Some(id.clone());
                frame.focus_owner = Some(id.clone());
            }
            self.pending_focus.borrow_mut().remove(&token.window_id);
            if self
                .requested_focus
                .borrow()
                .get(&token.window_id)
                .map_or(false, |requested| requested == &id)
            {
                self.requested_focus.borrow_mut().remove(&token.window_id);
            }
            return;
        }

        let requested_focus = self
            .requested_focus
            .borrow()
            .get(&token.window_id)
            .map_or(false, |requested| requested == &id);
        let should_focus = declared_focused || requested_focus;
        if let Some(frame) = self
            .measured
            .borrow_mut()
            .get_mut(&token.window_id)
            .filter(|frame| frame.revision == token.revision)
        {
            if frame.native_focus_owner.as_deref() == Some(id.as_str()) {
                frame.native_focus_owner = None;
            }
        }
        if should_focus
            && self
                .pending_focus
                .borrow()
                .get(&token.window_id)
                .map_or(true, |pending| pending != &id)
        {
            self.pending_focus.borrow_mut().insert(token.window_id, id);
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
        if let Some(tree) = self
            .frames
            .borrow_mut()
            .get_mut(&token.window_id)
            .filter(|tree| tree.revision() == token.revision)
        {
            tree.set_pointer_state(id.as_ref(), state, active);
        }
    }

    /// Requests focus restoration to a stable action id on the next concrete
    /// render of that control. The actual FocusHandle is supplied by that
    /// control's observer, so restoration never relies on a synthetic handle.
    pub(crate) fn request_focus(&self, window: &Window, id: impl Into<String>) {
        self.requested_focus
            .borrow_mut()
            .insert(window.window_handle().window_id().as_u64(), id.into());
    }

    pub(crate) fn remember_modal_restore(&self, window: &Window) {
        let window_id = window.window_handle().window_id().as_u64();
        if let Some(owner) = self
            .measured
            .borrow()
            .get(&window_id)
            .and_then(|frame| frame.native_focus_owner.clone())
        {
            self.modal_restore.borrow_mut().insert(window_id, owner);
        }
    }

    pub(crate) fn request_modal_restore(&self, window: &Window) {
        let window_id = window.window_handle().window_id().as_u64();
        if let Some(owner) = self.modal_restore.borrow_mut().remove(&window_id) {
            self.requested_focus.borrow_mut().insert(window_id, owner);
        }
    }

    pub(crate) fn snapshot_bounds(&self, window: &Window) -> HashMap<String, SemanticBounds> {
        self.snapshot_bounds_id(window.window_handle().window_id().as_u64())
    }

    pub(super) fn snapshot_bounds_id(&self, window_id: u64) -> HashMap<String, SemanticBounds> {
        self.measured
            .borrow()
            .get(&window_id)
            .map(|frame| frame.bounds.clone())
            .unwrap_or_default()
    }

    /// Returns focus order proven by the same measured controls that supplied
    /// the rectangles. A metadata-only action never enters this map.
    pub(crate) fn snapshot_focus_order(&self, window: &Window) -> HashMap<String, u32> {
        self.snapshot_focus_order_id(window.window_handle().window_id().as_u64())
    }

    pub(super) fn snapshot_focus_order_id(&self, window_id: u64) -> HashMap<String, u32> {
        self.measured
            .borrow()
            .get(&window_id)
            .map(|frame| frame.focus_order.clone())
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
        self.measured
            .borrow()
            .get(&window_id)
            .and_then(|frame| frame.focus_owner.clone())
    }

    /// Returns the owner proven by a concrete GPUI focus handle during the
    /// render pass. It is intentionally distinct from the metadata owner.
    pub(crate) fn snapshot_native_focus_owner(&self, window: &Window) -> Option<String> {
        self.snapshot_native_focus_owner_id(window.window_handle().window_id().as_u64())
    }

    pub(super) fn snapshot_native_focus_owner_id(&self, window_id: u64) -> Option<String> {
        self.measured
            .borrow()
            .get(&window_id)
            .and_then(|frame| frame.native_focus_owner.clone())
    }
}
