//! Immutable model, selectors, virtualization, and durable local state.

pub mod browse;
pub mod hand;
pub mod local_package;
pub mod pages;
pub mod persistence;
pub mod release;
pub mod selectors;
pub mod snapshot;
pub mod source_facts;
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
    AppSnapshot, AppearancePreference, CatalogState, ConnectionStatus, ContrastPreference, DeltaId,
    DensityPreference, DocumentState, DocumentTab, MotionPreference, ObjectId, PackageSummary,
    PrivacyPreference, ProjectPhase, ProjectState, ServiceMode, SessionState, SettingsState,
    ShelfItem, ShelfState, WorkspaceProject, WorkspaceState, ZoomPreference, ZoomStep,
};
pub use viewport::{
    DocumentViewportState, SourceViewportState, ViewportId, ViewportState, VirtualCollection,
};
pub use workspace::{Note, WindowSize};
