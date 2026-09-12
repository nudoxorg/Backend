//! Defines project service behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the project service invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The project-folder commands as this build answers them: the durable store and the lockfile
//! readers land here, and until they do every answer is the typed [`ProjectError::Store`] state.

use crate::{
    CreateProject, Library, MemberChange, Project, ProjectError, ProjectId, ProjectSelector,
    Projects, SyncReport, SyncRequest,
};

/// The one sentence every unattached project answer carries.
const UNATTACHED: &str = "the project store is not attached in this build";

fn unattached() -> ProjectError {
    ProjectError::Store {
        detail: UNATTACHED.into(),
    }
}

impl Library {
    /// Reads every folder.
    ///
    /// # Errors
    ///
    /// Returns the exact store failure.
    pub fn projects(&self) -> Result<Projects, ProjectError> {
        Err(unattached())
    }

    /// Creates one folder.
    ///
    /// # Errors
    ///
    /// Returns the exact store, capacity, name, or lockfile failure.
    pub fn project_create(&self, request: &CreateProject) -> Result<Project, ProjectError> {
        let _ = request;
        Err(unattached())
    }

    /// Deletes one folder.
    ///
    /// # Errors
    ///
    /// Returns the exact store or lookup failure.
    pub fn project_delete(&self, selector: &ProjectSelector) -> Result<ProjectId, ProjectError> {
        let _ = selector;
        Err(unattached())
    }

    /// Adds one member.
    ///
    /// # Errors
    ///
    /// Returns the exact store, lookup, or capacity failure.
    pub fn project_add(&self, change: &MemberChange) -> Result<Project, ProjectError> {
        let _ = change;
        Err(unattached())
    }

    /// Removes one member.
    ///
    /// # Errors
    ///
    /// Returns the exact store or lookup failure.
    pub fn project_remove(&self, change: &MemberChange) -> Result<Project, ProjectError> {
        let _ = change;
        Err(unattached())
    }

    /// Reconciles one folder with its lockfile.
    ///
    /// # Errors
    ///
    /// Returns the exact store, lookup, binding, or lockfile failure.
    pub fn project_sync(&self, request: &SyncRequest) -> Result<SyncReport, ProjectError> {
        let _ = request;
        Err(unattached())
    }
}
