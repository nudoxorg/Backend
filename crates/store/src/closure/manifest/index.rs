//! Persistent lookup and export state for a closure manifest.

use super::{ManifestChange, ManifestTree, ObjectId, SchemaIdentity, TypedObject};
use crate::Hash;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

type ManifestLookupKey = (SchemaIdentity, Hash);

const LOOKUP_BITS: u32 = 4;
const LOOKUP_DEPTH: usize = 16;
const LOOKUP_FANOUT: usize = 1 << LOOKUP_BITS;

#[derive(Clone, Debug)]
struct LookupItem {
    key: ManifestLookupKey,
    objects: Arc<[Arc<TypedObject>]>,
}

#[derive(Debug)]
enum LookupNode {
    Branch(Box<[Option<Arc<LookupNode>>]>),
    Leaf(Arc<[LookupItem]>),
}

/// A persistent radix lookup layered alongside the canonical membership tree.
/// Each update path-copies a fixed sixteen-level hash trie, so generations do
/// not form an unbounded overlay chain and reference admission has bounded
/// lookup work regardless of edit history length.
#[derive(Debug)]
pub(super) struct ManifestLookup {
    root: Option<Arc<LookupNode>>,
}

impl ManifestLookup {
    pub(super) fn root(records: &BTreeMap<ObjectId, Arc<TypedObject>>) -> Arc<Self> {
        let mut root = None;
        for object in records.values() {
            root = update_root(
                root.as_deref(),
                (object.schema(), *object.version()),
                object.id(),
                Some(object.clone()),
            );
        }
        Arc::new(Self { root })
    }

    pub(super) fn delta(base: &Self, changes: &[ManifestChange]) -> Arc<Self> {
        let mut root = base.root.clone();
        for change in changes {
            let key = change
                .after
                .as_ref()
                .map(|object| (object.schema(), *object.version()))
                .or_else(|| {
                    change.before.as_ref().map(|entry| {
                        (
                            SchemaIdentity::new(
                                entry.schema_domain,
                                entry.schema_type,
                                entry.schema_version,
                            ),
                            entry.object_version,
                        )
                    })
                });
            if let Some(key) = key {
                root = update_root(
                    root.as_deref(),
                    key,
                    change.key,
                    change.after.as_ref().map(|object| Arc::new(object.clone())),
                );
            }
        }
        Arc::new(Self { root })
    }

    pub(super) fn find(&self, key: ManifestLookupKey) -> Option<Arc<TypedObject>> {
        lookup_bucket(self.root.as_deref(), key).and_then(|objects| objects.first().cloned())
    }

    pub(super) fn find_entry(
        &self,
        key: ManifestLookupKey,
        object_key: Hash,
        id: ObjectId,
    ) -> Option<Arc<TypedObject>> {
        lookup_bucket(self.root.as_deref(), key).and_then(|objects| {
            objects
                .iter()
                .find(|object| object.id() == id && object.key() == &object_key)
                .cloned()
        })
    }

    pub(super) fn all_objects(&self) -> Vec<Arc<TypedObject>> {
        let mut objects = Vec::new();
        collect_lookup_objects(self.root.as_deref(), &mut objects);
        objects
    }
}

fn lookup_hash(key: ManifestLookupKey) -> u64 {
    let schema = (u64::from(key.0.domain()) << 24)
        | (u64::from(key.0.ty()) << 8)
        | u64::from(key.0.version());
    let mut leading = [0; 8];
    leading.copy_from_slice(&key.1[..8]);
    let version = u64::from_be_bytes(leading);
    version ^ schema.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

fn lookup_slot(hash: u64, depth: usize) -> usize {
    let shift = 64 - usize::try_from(LOOKUP_BITS).unwrap_or_default() * (depth + 1);
    let mask = u64::try_from(LOOKUP_FANOUT - 1).unwrap_or_default();
    usize::try_from((hash >> shift) & mask).unwrap_or_default()
}

fn update_root(
    root: Option<&LookupNode>,
    key: ManifestLookupKey,
    id: ObjectId,
    object: Option<Arc<TypedObject>>,
) -> Option<Arc<LookupNode>> {
    update_node(root, 0, lookup_hash(key), key, id, object)
}

fn update_node(
    node: Option<&LookupNode>,
    depth: usize,
    hash: u64,
    key: ManifestLookupKey,
    id: ObjectId,
    object: Option<Arc<TypedObject>>,
) -> Option<Arc<LookupNode>> {
    if depth == LOOKUP_DEPTH {
        let mut entries = match node {
            Some(LookupNode::Leaf(entries)) => entries.to_vec(),
            Some(LookupNode::Branch(_)) => return None,
            None => Vec::new(),
        };
        let entry = entries.binary_search_by(|entry| entry.key.cmp(&key));
        match (entry, object) {
            (Ok(index), Some(object)) => {
                let mut objects = entries[index].objects.to_vec();
                match objects.binary_search_by_key(&id, |candidate| candidate.id()) {
                    Ok(object_index) => objects[object_index] = object,
                    Err(object_index) => objects.insert(object_index, object),
                }
                entries[index].objects = Arc::from(objects.into_boxed_slice());
            }
            (Ok(index), None) => {
                let mut objects = entries[index].objects.to_vec();
                if let Ok(object_index) =
                    objects.binary_search_by_key(&id, |candidate| candidate.id())
                {
                    objects.remove(object_index);
                }
                if objects.is_empty() {
                    entries.remove(index);
                } else {
                    entries[index].objects = Arc::from(objects.into_boxed_slice());
                }
            }
            (Err(index), Some(object)) => entries.insert(
                index,
                LookupItem {
                    key,
                    objects: Arc::from(vec![object].into_boxed_slice()),
                },
            ),
            (Err(_), None) => {}
        }
        return (!entries.is_empty())
            .then(|| Arc::new(LookupNode::Leaf(Arc::from(entries.into_boxed_slice()))));
    }
    let mut children = match node {
        Some(LookupNode::Branch(children)) => children.to_vec(),
        Some(LookupNode::Leaf(_)) => return None,
        None => vec![None; LOOKUP_FANOUT],
    };
    let slot = lookup_slot(hash, depth);
    children[slot] = update_node(children[slot].as_deref(), depth + 1, hash, key, id, object);
    if children.iter().all(Option::is_none) {
        None
    } else {
        Some(Arc::new(LookupNode::Branch(children.into_boxed_slice())))
    }
}

fn lookup_bucket(root: Option<&LookupNode>, key: ManifestLookupKey) -> Option<&[Arc<TypedObject>]> {
    let hash = lookup_hash(key);
    let mut node = root?;
    for depth in 0..LOOKUP_DEPTH {
        node = match node {
            LookupNode::Branch(children) => children.get(lookup_slot(hash, depth))?.as_deref()?,
            LookupNode::Leaf(entries) => {
                return entries
                    .binary_search_by(|entry| entry.key.cmp(&key))
                    .ok()
                    .map(|index| entries[index].objects.as_ref());
            }
        };
    }
    match node {
        LookupNode::Leaf(entries) => entries
            .binary_search_by(|entry| entry.key.cmp(&key))
            .ok()
            .map(|index| entries[index].objects.as_ref()),
        LookupNode::Branch(_) => None,
    }
}

fn collect_lookup_objects(node: Option<&LookupNode>, output: &mut Vec<Arc<TypedObject>>) {
    let Some(node) = node else {
        return;
    };
    match node {
        LookupNode::Branch(children) => {
            for child in children.iter().flatten() {
                collect_lookup_objects(Some(child), output);
            }
        }
        LookupNode::Leaf(entries) => {
            for entry in entries.iter() {
                output.extend(entry.objects.iter().cloned());
            }
        }
    }
}

pub(super) struct ManifestState {
    pub(super) tree: ManifestTree,
    pub(super) lookup: Arc<ManifestLookup>,
    pub(super) changed: Arc<[Arc<TypedObject>]>,
    export: OnceLock<Arc<[TypedObject]>>,
}

impl std::fmt::Debug for ManifestState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManifestState")
            .field("root", &self.tree.root_id())
            .finish_non_exhaustive()
    }
}

impl ManifestState {
    pub(super) fn new(
        tree: ManifestTree,
        lookup: Arc<ManifestLookup>,
        changed: Arc<[Arc<TypedObject>]>,
    ) -> Self {
        Self {
            tree,
            lookup,
            changed,
            export: OnceLock::new(),
        }
    }

    pub(super) fn objects(&self) -> &[TypedObject] {
        self.export
            .get_or_init(|| {
                let mut objects = self
                    .lookup
                    .all_objects()
                    .into_iter()
                    .map(|object| (*object).clone())
                    .collect::<Vec<_>>();
                objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
                Arc::from(objects.into_boxed_slice())
            })
            .as_ref()
    }
}
