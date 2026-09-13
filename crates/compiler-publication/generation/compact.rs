//! Compact-only complete-generation construction and reopen verification.
//!
//! These paths intentionally retain the legacy one-fragment-per-entry closure.
//! Paired compact/semantic generation construction lives in the sibling module so
//! its two-entry-per-artifact topology cannot accidentally enter this grammar.

use heart_hydration::{PlanScratch, Projection, VerifiedGeneration, demand, plan};
use heart_identity::ObjectDomain;
use heart_memory::MemoryStore;
use heart_root::{
    ClosureScratch, GenerationRoot, GenerationRootBuilder, GenerationView, PreparedLocality,
    RootEntry,
};

use crate::manifest::{CanonicalCompilation, CompilationManifestView};

use super::{
    GenerationBuildError, MANIFEST_ENTRY_KEY, fragment_object, insert, manifest_object,
    next_fragment_bytes, next_object, push, store_capacity, verify_generation,
};

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

/// Rebuilds the exact complete generation selected by a persisted compact compiler package.
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
) -> Result<heart_hydration::VerifiedGenerationFacts, GenerationBuildError> {
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
