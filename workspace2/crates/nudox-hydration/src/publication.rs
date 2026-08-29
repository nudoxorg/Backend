use core::marker::PhantomData;

use nudox_id::GenerationId;
use nudox_object::{DepSetId, ObjectRef};
use nudox_root::LocalityReadError;
use thiserror::Error;

use crate::HydrationPlanView;

/// Borrowed staged transition with no readiness authority.
pub struct StagedGeneration<'plan, 'selection, 'storage, DomainTag> {
    plan: &'plan HydrationPlanView<'selection, 'storage, DomainTag>,
}
impl<'plan, 'selection, 'storage, DomainTag>
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
    /// neither result can be converted to a publication witness.
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
        for item in self.plan.required() {
            let object = item.map_err(|source| VerificationError::LocalityRead {
                pinned_root: self.plan.pinned_root,
                source,
            })?;
            if !is_present(object) {
                return Err(VerificationError::MissingObject {
                    pinned_root: self.plan.pinned_root,
                    object,
                });
            }
        }
        Ok(Generation {
            pinned_root: self.plan.pinned_root,
            dep_set: self.plan.dep_set,
            state: PhantomData,
        })
    }
}

/// One generation carrying compile-time publication authority.
pub struct Generation<PublicationState> {
    /// Immutable generation whose complete closure the state witnesses.
    pub pinned_root: GenerationId,
    /// Canonical identity of that exact dependency closure.
    pub dep_set: DepSetId,
    state: PhantomData<fn() -> PublicationState>,
}

/// State reached only after complete local closure verification.
pub enum Verified {}

/// State reached only after consuming verified publication authority.
pub enum Ready {}

/// Verified closure proof, consumed by publication.
pub type VerifiedGeneration = Generation<Verified>;

/// Non-forgeable proof of complete locally verified root closure.
pub type ReadyGeneration = Generation<Ready>;

impl Generation<Verified> {
    /// Consumes verification and produces the sole public readiness witness.
    #[must_use]
    pub const fn publish(self) -> ReadyGeneration {
        Generation {
            pinned_root: self.pinned_root,
            dep_set: self.dep_set,
            state: PhantomData,
        }
    }
}

/// Verification failure consumes stage and exposes no publication capability.
#[derive(Debug, Error)]
pub enum VerificationError<DomainTag> {
    /// A projected subset cannot prove an entire root ready.
    #[error("projection cannot publish generation {pinned_root:?}")]
    PartialProjection {
        /// Incomplete root.
        pinned_root: GenerationId,
    },
    /// Validated locality could not reconstruct one required entry.
    #[error("could not read required locality for generation {pinned_root:?}")]
    LocalityRead {
        /// Generation whose selected locality failed reconstruction.
        pinned_root: GenerationId,
        /// Exact validated-lane read cause and ordinal.
        #[source]
        source: LocalityReadError,
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
