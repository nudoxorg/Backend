//! Complete-generation construction over a canonical compiler package.

use core::num::TryFromIntError;
use std::collections::TryReserveError;

use nudox_hydration::{
    PlanError, PlanScratch, Projection, VerificationError, VerifiedGeneration, demand, plan,
};
use nudox_id::{ContentId, ObjectDomain};
use nudox_object::{ObjectKind, ObjectLength, ObjectRef};
use nudox_root::{
    ClosureScratch, GenerationRoot, GenerationRootBuilder, GenerationView, PreparedLocality,
    RootBuildError, RootEntry, RootPushError,
};
use nudox_schema::SchemaId;
use nudox_store_memory::{
    InsertOutcome, MemoryStore, RejectedInsert, StoreCapacity, StoreError, StoreInitError,
};
use thiserror::Error;

use crate::manifest::{CanonicalCompilation, CompilationManifestView, StoredFragmentFacts};

const MANIFEST_ENTRY_KEY: u64 = 0;
const MANIFEST_KIND: u16 = 1;
const FRAGMENT_KIND: u16 = 1;

/// Runs a continuation only while it holds a real complete-generation witness over the exact
/// borrowed compiler manifest and fragment bytes.
#[allow(
    clippy::result_large_err,
    reason = "cold generation construction retains exact sources"
)]
pub(crate) fn with_verified_generation<'input, 'scratch, 'fragment, 'manifest, 'facts, Output>(
    canonical: &CanonicalCompilation<'input, 'scratch, 'fragment>,
    manifest: &CompilationManifestView<'manifest, 'facts>,
    locality_output: &mut [u8],
    visit: impl FnOnce(VerifiedGeneration<'_, ObjectDomain, &'_ [u8]>) -> Output,
) -> Result<Output, GenerationBuildError> {
    let root = build_root(canonical, manifest)?;
    let capacity = store_capacity(manifest)?;
    let mut store = MemoryStore::<ObjectDomain, &[u8]>::new(capacity)
        .map_err(GenerationBuildError::StoreInitialization)?;
    let mut entries = root.closure();
    insert(
        &mut store,
        next_object(&mut entries, MANIFEST_ENTRY_KEY)?,
        manifest.as_ref(),
    )?;
    for (ordinal, compiled) in canonical.fragments().enumerate() {
        let key = u64::try_from(ordinal)
            .map_err(|source| GenerationBuildError::EntryKeyAddressSpace { ordinal, source })?
            .checked_add(1)
            .ok_or(GenerationBuildError::EntryCountOverflow)?;
        insert(
            &mut store,
            next_object(&mut entries, key)?,
            compiled.fragment.as_ref(),
        )?;
    }
    if entries.next().is_some() {
        return Err(GenerationBuildError::RootClosureExtra);
    }

    let prepared = PreparedLocality::prepare(&root, &[]).map_err(GenerationBuildError::Locality)?;
    let locality = prepared
        .write(locality_output)
        .map_err(GenerationBuildError::LocalityWrite)?;
    let view = GenerationView::new(&root, &locality).map_err(GenerationBuildError::Locality)?;
    let mut closure =
        ClosureScratch::new(root.len()).map_err(GenerationBuildError::ClosureReservation)?;
    let mut planning =
        PlanScratch::new(root.entry_count.into()).map_err(GenerationBuildError::PlanReservation)?;
    let plan = plan(
        demand(&view, Projection::CompleteGeneration),
        &mut closure,
        &mut planning,
        |_| true,
    )
    .map_err(GenerationBuildError::Plan)?;
    let verified = plan
        .stage()
        .verify_store(&store)
        .map_err(GenerationBuildError::Verification)?;
    Ok(visit(verified))
}

/// Rebuilds the exact complete generation selected by a persisted compiler package.
///
/// `fragment_bytes` contains the manifest's canonical fragments back-to-back in manifest order.
/// It is caller-owned transient verification storage; no borrow escapes this function.
#[allow(
    clippy::result_large_err,
    reason = "cold generation reconstruction retains exact sources"
)]
pub(crate) fn verify_reopened_generation(
    manifest: &CompilationManifestView<'_, '_>,
    fragment_bytes: &[u8],
    locality_output: &mut [u8],
) -> Result<nudox_hydration::VerifiedGenerationFacts, GenerationBuildError> {
    let root = build_reopened_root(manifest, fragment_bytes)?;
    let capacity = store_capacity(manifest)?;
    let mut store = MemoryStore::<ObjectDomain, &[u8]>::new(capacity)
        .map_err(GenerationBuildError::StoreInitialization)?;
    let mut entries = root.closure();
    insert(
        &mut store,
        next_object(&mut entries, MANIFEST_ENTRY_KEY)?,
        manifest.as_ref(),
    )?;
    let mut offset = 0_usize;
    for (ordinal, facts) in manifest.fragments().enumerate() {
        let key = u64::try_from(ordinal)
            .map_err(|source| GenerationBuildError::EntryKeyAddressSpace { ordinal, source })?
            .checked_add(1)
            .ok_or(GenerationBuildError::EntryCountOverflow)?;
        let bytes = next_fragment_bytes(fragment_bytes, &mut offset, facts)?;
        insert(&mut store, next_object(&mut entries, key)?, bytes)?;
    }
    if offset != fragment_bytes.len() {
        return Err(GenerationBuildError::ReopenedBytesLength {
            expected: offset,
            observed: fragment_bytes.len(),
        });
    }
    if entries.next().is_some() {
        return Err(GenerationBuildError::RootClosureExtra);
    }
    verify_generation(&root, &store, locality_output)
}

#[allow(
    clippy::result_large_err,
    reason = "cold root build retains exact sources"
)]
fn build_root<'input, 'scratch, 'fragment, 'manifest, 'facts>(
    canonical: &CanonicalCompilation<'input, 'scratch, 'fragment>,
    manifest: &CompilationManifestView<'manifest, 'facts>,
) -> Result<GenerationRoot<ObjectDomain>, GenerationBuildError> {
    let entry_count = canonical
        .fragments()
        .count()
        .checked_add(1)
        .ok_or(GenerationBuildError::EntryCountOverflow)?;
    let mut builder = GenerationRootBuilder::with_capacity(entry_count)
        .map_err(GenerationBuildError::RootReservation)?;
    push(
        &mut builder,
        RootEntry {
            key: MANIFEST_ENTRY_KEY.into(),
            parent: None,
            object: manifest_object(manifest),
        },
    )?;
    for (ordinal, (compiled, facts)) in canonical.fragments().zip(manifest.fragments()).enumerate()
    {
        let key = u64::try_from(ordinal)
            .map_err(|source| GenerationBuildError::EntryKeyAddressSpace { ordinal, source })?
            .checked_add(1)
            .ok_or(GenerationBuildError::EntryCountOverflow)?;
        push(
            &mut builder,
            RootEntry {
                key: key.into(),
                parent: Some(MANIFEST_ENTRY_KEY.into()),
                object: fragment_object(compiled.fragment.as_ref(), facts),
            },
        )?;
    }
    builder.finish().map_err(GenerationBuildError::Root)
}

#[allow(
    clippy::result_large_err,
    reason = "cold root build retains exact sources"
)]
fn build_reopened_root(
    manifest: &CompilationManifestView<'_, '_>,
    fragment_bytes: &[u8],
) -> Result<GenerationRoot<ObjectDomain>, GenerationBuildError> {
    let entry_count = manifest
        .fragments()
        .count()
        .checked_add(1)
        .ok_or(GenerationBuildError::EntryCountOverflow)?;
    let mut builder = GenerationRootBuilder::with_capacity(entry_count)
        .map_err(GenerationBuildError::RootReservation)?;
    push(
        &mut builder,
        RootEntry {
            key: MANIFEST_ENTRY_KEY.into(),
            parent: None,
            object: manifest_object(manifest),
        },
    )?;
    let mut offset = 0_usize;
    for (ordinal, facts) in manifest.fragments().enumerate() {
        let key = u64::try_from(ordinal)
            .map_err(|source| GenerationBuildError::EntryKeyAddressSpace { ordinal, source })?
            .checked_add(1)
            .ok_or(GenerationBuildError::EntryCountOverflow)?;
        let bytes = next_fragment_bytes(fragment_bytes, &mut offset, facts)?;
        push(
            &mut builder,
            RootEntry {
                key: key.into(),
                parent: Some(MANIFEST_ENTRY_KEY.into()),
                object: fragment_object(bytes, facts),
            },
        )?;
    }
    if offset != fragment_bytes.len() {
        return Err(GenerationBuildError::ReopenedBytesLength {
            expected: offset,
            observed: fragment_bytes.len(),
        });
    }
    builder.finish().map_err(GenerationBuildError::Root)
}

#[allow(
    clippy::result_large_err,
    reason = "cold root build retains exact sources"
)]
fn push(
    builder: &mut GenerationRootBuilder<ObjectDomain>,
    entry: RootEntry<ObjectDomain>,
) -> Result<(), GenerationBuildError> {
    builder
        .try_push(entry)
        .map_err(|rejected| GenerationBuildError::RootPush {
            object: rejected.entry.object,
            source: rejected.error,
        })
}

#[allow(
    clippy::result_large_err,
    reason = "cold root build retains exact sources"
)]
fn next_object(
    entries: &mut impl Iterator<Item = RootEntry<ObjectDomain>>,
    expected_key: u64,
) -> Result<ObjectRef<ObjectDomain>, GenerationBuildError> {
    let Some(entry) = entries.next() else {
        return Err(GenerationBuildError::RootClosureMissing { expected_key });
    };
    if *entry.key != expected_key {
        return Err(GenerationBuildError::RootClosureKey {
            expected: expected_key,
            observed: *entry.key,
        });
    }
    Ok(entry.object)
}

#[allow(
    clippy::result_large_err,
    reason = "cold root build retains exact sources"
)]
fn next_fragment_bytes<'bytes>(
    all_bytes: &'bytes [u8],
    offset: &mut usize,
    facts: StoredFragmentFacts,
) -> Result<&'bytes [u8], GenerationBuildError> {
    let length = usize::try_from(facts.fragment_length).map_err(|source| {
        GenerationBuildError::FragmentLengthAddressSpace {
            observed: facts.fragment_length,
            source,
        }
    })?;
    let end = offset
        .checked_add(length)
        .ok_or(GenerationBuildError::ReopenedBytesLengthOverflow)?;
    let Some(bytes) = all_bytes.get(*offset..end) else {
        return Err(GenerationBuildError::ReopenedBytesLength {
            expected: end,
            observed: all_bytes.len(),
        });
    };
    *offset = end;
    Ok(bytes)
}

#[allow(
    clippy::result_large_err,
    reason = "cold root build retains exact sources"
)]
fn store_capacity<'manifest, 'facts>(
    manifest: &CompilationManifestView<'manifest, 'facts>,
) -> Result<StoreCapacity, GenerationBuildError> {
    let mut bytes = u64::from(manifest.byte_length);
    let mut slots = 1_u32;
    for facts in manifest.fragments() {
        bytes = bytes
            .checked_add(u64::from(facts.fragment_length))
            .ok_or(GenerationBuildError::StoreBytesOverflow)?;
        slots = slots
            .checked_add(1)
            .ok_or(GenerationBuildError::StoreSlotsOverflow)?;
    }
    Ok(StoreCapacity {
        bytes: bytes.into(),
        slots: slots.into(),
    })
}

#[allow(
    clippy::result_large_err,
    reason = "cold generation verification retains exact sources"
)]
fn verify_generation(
    root: &GenerationRoot<ObjectDomain>,
    store: &MemoryStore<ObjectDomain, &[u8]>,
    locality_output: &mut [u8],
) -> Result<nudox_hydration::VerifiedGenerationFacts, GenerationBuildError> {
    let prepared = PreparedLocality::prepare(root, &[]).map_err(GenerationBuildError::Locality)?;
    let locality = prepared
        .write(locality_output)
        .map_err(GenerationBuildError::LocalityWrite)?;
    let view = GenerationView::new(root, &locality).map_err(GenerationBuildError::Locality)?;
    let mut closure =
        ClosureScratch::new(root.len()).map_err(GenerationBuildError::ClosureReservation)?;
    let mut planning =
        PlanScratch::new(root.entry_count.into()).map_err(GenerationBuildError::PlanReservation)?;
    let plan = plan(
        demand(&view, Projection::CompleteGeneration),
        &mut closure,
        &mut planning,
        |_| true,
    )
    .map_err(GenerationBuildError::Plan)?;
    let verified = plan
        .stage()
        .verify_store(store)
        .map_err(GenerationBuildError::Verification)?;
    Ok(*verified)
}

#[allow(
    clippy::result_large_err,
    reason = "cold generation insertion retains exact sources"
)]
fn insert<'bytes>(
    store: &mut MemoryStore<ObjectDomain, &'bytes [u8]>,
    object: ObjectRef<ObjectDomain>,
    bytes: &'bytes [u8],
) -> Result<(), GenerationBuildError> {
    match store.insert_owned(object, bytes) {
        Ok(InsertOutcome::Inserted) => Ok(()),
        Ok(InsertOutcome::AlreadyPresent) => Err(GenerationBuildError::DuplicateObject { object }),
        Err(rejected) => Err(rejected_insert(rejected)),
    }
}

fn rejected_insert(rejected: RejectedInsert<ObjectDomain, &[u8]>) -> GenerationBuildError {
    GenerationBuildError::StoreInsert {
        object: rejected.reference,
        source: rejected.error,
    }
}

fn manifest_object(manifest: &CompilationManifestView<'_, '_>) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::<ObjectDomain>::from_canonical_bytes(manifest.as_ref()),
        length: ObjectLength::from(u64::from(manifest.byte_length)),
        schema: SchemaId::CompilationManifest,
        kind: ObjectKind::from(MANIFEST_KIND),
    }
}

fn fragment_object(bytes: &[u8], facts: StoredFragmentFacts) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::<ObjectDomain>::from_canonical_bytes(bytes),
        length: ObjectLength::from(u64::from(facts.fragment_length)),
        schema: SchemaId::IrFragment,
        kind: ObjectKind::from(FRAGMENT_KIND),
    }
}

/// Failure while constructing a complete root and store over one canonical package.
#[derive(Debug, Error)]
#[non_exhaustive]
#[allow(
    missing_docs,
    reason = "each field repeats the exact construction fact documented by its enclosing terminal"
)]
pub enum GenerationBuildError {
    #[error("compiler package root entry count overflowed native construction capacity")]
    EntryCountOverflow,
    #[error("compiler package fragment ordinal {ordinal} cannot fit a generation entry key")]
    EntryKeyAddressSpace {
        ordinal: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error("stored compiler fragment length {observed} cannot fit this address space")]
    FragmentLengthAddressSpace {
        observed: u32,
        #[source]
        source: TryFromIntError,
    },
    #[error("stored compiler fragment byte extent overflowed while rebuilding the generation")]
    ReopenedBytesLengthOverflow,
    #[error("stored compiler fragment bytes have {observed} bytes, require {expected}")]
    ReopenedBytesLength { expected: usize, observed: usize },
    #[error("could not reserve the bounded compiler generation root")]
    RootReservation(#[source] TryReserveError),
    #[error("compiler generation root rejected a preflighted entry")]
    RootPush {
        object: ObjectRef<ObjectDomain>,
        #[source]
        source: RootPushError,
    },
    #[error("could not finish the canonical compiler generation root")]
    Root(#[source] RootBuildError),
    #[error("compiler generation root closure omitted expected key {expected_key}")]
    RootClosureMissing { expected_key: u64 },
    #[error("compiler generation root closure key {observed} differs from expected {expected}")]
    RootClosureKey { expected: u64, observed: u64 },
    #[error("compiler generation root closure retained an unexpected extra entry")]
    RootClosureExtra,
    #[error("compiler package immutable byte capacity overflowed u64")]
    StoreBytesOverflow,
    #[error("compiler package object closure exceeds compact store slots")]
    StoreSlotsOverflow,
    #[error("could not initialize the bounded compiler object store")]
    StoreInitialization(#[source] StoreInitError),
    #[error("compiler package object {object:?} could not enter the exact object store")]
    StoreInsert {
        object: ObjectRef<ObjectDomain>,
        #[source]
        source: StoreError<ObjectDomain>,
    },
    #[error("compiler package root repeated object descriptor {object:?}")]
    DuplicateObject { object: ObjectRef<ObjectDomain> },
    #[error("could not prepare the all-resident compiler generation locality")]
    Locality(#[source] nudox_root::LocalityError),
    #[error("caller locality output could not encode the all-resident compiler generation")]
    LocalityWrite(#[source] nudox_root::LocalityWriteError),
    #[error("could not reserve compiler generation closure scratch")]
    ClosureReservation(#[source] TryReserveError),
    #[error("could not reserve compiler generation plan scratch")]
    PlanReservation(#[source] TryReserveError),
    #[error("could not build the complete compiler generation hydration plan")]
    Plan(#[source] PlanError),
    #[error("compiler generation closure did not verify against its exact object store")]
    Verification(#[source] VerificationError<ObjectDomain>),
}
