//! `FileStore` opening and immutable pack/object/closure access.

use super::nodes;
use super::recovery;
use super::{
    Arc, ClosureId, ClosureManifest, DurableManifest, DurableTree, FileStore, Mutex, ObjectEdge,
    ObjectId, ObjectWriteReceipt, Pack, PackId, Path, RawRelation, RelationAdmissionRegistry,
    SelectedHead, StoreError, TreeWriteStats, TypedObject, decode_object, decode_pack_file,
    encode_object, encode_pack_file, envelope_limit, fs, hex, io_error, map_read_error,
    relation_object_limit, sync_directory, write_immutable, write_immutable_file_with_status,
    write_immutable_with_status,
};
use crate::{UntrustedObjectId, WorkspaceClosure};

impl FileStore {
    /// Opens or creates a bounded filesystem store at `root`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the store directories cannot be created
    /// and [`StoreError::Corrupt`] when an existing complete journal is
    /// invalid.
    pub fn open(root: impl AsRef<Path>, max_pack_bytes: usize) -> Result<Self, StoreError> {
        Self::open_with_registry(root, max_pack_bytes, RelationAdmissionRegistry::default())
    }

    /// Opens a bounded filesystem store with explicit relation decoders.
    ///
    /// The default [`Self::open`] admits the built-in [`RawRelation`]. A
    /// product that persists another canonical relation must register it here
    /// so reopen can validate every relation root's grammar.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the store directories cannot be created
    /// and [`StoreError::Corrupt`] when an existing complete journal or typed
    /// object is invalid.
    pub fn open_with_registry(
        root: impl AsRef<Path>,
        max_pack_bytes: usize,
        relation_registry: RelationAdmissionRegistry,
    ) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("packs")).map_err(|error| io_error(&error))?;
        fs::create_dir_all(root.join("objects")).map_err(|error| io_error(&error))?;
        fs::create_dir_all(root.join("closures")).map_err(|error| io_error(&error))?;
        fs::create_dir_all(root.join("nodes")).map_err(|error| io_error(&error))?;
        let store = Self {
            root,
            max_pack_bytes,
            lock: Arc::new(Mutex::new(())),
            journal_tail: Arc::new(Mutex::new(recovery::JournalTail::default())),
            relation_registry: Arc::new(relation_registry),
            tree_write_stats: Arc::new(Mutex::new(TreeWriteStats::default())),
        };
        let _process_lock = store.acquire_process_lock()?;
        store.read_state()?;
        store.recover_gc_on_open()?;
        Ok(store)
    }

    /// Returns the directory containing this store.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the maximum admitted logical pack payload size.
    #[must_use]
    pub const fn max_pack_bytes(&self) -> usize {
        self.max_pack_bytes
    }

    /// Returns the relation decoder registry used for object admission.
    #[must_use]
    pub fn relation_registry(&self) -> &RelationAdmissionRegistry {
        &self.relation_registry
    }

    /// Returns the node and canonical-byte counts from the last tree
    /// publication performed by this store handle.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn last_tree_write_stats(&self) -> TreeWriteStats {
        self.tree_write_stats
            .lock()
            .map(|stats| *stats)
            .unwrap_or_default()
    }

    /// Durably writes and validates one immutable pack.
    ///
    /// # Errors
    ///
    /// Returns an admission, bounds, corruption, or filesystem error.
    pub fn write_pack(&self, pack: &Pack) -> Result<PackId, StoreError> {
        let admitted = crate::admit_pack(pack.to_wire(), self.max_pack_bytes)?;
        let encoded = encode_pack_file(&admitted, self.max_pack_bytes)?;
        let path = self.pack_path(admitted.id());
        write_immutable(&path, &encoded, &self.root.join("packs"))?;
        Ok(admitted.id())
    }

    /// Reads, admits, and verifies an immutable pack by its physical ID.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] for a missing or malformed object and
    /// [`StoreError::Io`] for other filesystem failures.
    pub fn read_pack(&self, id: PackId) -> Result<Pack, StoreError> {
        let path = self.pack_path(id);
        let metadata = fs::metadata(&path).map_err(|error| map_read_error(&error))?;
        let limit = envelope_limit(self.max_pack_bytes)?;
        if metadata.len() > u64::try_from(limit).map_err(|_| StoreError::Bounds)? {
            return Err(StoreError::Bounds);
        }
        let bytes = fs::read(path).map_err(|error| map_read_error(&error))?;
        let wire = decode_pack_file(&bytes, self.max_pack_bytes)?;
        let pack = crate::admit_pack(wire, self.max_pack_bytes)?;
        if pack.id() != id {
            return Err(StoreError::Corrupt);
        }
        Ok(pack)
    }

    /// Durably writes one immutable typed object without rewriting an
    /// existing identical object.  Closure publication calls this for every
    /// member, which permits structural sharing between manifests.
    ///
    /// # Errors
    ///
    /// Returns an admission, bounds, corruption, or filesystem error.
    pub fn write_object(&self, object: &TypedObject) -> Result<ObjectId, StoreError> {
        Ok(self.write_object_with_receipt(object)?.id())
    }

    /// Durably admits one typed object and reports physical CAS sharing.
    ///
    /// The returned byte count includes the immutable object envelope, while
    /// [`ObjectWriteReceipt::created`] is false when an identical object was
    /// already present.  Object contents are checked against the configured
    /// relation registry before any filesystem mutation.
    ///
    /// # Errors
    ///
    /// Returns an admission, bounds, corruption, or filesystem error.
    pub fn write_object_with_receipt(
        &self,
        object: &TypedObject,
    ) -> Result<ObjectWriteReceipt, StoreError> {
        self.write_object_with_limit(object, envelope_limit(self.max_pack_bytes)?)
    }

    pub(super) fn write_relation_object_with_receipt(
        &self,
        object: &TypedObject,
    ) -> Result<ObjectWriteReceipt, StoreError> {
        self.write_object_with_limit(object, relation_object_limit()?)
    }

    fn write_object_with_limit(
        &self,
        object: &TypedObject,
        limit: usize,
    ) -> Result<ObjectWriteReceipt, StoreError> {
        self.write_object_with_limit_and_sync(object, limit, true)
    }

    fn write_object_without_directory_sync(
        &self,
        object: &TypedObject,
    ) -> Result<ObjectWriteReceipt, StoreError> {
        self.write_object_with_limit_and_sync(object, envelope_limit(self.max_pack_bytes)?, false)
    }

    fn write_object_with_limit_and_sync(
        &self,
        object: &TypedObject,
        limit: usize,
        sync_parent: bool,
    ) -> Result<ObjectWriteReceipt, StoreError> {
        object.verify_wire_version(&self.relation_registry)?;
        let encoded = encode_object(object, limit)?;
        let id = object.id();
        let path = self.object_path(id);
        let created = if sync_parent {
            write_immutable_with_status(&path, &encoded, &self.root.join("objects"))?
        } else {
            write_immutable_file_with_status(&path, &encoded)?
        };
        Ok(ObjectWriteReceipt::new(
            id,
            created,
            u64::try_from(encoded.len()).map_err(|_| StoreError::Bounds)?,
        ))
    }

    /// Checks whether an immutable object file exists and is a regular file.
    ///
    /// This is a metadata-only query; use [`Self::read_object`] when the
    /// object's typed bytes and authenticated identity are required.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the path exists but is not a
    /// regular file, and [`StoreError::Io`] for other filesystem failures.
    pub fn contains_object(&self, id: ObjectId) -> Result<bool, StoreError> {
        match fs::metadata(self.object_path(id)) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_error(&error)),
        }
    }

    /// Reads and admits one immutable typed object by identity.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] for a missing or malformed object and
    /// [`StoreError::Io`] for other filesystem failures.
    pub fn read_object(&self, id: ObjectId) -> Result<TypedObject, StoreError> {
        self.read_object_claim(UntrustedObjectId::from_bytes(*id.as_bytes()))
    }

    /// Reads and admits one immutable typed object named by an untrusted
    /// object identity claim.
    ///
    /// The claim is used only to select a bounded filesystem path. The object
    /// envelope, canonical schema/version bytes, and complete object
    /// commitment are verified before a [`TypedObject`] is returned.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] for a missing, malformed, or
    /// mismatched object and [`StoreError::Io`] for other filesystem errors.
    pub fn read_object_claim(&self, claim: UntrustedObjectId) -> Result<TypedObject, StoreError> {
        let path = self
            .root
            .join("objects")
            .join(format!("{}.object", hex(claim.as_bytes())));
        let metadata = fs::metadata(&path).map_err(|error| map_read_error(&error))?;
        let limit = self.object_envelope_limit()?;
        if metadata.len() > u64::try_from(limit).map_err(|_| StoreError::Bounds)? {
            return Err(StoreError::Bounds);
        }
        let bytes = fs::read(path).map_err(|error| map_read_error(&error))?;
        let object = decode_object(&bytes, limit, &self.relation_registry)?;
        let _ = claim.admit(&object)?;
        Ok(object)
    }

    /// Durably writes an immutable, complete closure manifest.
    ///
    /// # Errors
    ///
    /// Returns an admission, bounds, corruption, or filesystem error.
    pub fn write_closure(&self, manifest: &ClosureManifest) -> Result<ClosureId, StoreError> {
        manifest.admit_with_registry(&self.relation_registry)?;
        self.write_closure_after_admission(manifest, &[])
    }

    fn write_closure_after_admission(
        &self,
        manifest: &ClosureManifest,
        selected_roots: &[TypedObject],
    ) -> Result<ClosureId, StoreError> {
        let mut changed = manifest
            .changed_objects()
            .map(|object| (*object).clone())
            .collect::<Vec<_>>();
        for root in selected_roots {
            if !changed.iter().any(|object| object.id() == root.id()) {
                changed.push(root.clone());
            }
        }
        // Root-only path copying tracks relation-node changes precisely, but
        // small non-relation objects can be retained from an in-memory base
        // whose physical CAS differs (for example a derived catalog joined
        // after source publication). Ensure that bounded metadata frontier is
        // present without traversing any relation descendants.
        for object in manifest.objects() {
            if !self.relation_registry.contains_schema(object.schema()) {
                self.write_object_without_directory_sync(object)?;
            }
        }
        self.write_root_relation_batch(&changed)?;
        sync_directory(&self.root.join("objects"))?;
        let root = self.write_manifest_publication(manifest)?;
        let count = manifest.root_handle().summary().len;
        let bytes = nodes::encode_manifest_descriptor(manifest.id(), root, count)?;
        let path = self.closure_path(manifest.id());
        write_immutable(&path, &bytes, &self.root.join("closures"))?;
        Ok(manifest.id())
    }

    pub(super) fn write_root_closure(
        &self,
        manifest: &ClosureManifest,
    ) -> Result<ClosureId, StoreError> {
        manifest.admit_objects_with_registry(&self.relation_registry)?;
        self.write_closure_after_admission(manifest, &[])
    }

    pub(super) fn write_workspace_root_closure(
        &self,
        closure: &WorkspaceClosure,
    ) -> Result<ClosureId, StoreError> {
        let manifest = closure.manifest();
        manifest.admit_objects_with_registry(&self.relation_registry)?;
        self.write_closure_after_admission(manifest, closure.selected_roots())
    }

    /// Reads and admits a complete immutable closure manifest.
    ///
    /// Every relation node referenced as a child by a node in this closure
    /// must also be *in* this closure. That is the right rule for a closure
    /// that has to travel — a replication payload is complete or it is
    /// useless — and the wrong rule for a workspace closure, which is written
    /// root-only against a node-addressed store. Use
    /// [`Self::read_workspace_root_closure`] for the latter.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] for a missing or malformed object and
    /// [`StoreError::Io`] for other filesystem failures.
    pub fn read_closure(&self, id: ClosureId) -> Result<ClosureManifest, StoreError> {
        self.read_closure_inner(id, true)
    }

    /// Reads a closure written by the workspace root-only publication path.
    ///
    /// [`Self::write_workspace_root_closure`] admits a workspace closure with
    /// `admit_objects_with_registry`: every retained object is checked, and
    /// the relation *children* of those objects are deliberately not required
    /// to be members, because a path-copying persistent tree retains untouched
    /// canonical nodes in the node store rather than re-listing them in every
    /// closure. Reading the same closure back through [`Self::read_closure`]
    /// applies the complete-closure rule instead and rejects exactly those
    /// workspaces whose relation tree grew past a single node — a workspace
    /// with one leaf has no child edges to miss, so the asymmetry stays
    /// invisible until a project is large enough to need an internal node.
    ///
    /// This reader is the exact counterpart of the writer. It still admits
    /// every retained object through the relation registry and still checks
    /// that the reconstructed manifest hashes to `id`; only the
    /// child-membership rule, which a root-only closure never claimed to
    /// satisfy, is not applied.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] for a missing or malformed object or a
    /// manifest whose identity is not `id`, [`StoreError::Bounds`] when the
    /// closure envelope exceeds the configured pack ceiling, and
    /// [`StoreError::Io`] for other filesystem failures.
    pub fn read_workspace_root_closure(
        &self,
        id: ClosureId,
    ) -> Result<ClosureManifest, StoreError> {
        self.read_closure_inner(id, false)
    }

    /// Opens a compact node-CAS closure as an authenticated lazy view.
    ///
    /// Opening reads only the descriptor and manifest root node. Use
    /// [`DurableManifest::get`](crate::DurableManifest::get) for one object or
    /// [`DurableManifest::page`](crate::DurableManifest::page) for bounded
    /// iteration. Flat compatibility closures must be imported through
    /// [`Self::read_closure`] before they can be published in node-CAS form.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] for a missing, flat, or malformed
    /// closure descriptor and [`StoreError::Io`] for filesystem failures.
    pub fn open_closure(&self, id: ClosureId) -> Result<DurableManifest, StoreError> {
        let path = self.closure_path(id);
        let metadata = fs::metadata(&path).map_err(|error| map_read_error(&error))?;
        let limit = envelope_limit(self.max_pack_bytes)?;
        if metadata.len() > u64::try_from(limit).map_err(|_| StoreError::Bounds)? {
            return Err(StoreError::Bounds);
        }
        let bytes = fs::read(path).map_err(|error| map_read_error(&error))?;
        if !nodes::is_manifest_descriptor(&bytes) {
            return Err(StoreError::Corrupt);
        }
        let descriptor = nodes::decode_manifest_descriptor(&bytes, id)?;
        self.open_manifest_index(id, descriptor)
    }

    pub(in crate::durable) fn read_closure_root(
        &self,
        id: ClosureId,
    ) -> Result<ClosureManifest, StoreError> {
        self.read_closure_inner(id, false)
    }

    fn read_closure_inner(
        &self,
        id: ClosureId,
        verify_edges: bool,
    ) -> Result<ClosureManifest, StoreError> {
        let path = self.closure_path(id);
        let metadata = fs::metadata(&path).map_err(|error| map_read_error(&error))?;
        let limit = envelope_limit(self.max_pack_bytes)?;
        if metadata.len() > u64::try_from(limit).map_err(|_| StoreError::Bounds)? {
            return Err(StoreError::Bounds);
        }
        let bytes = fs::read(path).map_err(|error| map_read_error(&error))?;
        let compact = nodes::is_manifest_descriptor(&bytes);
        let manifest = if compact {
            let descriptor = nodes::decode_manifest_descriptor(&bytes, id)?;
            self.read_manifest_closure(id, descriptor.root, verify_edges)?
        } else if verify_edges {
            ClosureManifest::decode_with_registry(&bytes, limit, &self.relation_registry)?
        } else {
            ClosureManifest::decode_root_with_registry(&bytes, limit, &self.relation_registry)?
        };
        if manifest.id() != id {
            return Err(StoreError::Corrupt);
        }
        if !compact {
            for object in manifest.objects() {
                let loaded = self.read_object(object.id())?;
                if loaded != *object {
                    return Err(StoreError::Corrupt);
                }
            }
        }
        Ok(manifest)
    }

    /// Reads a complete closure and returns its checked immutable-object
    /// reference edges for reachability or garbage-collection walks.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the closure, an admitted object,
    /// or a relation descendant is invalid, and [`StoreError::Io`] for
    /// filesystem failures.
    pub fn closure_edges(&self, id: ClosureId) -> Result<Vec<ObjectEdge>, StoreError> {
        self.read_closure(id)?.object_edges(&self.relation_registry)
    }

    /// Returns the selected durable head after strict journal recovery.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] for a complete invalid journal/head
    /// record, or [`StoreError::Io`] for filesystem failures.
    pub fn head(&self) -> Result<Option<SelectedHead>, StoreError> {
        let _process_lock = self.acquire_process_lock()?;
        Ok(self.read_state()?.selected)
    }

    /// Opens the selected canonical tree as a lazy durable view.
    ///
    /// The operation validates the selected journal binding, compact tree
    /// descriptor, root node, and root closure object. Descendant nodes are
    /// loaded only by lookups or by explicit materialization.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] for an invalid selected tree or closure
    /// and [`StoreError::Io`] for filesystem failures.
    pub fn open_tree(&self) -> Result<Option<DurableTree>, StoreError> {
        let _process_lock = self.acquire_process_lock()?;
        let state = self.read_state()?;
        let Some(head) = state.selected else {
            return Ok(None);
        };
        let Some(tree) = self.open_tree_publication(head.descriptor.pack)? else {
            return Ok(None);
        };
        if tree.root().as_bytes() != &head.descriptor.target {
            return Err(StoreError::Corrupt);
        }
        let closure = self.read_closure_root(head.descriptor.closure)?;
        let relation_schema = backend_version::SchemaIdentity::of_relation::<RawRelation>();
        if !closure.objects().iter().any(|object| {
            object.schema() == relation_schema && object.version() == tree.root().as_bytes()
        }) {
            return Err(StoreError::Corrupt);
        }
        Ok(Some(tree))
    }

    /// Alias for [`Self::open_tree`] used by recovery-oriented callers.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the selected tree, closure, or
    /// journal binding is invalid, and [`StoreError::Io`] for filesystem
    /// failures.
    pub fn recover_tree(&self) -> Result<Option<DurableTree>, StoreError> {
        self.open_tree()
    }
}
