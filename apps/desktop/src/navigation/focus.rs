//! Typed focus scopes, keyboard traversal, and route restoration.
//!
//! GPUI owns the native focus handles. This model owns the semantic identity
//! those handles are keyed by, which keeps tab order, modal trapping, Escape,
//! and restoration deterministic in both a live window and a screenshot run.

use super::action::ActionId;
use gpui::SharedString;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Collision-free semantic identity for one rendered action.
///
/// Action strings are interned by [`FocusTree`] rather than hashed into the
/// route identity. The monotonically allocated key remains stable while the
/// action is active across reordered frames, and the reverse map lets native
/// focus and Escape restoration resolve through the same table.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ActionKey(u64);

impl ActionKey {
    /// Creates a non-zero key for focused-action tests and persisted routes.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the monotonic key value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug)]
struct ActionInterner {
    by_id: HashMap<SharedString, ActionKey>,
    by_key: HashMap<ActionKey, SharedString>,
    next: u64,
}

impl Default for ActionInterner {
    fn default() -> Self {
        Self {
            by_id: HashMap::new(),
            by_key: HashMap::new(),
            next: 1,
        }
    }
}

impl ActionInterner {
    fn intern(&mut self, id: &str) -> ActionKey {
        if let Some(key) = self.by_id.get(id).copied() {
            return key;
        }
        let id: SharedString = id.into();
        let key = ActionKey::new(self.next).expect("interner key is non-zero");
        self.next = self
            .next
            .checked_add(1)
            .expect("action interner exhausted its monotonic key space");
        debug_assert!(!self.by_key.contains_key(&key));
        self.by_id.insert(id.clone(), key);
        self.by_key.insert(key, id);
        key
    }

    fn prune(&mut self, keep: &BTreeSet<ActionKey>) {
        let Self { by_id, by_key, .. } = self;
        by_key.retain(|key, id| {
            let retained = keep.contains(key);
            if !retained {
                by_id.remove(id);
            }
            retained
        });
    }
}

/// Stable focus node IDs.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FocusId {
    /// Window shell.
    Shell,
    /// Orbit navigation.
    Orbit,
    /// Package navigation.
    Package,
    /// Page content.
    Page,
    /// Source content.
    Source,
    /// Shelf list.
    Shelf,
    /// Context panel.
    Context,
    /// Command palette.
    CommandPalette,
    /// One rendered action from the post-layout action registry.
    Action(ActionKey),
    /// One modal dialog.
    Modal(u16),
}

/// Why the current focus was changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusOrigin {
    /// The user moved focus with Tab, an arrow key, or another keyboard action.
    Keyboard,
    /// A pointer or touch event moved focus.
    Pointer,
    /// Product state or modal restoration moved focus.
    Programmatic,
}

/// A focus route from shell root to the active target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusRoute {
    /// Ordered ancestors and active target.
    pub nodes: Vec<FocusId>,
}

impl FocusRoute {
    /// Creates a route, rejecting empty paths.
    #[must_use]
    pub fn new(nodes: impl IntoIterator<Item = FocusId>) -> Option<Self> {
        let nodes = nodes.into_iter().collect::<Vec<_>>();
        (!nodes.is_empty()).then_some(Self { nodes })
    }

    /// Returns the active target.
    #[must_use]
    pub fn active(&self) -> FocusId {
        self.nodes.last().copied().unwrap_or(FocusId::Shell)
    }
}

/// One focus node and its semantic navigation metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusNode {
    /// Stable node ID.
    pub id: FocusId,
    /// Parent in the focus tree.
    pub parent: Option<FocusId>,
    /// Child order.
    pub children: Vec<FocusId>,
    /// Actions accepted by the node.
    pub actions: Vec<ActionId>,
}

#[derive(Clone, Debug)]
struct FocusRestore {
    route: FocusRoute,
    origin: FocusOrigin,
    focus_visible: bool,
}

/// Result of handling Escape at the current focus scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EscapeResult {
    /// A modal or command surface was dismissed and focus was restored.
    Dismissed(FocusId),
    /// The current scope consumed Escape without changing focus.
    Consumed,
    /// No transient scope owned Escape.
    Ignored,
}

/// The UI-thread focus tree. It stores only handles/IDs, never widget data.
#[derive(Clone, Debug)]
pub struct FocusTree {
    nodes: BTreeMap<FocusId, FocusNode>,
    action_interner: ActionInterner,
    current: FocusRoute,
    restore: Vec<FocusRestore>,
    tab_order: Vec<FocusId>,
    origin: FocusOrigin,
    focus_visible: bool,
    window_focused: bool,
}

impl Default for FocusTree {
    fn default() -> Self {
        let mut tree = Self {
            nodes: BTreeMap::new(),
            action_interner: ActionInterner::default(),
            current: FocusRoute {
                nodes: vec![FocusId::Shell],
            },
            restore: Vec::new(),
            tab_order: Vec::new(),
            origin: FocusOrigin::Programmatic,
            focus_visible: true,
            window_focused: true,
        };
        tree.register(FocusNode {
            id: FocusId::Shell,
            parent: None,
            children: vec![
                FocusId::Orbit,
                FocusId::Shelf,
                FocusId::Context,
                FocusId::CommandPalette,
            ],
            actions: ActionId::ALL.to_vec(),
        });
        tree.register(FocusNode {
            id: FocusId::Orbit,
            parent: Some(FocusId::Shell),
            children: vec![FocusId::Package],
            actions: ActionId::ALL.to_vec(),
        });
        tree.register(FocusNode {
            id: FocusId::Package,
            parent: Some(FocusId::Orbit),
            children: vec![FocusId::Page],
            actions: ActionId::ALL.to_vec(),
        });
        tree.register(FocusNode {
            id: FocusId::Page,
            parent: Some(FocusId::Package),
            children: vec![FocusId::Source],
            actions: ActionId::ALL.to_vec(),
        });
        tree.register(FocusNode {
            id: FocusId::Source,
            parent: Some(FocusId::Page),
            children: Vec::new(),
            actions: ActionId::ALL.to_vec(),
        });
        for id in [FocusId::Shelf, FocusId::Context, FocusId::CommandPalette] {
            tree.register(FocusNode {
                id,
                parent: Some(FocusId::Shell),
                children: Vec::new(),
                actions: ActionId::ALL.to_vec(),
            });
        }
        for id in [
            FocusId::Modal(1),
            FocusId::Modal(2),
            FocusId::Modal(3),
            FocusId::Modal(4),
        ] {
            tree.register(FocusNode {
                id,
                parent: Some(FocusId::Shell),
                children: Vec::new(),
                actions: vec![ActionId::DismissOverlay, ActionId::Stop],
            });
        }
        tree.set_tab_order([
            FocusId::Orbit,
            FocusId::Shelf,
            FocusId::Context,
            FocusId::CommandPalette,
        ]);
        tree
    }
}

impl FocusTree {
    /// Registers a focus node and its parent link.
    pub fn register(&mut self, node: FocusNode) {
        self.nodes.insert(node.id, node);
    }

    /// Removes a node and repairs any active/restoration route that references it.
    ///
    /// This is used when a collapsible pane or overlay unmounts. A removed
    /// focused control falls back to the nearest surviving ancestor, so a stale
    /// native GPUI handle is never retained for the next keyboard event.
    pub fn unregister(&mut self, id: FocusId) -> bool {
        if id == FocusId::Shell || self.nodes.remove(&id).is_none() {
            return false;
        }
        self.tab_order.retain(|candidate| *candidate != id);
        let restore = std::mem::take(&mut self.restore);
        self.restore = restore
            .into_iter()
            .filter_map(|frame| {
                self.repair_route(frame.route.clone())
                    .map(|route| FocusRestore {
                        route,
                        origin: frame.origin,
                        focus_visible: frame.focus_visible,
                    })
            })
            .collect();
        self.current = self
            .repair_route(self.current.clone())
            .unwrap_or_else(|| FocusRoute {
                nodes: vec![FocusId::Shell],
            });
        true
    }

    /// Returns the current focus route.
    #[must_use]
    pub fn route(&self) -> &FocusRoute {
        &self.current
    }

    /// Returns the semantic origin of the current focus.
    #[must_use]
    pub const fn origin(&self) -> FocusOrigin {
        self.origin
    }

    /// Returns whether a keyboard-visible focus ring should be painted.
    #[must_use]
    pub const fn focus_visible(&self) -> bool {
        self.focus_visible
    }

    /// Returns whether the containing native window currently owns focus.
    #[must_use]
    pub const fn window_focused(&self) -> bool {
        self.window_focused
    }

    /// Applies a native window focus transition without losing the semantic route.
    pub fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
        self.focus_visible = focused
            && matches!(
                self.origin,
                FocusOrigin::Keyboard | FocusOrigin::Programmatic
            );
    }

    /// Returns the ordered focusable IDs used by Tab traversal.
    #[must_use]
    pub fn tab_order(&self) -> &[FocusId] {
        &self.tab_order
    }

    /// Returns the overlay owner of the current native focus route.
    ///
    /// A rendered action is a child of its modal node, so checking only the
    /// active ID would miss the trap as soon as Tab enters the first control.
    /// The first modal after the shell owns Escape; later modal IDs can be
    /// nested focus containers within that overlay.
    /// Keeping this query on the one semantic focus owner also lets overlay
    /// transitions repair a stale stack without introducing a second modal
    /// authority in the view layer.
    #[must_use]
    pub fn active_modal(&self) -> Option<FocusId> {
        self.current
            .nodes
            .iter()
            .find(|id| matches!(id, FocusId::Modal(_)))
            .copied()
    }

    /// Replaces Tab order, filtering unknown or duplicate nodes.
    pub fn set_tab_order(&mut self, order: impl IntoIterator<Item = FocusId>) {
        let mut seen = BTreeSet::new();
        self.tab_order = order
            .into_iter()
            .filter(|id| *id != FocusId::Shell)
            .filter(|id| self.nodes.contains_key(id))
            .filter(|id| seen.insert(*id))
            .collect();
    }

    /// Interns one rendered action ID into the stable focus route namespace.
    ///
    /// The same table serves frame reordering, native focus reconciliation,
    /// and modal restoration; no one-way hash is used as an identity.
    pub fn action_key(&mut self, id: &str) -> ActionKey {
        self.action_interner.intern(id)
    }

    /// Rebuilds dynamic focus nodes from the rendered action registry.
    ///
    /// The action registry is the source of truth for enabled/visible
    /// controls. This model only retains typed routes and modal restoration;
    /// it never invents a parallel list of labels or bounds. IDs are interned
    /// before the old dynamic nodes are removed, so a reordered frame retains
    /// exactly the same typed keys.
    pub fn sync_action_order(
        &mut self,
        shell_ids: &[SharedString],
        modal: Option<(FocusId, &[SharedString])>,
    ) {
        let shell_keys = shell_ids
            .iter()
            .map(|id| self.action_interner.intern(id.as_ref()))
            .collect::<Vec<_>>();
        let (modal_id, modal_keys) = match modal {
            Some((id, ids)) => (
                Some(id),
                ids.iter()
                    .map(|action| self.action_interner.intern(action.as_ref()))
                    .collect::<Vec<_>>(),
            ),
            None => (None, Vec::new()),
        };
        let current = self.current.clone();
        let restore = std::mem::take(&mut self.restore);
        let dynamic = self
            .nodes
            .keys()
            .copied()
            .filter(|id| matches!(id, FocusId::Action(_)))
            .collect::<Vec<_>>();
        for id in dynamic {
            self.nodes.remove(&id);
            self.tab_order.retain(|candidate| *candidate != id);
        }

        let register = |tree: &mut FocusTree, id: FocusId, parent: FocusId| {
            tree.register(FocusNode {
                id,
                parent: Some(parent),
                children: Vec::new(),
                actions: ActionId::ALL.to_vec(),
            });
        };
        for key in &shell_keys {
            register(self, FocusId::Action(*key), FocusId::Shell);
        }
        if let Some(modal_id) = modal_id {
            for key in &modal_keys {
                register(self, FocusId::Action(*key), modal_id);
            }
        }

        self.restore = restore
            .into_iter()
            .filter_map(|frame| {
                self.repair_route(frame.route.clone())
                    .map(|route| FocusRestore {
                        route,
                        origin: frame.origin,
                        focus_visible: frame.focus_visible,
                    })
            })
            .collect();
        self.current = self.repair_route(current).unwrap_or(FocusRoute {
            nodes: vec![FocusId::Shell],
        });
        self.tab_order = match modal_id {
            Some(_) => modal_keys.iter().copied().map(FocusId::Action).collect(),
            None => shell_keys.iter().copied().map(FocusId::Action).collect(),
        };

        let mut keep = shell_keys
            .into_iter()
            .chain(modal_keys)
            .collect::<BTreeSet<_>>();
        for id in &self.current.nodes {
            if let FocusId::Action(key) = id {
                keep.insert(*key);
            }
        }
        for frame in &self.restore {
            for id in &frame.route.nodes {
                if let FocusId::Action(key) = id {
                    keep.insert(*key);
                }
            }
        }
        self.action_interner.prune(&keep);
    }

    /// Projects native focus evidence from the rendered action frame.
    ///
    /// GPUI CE owns the actual `FocusHandle` and keyboard dispatch. When its
    /// post-layout action frame reports one focused control, this method keeps
    /// the semantic route in lockstep with that handle. It deliberately
    /// preserves the current origin and focus-ring policy: a native frame is
    /// evidence about *which* control is focused, not a new keyboard gesture.
    /// The action frame remains the only source of rendered control identity;
    /// this tree only stores the typed route needed by Escape and assertions.
    pub fn sync_native_action(&mut self, id: &str, modal: Option<FocusId>) -> bool {
        let key = self.action_interner.intern(id);
        let route = match modal {
            Some(modal) => FocusRoute::new([FocusId::Shell, modal, FocusId::Action(key)]),
            None => FocusRoute::new([FocusId::Shell, FocusId::Action(key)]),
        };
        let Some(route) = route else {
            return false;
        };
        if !self.valid_route(&route) {
            return false;
        }
        self.current = route;
        true
    }

    /// Moves focus when the target is a registered node.
    pub fn focus(&mut self, route: FocusRoute) -> bool {
        self.focus_with_origin(route, FocusOrigin::Programmatic)
    }

    /// Moves focus and records whether the transition came from keyboard or pointer input.
    pub fn focus_with_origin(&mut self, route: FocusRoute, origin: FocusOrigin) -> bool {
        if !self.valid_route(&route) {
            return false;
        }
        self.current = route;
        self.origin = origin;
        self.focus_visible = self.window_focused
            && matches!(origin, FocusOrigin::Keyboard | FocusOrigin::Programmatic);
        true
    }

    /// Records a pointer focus transition without requiring callers to rebuild a route.
    pub fn focus_pointer(&mut self, id: FocusId) -> bool {
        let Some(route) = self.route_to(id) else {
            return false;
        };
        self.focus_with_origin(route, FocusOrigin::Pointer)
    }

    /// Moves to the next focusable control, wrapping within the active scope.
    pub fn focus_next(&mut self) -> Option<FocusId> {
        self.focus_relative(1)
    }

    /// Moves to the previous focusable control, wrapping within the active scope.
    pub fn focus_previous(&mut self) -> Option<FocusId> {
        self.focus_relative(-1)
    }

    fn focus_relative(&mut self, direction: isize) -> Option<FocusId> {
        let candidates = self.scoped_tab_order();
        if candidates.is_empty() {
            return None;
        }
        let current = self.current.active();
        let index = candidates.iter().position(|id| *id == current);
        let next = match index {
            Some(index) => {
                let len = candidates.len() as isize;
                let next = (index as isize + direction).rem_euclid(len);
                candidates[next as usize]
            }
            None if direction >= 0 => candidates[0],
            None => *candidates.last().expect("non-empty focus candidates"),
        };
        let route = self.route_to(next)?;
        self.focus_with_origin(route, FocusOrigin::Keyboard)
            .then_some(next)
    }

    fn scoped_tab_order(&self) -> Vec<FocusId> {
        let scope = self.active_modal();
        self.tab_order
            .iter()
            .copied()
            .filter(|id| self.nodes.contains_key(id))
            .filter(|id| match scope {
                Some(scope) => self.is_descendant_of(*id, scope),
                None => true,
            })
            .collect()
    }

    /// Pushes a modal focus route and remembers the previous route.
    pub fn push_modal(&mut self, modal: FocusId) -> bool {
        self.push_modal_with_restore(modal, None)
    }

    /// Pushes a modal while honoring a restore target proven by the rendered
    /// ActionFrames. The native focus handle remains owned by the concrete
    /// GPUI control; this route only preserves the semantic launch identity.
    pub fn push_modal_with_restore(&mut self, modal: FocusId, restore: Option<FocusId>) -> bool {
        if !matches!(modal, FocusId::Modal(_)) || !self.nodes.contains_key(&modal) {
            return false;
        }
        let restore_route = restore
            .and_then(|id| self.route_to(id))
            .unwrap_or_else(|| self.current.clone());
        self.restore.push(FocusRestore {
            route: restore_route,
            origin: self.origin,
            focus_visible: self.focus_visible,
        });
        self.current = FocusRoute {
            nodes: vec![FocusId::Shell, modal],
        };
        true
    }

    /// Pops a modal and restores the exact route that launched it when possible.
    pub fn pop_modal(&mut self) -> bool {
        if !self
            .current
            .nodes
            .iter()
            .rev()
            .any(|id| matches!(id, FocusId::Modal(_)))
        {
            return false;
        }
        let Some(frame) = self.restore.pop().and_then(|frame| {
            self.repair_route(frame.route.clone())
                .map(|route| (frame, route))
        }) else {
            self.current = FocusRoute {
                nodes: vec![FocusId::Shell],
            };
            self.origin = FocusOrigin::Programmatic;
            self.focus_visible = self.window_focused;
            return true;
        };
        let (frame, route) = frame;
        self.current = route;
        self.origin = frame.origin;
        self.focus_visible = self.window_focused && frame.focus_visible;
        true
    }

    /// Handles Escape according to the transient-surface hierarchy.
    pub fn escape(&mut self) -> EscapeResult {
        if let Some(restored) = self.active_modal() {
            return self
                .pop_modal()
                .then_some(EscapeResult::Dismissed(restored))
                .unwrap_or(EscapeResult::Consumed);
        }
        if self.current.active() == FocusId::CommandPalette {
            let _ = self.focus_with_origin(
                FocusRoute {
                    nodes: vec![FocusId::Shell],
                },
                FocusOrigin::Programmatic,
            );
            return EscapeResult::Dismissed(FocusId::CommandPalette);
        }
        EscapeResult::Ignored
    }

    /// Returns whether an action is accepted by the active node.
    #[must_use]
    pub fn accepts(&self, action: ActionId) -> bool {
        self.nodes
            .get(&self.current.active())
            .is_some_and(|node| node.actions.contains(&action))
    }

    fn valid_route(&self, route: &FocusRoute) -> bool {
        let valid_root = route.nodes.first() == Some(&FocusId::Shell);
        let valid_nodes = route.nodes.iter().all(|id| self.nodes.contains_key(id));
        let valid_edges = route.nodes.windows(2).all(|edge| {
            let [parent, child] = edge else {
                return false;
            };
            self.nodes.get(child).and_then(|node| node.parent) == Some(*parent)
        });
        valid_root && valid_nodes && valid_edges
    }

    fn route_to(&self, id: FocusId) -> Option<FocusRoute> {
        if !self.nodes.contains_key(&id) {
            return None;
        }
        let mut nodes = Vec::new();
        let mut cursor = Some(id);
        while let Some(current) = cursor {
            nodes.push(current);
            cursor = self.nodes.get(&current).and_then(|node| node.parent);
            if nodes.len() > self.nodes.len() {
                return None;
            }
        }
        nodes.reverse();
        let route = FocusRoute { nodes };
        self.valid_route(&route).then_some(route)
    }

    fn is_descendant_of(&self, id: FocusId, ancestor: FocusId) -> bool {
        let mut cursor = Some(id);
        let mut steps = 0;
        while let Some(current) = cursor {
            if current == ancestor {
                return true;
            }
            cursor = self.nodes.get(&current).and_then(|node| node.parent);
            steps += 1;
            if steps > self.nodes.len() {
                return false;
            }
        }
        false
    }

    fn repair_route(&self, route: FocusRoute) -> Option<FocusRoute> {
        let mut nodes = route.nodes;
        while !nodes.is_empty() {
            let candidate = FocusRoute {
                nodes: nodes.clone(),
            };
            if self.valid_route(&candidate) {
                return Some(candidate);
            }
            nodes.pop();
        }
        self.nodes
            .contains_key(&FocusId::Shell)
            .then_some(FocusRoute {
                nodes: vec![FocusId::Shell],
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action_key(value: u64) -> ActionKey {
        ActionKey::new(value).expect("non-zero action key")
    }

    fn action_ids(ids: &[&'static str]) -> Vec<SharedString> {
        ids.iter().copied().map(Into::into).collect()
    }

    fn page_route() -> FocusRoute {
        FocusRoute::new([
            FocusId::Shell,
            FocusId::Orbit,
            FocusId::Package,
            FocusId::Page,
        ])
        .expect("route")
    }

    #[test]
    fn modal_focus_restores_the_previous_route() {
        let mut tree = FocusTree::default();
        tree.register(FocusNode {
            id: FocusId::Modal(1),
            parent: Some(FocusId::Shell),
            children: Vec::new(),
            actions: vec![ActionId::DismissOverlay],
        });
        assert!(tree.focus(page_route()));
        assert!(tree.push_modal(FocusId::Modal(1)));
        assert_eq!(tree.route().active(), FocusId::Modal(1));
        assert!(tree.pop_modal());
        assert_eq!(tree.route().active(), FocusId::Page);
    }

    #[test]
    fn focus_rejects_a_route_with_a_broken_parent_edge() {
        let mut tree = FocusTree::default();
        assert!(!tree.focus(FocusRoute::new([FocusId::Shell, FocusId::Page]).unwrap()));
    }

    #[test]
    fn tab_wraps_in_stable_order_and_marks_focus_visible() {
        let mut tree = FocusTree::default();
        assert_eq!(tree.focus_next(), Some(FocusId::Orbit));
        assert_eq!(tree.origin(), FocusOrigin::Keyboard);
        assert!(tree.focus_visible());
        assert_eq!(tree.focus_next(), Some(FocusId::Shelf));
        assert_eq!(tree.focus_previous(), Some(FocusId::Orbit));
        assert_eq!(tree.focus_previous(), Some(FocusId::CommandPalette));
    }

    #[test]
    fn pointer_focus_hides_keyboard_ring_until_keyboard_moves_again() {
        let mut tree = FocusTree::default();
        assert!(tree.focus_pointer(FocusId::Shelf));
        assert_eq!(tree.origin(), FocusOrigin::Pointer);
        assert!(!tree.focus_visible());
        let _ = tree.focus_next();
        assert_eq!(tree.origin(), FocusOrigin::Keyboard);
        assert!(tree.focus_visible());
    }

    #[test]
    fn lost_window_focus_preserves_route_and_restores_keyboard_ring() {
        let mut tree = FocusTree::default();
        assert!(tree.focus_pointer(FocusId::Shelf));
        tree.set_window_focused(false);
        assert!(!tree.window_focused());
        assert!(!tree.focus_visible());
        assert_eq!(tree.route().active(), FocusId::Shelf);
        let route = tree.route().clone();
        tree.focus_with_origin(route, FocusOrigin::Keyboard);
        assert!(!tree.focus_visible());
        tree.set_window_focused(true);
        assert!(tree.focus_visible());
        assert_eq!(tree.route().active(), FocusId::Shelf);
    }

    #[test]
    fn modal_tab_order_is_trapped_and_escape_restores_focus() {
        let mut tree = FocusTree::default();
        tree.register(FocusNode {
            id: FocusId::Modal(9),
            parent: Some(FocusId::Shell),
            children: vec![FocusId::Modal(10)],
            actions: vec![ActionId::DismissOverlay],
        });
        tree.register(FocusNode {
            id: FocusId::Modal(10),
            parent: Some(FocusId::Modal(9)),
            children: Vec::new(),
            actions: vec![ActionId::DismissOverlay],
        });
        tree.set_tab_order([FocusId::Shelf, FocusId::Modal(10)]);
        assert!(tree.focus(FocusRoute::new([FocusId::Shell, FocusId::Shelf]).unwrap()));
        assert!(tree.push_modal(FocusId::Modal(9)));
        assert_eq!(tree.active_modal(), Some(FocusId::Modal(9)));
        assert_eq!(tree.focus_next(), Some(FocusId::Modal(10)));
        assert_eq!(tree.focus_next(), Some(FocusId::Modal(10)));
        assert_eq!(tree.escape(), EscapeResult::Dismissed(FocusId::Modal(9)));
        assert_eq!(tree.route().active(), FocusId::Shelf);
    }

    #[test]
    fn unregister_repairs_focus_and_restoration_routes() {
        let mut tree = FocusTree::default();
        assert!(tree.focus(FocusRoute::new([FocusId::Shell, FocusId::Shelf]).unwrap()));
        assert!(tree.push_modal(FocusId::Modal(1)));
        assert!(tree.unregister(FocusId::Shelf));
        assert!(tree.pop_modal());
        assert_eq!(tree.route().active(), FocusId::Shell);
        assert!(!tree.tab_order().contains(&FocusId::Shelf));
    }

    #[test]
    fn command_palette_escape_returns_to_shell() {
        let mut tree = FocusTree::default();
        assert!(tree.focus(FocusRoute::new([FocusId::Shell, FocusId::CommandPalette]).unwrap()));
        assert_eq!(
            tree.escape(),
            EscapeResult::Dismissed(FocusId::CommandPalette)
        );
        assert_eq!(tree.route().active(), FocusId::Shell);
    }

    #[test]
    fn rendered_action_order_traps_a_modal_and_restores_the_exact_action() {
        let mut tree = FocusTree::default();
        tree.sync_action_order(
            &action_ids(&["101", "202"]),
            Some((FocusId::Modal(1), &action_ids(&["303", "404"]))),
        );
        assert!(
            tree.focus(FocusRoute::new([FocusId::Shell, FocusId::Action(action_key(2))]).unwrap())
        );
        assert!(tree.push_modal(FocusId::Modal(1)));
        tree.sync_action_order(
            &action_ids(&["101", "202"]),
            Some((FocusId::Modal(1), &action_ids(&["303", "404"]))),
        );
        assert_eq!(tree.focus_next(), Some(FocusId::Action(action_key(3))));
        assert_eq!(tree.focus_next(), Some(FocusId::Action(action_key(4))));
        assert_eq!(tree.focus_next(), Some(FocusId::Action(action_key(3))));
        assert_eq!(tree.escape(), EscapeResult::Dismissed(FocusId::Modal(1)));
        assert_eq!(tree.route().active(), FocusId::Action(action_key(2)));
    }

    #[test]
    fn action_frame_restore_id_overrides_stale_semantic_current_route() {
        let mut tree = FocusTree::default();
        tree.sync_action_order(
            &action_ids(&["101", "202"]),
            Some((FocusId::Modal(1), &action_ids(&["303"]))),
        );
        assert!(
            tree.focus(FocusRoute::new([FocusId::Shell, FocusId::Action(action_key(2))]).unwrap())
        );
        assert!(
            tree.push_modal_with_restore(FocusId::Modal(1), Some(FocusId::Action(action_key(1))),)
        );
        tree.sync_action_order(
            &action_ids(&["101", "202"]),
            Some((FocusId::Modal(1), &action_ids(&["303"]))),
        );
        assert_eq!(tree.focus_next(), Some(FocusId::Action(action_key(3))));
        assert_eq!(tree.escape(), EscapeResult::Dismissed(FocusId::Modal(1)));
        assert_eq!(tree.route().active(), FocusId::Action(action_key(1)));
    }

    #[test]
    fn disappearing_rendered_action_repairs_to_its_surviving_scope() {
        let mut tree = FocusTree::default();
        tree.sync_action_order(&action_ids(&["101", "202"]), None);
        assert!(
            tree.focus(FocusRoute::new([FocusId::Shell, FocusId::Action(action_key(2))]).unwrap())
        );
        tree.sync_action_order(&action_ids(&["101"]), None);
        assert_eq!(tree.route().active(), FocusId::Shell);
        assert_eq!(tree.focus_next(), Some(FocusId::Action(action_key(1))));
    }

    #[test]
    fn native_action_projection_keeps_origin_and_enters_the_active_modal_scope() {
        let mut tree = FocusTree::default();
        tree.sync_action_order(
            &action_ids(&["101"]),
            Some((FocusId::Modal(1), &action_ids(&["202"]))),
        );
        assert!(
            tree.focus(FocusRoute::new([FocusId::Shell, FocusId::Action(action_key(1))]).unwrap())
        );
        assert!(tree.focus_pointer(FocusId::Action(action_key(1))));
        assert!(!tree.focus_visible());

        assert!(tree.push_modal(FocusId::Modal(1)));
        assert!(tree.sync_native_action("202", Some(FocusId::Modal(1))));
        assert_eq!(
            tree.route(),
            &FocusRoute::new([
                FocusId::Shell,
                FocusId::Modal(1),
                FocusId::Action(action_key(2)),
            ])
            .unwrap()
        );
        assert_eq!(tree.origin(), FocusOrigin::Pointer);
        assert!(!tree.focus_visible());
    }

    #[test]
    fn action_interner_keeps_distinct_ids_stable_across_reordered_frames() {
        let mut tree = FocusTree::default();
        assert!(ActionKey::new(0).is_none());
        let first = action_ids(&["Aa"]);
        let second = action_ids(&["BB"]);
        tree.sync_action_order(&first, Some((FocusId::Modal(1), &second)));
        let aa = tree.action_key("Aa");
        let bb = tree.action_key("BB");
        assert_ne!(aa, bb);

        let reordered_shell = action_ids(&["BB", "Aa"]);
        tree.sync_action_order(&reordered_shell, None);
        assert_eq!(tree.action_key("Aa"), aa);
        assert_eq!(tree.action_key("BB"), bb);
        assert_eq!(
            tree.tab_order(),
            &[FocusId::Action(bb), FocusId::Action(aa)]
        );
    }

    #[test]
    fn action_interner_prunes_only_unreferenced_routes() {
        let mut tree = FocusTree::default();
        let shell = action_ids(&["launch"]);
        tree.sync_action_order(&shell, Some((FocusId::Modal(1), &action_ids(&["confirm"]))));
        let launch = tree.action_key("launch");
        let confirm = tree.action_key("confirm");
        assert!(tree.focus(FocusRoute::new([FocusId::Shell, FocusId::Action(launch)]).unwrap()));
        assert!(tree.push_modal_with_restore(FocusId::Modal(1), Some(FocusId::Action(launch)),));
        tree.sync_action_order(&shell, Some((FocusId::Modal(1), &action_ids(&["confirm"]))));
        assert_eq!(tree.action_key("launch"), launch);
        assert_eq!(tree.action_key("confirm"), confirm);

        tree.sync_action_order(&action_ids(&["replacement"]), None);
        let replacement = tree.action_key("replacement");
        assert_ne!(replacement, launch);
        assert_ne!(replacement, confirm);
    }
}
