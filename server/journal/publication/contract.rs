//! Defines publication contract behavior for `server-journal`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the publication contract invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Internal facade for the durable publication contract.
//!
//! The public names remain re-exported from this module, while each implementation owner lives in
//! a focused sibling: facts and paths, source-bearing errors, service handles, credit leases,
//! journal owner/grouping, and artifact format/I/O.

pub use super::errors::{
    ArtifactName, CancelError, PublicationConflict, PublicationError, PublicationFailure,
    PublicationGenerationError, PublicationIoStep, PublicationLimitError, PublicationOpenError,
    PublicationSnapshotError, PublicationStateConflict, SharedCommitError,
    SharedPublicationFailure, ShutdownError, SubmitError,
};
pub use super::facts::{
    ImmutablePublicationIdentity, PublicationFacts, PublicationHeadIdentity, PublicationLimits,
    PublicationPaths,
};
pub use super::service::{DurablePublisher, PendingPublication, PublishedGeneration};
