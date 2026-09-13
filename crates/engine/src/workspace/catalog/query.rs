//! Exact and semantic catalog lookups over owner-retained lazy roots.

use super::super::owner::WorkspaceError;
use super::proof::{
    CatalogRecord, DerivedOutputEntry, DerivedOutputProof, class_from_tag, manifest_version,
};
use super::relation::{CatalogState, PrimaryKey, PrimaryRelation};
use super::storage::{lookup_exact, lookup_lower_bound};
use super::{
    BYTES_SCHEMA_TYPE, DerivedOutputBytesSchema, DerivedOutputManifestSchema, MANIFEST_SCHEMA_TYPE,
    PRIMARY_KEY_BYTES, SEMANTIC_KEY_PREFIX_BYTES, catalog_schema,
};
use backend_execution::{OutputVersion, WorkKey};
use backend_store::{FileStore, UntrustedObjectId};
use backend_version::ObjectKey;
use std::sync::Arc;

/// Finds the newest exact entry for a semantic/authority binding. The
/// generation is selected from the authenticated primary relation rather than
/// accepted from the caller.
/// Finds the newest entry from the owner-retained authenticated catalog
/// state.  The state already carries the descriptor/root identity selected by
/// the workspace head, so a hot lookup never scans the compatibility closure
/// export to rediscover it.
#[derive(Clone, Copy)]
pub(crate) struct LatestQuery<'a> {
    pub(crate) key: WorkKey,
    pub(crate) dependency_manifest: &'a [u8],
    pub(crate) authority: [u8; 32],
    pub(crate) authority_epoch: u64,
    pub(crate) revocation_version: u64,
    pub(crate) coverage_identity: [u8; 32],
    pub(crate) scope: u64,
    pub(crate) read_manifest: [u8; 32],
}

pub(crate) fn find_latest_in_state(
    store: &FileStore,
    state: &CatalogState,
    query: &LatestQuery<'_>,
) -> Result<Option<DerivedOutputEntry>, WorkspaceError> {
    let query = *query;
    let LatestQuery {
        key,
        dependency_manifest,
        authority,
        authority_epoch,
        revocation_version,
        coverage_identity,
        scope,
        read_manifest,
    } = query;
    let manifest_version = manifest_version(dependency_manifest);
    let mut prefix = [0u8; SEMANTIC_KEY_PREFIX_BYTES];
    let mut at = 0;
    prefix[at..at + 32].copy_from_slice(&key.to_bytes());
    at += 32;
    prefix[at..at + 32].copy_from_slice(&manifest_version);
    at += 32;
    prefix[at..at + 32].copy_from_slice(&authority);
    at += 32;
    prefix[at..at + 8].copy_from_slice(&authority_epoch.to_be_bytes());
    at += 8;
    prefix[at..at + 8].copy_from_slice(&revocation_version.to_be_bytes());
    at += 8;
    prefix[at..at + 32].copy_from_slice(&coverage_identity);
    at += 32;
    prefix[at..at + 8].copy_from_slice(&scope.to_be_bytes());
    at += 8;
    prefix[at..at + 32].copy_from_slice(&read_manifest);
    at += 32;
    debug_assert_eq!(at, SEMANTIC_KEY_PREFIX_BYTES);
    let record = latest_record(
        store,
        state,
        prefix,
        coverage_identity,
        scope,
        read_manifest,
    )?;
    record
        .map(|record| entry_from_record(store, &record, key))
        .transpose()
}

/// Finds one exact staged proof using the owner-retained catalog roots.
pub(crate) fn find_proof_in_state(
    store: &FileStore,
    state: &CatalogState,
    proof: &DerivedOutputProof,
) -> Result<Option<DerivedOutputEntry>, WorkspaceError> {
    let manifest_bytes = proof.dependency_manifest.canonical_bytes();
    let manifest_version = manifest_version(&manifest_bytes);
    let primary_key = primary_key_for(&PrimaryKeyInput {
        key: proof.key,
        manifest_version,
        authority: proof.authority.to_bytes(),
        authority_epoch: proof.authority_epoch,
        revocation_version: proof.revocation_version,
        dependency_generation: proof.dependency_generation,
        coverage_identity: proof.coverage_identity,
        scope: proof.scope,
        read_manifest: proof.read_manifest,
    });
    let record = exact_record(store, state, primary_key)?;
    let Some(record) = record else {
        return Ok(None);
    };
    if record.request != proof.request || record.output != proof.output.to_bytes() {
        return Err(WorkspaceError::Corrupt(
            "conflicting derived output entries",
        ));
    }
    let entry = entry_from_record(store, &record, proof.key)?;
    if entry.dependency_manifest_bytes() != manifest_bytes.as_slice() {
        return Err(WorkspaceError::Corrupt("derived output manifest mismatch"));
    }
    if entry.authority_class() != proof.authority_class {
        return Err(WorkspaceError::Corrupt(
            "derived output authority class mismatch",
        ));
    }
    Ok(Some(entry))
}

fn latest_record(
    store: &FileStore,
    state: &CatalogState,
    prefix: [u8; SEMANTIC_KEY_PREFIX_BYTES],
    coverage_identity: [u8; 32],
    scope: u64,
    read_manifest: [u8; 32],
) -> Result<Option<CatalogRecord>, WorkspaceError> {
    let mut lower = [0u8; PRIMARY_KEY_BYTES];
    lower[..SEMANTIC_KEY_PREFIX_BYTES].copy_from_slice(&prefix);
    match state {
        CatalogState::Empty { .. } => Ok(None),
        CatalogState::Persisted { descriptor, .. } => {
            let Some((found_key, found_value)) = lookup_lower_bound::<PrimaryRelation>(
                store,
                descriptor.primary_object,
                descriptor.primary_root,
                &PrimaryKey(lower),
            )?
            else {
                return Ok(None);
            };
            if found_key.0[..SEMANTIC_KEY_PREFIX_BYTES] != prefix {
                return Ok(None);
            }
            let record = CatalogRecord::from_pair(found_key, &found_value)?;
            if record.coverage_identity != coverage_identity
                || record.scope != scope
                || record.read_manifest != read_manifest
            {
                return Ok(None);
            }
            Ok(Some(record))
        }
    }
}

#[derive(Clone, Copy)]
struct PrimaryKeyInput {
    key: WorkKey,
    manifest_version: [u8; 32],
    authority: [u8; 32],
    authority_epoch: u64,
    revocation_version: u64,
    dependency_generation: u64,
    coverage_identity: [u8; 32],
    scope: u64,
    read_manifest: [u8; 32],
}

fn primary_key_for(input: &PrimaryKeyInput) -> PrimaryKey {
    let input = *input;
    let PrimaryKeyInput {
        key,
        manifest_version,
        authority,
        authority_epoch,
        revocation_version,
        dependency_generation,
        coverage_identity,
        scope,
        read_manifest,
    } = input;
    let mut bytes = [0u8; PRIMARY_KEY_BYTES];
    let mut at = 0;
    bytes[at..at + 32].copy_from_slice(&key.to_bytes());
    at += 32;
    bytes[at..at + 32].copy_from_slice(&manifest_version);
    at += 32;
    bytes[at..at + 32].copy_from_slice(&authority);
    at += 32;
    bytes[at..at + 8].copy_from_slice(&authority_epoch.to_be_bytes());
    at += 8;
    bytes[at..at + 8].copy_from_slice(&revocation_version.to_be_bytes());
    at += 8;
    bytes[at..at + 32].copy_from_slice(&coverage_identity);
    at += 32;
    bytes[at..at + 8].copy_from_slice(&scope.to_be_bytes());
    at += 8;
    bytes[at..at + 32].copy_from_slice(&read_manifest);
    at += 32;
    bytes[at..at + 8].copy_from_slice(&(u64::MAX - dependency_generation).to_be_bytes());
    PrimaryKey(bytes)
}

fn exact_record(
    store: &FileStore,
    state: &CatalogState,
    key: PrimaryKey,
) -> Result<Option<CatalogRecord>, WorkspaceError> {
    match state {
        CatalogState::Empty { .. } => Ok(None),
        CatalogState::Persisted { descriptor, .. } => {
            let value = lookup_exact::<PrimaryRelation>(
                store,
                descriptor.primary_object,
                descriptor.primary_root,
                &key,
            )?;
            value
                .map(|value| CatalogRecord::from_pair(key, &value))
                .transpose()
        }
    }
}

fn entry_from_record(
    store: &FileStore,
    record: &CatalogRecord,
    key: WorkKey,
) -> Result<DerivedOutputEntry, WorkspaceError> {
    if record.key != key.to_bytes() || record.dependency_generation == 0 {
        return Err(WorkspaceError::Corrupt("derived output key binding"));
    }
    let output_object = store
        .read_object_claim(UntrustedObjectId::from_bytes(record.output_object))
        .map_err(WorkspaceError::store)?;
    let output_bytes = output_object.bytes();
    if output_object.schema() != catalog_schema(BYTES_SCHEMA_TYPE)
        || OutputVersion::from_value(output_bytes).to_bytes() != record.output
        || *output_object.key()
            != ObjectKey::<DerivedOutputBytesSchema>::from_value(output_bytes).to_bytes()
        || *output_object.id().as_bytes() != record.output_object
    {
        return Err(WorkspaceError::Corrupt("derived output object mismatch"));
    }
    let manifest_object = store
        .read_object_claim(UntrustedObjectId::from_bytes(record.manifest_object))
        .map_err(WorkspaceError::store)?;
    if manifest_object.schema() != catalog_schema(MANIFEST_SCHEMA_TYPE)
        || *manifest_object.id().as_bytes() != record.manifest_object
    {
        return Err(WorkspaceError::Corrupt("derived manifest object mismatch"));
    }
    let manifest_bytes = manifest_object.bytes();
    if *manifest_object.key()
        != ObjectKey::<DerivedOutputManifestSchema>::from_value(manifest_bytes).to_bytes()
        || manifest_version(manifest_bytes) != record.manifest_version
    {
        return Err(WorkspaceError::Corrupt(
            "derived manifest identity mismatch",
        ));
    }
    Ok(DerivedOutputEntry {
        key,
        output: OutputVersion::from_value(output_bytes),
        bytes: Arc::new(output_bytes.to_vec()),
        dependency_manifest: Arc::from(manifest_bytes.to_vec().into_boxed_slice()),
        dependency_manifest_version: record.manifest_version,
        coverage_identity: record.coverage_identity,
        scope: record.scope,
        read_manifest: record.read_manifest,
        authority: record.authority,
        authority_epoch: record.authority_epoch,
        revocation_version: record.revocation_version,
        dependency_generation: record.dependency_generation,
        authority_class: class_from_tag(record.authority_class)?,
    })
}
