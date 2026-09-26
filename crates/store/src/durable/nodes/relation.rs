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
mod write;

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
    /// Returns an owned read-only loader for version-owned lazy relation
    /// trees. The loader remains valid after the borrowing `FileStore` value
    /// goes out of scope and performs checked admission on every node load.
    #[must_use]
    pub fn owned_relation_node_loader(&self) -> OwnedRelationNodeLoader {
        OwnedRelationNodeLoader {
            store: self.clone(),
        }
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

    pub(in crate::durable) fn relation_ref_matches(
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
