//! Defines publication behavior for `heart-hydration`, whose purpose is to plan and verify borrowed object hydration without weakening generation authority.
//! This module owns the publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use alloc::boxed::Box;
use core::ops::Deref;

use backend_version::{Domain, GenerationId};
use backend_store::memory::MemoryStore;
use backend_version::object::{DepSetId, ObjectRef};
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
    /// Consumes staging after checking every descriptor in the exact memory store.
    ///
    /// # Errors
    ///
    /// Returns the exact partial-projection, missing-descriptor, or stored
    /// metadata-mismatch fact; neither result can be converted to a
    /// verified-generation witness. The returned witness retains the exact
    /// store borrow used for every lookup.
    pub fn verify_store<PayloadOwner>(
        self,
        store: &MemoryStore<DomainTag, PayloadOwner>,
    ) -> Result<VerifiedGeneration<'_, DomainTag, PayloadOwner>, VerificationError<DomainTag>>
    where
        PayloadOwner: AsRef<[u8]>,
    {
        if !self.plan.projection.is_complete() {
            return Err(VerificationError::PartialProjection {
                pinned_root: self.plan.pinned_root,
            });
        }
        verify_required(
            self.plan.pinned_root,
            self.plan.dep_set,
            self.plan.required(),
            store,
        )
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

    /// Consumes staging after checking every descriptor in the exact memory store.
    ///
    /// # Errors
    ///
    /// Returns the exact partial-projection, missing-descriptor, or stored
    /// metadata-mismatch fact; neither result can be converted to a
    /// verified-generation witness. The returned witness retains the exact
    /// store borrow used for every lookup.
    pub fn verify_store<PayloadOwner>(
        self,
        store: &MemoryStore<DomainTag, PayloadOwner>,
    ) -> Result<VerifiedGeneration<'_, DomainTag, PayloadOwner>, VerificationError<DomainTag>>
    where
        PayloadOwner: AsRef<[u8]>,
    {
        if !self.plan.projection.is_complete() {
            return Err(VerificationError::PartialProjection {
                pinned_root: self.plan.pinned_root,
            });
        }
        verify_required(
            self.plan.pinned_root,
            self.plan.dep_set,
            self.plan.required(),
            store,
        )
    }
}

fn verify_required<DomainTag, PayloadOwner>(
    pinned_root: GenerationId,
    dep_set: DepSetId,
    required: impl Iterator<Item = ObjectRef<DomainTag>>,
    store: &MemoryStore<DomainTag, PayloadOwner>,
) -> Result<VerifiedGeneration<'_, DomainTag, PayloadOwner>, VerificationError<DomainTag>>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    for expected in required {
        let Some(stored) = store.get(expected.content) else {
            return Err(VerificationError::MissingObject {
                report: Box::new(MissingRequiredObject {
                    pinned_root,
                    object: expected,
                }),
            });
        };
        let actual = stored.reference;
        if actual != expected {
            return Err(VerificationError::StoredDescriptorMismatch {
                report: Box::new(StoredDescriptorConflict {
                    pinned_root,
                    expected,
                    actual,
                }),
            });
        }
    }
    Ok(VerifiedGeneration {
        facts: VerifiedGenerationFacts {
            pinned_root,
            dep_set,
        },
        store,
    })
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

/// Sealed generation facts retaining the exact memory store that was checked.
///
/// Holding this value keeps the store immutably borrowed. A consumer can use
/// [`AsRef`] to read from that exact store rather than substituting another
/// instance after verification.
pub struct VerifiedGeneration<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    facts: VerifiedGenerationFacts,
    store: &'store MemoryStore<DomainTag, PayloadOwner>,
}

impl<DomainTag, PayloadOwner> Deref for VerifiedGeneration<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    type Target = VerifiedGenerationFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<DomainTag, PayloadOwner> AsRef<MemoryStore<DomainTag, PayloadOwner>>
    for VerifiedGeneration<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn as_ref(&self) -> &MemoryStore<DomainTag, PayloadOwner> {
        self.store
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
    #[error("{report}")]
    MissingObject {
        /// Complete cold-path diagnostic, allocated only after failed lookup.
        report: Box<MissingRequiredObject<DomainTag>>,
    },
    /// The store retained the required content under different descriptor metadata.
    #[error("{report}")]
    StoredDescriptorMismatch {
        /// Complete cold-path diagnostic, allocated only after failed comparison.
        report: Box<StoredDescriptorConflict<DomainTag>>,
    },
}

/// Exact missing-object report retained outside the hot result layout.
#[derive(Debug, Error)]
#[error("generation {pinned_root:?} is missing required object {object:?}")]
pub struct MissingRequiredObject<DomainTag> {
    /// Incomplete root.
    pub pinned_root: GenerationId,
    /// Absent descriptor.
    pub object: ObjectRef<DomainTag>,
}

/// Exact descriptor-conflict report retained outside the hot result layout.
#[derive(Debug, Error)]
#[error("generation {pinned_root:?} expected stored descriptor {expected:?}, found {actual:?}")]
pub struct StoredDescriptorConflict<DomainTag> {
    /// Incomplete root.
    pub pinned_root: GenerationId,
    /// Descriptor named by the generation closure.
    pub expected: ObjectRef<DomainTag>,
    /// Descriptor retained by the store for the same content identity.
    pub actual: ObjectRef<DomainTag>,
}
