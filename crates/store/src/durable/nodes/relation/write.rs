//! Writes admitted relation nodes and their schema/version references.

use super::*;

#[derive(Default)]
struct NodeWriteCounters {
    nodes_written: usize,
    bytes_written: usize,
}

impl FileStore {
    /// Writes the relation objects in one root-only closure and installs their
    /// schema/version references after validating the complete batch. Every
    /// child is either in this batch or already present in the durable CAS;
    /// no closure export is materialized to discover descendants.
    pub(in crate::durable) fn write_root_relation_batch(
        &self,
        objects: &[TypedObject],
    ) -> Result<(), StoreError> {
        let relation_objects = objects
            .iter()
            .filter(|object| self.relation_registry.contains_schema(object.schema()))
            .collect::<Vec<_>>();
        let batch = relation_objects
            .iter()
            .map(|object| (object.schema(), *object.version()))
            .collect::<BTreeSet<_>>();
        for object in &relation_objects {
            object.verify_wire_version(&self.relation_registry)?;
            let decoded = self.relation_registry.node_references(
                object.schema(),
                object.version(),
                object.bytes(),
            )?;
            for child in &decoded.children {
                if !batch.contains(&(object.schema(), *child))
                    && !self.relation_ref_matches(object.schema(), child)?
                {
                    return Err(StoreError::Corrupt);
                }
            }
        }
        let mut stats = NodeWriteCounters::default();
        for object in relation_objects {
            self.write_relation_object(object, object.schema(), object.version(), &mut stats)?;
        }
        sync_directory(&self.root.join("nodes"))?;
        Ok(())
    }

    /// Admits every newly reachable node of a checked relation state through
    /// the immutable object CAS and a schema/version reference index.
    ///
    /// The index is keyed by `(schema, state-root version)`, while object
    /// bytes remain content addressed by [`ObjectId`]. A node whose index
    /// already exists is validated and its descendants are skipped, so a
    /// path-copied update only examines the changed frontier.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a checked node, reference index,
    /// or immutable object disagrees with an existing identity, and
    /// [`StoreError::Io`] for filesystem failures.
    pub fn write_relation_state<R: CanonicalRelation>(
        &self,
        state: &backend_version::RelationState<R>,
    ) -> Result<super::super::RelationNodeWriteStats, StoreError> {
        fs::create_dir_all(self.root.join("nodes")).map_err(|error| io_error(&error))?;
        let mut closure = state.node_closure();
        let root = closure
            .try_next()
            .map_err(|_| StoreError::Corrupt)?
            .ok_or(StoreError::Corrupt)?;
        let schema = SchemaIdentity::of_relation::<R>();
        let mut counters = NodeWriteCounters::default();
        let mut seen = HashSet::new();
        let root = self.write_relation_node_view(root, schema, &mut counters, &mut seen)?;
        Ok(super::super::RelationNodeWriteStats {
            root,
            nodes_written: counters.nodes_written,
            bytes_written: counters.bytes_written,
        })
    }

    /// Writes only the changed canonical relation-node frontier emitted by a
    /// lazy path-copy update.  Unchanged descendants remain in the shared
    /// relation CAS, so one logical replacement writes O(tree height) nodes.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the update frontier does not
    /// contain its checked target or an emitted node cannot be admitted.
    pub fn write_lazy_relation_update<R: CanonicalRelation>(
        &self,
        update: &backend_version::LazyPreparedUpdate<R>,
    ) -> Result<super::super::RelationNodeWriteStats, StoreError> {
        fs::create_dir_all(self.root.join("nodes")).map_err(|error| io_error(&error))?;
        let schema = SchemaIdentity::of_relation::<R>();
        let target = update.target().root().to_bytes();
        let mut counters = NodeWriteCounters::default();
        let mut root_object = None;
        let mut seen = HashSet::new();
        for node in update.changed_nodes() {
            let version = node.commitment().to_bytes();
            if !seen.insert(version) {
                continue;
            }
            let object = TypedObject::from_state_root(node.commitment(), node)?;
            let (object_id, _) =
                self.write_relation_object(&object, schema, &version, &mut counters)?;
            if version == target {
                root_object = Some(object_id);
            }
        }
        let root = root_object.ok_or(StoreError::Corrupt)?;
        Ok(super::super::RelationNodeWriteStats {
            root,
            nodes_written: counters.nodes_written,
            bytes_written: counters.bytes_written,
        })
    }

    fn write_relation_node_view<R: CanonicalRelation>(
        &self,
        node: TreeNodeView<'_, R>,
        schema: SchemaIdentity,
        stats: &mut NodeWriteCounters,
        seen: &mut HashSet<Hash>,
    ) -> Result<ObjectId, StoreError> {
        let version = node.state_root().to_bytes();
        if !seen.insert(version) {
            return self.read_relation_ref(schema, &version);
        }
        let object = TypedObject::from_checked_state_object_ref(node.state_object())?;
        if object.schema() != schema || object.version() != &version {
            return Err(StoreError::Corrupt);
        }
        self.write_relation_object(&object, schema, &version, stats)?;
        for child in node.children() {
            let child_version = child.id().to_bytes();
            if self.relation_ref_matches(schema, &child_version)? {
                continue;
            }
            self.write_relation_node_view(child, schema, stats, seen)?;
        }
        Ok(object.id())
    }

    pub(in crate::durable::nodes) fn write_relation_node_handle<R: CanonicalRelation>(
        &self,
        node: &backend_version::TreeNodeHandle<R>,
        schema: SchemaIdentity,
        stats: &mut TreeWriteStats,
        seen: &mut HashSet<Hash>,
        recurse: bool,
    ) -> Result<ObjectId, StoreError> {
        let version = node.id().to_bytes();
        if !seen.insert(version) {
            return self.read_relation_ref(schema, &version);
        }
        let object = TypedObject::from_state_root(node.canonical().commitment(), node.canonical())?;
        if object.schema() != schema || object.version() != &version {
            return Err(StoreError::Corrupt);
        }
        self.write_relation_object_for_tree(&object, schema, &version, stats)?;
        if recurse {
            for child in node.children() {
                let child_version = child.id().to_bytes();
                if self.relation_ref_matches(schema, &child_version)? {
                    continue;
                }
                self.write_relation_node_handle(&child, schema, &mut *stats, seen, true)?;
            }
        }
        Ok(object.id())
    }

    fn write_relation_object(
        &self,
        object: &TypedObject,
        schema: SchemaIdentity,
        version: &Hash,
        stats: &mut NodeWriteCounters,
    ) -> Result<(ObjectId, bool), StoreError> {
        let receipt = self.write_relation_object_with_receipt(object)?;
        if receipt.created() {
            stats.nodes_written = stats
                .nodes_written
                .checked_add(1)
                .ok_or(StoreError::Bounds)?;
            stats.bytes_written = stats
                .bytes_written
                .checked_add(usize::try_from(receipt.bytes()).map_err(|_| StoreError::Bounds)?)
                .ok_or(StoreError::Bounds)?;
        }
        let index_path = self.relation_ref_path(schema, version);
        let index_created = if index_path.is_file() {
            if self.read_relation_ref(schema, version)? != object.id() {
                return Err(StoreError::Corrupt);
            }
            false
        } else {
            wire::write_relation_ref(
                &index_path,
                schema,
                version,
                object.id(),
                &self.root.join("nodes"),
            )?
        };
        if index_created {
            stats.bytes_written = stats
                .bytes_written
                .checked_add(wire::relation_ref_encoded_len())
                .ok_or(StoreError::Bounds)?;
        }
        Ok((object.id(), index_created))
    }

    pub(super) fn write_relation_object_for_tree(
        &self,
        object: &TypedObject,
        schema: SchemaIdentity,
        version: &Hash,
        stats: &mut TreeWriteStats,
    ) -> Result<(), StoreError> {
        stats.nodes_visited = stats
            .nodes_visited
            .checked_add(1)
            .ok_or(StoreError::Bounds)?;
        let receipt = self.write_relation_object_with_receipt(object)?;
        if receipt.created() {
            stats.nodes_written = stats
                .nodes_written
                .checked_add(1)
                .ok_or(StoreError::Bounds)?;
            stats.bytes_written = stats
                .bytes_written
                .checked_add(usize::try_from(receipt.bytes()).map_err(|_| StoreError::Bounds)?)
                .ok_or(StoreError::Bounds)?;
        }
        let index_path = self.relation_ref_path(schema, version);
        let index_created = if index_path.is_file() {
            if self.read_relation_ref(schema, version)? != object.id() {
                return Err(StoreError::Corrupt);
            }
            false
        } else {
            wire::write_relation_ref(
                &index_path,
                schema,
                version,
                object.id(),
                &self.root.join("nodes"),
            )?
        };
        if index_created {
            stats.bytes_written = stats
                .bytes_written
                .checked_add(wire::relation_ref_encoded_len())
                .ok_or(StoreError::Bounds)?;
        }
        Ok(())
    }
}
