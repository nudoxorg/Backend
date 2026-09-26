//! Lazy Merkle page source for product relation closures.

#[cfg(test)]
use backend_engine::RelationState;
use backend_engine::{
    CanonicalRelation, ImmutableObjectSchema, MerkleObject, MerklePage, MerklePageBody,
    MerklePageRequest, MerklePageSource, MerkleRoot, NodeDigest, ObjectKey, ObjectVersion,
    ReplicationError, TreeNodeHandle, WorkspaceRelationHandle, WorkspaceRelationNodeHandle,
    WorkspaceRoot,
};
use std::collections::{BTreeMap, VecDeque};
use std::marker::PhantomData;
use std::mem::size_of;
use std::sync::Arc;

use super::{encode_row, wire_key};

#[path = "page_source/workspace.rs"]
mod workspace;

const CLOSURE_SCHEMA: u32 = 1;

// These are deliberately small fixed windows.  The checked relation remains
// the authoritative backing store; caches only avoid repeating a nearby page
// or demanded object while a bounded protocol window is in flight.
const NODE_CACHE_CAPACITY: usize = 32;
const OBJECT_CACHE_CAPACITY: usize = 8;
const TRANSFER_INDEX_CAPACITY: usize = 64;
pub(super) const NODE_CACHE_BYTE_CAPACITY: usize = 64 * 1024;
pub(super) const OBJECT_CACHE_BYTE_CAPACITY: usize = 4 * 1024 * 1024;
const TRANSFER_INDEX_BYTE_CAPACITY: usize = 4 * 1024 * 1024;

/// One owned authenticated node in a product relation closure.
#[derive(Clone, Debug)]
struct ProductNode {
    level: u16,
    body: MerklePageBody,
}

#[derive(Debug)]
enum RelationBacking<R: CanonicalRelation> {
    /// A workspace-owned handle loads only the requested authenticated node.
    Workspace(WorkspaceRelationHandle<R>),
    /// Test and in-memory callers may provide a checked retained tree.
    Retained(TreeNodeHandle<R>),
}

impl<R: CanonicalRelation> Clone for RelationBacking<R> {
    fn clone(&self) -> Self {
        match self {
            Self::Workspace(handle) => Self::Workspace(handle.clone()),
            Self::Retained(handle) => Self::Retained(handle.clone()),
        }
    }
}

/// A local/remote authenticated source backed by the checked version tree.
///
/// Its constructors accept either a checked [`RelationState`] retained by a
/// compatibility fixture or an owner-admitted [`WorkspaceRelationHandle`].
/// The production path retains one checked root capability and stores row
/// payloads independently in bounded transfer caches. No digest/bytes pair
/// supplied by a client can create a source.
#[derive(Debug)]
pub(crate) struct ProductPageSource<R: CanonicalRelation> {
    workspace: WorkspaceRoot,
    relation_digest: NodeDigest,
    input_version: [u8; 32],
    /// Fixed recipe input retained outside the row LRU. It is referenced by
    /// every closure generation and must remain available after arbitrarily
    /// many authenticated leaf pages have populated the bounded row cache.
    input_bytes: Arc<[u8]>,
    root: MerkleRoot,
    /// The production path retains an O(1) workspace capability and loads
    /// descendants from its shared CAS on demand. The retained variant exists
    /// only for in-memory compatibility tests.
    backing: RelationBacking<R>,
    handles: VecDeque<(NodeDigest, TreeNodeHandle<R>)>,
    /// The selected relation root is a permanent capability.  A wide root
    /// page may issue more children than the bounded node LRU can retain; the
    /// root must still be available to re-validate a later child locator.
    workspace_root_handle: Option<WorkspaceRelationNodeHandle<R>>,
    workspace_handles: VecDeque<(NodeDigest, WorkspaceRelationNodeHandle<R>)>,
    workspace_proofs: VecDeque<(NodeDigest, Arc<[u8]>)>,
    handle_cache_bytes: usize,
    workspace_handle_cache_bytes: usize,
    workspace_proof_bytes: usize,
    nodes: BTreeMap<NodeDigest, ProductNode>,
    objects: VecDeque<([u8; 32], Arc<[u8]>)>,
    object_cache_bytes: usize,
    /// Bounded per-closure locator for rows emitted by leaf pages. This lets
    /// a broad missing set reuse the exact bytes already proved by the page
    /// walk instead of recursively searching the source for every version.
    transfer_objects: VecDeque<([u8; 32], Arc<[u8]>)>,
    transfer_object_bytes: usize,
    marker: PhantomData<fn() -> R>,
}

impl<R: CanonicalRelation> Clone for ProductPageSource<R> {
    fn clone(&self) -> Self {
        Self {
            workspace: self.workspace,
            relation_digest: self.relation_digest,
            input_version: self.input_version,
            input_bytes: Arc::clone(&self.input_bytes),
            root: self.root,
            backing: self.backing.clone(),
            handles: self.handles.clone(),
            workspace_root_handle: self.workspace_root_handle,
            workspace_handles: self.workspace_handles.clone(),
            workspace_proofs: self.workspace_proofs.clone(),
            handle_cache_bytes: self.handle_cache_bytes,
            workspace_handle_cache_bytes: self.workspace_handle_cache_bytes,
            workspace_proof_bytes: self.workspace_proof_bytes,
            nodes: self.nodes.clone(),
            objects: self.objects.clone(),
            object_cache_bytes: self.object_cache_bytes,
            transfer_objects: self.transfer_objects.clone(),
            transfer_object_bytes: self.transfer_object_bytes,
            marker: PhantomData,
        }
    }
}

impl<R: CanonicalRelation> ProductPageSource<R> {
    /// Builds an authenticated page source from a checked relation state.
    ///
    /// # Errors
    ///
    /// Returns a bounded or overflow error when the retained version-tree
    /// closure cannot be represented by the negotiated page limits.
    #[cfg(test)]
    pub(crate) fn from_relation(
        workspace: WorkspaceRoot,
        relation: &RelationState<R>,
    ) -> Result<Self, ReplicationError> {
        let root_handle = relation.root_handle();
        Self::from_descriptor(
            workspace,
            relation.root().to_bytes(),
            root_handle.summary().level,
            root_handle.summary().len,
            root_handle.summary().first_key.as_ref(),
            &RelationBacking::Retained(root_handle),
        )
    }

    /// Builds a product closure source directly from the workspace's checked
    /// lazy relation capability. No relation rows or `RelationState` are
    /// cloned by this constructor; the source owns only the handle and bounded
    /// page/object caches.
    pub(crate) fn from_workspace(
        workspace: WorkspaceRoot,
        relation: WorkspaceRelationHandle<R>,
    ) -> Result<Self, ReplicationError> {
        let root_node = relation
            .root_node()
            .map_err(|_| ReplicationError::CorruptFrame)?;
        let root = root_node.root();
        // The authenticated persisted root carries exact subtree length,
        // level, and first-key metadata. Merely advertising a closure never
        // scans relation leaves.
        let first_key = relation
            .root_first_key()
            .map_err(|_| ReplicationError::CorruptFrame)?;
        let mut source = Self::from_descriptor(
            workspace,
            root.to_bytes(),
            root_node.level(),
            usize::try_from(root_node.row_count()).map_err(|_| ReplicationError::Overflow)?,
            first_key.as_ref(),
            &RelationBacking::Workspace(relation),
        )?;
        source.workspace_root_handle = Some(root_node);
        source.workspace_handle_cache_bytes = size_of::<WorkspaceRelationNodeHandle<R>>();
        Ok(source)
    }

    fn from_descriptor(
        workspace: WorkspaceRoot,
        relation_root: [u8; 32],
        relation_level: u16,
        relation_len: usize,
        first_key_value: Option<&R::Key>,
        backing: &RelationBacking<R>,
    ) -> Result<Self, ReplicationError> {
        let relation_digest = NodeDigest(relation_root);
        let input_bytes = Arc::<[u8]>::from(relation_root);
        let input_version =
            ObjectVersion::<ImmutableObjectSchema>::from_value(input_bytes.as_ref()).to_bytes();
        let manifest =
            backend_engine::product_closure_manifest(workspace, relation_digest.0, input_version);
        let manifest_version = ObjectVersion::<ImmutableObjectSchema>::from_value(&manifest);
        let root = MerkleRoot::from_admitted_manifest(CLOSURE_SCHEMA, manifest_version);
        let first_key = first_key_value.map(wire_key::<R>).unwrap_or_default();
        // Empty source relations are valid workspace states. Their canonical
        // relation leaf is still paged and admitted; the empty first key is
        // a checked boundary marker rather than a fabricated row claim.
        let synthetic_level = relation_level
            .checked_add(1)
            .ok_or(ReplicationError::Overflow)?;
        let mut nodes = BTreeMap::new();
        nodes.insert(
            root.digest(),
            ProductNode {
                level: synthetic_level,
                // The manifest proof authenticates the fixed-size recipe
                // input separately. Keeping only the relation root in this
                // branch preserves the uniform child-level invariant for
                // every canonical tree height; the input CAS receives that
                // manifest-derived claim through its dedicated transfer.
                body: MerklePageBody::Branch(vec![backend_engine::MerkleChild {
                    first_key,
                    end_key: None,
                    digest: relation_digest,
                    level: relation_level,
                    row_count: relation_len as u64,
                }]),
            },
        );
        Ok(Self {
            workspace,
            relation_digest,
            input_version,
            input_bytes,
            root,
            backing: backing.clone(),
            handles: match &backing {
                RelationBacking::Retained(handle) => {
                    [(relation_digest, handle.clone())].into_iter().collect()
                }
                RelationBacking::Workspace(_) => VecDeque::new(),
            },
            workspace_root_handle: None,
            workspace_handles: VecDeque::new(),
            workspace_proofs: VecDeque::new(),
            handle_cache_bytes: match &backing {
                RelationBacking::Retained(_) => size_of::<TreeNodeHandle<R>>(),
                RelationBacking::Workspace(_) => 0,
            },
            workspace_handle_cache_bytes: 0,
            workspace_proof_bytes: 0,
            nodes,
            objects: VecDeque::new(),
            object_cache_bytes: 0,
            transfer_objects: VecDeque::new(),
            transfer_object_bytes: 0,
            marker: PhantomData,
        })
    }

    pub(crate) const fn root(&self) -> MerkleRoot {
        self.root
    }

    fn make_page(
        &self,
        request: MerklePageRequest,
        body: MerklePageBody,
        total: usize,
        level: u16,
        max_items: usize,
    ) -> Result<MerklePage, ReplicationError> {
        let count = match &body {
            MerklePageBody::Branch(items) => items.len(),
            MerklePageBody::Leaf(items) => items.len(),
        };
        let offset =
            usize::try_from(request.cursor.offset).map_err(|_| ReplicationError::Overflow)?;
        if count == 0 && offset < total {
            return Err(ReplicationError::Incomplete);
        }
        let next_offset = offset
            .checked_add(count)
            .ok_or(ReplicationError::Overflow)?;
        let page = MerklePage {
            root: self.root,
            node: request.node,
            level,
            cursor: request.cursor,
            next: (next_offset < total).then_some(backend_engine::PageCursor {
                offset: u32::try_from(next_offset).map_err(|_| ReplicationError::Overflow)?,
            }),
            body,
        };
        page.validate(max_items, 4096)?;
        Ok(page)
    }

    /// Returns the canonical payload for one object claim emitted by the
    /// authenticated source.  Callers must still admit the claim against the
    /// exact request before opening a receiving session.
    pub(crate) fn object_bytes(&mut self, version: [u8; 32]) -> Option<Arc<[u8]>> {
        if version == self.input_version {
            return Some(Arc::clone(&self.input_bytes));
        }
        if let Some((_, bytes)) = self.objects.iter().find(|(key, _)| *key == version) {
            return Some(Arc::clone(bytes));
        }
        if let Some(index) = self
            .transfer_objects
            .iter()
            .position(|(key, _)| *key == version)
        {
            let (key, bytes) = self.transfer_objects.remove(index)?;
            self.transfer_objects.push_back((key, Arc::clone(&bytes)));
            return Some(bytes);
        }
        let bytes = self.find_object(version)?;
        self.remember_transfer(version, Arc::clone(&bytes));
        self.remember_object(version, Arc::clone(&bytes));
        Some(bytes)
    }

    /// Returns the canonical proof for one page node. Synthetic nodes are
    /// authenticated manifests or immutable input objects; relation nodes
    /// expose the exact canonical bytes retained by the version tree.
    pub(crate) fn proof(&mut self, node: NodeDigest) -> Result<Vec<u8>, ReplicationError> {
        if node == self.root.digest() {
            return Ok(backend_engine::product_closure_manifest(
                // The workspace is encoded in the root commitment, but the
                // source retains the typed root only. Reconstructing the
                // manifest from the relation/input identities is impossible
                // without that typed binding, so keep it alongside the root.
                self.workspace,
                self.relation_digest.0,
                self.input_version,
            ));
        }
        // Synthetic closure leaves live above the persisted relation tree.
        // Resolve them before selecting the workspace-backed proof path; an
        // input object is deliberately absent from the relation-node CAS.
        if node.0 == self.input_version {
            return self
                .object_bytes(node.0)
                .map(|bytes| bytes.to_vec())
                .ok_or(ReplicationError::CorruptFrame);
        }
        if matches!(&self.backing, RelationBacking::Workspace(_)) {
            if let Some(proof) = self.workspace_proof_for(node) {
                return Ok(proof.to_vec());
            }
            let handle = self
                .workspace_handle_for(node)
                .ok_or(ReplicationError::CorruptFrame)?;
            let relation = match &self.backing {
                RelationBacking::Workspace(relation) => relation.clone(),
                RelationBacking::Retained(_) => return Err(ReplicationError::CorruptFrame),
            };
            return relation
                .node_proof(handle)
                .map(|proof| proof.to_vec())
                .map_err(|_| ReplicationError::CorruptFrame);
        }
        if let Some(handle) = self.handle_for(node) {
            return Ok(handle.canonical().as_bytes().to_vec());
        }
        self.object_bytes(node.0)
            .map(|bytes| bytes.to_vec())
            .ok_or(ReplicationError::CorruptFrame)
    }

    fn remember_handle(&mut self, digest: NodeDigest, handle: TreeNodeHandle<R>) {
        if let Some(index) = self.handles.iter().position(|(key, _)| *key == digest) {
            self.handles.remove(index);
            self.handle_cache_bytes = self
                .handle_cache_bytes
                .saturating_sub(size_of::<TreeNodeHandle<R>>());
        }
        self.handles.push_back((digest, handle));
        self.handle_cache_bytes = self
            .handle_cache_bytes
            .saturating_add(size_of::<TreeNodeHandle<R>>());
        while self.handles.len() > NODE_CACHE_CAPACITY
            || self.handle_cache_bytes > NODE_CACHE_BYTE_CAPACITY
        {
            if self.handles.pop_front().is_some() {
                self.handle_cache_bytes = self
                    .handle_cache_bytes
                    .saturating_sub(size_of::<TreeNodeHandle<R>>());
            } else {
                break;
            }
        }
    }

    fn handle_for(&mut self, digest: NodeDigest) -> Option<TreeNodeHandle<R>> {
        if let Some(index) = self.handles.iter().position(|(key, _)| *key == digest) {
            let (_, handle) = self.handles.remove(index)?;
            let result = handle.clone();
            self.handles.push_back((digest, handle));
            return Some(result);
        }
        let root = match &self.backing {
            RelationBacking::Retained(root) => root,
            RelationBacking::Workspace(_) => return None,
        };
        let handle = find_handle(root, digest)?;
        self.remember_handle(digest, handle.clone());
        Some(handle)
    }

    fn find_object(&mut self, target: [u8; 32]) -> Option<Arc<[u8]>> {
        if let RelationBacking::Retained(root) = &self.backing {
            return find_object(root, target);
        }
        // Workspace sources intentionally have no unbounded fallback search.
        // Missing objects must have been emitted by an authenticated leaf
        // page and retained in the bounded transfer index above; otherwise a
        // claim cannot turn into a whole-tree scan.
        None
    }

    fn remember_object(&mut self, version: [u8; 32], bytes: Arc<[u8]>) {
        if let Some(index) = self.objects.iter().position(|(key, _)| *key == version)
            && let Some((_, old)) = self.objects.remove(index)
        {
            self.object_cache_bytes = self.object_cache_bytes.saturating_sub(old.len());
        }
        self.objects.push_back((version, bytes));
        if let Some((_, inserted)) = self.objects.back() {
            self.object_cache_bytes = self.object_cache_bytes.saturating_add(inserted.len());
        }
        while self.objects.len() > OBJECT_CACHE_CAPACITY
            || self.object_cache_bytes > OBJECT_CACHE_BYTE_CAPACITY
        {
            if let Some((_, evicted)) = self.objects.pop_front() {
                self.object_cache_bytes = self.object_cache_bytes.saturating_sub(evicted.len());
            } else {
                break;
            }
        }
    }

    fn remember_transfer(&mut self, version: [u8; 32], bytes: Arc<[u8]>) {
        if let Some(index) = self
            .transfer_objects
            .iter()
            .position(|(key, _)| *key == version)
            && let Some((_, old)) = self.transfer_objects.remove(index)
        {
            self.transfer_object_bytes = self.transfer_object_bytes.saturating_sub(old.len());
        }
        let bytes_len = bytes.len();
        self.transfer_objects.push_back((version, bytes));
        self.transfer_object_bytes = self.transfer_object_bytes.saturating_add(bytes_len);
        while self.transfer_objects.len() > TRANSFER_INDEX_CAPACITY
            || self.transfer_object_bytes > TRANSFER_INDEX_BYTE_CAPACITY
        {
            let Some((_, evicted)) = self.transfer_objects.pop_front() else {
                break;
            };
            self.transfer_object_bytes = self.transfer_object_bytes.saturating_sub(evicted.len());
        }
    }

    /// Returns the number of authenticated nodes and immutable objects held
    /// by this source.  This is used by process journey diagnostics.
    #[cfg(test)]
    pub(crate) fn closure_counts(&self) -> (usize, usize) {
        (self.nodes.len(), self.objects.len().saturating_add(1))
    }

    #[cfg(test)]
    pub(crate) fn cache_bytes_for_test(&self) -> (usize, usize) {
        (
            self.handle_cache_bytes
                .saturating_add(self.workspace_handle_cache_bytes)
                .saturating_add(self.workspace_proof_bytes),
            self.object_cache_bytes
                .saturating_add(self.input_bytes.len()),
        )
    }

    #[cfg(test)]
    pub(crate) fn all_object_bytes_for_test(&self) -> Vec<Arc<[u8]>> {
        let mut output = vec![Arc::clone(&self.input_bytes)];
        if let RelationBacking::Retained(root) = &self.backing {
            collect_objects(root, &mut output);
        }
        output
    }
}

impl<R: CanonicalRelation> MerklePageSource for ProductPageSource<R> {
    #[expect(
        clippy::too_many_lines,
        reason = "page admission keeps cached and durable source paths identical"
    )]
    fn page(&mut self, request: MerklePageRequest) -> Result<MerklePage, ReplicationError> {
        if request.root != self.root || request.max_items == 0 {
            return Err(ReplicationError::IdentityMismatch);
        }
        let synthetic = self
            .nodes
            .get(&request.node)
            .map(|node| (node.level, node.body.clone()));
        let offset =
            usize::try_from(request.cursor.offset).map_err(|_| ReplicationError::Overflow)?;
        let max_items = usize::from(request.max_items);
        if synthetic.is_none() && matches!(&self.backing, RelationBacking::Workspace(_)) {
            let (body, exact_total, level, has_more) =
                self.workspace_page(request.node, offset, max_items)?;
            let total = if matches!(body, MerklePageBody::Branch(_)) {
                let count = match &body {
                    MerklePageBody::Branch(items) => items.len(),
                    MerklePageBody::Leaf(_) => 0,
                };
                offset
                    .checked_add(count)
                    .and_then(|value| value.checked_add(usize::from(has_more)))
                    .ok_or(ReplicationError::Overflow)?
            } else {
                exact_total
            };
            return self.make_page(request, body, total, level, max_items);
        }
        let handle = self.handle_for(request.node);
        if synthetic.is_none() && handle.is_none() {
            return Err(ReplicationError::CorruptFrame);
        }
        let (body, total, level) = if let Some((level, body)) = synthetic {
            let (body, total) = match body {
                MerklePageBody::Branch(children) => {
                    if offset > children.len() {
                        return Err(ReplicationError::Range);
                    }
                    let end = offset.saturating_add(max_items).min(children.len());
                    (
                        MerklePageBody::Branch(children[offset..end].to_vec()),
                        children.len(),
                    )
                }
                MerklePageBody::Leaf(entries) => {
                    if offset > entries.len() {
                        return Err(ReplicationError::Range);
                    }
                    let end = offset.saturating_add(max_items).min(entries.len());
                    (
                        MerklePageBody::Leaf(entries[offset..end].to_vec()),
                        entries.len(),
                    )
                }
            };
            (body, total, level)
        } else {
            let handle = handle.ok_or(ReplicationError::CorruptFrame)?;
            let summary = handle.summary();
            let total = if summary.level == 0 {
                summary.len
            } else {
                summary
                    .descendant_counts
                    .get(usize::from(summary.level.saturating_sub(1)))
                    .copied()
                    .ok_or(ReplicationError::CorruptFrame)?
            };
            let (body, total) = if summary.level != 0 {
                if offset > total {
                    return Err(ReplicationError::Range);
                }
                let mut iter = handle.children().skip(offset);
                let mut children_out = Vec::with_capacity(max_items);
                for _ in 0..max_items {
                    let Some(child) = iter.next() else { break };
                    let child_summary = child.summary();
                    children_out.push(backend_engine::MerkleChild {
                        first_key: child_summary
                            .first_key
                            .as_ref()
                            .map(wire_key::<R>)
                            .unwrap_or_default(),
                        end_key: None,
                        digest: NodeDigest(child.id().to_bytes()),
                        level: child_summary.level,
                        row_count: child_summary.len as u64,
                    });
                    self.remember_handle(NodeDigest(child.id().to_bytes()), child.clone());
                }
                let next_first_key = iter
                    .next()
                    .and_then(|next| next.summary().first_key.map(|key| wire_key::<R>(&key)));
                for index in 0..children_out.len().saturating_sub(1) {
                    children_out[index].end_key = Some(children_out[index + 1].first_key.clone());
                }
                if next_first_key.is_some()
                    && let Some(child) = children_out.last_mut()
                {
                    child.end_key = next_first_key;
                }
                let body = MerklePageBody::Branch(children_out);
                (body, total)
            } else {
                let entries = handle.entries().ok_or(ReplicationError::CorruptFrame)?;
                if offset > entries.len() {
                    return Err(ReplicationError::Range);
                }
                let end = offset.saturating_add(max_items).min(entries.len());
                let mut rows = Vec::with_capacity(end.saturating_sub(offset));
                for (key, value) in &entries[offset..end] {
                    let row = encode_row::<R>(key, value);
                    let version = ObjectVersion::<ImmutableObjectSchema>::from_value(&row);
                    let key_id = ObjectKey::<ImmutableObjectSchema>::from_value(&row).to_bytes();
                    let len = row.len() as u64;
                    let row_bytes = Arc::from(row.into_boxed_slice());
                    self.remember_transfer(version.to_bytes(), Arc::clone(&row_bytes));
                    self.remember_object(version.to_bytes(), row_bytes);
                    rows.push(MerkleObject {
                        key: wire_key::<R>(key),
                        key_id,
                        version: version.to_bytes(),
                        len,
                    });
                }
                (MerklePageBody::Leaf(rows), entries.len())
            };
            (body, total, summary.level)
        };
        self.make_page(request, body, total, level, max_items)
    }
}

fn find_handle<R: CanonicalRelation>(
    handle: &TreeNodeHandle<R>,
    target: NodeDigest,
) -> Option<TreeNodeHandle<R>> {
    if NodeDigest(handle.id().to_bytes()) == target {
        return Some(handle.clone());
    }
    if handle.summary().level == 0 {
        return None;
    }
    handle
        .children()
        .find_map(|child| find_handle(&child, target))
}

fn find_object<R: CanonicalRelation>(
    handle: &TreeNodeHandle<R>,
    target: [u8; 32],
) -> Option<Arc<[u8]>> {
    if let Some(entries) = handle.entries() {
        for (key, value) in entries {
            let row = encode_row::<R>(key, value);
            if ObjectVersion::<ImmutableObjectSchema>::from_value(&row).to_bytes() == target {
                return Some(Arc::from(row.into_boxed_slice()));
            }
        }
        return None;
    }
    handle
        .children()
        .find_map(|child| find_object(&child, target))
}

#[cfg(test)]
fn collect_objects<R: CanonicalRelation>(handle: &TreeNodeHandle<R>, output: &mut Vec<Arc<[u8]>>) {
    if let Some(entries) = handle.entries() {
        output.extend(
            entries
                .iter()
                .map(|(key, value)| Arc::from(encode_row::<R>(key, value).into_boxed_slice())),
        );
    } else {
        for child in handle.children() {
            collect_objects(&child, output);
        }
    }
}
