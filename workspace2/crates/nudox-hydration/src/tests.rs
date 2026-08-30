use alloc::{collections::TryReserveError, vec::Vec};
use core::mem::size_of;

use nudox_id::{ContentId, GenerationId, ObjectDomain};
use nudox_object::{ObjectRef, ProviderId, ProviderIdError, ProviderSet, RemoteBase};
use nudox_observe::{DropNewest, FlightRecorder};
use nudox_root::{
    ClosureScratch, EntryKey, EntryRangeError, GenerationRoot, GenerationView, LocalityError,
    LocalityException, LocalityWriteError, MetadataBytes, NonResident, PreparedLocality,
    RootBuildError, RootEntry, ValidatedLocality,
};
use nudox_schema::SchemaId;
use rstest::rstest;
use thiserror::Error;

use crate::{
    AbsentCount, DemandBindError, Fetch, FetchRoute, HydrationOutcome, HydrationProbeEvent, Need,
    PlanCoverage, PlanError, PlanScratch, Projection, VerificationError, demand, plan,
    plan_with_probe,
};

#[derive(Debug, Error)]
enum ScenarioError {
    #[error("root fixture construction failed")]
    Root(#[from] RootBuildError),
    #[error("locality fixture binding failed")]
    Locality(#[from] LocalityError),
    #[error("locality fixture output failed")]
    LocalityWrite(#[from] LocalityWriteError),
    #[error("locality fixture reservation failed")]
    LocalityReservation(#[source] TryReserveError),
    #[error("root fixture has no locality row for {key:?}")]
    MissingLocalityRow { key: EntryKey },
    #[error("provider fixture was invalid")]
    Provider(#[from] ProviderIdError),
    #[error("closure scratch reservation failed")]
    ClosureReservation(#[source] TryReserveError),
    #[error("plan scratch reservation failed")]
    PlanReservation(#[source] TryReserveError),
    #[error("demand binding failed")]
    Demand(#[from] DemandBindError),
    #[error("plan derivation failed")]
    Plan(#[from] PlanError),
    #[error("generation verification failed")]
    Verification(#[source] VerificationError<ObjectDomain>),
    #[error("range construction failed")]
    Range(#[from] EntryRangeError),
    #[error("{step:?}: expected {expected:?}, observed {observed:?}")]
    Transition {
        step: ScenarioStep,
        expected: ScenarioExpectation,
        observed: ScenarioObservation,
    },
}

#[derive(Debug)]
enum ScenarioStep {
    DemandBinding,
    PartialVerification,
    MissingVerification,
    PlanCapacity,
}

#[derive(Debug)]
enum ScenarioExpectation {
    GenerationMismatch,
    PartialProjection,
    MissingObject,
    ScratchTooSmall,
}

#[derive(Debug)]
enum ScenarioObservation {
    BoundDemand,
    DifferentVerificationError,
    VerifiedGeneration,
    DifferentPlanError,
    HydrationPlan,
}

fn key(raw: u64) -> EntryKey {
    raw.into()
}

fn object(byte: u8) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::from_digest([byte; 32]),
        length: 4_u64.into(),
        schema: SchemaId::Object,
        kind: 1_u16.into(),
    }
}

fn root() -> Result<GenerationRoot<ObjectDomain>, ScenarioError> {
    GenerationRoot::new(Vec::from([
        RootEntry {
            key: key(1),
            parent: None,
            object: object(1),
        },
        RootEntry {
            key: key(2),
            parent: Some(key(1)),
            object: object(2),
        },
        RootEntry {
            key: key(3),
            parent: Some(key(1)),
            object: object(3),
        },
    ]))
    .map_err(ScenarioError::Root)
}

fn locality(root: &GenerationRoot<ObjectDomain>) -> Result<Vec<u8>, ScenarioError> {
    let provider = ProviderId::try_from(7_u8)?;
    let row = root
        .locality_row(key(2))
        .ok_or(ScenarioError::MissingLocalityRow { key: key(2) })?;
    locality_bytes(
        root,
        &[LocalityException::new(
            row,
            NonResident::Promised(ProviderSet::only(provider)),
        )],
    )
}

fn locality_bytes(
    root: &GenerationRoot<ObjectDomain>,
    facts: &[LocalityException<ObjectDomain>],
) -> Result<Vec<u8>, ScenarioError> {
    let prepared = PreparedLocality::prepare(root, facts)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::from(prepared.required_bytes))
        .map_err(ScenarioError::LocalityReservation)?;
    bytes.resize(usize::from(prepared.required_bytes), 0);
    prepared.write(&mut bytes)?;
    Ok(bytes)
}

fn closure_scratch(root: &GenerationRoot<ObjectDomain>) -> Result<ClosureScratch, ScenarioError> {
    ClosureScratch::new(root.len()).map_err(ScenarioError::ClosureReservation)
}

fn plan_scratch(root: &GenerationRoot<ObjectDomain>) -> Result<PlanScratch, ScenarioError> {
    PlanScratch::new(root.entry_count.into()).map_err(ScenarioError::PlanReservation)
}

fn require_demand_mismatch(
    result: &Result<crate::BoundNeed<'_, '_, '_, ObjectDomain>, DemandBindError>,
    actual: GenerationId,
) -> Result<(), ScenarioError> {
    match result {
        Err(DemandBindError::GenerationMismatch {
            requested,
            actual: seen,
        }) => {
            assert_eq!(*requested, GenerationId::from_digest([9; 32]));
            assert_eq!(*seen, actual);
            Ok(())
        }
        Ok(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::DemandBinding,
            expected: ScenarioExpectation::GenerationMismatch,
            observed: ScenarioObservation::BoundDemand,
        }),
    }
}

mod planning;
mod publication;
