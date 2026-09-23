//! Immutable model, selectors, virtualization, and durable local state.

pub mod local_package;
pub mod persistence;
pub mod selectors;
pub mod snapshot;
pub mod viewport;
pub mod workspace;

pub use local_package::{
    CargoFailure, DependencyKind, LocalDependency, LocalFeature, LocalPackage, LocalPackageLoader,
    LocalPackageSource, ReadmeBlock,
};
pub use persistence::{
    PersistedAppearance, PersistedDesktopState, PersistedPackageLane, PersistedPrivacy,
    PersistedProjectPhase, PersistedRoute, PersistedServiceMode, PersistedShelfItem,
    PersistenceLoad, PersistenceRecovery, PersistenceRecoveryReason, PersistentState,
};
pub use selectors::{KeyedSelectorCache, LayoutKey, RowHeightCache, SelectorKey};
pub use snapshot::{
    AppSnapshot, AppearancePreference, CatalogState, ConnectionStatus, DeltaId, DocumentState,
    DocumentTab, ObjectId, PackageSummary, PrivacyPreference, ProjectPhase, ProjectState,
    ServiceMode, SessionState, SettingsState, ShelfItem, ShelfState, TextScalePreference,
    WorkspaceProject, WorkspaceState,
};
pub use viewport::{
    DocumentViewportState, SourceViewportState, ViewportId, ViewportState, VirtualCollection,
};
