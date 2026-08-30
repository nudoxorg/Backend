#![no_std]
#![forbid(unsafe_code)]
//! Pure wanted/have planning and typed readiness publication.

extern crate alloc;

mod need;
mod plan;
mod publication;

pub use need::{BoundBorrowedNeed, BoundNeed, DemandBindError, Need, Projection, demand};
pub use plan::{
    AbsentCount, BorrowedHydrationPlanView, Fetch, FetchRoute, HydrationOutcome,
    HydrationPlanFacts, HydrationPlanView, HydrationProbeEvent, PlanCoverage, PlanError,
    PlanRejection, PlanScratch, PlanScratchFacts, Promise, plan, plan_borrowed, plan_with_probe,
};
pub use publication::{
    BorrowedStagedGeneration, MissingRequiredObject, StagedGeneration, StoredDescriptorConflict,
    VerificationError, VerifiedGeneration, VerifiedGenerationFacts,
};

#[cfg(test)]
mod tests;
