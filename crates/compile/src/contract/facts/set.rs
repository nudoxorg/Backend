//! Sharded fact-set storage over the versioned relation kernel.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, OnceLock},
};

use backend_version::{
    ClosedRelationScope, CoverageWitness, ObjectKey, RelationState, Schema, ScopeRoot, StateError,
    StateRoot, prepare_delta_with_state,
};

use super::{FactIndex, FactKind, FactRecord, FactRelation, FactRootRelation};

const FACT_SHARD_COUNT: usize = 256;

/// Physical fact indexes are complete closed-world materialization state,
/// rather than producer authority coverage. The closed witness enables local
/// path-copy deltas while remaining ineligible for workspace publication.
fn fact_storage_closed() -> CoverageWitness {
    CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(ScopeRoot::from_bytes(
        [0; 32],
    )))
}

/// Internal failure while constructing or updating an indexed fact set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FactSetError {
    Duplicate,
    Canonical,
}

/// Persistent normalized fact storage backed by the shared version kernel.
///
/// The relation state owns an immutable path-copy tree for the 256 fixed
/// byte-prefix shards, and a second small relation commits their roots. This
/// bounded sharding is required because a fact row includes full key, value,
/// and evidence commitments while the shared kernel caps one canonical node
/// at 64 KiB. Cloning a snapshot clones only the `Arc` around this set, and a
/// prepared delta rebuilds only touched shards plus their root relation. The
/// compatibility slice is materialized lazily and remains in canonical
/// kind/key order.
pub(crate) struct FactSet<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> {
    pub(in crate::contract) state: RelationState<FactRootRelation<K, V>>,
    pub(in crate::contract) shards: Arc<[RelationState<FactRelation<K, V>>]>,
    pub(in crate::contract) len: usize,
    ordered: OnceLock<Arc<[FactRecord<K, V>]>>,
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> FactSet<K, V> {
    pub(in crate::contract) fn from_records(
        records: Vec<FactRecord<K, V>>,
    ) -> Result<Self, FactSetError> {
        let len = records.len();
        let mut buckets = (0..FACT_SHARD_COUNT)
            .map(|_| Vec::new())
            .collect::<Vec<Vec<(FactIndex<K>, FactRecord<K, V>)>>>();
        for record in records {
            let key = Self::key(&record);
            let Some(bucket) = buckets.get_mut(Self::shard(key.key)) else {
                return Err(FactSetError::Canonical);
            };
            bucket.push((key, record));
        }
        let mut roots = Vec::with_capacity(FACT_SHARD_COUNT);
        let mut shards = Vec::with_capacity(FACT_SHARD_COUNT);
        for entries in buckets {
            let shard =
                RelationState::<FactRelation<K, V>>::from_entries(entries, fact_storage_closed())
                    .map_err(|error| match error {
                    StateError::DuplicateKey => FactSetError::Duplicate,
                    StateError::Canonical(_) => FactSetError::Canonical,
                })?;
            roots.push(shard.root());
            shards.push(shard);
        }
        if roots.len() != FACT_SHARD_COUNT {
            return Err(FactSetError::Canonical);
        }
        let root_entries = roots
            .into_iter()
            .enumerate()
            .map(|(index, root)| {
                u16::try_from(index)
                    .map(|key| (key, root))
                    .map_err(|_| FactSetError::Canonical)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let state = RelationState::<FactRootRelation<K, V>>::from_entries(
            root_entries,
            fact_storage_closed(),
        )
        .map_err(|error| match error {
            StateError::DuplicateKey => FactSetError::Duplicate,
            StateError::Canonical(_) => FactSetError::Canonical,
        })?;
        Ok(Self {
            state,
            shards: Arc::from(shards.into_boxed_slice()),
            len,
            ordered: OnceLock::new(),
        })
    }

    pub(in crate::contract) fn empty() -> Result<Self, FactSetError> {
        Self::from_records(Vec::new())
    }

    pub(in crate::contract) fn key(record: &FactRecord<K, V>) -> FactIndex<K> {
        FactIndex {
            kind: record.kind,
            key: record.key,
        }
    }

    pub(in crate::contract) fn get(
        &self,
        kind: FactKind,
        key: ObjectKey<K>,
    ) -> Option<&FactRecord<K, V>> {
        self.shards
            .get(Self::shard(key))?
            .get(&FactIndex { kind, key })
    }

    pub(in crate::contract) fn iter(&self) -> impl Iterator<Item = &FactRecord<K, V>> {
        self.shards
            .iter()
            .flat_map(|shard| shard.iter().map(|(_, record)| record))
    }

    pub(in crate::contract) fn iter_shard(
        &self,
        shard: usize,
    ) -> impl Iterator<Item = &FactRecord<K, V>> {
        self.shards
            .get(shard)
            .into_iter()
            .flat_map(|state| state.iter().map(|(_, record)| record))
    }

    pub(in crate::contract) fn changed_shards<'a>(
        &'a self,
        other: &'a Self,
    ) -> impl Iterator<Item = usize> + 'a {
        (0..FACT_SHARD_COUNT)
            .filter(move |&shard| self.shards[shard].root() != other.shards[shard].root())
    }

    pub(in crate::contract) fn ordered(&self) -> &[FactRecord<K, V>] {
        self.ordered
            .get_or_init(|| {
                let mut records = self.iter().cloned().collect::<Vec<_>>();
                records.sort_by(|left, right| {
                    (left.kind, left.key_bytes.as_slice())
                        .cmp(&(right.kind, right.key_bytes.as_slice()))
                });
                Arc::from(records.into_boxed_slice())
            })
            .as_ref()
    }

    pub(in crate::contract) fn state_matches(&self, other: &Self) -> bool {
        self.state.root() == other.state.root()
    }

    pub(in crate::contract) fn len(&self) -> usize {
        self.len
    }

    pub(in crate::contract) fn root(&self) -> StateRoot<FactRootRelation<K, V>> {
        self.state.root()
    }

    /// Returns the fixed byte-prefix shard for one fact key.
    pub(super) fn shard(key: ObjectKey<K>) -> usize {
        key.as_bytes().first().copied().map_or(0, usize::from)
    }

    pub(in crate::contract) fn apply_changes(
        &self,
        mut changes: Vec<backend_version::MapChange<FactRelation<K, V>>>,
    ) -> Result<Self, FactSetError> {
        let removals = changes
            .iter()
            .filter(|change| change.before.is_some() && change.after.is_none())
            .count();
        let additions = changes
            .iter()
            .filter(|change| change.before.is_none() && change.after.is_some())
            .count();
        let len = self
            .len
            .checked_sub(removals)
            .and_then(|length| length.checked_add(additions))
            .ok_or(FactSetError::Canonical)?;
        let mut changes_by_shard =
            BTreeMap::<usize, Vec<backend_version::MapChange<FactRelation<K, V>>>>::new();
        for change in changes.drain(..) {
            changes_by_shard
                .entry(Self::shard(change.key.key))
                .or_default()
                .push(change);
        }
        let mut shards = self.shards.to_vec();
        let mut root_changes = Vec::with_capacity(changes_by_shard.len());
        for (shard_index, mut shard_changes) in changes_by_shard {
            shard_changes.sort_by_key(|change| change.key);
            let Some(shard) = shards.get(shard_index) else {
                return Err(FactSetError::Canonical);
            };
            let previous_root = shard.root();
            let (prepared, _) =
                prepare_delta_with_state(shard, shard_changes).map_err(|error| match error {
                    backend_version::DeltaError::DuplicateKey => FactSetError::Duplicate,
                    backend_version::DeltaError::Canonical(_)
                    | backend_version::DeltaError::IncompleteBase
                    | backend_version::DeltaError::BaseMismatch
                    | backend_version::DeltaError::BeforeMismatch
                    | backend_version::DeltaError::TargetMismatch
                    | backend_version::DeltaError::Unsorted
                    | backend_version::DeltaError::NonAdjacent
                    | backend_version::DeltaError::CompositionMismatch => FactSetError::Canonical,
                })?;
            let next = prepared
                .commit(shard)
                .map_err(|_| FactSetError::Canonical)?;
            let next_root = next.root();
            let Some(slot) = shards.get_mut(shard_index) else {
                return Err(FactSetError::Canonical);
            };
            *slot = next;
            root_changes.push(backend_version::MapChange {
                key: u16::try_from(shard_index).map_err(|_| FactSetError::Canonical)?,
                before: Some(previous_root),
                after: Some(next_root),
            });
        }
        root_changes.sort_by_key(|change| change.key);
        let (prepared, _) =
            prepare_delta_with_state(&self.state, root_changes).map_err(|error| match error {
                backend_version::DeltaError::DuplicateKey => FactSetError::Duplicate,
                backend_version::DeltaError::Canonical(_)
                | backend_version::DeltaError::IncompleteBase
                | backend_version::DeltaError::BaseMismatch
                | backend_version::DeltaError::BeforeMismatch
                | backend_version::DeltaError::TargetMismatch
                | backend_version::DeltaError::Unsorted
                | backend_version::DeltaError::NonAdjacent
                | backend_version::DeltaError::CompositionMismatch => FactSetError::Canonical,
            })?;
        let state = prepared
            .commit(&self.state)
            .map_err(|_| FactSetError::Canonical)?;
        Ok(Self {
            state,
            shards: Arc::from(shards.into_boxed_slice()),
            len,
            ordered: OnceLock::new(),
        })
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> fmt::Debug for FactSet<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.ordered()).finish()
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> PartialEq for FactSet<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.ordered() == other.ordered()
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Eq for FactSet<K, V> {}
