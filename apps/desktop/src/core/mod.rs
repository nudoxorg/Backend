//! Product-neutral desktop contracts.

pub mod admission;
pub mod ids;
pub mod ports;
pub mod state;

pub use backend_platform::NativePath;
pub use admission::{ReadHoldReason, ResourceAdmission, admit_resource};
pub use ids::{
    DocumentId, IdentityError, LocalProjectId, PackageId, ProducerAuthority, ProjectId,
    ResourceIdentity, RowId, VersionedRoot,
};
pub use ports::{ActionCatalog, IntentDispatcher, SnapshotReadModel};
pub use state::{Activity, ErrorValue, FaultCode, Resource, ResourceTerminal, UnavailableReason};
