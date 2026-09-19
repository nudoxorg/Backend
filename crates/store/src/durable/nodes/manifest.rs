//! Lazy persistent closure-index publication and bounded cursors.

use super::super::{
    ClosureId, FileStore, Hash, ObjectId, StoreError, TypedObject, fs, io_error, map_read_error,
};
use super::{ManifestDescriptor, NODE_MAX_COUNT};
use crate::UntrustedObjectId;
use crate::closure::{ClosureManifest, ManifestRelation};
use backend_version::{CheckedCanonicalRoot, IdContext, SchemaIdentity, UntrustedId};
use std::collections::HashSet;
use std::sync::Arc;

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

impl DurableManifest {
    /// Returns the authenticated closure-index identity.
    #[must_use]
    pub const fn id(&self) -> ClosureId {
        self.descriptor.id
    }

    /// Returns the checked object count recorded in the root descriptor.
    ///
    /// Legacy V1 descriptors did not retain this count and therefore return
    /// `None`; newly written descriptors always return `Some`.
    #[must_use]
    pub const fn entry_count(&self) -> Option<usize> {
        self.descriptor.count
    }

    /// Looks up one immutable object by its exact manifest identity.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a branch, reference, or selected
    /// object fails authenticated admission.
    pub fn get(&self, id: ObjectId) -> Result<Option<TypedObject>, StoreError> {
        self.get_with_stats(id).map(|(object, _)| object)
    }

    /// Looks up one object and reports root-to-leaf work.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a branch, reference, or selected
    /// object fails authenticated admission.
    pub fn get_with_stats(
        &self,
        id: ObjectId,
    ) -> Result<(Option<TypedObject>, ManifestReadStats), StoreError> {
        let claim = UntrustedObjectId::from_bytes(*id.as_bytes());
        let (admitted, mut stats) = self.admit_claim_with_stats(claim)?;
        let Some(admitted) = admitted else {
            return Ok((None, stats));
        };
        let object = self.store.read_object(admitted)?;
        if object.id() != admitted {
            return Err(StoreError::Corrupt);
        }
        stats.objects_read = stats
            .objects_read
            .checked_add(1)
            .ok_or(StoreError::Bounds)?;
        stats.bytes_read = stats
            .bytes_read
            .checked_add(object.bytes().len())
            .ok_or(StoreError::Bounds)?;
        Ok((Some(object), stats))
    }

    /// Admits an untrusted object identity by proving exact membership in the
    /// selected closure manifest.
    ///
    /// This operation intentionally does not read an object payload. Relation
    /// nodes and other auxiliary closure members may be stored in the node CAS
    /// while still appearing as authenticated leaf keys in this manifest. A
    /// caller that needs canonical object bytes must use [`Self::get`] after
    /// admission.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when any root-to-leaf node, child
    /// summary, or relation reference fails authenticated admission.
    pub fn admit_claim(&self, claim: UntrustedObjectId) -> Result<Option<ObjectId>, StoreError> {
        self.admit_claim_with_stats(claim)
            .map(|(admitted, _)| admitted)
    }

    fn admit_claim_with_stats(
        &self,
        claim: UntrustedObjectId,
    ) -> Result<(Option<ObjectId>, ManifestReadStats), StoreError> {
        let mut stats = ManifestReadStats::default();
        let key = *claim.as_bytes();
        let mut node = self.root_node.as_ref().clone();
        let mut expected_first_key = None;
        let mut expected_level = None;
        let mut expected_row_count = None;
        loop {
            account_manifest_node(&node, &mut stats)?;
            if expected_level.is_some_and(|level| node.node().level() != level)
                || expected_first_key
                    .as_ref()
                    .is_some_and(|first| node.node().first_key() != Some(first))
                || expected_row_count.is_some_and(|count| node.node().row_count() != count)
            {
                return Err(StoreError::Corrupt);
            }
            if node.node().level() == 0 {
                let entries = node.leaf_entries().map_err(|_| StoreError::Corrupt)?;
                if entries
                    .binary_search_by_key(&key, |(candidate, ())| *candidate)
                    .is_err()
                {
                    return Ok((None, stats));
                }
                return Ok((Some(ObjectId::from_bytes(key)), stats));
            }
            let children = node.child_summaries().map_err(|_| StoreError::Corrupt)?;
            let Some(index) = children
                .partition_point(|child| child.first_key.as_slice() <= key.as_slice())
                .checked_sub(1)
            else {
                return Ok((None, stats));
            };
            let child = children.get(index).ok_or(StoreError::Corrupt)?;
            let child_level = node
                .node()
                .level()
                .checked_sub(1)
                .ok_or(StoreError::Corrupt)?;
            if child.level != child_level {
                return Err(StoreError::Corrupt);
            }
            let child_version = child.commitment.to_bytes();
            let child_id = self.store.read_relation_ref(
                SchemaIdentity::of_relation::<ManifestRelation>(),
                &child_version,
            )?;
            node = self.store.read_manifest_node_checked(
                child_id,
                Some(&child.first_key),
                Some(child_level),
                Some(child.row_count),
                &mut stats,
            )?;
            expected_first_key = Some(child.first_key);
            expected_level = Some(child_level);
            expected_row_count = Some(child.row_count);
        }
    }

    /// Reads at most `limit` objects after `after` in canonical identity order.
    /// Pass the returned [`DurableManifestPage::next`] token to continue.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a branch, reference, or selected
    /// object fails authenticated admission, and [`StoreError::Bounds`] when
    /// the requested page exceeds the bounded node policy.
    pub fn page(
        &self,
        after: Option<ObjectId>,
        limit: usize,
    ) -> Result<DurableManifestPage, StoreError> {
        self.page_inner(after, limit, None)
    }

    /// Reads a bounded page while charging selected object payload bytes.
    ///
    /// The filesystem envelope is checked before each leaf object is loaded,
    /// so a caller can cap memory and I/O for a page independently of the
    /// manifest's total size.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Bounds`] when the next object would exceed
    /// `max_bytes`, or [`StoreError::Corrupt`] for an invalid branch,
    /// reference, or selected object.
    pub fn page_with_budget(
        &self,
        after: Option<ObjectId>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<DurableManifestPage, StoreError> {
        self.page_inner(after, limit, Some(max_bytes))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the bounded cursor is one state machine"
    )]
    fn page_inner(
        &self,
        after: Option<ObjectId>,
        limit: usize,
        mut remaining_bytes: Option<usize>,
    ) -> Result<DurableManifestPage, StoreError> {
        if limit > NODE_MAX_COUNT {
            return Err(StoreError::Bounds);
        }
        if limit == 0 {
            return Ok(DurableManifestPage {
                objects: Vec::new(),
                next: None,
                stats: ManifestReadStats::default(),
                node_ids: Vec::new(),
            });
        }
        let schema = SchemaIdentity::of_relation::<ManifestRelation>();
        let mut stack = vec![ManifestPageTask {
            object_id: Some(self.descriptor.root),
            version: None,
            expected_first_key: None,
            expected_level: None,
            expected_row_count: None,
            cached: Some(self.root_node.as_ref().clone()),
        }];
        let mut remaining_after = after.map(|id| *id.as_bytes());
        let mut objects = Vec::new();
        let mut stats = ManifestReadStats::default();
        let mut node_ids = vec![self.descriptor.root];
        let mut has_more = false;
        'walk: while let Some(task) = stack.pop() {
            let node = if let Some(node) = task.cached {
                account_manifest_node(&node, &mut stats)?;
                node
            } else {
                let object_id = if let Some(object_id) = task.object_id {
                    object_id
                } else if let Some(version) = task.version {
                    self.store.read_relation_ref(schema, &version)?
                } else {
                    return Err(StoreError::Corrupt);
                };
                node_ids.push(object_id);
                self.store.read_manifest_node_checked(
                    object_id,
                    task.expected_first_key.as_ref(),
                    task.expected_level,
                    task.expected_row_count,
                    &mut stats,
                )?
            };
            if let Some(remaining) = &mut remaining_bytes {
                let node_bytes = node.bytes().len();
                if node_bytes > *remaining {
                    if objects.is_empty() {
                        return Err(StoreError::Bounds);
                    }
                    has_more = true;
                    break 'walk;
                }
                *remaining -= node_bytes;
            }
            if node.node().level() == 0 {
                let entries = node.leaf_entries().map_err(|_| StoreError::Corrupt)?;
                for (index, (key, ())) in entries.iter().enumerate() {
                    if remaining_after.is_some_and(|after| *key <= after) {
                        continue;
                    }
                    let object_id = ObjectId::from_bytes(*key);
                    let encoded_len = self.store.object_encoded_len(object_id)?;
                    if remaining_bytes.is_some_and(|remaining| encoded_len > remaining) {
                        if objects.is_empty() {
                            return Err(StoreError::Bounds);
                        }
                        has_more = true;
                        break 'walk;
                    }
                    let object = self.store.read_object(object_id)?;
                    if object.id() != object_id {
                        return Err(StoreError::Corrupt);
                    }
                    stats.objects_read = stats
                        .objects_read
                        .checked_add(1)
                        .ok_or(StoreError::Bounds)?;
                    stats.bytes_read = stats
                        .bytes_read
                        .checked_add(object.bytes().len())
                        .ok_or(StoreError::Bounds)?;
                    if let Some(remaining) = &mut remaining_bytes {
                        *remaining = remaining
                            .checked_sub(encoded_len)
                            .ok_or(StoreError::Bounds)?;
                    }
                    objects.push(object);
                    remaining_after = None;
                    if objects.len() == limit {
                        has_more = index + 1 < entries.len() || !stack.is_empty();
                        break 'walk;
                    }
                }
                remaining_after = None;
                continue;
            }
            let children = node.child_summaries().map_err(|_| StoreError::Corrupt)?;
            if children.is_empty() {
                return Err(StoreError::Corrupt);
            }
            let start = remaining_after.map_or(0, |after| {
                children
                    .partition_point(|child| child.first_key.as_slice() <= after.as_slice())
                    .saturating_sub(1)
            });
            let child_level = node
                .node()
                .level()
                .checked_sub(1)
                .ok_or(StoreError::Corrupt)?;
            for child in children.iter().skip(start).rev() {
                if child.level != child_level {
                    return Err(StoreError::Corrupt);
                }
                stack.push(ManifestPageTask {
                    object_id: None,
                    version: Some(child.commitment.to_bytes()),
                    expected_first_key: Some(child.first_key),
                    expected_level: Some(child_level),
                    expected_row_count: Some(child.row_count),
                    cached: None,
                });
            }
        }
        let next = has_more
            .then(|| objects.last().map(TypedObject::id))
            .flatten();
        Ok(DurableManifestPage {
            objects,
            next,
            stats,
            node_ids,
        })
    }
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
