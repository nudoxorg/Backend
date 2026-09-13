//! Exercises the `server-operation` tests canonical-byte-local-closure contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Store-backed canonical-root operation journey.

use std::collections::TryReserveError;
use std::num::TryFromIntError;

use heart_hydration::{Need, PlanError, PlanScratch, Projection, VerificationError, plan_borrowed};
use backend_version::{ContentId, GenerationId, ObjectDomain};
use backend_store::memory::{InsertOutcome, MemoryStore, RejectedInsert, StoreCapacity, StoreInitError};
use backend_version::object::{ObjectKind, ObjectLength, ObjectRef, ProviderId, ProviderIdError, ProviderSet};
use heart_root::{
    BorrowedGenerationView, ClosureScratch, EntryKey, GenerationRoot, LocalityError,
    LocalityException, LocalityWriteError, NonResident, PreparedLocality, RootBuildError,
    RootEntry, RootReadError, RootWriteError, ValidatedRoot,
};
use backend_version::schema::SchemaId;
use server_operation::{
    BatchSource, LocalObjectProvider, ObjectProvenance, SourcePoll, TerminalSummary,
    VerifiedObjectBindError,
};
use thiserror::Error;

const FIRST_BYTES: [u8; 4] = *b"one!";
const SECOND_BYTES: [u8; 4] = *b"two!";

#[derive(Debug, Error)]
enum JourneyError {
    #[error("root construction failed")]
    Root(#[from] RootBuildError),
    #[error("root output failed")]
    RootWrite(#[from] RootWriteError),
    #[error("root validation failed")]
    RootRead(#[from] RootReadError),
    #[error("locality composition failed")]
    Locality(#[from] LocalityError),
    #[error("locality output failed")]
    LocalityWrite(#[from] LocalityWriteError),
    #[error("provider fixture failed")]
    Provider(#[from] ProviderIdError),
    #[error("scratch reservation failed")]
    Scratch(#[source] TryReserveError),
    #[error("hydration planning failed")]
    Plan(#[from] PlanError),
    #[error("memory-store construction failed")]
    Store(#[from] StoreInitError),
    #[error("memory store rejected an exact borrowed body: {0:?}")]
    StoreInsert(Box<RejectedInsert<ObjectDomain, &'static [u8]>>),
    #[error("memory store replayed a body that should be new")]
    StoreReplay,
    #[error("generation verification failed")]
    Verification(#[from] VerificationError<ObjectDomain>),
    #[error("bound object operation failed")]
    Operation(#[from] VerifiedObjectBindError),
    #[error("bound cursor emitted an unexpected phase")]
    UnexpectedPhase,
    #[error("object fixture length did not fit the canonical length")]
    ObjectLength(#[source] TryFromIntError),
}

impl From<core::convert::Infallible> for JourneyError {
    fn from(error: core::convert::Infallible) -> Self {
        match error {}
    }
}

#[test]
fn exact_store_witness_binds_once_and_keeps_promised_bytes_local() -> Result<(), JourneyError> {
    let first = object(&FIRST_BYTES, 1)?;
    let second = object(&SECOND_BYTES, 2)?;
    let root = GenerationRoot::new(Vec::from([
        RootEntry {
            key: EntryKey::from(1),
            parent: None,
            object: first,
        },
        RootEntry {
            key: EntryKey::from(2),
            parent: Some(EntryKey::from(1)),
            object: second,
        },
    ]))?;
    let mut root_bytes = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut root_bytes)?;
    let borrowed_root = ValidatedRoot::<ObjectDomain>::try_from(root_bytes.as_slice())?;

    let promised_row = root
        .locality_row(EntryKey::from(2))
        .ok_or(JourneyError::UnexpectedPhase)?;
    let exceptions = [LocalityException::new(
        promised_row,
        NonResident::Promised(ProviderSet::only(ProviderId::try_from(3)?)),
    )];
    let prepared = PreparedLocality::prepare(&root, &exceptions)?;
    let mut locality_bytes = vec![0; usize::from(prepared.required_bytes)];
    let locality = prepared.write(&mut locality_bytes)?;
    let view = BorrowedGenerationView::new(&borrowed_root, &locality)?;

    let mut closure = ClosureScratch::new(view.len()).map_err(JourneyError::Scratch)?;
    let mut planning = PlanScratch::new(view.entry_count.into()).map_err(JourneyError::Scratch)?;
    let plan = plan_borrowed(
        Need::new(view.id, Projection::CompleteGeneration).bind_borrowed(&view)?,
        &mut closure,
        &mut planning,
        |_| true,
    )?;

    let mut store: MemoryStore<ObjectDomain, &'static [u8]> = MemoryStore::new(StoreCapacity {
        bytes: 8_u64.into(),
        slots: 2_u32.into(),
    })?;
    insert(&mut store, first, &FIRST_BYTES)?;
    insert(&mut store, second, &SECOND_BYTES)?;
    let verified = plan.stage().verify_store(&store)?;

    assert_bound_routes(&view, &verified, first, second)?;
    assert_stale_precedes_lookup(first, &view, &verified)?;
    Ok(())
}

fn assert_bound_routes<PayloadOwner: AsRef<[u8]>>(
    view: &BorrowedGenerationView<'_, '_, ObjectDomain>,
    verified: &heart_hydration::VerifiedGeneration<'_, ObjectDomain, PayloadOwner>,
    first: ObjectRef<ObjectDomain>,
    second: ObjectRef<ObjectDomain>,
) -> Result<(), JourneyError> {
    let resident = LocalObjectProvider::bind_verified(view, EntryKey::from(1), verified)?;
    let hydrated = LocalObjectProvider::bind_verified(view, EntryKey::from(2), verified)?;
    assert_run(&resident, view.id, first, ObjectProvenance::Resident)?;
    assert_run(
        &hydrated,
        view.id,
        second,
        ObjectProvenance::HydratedPromise,
    )?;
    assert_eq!(
        verified
            .as_ref()
            .get(second.content)
            .map(|stored| stored.bytes.as_ptr()),
        Some(SECOND_BYTES.as_ptr())
    );
    Ok(())
}

fn assert_stale_precedes_lookup<PayloadOwner: AsRef<[u8]>>(
    first: ObjectRef<ObjectDomain>,
    view: &BorrowedGenerationView<'_, '_, ObjectDomain>,
    verified: &heart_hydration::VerifiedGeneration<'_, ObjectDomain, PayloadOwner>,
) -> Result<(), JourneyError> {
    let stale_owned_root = GenerationRoot::new(Vec::from([RootEntry {
        key: EntryKey::from(9),
        parent: None,
        object: first,
    }]))?;
    let mut stale_bytes = vec![0; usize::from(stale_owned_root.canonical_len())];
    stale_owned_root.write_canonical(&mut stale_bytes)?;
    let stale_root = ValidatedRoot::<ObjectDomain>::try_from(stale_bytes.as_slice())?;
    let stale_prepared = PreparedLocality::prepare(&stale_owned_root, &[])?;
    let mut stale_locality_bytes = vec![0; usize::from(stale_prepared.required_bytes)];
    let stale_locality = stale_prepared.write(&mut stale_locality_bytes)?;
    let stale_view = BorrowedGenerationView::new(&stale_root, &stale_locality)?;
    assert_eq!(
        LocalObjectProvider::bind_verified(&stale_view, EntryKey::from(99), verified).map(|_| ()),
        Err(VerifiedObjectBindError::StaleGeneration {
            expected: stale_view.id,
            observed: view.id,
        })
    );
    assert_eq!(
        LocalObjectProvider::bind_verified(view, EntryKey::from(99), verified).map(|_| ()),
        Err(VerifiedObjectBindError::MissingKey {
            generation: view.id,
            key: EntryKey::from(99),
        })
    );
    Ok(())
}

fn object(bytes: &[u8], kind: u16) -> Result<ObjectRef<ObjectDomain>, JourneyError> {
    Ok(ObjectRef {
        content: ContentId::from_canonical_bytes(bytes),
        length: ObjectLength::from(u64::try_from(bytes.len()).map_err(JourneyError::ObjectLength)?),
        schema: SchemaId::Object,
        kind: ObjectKind::from(kind),
    })
}

fn insert(
    store: &mut MemoryStore<ObjectDomain, &'static [u8]>,
    reference: ObjectRef<ObjectDomain>,
    bytes: &'static [u8],
) -> Result<(), JourneyError> {
    match store.insert_owned(reference, bytes) {
        Ok(InsertOutcome::Inserted) => Ok(()),
        Ok(InsertOutcome::AlreadyPresent) => Err(JourneyError::StoreReplay),
        Err(rejected) => Err(JourneyError::StoreInsert(Box::new(rejected))),
    }
}

fn assert_run<PayloadOwner: AsRef<[u8]>>(
    provider: &server_operation::BoundLocalObjectProvider<'_, '_, ObjectDomain, PayloadOwner>,
    generation: GenerationId,
    expected: ObjectRef<ObjectDomain>,
    provenance: ObjectProvenance,
) -> Result<(), JourneyError> {
    let mut run = provider.start();
    match run.next_batch()? {
        SourcePoll::Batch(batch) => {
            assert_eq!(
                (batch.generation, batch.provenance, *batch.item),
                (generation, provenance, expected)
            );
        }
        SourcePoll::Terminal(_) | SourcePoll::Finished => {
            return Err(JourneyError::UnexpectedPhase);
        }
    }
    assert_eq!(
        run.next_batch()?,
        SourcePoll::Terminal(TerminalSummary::Complete { emitted: 1 })
    );
    assert_eq!(run.next_batch()?, SourcePoll::Finished);
    assert_eq!(run.next_batch()?, SourcePoll::Finished);
    Ok(())
}
