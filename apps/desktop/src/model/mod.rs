//! Immutable model, selectors, virtualization, and durable local state.

pub mod persistence;
pub mod selectors;
pub mod snapshot;
pub mod viewport;

pub use persistence::{
    PersistedDesktopState, PersistedPackageLane, PersistedRoute, PersistedShelfItem,
    PersistentState,
};
pub use selectors::{KeyedSelectorCache, LayoutKey, RowHeightCache, SelectorKey};
pub use snapshot::{
    AppSnapshot, DeltaId, DocumentState, DocumentTab, ObjectId, ProjectState, SessionState,
    SettingsState, ShelfItem, ShelfState,
};
pub use viewport::{
    DocumentViewportState, SourceViewportState, ViewportId, ViewportState, VirtualCollection,
};
