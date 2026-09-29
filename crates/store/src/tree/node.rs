//! The store adapter for the shared persistent canonical tree.

use crate::{Arc, Hash, HashMap, Mutex, RawRelation, StoreError};
use std::collections::VecDeque;

/// One checked canonical node retained by an [`OrderedMap`](super::OrderedMap).
pub(crate) type Node = backend_version::TreeNodeHandle<RawRelation>;

/// Weak content interner shared by immutable map generations.
pub(crate) type Cas = Arc<StoreCas>;

const SHARD_COUNT: usize = 64;
const SHARD_MASK: usize = SHARD_COUNT - 1;
const REAP_BUDGET: usize = 8;
const MIN_QUEUE_CAPACITY_BEFORE_SHRINK: usize = 256;

/// Sharded weak content interner for immutable map generations.
///
/// Each node commitment selects one shard, so unrelated intern operations do
/// not contend on a single mutex. The per-shard FIFO tracks exactly one key
/// per weak CAS entry. Every intern examines at most `REAP_BUDGET` keys from
/// that FIFO, removing expired weak handles and cycling live handles to its
/// tail. This makes dead-entry cleanup O(1) amortized per intern while keeping
/// the lookup path expected O(1); the FIFO adds linear key storage alongside
/// the CAS itself. Shards that receive no further interns are left untouched
/// until their next use, so cleanup never requires a background thread.
pub(crate) struct StoreCas {
    shards: [Mutex<Shard>; SHARD_COUNT],
}

#[derive(Default)]
struct Shard {
    entries: HashMap<Hash, backend_version::WeakTreeNodeHandle<RawRelation>>,
    reap_queue: VecDeque<Hash>,
    #[cfg(test)]
    reap_visits: usize,
}

impl StoreCas {
    fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| Mutex::new(Shard::default())),
        }
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        let mut total = 0;
        let mut live = 0;
        for shard in &self.shards {
            let Ok(shard) = shard.lock() else {
                continue;
            };
            total += shard.entries.len();
            live += shard
                .entries
                .values()
                .filter(|entry| entry.upgrade().is_some())
                .count();
        }
        (total, live)
    }

    /// Removes every expired weak node handle from the interner.
    ///
    /// This explicit maintenance pass is O(number of indexed nodes). Regular
    /// interning uses bounded incremental cleanup instead.
    pub(crate) fn reap_stale(&self) -> usize {
        let mut removed = 0;
        for shard in &self.shards {
            let Ok(mut shard) = shard.lock() else {
                continue;
            };
            let before = shard.entries.len();
            shard.entries.retain(|_, weak| weak.upgrade().is_some());
            removed += before - shard.entries.len();
            let Shard {
                entries,
                reap_queue,
                ..
            } = &mut *shard;
            reap_queue.retain(|hash| entries.contains_key(hash));
            shrink_reap_queue_if_sparse(&mut shard);
        }
        removed
    }

    #[cfg(test)]
    pub(crate) fn reap_visits(&self) -> usize {
        self.shards
            .iter()
            .filter_map(|shard| shard.lock().ok())
            .map(|shard| shard.reap_visits)
            .sum()
    }
}

pub(crate) fn new_cas() -> Cas {
    Arc::new(StoreCas::new())
}

/// Interner adapter that keeps only weak handles so dropped map generations can
/// be reclaimed while live roots continue to deduplicate structurally equal
/// nodes.
#[derive(Clone)]
pub(crate) struct StoreInterner {
    pub(crate) cas: Cas,
}

impl backend_version::TreeInterner<RawRelation> for StoreInterner {
    fn intern(&self, node: Node) -> Node {
        let hash = node.id().to_bytes();
        let shard_index = usize::from(hash[0]) & SHARD_MASK;
        let Ok(mut shard) = self.cas.shards[shard_index].lock() else {
            return node;
        };

        reap(&mut shard);
        if let Some(existing) = shard
            .entries
            .get(&hash)
            .and_then(backend_version::WeakTreeNodeHandle::upgrade)
        {
            return existing;
        }
        if shard.entries.insert(hash, node.downgrade()).is_none() {
            shard.reap_queue.push_back(hash);
        }
        node
    }
}

fn reap(shard: &mut Shard) {
    for _ in 0..REAP_BUDGET {
        let Some(hash) = shard.reap_queue.pop_front() else {
            break;
        };
        #[cfg(test)]
        {
            shard.reap_visits += 1;
        }
        if shard
            .entries
            .get(&hash)
            .is_some_and(|weak| weak.upgrade().is_some())
        {
            shard.reap_queue.push_back(hash);
        } else {
            shard.entries.remove(&hash);
        }
    }
    shrink_reap_queue_if_sparse(shard);
}

fn shrink_reap_queue_if_sparse(shard: &mut Shard) {
    let capacity = shard.reap_queue.capacity();
    if capacity >= MIN_QUEUE_CAPACITY_BEFORE_SHRINK
        && capacity > shard.reap_queue.len().saturating_mul(2)
    {
        shard.reap_queue.shrink_to_fit();
    }
}

pub(crate) fn map_node_error(error: backend_version::NodeError) -> StoreError {
    match error {
        backend_version::NodeError::OversizedNode => StoreError::Bounds,
        backend_version::NodeError::UnsortedOrDuplicate => StoreError::MalformedDelta,
        backend_version::NodeError::InvalidBranch
        | backend_version::NodeError::LevelMismatch
        | backend_version::NodeError::AnchorMismatch
        | backend_version::NodeError::MalformedEncoding
        | backend_version::NodeError::SchemaMismatch
        | backend_version::NodeError::NonCanonicalEncoding
        | backend_version::NodeError::RelationDecode(_) => StoreError::Corrupt,
    }
}

pub(crate) fn map_tree_error(error: backend_version::TreeError) -> StoreError {
    match error {
        backend_version::TreeError::UnsortedOrDuplicate => StoreError::MalformedDelta,
        backend_version::TreeError::InvalidRoot => StoreError::Corrupt,
        backend_version::TreeError::Overflow => StoreError::Bounds,
        backend_version::TreeError::Canonical(error) => map_node_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoredValue;
    use backend_version::TreeInterner as _;

    const ACCOUNTED_NODE_COUNT: usize = 100_000;

    fn checked_leaf(key: u64) -> Node {
        let items = [(
            key.to_le_bytes().to_vec(),
            StoredValue::new(key.to_le_bytes().to_vec(), 1, Vec::new()),
        )];
        backend_version::PersistentTree::<RawRelation>::from_sorted_items(&items)
            .expect("one sorted row is a valid canonical tree")
            .root_handle()
    }

    fn shard_index(node: &Node) -> usize {
        usize::from(node.id().to_bytes()[0]) & SHARD_MASK
    }

    #[test]
    fn interning_100k_nodes_has_constant_maintenance_work_per_node() {
        let cas = new_cas();
        let interner = StoreInterner { cas: cas.clone() };
        let mut retained = Vec::with_capacity(ACCOUNTED_NODE_COUNT);

        for key in 0..u64::try_from(ACCOUNTED_NODE_COUNT).expect("count fits u64") {
            retained.push(interner.intern(checked_leaf(key)));
        }

        let (total, live) = cas.counts();
        assert_eq!(total, ACCOUNTED_NODE_COUNT);
        assert_eq!(live, ACCOUNTED_NODE_COUNT);
        assert!(cas.reap_visits() <= REAP_BUDGET * ACCOUNTED_NODE_COUNT);
    }

    #[test]
    fn dropped_history_is_reclaimed_incrementally() {
        const HISTORY_NODES: usize = 256;

        let cas = new_cas();
        let interner = StoreInterner { cas: cas.clone() };
        let mut history = (0..HISTORY_NODES as u64)
            .map(|key| interner.intern(checked_leaf(key)))
            .collect::<Vec<_>>();
        assert_eq!(cas.counts(), (HISTORY_NODES, HISTORY_NODES));

        history.clear();
        assert_eq!(cas.counts(), (HISTORY_NODES, 0));

        let pending_per_shard = cas
            .shards
            .iter()
            .map(|shard| {
                shard
                    .lock()
                    .expect("CAS shard is not poisoned")
                    .reap_queue
                    .len()
            })
            .collect::<Vec<_>>();
        let mut retained = Vec::new();
        let mut next_key = HISTORY_NODES as u64 + 1;

        for (target_shard, pending) in pending_per_shard.into_iter().enumerate() {
            let needed = pending.div_ceil(REAP_BUDGET);
            let mut interned = 0;
            while interned < needed {
                let candidate = checked_leaf(next_key);
                next_key += 1;
                if shard_index(&candidate) != target_shard {
                    continue;
                }
                retained.push(interner.intern(candidate));
                interned += 1;
            }
        }

        let (total, live) = cas.counts();
        assert_eq!(total, live, "all expired history entries should be reaped");
        assert_eq!(live, retained.len());
    }

    #[test]
    fn reusing_an_expired_commitment_keeps_one_reap_queue_slot() {
        let cas = new_cas();
        let interner = StoreInterner { cas: cas.clone() };
        let target_shard = 17;
        let mut candidates = Vec::with_capacity(REAP_BUDGET + 1);
        let mut key = 0;

        while candidates.len() < REAP_BUDGET + 1 {
            let candidate = checked_leaf(key);
            let candidate_key = key;
            key += 1;
            if shard_index(&candidate) == target_shard {
                candidates.push((candidate_key, candidate));
            }
        }

        let target_key = candidates[REAP_BUDGET].0;
        let history = candidates
            .into_iter()
            .map(|(_, candidate)| interner.intern(candidate))
            .collect::<Vec<_>>();
        drop(history);

        let _ = interner.intern(checked_leaf(target_key));
        let target = cas.shards[target_shard]
            .lock()
            .expect("CAS shard is not poisoned");
        assert_eq!(target.entries.len(), 1);
        assert_eq!(target.reap_queue.len(), 1);
    }

    #[test]
    fn concurrent_interns_deduplicate_one_live_node() {
        let cas = new_cas();
        let interner = StoreInterner { cas };
        let candidates = (0..32).map(|_| checked_leaf(7)).collect::<Vec<_>>();
        let threads = candidates
            .into_iter()
            .map(|candidate| {
                let interner = interner.clone();
                std::thread::spawn(move || interner.intern(candidate))
            })
            .collect::<Vec<_>>();
        let handles = threads
            .into_iter()
            .map(|thread| thread.join().expect("intern thread should complete"))
            .collect::<Vec<_>>();

        let first = handles[0].canonical();
        assert!(
            handles
                .iter()
                .all(|handle| std::ptr::eq(first, handle.canonical()))
        );
    }
}
