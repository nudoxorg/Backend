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
    /// Consumes staging after verifying every descriptor of a complete root closure.
    ///
    /// # Errors
    ///
    /// Returns the exact partial-projection or first missing-descriptor fact;
    /// neither result can be converted to a verified-generation witness.
    pub fn verify<IsPresent>(
        self,
        mut is_present: IsPresent,
    ) -> Result<VerifiedGeneration, VerificationError<DomainTag>>
    where
        IsPresent: FnMut(ObjectRef<DomainTag>) -> bool,
    {
        if !self.plan.projection.is_complete() {
            return Err(VerificationError::PartialProjection {
                pinned_root: self.plan.pinned_root,
            });
        }
        for object in self.plan.required() {
            if !is_present(object) {
                return Err(VerificationError::MissingObject {
                    pinned_root: self.plan.pinned_root,
                    object,
                });
            }
        }
        Ok(VerifiedGeneration {
            pinned_root: self.plan.pinned_root,
            dep_set: self.plan.dep_set,
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

    /// Consumes staging after verifying every descriptor of a complete root closure.
    ///
    /// # Errors
    ///
    /// Returns the exact partial-projection or first missing-descriptor fact;
    /// neither result can be converted to a verified-generation witness.
    pub fn verify<IsPresent>(
        self,
        mut is_present: IsPresent,
    ) -> Result<VerifiedGeneration, VerificationError<DomainTag>>
    where
        IsPresent: FnMut(ObjectRef<DomainTag>) -> bool,
    {
        if !self.plan.projection.is_complete() {
            return Err(VerificationError::PartialProjection {
                pinned_root: self.plan.pinned_root,
            });
        }
        for object in self.plan.required() {
            if !is_present(object) {
                return Err(VerificationError::MissingObject {
                    pinned_root: self.plan.pinned_root,
                    object,
                });
            }
        }
        Ok(VerifiedGeneration {
            pinned_root: self.plan.pinned_root,
            dep_set: self.plan.dep_set,
        })
    }
}

/// Non-forgeable proof that one generation's complete dependency closure is locally present.
///
/// A durable publication adapter will eventually consume this proof and issue a distinct published
/// capability only after stable storage. Until that effect exists, there is no nominal ready phase.
#[non_exhaustive]
pub struct VerifiedGeneration {
    /// Immutable generation whose complete closure was verified.
    pub pinned_root: GenerationId,
    /// Canonical identity of that exact dependency closure.
    pub dep_set: DepSetId,
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
