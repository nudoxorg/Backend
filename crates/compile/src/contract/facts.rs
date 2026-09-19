//! Typed immutable facts and their persistent normalized relation.

use super::{
    AuthorityEpoch, AuthorityIdentity, FactSchema, FactVersion, InputManifest, InputManifestId,
    SessionId, manifest_scope, typed_of,
};
use backend_version::{
    ClosedRelationScope, CoverageWitness, ObjectKey, ObjectVersion, Relation, RelationState,
    Schema, ScopeRoot, StateError, StateRoot, prepare_delta_with_state,
};
use std::{
    collections::BTreeMap,
    fmt,
    marker::PhantomData,
    sync::{Arc, OnceLock},
};

/// Semantic facet or relation family carried by a typed fact record.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FactKind {
    /// A declaration or entity fact.
    Declaration = 1,
    /// A type or signature fact.
    Type = 2,
    /// An edge or occurrence relation.
    Edge = 3,
    /// A diagnostic fact.
    Diagnostic = 4,
    /// A positive dependency fact.
    Dependency = 5,
    /// An explicitly observed negative dependency.
    NegativeDependency = 6,
}

/// Exact provenance attached to an immutable typed fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FactEvidence {
    pub(super) authority: SessionId,
    pub(super) manifest: InputManifestId,
    pub(super) scope: ScopeRoot,
    pub(super) revision: u64,
    pub(super) epoch: AuthorityEpoch,
}

impl FactEvidence {
    /// Binds evidence to one authority, manifest, and discovery revision.
    #[must_use]
    pub fn new(authority: AuthorityIdentity, manifest: &InputManifest, revision: u64) -> Self {
        Self {
            authority: authority.digest(),
            manifest: manifest.digest(),
            scope: manifest_scope(manifest),
            revision,
            epoch: legacy_authority_epoch(authority.digest(), manifest.digest(), revision),
        }
    }

    /// Binds evidence to one byte-prefix fact shard of a manifest.
    ///
    /// Shard evidence is intentionally distinct from the manifest-wide
    /// scope. It permits selective validation of a changed shard while the
    /// enclosing snapshot still retains the complete manifest identity.
    #[must_use]
    pub fn for_shard(
        authority: AuthorityIdentity,
        manifest: &InputManifest,
        revision: u64,
        shard: u8,
    ) -> Self {
        Self {
            authority: authority.digest(),
            manifest: manifest.digest(),
            scope: fact_shard_scope(manifest.digest(), shard),
            revision,
            epoch: legacy_authority_epoch(authority.digest(), manifest.digest(), revision),
        }
    }

    /// Returns the authority identity that produced the fact.
    #[must_use]
    pub const fn authority(&self) -> SessionId {
        self.authority
    }

    /// Returns the exact manifest identity covered by this evidence.
    #[must_use]
    pub const fn manifest(&self) -> InputManifestId {
        self.manifest
    }

    /// Returns the exact scope covered by this evidence.
    #[must_use]
    pub const fn scope(&self) -> ScopeRoot {
        self.scope
    }

    /// Returns the discovery revision associated with this evidence.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the authority registration epoch associated with this evidence.
    #[must_use]
    pub const fn epoch(&self) -> AuthorityEpoch {
        self.epoch
    }
}

/// Immutable typed key/value fact with canonical bytes and admitted versions.
///
/// The schema parameters keep cross-schema substitution out of the type
/// system.  The key and value digests are recomputed from the retained bytes,
/// so callers cannot construct a record whose identity disagrees with its
/// canonical content.
pub struct FactRecord<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> {
    pub(super) kind: FactKind,
    pub(super) key: ObjectKey<K>,
    pub(super) key_bytes: Vec<u8>,
    pub(super) value: Vec<u8>,
    pub(super) version: ObjectVersion<V>,
    pub(super) evidence: Option<FactEvidence>,
}

// The schema marker types are zero-sized witnesses. Keep these impls
// explicitly bounded only by `Schema`; a derive would add accidental `K: Clone`
// and `V: Clone` requirements to otherwise copyable typed IDs.
impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Clone for FactRecord<K, V> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            key: self.key,
            key_bytes: self.key_bytes.clone(),
            value: self.value.clone(),
            version: self.version,
            evidence: self.evidence,
        }
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> fmt::Debug for FactRecord<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FactRecord")
            .field("kind", &self.kind)
            .field("key", &self.key)
            .field("key_bytes", &self.key_bytes)
            .field("value", &self.value)
            .field("version", &self.version)
            .field("evidence", &self.evidence)
            .finish()
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> PartialEq for FactRecord<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.key == other.key
            && self.key_bytes == other.key_bytes
            && self.value == other.value
            && self.version == other.version
            && self.evidence == other.evidence
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Eq for FactRecord<K, V> {}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> FactRecord<K, V> {
    /// Creates a canonical record from logical key and value bytes.
    #[must_use]
    pub fn new(kind: FactKind, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        let key_bytes = key.into();
        let value = value.into();
        let key = ObjectKey::from_value(key_bytes.as_slice());
        let version = ObjectVersion::from_value(value.as_slice());
        Self {
            kind,
            key,
            key_bytes,
            value,
            version,
            evidence: None,
        }
    }

    /// Attaches checked authority evidence to this record.
    #[must_use]
    pub fn with_evidence(mut self, evidence: FactEvidence) -> Self {
        self.evidence = Some(evidence);
        self
    }

    /// Returns the semantic facet or relation family.
    #[must_use]
    pub const fn kind(&self) -> FactKind {
        self.kind
    }

    /// Returns the stable typed object key.
    #[must_use]
    pub const fn key(&self) -> &ObjectKey<K> {
        &self.key
    }

    /// Returns the canonical logical key bytes retained for materialization.
    #[must_use]
    pub fn key_bytes(&self) -> &[u8] {
        &self.key_bytes
    }

    /// Returns the canonical value bytes retained for materialization.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }

    /// Returns the admitted typed value version.
    #[must_use]
    pub const fn version(&self) -> ObjectVersion<V> {
        self.version
    }

    /// Returns evidence, when this record has been bound to an authority.
    #[must_use]
    pub const fn evidence(&self) -> Option<FactEvidence> {
        self.evidence
    }
}

/// Compatibility fact projection retained for existing frontend callers.
///
/// New code should use [`FactRecord`], which retains typed key/value bytes and
/// evidence instead of exposing a string key and value digest alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactChange {
    /// Stable fact identity.
    pub key: String,
    /// Canonical fact payload digest.
    pub digest: FactVersion,
}

impl FactChange {
    /// Creates one fact change.
    #[must_use]
    pub fn new(key: impl Into<String>, digest: FactVersion) -> Self {
        Self {
            key: key.into(),
            digest,
        }
    }

    /// Returns the stable fact identity.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the canonical fact payload digest.
    #[must_use]
    pub const fn digest(&self) -> FactVersion {
        self.digest
    }
}

/// The immutable index key used by the normalized fact set.
///
/// The typed object key is the identity used for lookup and conflict
/// checking.  The retained canonical key bytes remain on the record and are
/// used only when producing the ordered compatibility view.
pub(crate) struct FactIndex<K: Schema<Value = [u8]>> {
    pub(super) kind: FactKind,
    pub(super) key: ObjectKey<K>,
}

impl<K: Schema<Value = [u8]>> Copy for FactIndex<K> {}

impl<K: Schema<Value = [u8]>> Clone for FactIndex<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: Schema<Value = [u8]>> fmt::Debug for FactIndex<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FactIndex")
            .field("kind", &self.kind)
            .field("key", &self.key)
            .finish()
    }
}

impl<K: Schema<Value = [u8]>> PartialEq for FactIndex<K> {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.key == other.key
    }
}

impl<K: Schema<Value = [u8]>> Eq for FactIndex<K> {}

impl<K: Schema<Value = [u8]>> PartialOrd for FactIndex<K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: Schema<Value = [u8]>> Ord for FactIndex<K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.kind.cmp(&other.kind).then(self.key.cmp(&other.key))
    }
}

/// Relation marker used to store facts in the shared version kernel.
pub(crate) struct FactRelation<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> {
    _marker: PhantomData<fn() -> (K, V)>,
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Relation for FactRelation<K, V> {
    const DOMAIN: u8 = 0x46;
    const TYPE: u16 = 1;
    type Key = FactIndex<K>;
    type Value = FactRecord<K, V>;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        output.push(K::DOMAIN);
        output.extend_from_slice(&K::TYPE.to_be_bytes());
        output.push(K::VERSION);
        output.push(key.kind as u8);
        output.extend_from_slice(key.key.as_bytes());
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.push(V::DOMAIN);
        output.extend_from_slice(&V::TYPE.to_be_bytes());
        output.push(V::VERSION);
        output.push(value.kind as u8);
        output.extend_from_slice(value.version.as_bytes());
        match value.evidence {
            Some(evidence) => {
                output.push(1);
                output.extend_from_slice(fact_evidence_digest(evidence).as_bytes());
            }
            None => output.push(0),
        }
    }
}

/// Root relation for the fixed-size fact shards.
///
/// A fact row carries a full typed value commitment and optional evidence
/// commitment. The shared kernel's 64 KiB canonical-node bound therefore
/// cannot safely pack an unbounded authority snapshot into one leaf. The
/// private root relation commits all 256 byte-prefix shards while each shard
/// still uses [`FactRelation`] keyed by `(FactKind, ObjectKey)`.
pub(crate) struct FactRootRelation<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> {
    _marker: PhantomData<fn() -> (K, V)>,
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Relation for FactRootRelation<K, V> {
    const DOMAIN: u8 = 0x47;
    const TYPE: u16 = 1;
    type Key = u16;
    type Value = StateRoot<FactRelation<K, V>>;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&key.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value.as_bytes());
    }
}

const FACT_SHARD_COUNT: usize = 256;

fn fact_evidence_digest(evidence: FactEvidence) -> FactVersion {
    let mut bytes = Vec::with_capacity(32 * 3 + 8);
    bytes.extend_from_slice(evidence.authority.as_bytes());
    bytes.extend_from_slice(evidence.manifest.as_bytes());
    bytes.extend_from_slice(evidence.scope.as_bytes());
    bytes.extend_from_slice(&evidence.revision.to_be_bytes());
    typed_of::<FactSchema>(&bytes)
}

/// Derives the stable scope name for one manifest byte-prefix shard.
fn fact_shard_scope(manifest: InputManifestId, shard: u8) -> ScopeRoot {
    let mut bytes = Vec::with_capacity(33);
    bytes.extend_from_slice(manifest.as_bytes());
    bytes.push(shard);
    ScopeRoot::from_bytes(typed_of::<FactSchema>(&bytes).to_bytes())
}

pub(crate) fn record_evidence_matches_snapshot<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>>(
    record: &FactRecord<K, V>,
    manifest: InputManifestId,
    revision: u64,
    authority: Option<SessionId>,
    epoch: Option<AuthorityEpoch>,
) -> bool {
    record.evidence.is_none_or(|evidence| {
        evidence.manifest == manifest
            && (evidence.scope == ScopeRoot::from_bytes(manifest.to_bytes())
                || evidence.scope
                    == fact_shard_scope(
                        manifest,
                        record.key.as_bytes().first().copied().unwrap_or(0),
                    ))
            && evidence.revision <= revision
            && authority.is_none_or(|expected| evidence.authority == expected)
            && epoch.is_none_or(|expected| evidence.epoch == expected)
    })
}

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
    pub(super) state: RelationState<FactRootRelation<K, V>>,
    pub(super) shards: Arc<[RelationState<FactRelation<K, V>>]>,
    pub(super) len: usize,
    ordered: OnceLock<Arc<[FactRecord<K, V>]>>,
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> FactSet<K, V> {
    pub(super) fn from_records(records: Vec<FactRecord<K, V>>) -> Result<Self, FactSetError> {
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

    pub(super) fn empty() -> Result<Self, FactSetError> {
        Self::from_records(Vec::new())
    }

    pub(super) fn key(record: &FactRecord<K, V>) -> FactIndex<K> {
        FactIndex {
            kind: record.kind,
            key: record.key,
        }
    }

    pub(super) fn get(&self, kind: FactKind, key: ObjectKey<K>) -> Option<&FactRecord<K, V>> {
        self.shards
            .get(Self::shard(key))?
            .get(&FactIndex { kind, key })
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &FactRecord<K, V>> {
        self.shards
            .iter()
            .flat_map(|shard| shard.iter().map(|(_, record)| record))
    }

    pub(super) fn iter_shard(&self, shard: usize) -> impl Iterator<Item = &FactRecord<K, V>> {
        self.shards
            .get(shard)
            .into_iter()
            .flat_map(|state| state.iter().map(|(_, record)| record))
    }

    pub(super) fn changed_shards<'a>(
        &'a self,
        other: &'a Self,
    ) -> impl Iterator<Item = usize> + 'a {
        (0..FACT_SHARD_COUNT)
            .filter(move |&shard| self.shards[shard].root() != other.shards[shard].root())
    }

    pub(super) fn ordered(&self) -> &[FactRecord<K, V>] {
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

    pub(super) fn state_matches(&self, other: &Self) -> bool {
        self.state.root() == other.state.root()
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn root(&self) -> StateRoot<FactRootRelation<K, V>> {
        self.state.root()
    }

    fn shard(key: ObjectKey<K>) -> usize {
        key.as_bytes().first().copied().map_or(0, usize::from)
    }

    pub(super) fn apply_changes(
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

fn legacy_authority_epoch(
    authority: SessionId,
    manifest: InputManifestId,
    revision: u64,
) -> AuthorityEpoch {
    let mut bytes = Vec::with_capacity(32 * 2 + 8);
    bytes.extend_from_slice(authority.as_bytes());
    bytes.extend_from_slice(manifest.as_bytes());
    bytes.extend_from_slice(&revision.to_be_bytes());
    typed_of::<super::AuthorityEpochSchema>(&bytes)
}

#[cfg(test)]
mod tests {
    use super::{FactKind, FactRecord, FactSet};
    use crate::{FactKeySchema, FactValueSchema};
    use backend_version::{Coverage, CoverageWitness, MapChange, apply_delta, prepare_delta};

    #[test]
    #[allow(
        clippy::expect_used,
        reason = "The regression fixture is constructed from bounded canonical constants."
    )]
    fn closed_materialization_witness_stays_out_of_authority() {
        let record = FactRecord::<FactKeySchema, FactValueSchema>::new(
            FactKind::Declaration,
            [0_u8],
            [1_u8],
        );
        let set = FactSet::from_records(vec![record.clone()]).expect("canonical fact set");
        assert_eq!(set.state.coverage().state(), Coverage::Closed);
        assert!(!set.state.coverage().state().is_complete());
        assert!(matches!(set.state.coverage(), CoverageWitness::Closed(_)));

        let index = FactSet::key(&record);
        let shard = FactSet::<FactKeySchema, FactValueSchema>::shard(index.key);
        let state = &set.shards[shard];
        let before = state.get(&index).cloned();
        assert!(before.is_some());
        let delta = prepare_delta(
            state,
            vec![MapChange {
                key: index,
                before,
                after: None,
            }],
        )
        .expect("closed relation deletion");
        let deleted = apply_delta(state, &delta).expect("closed relation apply");
        assert_eq!(deleted.get(&index), None);
        assert_eq!(deleted.coverage().state(), Coverage::Closed);

        // FactSet uses the same checked path-copy capability and retains the
        // closed witness after the physical row is removed.
        let updated = set
            .apply_changes(vec![MapChange {
                key: index,
                before: state.get(&index).cloned(),
                after: None,
            }])
            .expect("materialization update");
        assert_eq!(updated.len(), 0);
        assert!(matches!(
            updated.state.coverage(),
            CoverageWitness::Closed(_)
        ));
    }
}
