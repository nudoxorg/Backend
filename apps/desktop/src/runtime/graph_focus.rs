//! Ephemeral graph selection. This is a measured selection in the current
//! indexed world, never a replacement navigation address or persisted history entry.

use crate::core::VersionedRoot;
use crate::model::AppSnapshot;
use crate::model::pages::{PackageRef, PageKey, SymbolRef};
use crate::navigation::{Route, View};
use crate::runtime::indexed_world::Origin;
use backend_library::DeclarationKind;
use std::sync::Arc;

/// The current graph selection, scoped to the exact mounted visit and producer
/// authority. `root` retains the observation metadata from publication.
#[derive(Clone, Debug)]
pub(crate) struct GraphFocus {
    pub visit: Route,
    pub root: VersionedRoot,
    pub node: u32,
    pub name: Arc<str>,
    pub package: Arc<str>,
    pub module: Arc<str>,
    pub kind: DeclarationKind,
    /// Provenance of the projection that supplied this focus. Fixtures keep
    /// their label through the same resolver without claiming owner data.
    pub origin: Origin,
    /// Exact coordinate admitted by this projection's identity join. Tests
    /// tag their owner as synthetic; production values come from the index.
    pub indexed: Option<(PackageRef, SymbolRef)>,
}

impl PartialEq for GraphFocus {
    fn eq(&self, other: &Self) -> bool {
        self.visit == other.visit
            && self.root.same_authority(other.root)
            && self.node == other.node
            && self.name == other.name
            && self.package == other.package
            && self.module == other.module
            && self.kind == other.kind
            && self.origin == other.origin
            && self.indexed == other.indexed
    }
}

impl Eq for GraphFocus {}

/// Whether the current graph visit has a selected node for a Page/Code
/// action. Its indexed coordinate is retained when known; the map's guarded
/// resolver still decides whether a source identity or exact search opens it.
/// A selected node always takes priority over the route that entered the
/// graph. While the optional projection has no selection, an exact typed
/// declaration still has its own Page and Code destinations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GraphViewEligibility {
    Covered,
    OutsideGraph,
    Selected { node: u32, indexed: Option<(PackageRef, SymbolRef)> },
    Declaration { package: PackageRef, symbol: SymbolRef },
    ChooseSymbol,
}

impl GraphViewEligibility {
    pub(crate) fn from_current(snapshot: &AppSnapshot, focus: Option<&GraphFocus>) -> Self {
        if snapshot.overlay().is_some() || snapshot.page_overlay().is_some() {
            return Self::Covered;
        }
        if !matches!(snapshot.route(), Route::World | Route::Symbol(crate::navigation::SymbolRoute { view: View::Graph, .. })) {
            return Self::OutsideGraph;
        }
        match focus.filter(|focus| focus.active(snapshot)) {
            Some(GraphFocus { node, indexed, .. }) => Self::Selected {
                node: *node,
                indexed: indexed.clone(),
            },
            None => match (
                super::store::route_package(snapshot.route()),
                super::store::route_symbol(snapshot.route()),
            ) {
                (Some(package), Some(symbol)) => Self::Declaration { package, symbol },
                _ => Self::ChooseSymbol,
            },
        }
    }

    pub(crate) const fn can_open(&self) -> bool {
        matches!(self, Self::Selected { .. } | Self::Declaration { .. })
    }

    pub(crate) const fn allows(&self, view: View) -> bool {
        !matches!(self, Self::Covered)
            && (matches!(view, View::Graph) || matches!(self, Self::OutsideGraph | Self::Selected { .. } | Self::Declaration { .. }))
    }

    pub(crate) const fn guidance(&self) -> Option<&'static str> {
        match self {
            Self::ChooseSymbol => Some("Choose a graph symbol to open Page or Code."),
            Self::Covered | Self::OutsideGraph | Self::Selected { .. } | Self::Declaration { .. } => None,
        }
    }

    pub(crate) const fn brief_guidance(&self) -> Option<&'static str> {
        match self {
            Self::ChooseSymbol => Some("Choose a graph symbol"),
            Self::Covered | Self::OutsideGraph | Self::Selected { .. } | Self::Declaration { .. } => None,
        }
    }
}

impl GraphFocus {
    pub(crate) fn active(&self, snapshot: &AppSnapshot) -> bool {
        snapshot.overlay().is_none()
            && snapshot.key().same_authority(self.root)
            && snapshot.route() == &self.visit
            && matches!(
                snapshot.route(),
                Route::World
                    | Route::Symbol(crate::navigation::SymbolRoute {
                        view: View::Graph,
                        ..
                    })
            )
    }

    pub(crate) fn caption_path(&self) -> String {
        format!("{} › {} · {}", self.package, self.module, self.origin.lower_label())
    }

    /// Plain provenance, deliberately without a navigation scheme.
    pub(crate) fn status(&self) -> String {
        format!(
            "{} · {}::{}::{}",
            self.origin.label(),
            self.package, self.module, self.name
        )
    }
}

/// The one visit-scoped notice, for every route (§15 ruling 1). A failure
/// that cannot move the page (an unresolved link, an unindexed hold, a
/// pinned card whose lookup finished while the graph was hidden) belongs to
/// this exact visible page authority, never to a future navigation: it clears the
/// moment the route changes, and a newer notice always replaces an older one
/// (`DataStore::set_notice`). Drawn in the foot, to the right of the hand's
/// marks (`shell::status`).
#[derive(Clone, Debug)]
pub(crate) struct Notice {
    pub visit: Route,
    pub root: VersionedRoot,
    pub message: Arc<str>,
    /// The page whose "Try again" the foot offers beside the message: for a
    /// notice that says the index could not start, asking for this page again
    /// starts the owner again (`DataStore::retry`). `None`: nothing to retry.
    pub retry: Option<PageKey>,
}

impl PartialEq for Notice {
    fn eq(&self, other: &Self) -> bool {
        self.visit == other.visit
            && self.root.same_authority(other.root)
            && self.message == other.message
            && self.retry == other.retry
    }
}

impl Eq for Notice {}

impl Notice {
    pub(crate) fn active(&self, snapshot: &AppSnapshot) -> bool {
        snapshot.overlay().is_none()
            && snapshot.route() == &self.visit
            && snapshot.key().same_authority(self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SessionState;

    fn root(tag: &str, epoch: u64, generation: u64, observation: u64) -> VersionedRoot {
        let digest = backend_library::view_state_root(&[("focus".to_owned(), tag.to_owned())]);
        VersionedRoot::from_revision(
            epoch,
            backend_library::Cursor::at(digest, generation),
            observation,
        )
    }

    #[test]
    fn focus_survives_observation_only_changes_but_not_authority_changes() {
        let captured = root("same", 3, 9, 1);
        let observed = root("same", 3, 9, 2);
        let focus = GraphFocus {
            visit: Route::World,
            root: captured,
            node: 1,
            name: Arc::from("Value"),
            package: Arc::from("fixture"),
            module: Arc::from("src/lib.rs"),
            kind: DeclarationKind::Struct,
            origin: Origin::IndexedOwner,
            indexed: None,
        };
        let snapshot = |key| {
            AppSnapshot::empty(key).with_session(SessionState {
                route: Route::World,
                ..SessionState::default()
            })
        };

        assert!(focus.active(&snapshot(observed)));
        assert_eq!(GraphViewEligibility::from_current(&snapshot(observed), None), GraphViewEligibility::ChooseSymbol);
        assert_eq!(GraphViewEligibility::from_current(&snapshot(observed), Some(&focus)),
            GraphViewEligibility::Selected { node: 1, indexed: None });
        assert_eq!(focus, GraphFocus { root: observed, ..focus.clone() });
        let changed = root("changed", 3, 9, 2);
        assert_ne!(focus, GraphFocus { root: changed, ..focus.clone() });
        let notice = Notice {
            visit: Route::World,
            root: captured,
            message: Arc::from("index unavailable"),
            retry: None,
        };
        assert_eq!(notice, Notice { root: observed, ..notice.clone() });
        assert_ne!(notice, Notice { root: changed, ..notice.clone() });
        assert!(!focus.active(&snapshot(changed)));
        assert_eq!(GraphViewEligibility::from_current(&snapshot(changed), Some(&focus)), GraphViewEligibility::ChooseSymbol,
            "an old-root selection cannot enable Page or Code");
        assert!(!focus.active(&snapshot(root("same", 4, 9, 2))));
        assert!(!focus.active(&snapshot(root("same", 3, 10, 2))));
    }

    #[test]
    fn an_exact_graph_declaration_remains_a_destination_until_a_node_is_selected() {
        let route = crate::shell::tests::view_route("RelationLabel", View::Graph);
        let snapshot = AppSnapshot::empty(root("declaration", 1, 2, 0)).with_session(SessionState {
            route: route.clone(),
            ..SessionState::default()
        });
        let declaration = GraphViewEligibility::Declaration {
            package: super::super::store::route_package(&route).expect("exact package"),
            symbol: super::super::store::route_symbol(&route).expect("exact declaration"),
        };
        assert_eq!(GraphViewEligibility::from_current(&snapshot, None), declaration);
        assert!(declaration.can_open());
        assert!(declaration.allows(View::Page));
        assert!(declaration.allows(View::Code));
        let selected = GraphFocus {
            visit: route,
            root: snapshot.key(),
            node: 4,
            name: Arc::from("another declaration"),
            package: Arc::from("fixture"),
            module: Arc::from("glyph.rs"),
            kind: DeclarationKind::Struct,
            origin: Origin::IndexedOwner,
            indexed: None,
        };
        assert_eq!(GraphViewEligibility::from_current(&snapshot, Some(&selected)),
            GraphViewEligibility::Selected { node: 4, indexed: None },
            "a selected node must never borrow the entry declaration's destination");
    }
}
