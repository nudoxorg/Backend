//! Defines tests behavior for `heart-hydration`, whose purpose is to plan and verify borrowed object hydration without weakening generation authority.
//! This module owns the tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use alloc::{boxed::Box, collections::TryReserveError, vec::Vec};
use core::{mem::size_of, num::TryFromIntError};

use backend_version::{ContentId, GenerationId, ObjectDomain};
use backend_store::memory::{InsertOutcome, MemoryStore, RejectedInsert, StoreCapacity, StoreInitError};
use backend_version::object::{ObjectRef, ProviderId, ProviderIdError, ProviderSet, RemoteBase};
use backend_version::observe::{DropNewest, FlightRecorder};
use heart_root::{
    BorrowedGenerationView, ClosureError, ClosureScratch, EntryKey, EntryRangeError,
    GenerationRoot, GenerationView, LocalityError, LocalityException, LocalityWriteError,
    MetadataBytes, NonResident, PreparedLocality, RootBuildError, RootEntry, RootReadError,
    RootWriteError, ValidatedLocality, ValidatedRoot,
};
use backend_version::schema::SchemaId;
use rstest::rstest;
use thiserror::Error;

use crate::{
    AbsentCount, DemandBindError, Fetch, FetchRoute, HydrationOutcome, HydrationProbeEvent, Need,
    PlanCoverage, PlanError, PlanScratch, Projection, VerificationError, demand, plan,
    plan_borrowed, plan_with_probe,
};

#[derive(Debug, Error)]
enum ScenarioError {
    #[error("root fixture construction failed")]
    Root(#[from] RootBuildError),
    #[error("borrowed root fixture decoding failed")]
    RootRead(#[from] RootReadError),
    #[error("borrowed root fixture encoding failed")]
    RootWrite(#[from] RootWriteError),
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
    #[error("memory-store fixture construction failed")]
    StoreInit(#[from] StoreInitError),
    #[error("memory-store fixture rejected an exact object: {0:?}")]
    StoreInsert(Box<RejectedInsert<ObjectDomain>>),
    #[error("memory-store fixture replayed an object that should be newly inserted")]
    UnexpectedStoreReplay,
    #[error("memory-store fixture cardinality did not fit its typed capacity")]
    StoreCardinality(#[source] TryFromIntError),
    #[error("memory-store fixture byte capacity overflowed")]
    StoreByteCapacityOverflow,
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
    DescriptorVerification,
    PlanCapacity,
}

#[derive(Debug)]
enum ScenarioExpectation {
    GenerationMismatch,
    PartialProjection,
    MissingObject,
    StoredDescriptorMismatch,
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
    let bytes = object_bytes(byte);
    ObjectRef {
        content: ContentId::from_canonical_bytes(&bytes),
        length: 4_u64.into(),
        schema: SchemaId::Object,
        kind: u16::from(byte).into(),
    }
}

const fn object_bytes(byte: u8) -> [u8; 4] {
    [byte; 4]
}

fn memory_store(bytes: &[u8]) -> Result<MemoryStore<ObjectDomain>, ScenarioError> {
    let slots = u32::try_from(bytes.len()).map_err(ScenarioError::StoreCardinality)?;
    let byte_capacity = u64::try_from(bytes.len())
        .map_err(ScenarioError::StoreCardinality)?
        .checked_mul(4)
        .ok_or(ScenarioError::StoreByteCapacityOverflow)?;
    let mut store = MemoryStore::new(StoreCapacity {
        bytes: byte_capacity.into(),
        slots: slots.into(),
    })?;
    for &byte in bytes {
        insert_fixture(&mut store, object(byte), byte)?;
    }
    Ok(store)
}

fn insert_fixture(
    store: &mut MemoryStore<ObjectDomain>,
    reference: ObjectRef<ObjectDomain>,
    byte: u8,
) -> Result<(), ScenarioError> {
    match store.insert_owned(reference, Box::from(object_bytes(byte))) {
        Ok(InsertOutcome::Inserted) => Ok(()),
        Ok(InsertOutcome::AlreadyPresent) => Err(ScenarioError::UnexpectedStoreReplay),
        Err(rejected) => Err(ScenarioError::StoreInsert(Box::new(rejected))),
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

#[path = "unit/borrowed.rs"]
mod borrowed;
#[path = "unit/planning.rs"]
mod planning;
#[path = "unit/publication.rs"]
mod publication;
