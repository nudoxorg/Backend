//! Durable path-copy catalog publication.

use super::super::owner::WorkspaceError;
use super::MAX_CATALOG_ENTRIES;
use super::proof::{CatalogRecord, DerivedOutputProof};
use super::relation::{
    CatalogState, FreshnessKey, FreshnessRelation, InitialCatalog, PayloadKey, PayloadRefRelation,
    PayloadRefs, PrimaryRelation,
};
use super::storage::{
    CatalogDescriptor, descriptor_object, lookup_exact, lookup_lower_bound, prepare_lazy_insert,
    prepare_lazy_remove, prepare_lazy_replace, write_lazy_relation_update, write_relation_state,
};
use super::work::CatalogWork;
use backend_store::{
    ClosureManifest, FileStore, ManifestChange, TypedObject, UntrustedObjectId, WorkspaceClosure,
};

pub(crate) fn append_to_closure(
    store: &FileStore,
    base: &WorkspaceClosure,
    state: &CatalogState,
    proof: &DerivedOutputProof,
) -> Result<(ClosureManifest, CatalogState), WorkspaceError> {
    append_to_closure_with_work(store, base, state, proof)
        .map(|(manifest, state, _)| (manifest, state))
}

pub(crate) fn append_to_closure_with_work(
    store: &FileStore,
    base: &WorkspaceClosure,
    state: &CatalogState,
    proof: &DerivedOutputProof,
) -> Result<(ClosureManifest, CatalogState, CatalogWork), WorkspaceError> {
    let mut work = CatalogWork::default();
    let (output_object, manifest_object) = proof.payload_objects();
    // Payloads are immutable CAS objects. They must also be reachable from
    // the selected workspace closure so the store's ordinary mark phase can
    // retain them without a catalog-wide row scan.
    store
        .write_object(&output_object)
        .map_err(WorkspaceError::store)?;
    store
        .write_object(&manifest_object)
        .map_err(WorkspaceError::store)?;
    let candidate = CatalogRecord::from_proof(proof, &output_object, &manifest_object);
    if state.persisted_parts().is_some() {
        return append_to_persisted_closure(
            store,
            base,
            state,
            &candidate,
            &output_object,
            &manifest_object,
        );
    }
    let InitialCatalog {
        primary,
        freshness,
        payload_refs,
        count,
    } = state.initial_insert(&candidate, &mut work)?;
    let primary_object = write_relation_state(store, &primary, &mut work)?;
    let freshness_object = write_relation_state(store, &freshness, &mut work)?;
    let payload_ref_object = write_relation_state(store, &payload_refs, &mut work)?;
    let descriptor = CatalogDescriptor {
        count,
        primary_root: primary.root().to_bytes(),
        primary_object,
        freshness_root: freshness.root().to_bytes(),
        freshness_object,
        payload_ref_root: payload_refs.root().to_bytes(),
        payload_ref_object,
    };
    let descriptor_bytes = descriptor.encode()?;
    let descriptor_value = descriptor_object(&descriptor_bytes);
    work.nodes_written = work
        .nodes_written
        .checked_add(1)
        .ok_or(WorkspaceError::Bounds)?;
    work.bytes_written = work
        .bytes_written
        .checked_add(descriptor_bytes.len())
        .ok_or(WorkspaceError::Bounds)?;

    // Source workspace objects remain part of the checked closure. Add only
    // payloads that are not already members, then replace the prior fixed
    // descriptor through the store's authenticated persistent manifest
    // relation. The membership probes use the closure tree's authenticated
    // path and do not materialize the complete source closure.
    let mut changes = Vec::with_capacity(4);
    for payload in [&output_object, &manifest_object] {
        if !base.manifest().contains_object_id(payload.id()) {
            changes.push(ManifestChange::insert(payload).map_err(WorkspaceError::store)?);
        }
    }
    if let Some(previous) = state.descriptor() {
        let previous_bytes = previous.encode()?;
        let previous_object = descriptor_object(&previous_bytes);
        changes.push(ManifestChange::delete(&previous_object).map_err(WorkspaceError::store)?);
    }
    changes.push(ManifestChange::insert(&descriptor_value)?);
    changes.sort_by_key(ManifestChange::key);
    let prepared_manifest = base
        .manifest()
        .prepare_delta(&changes)
        .map_err(WorkspaceError::store)?;
    let manifest_work = prepared_manifest.work();
    work.tree_nodes_read = work
        .tree_nodes_read
        .checked_add(manifest_work.visited_nodes)
        .ok_or(WorkspaceError::Bounds)?;
    work.tree_nodes_written = work
        .tree_nodes_written
        .checked_add(manifest_work.copied_nodes)
        .ok_or(WorkspaceError::Bounds)?;
    let closure = prepared_manifest.commit();
    let next = CatalogState::persisted(descriptor, state.coverage())?;
    Ok((closure, next, work))
}

/// Performs one catalog update from an already persisted descriptor.  The
/// version layer loads only the affected root-to-leaf paths and the store
/// admits only the changed relation-node frontier.  This keeps the first
/// update after restart logarithmic in the relation height rather than
/// rebuilding every historical row into `RelationState`.
fn append_to_persisted_closure(
    store: &FileStore,
    base: &WorkspaceClosure,
    state: &CatalogState,
    candidate: &CatalogRecord,
    output_object: &TypedObject,
    manifest_object: &TypedObject,
) -> Result<(ClosureManifest, CatalogState, CatalogWork), WorkspaceError> {
    let (old_descriptor, coverage) = state
        .persisted_parts()
        .ok_or(WorkspaceError::Corrupt("catalog persisted state"))?;
    let mut work = CatalogWork::default();
    let primary_key = candidate.primary_key();
    if let Some(existing) = lookup_exact::<PrimaryRelation>(
        store,
        old_descriptor.primary_object,
        old_descriptor.primary_root,
        &primary_key,
    )? {
        if existing != candidate.value() {
            return Err(WorkspaceError::Corrupt(
                "conflicting derived output identity",
            ));
        }
        return Ok((base.manifest().clone(), state.clone(), work));
    }

    let primary_update = prepare_lazy_insert::<PrimaryRelation>(
        store,
        old_descriptor.primary_root,
        &primary_key,
        candidate.value(),
    )?;
    work.add_lazy(primary_update.work())?;
    let (primary_root, primary_object) =
        write_lazy_relation_update(store, &primary_update, &mut work)?;

    let freshness_key = candidate.freshness_key();
    let freshness_update = prepare_lazy_insert::<FreshnessRelation>(
        store,
        old_descriptor.freshness_root,
        &freshness_key,
        super::relation::FreshnessValue(primary_key),
    )?;
    work.add_lazy(freshness_update.work())?;
    let (freshness_root, freshness_object) =
        write_lazy_relation_update(store, &freshness_update, &mut work)?;

    let mut roots = PersistedCatalogRoots {
        count: old_descriptor
            .count
            .checked_add(1)
            .ok_or(WorkspaceError::Bounds)?,
        primary_root,
        primary_object,
        freshness_root,
        freshness_object,
        payload_ref_root: old_descriptor.payload_ref_root,
        payload_ref_object: old_descriptor.payload_ref_object,
    };
    for payload_key in candidate.payload_keys() {
        increment_payload_ref(store, &mut roots, payload_key, &mut work)?;
    }
    let retired_payloads = evict_catalog_overflow(store, &freshness_key, &mut roots, &mut work)?;

    let descriptor = roots.descriptor();
    let commit = CatalogCommit {
        previous: old_descriptor,
        next: descriptor,
        output: output_object,
        manifest: manifest_object,
        retired_payloads: &retired_payloads,
    };
    let (closure, next) = commit_catalog_descriptor(store, base, &commit, &mut work)?;
    let next = CatalogState::persisted(next, coverage)?;
    Ok((closure, next, work))
}

struct PersistedCatalogRoots {
    count: usize,
    primary_root: [u8; 32],
    primary_object: [u8; 32],
    freshness_root: [u8; 32],
    freshness_object: [u8; 32],
    payload_ref_root: [u8; 32],
    payload_ref_object: [u8; 32],
}

impl PersistedCatalogRoots {
    fn descriptor(&self) -> CatalogDescriptor {
        CatalogDescriptor {
            count: self.count,
            primary_root: self.primary_root,
            primary_object: self.primary_object,
            freshness_root: self.freshness_root,
            freshness_object: self.freshness_object,
            payload_ref_root: self.payload_ref_root,
            payload_ref_object: self.payload_ref_object,
        }
    }
}

fn increment_payload_ref(
    store: &FileStore,
    roots: &mut PersistedCatalogRoots,
    key: PayloadKey,
    work: &mut CatalogWork,
) -> Result<(), WorkspaceError> {
    let before = lookup_exact::<PayloadRefRelation>(
        store,
        roots.payload_ref_object,
        roots.payload_ref_root,
        &key,
    )?;
    let count = before
        .map_or(Some(1), |refs| refs.0.checked_add(1))
        .filter(|count| usize::from(*count) <= MAX_CATALOG_ENTRIES * 2)
        .ok_or(WorkspaceError::Bounds)?;
    let update = if before.is_some() {
        prepare_lazy_replace::<PayloadRefRelation>(
            store,
            roots.payload_ref_root,
            &key,
            PayloadRefs(count),
        )?
    } else {
        prepare_lazy_insert::<PayloadRefRelation>(
            store,
            roots.payload_ref_root,
            &key,
            PayloadRefs(count),
        )?
    };
    work.add_lazy(update.work())?;
    (roots.payload_ref_root, roots.payload_ref_object) =
        write_lazy_relation_update(store, &update, work)?;
    Ok(())
}

fn decrement_payload_ref(
    store: &FileStore,
    roots: &mut PersistedCatalogRoots,
    key: PayloadKey,
    work: &mut CatalogWork,
) -> Result<bool, WorkspaceError> {
    let before = lookup_exact::<PayloadRefRelation>(
        store,
        roots.payload_ref_object,
        roots.payload_ref_root,
        &key,
    )?
    .ok_or(WorkspaceError::Corrupt("catalog payload reference"))?;
    let update = if before.0 == 1 {
        prepare_lazy_remove::<PayloadRefRelation>(store, roots.payload_ref_root, &key)?
    } else {
        let after = before
            .0
            .checked_sub(1)
            .map(PayloadRefs)
            .ok_or(WorkspaceError::Corrupt("catalog payload reference"))?;
        prepare_lazy_replace::<PayloadRefRelation>(store, roots.payload_ref_root, &key, after)?
    };
    work.add_lazy(update.work())?;
    (roots.payload_ref_root, roots.payload_ref_object) =
        write_lazy_relation_update(store, &update, work)?;
    Ok(before.0 == 1)
}

fn evict_catalog_overflow(
    store: &FileStore,
    newest_key: &FreshnessKey,
    roots: &mut PersistedCatalogRoots,
    work: &mut CatalogWork,
) -> Result<Vec<PayloadKey>, WorkspaceError> {
    evict_catalog_overflow_to(store, newest_key, roots, MAX_CATALOG_ENTRIES, work)
}

fn evict_catalog_overflow_to(
    store: &FileStore,
    newest_key: &FreshnessKey,
    roots: &mut PersistedCatalogRoots,
    max_entries: usize,
    work: &mut CatalogWork,
) -> Result<Vec<PayloadKey>, WorkspaceError> {
    let mut retired_payloads = Vec::new();
    while roots.count > max_entries {
        let Some((old_key, old_value)) = lookup_lower_bound::<FreshnessRelation>(
            store,
            roots.freshness_object,
            roots.freshness_root,
            &FreshnessKey([0; super::FRESHNESS_KEY_BYTES]),
        )?
        else {
            return Err(WorkspaceError::Corrupt("catalog eviction index"));
        };
        if old_key == *newest_key {
            return Err(WorkspaceError::Bounds);
        }
        let old_primary = old_value.0;
        let old_primary_value = lookup_exact::<PrimaryRelation>(
            store,
            roots.primary_object,
            roots.primary_root,
            &old_primary,
        )?
        .ok_or(WorkspaceError::Corrupt("catalog primary eviction"))?;
        let primary_remove =
            prepare_lazy_remove::<PrimaryRelation>(store, roots.primary_root, &old_primary)?;
        work.add_lazy(primary_remove.work())?;
        (roots.primary_root, roots.primary_object) =
            write_lazy_relation_update(store, &primary_remove, work)?;
        let freshness_remove =
            prepare_lazy_remove::<FreshnessRelation>(store, roots.freshness_root, &old_key)?;
        work.add_lazy(freshness_remove.work())?;
        (roots.freshness_root, roots.freshness_object) =
            write_lazy_relation_update(store, &freshness_remove, work)?;
        for payload_key in [
            PayloadKey(old_primary_value.output_object),
            PayloadKey(old_primary_value.manifest_object),
        ] {
            if decrement_payload_ref(store, roots, payload_key, work)? {
                retired_payloads.push(payload_key);
            }
        }
        roots.count = roots
            .count
            .checked_sub(1)
            .ok_or(WorkspaceError::Corrupt("catalog count"))?;
    }
    retired_payloads.sort_unstable();
    retired_payloads.dedup();
    Ok(retired_payloads)
}

struct CatalogCommit<'a> {
    previous: CatalogDescriptor,
    next: CatalogDescriptor,
    output: &'a TypedObject,
    manifest: &'a TypedObject,
    retired_payloads: &'a [PayloadKey],
}

fn commit_catalog_descriptor(
    store: &FileStore,
    base: &WorkspaceClosure,
    commit: &CatalogCommit<'_>,
    work: &mut CatalogWork,
) -> Result<(ClosureManifest, CatalogDescriptor), WorkspaceError> {
    let descriptor_bytes = commit.next.encode()?;
    let descriptor_value = descriptor_object(&descriptor_bytes);
    work.nodes_written = work
        .nodes_written
        .checked_add(1)
        .ok_or(WorkspaceError::Bounds)?;
    work.bytes_written = work
        .bytes_written
        .checked_add(descriptor_bytes.len())
        .ok_or(WorkspaceError::Bounds)?;

    let mut changes = Vec::with_capacity(4 + commit.retired_payloads.len());
    for payload in [commit.output, commit.manifest] {
        if !base.manifest().contains_object_id(payload.id()) {
            changes.push(ManifestChange::insert(payload).map_err(WorkspaceError::store)?);
        }
    }
    for payload in commit.retired_payloads {
        let object = store_payload_object(store, base, *payload)?;
        if let Some(object) = object {
            changes.push(ManifestChange::delete(&object).map_err(WorkspaceError::store)?);
        }
    }
    let previous_bytes = commit.previous.encode()?;
    let previous_object = descriptor_object(&previous_bytes);
    changes.push(ManifestChange::delete(&previous_object).map_err(WorkspaceError::store)?);
    changes.push(ManifestChange::insert(&descriptor_value)?);
    changes.sort_by_key(ManifestChange::key);
    let prepared_manifest = base
        .manifest()
        .prepare_delta(&changes)
        .map_err(WorkspaceError::store)?;
    let manifest_work = prepared_manifest.work();
    work.tree_nodes_read = work
        .tree_nodes_read
        .checked_add(manifest_work.visited_nodes)
        .ok_or(WorkspaceError::Bounds)?;
    work.tree_nodes_written = work
        .tree_nodes_written
        .checked_add(manifest_work.copied_nodes)
        .ok_or(WorkspaceError::Bounds)?;
    let closure = prepared_manifest.commit();
    Ok((closure, commit.next))
}

fn store_payload_object(
    store: &FileStore,
    base: &WorkspaceClosure,
    payload: PayloadKey,
) -> Result<Option<TypedObject>, WorkspaceError> {
    let object = store
        .read_object_claim(UntrustedObjectId::from_bytes(payload.0))
        .map_err(WorkspaceError::store)?;
    if !base.manifest().contains_object_id(object.id()) {
        return Err(WorkspaceError::Corrupt(
            "catalog payload outside workspace closure",
        ));
    }
    Ok(Some(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_version::{ClosedRelationScope, CoverageWitness, ScopeRoot};

    fn record() -> CatalogRecord {
        CatalogRecord {
            key: [1; 32],
            request: [2; 32],
            output: [3; 32],
            output_object: [4; 32],
            manifest_object: [5; 32],
            manifest_version: [6; 32],
            coverage_identity: [7; 32],
            scope: 8,
            read_manifest: [9; 32],
            authority: [10; 32],
            authority_epoch: 11,
            revocation_version: 12,
            dependency_generation: 13,
            authority_class: 14,
        }
    }

    #[test]
    fn one_eviction_retracts_indexes_and_payload_lifetimes_together() {
        let path = std::env::temp_dir().join(format!(
            "backend-catalog-eviction-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&path);
        let registry = backend_store::RelationAdmissionRegistry::default()
            .with_relation::<PrimaryRelation>()
            .expect("primary registry")
            .with_relation::<FreshnessRelation>()
            .expect("freshness registry")
            .with_relation::<PayloadRefRelation>()
            .expect("payload registry");
        let store = FileStore::open_with_registry(&path, 16 * 1024 * 1024, registry)
            .expect("catalog store");
        let coverage = CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(
            ScopeRoot::from_u64(1),
        ));
        let candidate = record();
        let state = CatalogState::empty(coverage);
        let mut work = CatalogWork::default();
        let InitialCatalog {
            primary,
            freshness,
            payload_refs,
            count,
        } = state
            .initial_insert(&candidate, &mut work)
            .expect("initial catalog");
        let mut roots = PersistedCatalogRoots {
            count,
            primary_root: primary.root().to_bytes(),
            primary_object: write_relation_state(&store, &primary, &mut work)
                .expect("primary relation"),
            freshness_root: freshness.root().to_bytes(),
            freshness_object: write_relation_state(&store, &freshness, &mut work)
                .expect("freshness relation"),
            payload_ref_root: payload_refs.root().to_bytes(),
            payload_ref_object: write_relation_state(&store, &payload_refs, &mut work)
                .expect("payload relation"),
        };

        let shared_payload = candidate.payload_keys()[0];
        increment_payload_ref(&store, &mut roots, shared_payload, &mut work)
            .expect("increment existing payload reference");
        assert_eq!(
            lookup_exact::<PayloadRefRelation>(
                &store,
                roots.payload_ref_object,
                roots.payload_ref_root,
                &shared_payload,
            )
            .expect("incremented payload lookup"),
            Some(PayloadRefs(2))
        );
        assert!(
            !decrement_payload_ref(&store, &mut roots, shared_payload, &mut work)
                .expect("decrement shared payload reference")
        );

        let retired = evict_catalog_overflow_to(
            &store,
            &FreshnessKey([u8::MAX; super::super::FRESHNESS_KEY_BYTES]),
            &mut roots,
            0,
            &mut work,
        )
        .expect("evict one catalog row");

        assert_eq!(roots.count, 0);
        assert_eq!(retired, candidate.payload_keys());
        assert!(
            lookup_exact::<PrimaryRelation>(
                &store,
                roots.primary_object,
                roots.primary_root,
                &candidate.primary_key()
            )
            .expect("primary lookup")
            .is_none()
        );
        for payload in candidate.payload_keys() {
            assert!(
                lookup_exact::<PayloadRefRelation>(
                    &store,
                    roots.payload_ref_object,
                    roots.payload_ref_root,
                    &payload
                )
                .expect("payload lookup")
                .is_none()
            );
        }

        drop(store);
        std::fs::remove_dir_all(path).expect("remove catalog fixture");
    }
}
