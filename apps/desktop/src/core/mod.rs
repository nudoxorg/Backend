//! Product-neutral desktop contracts.

pub mod ids;
pub mod layout;
pub mod ports;
pub mod state;
pub mod tokens;

pub use backend_platform::NativePath;
pub use ids::{
    DocumentId, IdentityError, LocalProjectId, PackageId, ProducerAuthority, ProjectId,
    ResourceIdentity, RowId, VersionedRoot,
};
pub use layout::{
    CollapseStage, HorizontalOverflow, LayoutCache, LayoutInput, LayoutTransitionKind, LogicalPx,
    PanelMode, PanelPreferences, RegionBounds, RegionId, RegionPresentation, RegionSlot,
    ResponsiveLayout, SafeContentBounds, SheetKind, ShellRegions, TextScale, TransitionPlan,
    WidthClass, WindowContentSize, resolve as resolve_layout, transition_plan,
};
pub use ports::{ActionCatalog, IntentDispatcher, SnapshotReadModel};
pub use state::{Activity, ErrorValue, FaultCode, Resource, ResourceTerminal, UnavailableReason};
pub use tokens::{DensityToken, PaletteChannel, SemanticMark, SurfaceToken};
