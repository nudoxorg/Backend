//! Defines registry service behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the registry service invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The registry commands as this build answers them: the adapters land here, and until they do
//! every answer is the typed [`RegistryError::Unconfigured`] state rather than an empty page.

use crate::{
    DependentsPage, DependentsRequest, DetailRequest, ExplorePage, ExploreRequest, Library,
    OwnerPage, OwnerRequest, PackageDetail, RegistryError,
};

/// The one sentence every unattached registry answer carries.
const UNATTACHED: &str = "no registry adapter is attached in this build";

impl Library {
    /// Browses or searches the registries.
    ///
    /// # Errors
    ///
    /// Returns the exact registry, cache, or configuration failure.
    pub fn explore(&self, request: &ExploreRequest) -> Result<ExplorePage, RegistryError> {
        let _ = request;
        Err(RegistryError::Unconfigured {
            detail: UNATTACHED.into(),
        })
    }

    /// Profiles one registry package.
    ///
    /// # Errors
    ///
    /// Returns the exact registry, cache, or configuration failure.
    pub fn package_detail(&self, request: &DetailRequest) -> Result<PackageDetail, RegistryError> {
        let _ = request;
        Err(RegistryError::Unconfigured {
            detail: UNATTACHED.into(),
        })
    }

    /// Pages the dependents of one registry package.
    ///
    /// # Errors
    ///
    /// Returns the exact registry, cache, or configuration failure.
    pub fn dependents(&self, request: &DependentsRequest) -> Result<DependentsPage, RegistryError> {
        let _ = request;
        Err(RegistryError::Unconfigured {
            detail: UNATTACHED.into(),
        })
    }

    /// Lists one owner's packages.
    ///
    /// # Errors
    ///
    /// Returns the exact registry, cache, or configuration failure.
    pub fn owner(&self, request: &OwnerRequest) -> Result<OwnerPage, RegistryError> {
        let _ = request;
        Err(RegistryError::Unconfigured {
            detail: UNATTACHED.into(),
        })
    }
}
