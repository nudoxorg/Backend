//! Defines tests behavior for `backend_store::memory`, whose purpose is to store immutable objects in bounded caller-selected memory.
//! This module owns the tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use alloc::{boxed::Box, vec::Vec};
use core::{mem::size_of, num::TryFromIntError};

use allocation_counter::{AllocationInfo, measure};
use backend_version::{ContentId, ContentRoutingWord, ObjectDomain};
use backend_version::object::ObjectRef;
use backend_version::observe::{DropNewest, FlightRecorder};
use backend_version::schema::SchemaId;
use thiserror::Error;

use crate::memory::{
    HeapBacking, InlineMemoryStore, InsertOutcome, LeanMemoryStore, Lookup, MemoryStore,
    RejectedInsert, Store, StoreAdmission, StoreCapacity, StoreError, StoreInitError,
    StoreProbeEvent,
};

const SUSTAINED_OBJECT_COUNT: u64 = 100_000;
const SUSTAINED_OBJECT_SLOTS: u32 = 100_000;
const OBJECT_PAYLOAD_BYTES: u64 = 8;
const FIRST_OBJECT_SEQUENCE: u64 = 0;

#[derive(Debug, Error)]
enum ScenarioError {
    #[error("store allocation failed")]
    Store(#[from] StoreInitError),
    #[error("{step:?}: unexpected store rejection {admission:?}")]
    UnexpectedRejection {
        step: ScenarioStep,
        admission: StoreAdmission,
    },
    #[error("reference fixture length did not fit the canonical object length")]
    FixtureLength(#[source] TryFromIntError),
    #[error("validated store geometry did not fit this target")]
    Geometry(#[source] TryFromIntError),
    #[error("allocation measurement did not execute its construction closure")]
    AllocationMeasurementDidNotRun,
    #[error("colliding content fixture exhausted its bounded search domain")]
    NoCollision,
    #[error("entry position {position} unexpectedly fell outside validated geometry")]
    OrdinalGeometry { position: usize },
    #[error("{step:?}: expected {expected:?}, observed {observed:?}")]
    Transition {
        step: ScenarioStep,
        expected: ScenarioExpectation,
        observed: ScenarioObservation,
    },
    #[error("{step:?}: expected stored object {content:?} to be present")]
    MissingObject {
        step: ScenarioStep,
        content: ContentId<ObjectDomain>,
    },
}

#[derive(Debug)]
enum ScenarioStep {
    ExactBoundary,
    FirstWrite,
    Replay,
    BorrowedViewAdmission,
    BorrowedView,
    WorkloadAdmission,
}

#[derive(Debug)]
enum ScenarioExpectation {
    Inserted,
    AlreadyPresent,
    ByteCapacityExceeded,
}

#[derive(Debug)]
enum ScenarioObservation {
    Inserted,
    AlreadyPresent,
}

fn reference(bytes: &[u8]) -> Result<ObjectRef<ObjectDomain>, ScenarioError> {
    let length = u64::try_from(bytes.len()).map_err(ScenarioError::FixtureLength)?;
    Ok(ObjectRef {
        content: ContentId::from_canonical_bytes(bytes),
        length: length.into(),
        schema: SchemaId::Object,
        kind: 1_u16.into(),
    })
}

fn store(bytes: u64, slots: u32) -> Result<MemoryStore<ObjectDomain>, ScenarioError> {
    MemoryStore::new(StoreCapacity {
        bytes: bytes.into(),
        slots: slots.into(),
    })
    .map_err(ScenarioError::Store)
}

fn require_outcome<PayloadOwner: AsRef<[u8]>>(
    result: Result<InsertOutcome, RejectedInsert<ObjectDomain, PayloadOwner>>,
    expected: InsertOutcome,
    step: ScenarioStep,
) -> Result<(), ScenarioError> {
    match result {
        Ok(observed) if observed == expected => Ok(()),
        Ok(InsertOutcome::Inserted) => Err(ScenarioError::Transition {
            step,
            expected: ScenarioExpectation::AlreadyPresent,
            observed: ScenarioObservation::Inserted,
        }),
        Ok(InsertOutcome::AlreadyPresent) => Err(ScenarioError::Transition {
            step,
            expected: ScenarioExpectation::Inserted,
            observed: ScenarioObservation::AlreadyPresent,
        }),
        Err(rejected) => Err(unexpected_rejection(step, &rejected)),
    }
}

fn unexpected_rejection(
    step: ScenarioStep,
    rejected: &RejectedInsert<ObjectDomain, impl AsRef<[u8]>>,
) -> ScenarioError {
    unexpected_error(step, &rejected.error)
}

fn unexpected_error(step: ScenarioStep, error: &StoreError<ObjectDomain>) -> ScenarioError {
    ScenarioError::UnexpectedRejection {
        step,
        admission: rejection_admission(error),
    }
}

const fn rejection_admission(error: &StoreError<ObjectDomain>) -> StoreAdmission {
    match error {
        StoreError::LengthMismatch { .. } => StoreAdmission::LengthMismatch,
        StoreError::ContentMismatch { .. } => StoreAdmission::ContentMismatch,
        StoreError::IntegrityConflict { .. } => StoreAdmission::IntegrityConflict,
        StoreError::ByteCapacityExceeded { .. } => StoreAdmission::ByteCapacityExceeded,
        StoreError::SlotCapacityExceeded { .. } => StoreAdmission::SlotCapacityExceeded,
    }
}

fn rejected_insert(
    result: Result<InsertOutcome, RejectedInsert<ObjectDomain>>,
) -> Result<RejectedInsert<ObjectDomain>, ScenarioError> {
    match result {
        Err(rejected) => Ok(rejected),
        Ok(InsertOutcome::Inserted) => Err(ScenarioError::Transition {
            step: ScenarioStep::ExactBoundary,
            expected: ScenarioExpectation::ByteCapacityExceeded,
            observed: ScenarioObservation::Inserted,
        }),
        Ok(InsertOutcome::AlreadyPresent) => Err(ScenarioError::Transition {
            step: ScenarioStep::ExactBoundary,
            expected: ScenarioExpectation::ByteCapacityExceeded,
            observed: ScenarioObservation::AlreadyPresent,
        }),
    }
}

const fn lookup_probes(lookup: Lookup) -> usize {
    match lookup {
        Lookup::Present { probes } | Lookup::Absent { probes } => probes,
    }
}

#[path = "unit/admission.rs"]
mod admission;
#[path = "unit/layout.rs"]
mod layout;
