//! Defines generation behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the generation invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Complete-generation construction over a canonical compiler package.

use core::num::TryFromIntError;
use std::collections::TryReserveError;

use heart_hydration::{PlanError, PlanScratch, Projection, VerificationError, demand, plan};
use backend_version::{ContentId, ObjectDomain};
use backend_store::memory::{
    InsertOutcome, MemoryStore, RejectedInsert, StoreCapacity, StoreError, StoreInitError,
};
use backend_version::object::{ObjectKind, ObjectLength, ObjectRef};
use heart_root::{
    ClosureScratch, GenerationRoot, GenerationRootBuilder, GenerationView, PreparedLocality,
    RootBuildError, RootEntry, RootPushError,
};
use backend_version::schema::SchemaId;
use thiserror::Error;

use crate::manifest::{CompilationManifestView, StoredFragmentFacts};

pub(super) const MANIFEST_ENTRY_KEY: u64 = 0;
const MANIFEST_KIND: u16 = 1;
const FRAGMENT_KIND: u16 = 1;
const SEMANTIC_IMAGE_KIND: u16 = 2;

mod compact;
mod semantic;

pub(crate) use compact::{verify_reopened_generation, with_verified_generation};
pub(crate) use semantic::{verify_reopened_semantic_generation, with_verified_semantic_generation};

#[allow(
    clippy::result_large_err,
    reason = "cold root build retains exact sources"
)]
pub(super) fn push(
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
pub(super) fn next_object(
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
pub(super) fn next_fragment_bytes<'bytes>(
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

pub(super) fn next_semantic_image_bytes<'bytes>(
    all_bytes: &'bytes [u8],
    offset: &mut usize,
    ordinal: usize,
    facts: StoredFragmentFacts,
) -> Result<&'bytes [u8], GenerationBuildError> {
    let semantic = facts
        .semantic_image
        .ok_or(GenerationBuildError::MissingSemanticImage { ordinal })?;
    let length = usize::try_from(semantic.byte_length).map_err(|source| {
        GenerationBuildError::SemanticImageLengthAddressSpace {
            observed: semantic.byte_length,
            source,
        }
    })?;
    let end = offset
        .checked_add(length)
        .ok_or(GenerationBuildError::ReopenedSemanticBytesLengthOverflow)?;
    let bytes =
        all_bytes
            .get(*offset..end)
            .ok_or(GenerationBuildError::ReopenedSemanticBytesLength {
                expected: end,
                observed: all_bytes.len(),
            })?;
    *offset = end;
    Ok(bytes)
}

#[allow(
    clippy::result_large_err,
    reason = "cold root build retains exact sources"
)]
pub(super) fn store_capacity<'manifest, 'facts>(
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
        if let Some(semantic) = facts.semantic_image {
            bytes = bytes
                .checked_add(u64::from(semantic.byte_length))
                .ok_or(GenerationBuildError::StoreBytesOverflow)?;
            slots = slots
                .checked_add(1)
                .ok_or(GenerationBuildError::StoreSlotsOverflow)?;
        }
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
pub(super) fn verify_generation(
    root: &GenerationRoot<ObjectDomain>,
    store: &MemoryStore<ObjectDomain, &[u8]>,
    locality_output: &mut [u8],
) -> Result<heart_hydration::VerifiedGenerationFacts, GenerationBuildError> {
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
pub(super) fn insert<'bytes>(
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

pub(super) fn manifest_object(
    manifest: &CompilationManifestView<'_, '_>,
) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::<ObjectDomain>::from_canonical_bytes(manifest.as_ref()),
        length: ObjectLength::from(u64::from(manifest.byte_length)),
        schema: SchemaId::CompilationManifest,
        kind: ObjectKind::from(MANIFEST_KIND),
    }
}

pub(super) fn fragment_object(bytes: &[u8], facts: StoredFragmentFacts) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::<ObjectDomain>::from_canonical_bytes(bytes),
        length: ObjectLength::from(u64::from(facts.fragment_length)),
        schema: SchemaId::IrFragment,
        kind: ObjectKind::from(FRAGMENT_KIND),
    }
}

pub(super) fn semantic_image_object(bytes: &[u8], length: u32) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::<ObjectDomain>::from_canonical_bytes(bytes),
        length: ObjectLength::from(u64::from(length)),
        schema: SchemaId::IrSemanticImage,
        kind: ObjectKind::from(SEMANTIC_IMAGE_KIND),
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
    #[error(
        "semantic image {ordinal} at {offset} with length {length} exceeds {available} prepared bytes"
    )]
    SemanticImageBytes {
        ordinal: usize,
        offset: usize,
        length: u32,
        available: usize,
    },
    #[error("stored compiler fragment length {observed} cannot fit this address space")]
    FragmentLengthAddressSpace {
        observed: u32,
        #[source]
        source: TryFromIntError,
    },
    #[error("semantic manifest entry {ordinal} omitted its complete image fact")]
    MissingSemanticImage { ordinal: usize },
    #[error("stored semantic image length {observed} cannot fit this address space")]
    SemanticImageLengthAddressSpace {
        observed: u32,
        #[source]
        source: TryFromIntError,
    },
    #[error("stored compiler fragment byte extent overflowed while rebuilding the generation")]
    ReopenedBytesLengthOverflow,
    #[error("stored compiler fragment bytes have {observed} bytes, require {expected}")]
    ReopenedBytesLength { expected: usize, observed: usize },
    #[error("stored semantic-image byte extent overflowed while rebuilding the generation")]
    ReopenedSemanticBytesLengthOverflow,
    #[error("stored semantic-image bytes have {observed} bytes, require {expected}")]
    ReopenedSemanticBytesLength { expected: usize, observed: usize },
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
    Locality(#[source] heart_root::LocalityError),
    #[error("caller locality output could not encode the all-resident compiler generation")]
    LocalityWrite(#[source] heart_root::LocalityWriteError),
    #[error("could not reserve compiler generation closure scratch")]
    ClosureReservation(#[source] TryReserveError),
    #[error("could not reserve compiler generation plan scratch")]
    PlanReservation(#[source] TryReserveError),
    #[error("could not build the complete compiler generation hydration plan")]
    Plan(#[source] PlanError),
    #[error("compiler generation closure did not verify against its exact object store")]
    Verification(#[source] VerificationError<ObjectDomain>),
}
