//! Lazy persistent closure-index publication and bounded cursors.

use super::super::{
    ClosureId, FileStore, Hash, ObjectId, StoreError, TypedObject, fs, io_error, map_read_error,
};
use super::{ManifestDescriptor, NODE_MAX_COUNT};
use crate::closure::{ClosureManifest, ManifestRelation};
use backend_version::{CheckedCanonicalRoot, IdContext, SchemaIdentity, UntrustedId};
use std::collections::HashSet;
use std::sync::Arc;

mod page;

/// Work observed while reading a lazy closure-index page or point lookup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ManifestReadStats {
    /// Number of authenticated manifest nodes read from the object CAS.
    pub nodes_read: usize,
    /// Number of leaf objects read from the object CAS.
    pub objects_read: usize,
    /// Total canonical object bytes read for the operation.
    pub bytes_read: usize,
}

/// One bounded page from a durable closure manifest.
#[derive(Clone, Debug)]
pub struct DurableManifestPage {
    objects: Vec<TypedObject>,
    next: Option<ObjectId>,
    stats: ManifestReadStats,
    node_ids: Vec<ObjectId>,
}

impl DurableManifestPage {
    /// Returns the objects in canonical object-identity order.
    #[must_use]
    pub fn objects(&self) -> &[TypedObject] {
        &self.objects
    }

    /// Returns the token to pass as `after` for the next page, if one exists.
    #[must_use]
    pub const fn next(&self) -> Option<ObjectId> {
        self.next
    }

    /// Returns bounded node/object work for this page.
    #[must_use]
    pub const fn stats(&self) -> ManifestReadStats {
        self.stats
    }

    /// Returns the authenticated manifest node objects touched while filling
    /// this page. The IDs are suitable for a bounded reachability cursor.
    #[must_use]
    pub(crate) fn node_ids(&self) -> &[ObjectId] {
        &self.node_ids
    }
}

/// An authenticated, lazy view of a persistent closure-index root.
///
/// Opening this view reads and admits only the compact descriptor and its root
/// node. Point lookups read one branch path, while [`Self::page`] reads only
/// the leaves needed to fill the requested bounded page. The flat
/// [`ClosureManifest`](crate::ClosureManifest) remains available through
/// [`FileStore::read_closure`](super::super::FileStore::read_closure) for
/// compatibility callers that explicitly need a complete export.
#[derive(Clone, Debug)]
pub struct DurableManifest {
    store: FileStore,
    descriptor: ManifestDescriptor,
    root_node: Arc<CheckedCanonicalRoot<ManifestRelation>>,
}

struct ManifestPageTask {
    object_id: Option<ObjectId>,
    version: Option<Hash>,
    expected_first_key: Option<Hash>,
    expected_level: Option<u16>,
    expected_row_count: Option<u64>,
    cached: Option<CheckedCanonicalRoot<ManifestRelation>>,
}

fn account_manifest_node(
    node: &CheckedCanonicalRoot<ManifestRelation>,
    stats: &mut ManifestReadStats,
) -> Result<(), StoreError> {
    stats.nodes_read = stats.nodes_read.checked_add(1).ok_or(StoreError::Bounds)?;
    stats.bytes_read = stats
        .bytes_read
        .checked_add(node.bytes().len())
        .ok_or(StoreError::Bounds)?;
    Ok(())
}

impl FileStore {
    /// Writes only the changed frontier of a persistent closure index when a
    /// manifest was produced by an exact-base path-copy update. The flat
    /// object export is not part of this physical publication path.
    pub(in crate::durable) fn write_manifest_publication(
        &self,
        manifest: &ClosureManifest,
    ) -> Result<ObjectId, StoreError> {
        fs::create_dir_all(self.root.join("nodes")).map_err(|error| io_error(&error))?;
        let schema = SchemaIdentity::of_relation::<ManifestRelation>();
        let root = manifest.root_handle();
        let mut stats = super::TreeWriteStats::default();
        let mut seen = HashSet::new();
        let incremental = manifest
            .tree_source_root()
            .is_some_and(|source| self.relation_ref_path(schema, &source).is_file());
        if incremental {
            let frontier = manifest.tree_frontier().ok_or(StoreError::Corrupt)?;
            for node in frontier {
                self.write_relation_node_handle(node, schema, &mut stats, &mut seen, false)?;
            }
            self.write_relation_node_handle(&root, schema, &mut stats, &mut seen, false)
        } else {
            self.write_relation_node_handle(&root, schema, &mut stats, &mut seen, true)
        }
    }

    /// Reopens a persistent closure index and materializes its leaf object
    /// references only after the compact descriptor has been authenticated.
    pub(in crate::durable) fn read_manifest_closure(
        &self,
        id: ClosureId,
        root_object: ObjectId,
        verify_edges: bool,
    ) -> Result<ClosureManifest, StoreError> {
        let mut visited = HashSet::new();
        let mut objects = Vec::new();
        self.read_manifest_node(root_object, &mut visited, &mut objects)?;
        objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
        let manifest = ClosureManifest::from_index(objects)?;
        let admission = if verify_edges {
            manifest.admit_with_registry(&self.relation_registry)
        } else {
            manifest.admit_objects_with_registry(&self.relation_registry)
        };
        admission?;
        if manifest.id() != id {
            return Err(StoreError::Corrupt);
        }
        Ok(manifest)
    }

    fn read_manifest_node(
        &self,
        object_id: ObjectId,
        visited: &mut HashSet<Hash>,
        objects: &mut Vec<TypedObject>,
    ) -> Result<(), StoreError> {
        let object = self.read_object(object_id)?;
        let schema = SchemaIdentity::of_relation::<ManifestRelation>();
        if object.schema() != schema {
            return Err(StoreError::Corrupt);
        }
        let version = object.version();
        if !visited.insert(*version) {
            return Err(StoreError::Corrupt);
        }
        if visited.len() > NODE_MAX_COUNT {
            return Err(StoreError::Bounds);
        }
        let claim = UntrustedId::<ManifestRelation>::from_wire(
            version,
            IdContext::relation::<ManifestRelation>(),
        )
        .map_err(|_| StoreError::Corrupt)?;
        let (admitted_object, _, admitted) = self.read_relation_node_claim_parts(claim)?;
        if admitted_object != object_id {
            return Err(StoreError::Corrupt);
        }
        if admitted.node().level() == 0 {
            for (key, _value) in admitted.leaf_entries().map_err(|_| StoreError::Corrupt)? {
                let key = ObjectId::from_bytes(key);
                objects.push(self.read_object(key)?);
            }
        } else {
            for child in admitted
                .child_summaries()
                .map_err(|_| StoreError::Corrupt)?
            {
                let child = self.read_relation_ref(schema, &child.commitment.to_bytes())?;
                self.read_manifest_node(child, visited, objects)?;
            }
        }
        Ok(())
    }

    pub(super) fn read_manifest_node_checked(
        &self,
        object_id: ObjectId,
        expected_first_key: Option<&Hash>,
        expected_level: Option<u16>,
        expected_row_count: Option<u64>,
        stats: &mut ManifestReadStats,
    ) -> Result<CheckedCanonicalRoot<ManifestRelation>, StoreError> {
        let claim = UntrustedId::<ManifestRelation>::from_wire(
            self.read_object(object_id)?.version().as_ref(),
            IdContext::relation::<ManifestRelation>(),
        )
        .map_err(|_| StoreError::Corrupt)?;
        let (admitted_object, _, admitted) = self.read_relation_node_claim_parts(claim)?;
        if admitted_object != object_id {
            return Err(StoreError::Corrupt);
        }
        if expected_level.is_some_and(|level| admitted.node().level() != level)
            || expected_first_key.is_some_and(|first| admitted.node().first_key() != Some(first))
            || expected_row_count.is_some_and(|count| admitted.node().row_count() != count)
        {
            return Err(StoreError::Corrupt);
        }
        account_manifest_node(&admitted, stats)?;
        Ok(admitted)
    }

    fn object_encoded_len(&self, id: ObjectId) -> Result<usize, StoreError> {
        let path = self
            .root
            .join("objects")
            .join(format!("{}.object", super::wire::hex(id.as_bytes())));
        let length = fs::metadata(path)
            .map_err(|error| map_read_error(&error))?
            .len();
        usize::try_from(length).map_err(|_| StoreError::Bounds)
    }

    pub(in crate::durable) fn open_manifest_index(
        &self,
        id: ClosureId,
        descriptor: ManifestDescriptor,
    ) -> Result<DurableManifest, StoreError> {
        if descriptor.id != id {
            return Err(StoreError::Corrupt);
        }
        if descriptor.count.is_some_and(|count| count > NODE_MAX_COUNT) {
            return Err(StoreError::Bounds);
        }
        let mut stats = ManifestReadStats::default();
        let root_node =
            self.read_manifest_node_checked(descriptor.root, None, None, None, &mut stats)?;
        if root_node.root().as_bytes() != id.as_bytes() {
            return Err(StoreError::Corrupt);
        }
        if descriptor
            .count
            .is_some_and(|count| u64::try_from(count).ok() != Some(root_node.node().row_count()))
        {
            return Err(StoreError::Corrupt);
        }
        Ok(DurableManifest {
            store: self.clone(),
            descriptor,
            root_node: Arc::new(root_node),
        })
    }
}
