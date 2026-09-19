//! Durable catalog descriptor and lazy authenticated relation traversal.
//!
//! Relation nodes are written through `FileStore::write_relation_state`,
//! which uses the store's shared content-addressed node/ref index.  The
//! workspace closure contains one small descriptor and the source closure;
//! output/manifest bytes and relation descendants remain in that same store
//! CAS.  A lookup reads only the descriptor, the requested relation path, and
//! the two referenced immutable payload objects.

use super::super::owner::WorkspaceError;
use super::relation::{FreshnessRelation, PayloadRefRelation, PrimaryRelation};
use super::{DerivedOutputIndexSchema, INDEX_SCHEMA_TYPE, MAX_CATALOG_ENTRIES};
use backend_store::{DurableManifest, FileStore, ObjectId, TypedObject, UntrustedObjectId};
use backend_version::{
    CanonicalRelation, IdContext, LazyTree, LazyTreeError, RelationState, SchemaIdentity,
    StateRoot, TreeNodeLoader, UntrustedId, admit_canonical_root_claim,
};

pub(super) const DESCRIPTOR_MAGIC: &[u8] = b"backend.engine.derived-output.relation-index.v2\0";
pub(super) const DESCRIPTOR_BYTES: usize = DESCRIPTOR_MAGIC.len() + 8 + 32 * 6;

pub(super) fn admit_object_id(
    store: &FileStore,
    bytes: [u8; 32],
) -> Result<ObjectId, WorkspaceError> {
    store
        .read_object_claim(UntrustedObjectId::from_bytes(bytes))
        .map(|object| object.id())
        .map_err(WorkspaceError::store)
}

/// Engine adapter for the version-owned lazy tree kernel.  Storage admits the
/// exact node bytes and relation identity; the catalog never reconstructs a
/// second canonical node grammar for path lookups or updates.
struct LazyNodeStore<'a> {
    store: &'a FileStore,
}

impl<R: CanonicalRelation> TreeNodeLoader<R> for LazyNodeStore<'_> {
    type Error = backend_store::StoreError;

    fn load(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<backend_version::CheckedCanonicalRoot<R>, Self::Error> {
        self.store.read_relation_node_claim(claim)
    }
}

fn map_lazy_error(error: LazyTreeError<backend_store::StoreError>) -> WorkspaceError {
    match error {
        LazyTreeError::Load(error) => WorkspaceError::store(error),
        LazyTreeError::Node(_) => WorkspaceError::Corrupt("catalog relation node"),
        LazyTreeError::MissingKey => WorkspaceError::Corrupt("catalog relation missing key"),
        LazyTreeError::DuplicateKey => WorkspaceError::Corrupt("catalog relation duplicate key"),
    }
}

/// Fixed descriptor committed by the workspace closure. It binds all three
/// version-layer roots to their physical immutable root objects and bounds
/// the number of visible catalog entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogDescriptor {
    pub(super) count: usize,
    pub(super) primary_root: [u8; 32],
    pub(super) primary_object: [u8; 32],
    pub(super) freshness_root: [u8; 32],
    pub(super) freshness_object: [u8; 32],
    pub(super) payload_ref_root: [u8; 32],
    pub(super) payload_ref_object: [u8; 32],
}

impl CatalogDescriptor {
    pub(super) fn object_id(self) -> Result<ObjectId, WorkspaceError> {
        let bytes = self.encode()?;
        Ok(descriptor_object(&bytes).id())
    }

    pub(super) fn encode(self) -> Result<Vec<u8>, WorkspaceError> {
        if self.count > MAX_CATALOG_ENTRIES {
            return Err(WorkspaceError::Bounds);
        }
        let count = u64::try_from(self.count).map_err(|_| WorkspaceError::Bounds)?;
        let mut bytes = Vec::with_capacity(DESCRIPTOR_BYTES);
        bytes.extend_from_slice(DESCRIPTOR_MAGIC);
        bytes.extend_from_slice(&count.to_be_bytes());
        bytes.extend_from_slice(&self.primary_root);
        bytes.extend_from_slice(&self.primary_object);
        bytes.extend_from_slice(&self.freshness_root);
        bytes.extend_from_slice(&self.freshness_object);
        bytes.extend_from_slice(&self.payload_ref_root);
        bytes.extend_from_slice(&self.payload_ref_object);
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, WorkspaceError> {
        if bytes.len() != DESCRIPTOR_BYTES || !bytes.starts_with(DESCRIPTOR_MAGIC) {
            return Err(WorkspaceError::Corrupt("catalog descriptor"));
        }
        let mut at = DESCRIPTOR_MAGIC.len();
        let count = usize::try_from(read_fixed::<8>(bytes, &mut at).map(u64::from_be_bytes)?)
            .map_err(|_| WorkspaceError::Bounds)?;
        let descriptor = Self {
            count,
            primary_root: read_fixed(bytes, &mut at)?,
            primary_object: read_fixed(bytes, &mut at)?,
            freshness_root: read_fixed(bytes, &mut at)?,
            freshness_object: read_fixed(bytes, &mut at)?,
            payload_ref_root: read_fixed(bytes, &mut at)?,
            payload_ref_object: read_fixed(bytes, &mut at)?,
        };
        if at != bytes.len() || count > MAX_CATALOG_ENTRIES {
            return Err(WorkspaceError::Bounds);
        }
        Ok(descriptor)
    }
}

pub(super) fn descriptor_object(bytes: &[u8]) -> TypedObject {
    let key = backend_version::ObjectKey::<DerivedOutputIndexSchema>::from_value(bytes);
    TypedObject::from_value(&key, bytes)
}

pub(super) fn check_descriptor_object(
    object: &TypedObject,
) -> Result<CatalogDescriptor, WorkspaceError> {
    if object.schema() != super::catalog_schema(INDEX_SCHEMA_TYPE)
        || *object.key()
            != backend_version::ObjectKey::<DerivedOutputIndexSchema>::from_value(object.bytes())
                .to_bytes()
    {
        return Err(WorkspaceError::Corrupt("catalog descriptor identity"));
    }
    CatalogDescriptor::decode(object.bytes())
}

/// Reads and validates a relation root object using its descriptor identity.
pub(crate) fn read_relation_root<R: CanonicalRelation>(
    store: &FileStore,
    object_bytes: [u8; 32],
    root_bytes: [u8; 32],
) -> Result<(StateRoot<R>, TypedObject), WorkspaceError> {
    let object_id = admit_object_id(store, object_bytes)?;
    let object = store
        .read_object(object_id)
        .map_err(WorkspaceError::store)?;
    if object.schema() != SchemaIdentity::of_relation::<R>()
        || object.version() != &root_bytes
        || object.id().as_bytes() != &object_bytes
    {
        return Err(WorkspaceError::Corrupt("catalog relation root object"));
    }
    let claim = UntrustedId::<R>::from_wire(&root_bytes, IdContext::relation::<R>())
        .map_err(|_| WorkspaceError::Corrupt("catalog relation root context"))?;
    let admitted = admit_canonical_root_claim::<R>(claim, object.bytes())
        .map_err(|_| WorkspaceError::Corrupt("catalog relation root admission"))?;
    let root = admitted.root();
    if root.to_bytes() != root_bytes {
        return Err(WorkspaceError::Corrupt("catalog relation root identity"));
    }
    // The store's relation ref index proves every direct descendant is
    // present. This is bounded by one canonical branch and does not walk the
    // unrelated catalog or workspace objects.
    store
        .relation_node_edges(root)
        .map_err(WorkspaceError::store)?;
    Ok((root, object))
}

/// Loads a catalog descriptor through its fixed authenticated object ID in a
/// durable manifest.  The workspace pack carries this ID, so restart does a
/// single manifest path lookup instead of scanning every closure object by
/// schema.
pub(crate) fn load_descriptor_by_id(
    store: &FileStore,
    manifest: &DurableManifest,
    descriptor_id: ObjectId,
    coverage: backend_version::CoverageWitness,
) -> Result<CatalogDescriptor, WorkspaceError> {
    let object = manifest
        .get(descriptor_id)
        .map_err(WorkspaceError::store)?
        .ok_or(WorkspaceError::Corrupt("catalog descriptor missing"))?;
    let descriptor = check_descriptor_object(&object)?;
    read_relation_root::<PrimaryRelation>(
        store,
        descriptor.primary_object,
        descriptor.primary_root,
    )?;
    read_relation_root::<FreshnessRelation>(
        store,
        descriptor.freshness_object,
        descriptor.freshness_root,
    )?;
    read_relation_root::<PayloadRefRelation>(
        store,
        descriptor.payload_ref_object,
        descriptor.payload_ref_root,
    )?;
    let _ = coverage;
    Ok(descriptor)
}

/// Canonical node data used only by the lazy store adapter. Admission is
/// performed by `admit_canonical_root_claim` before this parser is trusted.
/// Exact relation lookup. It reads one canonical node at each selected level.
pub(crate) fn lookup_exact<R: CanonicalRelation>(
    store: &FileStore,
    root_object: [u8; 32],
    root_bytes: [u8; 32],
    key: &R::Key,
) -> Result<Option<R::Value>, WorkspaceError> {
    let claim = UntrustedId::<R>::from_wire(&root_bytes, IdContext::relation::<R>())
        .map_err(|_| WorkspaceError::Corrupt("catalog relation root context"))?;
    let loader = LazyNodeStore { store };
    let tree = LazyTree::open(&loader, claim).map_err(map_lazy_error)?;
    let object = store
        .read_relation_node(tree.root().root())
        .map_err(WorkspaceError::store)?;
    if object.id().as_bytes() != &root_object {
        return Err(WorkspaceError::Corrupt("catalog relation root object"));
    }
    tree.lookup(key).map_err(map_lazy_error)
}

/// Prepares one authenticated insertion using the version layer's lazy
/// path-copy kernel. The returned frontier owns only changed nodes; untouched
/// descendants stay in the store CAS and are never materialized here.
pub(crate) fn prepare_lazy_insert<R: CanonicalRelation>(
    store: &FileStore,
    root_bytes: [u8; 32],
    key: &R::Key,
    value: R::Value,
) -> Result<backend_version::LazyPreparedUpdate<R>, WorkspaceError> {
    let claim = UntrustedId::<R>::from_wire(&root_bytes, IdContext::relation::<R>())
        .map_err(|_| WorkspaceError::Corrupt("catalog relation root context"))?;
    let loader = LazyNodeStore { store };
    let tree = LazyTree::open(&loader, claim).map_err(map_lazy_error)?;
    tree.prepare_insert(key, value).map_err(map_lazy_error)
}

/// Prepares one authenticated value replacement. The key must already exist,
/// which keeps refcount updates distinct from first admission.
pub(crate) fn prepare_lazy_replace<R: CanonicalRelation>(
    store: &FileStore,
    root_bytes: [u8; 32],
    key: &R::Key,
    value: R::Value,
) -> Result<backend_version::LazyPreparedUpdate<R>, WorkspaceError> {
    let claim = UntrustedId::<R>::from_wire(&root_bytes, IdContext::relation::<R>())
        .map_err(|_| WorkspaceError::Corrupt("catalog relation root context"))?;
    let loader = LazyNodeStore { store };
    let tree = LazyTree::open(&loader, claim).map_err(map_lazy_error)?;
    tree.prepare_replace(key, value).map_err(map_lazy_error)
}

/// Prepares one authenticated removal using the version layer's lazy
/// path-copy kernel. Missing keys and malformed nodes are rejected before any
/// new relation object is written.
pub(crate) fn prepare_lazy_remove<R: CanonicalRelation>(
    store: &FileStore,
    root_bytes: [u8; 32],
    key: &R::Key,
) -> Result<backend_version::LazyPreparedUpdate<R>, WorkspaceError> {
    let claim = UntrustedId::<R>::from_wire(&root_bytes, IdContext::relation::<R>())
        .map_err(|_| WorkspaceError::Corrupt("catalog relation root context"))?;
    let loader = LazyNodeStore { store };
    let tree = LazyTree::open(&loader, claim).map_err(map_lazy_error)?;
    tree.prepare_remove(key).map_err(map_lazy_error)
}

/// Persists only the changed frontier emitted by a lazy relation update and
/// charges the operation to the caller's bounded work report.
pub(crate) fn write_lazy_relation_update<R: CanonicalRelation>(
    store: &FileStore,
    update: &backend_version::LazyPreparedUpdate<R>,
    work: &mut super::CatalogWork,
) -> Result<([u8; 32], [u8; 32]), WorkspaceError> {
    let stats = store
        .write_lazy_relation_update(update)
        .map_err(WorkspaceError::store)?;
    work.nodes_written = work
        .nodes_written
        .checked_add(stats.nodes_written)
        .ok_or(WorkspaceError::Bounds)?;
    work.bytes_written = work
        .bytes_written
        .checked_add(stats.bytes_written)
        .ok_or(WorkspaceError::Bounds)?;
    let target = update.target().root().to_bytes();
    Ok((target, stats.root().as_bytes().to_owned()))
}

/// Finds the first row at or above a key. This is used by freshness eviction
/// and bounded prefix selection; callers stop as soon as the prefix changes.
pub(crate) type LowerBoundResult<R> = Result<
    Option<(
        <R as backend_version::Relation>::Key,
        <R as backend_version::Relation>::Value,
    )>,
    WorkspaceError,
>;

pub(crate) fn lookup_lower_bound<R: CanonicalRelation>(
    store: &FileStore,
    root_object: [u8; 32],
    root_bytes: [u8; 32],
    key: &R::Key,
) -> LowerBoundResult<R> {
    let claim = UntrustedId::<R>::from_wire(&root_bytes, IdContext::relation::<R>())
        .map_err(|_| WorkspaceError::Corrupt("catalog relation root context"))?;
    let mut node = store
        .read_relation_node_with_children(claim)
        .map_err(|error| {
            WorkspaceError::Store(format!("catalog lower-bound root admission: {error:?}"))
        })?;
    if node.object().as_bytes() != &root_object {
        return Err(WorkspaceError::Corrupt("catalog relation root object"));
    }
    loop {
        if node.node().node().level() == 0 {
            let entries = node
                .node()
                .leaf_entries()
                .map_err(|_| WorkspaceError::Corrupt("catalog relation leaf"))?;
            let index = entries.partition_point(|(candidate, _)| candidate < key);
            return Ok(entries.get(index).cloned());
        }
        let children = node.children();
        let last_index = children
            .len()
            .checked_sub(1)
            .ok_or(WorkspaceError::Corrupt("catalog relation branch"))?;
        let index = children
            .partition_point(|child| child.summary().first_key <= *key)
            .saturating_sub(1)
            .min(last_index);
        let child = children
            .get(index)
            .ok_or(WorkspaceError::Corrupt("catalog relation branch"))?;
        let child_claim = UntrustedId::<R>::from_wire(
            child.summary().commitment.as_bytes(),
            IdContext::relation::<R>(),
        )
        .map_err(|_| WorkspaceError::Corrupt("catalog relation child context"))?;
        node = store
            .read_relation_node_with_children(child_claim)
            .map_err(|error| {
                WorkspaceError::Store(format!("catalog lower-bound child admission: {error:?}"))
            })?;
    }
}

/// Writes path-copied relation states through the shared store CAS.
pub(super) fn write_relation_state<R: CanonicalRelation>(
    store: &FileStore,
    state: &RelationState<R>,
    work: &mut super::CatalogWork,
) -> Result<[u8; 32], WorkspaceError> {
    let write_stats = store
        .write_relation_state(state)
        .map_err(WorkspaceError::store)?;
    work.nodes_written = work
        .nodes_written
        .checked_add(write_stats.nodes_written)
        .ok_or(WorkspaceError::Bounds)?;
    work.bytes_written = work
        .bytes_written
        .checked_add(write_stats.bytes_written)
        .ok_or(WorkspaceError::Bounds)?;
    Ok(write_stats.root().as_bytes().to_owned())
}

fn read_fixed<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], WorkspaceError> {
    let end = at.checked_add(N).ok_or(WorkspaceError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(WorkspaceError::Corrupt("catalog descriptor field"))?;
    *at = end;
    value
        .try_into()
        .map_err(|_| WorkspaceError::Corrupt("catalog descriptor field"))
}
