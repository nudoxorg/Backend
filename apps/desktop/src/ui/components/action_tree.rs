use super::semantic::{ActionMetadata, ActionRole, ActionState};
use gpui::SharedString;
use std::collections::HashMap;

/// A deterministic semantic action tree for journeys, accessibility, and
/// focus assertions. It deliberately retains disabled actions so a user can
/// understand why a capability is unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActionTree {
    actions: Vec<ActionMetadata>,
    route: SharedString,
    revision: u64,
    modal_root: Option<SharedString>,
    restore_focus: Option<SharedString>,
    focus_trap: bool,
}

impl Default for ActionTree {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
            route: SharedString::new_static("unbound"),
            revision: 0,
            modal_root: None,
            restore_focus: None,
            focus_trap: false,
        }
    }
}

impl ActionTree {
    pub(super) fn new(route: SharedString, revision: u64) -> Self {
        Self {
            actions: Vec::new(),
            route,
            revision,
            modal_root: None,
            restore_focus: None,
            focus_trap: false,
        }
    }

    fn register(&mut self, action: ActionMetadata) -> bool {
        if action.id.is_empty() || action.label.is_empty() {
            return false;
        }
        if self.actions.iter().any(|current| current.id == action.id) {
            return false;
        }
        let mut action = action;
        action.focus_order = self.actions.len();
        self.actions.push(action);
        true
    }

    /// Starts a registration scope for rendered CE components.
    ///
    /// Routes should build their semantic tree through this scope while they
    /// instantiate the matching component. This keeps focus metadata next to
    /// the real control and makes a stale, hand-maintained action list
    /// impossible to mistake for the rendered UI.
    pub(crate) fn registrar(&mut self) -> ActionRegistrar<'_> {
        ActionRegistrar { tree: self }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &ActionMetadata> {
        self.actions.iter()
    }

    pub(crate) fn focusable(&self) -> impl Iterator<Item = &ActionMetadata> {
        self.actions.iter().filter(|action| action.is_focusable())
    }

    pub(crate) fn len(&self) -> usize {
        self.actions.len()
    }

    /// Finalises modal ownership after the complete shell has rendered.
    ///
    /// Overlay components render after the page, so the first dialog action
    /// is a stable boundary: earlier actions remain in the tree for screen
    /// readers but become inert while the dialog owns keyboard focus. The
    /// saved page action lets the native focus owner return to the exact
    /// launch context when the overlay closes.
    pub(crate) fn finalize_modal(&mut self) {
        let Some(dialog_index) = self
            .actions
            .iter()
            .position(|action| action.role == ActionRole::Dialog && action.visible)
        else {
            self.modal_root = None;
            self.restore_focus = None;
            self.focus_trap = false;
            self.reindex_focusable();
            self.ensure_focus_owner();
            return;
        };
        let modal_root = self.actions[dialog_index].id.clone();
        self.modal_root = Some(modal_root);
        self.focus_trap = true;
        self.restore_focus = self.actions[..dialog_index]
            .iter()
            .find(|action| action.is_focusable())
            .map(|action| action.id.clone());
        for (index, action) in self.actions.iter_mut().enumerate() {
            if index < dialog_index {
                *action = action.clone().focused(false).inert(true);
            }
        }
        for action in self.actions.iter_mut().skip(dialog_index) {
            *action = action.clone().focused(false);
        }
        if let Some(action) = self
            .actions
            .iter_mut()
            .skip(dialog_index)
            .find(|action| action.is_focusable())
        {
            *action = action.clone().focused(true);
        }
        self.reindex_focusable();
        self.ensure_focus_owner();
    }

    fn reindex_focusable(&mut self) {
        let mut focus_order = 0;
        for action in &mut self.actions {
            if action.is_focusable() {
                action.focus_order = focus_order;
                focus_order = focus_order.saturating_add(1);
            } else {
                action.focus_order = 0;
            }
        }
    }

    /// Gives a freshly rendered frame a deterministic keyboard entry point.
    ///
    /// GPUI may not have delivered a native pointer or key event yet when a
    /// headless capture observes the first frame. Keeping one semantic focus
    /// owner makes Tab traversal and screen-reader output useful from that
    /// frame while preserving the actual GPUI focus handle for subsequent
    /// events.
    fn ensure_focus_owner(&mut self) {
        let mut owner_seen = false;
        for action in &mut self.actions {
            if !action.is_focusable() {
                if action.is_focused() {
                    *action = action.clone().focused(false);
                }
            } else if action.is_focused() {
                if owner_seen {
                    *action = action.clone().focused(false);
                } else {
                    owner_seen = true;
                }
            }
        }
        if owner_seen {
            return;
        }
        if let Some(action) = self.actions.iter_mut().find(|action| action.is_focusable()) {
            *action = action.clone().focused(true);
        }
    }

    /// Reconciles the declared owner with the focus handle observed during the
    /// current frame. Native GPUI focus is authoritative once it exists.
    pub(super) fn set_focus_owner(&mut self, id: &str) -> bool {
        let Some(index) = self
            .actions
            .iter()
            .position(|action| action.id().as_ref() == id && action.is_focusable())
        else {
            return false;
        };
        for (current_index, action) in self.actions.iter_mut().enumerate() {
            *action = action.clone().focused(current_index == index);
        }
        true
    }

    pub(super) fn set_pointer_state(&mut self, id: &str, state: ActionState, active: bool) -> bool {
        let Some(action) = self
            .actions
            .iter_mut()
            .find(|action| action.id().as_ref() == id)
        else {
            return false;
        };
        *action = match state {
            ActionState::Hovered => action.clone().hovered(active),
            ActionState::Pressed => action.clone().pressed(active),
            _ => action.clone(),
        };
        true
    }

    pub(super) fn restore_pointer_states(&mut self, states: &HashMap<String, (bool, bool)>) {
        for (id, (hovered, pressed)) in states {
            self.set_pointer_state(id, ActionState::Hovered, *hovered);
            self.set_pointer_state(id, ActionState::Pressed, *pressed);
        }
    }

    pub(super) fn set_restore_focus(&mut self, id: impl Into<SharedString>) {
        self.restore_focus = Some(id.into());
    }

    /// Returns the active modal root, when one owns focus.
    pub(crate) fn modal_root(&self) -> Option<&SharedString> {
        self.modal_root.as_ref()
    }

    /// Returns the focus target that will be restored after dismissal.
    pub(crate) fn restore_focus(&self) -> Option<&SharedString> {
        self.restore_focus.as_ref()
    }

    /// Returns whether keyboard focus is trapped in an overlay.
    pub(crate) const fn focus_trap(&self) -> bool {
        self.focus_trap
    }

    /// Returns the route identifier associated with this published frame.
    pub(crate) fn route(&self) -> &SharedString {
        &self.route
    }

    /// Returns the monotonically increasing frame revision.
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the per-window render generation represented by this tree.
    pub(crate) const fn generation(&self) -> u64 {
        self.revision
    }
}

/// Registration boundary used while rendering component-backed controls.
///
/// The registrar deliberately returns the concrete GPUI CE element from every
/// builder. A route therefore records an action only in the same expression
/// that creates the focusable component; it does not maintain a second list
/// of labels or shortcuts.
pub(crate) struct ActionRegistrar<'a> {
    tree: &'a mut ActionTree,
}

impl ActionRegistrar<'_> {
    pub(super) fn record(&mut self, action: ActionMetadata) -> bool {
        self.tree.register(action)
    }
}
