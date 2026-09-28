//! Ephemeral graph selection. This is a measured selection in the temporary
//! fixture, never a replacement navigation address or persisted history entry.

use crate::core::VersionedRoot;
use crate::model::AppSnapshot;
use crate::model::pages::{PackageRef, SymbolRef};
use crate::navigation::{Route, View};
use backend_library::DeclarationKind;
use std::sync::Arc;

/// The current graph selection, scoped to the exact mounted visit and root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GraphFocus {
    pub visit: Route,
    pub root: VersionedRoot,
    pub node: u32,
    pub name: Arc<str>,
    pub package: Arc<str>,
    pub module: Arc<str>,
    pub kind: DeclarationKind,
    /// Only an actual indexed coordinate admitted by the exact identity join.
    pub indexed: Option<(PackageRef, SymbolRef)>,
}

impl GraphFocus {
    pub(crate) fn active(&self, snapshot: &AppSnapshot) -> bool {
        snapshot.overlay().is_none()
            && snapshot.route().at().is_none()
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
        format!("{} › {} · graph fixture", self.package, self.module)
    }

    /// Plain fixture provenance, deliberately without a navigation scheme.
    pub(crate) fn status(&self) -> String {
        format!(
            "Graph fixture · {}::{}::{}",
            self.package, self.module, self.name
        )
    }
}

/// The one visit-scoped notice, for every route (§15 ruling 1). A failure
/// that cannot move the page (an unresolved link, an unindexed hold, a
/// pinned card whose lookup finished while the graph was hidden) belongs to
/// this exact visible page/root, never to a future navigation: it clears the
/// moment the route changes, and a newer notice always replaces an older one
/// (`DataStore::set_notice`). Drawn in the foot, to the right of the hand's
/// marks (`shell::status`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Notice {
    pub visit: Route,
    pub root: VersionedRoot,
    pub message: Arc<str>,
}
impl Notice {
    pub(crate) fn active(&self, snapshot: &AppSnapshot) -> bool {
        snapshot.overlay().is_none()
            && snapshot.route() == &self.visit
            && snapshot.key().same_authority(self.root)
    }
}
