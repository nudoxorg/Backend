//! Internal facade for the durable publication contract.
//!
//! The public names remain re-exported from this module, while each implementation owner lives in
//! a focused sibling: facts and paths, source-bearing errors, service handles, credit leases,
//! journal owner/grouping, and artifact format/I/O.

pub use super::errors::{
    ArtifactName, CancelError, PublicationConflict, PublicationError, PublicationFailure,
    PublicationIoStep, PublicationLimitError, PublicationOpenError, SharedCommitError,
    SharedPublicationFailure, ShutdownError, SubmitError,
};
pub use super::facts::{
    ImmutablePublicationIdentity, PublicationFacts, PublicationHeadIdentity, PublicationLimits,
    PublicationPaths,
};
pub use super::service::{DurablePublisher, PendingPublication, PublishedGeneration};
