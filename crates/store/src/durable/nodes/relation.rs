//! Schema-parametric relation-node admission and reference indexing.

use super::super::{FileStore, Hash, ObjectEdge, ObjectId, StoreError, TypedObject, fs, io_error};
use super::{RelationAdmission, TreeWriteStats, wire};
use backend_version::{
    CanonicalRelation, CheckedCanonicalRoot, CommittedChild, IdContext, SchemaIdentity, StateRoot,
    TreeNodeLoader, TreeNodeView, UntrustedId, admit_canonical_root_claim,
};
use std::collections::HashSet;
use std::io::Read as _;

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

fn relation_write_checkpoint() -> Result<(), StoreError> {
    if super::super::artifact_fs::maybe_test_fault(14) {
        return Err(StoreError::Io(
            "injected interruption after relation object and reference".to_owned(),
        ));
    }
    Ok(())
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
    /// Installs or checks the schema/version reference for an object already
    /// streamed into the immutable object CAS.
    pub(in crate::durable) fn write_relation_reference(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
        object: ObjectId,
    ) -> Result<bool, StoreError> {
        if !self.relation_registry.contains_schema(schema) {
            return Ok(false);
        }
        let path = self.relation_ref_path(schema, version);
        if path.is_file() {
            return if self.read_relation_ref(schema, version)? == object {
                Ok(false)
            } else {
                Err(StoreError::Corrupt)
            };
        }
        fs::create_dir_all(self.root.join("nodes")).map_err(|error| io_error(&error))?;
        wire::write_relation_ref(&path, schema, version, object, &self.root.join("nodes"))
    }

    /// Writes the relation objects in one root-only closure and installs their
    /// schema/version references after validating the complete batch. Every
    /// child is either in this batch or already present in the durable CAS;
    /// no closure export is materialized to discover descendants.
    pub(in crate::durable) fn write_root_relation_batch(
        &self,
        objects: &[TypedObject],
        admission: &mut RelationAdmission<'_>,
    ) -> Result<(), StoreError> {
        let relation_objects = objects.iter()
            .filter(|object| self.relation_registry.contains_schema(object.schema()))
            .collect::<Vec<_>>();
        let mut stats = NodeWriteCounters::default();
        self.write_relation_objects_in_order(&relation_objects, &mut stats, admission)
    }

    fn write_relation_objects_in_order(
        &self,
        objects: &[&TypedObject],
        stats: &mut NodeWriteCounters,
        admission: &mut RelationAdmission<'_>,
    ) -> Result<(), StoreError> {
        if objects.len() > super::NODE_MAX_COUNT { return Err(StoreError::Bounds); }
        let members = objects.iter().enumerate()
            .map(|(index, object)| ((object.schema(), *object.version()), index))
            .collect::<std::collections::BTreeMap<_, _>>();
        let identities = objects.iter().enumerate()
            .map(|(index, object)| (object.id(), index))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut states = vec![0_u8; objects.len()];
        for root in 0..objects.len() {
            let mut pending = vec![(root, false)];
            while let Some((index, finish)) = pending.pop() {
                if states[index] == 2 { continue; }
                let object = objects[index];
                if finish {
                    self.write_relation_object(object, object.schema(), object.version(), stats, admission)?;
                    states[index] = 2;
                    continue;
                }
                if states[index] == 1 { return Err(StoreError::Corrupt); }
                states[index] = 1;
                object.verify_wire_version(&self.relation_registry)?;
                let references = self.relation_registry.node_references(object.schema(), object.version(), object.bytes())?;
                pending.push((index, true));
                for version in references.children {
                    if let Some(child) = members.get(&(object.schema(), version)) {
                        if states[*child] != 2 { pending.push((*child, false)); }
                    } else if admission.existing(object.schema(), &version)?.is_none() {
                        return Err(StoreError::Corrupt);
                    }
                }
                for reference in references.value_references {
                    if let Some(child) = identities.get(&ObjectId::from_bytes(reference)) {
                        if states[*child] != 2 { pending.push((*child, false)); }
                    }
                }
                if pending.len() > super::NODE_MAX_COUNT.checked_mul(4).ok_or(StoreError::Bounds)? {
                    return Err(StoreError::Bounds);
                }
            }
        }
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
        let pin = self.acquire_gc_pin()?;
        let _process_lock = self.acquire_process_lock()?;
        let mut admission = RelationAdmission::new(self, &pin)?;
        let root = self.write_relation_node_view(root, schema, &mut counters, &mut seen, &mut admission)?;
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
        let pin = self.acquire_gc_pin()?;
        let _process_lock = self.acquire_process_lock()?;
        let mut admission = RelationAdmission::new(self, &pin)?;
        self.write_lazy_relation_update_admitted(update, &mut admission)
    }

    pub(in crate::durable) fn write_lazy_relation_update_admitted<R: CanonicalRelation>(
        &self,
        update: &backend_version::LazyPreparedUpdate<R>,
        admission: &mut RelationAdmission<'_>,
    ) -> Result<super::RelationNodeWriteStats, StoreError> {
        fs::create_dir_all(self.root.join("nodes")).map_err(|error| io_error(&error))?;
        let schema = SchemaIdentity::of_relation::<R>();
        let target = update.target().root().to_bytes();
        let mut counters = NodeWriteCounters::default();
        let mut root_object = None;
        let mut seen = HashSet::new();
        let objects = update.changed_nodes().iter().map(|node| {
            TypedObject::from_state_root(node.commitment(), node)
        }).collect::<Result<Vec<_>, StoreError>>()?;
        let objects = objects.iter().filter(|object| seen.insert(*object.version())).collect::<Vec<_>>();
        self.write_relation_objects_in_order(&objects, &mut counters, admission)?;
        for object in objects {
            if object.version() == &target { root_object = Some(object.id()); }
        }
        let root = if let Some(root) = root_object {
            root
        } else if admission.existing(schema, &target)?.is_some() {
            self.read_relation_ref(schema, &target)?
        } else {
            // An empty initial tree has no changed frontier but still needs a
            // durable root node so its closure descriptor survives reopen.
            let object =
                TypedObject::from_state_root(update.target().root(), update.target().node())?;
            self.write_relation_object(&object, schema, &target, &mut counters, admission)?
                .0
        };
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
        admission: &mut RelationAdmission<'_>,
    ) -> Result<ObjectId, StoreError> {
        let version = node.state_root().to_bytes();
        if !seen.insert(version) {
            return self.read_relation_ref(schema, &version);
        }
        let object = TypedObject::from_checked_state_object_ref(node.state_object())?;
        if object.schema() != schema || object.version() != &version {
            return Err(StoreError::Corrupt);
        }
        for child in node.children() {
            let child_version = child.id().to_bytes();
            if admission.existing(schema, &child_version)?.is_some() {
                continue;
            }
            self.write_relation_node_view(child, schema, stats, seen, admission)?;
        }
        self.write_relation_object(&object, schema, &version, stats, admission)?;
        Ok(object.id())
    }

    pub(super) fn write_relation_node_handle<R: CanonicalRelation>(
        &self,
        node: &backend_version::TreeNodeHandle<R>,
        schema: SchemaIdentity,
        stats: &mut TreeWriteStats,
        seen: &mut HashSet<Hash>,
        _recurse: bool,
        admission: &mut RelationAdmission<'_>,
    ) -> Result<ObjectId, StoreError> {
        let version = node.id().to_bytes();
        if !seen.insert(version) {
            return self.read_relation_ref(schema, &version);
        }
        let object = TypedObject::from_state_root(node.canonical().commitment(), node.canonical())?;
        if object.schema() != schema || object.version() != &version {
            return Err(StoreError::Corrupt);
        }
        for child in node.children() {
            let child_version = child.id().to_bytes();
            if admission.existing(schema, &child_version)?.is_some() { continue; }
            self.write_relation_node_handle(&child, schema, &mut *stats, seen, true, admission)?;
        }
        self.write_relation_object_for_tree(&object, schema, &version, stats, admission)?;
        Ok(object.id())
    }

    fn write_relation_object(
        &self,
        object: &TypedObject,
        schema: SchemaIdentity,
        version: &Hash,
        stats: &mut NodeWriteCounters,
        admission: &mut RelationAdmission<'_>,
    ) -> Result<(ObjectId, bool), StoreError> {
        admission.verify_for(self)?;
        let receipt = self.write_relation_object_with_receipt(object)?;
        admission.object_written(receipt);
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
        admission.checkpoint(1, object.id())?;
        let complete = admission.admit_object(object.id())?.ok_or(StoreError::Corrupt)?;
        admission.checkpoint(2, object.id())?;
        let index_path = self.relation_ref_path(schema, version);
        let index_created = if index_path.is_file() {
            if self.read_relation_ref(schema, version)? != object.id() {
                return Err(StoreError::Corrupt);
            }
            false
        } else {
            wire::write_complete_relation_ref(
                &index_path,
                schema,
                version,
                complete,
                &self.root.join("nodes"),
            )?
        };
        if index_created {
            stats.bytes_written = stats
                .bytes_written
                .checked_add(wire::relation_ref_encoded_len())
                .ok_or(StoreError::Bounds)?;
        }
        admission.reference_installed(schema, version, object.id(), index_created)?;
        relation_write_checkpoint()?;
        Ok((object.id(), index_created))
    }

    pub(super) fn write_relation_object_for_tree(
        &self,
        object: &TypedObject,
        schema: SchemaIdentity,
        version: &Hash,
        stats: &mut TreeWriteStats,
        admission: &mut RelationAdmission<'_>,
    ) -> Result<(), StoreError> {
        stats.nodes_visited = stats
            .nodes_visited
            .checked_add(1)
            .ok_or(StoreError::Bounds)?;
        admission.verify_for(self)?;
        let receipt = self.write_relation_object_with_receipt(object)?;
        admission.object_written(receipt);
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
        admission.checkpoint(1, object.id())?;
        let complete = admission.admit_object(object.id())?.ok_or(StoreError::Corrupt)?;
        admission.checkpoint(2, object.id())?;
        let index_path = self.relation_ref_path(schema, version);
        let index_created = if index_path.is_file() {
            if self.read_relation_ref(schema, version)? != object.id() {
                return Err(StoreError::Corrupt);
            }
            false
        } else {
            wire::write_complete_relation_ref(
                &index_path,
                schema,
                version,
                complete,
                &self.root.join("nodes"),
            )?
        };
        if index_created {
            stats.bytes_written = stats
                .bytes_written
                .checked_add(wire::relation_ref_encoded_len())
                .ok_or(StoreError::Bounds)?;
        }
        admission.reference_installed(schema, version, object.id(), index_created)?;
        relation_write_checkpoint()?;
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::durable) fn relation_ref_matches(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
    ) -> Result<bool, StoreError> {
        let pin = self.acquire_gc_pin()?;
        Ok(RelationAdmission::new(self, &pin)?.existing(schema, version)?.is_some())
    }

    pub(in crate::durable) fn relation_ref_exists(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
    ) -> Result<bool, StoreError> {
        Ok(self
            .read_relation_ref_if_present(schema, version)?
            .is_some())
    }

    pub(in crate::durable) fn relation_ref_path(
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
        self.read_relation_ref_if_present(schema, version)?
            .ok_or(StoreError::Corrupt)
    }

    pub(super) fn read_relation_ref_if_present(
        &self,
        schema: SchemaIdentity,
        version: &Hash,
    ) -> Result<Option<ObjectId>, StoreError> {
        let path = self.relation_ref_path(schema, version);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(StoreError::Corrupt)?;
        let Some(mut file) = super::super::artifact_fs::open_store_file(self, "nodes", name)?
        else {
            return Ok(None);
        };
        let expected_len = wire::relation_ref_encoded_len();
        if usize::try_from(file.metadata().map_err(|error| io_error(&error))?.len())
            .map_err(|_| StoreError::Bounds)?
            != expected_len
        {
            return Err(StoreError::Corrupt);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(expected_len)
            .map_err(|_| StoreError::Bounds)?;
        file.read_to_end(&mut bytes)
            .map_err(|error| io_error(&error))?;
        if bytes.len() != expected_len {
            return Err(StoreError::Corrupt);
        }
        wire::decode_relation_ref(&bytes, schema, version).map(Some)
    }
}

impl<R: CanonicalRelation> TreeNodeLoader<R> for FileStore {
    type Error = StoreError;

    fn load(&self, claim: UntrustedId<R>) -> Result<CheckedCanonicalRoot<R>, Self::Error> {
        self.read_relation_node_claim(claim)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use super::super::super::artifact_fs::{
        TestDirectoryCleanup, remove_test_directory as remove_store_fixture, remove_test_file,
    };
    use backend_version::{ObjectKey, Schema};
    use std::fs::File;
    use std::path::PathBuf;

    struct BytesSchema;

    impl Schema for BytesSchema {
        const DOMAIN: u8 = 0xf2;
        const TYPE: u16 = 0x5102;
        type Value = Vec<u8>;

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    fn test_store(label: &str) -> (TestDirectoryCleanup, FileStore, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-store-relation-ref-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = remove_store_fixture(&root);
        let cleanup = TestDirectoryCleanup::new(&root);
        let store = FileStore::open(&root, 1024 * 1024).expect("open test store");
        (cleanup, store, root)
    }

    fn bytes_object(bytes: Vec<u8>) -> TypedObject {
        let key = ObjectKey::<BytesSchema>::from_value(&bytes);
        TypedObject::from_value(&key, &bytes)
    }

    fn write_reference(store: &FileStore, object: &TypedObject, target: ObjectId) {
        let schema = object.schema();
        let version = *object.version();
        let path = store.relation_ref_path(schema, &version);
        wire::write_relation_ref(&path, schema, &version, target, &store.root.join("nodes"))
            .expect("write relation reference");
    }

    fn index_object(store: &FileStore, object: &TypedObject) {
        store.write_object(object).expect("write relation object");
        write_reference(store, object, object.id());
    }

    fn replace_object(store: &FileStore, id: ObjectId, bytes: &[u8]) {
        let path = store.object_path(id);
        remove_immutable_fixture(&path);
        fs::write(path, bytes).expect("write replacement object fixture");
    }

    fn remove_immutable_fixture(path: &std::path::Path) {
        remove_test_file(path).expect("remove immutable fixture without unsealing");
    }

    fn branch() -> crate::Node {
        let map = crate::OrderedMap::try_from_iter((0..2048_u64).map(|key| {
            (key.to_be_bytes().to_vec(), crate::StoredValue::new(vec![1], 1, vec![]))
        })).expect("branch fixture");
        let node = map.root_node();
        assert!(node.children().next().is_some(), "fixture must have dependencies");
        node
    }

    fn root_object(node: &crate::Node) -> TypedObject {
        TypedObject::from_state_root(node.canonical().commitment(), node.canonical()).expect("root object")
    }

    fn install_branch(store: &FileStore, root: &crate::Node) -> Result<ObjectId, StoreError> {
        let pin = store.acquire_gc_pin()?;
        let _process_lock = store.acquire_process_lock()?;
        let mut admission = RelationAdmission::new(store, &pin)?;
        store.write_relation_node_handle(root, SchemaIdentity::of_relation::<crate::RawRelation>(),
            &mut TreeWriteStats::default(), &mut HashSet::new(), true, &mut admission)
    }

    #[test]
    fn legacy_parent_reference_without_descendants_is_a_miss_after_cold_reopen_and_repairs() {
        let (_cleanup_path, store, path) = test_store("legacy-incomplete-parent");
        let node = branch();
        let object = root_object(&node);
        index_object(&store, &object);
        let schema = object.schema();
        let version = *object.version();
        assert_eq!(store.relation_ref_matches(schema, &version), Ok(false));
        drop(store);
        let store = FileStore::open(&path, 1024 * 1024).expect("cold reopen");
        assert_eq!(store.relation_ref_matches(schema, &version), Ok(false));
        install_branch(&store, &node).expect("repair complete dependency graph");
        assert_eq!(store.relation_ref_matches(schema, &version), Ok(true));
        drop(store);
        let store = FileStore::open(&path, 1024 * 1024).expect("reopen repaired tree");
        assert_eq!(store.relation_ref_matches(schema, &version), Ok(true));
        drop(store);
        remove_store_fixture(path).expect("remove fixture");
    }

    #[test]
    fn interruption_after_object_or_child_barriers_never_installs_a_parent_reference() {
        for point in [1, 2] {
            let (_cleanup_path, store, path) = test_store("interrupted-parent");
            let node = branch();
            let object = root_object(&node);
            super::super::dependencies::set_test_fault(point, object.id());
            assert!(install_branch(&store, &node).is_err());
            assert!(store.contains_object(object.id()).expect("parent object exists"));
            assert!(!store.relation_ref_exists(object.schema(), object.version()).expect("parent ref absent"));
            assert_eq!(store.relation_ref_matches(object.schema(), object.version()), Ok(false));
            drop(store);
            let store = FileStore::open(&path, 1024 * 1024).expect("cold reopen");
            assert_eq!(store.relation_ref_matches(object.schema(), object.version()), Ok(false));
            install_branch(&store, &node).expect("retry complete admission");
            assert_eq!(store.relation_ref_matches(object.schema(), object.version()), Ok(true));
            drop(store);
            remove_store_fixture(path).expect("remove fixture");
        }
    }

    #[test]
    fn failed_dependency_file_or_directory_barrier_cannot_populate_the_reuse_memo() {
        for point in [3, 4] {
            let (_cleanup_path, store, path) = test_store("dependency-barrier-failure");
            let node = branch();
            let object = root_object(&node);
            install_branch(&store, &node).expect("install complete branch");
            let pin = store.acquire_gc_pin().expect("GC custody");
            let mut admission = RelationAdmission::new(&store, &pin).expect("dependency admission");
            super::super::dependencies::set_test_fault(point, object.id());
            assert!(admission.existing(object.schema(), object.version()).is_err());
            let child = node.children().next().expect("child");
            remove_immutable_fixture(&store.object_path(root_object(&child).id()));
            assert!(admission.existing(object.schema(), object.version()).expect("probe again").is_none());
            drop(admission);
            drop(pin);
            drop(store);
            remove_store_fixture(path).expect("remove fixture");
        }
    }

    #[test]
    fn cancelled_dependency_admission_grants_no_reuse_receipt_and_releases_gc_custody() {
        let (_cleanup_path, store, path) = test_store("cancelled-dependencies");
        let node = branch();
        let object = root_object(&node);
        install_branch(&store, &node).expect("install complete branch");
        let pin = store.acquire_gc_pin().expect("GC custody");
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let mut admission = RelationAdmission::controlled(&store, &pin, Some(&cancelled)).expect("controlled admission");
        assert!(admission.existing(object.schema(), object.version()).is_err());
        assert!(store.try_with_gc_exclusive_lease(|| Ok(())).expect("try GC").is_none());
        cancelled.store(false, std::sync::atomic::Ordering::Release);
        assert!(admission.existing(object.schema(), object.version()).expect("retry").is_some());
        drop(admission);
        drop(pin);
        assert!(store.try_with_gc_exclusive_lease(|| Ok(())).expect("try GC again").is_some());
        drop(store);
        remove_store_fixture(path).expect("remove fixture");
    }

    #[test]
    fn dependency_admission_refuses_a_pin_for_another_store() {
        let (_cleanup_first_path, first, first_path) = test_store("first-pin-root");
        let (_cleanup_second_path, second, second_path) = test_store("second-pin-root");
        let pin = second.acquire_gc_pin().expect("second root pin");
        assert!(RelationAdmission::new(&first, &pin).is_err());
        drop((pin, first, second));
        remove_store_fixture(first_path).expect("remove first root");
        remove_store_fixture(second_path).expect("remove second root");
    }

    #[test]
    fn cold_recovery_removes_only_a_reference_writers_orphan_temporary_link() {
        let (_cleanup_path, store, path) = test_store("orphan-ref-link");
        let node = branch();
        let object = root_object(&node);
        install_branch(&store, &node).expect("complete branch");
        let reference = store.relation_ref_path(object.schema(), object.version());
        let temporary = reference.with_file_name(format!(
            ".{}.4242.17.tmp", reference.file_name().expect("name").to_string_lossy(),
        ));
        fs::hard_link(&reference, &temporary).expect("simulate crash before temporary unlink");
        let unrelated = path.join("nodes/notes.tmp");
        fs::write(&unrelated, b"not a store writer temporary").expect("unrelated file");
        drop(store);
        let store = FileStore::open(&path, 1024 * 1024).expect("recover writer temporary");
        assert!(!temporary.exists());
        assert!(unrelated.exists());
        assert_eq!(store.relation_ref_matches(object.schema(), object.version()), Ok(true));
        drop(store);
        remove_store_fixture(path).expect("remove fixture");
    }

    #[test]
    fn relation_dependency_crash_probe_child() {
        let Some(path) = std::env::var_os("BACKEND_STORE_DEPENDENCY_CRASH_ROOT") else { return; };
        let point = std::env::var("BACKEND_STORE_DEPENDENCY_CRASH_POINT")
            .expect("crash point").parse::<u8>().expect("numeric crash point");
        let store = FileStore::open(PathBuf::from(path), 1024 * 1024).expect("child store");
        let node = branch();
        super::super::dependencies::set_test_fault(point, root_object(&node).id());
        assert!(install_branch(&store, &node).is_ok(), "crash point must terminate the child");
    }

    #[test]
    fn process_loss_between_parent_object_and_reference_cold_reopens_without_false_reuse() {
        for point in [1_u8, 2] {
            let (_cleanup_path, store, path) = test_store("process-loss-parent");
            drop(store);
            let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "durable::nodes::relation::tests::relation_dependency_crash_probe_child", "--nocapture"])
                .env("BACKEND_STORE_DEPENDENCY_CRASH_ROOT", &path)
                .env("BACKEND_STORE_DEPENDENCY_CRASH_POINT", point.to_string())
                .status().expect("run crash child");
            assert_eq!(status.code(), Some(160 + i32::from(point)));
            let store = FileStore::open(&path, 1024 * 1024).expect("recover after process loss");
            let node = branch();
            let object = root_object(&node);
            assert_eq!(store.relation_ref_matches(object.schema(), object.version()), Ok(false));
            install_branch(&store, &node).expect("retry after process loss");
            assert_eq!(store.relation_ref_matches(object.schema(), object.version()), Ok(true));
            drop(store);
            remove_store_fixture(path).expect("remove fixture");
        }
    }

    #[test]
    fn dangling_relation_reference_is_a_miss() {
        let (_cleanup_root, store, root) = test_store("dangling");
        let object = bytes_object(b"dangling relation node".to_vec());
        index_object(&store, &object);
        let schema = object.schema();
        let version = *object.version();
        remove_immutable_fixture(&store.object_path(object.id()));

        assert_eq!(store.relation_ref_matches(schema, &version), Ok(false));
        let _ = remove_store_fixture(root);
    }

    #[test]
    fn relation_reference_rejects_a_different_object_descriptor() {
        let (_cleanup_root, store, root) = test_store("wrong-descriptor");
        let expected = bytes_object(b"expected relation node".to_vec());
        let actual = bytes_object(b"different relation node".to_vec());
        store.write_object(&actual).expect("write actual object");
        let schema = expected.schema();
        let version = *expected.version();
        write_reference(&store, &expected, actual.id());

        assert_eq!(
            store.relation_ref_matches(schema, &version),
            Err(StoreError::Corrupt)
        );
        let _ = remove_store_fixture(root);
    }

    #[test]
    fn relation_reference_rejects_a_corrupt_or_wrong_hash_envelope() {
        let (_cleanup_root, store, root) = test_store("corrupt-object");
        let object = bytes_object(b"relation node bytes".to_vec());
        index_object(&store, &object);
        let schema = object.schema();
        let version = *object.version();
        let original = fs::read(store.object_path(object.id())).expect("read object envelope");
        let mut corrupt = original.clone();
        corrupt[0] ^= 0x01;
        replace_object(&store, object.id(), &corrupt);
        assert_eq!(
            store.relation_ref_matches(schema, &version),
            Err(StoreError::Corrupt)
        );

        let mut wrong_hash = original;
        wrong_hash[super::super::super::OBJECT_MAGIC.len()] ^= 0x01;
        replace_object(&store, object.id(), &wrong_hash);

        assert_eq!(
            store.relation_ref_matches(schema, &version),
            Err(StoreError::Corrupt)
        );
        let _ = remove_store_fixture(root);
    }

    #[test]
    fn relation_reference_rejects_an_oversized_object() {
        let (_cleanup_root, store, root) = test_store("oversized-object");
        let object = bytes_object(b"relation node bytes".to_vec());
        index_object(&store, &object);
        let schema = object.schema();
        let version = *object.version();
        let path = store.object_path(object.id());
        remove_immutable_fixture(&path);
        let file = File::create(path).expect("create sparse object fixture");
        let oversized_len = u64::try_from(store.object_envelope_limit().expect("envelope limit"))
            .expect("limit fits")
            + 1;
        file.set_len(oversized_len)
            .expect("grow object past the bounded envelope");

        assert_eq!(
            store.relation_ref_matches(schema, &version),
            Err(StoreError::Bounds)
        );
        let _ = remove_store_fixture(root);
    }

    #[cfg(unix)]
    #[test]
    fn relation_reference_rejects_a_symlinked_object_path() {
        let (_cleanup_root, store, root) = test_store("symlink-object");
        let object = bytes_object(b"relation node bytes".to_vec());
        index_object(&store, &object);
        let schema = object.schema();
        let version = *object.version();
        let path = store.object_path(object.id());
        let target = root.join("outside-object");
        fs::write(&target, b"not an object envelope").expect("write symlink target");
        remove_immutable_fixture(&path);
        std::os::unix::fs::symlink(&target, &path).expect("replace object with symlink");

        assert_eq!(
            store.relation_ref_matches(schema, &version),
            Err(StoreError::UnsafePath)
        );
        let _ = remove_store_fixture(root);
    }

    #[cfg(unix)]
    #[test]
    fn relation_reference_rejects_a_hard_linked_object_path() {
        let (_cleanup_root, store, root) = test_store("hardlink-object");
        let object = bytes_object(b"relation node bytes".to_vec());
        index_object(&store, &object);
        let schema = object.schema();
        let version = *object.version();
        let path = store.object_path(object.id());
        let alias = root.join("object-alias");
        fs::hard_link(&path, &alias).expect("create hostile hard link");

        assert_eq!(
            store.relation_ref_matches(schema, &version),
            Err(StoreError::UnsafePath)
        );
        let _ = remove_store_fixture(root);
    }
}
