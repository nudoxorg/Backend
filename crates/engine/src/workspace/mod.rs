//! Durable workspace publication and sole ownership.
//!
//! The engine keeps the public model seam entirely in the checked version and
//! store layers.  A model returns a checked [`backend_version::WorkspaceDelta`]
//! and checked [`backend_version::Commit`] together with a
//! [`backend_store::WorkspaceClosure`].  Roots, commit IDs, manifests, and
//! closure IDs are therefore derived from admitted values rather than copied
//! from caller supplied byte arrays.

pub mod catalog;
pub mod head;
pub mod lazy;
pub mod model;
pub mod owner;
pub mod pack;
pub mod publication;
pub mod record;
pub mod recovery;
pub mod transition;

pub use catalog::{DerivedOutputEntry, DerivedOutputPublication};
pub use head::{HeadExpectation, WorkspaceHead, WorkspaceSnapshot};
pub use lazy::{
    RelationIdentity, RelationKeyPrefix, WorkspaceRelationChild, WorkspaceRelationError,
    WorkspaceRelationFault, WorkspaceRelationHandle, WorkspaceRelationNodeHandle,
    WorkspaceRelationNodePage, WorkspaceRelationRejection,
};
pub use model::WorkspaceModel;
pub use owner::{OwnerLease, WorkspaceError, WorkspaceGcPin, WorkspaceOwner};
pub use publication::{
    Durable, DurablePublication, Prepared, PreparedPublication, PublicationStatus, Published,
    PublishedPublication,
};
pub use record::WorkspaceRecord;
pub use recovery::{RecoveryReport, TransactionDisposition};
pub use transition::{
    PersistedTransition, PreparedTransition, TransactionId, TransactionSchema, TransactionVersion,
    TransitionWork,
};
