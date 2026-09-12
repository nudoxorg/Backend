//! Defines follow service behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the follow service invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The subscription commands as this build answers them: the durable store lands here, and until
//! it does every answer is the typed [`FollowError::Store`] state rather than an empty list.

use crate::{
    FollowError, FollowKey, FollowOutcome, FollowRequest, Library, Releases, ReleasesRequest,
    Subscriptions,
};

/// The one sentence every unattached subscription answer carries.
const UNATTACHED: &str = "the subscription store is not attached in this build";

impl Library {
    /// Follows one package.
    ///
    /// # Errors
    ///
    /// Returns the exact store, capacity, project, or registry failure.
    pub fn subscribe(&self, request: &FollowRequest) -> Result<FollowOutcome, FollowError> {
        let _ = request;
        Err(FollowError::Store {
            detail: UNATTACHED.into(),
        })
    }

    /// Stops following one package.
    ///
    /// # Errors
    ///
    /// Returns the exact store failure.
    pub fn unsubscribe(&self, key: &FollowKey) -> Result<FollowOutcome, FollowError> {
        let _ = key;
        Err(FollowError::Store {
            detail: UNATTACHED.into(),
        })
    }

    /// Reads every subscription.
    ///
    /// # Errors
    ///
    /// Returns the exact store failure.
    pub fn subscriptions(&self) -> Result<Subscriptions, FollowError> {
        Err(FollowError::Store {
            detail: UNATTACHED.into(),
        })
    }

    /// Checks every subscription for releases newer than last seen.
    ///
    /// # Errors
    ///
    /// Returns the exact store failure; a registry that could not answer is a row inside the
    /// feed, never a failure of the whole feed.
    pub fn releases(&self, request: &ReleasesRequest) -> Result<Releases, FollowError> {
        let _ = request;
        Err(FollowError::Store {
            detail: UNATTACHED.into(),
        })
    }
}
