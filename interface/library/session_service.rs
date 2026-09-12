//! Defines session service behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the session service invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The session-tree commands as this build answers them: the durable line codec lands here, and
//! until it does every answer is the typed [`TreeError::Store`] state rather than an empty tree.

use crate::{Library, SessionTree, TreeCloseRequest, TreeError, TreeOpenRequest, TreeOutcome};

/// The one sentence every unattached tree answer carries.
const UNATTACHED: &str = "the session tree store is not attached in this build";

fn unattached() -> TreeError {
    TreeError::Store {
        detail: UNATTACHED.into(),
    }
}

impl Library {
    /// Reads the whole tree.
    ///
    /// # Errors
    ///
    /// Returns the exact store failure.
    pub fn tree(&self) -> Result<SessionTree, TreeError> {
        Err(unattached())
    }

    /// Opens one subject, reusing an identical node under the same parent.
    ///
    /// # Errors
    ///
    /// Returns the exact store, lookup, or capacity failure.
    pub fn tree_open(&self, request: &TreeOpenRequest) -> Result<TreeOutcome, TreeError> {
        let _ = request;
        Err(unattached())
    }

    /// Closes one node or branch.
    ///
    /// # Errors
    ///
    /// Returns the exact store or lookup failure.
    pub fn tree_close(&self, request: &TreeCloseRequest) -> Result<TreeOutcome, TreeError> {
        let _ = request;
        Err(unattached())
    }
}
