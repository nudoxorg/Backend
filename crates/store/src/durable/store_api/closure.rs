//! Writes and reopens immutable closure manifests on a file store.

use super::*;

impl FileStore {
    /// Durably writes an immutable, complete closure manifest.
    ///
    /// # Errors
    ///
    /// Returns an admission, bounds, corruption, or filesystem error.
    pub fn write_closure(&self, manifest: &ClosureManifest) -> Result<ClosureId, StoreError> {
        manifest.admit_with_registry(&self.relation_registry)?;
        // A complete closure lists every relation node it needs as a member,
        // but its path-copy `changed` set is relative to whichever manifest
        // it was derived from, and that base need not have been physically
        // written. A model-built workspace closure extended with transition
        // payloads is exactly that case: only the payloads are "changed", so
        // the selected relation root would never reach the node index that
        // restart recovery reopens it through. Persist every member relation
        // node the index does not already hold.
        let mut unwritten = Vec::new();
        for object in manifest.objects() {
            if self.relation_registry.contains_schema(object.schema())
                && !self.relation_ref_matches(object.schema(), object.version())?
            {
                unwritten.push(object.clone());
            }
        }
        self.write_closure_after_admission(manifest, &unwritten)
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

    pub(in crate::durable) fn write_root_closure(
        &self,
        manifest: &ClosureManifest,
    ) -> Result<ClosureId, StoreError> {
        manifest.admit_objects_with_registry(&self.relation_registry)?;
        self.write_closure_after_admission(manifest, &[])
    }

    pub(in crate::durable) fn write_workspace_root_closure(
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
}
