//! Paired compact/semantic complete-generation construction and reopen verification.
//!
//! One artifact always occupies two adjacent child entries: compact fragment first,
//! then its complete semantic image.  Keeping that topology here prevents a compact
//! caller from accidentally proving only half of a semantic publication.

use backend_store::hydration::{PlanScratch, Projection, VerifiedGeneration, demand, plan};
use backend_version::ObjectDomain;
use backend_store::memory::MemoryStore;
use backend_store::root::{
    ClosureScratch, GenerationRoot, GenerationRootBuilder, GenerationView, PreparedLocality,
    RootEntry,
};

use crate::manifest::{CanonicalSemanticCompilation, CompilationManifestView};

use super::{
    GenerationBuildError, MANIFEST_ENTRY_KEY, fragment_object, insert, manifest_object,
    next_fragment_bytes, next_object, next_semantic_image_bytes, push, semantic_image_object,
    store_capacity, verify_generation,
};

/// Runs a continuation while holding a complete-generation witness over each
/// compact fragment and its paired full semantic image.
#[allow(
    clippy::result_large_err,
    reason = "cold generation construction retains exact sources"
)]
pub(crate) fn with_verified_semantic_generation<
    'input,
    'scratch,
    'fragment,
    'images,
    'manifest,
    'facts,
    Output,
>(
    canonical: &CanonicalSemanticCompilation<'input, 'scratch, 'fragment, 'images>,
    manifest: &CompilationManifestView<'manifest, 'facts>,
    semantic_bytes: &[u8],
    locality_output: &mut [u8],
    visit: impl FnOnce(VerifiedGeneration<'_, ObjectDomain, &'_ [u8]>) -> Output,
) -> Result<Output, GenerationBuildError> {
    let root = build_semantic_root(canonical, manifest, semantic_bytes)?;
    let capacity = store_capacity(manifest)?;
    let mut store = MemoryStore::<ObjectDomain, &[u8]>::new(capacity)
        .map_err(GenerationBuildError::StoreInitialization)?;
    let mut entries = root.closure();
    insert(
        &mut store,
        next_object(&mut entries, MANIFEST_ENTRY_KEY)?,
        manifest.as_ref(),
    )?;
    for (ordinal, (compiled, prepared)) in canonical.artifacts().enumerate() {
        let fragment_key = semantic_fragment_key(ordinal)?;
        insert(
            &mut store,
            next_object(&mut entries, fragment_key)?,
            compiled.artifact.fragment.as_ref(),
        )?;
        let image_key = fragment_key
            .checked_add(1)
            .ok_or(GenerationBuildError::EntryCountOverflow)?;
        let bytes =
            prepared
                .bytes(semantic_bytes)
                .ok_or(GenerationBuildError::SemanticImageBytes {
                    ordinal,
                    offset: prepared.offset,
                    length: prepared.byte_length,
                    available: semantic_bytes.len(),
                })?;
        insert(&mut store, next_object(&mut entries, image_key)?, bytes)?;
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

/// Rebuilds and verifies a schema-2 generation from caller-owned compact and
/// semantic artifact bytes in canonical manifest order.
#[allow(
    clippy::result_large_err,
    reason = "cold generation reconstruction retains exact sources"
)]
pub(crate) fn verify_reopened_semantic_generation(
    manifest: &CompilationManifestView<'_, '_>,
    fragment_bytes: &[u8],
    semantic_bytes: &[u8],
    locality_output: &mut [u8],
) -> Result<backend_store::hydration::VerifiedGenerationFacts, GenerationBuildError> {
    let root = build_reopened_semantic_root(manifest, fragment_bytes, semantic_bytes)?;
    let capacity = store_capacity(manifest)?;
    let mut store = MemoryStore::<ObjectDomain, &[u8]>::new(capacity)
        .map_err(GenerationBuildError::StoreInitialization)?;
    let mut entries = root.closure();
    insert(
        &mut store,
        next_object(&mut entries, MANIFEST_ENTRY_KEY)?,
        manifest.as_ref(),
    )?;
    let mut fragment_offset = 0_usize;
    let mut semantic_offset = 0_usize;
    for (ordinal, facts) in manifest.fragments().enumerate() {
        let fragment_key = semantic_fragment_key(ordinal)?;
        let fragment = next_fragment_bytes(fragment_bytes, &mut fragment_offset, facts)?;
        insert(
            &mut store,
            next_object(&mut entries, fragment_key)?,
            fragment,
        )?;
        let semantic =
            next_semantic_image_bytes(semantic_bytes, &mut semantic_offset, ordinal, facts)?;
        insert(
            &mut store,
            next_object(
                &mut entries,
                fragment_key
                    .checked_add(1)
                    .ok_or(GenerationBuildError::EntryCountOverflow)?,
            )?,
            semantic,
        )?;
    }
    if fragment_offset != fragment_bytes.len() {
        return Err(GenerationBuildError::ReopenedBytesLength {
            expected: fragment_offset,
            observed: fragment_bytes.len(),
        });
    }
    if semantic_offset != semantic_bytes.len() {
        return Err(GenerationBuildError::ReopenedSemanticBytesLength {
            expected: semantic_offset,
            observed: semantic_bytes.len(),
        });
    }
    if entries.next().is_some() {
        return Err(GenerationBuildError::RootClosureExtra);
    }
    verify_generation(&root, &store, locality_output)
}

fn build_semantic_root<'input, 'scratch, 'fragment, 'images>(
    canonical: &CanonicalSemanticCompilation<'input, 'scratch, 'fragment, 'images>,
    manifest: &CompilationManifestView<'_, '_>,
    semantic_bytes: &[u8],
) -> Result<GenerationRoot<ObjectDomain>, GenerationBuildError> {
    let entry_count = canonical
        .artifacts()
        .count()
        .checked_mul(2)
        .and_then(|count| count.checked_add(1))
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
    for (ordinal, ((compiled, prepared), facts)) in
        canonical.artifacts().zip(manifest.fragments()).enumerate()
    {
        let fragment_key = semantic_fragment_key(ordinal)?;
        push(
            &mut builder,
            RootEntry {
                key: fragment_key.into(),
                parent: Some(MANIFEST_ENTRY_KEY.into()),
                object: fragment_object(compiled.artifact.fragment.as_ref(), facts),
            },
        )?;
        let bytes =
            prepared
                .bytes(semantic_bytes)
                .ok_or(GenerationBuildError::SemanticImageBytes {
                    ordinal,
                    offset: prepared.offset,
                    length: prepared.byte_length,
                    available: semantic_bytes.len(),
                })?;
        push(
            &mut builder,
            RootEntry {
                key: fragment_key
                    .checked_add(1)
                    .ok_or(GenerationBuildError::EntryCountOverflow)?
                    .into(),
                parent: Some(MANIFEST_ENTRY_KEY.into()),
                object: semantic_image_object(bytes, prepared.byte_length),
            },
        )?;
    }
    builder.finish().map_err(GenerationBuildError::Root)
}

fn build_reopened_semantic_root(
    manifest: &CompilationManifestView<'_, '_>,
    fragment_bytes: &[u8],
    semantic_bytes: &[u8],
) -> Result<GenerationRoot<ObjectDomain>, GenerationBuildError> {
    let entry_count = manifest
        .fragments()
        .count()
        .checked_mul(2)
        .and_then(|count| count.checked_add(1))
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
    let mut fragment_offset = 0_usize;
    let mut semantic_offset = 0_usize;
    for (ordinal, facts) in manifest.fragments().enumerate() {
        let fragment_key = semantic_fragment_key(ordinal)?;
        let fragment = next_fragment_bytes(fragment_bytes, &mut fragment_offset, facts)?;
        push(
            &mut builder,
            RootEntry {
                key: fragment_key.into(),
                parent: Some(MANIFEST_ENTRY_KEY.into()),
                object: fragment_object(fragment, facts),
            },
        )?;
        let semantic =
            next_semantic_image_bytes(semantic_bytes, &mut semantic_offset, ordinal, facts)?;
        let semantic_facts = facts
            .semantic_image
            .ok_or(GenerationBuildError::MissingSemanticImage { ordinal })?;
        push(
            &mut builder,
            RootEntry {
                key: fragment_key
                    .checked_add(1)
                    .ok_or(GenerationBuildError::EntryCountOverflow)?
                    .into(),
                parent: Some(MANIFEST_ENTRY_KEY.into()),
                object: semantic_image_object(semantic, semantic_facts.byte_length),
            },
        )?;
    }
    if fragment_offset != fragment_bytes.len() {
        return Err(GenerationBuildError::ReopenedBytesLength {
            expected: fragment_offset,
            observed: fragment_bytes.len(),
        });
    }
    if semantic_offset != semantic_bytes.len() {
        return Err(GenerationBuildError::ReopenedSemanticBytesLength {
            expected: semantic_offset,
            observed: semantic_bytes.len(),
        });
    }
    builder.finish().map_err(GenerationBuildError::Root)
}

fn semantic_fragment_key(ordinal: usize) -> Result<u64, GenerationBuildError> {
    u64::try_from(ordinal)
        .map_err(|source| GenerationBuildError::EntryKeyAddressSpace { ordinal, source })?
        .checked_mul(2)
        .and_then(|key| key.checked_add(1))
        .ok_or(GenerationBuildError::EntryCountOverflow)
}
