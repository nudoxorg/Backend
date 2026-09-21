//! Product-neutral desktop contracts.

pub mod ids;
pub mod layout;
pub mod ports;
pub mod state;
pub mod tokens;

pub use ids::{
    DocumentId, IdentityError, LocalProjectId, PackageId, ProjectId, ResourceIdentity, RowId,
    VersionedRoot,
};
pub use layout::{PanelMode, ResponsiveLayout, WidthClass};
pub use ports::{ActionCatalog, IntentDispatcher, SnapshotReadModel};
pub use state::{Activity, ErrorValue, FaultCode, Resource, ResourceTerminal, UnavailableReason};
pub use tokens::{DensityToken, PaletteChannel, SemanticMark, SurfaceToken};
