//! Typed focus tree and route restoration.

use super::action::ActionId;
use std::collections::BTreeMap;

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
    /// One modal dialog.
    Modal(u16),
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

/// The UI-thread focus tree.  It stores only handles/IDs, never widget data.
#[derive(Clone, Debug)]
pub struct FocusTree {
    nodes: BTreeMap<FocusId, FocusNode>,
    current: FocusRoute,
    restore: Vec<FocusRoute>,
}

impl Default for FocusTree {
    fn default() -> Self {
        let mut tree = Self {
            nodes: BTreeMap::new(),
            current: FocusRoute {
                nodes: vec![FocusId::Shell],
            },
            restore: Vec::new(),
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
        tree
    }
}

impl FocusTree {
    /// Registers a focus node and its parent link.
    pub fn register(&mut self, node: FocusNode) {
        self.nodes.insert(node.id, node);
    }

    /// Returns the current focus route.
    #[must_use]
    pub fn route(&self) -> &FocusRoute {
        &self.current
    }

    /// Moves focus when the target is a registered node.
    pub fn focus(&mut self, route: FocusRoute) -> bool {
        let valid_root = route.nodes.first() == Some(&FocusId::Shell);
        let valid_nodes = route.nodes.iter().all(|id| self.nodes.contains_key(id));
        let valid_edges = route.nodes.windows(2).all(|edge| {
            let [parent, child] = edge else {
                return false;
            };
            self.nodes.get(child).and_then(|node| node.parent) == Some(*parent)
        });
        if valid_root && valid_nodes && valid_edges {
            self.current = route;
            true
        } else {
            false
        }
    }

    /// Pushes a modal focus route and remembers the previous route.
    pub fn push_modal(&mut self, modal: FocusId) -> bool {
        if !matches!(modal, FocusId::Modal(_)) || !self.nodes.contains_key(&modal) {
            return false;
        }
        self.restore.push(self.current.clone());
        self.current = FocusRoute {
            nodes: vec![FocusId::Shell, modal],
        };
        true
    }

    /// Pops a modal and restores the exact route that launched it.
    pub fn pop_modal(&mut self) -> bool {
        if !matches!(self.current.active(), FocusId::Modal(_)) {
            return false;
        }
        if let Some(route) = self.restore.pop() {
            self.current = route;
            true
        } else {
            false
        }
    }

    /// Returns whether an action is accepted by the active node.
    #[must_use]
    pub fn accepts(&self, action: ActionId) -> bool {
        self.nodes
            .get(&self.current.active())
            .is_some_and(|node| node.actions.contains(&action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_focus_restores_the_previous_route() {
        let mut tree = FocusTree::default();
        tree.register(FocusNode {
            id: FocusId::Modal(1),
            parent: Some(FocusId::Shell),
            children: Vec::new(),
            actions: vec![ActionId::DismissOverlay],
        });
        assert!(
            tree.focus(
                FocusRoute::new([
                    FocusId::Shell,
                    FocusId::Orbit,
                    FocusId::Package,
                    FocusId::Page,
                ])
                .unwrap()
            )
        );
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
}
