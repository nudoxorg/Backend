//! Defines publication behavior for `server-journal`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
#[cfg(not(all(test, feature = "loom-model")))]
mod contract;
mod credit;
mod snapshot;
#[cfg(not(all(test, feature = "loom-model")))]
mod errors;
#[cfg(not(all(test, feature = "loom-model")))]
mod facts;
#[cfg(not(all(test, feature = "loom-model")))]
mod format;
#[cfg(not(all(test, feature = "loom-model")))]
mod latest;
#[cfg(not(all(test, feature = "loom-model")))]
mod owner;
#[cfg(not(all(test, feature = "loom-model")))]
mod service;

#[cfg(not(all(test, feature = "loom-model")))]
pub use contract::{
    ArtifactName, CancelError, DurablePublisher, ImmutablePublicationIdentity, PendingPublication,
    PublicationConflict, PublicationError, PublicationFacts, PublicationFailure,
    PublicationGenerationError, PublicationHeadIdentity, PublicationIoStep, PublicationLimitError,
    PublicationLimits, PublicationOpenError, PublicationPaths, PublicationSnapshotError,
    PublicationStateConflict,
    PublishedGeneration, SharedCommitError, SharedPublicationFailure, ShutdownError, SubmitError,
};
