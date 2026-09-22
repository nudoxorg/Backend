//! Immutable model, selectors, virtualization, and durable local state.

pub mod persistence;
pub mod selectors;
pub mod snapshot;
pub mod viewport;

pub use persistence::{
    PersistedAppearance, PersistedDesktopState, PersistedPackageLane, PersistedPrivacy,
    PersistedProjectPhase, PersistedRoute, PersistedServiceMode, PersistedShelfItem,
    PersistenceLoad, PersistenceRecovery, PersistenceRecoveryReason, PersistentState,
};
pub use selectors::{KeyedSelectorCache, LayoutKey, RowHeightCache, SelectorKey};
pub use snapshot::{
    AppSnapshot, CatalogState, DeltaId, DocumentState, DocumentTab, ObjectId, PackageSummary,
    ProjectState, SessionState, SettingsState, ShelfItem, ShelfState,
};
pub use viewport::{
    DocumentViewportState, SourceViewportState, ViewportId, ViewportState, VirtualCollection,
};
