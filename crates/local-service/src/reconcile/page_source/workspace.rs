//! Bounded workspace-node cache for a product page source.
use std::mem::size_of;
use std::sync::Arc;

use backend_engine::{
    CanonicalRelation, ImmutableObjectSchema, MerkleObject, MerklePageBody, NodeDigest, ObjectKey,
    ObjectVersion, ReplicationError, WorkspaceRelationNodeHandle, WorkspaceRelationNodePage,
};

use super::super::{encode_row, wire_key};
use super::{NODE_CACHE_BYTE_CAPACITY, NODE_CACHE_CAPACITY, ProductPageSource, RelationBacking};

impl<R: CanonicalRelation> ProductPageSource<R> {
    fn remember_workspace_handle(
        &mut self,
        digest: NodeDigest,
        handle: WorkspaceRelationNodeHandle<R>,
    ) {
        if let Some(index) = self
            .workspace_handles
            .iter()
            .position(|(key, _)| *key == digest)
        {
            self.workspace_handles.remove(index);
            self.workspace_handle_cache_bytes = self
                .workspace_handle_cache_bytes
                .saturating_sub(size_of::<WorkspaceRelationNodeHandle<R>>());
        }
        self.workspace_handles.push_back((digest, handle));
        self.workspace_handle_cache_bytes = self
            .workspace_handle_cache_bytes
            .saturating_add(size_of::<WorkspaceRelationNodeHandle<R>>());
        while self.workspace_handles.len() > NODE_CACHE_CAPACITY
            || self.workspace_handle_cache_bytes > NODE_CACHE_BYTE_CAPACITY
        {
            if self.workspace_handles.pop_front().is_some() {
                self.workspace_handle_cache_bytes = self
                    .workspace_handle_cache_bytes
                    .saturating_sub(size_of::<WorkspaceRelationNodeHandle<R>>());
            } else {
                break;
            }
        }
    }

    fn remember_workspace_proof(&mut self, digest: NodeDigest, proof: Arc<[u8]>) {
        if let Some(index) = self
            .workspace_proofs
            .iter()
            .position(|(key, _)| *key == digest)
            && let Some((_, old)) = self.workspace_proofs.remove(index)
        {
            self.workspace_proof_bytes = self.workspace_proof_bytes.saturating_sub(old.len());
        }
        let proof_len = proof.len();
        self.workspace_proofs.push_back((digest, proof));
        self.workspace_proof_bytes = self.workspace_proof_bytes.saturating_add(proof_len);
        while self.workspace_proofs.len() > NODE_CACHE_CAPACITY
            || self.workspace_proof_bytes > NODE_CACHE_BYTE_CAPACITY
        {
            let Some((_, old)) = self.workspace_proofs.pop_front() else {
                break;
            };
            self.workspace_proof_bytes = self.workspace_proof_bytes.saturating_sub(old.len());
        }
    }

    pub(super) fn workspace_proof_for(&mut self, digest: NodeDigest) -> Option<Arc<[u8]>> {
        let index = self
            .workspace_proofs
            .iter()
            .position(|(key, _)| *key == digest)?;
        let (key, proof) = self.workspace_proofs.remove(index)?;
        self.workspace_proofs.push_back((key, Arc::clone(&proof)));
        Some(proof)
    }

    /// Finds a node claim by following already admitted branch pages. A node
    /// is never promoted from a same-schema digest alone: its parent must have
    /// committed the child and the store handle rechecks that exact object.
    pub(super) fn workspace_handle_for(
        &mut self,
        digest: NodeDigest,
    ) -> Option<WorkspaceRelationNodeHandle<R>> {
        if digest == self.relation_digest
            && let Some(handle) = self.workspace_root_handle
        {
            return Some(handle);
        }
        if let Some(index) = self
            .workspace_handles
            .iter()
            .position(|(key, _)| *key == digest)
        {
            let (_, handle) = self.workspace_handles.remove(index)?;
            let result = handle;
            self.workspace_handles.push_back((digest, handle));
            return Some(result);
        }
        let RelationBacking::Workspace(relation) = &self.backing else {
            return None;
        };
        let relation = relation.clone();
        let parents = self
            .workspace_handles
            .iter()
            .map(|(_, handle)| *handle)
            .chain(self.workspace_root_handle)
            .collect::<Vec<_>>();
        for parent in parents {
            if parent.level() == 0 {
                continue;
            }
            let mut offset = 0usize;
            loop {
                let page = relation.node_page(parent, offset, 256).ok()?;
                let (children, has_more) = match page {
                    WorkspaceRelationNodePage::Branch {
                        children, has_more, ..
                    } => (children, has_more),
                    WorkspaceRelationNodePage::Leaf { .. } => return None,
                };
                for child in &children {
                    if child.handle().root().to_bytes() == digest.0 {
                        let handle = child.handle();
                        self.remember_workspace_handle(digest, handle);
                        return Some(handle);
                    }
                }
                if !has_more {
                    break;
                }
                offset = offset.checked_add(children.len())?;
            }
        }
        None
    }

    pub(super) fn workspace_page(
        &mut self,
        node: NodeDigest,
        offset: usize,
        limit: usize,
    ) -> Result<(MerklePageBody, usize, u16, bool), ReplicationError> {
        let handle = self
            .workspace_handle_for(node)
            .ok_or(ReplicationError::CorruptFrame)?;
        let relation = match &self.backing {
            RelationBacking::Workspace(relation) => relation.clone(),
            RelationBacking::Retained(_) => return Err(ReplicationError::CorruptFrame),
        };
        let page = relation
            .node_page(handle, offset, limit)
            .map_err(|_| ReplicationError::CorruptFrame)?;
        let proof = Arc::from(page.proof_bytes().to_vec().into_boxed_slice());
        self.remember_workspace_proof(node, proof);
        match page {
            WorkspaceRelationNodePage::Leaf {
                entries, has_more, ..
            } => {
                let mut rows = Vec::with_capacity(entries.len());
                for (key, value) in &entries {
                    let row = encode_row::<R>(key, value);
                    let row_len = row.len();
                    let version = ObjectVersion::<ImmutableObjectSchema>::from_value(&row);
                    let key_id = ObjectKey::<ImmutableObjectSchema>::from_value(&row).to_bytes();
                    let row_bytes = Arc::from(row.into_boxed_slice());
                    self.remember_transfer(version.to_bytes(), Arc::clone(&row_bytes));
                    self.remember_object(version.to_bytes(), row_bytes);
                    rows.push(MerkleObject {
                        key: wire_key::<R>(key),
                        key_id,
                        version: version.to_bytes(),
                        len: u64::try_from(row_len).map_err(|_| ReplicationError::Overflow)?,
                    });
                }
                Ok((
                    MerklePageBody::Leaf(rows),
                    usize::try_from(handle.row_count()).map_err(|_| ReplicationError::Overflow)?,
                    handle.level(),
                    has_more,
                ))
            }
            WorkspaceRelationNodePage::Branch {
                children, has_more, ..
            } => {
                let mut output = Vec::with_capacity(children.len());
                for child in &children {
                    self.remember_workspace_handle(
                        NodeDigest(child.handle().root().to_bytes()),
                        child.handle(),
                    );
                    output.push(backend_engine::MerkleChild {
                        first_key: wire_key::<R>(child.first_key()),
                        end_key: None,
                        digest: NodeDigest(child.handle().root().to_bytes()),
                        level: child.level(),
                        row_count: child.row_count(),
                    });
                }
                for index in 0..output.len().saturating_sub(1) {
                    output[index].end_key = Some(output[index + 1].first_key.clone());
                }
                if has_more {
                    let next_offset = offset
                        .checked_add(output.len())
                        .ok_or(ReplicationError::Overflow)?;
                    let next = relation
                        .node_page(handle, next_offset, 1)
                        .map_err(|_| ReplicationError::CorruptFrame)?;
                    let next_key = match next {
                        WorkspaceRelationNodePage::Branch { children, .. } => children
                            .first()
                            .map(|child| wire_key::<R>(child.first_key())),
                        WorkspaceRelationNodePage::Leaf { .. } => None,
                    };
                    if let Some(next_key) = next_key
                        && let Some(child) = output.last_mut()
                        && child.end_key.is_none()
                    {
                        child.end_key = Some(next_key);
                    }
                }
                Ok((
                    MerklePageBody::Branch(output),
                    usize::try_from(handle.row_count()).map_err(|_| ReplicationError::Overflow)?,
                    handle.level(),
                    has_more,
                ))
            }
        }
    }
}
