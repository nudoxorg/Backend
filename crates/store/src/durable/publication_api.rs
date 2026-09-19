//! `FileStore` preparation, publication, and map recovery façade.

use super::nodes;
use super::{
    CheckedWorkspacePublication, ClosureId, ClosureManifest, FilePrepared, FileStore, LayoutId,
    ObjectId, Pack, PackId, PathBuf, PublicationBase, PublicationDescriptor, RawRelation,
    SelectedHead, StateRoot, StoreError, StorePublicationAuthority, WorkspaceClosure,
    WorkspaceFilePrepared, WorkspaceRoot, base_matches, decode_pack, envelope_limit, hex,
    relation_object_limit,
};
use crate::OrderedMap;

impl FileStore {
    /// Prepares a map publication against the currently selected head.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::StaleHead`] when the selected head changes while
    /// preparing, or a validation/encoding error for the map.
    pub fn prepare_map(
        &self,
        map: &OrderedMap,
        layout: LayoutId,
    ) -> Result<FilePrepared, StoreError> {
        let base = self.head()?.map(SelectedHead::as_base);
        let root = map.root_node();
        let frontier = map
            .pending_tree_frontier()
            .filter(|(source, _)| base.is_some_and(|base| base.target() == *source))
            .map(|(_, nodes)| nodes);
        let tree = nodes::TreePublication::new(root.clone(), map.state_root(), layout, frontier)?;
        let closure = ClosureManifest::root_only_from_node(&root)?;
        self.prepare_tree_publication(tree, map.state_root(), layout, closure, base)
    }

    fn prepare_tree_publication(
        &self,
        tree: nodes::TreePublication,
        target: StateRoot<RawRelation>,
        layout: LayoutId,
        closure: ClosureManifest,
        base: Option<PublicationBase>,
    ) -> Result<FilePrepared, StoreError> {
        let _process_lock = self.acquire_process_lock()?;
        if tree.layout != layout
            || tree.target != target
            || tree.root.id().to_bytes() != *target.as_bytes()
        {
            return Err(StoreError::Corrupt);
        }
        closure.admit_objects_with_registry(&self.relation_registry)?;
        let state = self.read_state()?;
        let current = state.selected.map(|head| head.descriptor);
        if !base_matches(current.as_ref(), base) {
            return Err(StoreError::StaleHead);
        }
        let (base_root, base_generation) = match base {
            Some(base) => (Some(base.target), base.generation),
            None => (None, 0),
        };
        let target_generation = base_generation.checked_add(1).ok_or(StoreError::Bounds)?;
        let descriptor = PublicationDescriptor::from_parts(
            base_root,
            *target.as_bytes(),
            layout,
            tree.id,
            closure.id(),
            None,
            base_generation,
            target_generation,
        );
        Ok(FilePrepared {
            store: self.clone(),
            pack: None,
            tree: Some(tree),
            target,
            closure,
            workspace: None,
            descriptor,
        })
    }

    /// Prepares one checked publication with an exact observed base.
    ///
    /// A caller that owns a typed workspace can supply a [`WorkspaceClosure`]
    /// here.  Its root and complete object manifest are then bound into the
    /// transaction identity and durable frame.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::StaleHead`] for a mismatched base and an
    /// admission, bounds, or corruption error for invalid objects or roots.
    pub fn prepare_publication(
        &self,
        pack: Pack,
        target: StateRoot<RawRelation>,
        layout: LayoutId,
        closure: ClosureManifest,
        workspace: Option<WorkspaceClosure>,
        base: Option<PublicationBase>,
    ) -> Result<FilePrepared, StoreError> {
        let _process_lock = self.acquire_process_lock()?;
        if pack.layout() != layout || decode_pack(&pack)?.state_root() != target {
            return Err(StoreError::Corrupt);
        }
        closure.admit_with_registry(&self.relation_registry)?;
        if let Some(workspace) = &workspace {
            if workspace.manifest().id() != closure.id() {
                return Err(StoreError::Corrupt);
            }
            workspace.binding().verify()?;
        }
        let state = self.read_state()?;
        let current = state.selected.map(|head| head.descriptor);
        if !base_matches(current.as_ref(), base) {
            return Err(StoreError::StaleHead);
        }
        let (base_root, base_generation) = match base {
            Some(base) => (Some(base.target), base.generation),
            None => (None, 0),
        };
        let target_generation = base_generation.checked_add(1).ok_or(StoreError::Bounds)?;
        let workspace_binding = workspace.as_ref().map(WorkspaceClosure::binding);
        let pack_id = pack.id();
        let descriptor = PublicationDescriptor::from_parts(
            base_root,
            *target.as_bytes(),
            layout,
            pack_id,
            closure.id(),
            workspace_binding,
            base_generation,
            target_generation,
        );
        Ok(FilePrepared {
            store: self.clone(),
            pack: Some(pack),
            tree: None,
            target,
            closure,
            workspace,
            descriptor,
        })
    }

    /// Prepares a publication for a checked workspace root and complete typed
    /// closure.  The pack must already be an admitted immutable pack; this
    /// lets the workspace owner share physical packs without rewriting them.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::StaleHead`] for a mismatched base and an
    /// admission, bounds, or corruption error for invalid closure or pack
    /// identities.
    pub fn prepare_workspace_publication(
        &self,
        target: WorkspaceRoot,
        layout: LayoutId,
        pack: PackId,
        closure: WorkspaceClosure,
        base: Option<PublicationBase>,
    ) -> Result<WorkspaceFilePrepared, StoreError> {
        let _process_lock = self.acquire_process_lock()?;
        if closure.is_root_only() {
            closure
                .manifest()
                .admit_objects_with_registry(&self.relation_registry)?;
        } else {
            closure
                .manifest()
                .admit_with_registry(&self.relation_registry)?;
        }
        if closure.root() != target || closure.binding().closure() != closure.manifest().id() {
            return Err(StoreError::Corrupt);
        }
        closure.binding().verify()?;
        if self.read_pack(pack)?.layout() != layout {
            return Err(StoreError::Corrupt);
        }
        let state = self.read_state()?;
        let current = state.selected.map(|head| head.descriptor);
        if !base_matches(current.as_ref(), base) {
            return Err(StoreError::StaleHead);
        }
        let (base_root, base_generation) = match base {
            Some(base) => (Some(base.target), base.generation),
            None => (None, 0),
        };
        let target_generation = base_generation.checked_add(1).ok_or(StoreError::Bounds)?;
        let workspace_binding = closure.binding();
        let descriptor = PublicationDescriptor::from_parts(
            base_root,
            target.to_bytes(),
            layout,
            pack,
            closure.manifest().id(),
            Some(workspace_binding),
            base_generation,
            target_generation,
        );
        Ok(WorkspaceFilePrepared {
            store: self.clone(),
            closure,
            descriptor,
        })
    }

    /// Admits backend-version checked workspace evidence and prepares its
    /// publication in one operation.
    ///
    /// The object manifest must contain every typed reference selected by the
    /// checked target manifest, transition, and optional commit.  This helper
    /// is the composition seam for an engine owner that keeps planning and
    /// history in the version crate while delegating physical durability to
    /// the store.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::StaleHead`] for a mismatched base and
    /// [`StoreError::Corrupt`] when checked workspace evidence or immutable
    /// object admission fails.
    pub fn prepare_checked_workspace_publication(
        &self,
        publication: CheckedWorkspacePublication<'_>,
    ) -> Result<WorkspaceFilePrepared, StoreError> {
        let CheckedWorkspacePublication {
            target,
            transition,
            commit,
            objects,
            layout,
            pack,
            base,
            root_only,
        } = publication;
        let closure = if let Some(transition) = transition {
            if root_only {
                WorkspaceClosure::from_checked_transition_root_only_with_registry(
                    target,
                    transition,
                    commit,
                    objects,
                    &self.relation_registry,
                )?
            } else {
                WorkspaceClosure::from_checked_transition_with_registry(
                    target,
                    transition,
                    commit,
                    objects,
                    &self.relation_registry,
                )?
            }
        } else {
            if commit.is_some() {
                return Err(StoreError::Corrupt);
            }
            if root_only {
                WorkspaceClosure::from_checked_manifest_root_only_with_registry(
                    target,
                    objects,
                    &self.relation_registry,
                )?
            } else {
                WorkspaceClosure::from_checked_manifest_with_registry(
                    target,
                    objects,
                    &self.relation_registry,
                )?
            }
        };
        self.prepare_workspace_publication(target.root(), layout, pack, closure, base)
    }

    /// Publishes one map through the durable typestate protocol for store
    /// internal callers.
    ///
    /// # Errors
    ///
    /// Returns any preparation or publication error, including a stale base.
    pub fn publish(
        &self,
        map: &OrderedMap,
        layout: LayoutId,
    ) -> Result<StateRoot<RawRelation>, StoreError> {
        let prepared = self.prepare_map(map, layout)?;
        let durable = prepared.durable()?;
        Ok(durable.publish()?.root())
    }

    /// Publishes one map while holding the store's owner authority.
    ///
    /// # Errors
    ///
    /// Returns any preparation, authority, or publication error, including a
    /// stale base.
    pub fn publish_with_authority(
        &self,
        map: &OrderedMap,
        layout: LayoutId,
        authority: &StorePublicationAuthority,
    ) -> Result<StateRoot<RawRelation>, StoreError> {
        let prepared = self.prepare_map(map, layout)?;
        let durable = prepared.durable()?;
        Ok(durable.publish_with_authority(authority)?.root())
    }

    /// Recovers the newest fully paired publication and its map.
    ///
    /// Only an incomplete final frame is treated as a torn tail.  Any
    /// complete frame with a bad checksum, chain link, object binding, or
    /// prepare/publish pairing is corruption.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a complete frame, object, closure,
    /// or selected head is invalid, and [`StoreError::Io`] for filesystem
    /// failures.
    pub fn recover(&self) -> Result<Option<OrderedMap>, StoreError> {
        let _process_lock = self.acquire_process_lock()?;
        let state = self.read_state()?;
        let Some(head) = state.selected else {
            return Ok(None);
        };
        let map = if let Some((map, _)) = self.read_tree_publication(head.descriptor.pack)? {
            map
        } else {
            let pack = self.read_pack(head.descriptor.pack)?;
            decode_pack(&pack)?
        };
        if *map.state_root().as_bytes() != head.descriptor.target {
            return Err(StoreError::Corrupt);
        }
        let closure = if self.tree_pack_path(head.descriptor.pack).is_file() {
            self.read_closure_root(head.descriptor.closure)?
        } else {
            self.read_closure(head.descriptor.closure)?
        };
        if let Some(workspace) = head.descriptor.workspace {
            workspace.verify()?;
            if workspace.closure() != closure.id() {
                return Err(StoreError::Corrupt);
            }
        }
        self.ensure_head_file(&head)?;
        Ok(Some(map))
    }

    pub(in crate::durable) fn pack_path(&self, id: PackId) -> PathBuf {
        self.root
            .join("packs")
            .join(format!("{}.pack", hex(id.as_bytes())))
    }

    pub(in crate::durable) fn closure_path(&self, id: ClosureId) -> PathBuf {
        self.root
            .join("closures")
            .join(format!("{}.closure", hex(id.as_bytes())))
    }

    pub(in crate::durable) fn object_path(&self, id: ObjectId) -> PathBuf {
        self.root
            .join("objects")
            .join(format!("{}.object", hex(id.as_bytes())))
    }

    pub(in crate::durable) fn object_envelope_limit(&self) -> Result<usize, StoreError> {
        Ok(envelope_limit(self.max_pack_bytes)?.max(relation_object_limit()?))
    }
}
