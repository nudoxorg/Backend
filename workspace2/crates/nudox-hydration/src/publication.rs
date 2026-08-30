use nudox_id::{Domain, GenerationId};
use nudox_object::{DepSetId, ObjectRef};
use thiserror::Error;

use crate::{BorrowedHydrationPlanView, HydrationPlanView};

/// Borrowed staged transition with no verified-completeness authority.
pub struct StagedGeneration<'plan, 'selection, 'storage, DomainTag> {
    plan: &'plan HydrationPlanView<'selection, 'storage, DomainTag>,
}
impl<'plan, 'selection, 'storage, DomainTag: Domain>
    StagedGeneration<'plan, 'selection, 'storage, DomainTag>
{
    pub(crate) const fn new(
        plan: &'plan HydrationPlanView<'selection, 'storage, DomainTag>,
    ) -> Self {
        Self { plan }
    }
    /// Consumes staging after the borrowed evidence owner affirms every descriptor.
    ///
    /// # Errors
    ///
    /// Returns the exact partial-projection or first missing-descriptor fact;
    /// neither result can be converted to a verified-generation witness. The
    /// returned witness retains `evidence`, so later consumers can use the exact
    /// owner that supplied these answers.
    pub fn verify<Evidence: ?Sized, IsPresent>(
        self,
        evidence: &Evidence,
        mut is_present: IsPresent,
    ) -> Result<VerifiedGeneration<'_, Evidence>, VerificationError<DomainTag>>
    where
        IsPresent: FnMut(&Evidence, ObjectRef<DomainTag>) -> bool,
    {
        if !self.plan.projection.is_complete() {
            return Err(VerificationError::PartialProjection {
                pinned_root: self.plan.pinned_root,
            });
        }
        for object in self.plan.required() {
            if !is_present(evidence, object) {
                return Err(VerificationError::MissingObject {
                    pinned_root: self.plan.pinned_root,
                    object,
                });
            }
        }
        Ok(VerifiedGeneration {
            facts: VerifiedGenerationFacts {
                pinned_root: self.plan.pinned_root,
                dep_set: self.plan.dep_set,
            },
            evidence,
        })
    }
}

/// Borrowed staged transition with no verified-completeness authority.
pub struct BorrowedStagedGeneration<'plan, 'selection, 'storage, 'root, 'locality, DomainTag> {
    plan: &'plan BorrowedHydrationPlanView<'selection, 'storage, 'root, 'locality, DomainTag>,
}

impl<'plan, 'selection, 'storage, 'root, 'locality, DomainTag: Domain>
    BorrowedStagedGeneration<'plan, 'selection, 'storage, 'root, 'locality, DomainTag>
{
    pub(crate) const fn new(
        plan: &'plan BorrowedHydrationPlanView<'selection, 'storage, 'root, 'locality, DomainTag>,
    ) -> Self {
        Self { plan }
    }

    /// Consumes staging after the borrowed evidence owner affirms every descriptor.
    ///
    /// # Errors
    ///
    /// Returns the exact partial-projection or first missing-descriptor fact;
    /// neither result can be converted to a verified-generation witness. The
    /// returned witness retains `evidence`, so later consumers can use the exact
    /// owner that supplied these answers.
    pub fn verify<Evidence: ?Sized, IsPresent>(
        self,
        evidence: &Evidence,
        mut is_present: IsPresent,
    ) -> Result<VerifiedGeneration<'_, Evidence>, VerificationError<DomainTag>>
    where
        IsPresent: FnMut(&Evidence, ObjectRef<DomainTag>) -> bool,
    {
        if !self.plan.projection.is_complete() {
            return Err(VerificationError::PartialProjection {
                pinned_root: self.plan.pinned_root,
            });
        }
        for object in self.plan.required() {
            if !is_present(evidence, object) {
                return Err(VerificationError::MissingObject {
                    pinned_root: self.plan.pinned_root,
                    object,
                });
            }
        }
        Ok(VerifiedGeneration {
            facts: VerifiedGenerationFacts {
                pinned_root: self.plan.pinned_root,
                dep_set: self.plan.dep_set,
            },
            evidence,
        })
    }
}

/// Read-only facts from checking a generation's complete dependency closure.
///
/// A durable publication adapter will eventually consume these facts together
/// with the retained evidence owner and issue a distinct published capability
/// only after stable storage. Until that effect exists, there is no nominal
/// ready phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedGenerationFacts {
    /// Immutable generation whose complete closure was verified.
    pub pinned_root: GenerationId,
    /// Canonical identity of that exact dependency closure.
    pub dep_set: DepSetId,
}

/// Sealed generation facts retaining the exact evidence owner that was checked.
///
/// Holding this value keeps the evidence immutably borrowed. A consumer can use
/// [`AsRef`] to read from that exact store, lease, or snapshot rather than
/// substituting another instance after verification.
pub struct VerifiedGeneration<'evidence, Evidence: ?Sized> {
    facts: VerifiedGenerationFacts,
    evidence: &'evidence Evidence,
}

impl<Evidence: ?Sized> Deref for VerifiedGeneration<'_, Evidence> {
    type Target = VerifiedGenerationFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<Evidence: ?Sized> AsRef<Evidence> for VerifiedGeneration<'_, Evidence> {
    fn as_ref(&self) -> &Evidence {
        self.evidence
    }
}

/// Verification failure consumes stage and exposes no publication capability.
#[derive(Debug, Error)]
pub enum VerificationError<DomainTag> {
    /// A projected subset cannot verify an entire generation closure.
    #[error("projection cannot verify complete generation {pinned_root:?}")]
    PartialProjection {
        /// Incomplete root.
        pinned_root: GenerationId,
    },
    /// Exact descriptor absent at final closure verification.
    #[error("generation {pinned_root:?} is missing required object {object:?}")]
    MissingObject {
        /// Incomplete root.
        pinned_root: GenerationId,
        /// Absent descriptor.
        object: ObjectRef<DomainTag>,
    },
}
use core::ops::Deref;
