//! Lazy persistent closure-index publication and bounded cursors.

use super::super::{
    ClosureId, FileStore, Hash, ObjectId, StoreError, TypedObject, fs, io_error, map_read_error,
};
use super::{ManifestDescriptor, NODE_MAX_COUNT, RelationAdmission};
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

/// One bounded page of authenticated closure member identities without
/// loading member payloads.
#[derive(Clone, Debug)]
pub struct DurableManifestIdPage {
    object_ids: Vec<ObjectId>,
    next: Option<ObjectId>,
    stats: ManifestReadStats,
}

impl DurableManifestIdPage {
    /// Returns member IDs in canonical ascending identity order.
    #[must_use]
    pub fn object_ids(&self) -> &[ObjectId] {
        &self.object_ids
    }

    /// Returns the token to pass as `after` for the next ID page.
    #[must_use]
    pub const fn next(&self) -> Option<ObjectId> {
        self.next
    }

    /// Returns authenticated manifest-node work for this page.
    #[must_use]
    pub const fn stats(&self) -> ManifestReadStats {
        self.stats
    }
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

#[cfg(unix)]
fn bounded_manifest_ref(dirs: &super::super::artifact_fs::BoundedReadDirectories, schema: SchemaIdentity, version: &Hash) -> Result<ObjectId, StoreError> {
    let mut file = dirs.relation_ref(schema, version)?;
    let len = super::wire::relation_ref_encoded_len();
    if usize::try_from(file.metadata().map_err(|e| io_error(&e))?.len()).map_err(|_| StoreError::Bounds)? != len { return Err(StoreError::Corrupt); }
    let bytes = super::relation::read_relation_reference(&mut file)?;
    super::wire::decode_relation_ref(&bytes[..len], schema, version)
}

impl FileStore {
    /// Tests membership in a selected closure without materializing members.
    ///
    /// The Unix implementation debits fixed locator/envelope stack before
    /// opening and node wire/key allocations before typed decode. Other
    /// platforms fail closed. `before_read` must charge work and check cancellation
    /// before each descriptor or relation-node read. `preflight` must charge
    /// typed node admission and retained node copies before each decode. The
    /// supplied credit remains debited; temporary reservations may be refunded
    /// by the caller only after this operation returns. This admits one exact
    /// selected path, not every member or an exported closure.
    pub fn contains_closure_member_bounded<B, P>(
        &self, pin: &super::super::GcPinGuard, closure: ClosureId, member: ObjectId,
        max_depth: usize, budget: &mut usize, mut before_read: B, preflight: P,
    ) -> Result<(bool, u64), StoreError>
    where
        B: FnMut(&mut usize) -> Result<(), StoreError>,
        P: for<'a> FnMut(&backend_version::CanonicalNodeView<'a>, &mut usize) -> Result<(), StoreError>,
    {
        use backend_version::{LazyTree, LazyTreeError, TreeNodeLoader};
        use std::cell::RefCell;
        use std::io::Read;
        if max_depth == 0 || max_depth > 32 { return Err(StoreError::Bounds); }
        // Fixed root path, locators, descriptor/reference buffers and the
        // relation envelope are reserved before opening the selected closure.
        const ENVELOPE: usize = backend_version::DEFAULT_CUT_POLICY.max_encoded_bytes() + super::super::OBJECT_MAGIC.len() + 32 + 1 + 2 + 1 + 32 + 32 + 8;
        const STACK: usize = 4096 + 96 + 80 + 128 + 128 + ENVELOPE;
        if cfg!(not(unix)) { return Err(StoreError::Bounds); }
        *budget = budget.checked_sub(STACK).ok_or(StoreError::Bounds)?;
        if !pin.covers_identity(self.gc_identity) { return Err(StoreError::Corrupt); }
        before_read(budget)?;
        #[cfg(unix)]
        let dirs = super::super::artifact_fs::BoundedReadDirectories::open(self)?;
        #[cfg(unix)]
        let mut file = dirs.closure(closure)?;
        #[cfg(not(unix))]
        let mut file = super::super::artifact_fs::open_closure(self, closure)?;
        let length = usize::try_from(file.metadata().map_err(|e| io_error(&e))?.len()).map_err(|_| StoreError::Bounds)?;
        // Both supported fixed-size descriptor formats fit this stack buffer.
        let mut bytes = [0u8; 128];
        if length > bytes.len() { return Err(StoreError::Bounds); }
        file.read_exact(&mut bytes[..length]).map_err(|e| io_error(&e))?;
        let mut trailing = [0u8; 1];
        if file.read(&mut trailing).map_err(|e| io_error(&e))? != 0 { return Err(StoreError::Corrupt); }
        let descriptor = super::wire::decode_manifest_descriptor(&bytes[..length], closure)?;
        if descriptor.count.is_some_and(|n| n > NODE_MAX_COUNT) { return Err(StoreError::Bounds); }
        before_read(budget)?;
        let schema = SchemaIdentity::of_relation::<ManifestRelation>();
        #[cfg(unix)]
        let root_object = bounded_manifest_ref(&dirs, schema, closure.as_bytes())?;
        #[cfg(not(unix))]
        let root_object = self.read_relation_ref(schema, closure.as_bytes())?;
        if root_object != descriptor.root { return Err(StoreError::Corrupt); }
        struct Loader<'a, B, P> { store: &'a FileStore, #[cfg(unix)] dirs: &'a super::super::artifact_fs::BoundedReadDirectories, state: RefCell<(&'a mut usize, B, P)> }
        impl<B, P> TreeNodeLoader<ManifestRelation> for Loader<'_, B, P>
        where B: FnMut(&mut usize) -> Result<(), StoreError>,
              P: for<'a> FnMut(&backend_version::CanonicalNodeView<'a>, &mut usize) -> Result<(), StoreError> {
            type Error = StoreError;
            fn load(&self, claim: UntrustedId<ManifestRelation>) -> Result<CheckedCanonicalRoot<ManifestRelation>, StoreError> {
                let mut state = self.state.borrow_mut();
                let (budget, before, gate) = &mut *state;
                before(budget)?;
                #[cfg(not(unix))]
                { self.store.read_relation_node_claim_preflight(claim, |view| gate(view, budget)) }
                #[cfg(unix)]
                {
                    let schema = SchemaIdentity::of_relation::<ManifestRelation>();
                    let object = bounded_manifest_ref(self.dirs, schema, claim.as_bytes())?;
                    let file = self.dirs.object(object)?;
                    self.store.with_relation_object_preflight_file(file, crate::UntrustedObjectId::from_bytes(*object.as_bytes()),
                        |object| {
                            if object.schema() != schema || object.version() != claim.as_bytes() || object.key() != &crate::closure::relation_key(schema) { return Err(StoreError::Corrupt); }
                            backend_version::StateRoot::<ManifestRelation>::admit_canonical_bytes(claim, object.bytes()).map_err(|_| StoreError::Corrupt)?;
                            let view = backend_version::CanonicalNodeView::parse(object.bytes(), schema).map_err(|_| StoreError::Corrupt)?;
                            // Each admitted node has an exact-length wire copy;
                            // reserve two copies plus the single 32-byte key encoder.
                            let node_charge = view.encoded_len().checked_mul(2).and_then(|n| n.checked_add(32)).ok_or(StoreError::Bounds)?;
                            **budget = budget.checked_sub(node_charge).ok_or(StoreError::Bounds)?;
                            gate(&view, budget)
                        },
                        |object| backend_version::admit_canonical_root_claim(claim, object.bytes()).map_err(|_| StoreError::Corrupt))
                }
            }
        }
        let loader = Loader { store: self, #[cfg(unix)] dirs: &dirs, state: RefCell::new((budget, before_read, preflight)) };
        let claim = UntrustedId::<ManifestRelation>::from_wire(closure.as_bytes(), IdContext::relation::<ManifestRelation>()).map_err(|_| StoreError::Corrupt)?;
        let map_error = |error| match error { LazyTreeError::Load(error) => error, _ => StoreError::Corrupt };
        let tree = LazyTree::open(&loader, claim).map_err(map_error)?;
        if descriptor.count.is_some_and(|count| u64::try_from(count).ok() != Some(tree.root().node().row_count())) { return Err(StoreError::Corrupt); }
        let count = tree.root().node().row_count();
        tree.lookup_streaming_with_depth_limit(member.as_bytes(), max_depth).map(|v| (v.is_some(), count)).map_err(map_error)
    }

    /// Writes only the changed frontier of a persistent closure index when a
    /// manifest was produced by an exact-base path-copy update. The flat
    /// object export is not part of this physical publication path.
    pub(in crate::durable) fn write_manifest_publication(
        &self,
        manifest: &ClosureManifest,
        admission: &mut RelationAdmission<'_>,
    ) -> Result<ObjectId, StoreError> {
        fs::create_dir_all(self.root.join("nodes")).map_err(|error| io_error(&error))?;
        let schema = SchemaIdentity::of_relation::<ManifestRelation>();
        let root = manifest.root_handle();
        let mut stats = super::TreeWriteStats::default();
        let mut seen = HashSet::new();
        let incremental = if let Some(source) = manifest.tree_source_root() {
            admission.existing(schema, &source)?.is_some()
        } else { false };
        if incremental {
            let frontier = manifest.tree_frontier().ok_or(StoreError::Corrupt)?;
            for node in frontier {
                self.write_relation_node_handle(node, schema, &mut stats, &mut seen, false, admission)?;
            }
            self.write_relation_node_handle(&root, schema, &mut stats, &mut seen, false, admission)
        } else {
            self.write_relation_node_handle(&root, schema, &mut stats, &mut seen, true, admission)
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

impl DurableManifest {
    /// Returns the checked root evidence for storage-layer path-copy updates.
    #[must_use]
    pub(in crate::durable) fn root_evidence(&self) -> CheckedCanonicalRoot<ManifestRelation> {
        self.root_node.as_ref().clone()
    }
}
