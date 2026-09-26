//! Typed immutable facts and their persistent normalized relation.

use super::{
    AuthorityEpoch, AuthorityIdentity, FactSchema, FactVersion, InputManifest, InputManifestId,
    SessionId, manifest_scope, typed_of,
};
use backend_version::{ObjectKey, ObjectVersion, Relation, Schema, ScopeRoot, StateRoot};
use std::{fmt, marker::PhantomData};

mod set;

pub(crate) use set::{FactSet, FactSetError};

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
