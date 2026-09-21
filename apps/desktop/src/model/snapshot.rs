//! The one immutable application read model.
//!
//! [`AppSnapshot`] is the only state value a view can observe.  A snapshot is
//! keyed by a certified [`VersionedRoot`] and owns its branches through
//! `Arc`s.  Replacing one branch copies only the small `SnapshotData` shell;
//! untouched shelf, document, settings, and session branches remain shared.

use crate::core::ids::{DocumentId, PackageId, ProjectId, ResourceIdentity, VersionedRoot};
use crate::core::state::Resource;
use crate::navigation::{Overlay, Route, RouteHistory, Selection};
use std::sync::Arc;

/// Stable identity for one immutable source/object projection.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectId(backend_library::SemanticObject);

/// Stable identity for one admitted view transition.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeltaId([u8; 32]);

impl ObjectId {
    /// Creates an object identity from an engine mapping.
    #[must_use]
    pub const fn from_backend(value: backend_library::SemanticObject) -> Self {
        Self(value)
    }

    /// Creates a deterministic object identity for unit tests only.
    #[must_use]
    #[cfg(test)]
    pub fn test(value: u64) -> Self {
        Self::from_backend(backend_library::object_version(&value.to_be_bytes()))
    }

    /// Returns the canonical engine value.
    #[must_use]
    pub const fn get(self) -> backend_library::SemanticObject {
        self.0
    }
}

impl DeltaId {
    /// Creates a deterministic delta identity for unit tests only.
    #[must_use]
    #[cfg(test)]
    pub fn test(value: u64) -> Self {
        let mut bytes = [0; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        Self(bytes)
    }

    /// Creates a cache identity from a certified engine delta.
    #[must_use]
    pub fn from_backend(value: backend_library::ViewDeltaId) -> Self {
        Self(*value.as_bytes())
    }

    /// Returns the engine-facing number.
    #[must_use]
    pub const fn get(self) -> [u8; 32] {
        self.0
    }
}

/// One shelf row projected for the sidebar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShelfItem {
    /// Stable producer identity.
    pub identity: ResourceIdentity,
    /// Display label owned by the immutable snapshot.
    pub label: Arc<str>,
    /// Stable item object used by selectors and row measurement.
    pub object: ObjectId,
}

/// The complete shelf branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShelfState {
    /// Stable order of visible items.
    pub items: Arc<[ShelfItem]>,
    /// Selected shelf item, if any.
    pub selected: Option<ResourceIdentity>,
}

impl Default for ShelfState {
    fn default() -> Self {
        Self {
            items: Arc::from([]),
            selected: None,
        }
    }
}

/// One document tab summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentTab {
    /// Stable tab identity.
    pub id: DocumentId,
    /// Human-readable title.
    pub title: Arc<str>,
    /// Stable object used by selector and layout caches.
    pub object: ObjectId,
    /// Whether this tab is waiting for its first source response.
    pub pending: bool,
}

/// The open document branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentState {
    /// Open tabs in tab-bar order.
    pub tabs: Arc<[DocumentTab]>,
    /// Active tab identity.
    pub active: Option<DocumentId>,
}

impl Default for DocumentState {
    fn default() -> Self {
        Self {
            tabs: Arc::from([]),
            active: None,
        }
    }
}

/// The read-only project branch consumed by project and source views.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectState {
    /// Producer project identity.
    pub id: ProjectId,
    /// Human-readable project name.
    pub label: Arc<str>,
    /// Package identities admitted under the project.
    pub packages: Arc<[PackageId]>,
}

/// Persistent settings exposed through the snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsState {
    /// Whether reduced motion is requested.
    pub reduced_motion: bool,
    /// Whether the shelf is open.
    pub shelf_open: bool,
    /// Whether the context panel is open.
    pub context_open: bool,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            reduced_motion: false,
            shelf_open: true,
            context_open: true,
        }
    }
}

/// Session-local state restored on cold start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionState {
    /// Last route the user viewed.
    pub route: Route,
    /// Orthogonal shell overlay, if one is open.
    pub overlay: Option<Overlay>,
    /// Bounded persistent back history.
    pub back: RouteHistory,
    /// Bounded persistent forward history.
    pub forward: RouteHistory,
    /// Selected object/document retained across zoom transitions.
    pub selected: Option<Selection>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            route: Route::Orbit(crate::navigation::OrbitRoute::Home),
            overlay: None,
            back: RouteHistory::new(),
            forward: RouteHistory::new(),
            selected: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SnapshotData {
    shelf: Arc<ShelfState>,
    documents: Arc<DocumentState>,
    project: Resource<ProjectState>,
    settings: Arc<SettingsState>,
    session: Arc<SessionState>,
    delta: Option<DeltaId>,
}

/// One immutable, versioned UI read model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppSnapshot {
    key: VersionedRoot,
    data: Arc<SnapshotData>,
}

impl AppSnapshot {
    /// Creates an empty snapshot at an admitted root.
    #[must_use]
    pub fn empty(key: VersionedRoot) -> Self {
        Self {
            key,
            data: Arc::new(SnapshotData {
                shelf: Arc::new(ShelfState::default()),
                documents: Arc::new(DocumentState::default()),
                project: Resource::not_yet(),
                settings: Arc::new(SettingsState::default()),
                session: Arc::new(SessionState::default()),
                delta: None,
            }),
        }
    }

    /// Returns the exact root/version key this snapshot represents.
    #[must_use]
    pub const fn key(&self) -> VersionedRoot {
        self.key
    }

    /// Returns the root digest used by engine requests.
    #[must_use]
    pub const fn root(&self) -> backend_library::ViewStateRoot {
        self.key.root
    }

    /// Returns the monotonic observation sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.key.observation
    }

    /// Returns the shelf branch.
    #[must_use]
    pub fn shelf(&self) -> &ShelfState {
        &self.data.shelf
    }

    /// Returns the documents branch.
    #[must_use]
    pub fn documents(&self) -> &DocumentState {
        &self.data.documents
    }

    /// Returns the project branch with its sealed availability state.
    #[must_use]
    pub fn project(&self) -> &Resource<ProjectState> {
        &self.data.project
    }

    /// Returns the settings branch.
    #[must_use]
    pub fn settings(&self) -> &SettingsState {
        &self.data.settings
    }

    /// Returns the session branch.
    #[must_use]
    pub fn session(&self) -> &SessionState {
        &self.data.session
    }

    /// Returns the delta that invalidated derived selectors, when one exists.
    #[must_use]
    pub fn delta(&self) -> Option<DeltaId> {
        self.data.delta
    }

    /// Returns the current route without exposing mutable navigation storage.
    #[must_use]
    pub fn route(&self) -> &Route {
        &self.data.session.route
    }

    /// Returns the transient shell overlay without changing content route.
    #[must_use]
    pub fn overlay(&self) -> Option<Overlay> {
        self.data.session.overlay
    }

    /// Re-keys this same immutable projection at a newer engine root.
    #[must_use]
    pub fn with_key(&self, key: VersionedRoot, delta: Option<DeltaId>) -> Self {
        let mut next = self.clone();
        next.key = key;
        next.data = Arc::new(SnapshotData {
            shelf: Arc::clone(&self.data.shelf),
            documents: Arc::clone(&self.data.documents),
            project: self.data.project.clone(),
            settings: Arc::clone(&self.data.settings),
            session: Arc::clone(&self.data.session),
            delta,
        });
        next
    }

    /// Returns a copy with a changed route and back/forward state handled by
    /// the typed navigation reducer.
    #[must_use]
    pub(crate) fn with_session(&self, session: SessionState) -> Self {
        let mut next = self.clone();
        next.data = Arc::new(SnapshotData {
            shelf: Arc::clone(&self.data.shelf),
            documents: Arc::clone(&self.data.documents),
            project: self.data.project.clone(),
            settings: Arc::clone(&self.data.settings),
            session: Arc::new(session),
            delta: self.data.delta,
        });
        next
    }

    /// Returns a copy with a new shelf branch, sharing every other branch.
    #[must_use]
    pub(crate) fn with_shelf(&self, shelf: ShelfState) -> Self {
        let mut next = self.clone();
        next.data = Arc::new(SnapshotData {
            shelf: Arc::new(shelf),
            documents: Arc::clone(&self.data.documents),
            project: self.data.project.clone(),
            settings: Arc::clone(&self.data.settings),
            session: Arc::clone(&self.data.session),
            delta: self.data.delta,
        });
        next
    }

    /// Returns a copy with a new settings branch, sharing every other branch.
    #[must_use]
    pub(crate) fn with_settings(&self, settings: SettingsState) -> Self {
        let mut next = self.clone();
        next.data = Arc::new(SnapshotData {
            shelf: Arc::clone(&self.data.shelf),
            documents: Arc::clone(&self.data.documents),
            project: self.data.project.clone(),
            settings: Arc::new(settings),
            session: Arc::clone(&self.data.session),
            delta: self.data.delta,
        });
        next
    }

    /// Returns a copy with a producer-mapped project branch.
    #[must_use]
    pub(crate) fn with_project(&self, project: ProjectState, key: VersionedRoot) -> Self {
        let mut next = self.clone();
        next.data = Arc::new(SnapshotData {
            shelf: Arc::clone(&self.data.shelf),
            documents: Arc::clone(&self.data.documents),
            project: Resource::loaded_at(project, key),
            settings: Arc::clone(&self.data.settings),
            session: Arc::clone(&self.data.session),
            delta: self.data.delta,
        });
        next
    }

    /// Returns whether the shelf branch is structurally shared with `other`.
    #[must_use]
    pub fn shares_shelf_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data.shelf, &other.data.shelf)
    }

    /// Returns whether the settings branch is structurally shared with `other`.
    #[must_use]
    pub fn shares_settings_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data.settings, &other.data.settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> VersionedRoot {
        VersionedRoot::new(
            backend_library::view_state_root(&[("snapshot".to_owned(), "one".to_owned())]),
            1,
        )
    }

    #[test]
    fn changing_one_branch_keeps_other_branches_structurally_shared() {
        let initial = AppSnapshot::empty(root());
        let mut settings = initial.settings().clone();
        settings.reduced_motion = true;
        let changed = initial.with_settings(settings);
        assert!(changed.shares_shelf_with(&initial));
        assert!(!changed.shares_settings_with(&initial));

        let rekeyed = initial.with_key(root().with_generation(1), None);
        assert!(rekeyed.shares_shelf_with(&initial));
        assert!(rekeyed.shares_settings_with(&initial));
    }
}
