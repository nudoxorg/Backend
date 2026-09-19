//! Schema-parametric relation-node admission and reference indexing.

use super::super::{
    FileStore, Hash, ObjectEdge, ObjectId, StoreError, TypedObject, fs, io_error, map_read_error,
    sync_directory,
};
use super::{TreeWriteStats, wire};
use backend_version::{
    CanonicalRelation, CheckedCanonicalRoot, CommittedChild, IdContext, SchemaIdentity, StateRoot,
    TreeNodeLoader, TreeNodeView, UntrustedId, admit_canonical_root_claim,
};
use std::collections::{BTreeSet, HashSet};

/// An owned, read-only adapter for the version layer's lazy relation tree.
///
/// Cloning this loader only clones the store handle; every node load still
/// passes through the store's checked relation admission and reference index.
#[derive(Clone, Debug)]
pub struct OwnedRelationNodeLoader {
    store: FileStore,
}

impl<R: CanonicalRelation> TreeNodeLoader<R> for OwnedRelationNodeLoader {
    type Error = StoreError;

    fn load(&self, claim: UntrustedId<R>) -> Result<CheckedCanonicalRoot<R>, Self::Error> {
        self.store.read_relation_node_claim(claim)
    }
}

#[derive(Default)]
struct NodeWriteCounters {
    nodes_written: usize,
    bytes_written: usize,
}

/// One authenticated child summary paired with its durable object identity.
///
/// The summary is decoded by `backend-version` from an already admitted parent
/// node. The object identity is obtained from the store's authenticated
/// relation reference index; callers do not need to parse a second node
/// grammar to continue a lazy walk.
#[derive(Clone, Debug)]
pub struct RelationNodeChild<R: CanonicalRelation> {
    summary: CommittedChild<R>,
    object: ObjectId,
}

impl<R: CanonicalRelation> RelationNodeChild<R> {
    /// Returns the authenticated child summary committed by the parent.
    #[must_use]
    pub const fn summary(&self) -> &CommittedChild<R> {
        &self.summary
    }

    /// Returns the immutable object containing the child node.
    #[must_use]
    pub const fn object(&self) -> ObjectId {
        self.object
    }
}

/// One durable relation node admitted by the backend-version canonical ABI.
///
/// Opening this view reads and authenticates exactly one node object. Branch
/// children remain summaries until the caller follows the returned object
/// identities, so opening a root does not materialize the relation or inspect
/// unrelated subtrees.
#[derive(Clone, Debug)]
pub struct RelationNodeRead<R: CanonicalRelation> {
    object: ObjectId,
    node: CheckedCanonicalRoot<R>,
    children: Box<[RelationNodeChild<R>]>,
}

impl<R: CanonicalRelation> RelationNodeRead<R> {
    /// Returns the physical identity of this node object.
    #[must_use]
    pub const fn object(&self) -> ObjectId {
        self.object
    }

    /// Returns the admitted canonical node and its typed root commitment.
    #[must_use]
    pub const fn node(&self) -> &CheckedCanonicalRoot<R> {
        &self.node
    }

    /// Returns authenticated child summaries paired with durable object IDs.
    #[must_use]
    pub fn children(&self) -> &[RelationNodeChild<R>] {
        &self.children
    }
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

    /// Returns an owned read-only loader for version-owned lazy relation
    /// trees. The loader remains valid after the borrowing `FileStore` value
    /// goes out of scope and performs checked admission on every node load.
    #[must_use]
    pub fn owned_relation_node_loader(&self) -> OwnedRelationNodeLoader {
        OwnedRelationNodeLoader {
            store: self.clone(),
        }
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
    ) -> Result<super::RelationNodeWriteStats, StoreError> {
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
        Ok(super::RelationNodeWriteStats {
            root,
            nodes_written: counters.nodes_written,
            bytes_written: counters.bytes_written,
        })
    }

    /// Reads one checked relation node from the schema/version reference
    /// index and validates its immutable object envelope.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the reference or object is absent,
    /// malformed, or admitted under another schema/version.
    pub fn read_relation_node<R: CanonicalRelation>(
        &self,
        root: StateRoot<R>,
    ) -> Result<TypedObject, StoreError> {
        let schema = SchemaIdentity::of_relation::<R>();
        let claim = UntrustedId::<R>::from_wire(&root.to_bytes(), IdContext::relation::<R>())
            .map_err(|_| StoreError::Corrupt)?;
        let (_, object, _) = self.read_relation_node_claim_parts(claim)?;
        if object.schema() != schema {
            return Err(StoreError::Corrupt);
        }
        Ok(object)
    }

    /// Reads and admits one relation node from an untrusted root claim.
    ///
    /// This is the loader seam for [`backend_version::LazyTree`].  The claim
    /// supplies only the schema-qualified lookup key; canonical node bytes
    /// are loaded from the relation reference index and admitted before the
    /// typed root is returned.  No caller can forge a typed root from a raw
    /// digest or bypass the relation grammar.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the reference, object envelope,
    /// canonical node, or claimed root is invalid.
    pub fn read_relation_node_claim<R: CanonicalRelation>(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<CheckedCanonicalRoot<R>, StoreError> {
        self.read_relation_node_claim_parts(claim)
            .map(|(_, _, node)| node)
    }

    pub(super) fn read_relation_node_claim_parts<R: CanonicalRelation>(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<(ObjectId, TypedObject, CheckedCanonicalRoot<R>), StoreError> {
        let schema = SchemaIdentity::of_relation::<R>();
        if claim.context() != IdContext::relation::<R>() {
            return Err(StoreError::Corrupt);
        }
        let object_id = self.read_relation_ref(schema, claim.as_bytes())?;
        let object = self.read_object(object_id)?;
        if object.schema() != schema || object.version() != claim.as_bytes() {
            return Err(StoreError::Corrupt);
        }
        let node =
            admit_canonical_root_claim(claim, object.bytes()).map_err(|_| StoreError::Corrupt)?;
        Ok((object_id, object, node))
    }

    /// Loads one authenticated relation node and resolves its direct child
    /// commitments to physical object IDs.
    ///
    /// The canonical bytes for the requested node are fetched and admitted
    /// once. Child summaries are decoded through the checked
    /// `backend-version` evidence and resolved through the authenticated
    /// relation index; child node bytes are not fetched.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the claim, node, or a direct child
    /// reference is missing or inconsistent.
    pub fn read_relation_node_with_children<R: CanonicalRelation>(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<RelationNodeRead<R>, StoreError> {
        if claim.context() != IdContext::relation::<R>() {
            return Err(StoreError::Corrupt);
        }
        let schema = SchemaIdentity::of_relation::<R>();
        let (object, _, node) = self.read_relation_node_claim_parts(claim)?;
        let mut children = Vec::new();
        for child in node.child_iter().map_err(|_| StoreError::Corrupt)? {
            let summary = child.map_err(|_| StoreError::Corrupt)?;
            let object = self.read_relation_ref(schema, summary.commitment.as_bytes())?;
            children.push(RelationNodeChild { summary, object });
        }
        Ok(RelationNodeRead {
            object,
            node,
            children: children.into_boxed_slice(),
        })
    }

    /// Loads one authenticated relation node from a checked typed root and
    /// resolves its direct child commitments to physical object IDs.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the root, node, or child index is
    /// inconsistent.
    pub fn read_checked_relation_node_with_children<R: CanonicalRelation>(
        &self,
        root: StateRoot<R>,
    ) -> Result<RelationNodeRead<R>, StoreError> {
        let claim = UntrustedId::<R>::from_wire(root.as_bytes(), IdContext::relation::<R>())
            .map_err(|_| StoreError::Corrupt)?;
        self.read_relation_node_with_children(claim)
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
    ) -> Result<super::RelationNodeWriteStats, StoreError> {
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
        Ok(super::RelationNodeWriteStats {
            root,
            nodes_written: counters.nodes_written,
            bytes_written: counters.bytes_written,
        })
    }

    /// Returns the direct immutable-object edges from one checked relation
    /// node. Callers can feed the result into a bounded mark cursor without
    /// loading the relation's full payload or manifest.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the node or any direct child
    /// reference is missing or malformed.
    pub fn relation_node_edges<R: CanonicalRelation>(
        &self,
        root: StateRoot<R>,
    ) -> Result<Vec<ObjectEdge>, StoreError> {
        let object = self.read_relation_node(root)?;
        let schema = SchemaIdentity::of_relation::<R>();
        let references =
            self.relation_registry
                .node_references(schema, object.version(), object.bytes())?;
        let children = references.children;
        let value_references = references.value_references;
        let mut edges = Vec::with_capacity(
            children
                .len()
                .checked_add(value_references.len())
                .ok_or(StoreError::Bounds)?,
        );
        for child in children {
            let child_id = self.read_relation_ref(schema, &child)?;
            let _ = self.read_object(child_id)?;
            edges.push(ObjectEdge::new(object.id(), child_id));
        }
        for reference in value_references {
            edges.push(ObjectEdge::new(
                object.id(),
                ObjectId::from_bytes(reference),
            ));
        }
        edges.sort_unstable();
        edges.dedup();
        Ok(edges)
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

    pub(super) fn write_relation_node_handle<R: CanonicalRelation>(
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

    pub(super) fn relation_ref_matches(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
    ) -> Result<bool, StoreError> {
        let path = self.relation_ref_path(schema, version);
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => {
                let object_id = self.read_relation_ref(schema, version)?;
                let object = self.read_object(object_id)?;
                if object.schema() != schema || object.version() != version {
                    return Err(StoreError::Corrupt);
                }
                Ok(true)
            }
            Ok(_) => Err(StoreError::Corrupt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_error(&error)),
        }
    }

    pub(super) fn relation_ref_path(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
    ) -> std::path::PathBuf {
        self.root.join("nodes").join(format!(
            "rel-{:02x}-{:04x}-{:02x}-{}.ref",
            schema.domain(),
            schema.ty(),
            schema.version(),
            wire::hex(version)
        ))
    }

    pub(in crate::durable) fn read_relation_ref(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
    ) -> Result<ObjectId, StoreError> {
        let path = self.relation_ref_path(schema, version);
        let bytes = fs::read(path).map_err(|error| map_read_error(&error))?;
        wire::decode_relation_ref(&bytes, schema, version)
    }
}

impl<R: CanonicalRelation> TreeNodeLoader<R> for FileStore {
    type Error = StoreError;

    fn load(&self, claim: UntrustedId<R>) -> Result<CheckedCanonicalRoot<R>, Self::Error> {
        self.read_relation_node_claim(claim)
    }
}
