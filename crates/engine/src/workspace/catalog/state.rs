//! Lazy catalog descriptor state and owner GC projection.

use super::super::owner::WorkspaceError;
use super::relation::{CatalogState, FreshnessRelation, PayloadRefRelation, PrimaryRelation};
use super::storage::{admit_object_id, load_descriptor_by_id, read_relation_root};
use backend_store::{DurableManifest, FileStore, GcRoot, GcRoots, ObjectId};
use backend_version::CoverageWitness;

/// Loads a catalog through the descriptor object ID retained by the selected
/// workspace pack. This is the restart path for compact store manifests and
/// does not enumerate unrelated closure objects.
pub(crate) fn state_from_durable_manifest(
    store: &FileStore,
    manifest: &DurableManifest,
    descriptor_id: ObjectId,
    coverage: CoverageWitness,
) -> Result<CatalogState, WorkspaceError> {
    let descriptor = load_descriptor_by_id(store, manifest, descriptor_id, coverage)?;
    CatalogState::persisted(descriptor, coverage)
}

/// Returns the three authenticated catalog relation roots as explicit GC roots.
///
/// Catalog payload objects are part of the selected workspace closure when a
/// derived entry is published. The store's closure mark phase retains them
/// through that ordinary closure index, so this projection never materializes
/// or scans catalog rows.
pub(crate) fn catalog_gc_roots(
    store: &FileStore,
    state: &CatalogState,
) -> Result<GcRoots, WorkspaceError> {
    let Some(descriptor) = state.descriptor() else {
        return Ok(GcRoots::new());
    };
    read_relation_root::<PrimaryRelation>(
        store,
        descriptor.primary_object,
        descriptor.primary_root,
    )?;
    read_relation_root::<FreshnessRelation>(
        store,
        descriptor.freshness_object,
        descriptor.freshness_root,
    )?;
    read_relation_root::<PayloadRefRelation>(
        store,
        descriptor.payload_ref_object,
        descriptor.payload_ref_root,
    )?;
    let mut roots = GcRoots::new();
    roots.add(GcRoot::Object(admit_object_id(
        store,
        descriptor.primary_object,
    )?));
    roots.add(GcRoot::Object(admit_object_id(
        store,
        descriptor.payload_ref_object,
    )?));
    roots.add(GcRoot::Object(admit_object_id(
        store,
        descriptor.freshness_object,
    )?));
    Ok(roots)
}
