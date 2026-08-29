use alloc::{collections::TryReserveError, vec, vec::Vec};
use core::num::TryFromIntError;

use bolero::check;
use nudox_id::{ContentId, GenerationId, ObjectDomain};
use nudox_object::{ObjectRef, ProviderIdError, RemoteBase};
use nudox_observe::{DropNewest, FlightRecorder};
use nudox_schema::SchemaId;
use thiserror::Error;

use crate::{
    ClosureError, ClosureScratch, EntryKey, EntryRangeError, GenerationRoot, GenerationView,
    Locality, LocalityError, LocalityException, LocalityReadError, LocalityWriteError, NonResident,
    OverlayError, PreparedLocality, RootBuildError, RootChange, RootEntry, RootProbeEvent,
    RootWriteError, SelectedOrdinalBuffer, SelectedOrdinalBufferError, ValidatedLocality,
    propagate_overlays,
};

const LARGE_ROOT_ROWS: u64 = 100_000;
const LARGE_ROOT_ROW_COUNT: usize = 100_000;
const MILLION_UNCHANGED_ROWS: u64 = 1_000_000;
const DIFF_MUTATION_STRIDE: u64 = 10_000;
const FIRST_ROOT_KEY: u64 = 0;

#[derive(Debug, Error)]
enum ScenarioError {
    #[error("root construction failed")]
    Root(#[from] RootBuildError),
    #[error("root output was too short")]
    RootWrite(#[from] RootWriteError),
    #[error("locality binding failed")]
    Locality(#[from] LocalityError),
    #[error("locality output failed")]
    LocalityWrite(#[from] LocalityWriteError),
    #[error("validated locality fixture read failed")]
    LocalityRead(#[from] LocalityReadError),
    #[error("closure selection failed")]
    Closure(#[from] ClosureError),
    #[error("range construction failed")]
    Range(#[from] EntryRangeError),
    #[error("provider fixture was invalid")]
    Provider(#[from] ProviderIdError),
    #[error("closure scratch reservation failed")]
    ClosureReservation(#[source] TryReserveError),
    #[error("selected ordinal buffer rejected a root-bounded selection")]
    OrdinalBuffer(#[from] SelectedOrdinalBufferError),
    #[error("million-row fixture count did not fit this process")]
    MillionRowCount(#[source] TryFromIntError),
    #[error("{step:?}: overlay propagation rejected with {rejection:?}")]
    Overlay {
        step: ScenarioStep,
        rejection: OverlayRejection,
    },
    #[error("{step:?}: expected {expected:?}, observed {observed:?}")]
    Transition {
        step: ScenarioStep,
        expected: ScenarioExpectation,
        observed: ScenarioObservation,
    },
    #[error("{step:?}: expected entry {key:?} to remain in the coherent view")]
    MissingEntry { step: ScenarioStep, key: EntryKey },
}

#[derive(Debug)]
enum ScenarioStep {
    RootRejection,
    PropagatedLocality,
    RemoteTombstone,
}

#[derive(Debug)]
enum ScenarioExpectation {
    DuplicateKey,
    MissingParent,
    HierarchyCycle,
    RemoteAbsent,
    Overlaid,
}

#[derive(Debug)]
enum ScenarioObservation {
    DifferentRootError,
    RootConstructed,
    RemotePresent,
    NonOverlay,
}

#[derive(Debug)]
enum OverlayRejection {
    Locality,
    BaseGenerationMismatch,
    BaseObjectMismatch,
    ExpectedAbsentBase,
    MarkScratchTooSmall,
    Output,
    MetadataByteOverflow,
}

#[derive(Debug, Eq, PartialEq)]
enum RootErrorKind {
    DuplicateKey,
    MissingParent,
    HierarchyCycle,
    IndexConversion,
    RowReservation,
    HierarchyReservation,
    PathReservation,
    DepthOverflow,
}

#[derive(Debug, Eq, PartialEq)]
enum PermutationOutcome {
    Equivalent,
    DifferentIdentity,
    OlderRejected(RootErrorKind),
    NewerRejected(RootErrorKind),
    BothRejected {
        older: RootErrorKind,
        newer: RootErrorKind,
    },
}

fn key(raw: u64) -> EntryKey {
    raw.into()
}

fn object(byte: u8) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::from([byte; 32]),
        length: u64::from(byte).into(),
        schema: SchemaId::Object,
        kind: 1_u16.into(),
    }
}

fn resident(
    raw: u64,
    parent: Option<u64>,
    object: ObjectRef<ObjectDomain>,
) -> RootEntry<ObjectDomain> {
    RootEntry {
        key: key(raw),
        parent: parent.map(EntryKey::from),
        object,
    }
}

fn root(
    entries: Vec<RootEntry<ObjectDomain>>,
) -> Result<GenerationRoot<ObjectDomain>, ScenarioError> {
    GenerationRoot::new(entries).map_err(ScenarioError::Root)
}

fn locality_exception(
    root: &GenerationRoot<ObjectDomain>,
    selected_key: EntryKey,
    placement: NonResident<ObjectDomain>,
) -> Result<LocalityException<ObjectDomain>, ScenarioError> {
    let row = root
        .locality_row(selected_key)
        .ok_or(ScenarioError::MissingEntry {
            step: ScenarioStep::PropagatedLocality,
            key: selected_key,
        })?;
    Ok(LocalityException::new(row, placement))
}

fn locality_bytes(
    root: &GenerationRoot<ObjectDomain>,
    facts: &[LocalityException<ObjectDomain>],
) -> Result<Vec<u8>, ScenarioError> {
    let prepared = PreparedLocality::prepare(root, facts)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::from(prepared.required_bytes))
        .map_err(ScenarioError::ClosureReservation)?;
    bytes.resize(usize::from(prepared.required_bytes), 0);
    prepared.write(&mut bytes)?;
    Ok(bytes)
}

fn require_root_error(
    result: Result<GenerationRoot<ObjectDomain>, RootBuildError>,
    expected: ScenarioExpectation,
    accepts: impl FnOnce(&RootBuildError) -> bool,
) -> Result<(), ScenarioError> {
    match result {
        Err(error) if accepts(&error) => Ok(()),
        Err(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::RootRejection,
            expected,
            observed: ScenarioObservation::DifferentRootError,
        }),
        Ok(_) => Err(ScenarioError::Transition {
            step: ScenarioStep::RootRejection,
            expected,
            observed: ScenarioObservation::RootConstructed,
        }),
    }
}

const fn root_error_kind(error: &RootBuildError) -> RootErrorKind {
    match error {
        RootBuildError::DuplicateKey { .. } => RootErrorKind::DuplicateKey,
        RootBuildError::MissingParent { .. } => RootErrorKind::MissingParent,
        RootBuildError::HierarchyCycle { .. } => RootErrorKind::HierarchyCycle,
        RootBuildError::IndexConversion(_) => RootErrorKind::IndexConversion,
        RootBuildError::RowReservation(_) => RootErrorKind::RowReservation,
        RootBuildError::HierarchyReservation(_) => RootErrorKind::HierarchyReservation,
        RootBuildError::PathReservation(_) => RootErrorKind::PathReservation,
        RootBuildError::DepthOverflow => RootErrorKind::DepthOverflow,
    }
}

mod locality_contracts;
mod root_contracts;
mod selection_contracts;
