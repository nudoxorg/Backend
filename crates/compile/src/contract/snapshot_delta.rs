//! Exact scoped fact deltas and consumed prepared transitions.
use super::{
    AuthorityFence, FactIndex, FactKind, FactRecord, FactRelation, FactRootRelation, FactSet,
    FactSnapshot, InputManifestId, record_evidence_matches_snapshot,
};
use backend_version::{CoverageWitness, ObjectKey, ObjectVersion, Schema, StateRoot};
use std::{fmt, sync::Arc};

/// A typed replacement or incremental update for one exact fact scope.
pub struct FactDelta<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> {
    pub(super) base_manifest: InputManifestId,
    pub(super) base_revision: u64,
    pub(super) target_revision: u64,
    pub(super) coverage: CoverageWitness,
    pub(super) base_root: StateRoot<FactRootRelation<K, V>>,
    pub(super) target_root: StateRoot<FactRootRelation<K, V>>,
    pub(super) authority: Option<Arc<AuthorityFence>>,
    pub(super) changes: Vec<FactDeltaChange<K, V>>,
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Clone for FactDelta<K, V> {
    fn clone(&self) -> Self {
        Self {
            base_manifest: self.base_manifest,
            base_revision: self.base_revision,
            target_revision: self.target_revision,
            coverage: self.coverage,
            base_root: self.base_root,
            target_root: self.target_root,
            authority: self.authority.clone(),
            changes: self.changes.clone(),
        }
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> fmt::Debug for FactDelta<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FactDelta")
            .field("base_manifest", &self.base_manifest)
            .field("base_revision", &self.base_revision)
            .field("target_revision", &self.target_revision)
            .field("coverage", &self.coverage)
            .field("base_root", &self.base_root)
            .field("target_root", &self.target_root)
            .field("authority", &self.authority)
            .field("changes", &self.changes)
            .finish()
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> PartialEq for FactDelta<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.base_manifest == other.base_manifest
            && self.base_revision == other.base_revision
            && self.target_revision == other.target_revision
            && self.coverage == other.coverage
            && self.base_root == other.base_root
            && self.target_root == other.target_root
            && self.authority == other.authority
            && self.changes == other.changes
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Eq for FactDelta<K, V> {}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> FactDelta<K, V> {
    /// Returns the exact manifest scope this delta may modify.
    #[must_use]
    pub const fn manifest(&self) -> InputManifestId {
        self.base_manifest
    }

    /// Returns the source discovery revision.
    #[must_use]
    pub const fn base_revision(&self) -> u64 {
        self.base_revision
    }

    /// Returns the target discovery revision.
    #[must_use]
    pub const fn target_revision(&self) -> u64 {
        self.target_revision
    }

    /// Returns the target coverage capability.
    #[must_use]
    pub const fn coverage(&self) -> &CoverageWitness {
        &self.coverage
    }

    /// Returns canonical typed additions, removals, and replacements.
    #[must_use]
    pub fn changes(&self) -> &[FactDeltaChange<K, V>] {
        &self.changes
    }

    /// Prepares this delta against its exact base and retains the path-copy
    /// target for a later consuming commit.
    ///
    /// Preparation performs all base, evidence, canonical, and target-root
    /// checks once.  [`PreparedFactDelta::commit`] then publishes the retained
    /// snapshot without rebuilding the relation tree.
    ///
    /// # Errors
    ///
    /// Returns [`FactDeltaError::BaseMismatch`] when the base fence or root
    /// differs, or another [`FactDeltaError`] when a change is invalid.
    pub fn prepare(
        &self,
        base: &FactSnapshot<K, V>,
    ) -> Result<PreparedFactDelta<K, V>, FactDeltaError> {
        self.validate_base(base)?;
        let set = if self.changes.is_empty() {
            base.set.clone()
        } else {
            let updates = self.checked_updates(base)?;
            Arc::new(
                base.set
                    .apply_changes(updates)
                    .map_err(|_| FactDeltaError::InvalidSnapshot)?,
            )
        };
        if self.coverage.state().is_complete() && set.root() != self.target_root {
            return Err(FactDeltaError::InvalidSnapshot);
        }
        let snapshot = FactSnapshot::from_set(
            self.base_manifest,
            self.target_revision,
            self.coverage,
            set,
            self.authority.clone(),
        )
        .map_err(|_| FactDeltaError::InvalidSnapshot)?;
        let changed_shards = self
            .changes
            .iter()
            .map(FactDeltaChange::shard)
            .collect::<std::collections::BTreeSet<_>>();
        for shard in changed_shards {
            snapshot
                .validate_shard(shard)
                .map_err(|_| FactDeltaError::InvalidSnapshot)?;
        }
        Ok(PreparedFactDelta {
            delta: self.clone(),
            snapshot,
        })
    }

    /// Applies this delta to its exact base snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`FactDeltaError::BaseMismatch`] when the base fence differs,
    /// or another [`FactDeltaError`] when a change conflicts with the base
    /// records or cannot produce a valid target snapshot.
    pub fn apply(&self, base: &FactSnapshot<K, V>) -> Result<FactSnapshot<K, V>, FactDeltaError> {
        self.prepare(base).map(PreparedFactDelta::commit)
    }

    fn validate_base(&self, base: &FactSnapshot<K, V>) -> Result<(), FactDeltaError> {
        if base.manifest != self.base_manifest || base.revision != self.base_revision {
            return Err(FactDeltaError::BaseMismatch);
        }
        if base.set.root() != self.base_root {
            return Err(FactDeltaError::BaseMismatch);
        }
        if self.target_revision < self.base_revision {
            return Err(FactDeltaError::InvalidSnapshot);
        }
        Ok(())
    }

    fn checked_updates(
        &self,
        base: &FactSnapshot<K, V>,
    ) -> Result<Vec<backend_version::MapChange<FactRelation<K, V>>>, FactDeltaError> {
        let mut updates = Vec::with_capacity(self.changes.len());
        for change in &self.changes {
            match change {
                FactDeltaChange::Added(record) => {
                    if base.set.get(record.kind, record.key).is_some() {
                        return Err(FactDeltaError::Conflict);
                    }
                    if !record_evidence_matches_snapshot(
                        record,
                        self.base_manifest,
                        self.target_revision,
                        self.authority
                            .as_ref()
                            .map(|fence| fence.authority.digest()),
                        self.authority.as_ref().map(|fence| fence.epoch),
                    ) {
                        return Err(FactDeltaError::InvalidSnapshot);
                    }
                    updates.push(backend_version::MapChange {
                        key: FactSet::<K, V>::key(record),
                        before: None,
                        after: Some(record.clone()),
                    });
                }
                FactDeltaChange::Removed {
                    kind,
                    key,
                    key_bytes,
                    before,
                } => {
                    let Some(record) = base.set.get(*kind, *key) else {
                        return Err(FactDeltaError::MissingRecord);
                    };
                    if record.version != *before
                        || record.key_bytes.as_slice() != key_bytes.as_slice()
                    {
                        return Err(FactDeltaError::BaseMismatch);
                    }
                    updates.push(backend_version::MapChange {
                        key: FactIndex {
                            kind: *kind,
                            key: *key,
                        },
                        before: Some(record.clone()),
                        after: None,
                    });
                }
                FactDeltaChange::Modified { before, after } => {
                    let Some(record) = base.set.get(before.kind, before.key) else {
                        return Err(FactDeltaError::BaseMismatch);
                    };
                    if record != before.as_ref() {
                        return Err(FactDeltaError::BaseMismatch);
                    }
                    if FactSet::<K, V>::key(before.as_ref()) != FactSet::key(after.as_ref()) {
                        return Err(FactDeltaError::BaseMismatch);
                    }
                    if !record_evidence_matches_snapshot(
                        after,
                        self.base_manifest,
                        self.target_revision,
                        self.authority
                            .as_ref()
                            .map(|fence| fence.authority.digest()),
                        self.authority.as_ref().map(|fence| fence.epoch),
                    ) {
                        return Err(FactDeltaError::InvalidSnapshot);
                    }
                    updates.push(backend_version::MapChange {
                        key: FactSet::<K, V>::key(before.as_ref()),
                        before: Some(before.as_ref().clone()),
                        after: Some(after.as_ref().clone()),
                    });
                }
            }
        }
        Ok(updates)
    }

    pub(super) fn validate_against_target(
        &self,
        base: &FactSnapshot<K, V>,
        after: &FactSnapshot<K, V>,
    ) -> Result<(), FactDeltaError> {
        self.validate_base(base)?;
        if after.manifest != self.base_manifest
            || after.revision != self.target_revision
            || after.coverage != self.coverage
            || after.authority != self.authority
            || (after.coverage.state().is_complete() && after.set.root() != self.target_root)
        {
            return Err(FactDeltaError::InvalidSnapshot);
        }
        let mut keys = self
            .changes
            .iter()
            .map(FactDeltaChange::index)
            .collect::<Vec<_>>();
        keys.sort();
        if keys.windows(2).any(|window| window[0] == window[1]) {
            return Err(FactDeltaError::Conflict);
        }
        let mut additions = 0usize;
        let mut removals = 0usize;
        for change in &self.changes {
            match change {
                FactDeltaChange::Added(record) => {
                    if base.set.get(record.kind, record.key).is_some()
                        || after.set.get(record.kind, record.key) != Some(record)
                    {
                        return Err(FactDeltaError::InvalidSnapshot);
                    }
                    additions = additions
                        .checked_add(1)
                        .ok_or(FactDeltaError::InvalidSnapshot)?;
                }
                FactDeltaChange::Removed {
                    kind,
                    key,
                    key_bytes,
                    before,
                } => {
                    if !after.coverage.state().is_complete() || after.set.get(*kind, *key).is_some()
                    {
                        return Err(FactDeltaError::InvalidSnapshot);
                    }
                    let Some(record) = base.set.get(*kind, *key) else {
                        return Err(FactDeltaError::InvalidSnapshot);
                    };
                    if record.version != *before
                        || record.key_bytes.as_slice() != key_bytes.as_slice()
                    {
                        return Err(FactDeltaError::InvalidSnapshot);
                    }
                    removals = removals
                        .checked_add(1)
                        .ok_or(FactDeltaError::InvalidSnapshot)?;
                }
                FactDeltaChange::Modified {
                    before,
                    after: changed,
                } => {
                    if FactSet::<K, V>::key(before.as_ref())
                        != FactSet::<K, V>::key(changed.as_ref())
                        || base.set.get(before.kind, before.key) != Some(before.as_ref())
                        || after.set.get(changed.kind, changed.key) != Some(changed.as_ref())
                    {
                        return Err(FactDeltaError::InvalidSnapshot);
                    }
                }
            }
        }
        let expected_len = base
            .set
            .len()
            .checked_add(additions)
            .and_then(|length| length.checked_sub(removals))
            .ok_or(FactDeltaError::InvalidSnapshot)?;
        if after.set.len() != expected_len {
            return Err(FactDeltaError::InvalidSnapshot);
        }
        Ok(())
    }
}

/// A prepared fact delta whose target relation has already been path-copied.
///
/// The capability is consumed by [`Self::commit`], making it impossible for a
/// caller to publish an unvalidated target or accidentally rebuild the same
/// relation during an immediate authority transition.
pub struct PreparedFactDelta<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> {
    delta: FactDelta<K, V>,
    snapshot: FactSnapshot<K, V>,
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> fmt::Debug for PreparedFactDelta<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedFactDelta")
            .field("delta", &self.delta)
            .field("snapshot", &self.snapshot)
            .finish()
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> PreparedFactDelta<K, V> {
    /// Returns the replayable semantic delta retained by this capability.
    #[must_use]
    pub const fn delta(&self) -> &FactDelta<K, V> {
        &self.delta
    }

    /// Returns the already checked target snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &FactSnapshot<K, V> {
        &self.snapshot
    }

    /// Consumes the capability and publishes the retained target snapshot.
    #[must_use]
    pub fn commit(self) -> FactSnapshot<K, V> {
        self.snapshot
    }
}

/// One typed change in an exact fact delta.
pub enum FactDeltaChange<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> {
    /// Add a key absent from the base snapshot.
    Added(FactRecord<K, V>),
    /// Remove a key only when target coverage permits deletion.
    Removed {
        /// Semantic family of the removed key.
        kind: FactKind,
        /// Typed object key being removed.
        key: ObjectKey<K>,
        /// Canonical key bytes retained for store materialization.
        key_bytes: Vec<u8>,
        /// Exact base value version required by this removal.
        before: ObjectVersion<V>,
    },
    /// Replace a value at the same typed key.
    Modified {
        /// Exact base record required by the replacement.
        before: Box<FactRecord<K, V>>,
        /// New canonical record at that key.
        after: Box<FactRecord<K, V>>,
    },
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Clone for FactDeltaChange<K, V> {
    fn clone(&self) -> Self {
        match self {
            Self::Added(record) => Self::Added(record.clone()),
            Self::Removed {
                kind,
                key,
                key_bytes,
                before,
            } => Self::Removed {
                kind: *kind,
                key: *key,
                key_bytes: key_bytes.clone(),
                before: *before,
            },
            Self::Modified { before, after } => Self::Modified {
                before: Box::new(before.as_ref().clone()),
                after: Box::new(after.as_ref().clone()),
            },
        }
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> fmt::Debug for FactDeltaChange<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Added(record) => formatter.debug_tuple("Added").field(record).finish(),
            Self::Removed {
                kind,
                key,
                key_bytes,
                before,
            } => formatter
                .debug_struct("Removed")
                .field("kind", kind)
                .field("key", key)
                .field("key_bytes", key_bytes)
                .field("before", before)
                .finish(),
            Self::Modified { before, after } => formatter
                .debug_struct("Modified")
                .field("before", before)
                .field("after", after)
                .finish(),
        }
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> PartialEq for FactDeltaChange<K, V> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Added(left), Self::Added(right)) => left == right,
            (
                Self::Removed {
                    kind: left_kind,
                    key: left_key,
                    key_bytes: left_bytes,
                    before: left_before,
                },
                Self::Removed {
                    kind: right_kind,
                    key: right_key,
                    key_bytes: right_bytes,
                    before: right_before,
                },
            ) => {
                left_kind == right_kind
                    && left_key == right_key
                    && left_bytes == right_bytes
                    && left_before == right_before
            }
            (
                Self::Modified {
                    before: left_before,
                    after: left_after,
                },
                Self::Modified {
                    before: right_before,
                    after: right_after,
                },
            ) => left_before == right_before && left_after == right_after,
            _ => false,
        }
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Eq for FactDeltaChange<K, V> {}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> FactDeltaChange<K, V> {
    pub(super) fn shard(&self) -> u8 {
        self.index().key.as_bytes()[0]
    }

    fn index(&self) -> FactIndex<K> {
        match self {
            Self::Added(record) => FactSet::<K, V>::key(record),
            Self::Removed { kind, key, .. } => FactIndex {
                kind: *kind,
                key: *key,
            },
            Self::Modified { before, .. } => FactSet::<K, V>::key(before.as_ref()),
        }
    }

    pub(super) fn sort_key(&self) -> (FactKind, &[u8]) {
        match self {
            Self::Added(record) => (record.kind, record.key_bytes.as_slice()),
            Self::Removed {
                kind, key_bytes, ..
            } => (*kind, key_bytes.as_slice()),
            Self::Modified { after, .. } => (after.kind, after.key_bytes.as_slice()),
        }
    }
}

/// Failure while constructing a typed fact snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactSnapshotError {
    /// Complete coverage was supplied without an authority registry fence.
    UnboundComplete,
    /// A complete authority result contained no typed fact records.
    EmptyComplete,
    /// Complete coverage did not name the exact manifest scope.
    CoverageScopeMismatch,
    /// A semantic family/key pair occurred more than once.
    DuplicateRecord,
    /// A record's manifest, scope, or revision evidence differed from the snapshot.
    EvidenceMismatch,
    /// The normalized relation exceeded the canonical kernel limits.
    Canonical,
}

impl fmt::Display for FactSnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnboundComplete => "complete fact snapshot requires an admitted authority fence",
            Self::EmptyComplete => "complete fact snapshot contains no typed records",
            Self::CoverageScopeMismatch => "fact snapshot coverage scope does not match manifest",
            Self::DuplicateRecord => "fact snapshot contains a duplicate record",
            Self::EvidenceMismatch => "fact snapshot record evidence does not match its fence",
            Self::Canonical => "fact snapshot exceeds canonical relation limits",
        };
        f.write_str(message)
    }
}

impl std::error::Error for FactSnapshotError {}

/// Failure while deriving or applying an exact scoped fact delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactDeltaError {
    /// Base and target snapshots name different manifest scopes.
    ScopeMismatch,
    /// The supplied base snapshot does not match the delta fence.
    BaseMismatch,
    /// A delta adds a key already present in its base.
    Conflict,
    /// A delta removes a key absent from its base.
    MissingRecord,
    /// Applying the delta could not rebuild its exact typed manifest.
    InvalidManifest,
    /// The rebuilt target snapshot violated its coverage contract.
    InvalidSnapshot,
}

impl fmt::Display for FactDeltaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ScopeMismatch => "fact delta scopes do not match",
            Self::BaseMismatch => "fact delta base fence does not match",
            Self::Conflict => "fact delta adds an existing record",
            Self::MissingRecord => "fact delta removes a missing record",
            Self::InvalidManifest => "fact delta manifest could not be rebuilt",
            Self::InvalidSnapshot => "fact delta target snapshot is invalid",
        };
        f.write_str(message)
    }
}

impl std::error::Error for FactDeltaError {}
