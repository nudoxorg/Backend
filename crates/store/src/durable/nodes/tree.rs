//! Lazy map-root hydration and canonical tree publication.

use super::super::{
    FileStore, Hash, LayoutId, PackId, RawRelation, StateRoot, StoreError, io_error,
};
use super::{TreePackDescriptor, TreePublication, TreeReadStats, TreeWriteStats, wire};
use crate::{OrderedMap, StoredValue, digest};
use backend_version::{CheckedCanonicalRoot, IdContext, SchemaIdentity, UntrustedId};
use std::collections::HashSet;
use std::fs;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) struct DecodedNode {
    pub(super) root: StateRoot<RawRelation>,
    pub(super) level: u16,
    pub(super) row_count: u64,
    pub(super) first_key: Option<Vec<u8>>,
    pub(super) entries: Option<Vec<(Vec<u8>, StoredValue)>>,
    pub(super) children: Vec<DecodedChild>,
    pub(super) encoded_bytes: usize,
}

#[derive(Clone, Debug)]
pub(super) struct DecodedChild {
    pub(super) first_key: Vec<u8>,
    pub(super) id: Hash,
    pub(super) row_count: u64,
}

/// A checked durable tree root that hydrates one path at a time.
///
/// Opening this view admits only the compact root descriptor and root node.
/// Point lookups then read one node per tree level; callers that need an
/// in-memory [`OrderedMap`] can explicitly call [`Self::materialize`].
#[derive(Clone, Debug)]
pub struct DurableTree {
    store: FileStore,
    descriptor: TreePackDescriptor,
    root: StateRoot<RawRelation>,
    root_node: Arc<DecodedNode>,
}

impl DurableTree {
    /// Returns the checked state root selected by this durable tree.
    #[must_use]
    pub const fn root(&self) -> StateRoot<RawRelation> {
        self.root
    }

    /// Returns the selected physical layout identity.
    #[must_use]
    pub const fn layout(&self) -> LayoutId {
        self.descriptor.layout
    }

    /// Looks up one key by reading only its root-to-leaf path.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a referenced node is absent,
    /// malformed, or does not match its authenticated branch anchor.
    pub fn get(&self, key: &[u8]) -> Result<Option<StoredValue>, StoreError> {
        self.get_with_stats(key).map(|(value, _)| value)
    }

    /// Looks up one key and reports the bounded path work performed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a referenced node is absent,
    /// malformed, or does not match its authenticated branch anchor.
    pub fn get_with_stats(
        &self,
        key: &[u8],
    ) -> Result<(Option<StoredValue>, TreeReadStats), StoreError> {
        let mut stats = TreeReadStats::default();
        let mut id = self.descriptor.root;
        let mut expected_first_key: Option<Vec<u8>> = None;
        let mut expected_level = None;
        let mut cached = Some(self.root_node.as_ref().clone());
        loop {
            let node = if let Some(root) = cached.take() {
                stats.nodes_read = stats.nodes_read.checked_add(1).ok_or(StoreError::Bounds)?;
                stats.bytes_read = stats
                    .bytes_read
                    .checked_add(root.encoded_bytes)
                    .ok_or(StoreError::Bounds)?;
                root
            } else {
                let node = self.store.read_raw_node(&id)?;
                stats.nodes_read = stats.nodes_read.checked_add(1).ok_or(StoreError::Bounds)?;
                stats.bytes_read = stats
                    .bytes_read
                    .checked_add(node.encoded_bytes)
                    .ok_or(StoreError::Bounds)?;
                node
            };
            if expected_level.is_some_and(|level| node.level != level)
                || expected_first_key
                    .as_deref()
                    .is_some_and(|first| node.first_key.as_deref() != Some(first))
            {
                return Err(StoreError::Corrupt);
            }
            if let Some(entries) = node.entries {
                return Ok((
                    entries
                        .binary_search_by(|(candidate, _)| candidate.as_slice().cmp(key))
                        .ok()
                        .map(|index| entries[index].1.clone()),
                    stats,
                ));
            }
            let index = node
                .children
                .partition_point(|child| child.first_key.as_slice() <= key)
                .checked_sub(1)
                .ok_or(StoreError::Corrupt)?;
            let child = node.children.get(index).ok_or(StoreError::Corrupt)?;
            expected_first_key = Some(child.first_key.clone());
            expected_level = Some(node.level.checked_sub(1).ok_or(StoreError::Corrupt)?);
            id = child.id;
        }
    }

    /// Materializes this checked root into the existing in-memory map facade.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when any transitive node is missing or
    /// fails canonical admission.
    pub fn materialize(&self) -> Result<OrderedMap, StoreError> {
        self.store
            .read_tree_publication(self.descriptor.id)?
            .map(|(map, _)| map)
            .ok_or(StoreError::Corrupt)
    }
}

impl FileStore {
    pub(super) fn read_raw_node(&self, version: &Hash) -> Result<DecodedNode, StoreError> {
        let claim =
            UntrustedId::<RawRelation>::from_wire(version, IdContext::relation::<RawRelation>())
                .map_err(|_| StoreError::Corrupt)?;
        let admitted = self.read_relation_node_claim(claim)?;
        decode_canonical_node(&admitted)
    }

    pub(in crate::durable) fn open_tree_publication(
        &self,
        id: PackId,
    ) -> Result<Option<DurableTree>, StoreError> {
        let path = self.tree_pack_path(id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(&error)),
        };
        let descriptor = wire::decode_tree_descriptor(&bytes, id)?;
        let root_node = self.read_raw_node(&descriptor.root)?;
        let root = root_node.root;
        if root.as_bytes() != &descriptor.target {
            return Err(StoreError::Corrupt);
        }
        Ok(Some(DurableTree {
            store: self.clone(),
            descriptor,
            root,
            root_node: Arc::new(root_node),
        }))
    }

    pub(in crate::durable) fn write_tree_publication(
        &self,
        publication: &TreePublication,
    ) -> Result<TreeWriteStats, StoreError> {
        if publication.root.id().to_bytes() != *publication.target.as_bytes() {
            return Err(StoreError::Corrupt);
        }
        fs::create_dir_all(self.root.join("nodes")).map_err(|error| io_error(&error))?;
        let mut stats = TreeWriteStats::default();
        let schema = SchemaIdentity::of_relation::<RawRelation>();
        let mut seen = HashSet::new();
        if let Some(frontier) = publication.frontier.as_deref() {
            for node in frontier {
                self.write_relation_node_handle(node, schema, &mut stats, &mut seen, false)?;
            }
        } else {
            self.write_relation_node_handle(
                &publication.root,
                schema,
                &mut stats,
                &mut seen,
                true,
            )?;
        }
        let descriptor = TreePackDescriptor {
            id: publication.id,
            layout: publication.layout,
            target: *publication.target.as_bytes(),
            root: publication.root.id().to_bytes(),
        };
        let bytes = wire::encode_tree_descriptor(descriptor);
        let path = self.tree_pack_path(publication.id);
        wire::write_immutable_descriptor(&path, &bytes, &self.root.join("packs"))?;
        if let Ok(mut last) = self.tree_write_stats.lock() {
            *last = stats;
        }
        Ok(stats)
    }

    pub(in crate::durable) fn read_tree_publication(
        &self,
        id: PackId,
    ) -> Result<Option<(OrderedMap, TreeWriteStats)>, StoreError> {
        let path = self.tree_pack_path(id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(&error)),
        };
        let descriptor = wire::decode_tree_descriptor(&bytes, id)?;
        let target = descriptor.target;
        if descriptor.root != target {
            return Err(StoreError::Corrupt);
        }
        let mut reader = TreeReader {
            store: self,
            visited: HashSet::new(),
            rows: Vec::new(),
            node_count: 0,
        };
        reader.read_node(descriptor.root, None, None)?;
        let map = OrderedMap::try_from_iter(reader.rows)?;
        if map.state_root().as_bytes() != &target {
            return Err(StoreError::Corrupt);
        }
        Ok(Some((
            map,
            TreeWriteStats {
                nodes_written: reader.node_count,
                bytes_written: 0,
                nodes_visited: reader.node_count,
            },
        )))
    }

    pub(in crate::durable) fn tree_pack_path(&self, id: PackId) -> std::path::PathBuf {
        self.root
            .join("packs")
            .join(format!("{}.tree", wire::hex(id.as_bytes())))
    }
}

fn decode_canonical_node(
    admitted: &CheckedCanonicalRoot<RawRelation>,
) -> Result<DecodedNode, StoreError> {
    let (entries, children) = if admitted.node().level() == 0 {
        let entries = admitted.leaf_entries().map_err(|_| StoreError::Corrupt)?;
        (Some(entries), Vec::new())
    } else {
        let children = admitted
            .child_summaries()
            .map_err(|_| StoreError::Corrupt)?
            .into_iter()
            .map(|child| {
                Ok(DecodedChild {
                    first_key: child.first_key,
                    id: child.commitment.to_bytes(),
                    row_count: child.row_count,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        (None, children)
    };
    let first_key = entries
        .as_ref()
        .and_then(|entries| entries.first().map(|(key, _)| key.clone()))
        .or_else(|| children.first().map(|child| child.first_key.clone()));
    Ok(DecodedNode {
        root: admitted.root(),
        level: admitted.node().level(),
        row_count: admitted.node().row_count(),
        first_key,
        entries,
        children,
        encoded_bytes: admitted.bytes().len(),
    })
}

struct TreeReader<'a> {
    store: &'a FileStore,
    visited: HashSet<Hash>,
    rows: Vec<(Vec<u8>, StoredValue)>,
    node_count: usize,
}

impl TreeReader<'_> {
    fn read_node(
        &mut self,
        id: Hash,
        expected_first_key: Option<&[u8]>,
        expected_level: Option<u16>,
    ) -> Result<(), StoreError> {
        if !self.visited.insert(id) {
            return Err(StoreError::Corrupt);
        }
        self.node_count = self.node_count.checked_add(1).ok_or(StoreError::Bounds)?;
        if self.node_count > super::NODE_MAX_COUNT {
            return Err(StoreError::Bounds);
        }
        let node = self.store.read_raw_node(&id)?;
        if expected_level.is_some_and(|level| node.level != level)
            || expected_first_key.is_some_and(|key| node.first_key.as_deref() != Some(key))
        {
            return Err(StoreError::Corrupt);
        }
        if let Some(entries) = node.entries {
            if u64::try_from(entries.len()).map_err(|_| StoreError::Bounds)? != node.row_count {
                return Err(StoreError::Corrupt);
            }
            self.rows.extend(entries);
            return Ok(());
        }
        let child_rows = node.children.iter().try_fold(0u64, |total, child| {
            total.checked_add(child.row_count).ok_or(StoreError::Bounds)
        })?;
        if child_rows != node.row_count {
            return Err(StoreError::Corrupt);
        }
        let child_level = node.level.checked_sub(1).ok_or(StoreError::Corrupt)?;
        let mut previous: Option<&[u8]> = None;
        for child in &node.children {
            if previous.is_some_and(|key| key >= child.first_key.as_slice()) {
                return Err(StoreError::Corrupt);
            }
            previous = Some(&child.first_key);
            self.read_node(child.id, Some(&child.first_key), Some(child_level))?;
        }
        Ok(())
    }
}

pub(super) fn tree_pack_id(layout: LayoutId, target: StateRoot<RawRelation>, root: Hash) -> PackId {
    let mut bytes = Vec::with_capacity(96);
    bytes.extend_from_slice(layout.as_bytes());
    bytes.extend_from_slice(target.as_bytes());
    bytes.extend_from_slice(&root);
    PackId::from_wire(digest(b"store.tree-pack.v1\0", &bytes))
}
