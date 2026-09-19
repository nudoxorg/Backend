//! Persistent fact snapshots and exact scoped deltas.

use super::{
    AuthorityFence, CompleteAuthorityCoverage, FactIndex, FactKind, FactRecord, FactRelation,
    FactRootRelation, FactSet, FactSetError, InputManifest, InputManifestId,
    record_evidence_matches_snapshot,
};
use backend_version::{CoverageWitness, Schema};
use std::{fmt, sync::Arc};

#[path = "snapshot_delta.rs"]
mod delta;
pub use delta::{FactDelta, FactDeltaChange, FactDeltaError, FactSnapshotError, PreparedFactDelta};

/// A manifest-bound immutable fact snapshot.
///
/// Snapshots share their normalized fact index through immutable path-copying
/// storage.  [`Self::records`] keeps the historical ordered slice contract and
/// materializes that view only when a caller requests it.
pub struct FactSnapshot<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> {
    manifest: InputManifestId,
    revision: u64,
    coverage: CoverageWitness,
    set: Arc<FactSet<K, V>>,
    authority: Option<Arc<AuthorityFence>>,
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Clone for FactSnapshot<K, V> {
    fn clone(&self) -> Self {
        Self {
            manifest: self.manifest,
            revision: self.revision,
            coverage: self.coverage,
            set: self.set.clone(),
            authority: self.authority.clone(),
        }
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> fmt::Debug for FactSnapshot<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FactSnapshot")
            .field("manifest", &self.manifest)
            .field("revision", &self.revision)
            .field("coverage", &self.coverage)
            .field("authority", &self.authority)
            .field("records", &self.set.ordered())
            .finish()
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> PartialEq for FactSnapshot<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.manifest == other.manifest
            && self.revision == other.revision
            && self.coverage == other.coverage
            && self.authority == other.authority
            && self.set == other.set
    }
}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> Eq for FactSnapshot<K, V> {}

impl<K: Schema<Value = [u8]>, V: Schema<Value = [u8]>> FactSnapshot<K, V> {
    /// Builds a sorted partial snapshot.
    ///
    /// Complete snapshots cannot be declared with a raw coverage label. Use
    /// the checked native authority admission path, which retains the private
    /// authority fence alongside the typed records.
    ///
    /// # Errors
    ///
    /// Returns [`FactSnapshotError::UnboundComplete`] when complete coverage
    /// is supplied without a checked authority fence,
    /// [`FactSnapshotError::CoverageScopeMismatch`] when a checked complete
    /// coverage scope does not name this manifest, or
    /// [`FactSnapshotError::DuplicateRecord`] when a key appears twice,
    /// [`FactSnapshotError::Canonical`] when the normalized relation exceeds
    /// its checked limits, or [`FactSnapshotError::EvidenceMismatch`] when
    /// record evidence names a different manifest or a future revision.
    pub fn new(
        manifest: &InputManifest,
        revision: u64,
        coverage: CoverageWitness,
        records: Vec<FactRecord<K, V>>,
    ) -> Result<Self, FactSnapshotError> {
        Self::from_parts(manifest.digest(), revision, coverage, records)
    }

    fn from_parts(
        manifest: InputManifestId,
        revision: u64,
        coverage: CoverageWitness,
        mut records: Vec<FactRecord<K, V>>,
    ) -> Result<Self, FactSnapshotError> {
        if coverage.state().is_complete() {
            return Err(FactSnapshotError::UnboundComplete);
        }
        if records
            .iter()
            .any(|record| !record_evidence_matches_snapshot(record, manifest, revision, None, None))
        {
            return Err(FactSnapshotError::EvidenceMismatch);
        }
        records.sort_by(|left, right| {
            (left.kind, left.key_bytes.as_slice()).cmp(&(right.kind, right.key_bytes.as_slice()))
        });
        if records
            .windows(2)
            .any(|window| window[0].kind == window[1].kind && window[0].key == window[1].key)
        {
            return Err(FactSnapshotError::DuplicateRecord);
        }
        let records = FactSet::from_records(records).map_err(|error| match error {
            FactSetError::Duplicate => FactSnapshotError::DuplicateRecord,
            FactSetError::Canonical => FactSnapshotError::Canonical,
        })?;
        Ok(Self {
            manifest,
            revision,
            coverage,
            set: Arc::new(records),
            authority: None,
        })
    }

    fn from_set(
        manifest: InputManifestId,
        revision: u64,
        coverage: CoverageWitness,
        set: Arc<FactSet<K, V>>,
        authority: Option<Arc<AuthorityFence>>,
    ) -> Result<Self, FactSnapshotError> {
        // `FactDelta::apply` validates changed rows before this handoff. The
        // snapshot revision is a coverage frontier: unchanged rows may carry
        // older producing evidence and remain valid under the same manifest.
        if coverage.state().is_complete() {
            let Some(fence) = authority.as_ref() else {
                return Err(FactSnapshotError::UnboundComplete);
            };
            if fence.manifest != manifest
                || fence.revision != revision
                || fence.witness().scope_root() != coverage.scope_root()
            {
                return Err(FactSnapshotError::CoverageScopeMismatch);
            }
        } else if authority.is_some() {
            return Err(FactSnapshotError::CoverageScopeMismatch);
        }
        Ok(Self {
            manifest,
            revision,
            coverage,
            set,
            authority,
        })
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "consuming the private capability closes its admission handoff"
    )]
    pub(crate) fn from_complete_capability(
        capability: CompleteAuthorityCoverage,
        records: Vec<FactRecord<K, V>>,
    ) -> Result<Self, FactSnapshotError> {
        if records.is_empty() {
            return Err(FactSnapshotError::EmptyComplete);
        }
        let fence = capability.fence().clone();
        let expected = fence.evidence();
        let records = records
            .into_iter()
            .map(|record| {
                if record_evidence_matches_snapshot(
                    &record,
                    fence.manifest,
                    fence.revision,
                    Some(expected.authority),
                    Some(expected.epoch),
                ) {
                    Ok(record)
                } else {
                    Err(FactSnapshotError::EvidenceMismatch)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut records = records;
        records.sort_by(|left, right| {
            (left.kind, left.key_bytes.as_slice()).cmp(&(right.kind, right.key_bytes.as_slice()))
        });
        if records
            .windows(2)
            .any(|window| window[0].kind == window[1].kind && window[0].key == window[1].key)
        {
            return Err(FactSnapshotError::DuplicateRecord);
        }
        let set = FactSet::from_records(records).map_err(|error| match error {
            FactSetError::Duplicate => FactSnapshotError::DuplicateRecord,
            FactSetError::Canonical => FactSnapshotError::Canonical,
        })?;
        Ok(Self {
            manifest: fence.manifest,
            revision: fence.revision,
            coverage: fence.witness(),
            set: Arc::new(set),
            authority: Some(fence),
        })
    }

    pub(crate) fn into_complete_parts(
        self,
    ) -> (
        CoverageWitness,
        Arc<FactSet<K, V>>,
        Option<Arc<AuthorityFence>>,
    ) {
        (self.coverage, self.set, self.authority)
    }

    /// Returns the exact manifest identity named by this snapshot.
    #[must_use]
    pub const fn manifest(&self) -> InputManifestId {
        self.manifest
    }

    /// Returns the authority discovery revision represented by this snapshot.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the coverage capability for this snapshot.
    #[must_use]
    pub const fn coverage(&self) -> &CoverageWitness {
        &self.coverage
    }

    /// Returns records in canonical kind/key order.
    #[must_use]
    pub fn records(&self) -> &[FactRecord<K, V>] {
        self.set.ordered()
    }

    /// Returns the committed root bytes for one selective fact shard.
    #[must_use]
    pub fn shard_root(&self, shard: u8) -> [u8; 32] {
        self.set.shards[usize::from(shard)].root().to_bytes()
    }

    /// Revalidates only one shard's evidence and canonical relation root.
    ///
    /// This is the bounded validation path used by selective authority
    /// updates. A caller can validate a changed shard without rescanning the
    /// other 255 immutable shard states.
    ///
    /// # Errors
    ///
    /// Returns [`FactSnapshotError::EvidenceMismatch`] when a row in the
    /// selected shard is bound to another manifest, revision, authority, or
    /// shard scope.
    pub fn validate_shard(&self, shard: u8) -> Result<(), FactSnapshotError> {
        let _root = self.shard_root(shard);
        let authority = self
            .authority
            .as_ref()
            .map(|fence| fence.authority.digest());
        let epoch = self.authority.as_ref().map(|fence| fence.epoch);
        if self.set.iter_shard(usize::from(shard)).all(|record| {
            record_evidence_matches_snapshot(record, self.manifest, self.revision, authority, epoch)
        }) {
            Ok(())
        } else {
            Err(FactSnapshotError::EvidenceMismatch)
        }
    }

    #[cfg(test)]
    pub(crate) fn shares_record_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.set, &other.set)
    }

    #[cfg(test)]
    pub(crate) fn shared_record_nodes_with(&self, other: &Self) -> usize {
        if self.set.state_matches(&other.set) {
            self.set.state.iter().count()
        } else {
            // A fact update path-copies the touched shard and the compact
            // root relation while retaining every untouched shard state by
            // Arc. Count those retained relation subtrees as structural
            // reuse even when a one-leaf kernel update has no internal child
            // node to report through DeltaWork.
            self.set
                .shards
                .iter()
                .zip(other.set.shards.iter())
                .filter(|(left, right)| left.root() == right.root())
                .count()
        }
    }

    /// Computes an exact scoped delta to `after`.
    ///
    /// # Errors
    ///
    /// Returns [`FactDeltaError::ScopeMismatch`] when the snapshots name
    /// different manifest scopes.
    pub fn delta_to(&self, after: &Self) -> Result<FactDelta<K, V>, FactDeltaError> {
        if self.manifest != after.manifest {
            return Err(FactDeltaError::ScopeMismatch);
        }
        if after.revision < self.revision {
            return Err(FactDeltaError::InvalidSnapshot);
        }
        // A root match proves every normalized row, including evidence
        // commitments, is unchanged. A revision or coverage-frontier
        // advance can therefore publish an empty metadata-only delta without
        // materializing or scanning the ordered compatibility view.
        if self.set.state_matches(&after.set) {
            return Ok(FactDelta {
                base_manifest: self.manifest,
                base_revision: self.revision,
                target_revision: after.revision,
                coverage: after.coverage,
                base_root: self.set.root(),
                target_root: after.set.root(),
                authority: after.authority.clone(),
                changes: Vec::new(),
            });
        }
        let changed_shards = self.set.changed_shards(&after.set).collect::<Vec<_>>();
        let mut changes = Vec::new();
        for &shard in &changed_shards {
            for before in self.set.iter_shard(shard) {
                match after.set.get(before.kind, before.key) {
                    Some(after_record) if after_record == before => {}
                    Some(after_record) => changes.push(FactDeltaChange::Modified {
                        before: Box::new(before.clone()),
                        after: Box::new(after_record.clone()),
                    }),
                    None if after.coverage.state().is_complete() => {
                        changes.push(FactDeltaChange::Removed {
                            kind: before.kind,
                            key: before.key,
                            key_bytes: before.key_bytes.clone(),
                            before: before.version,
                        });
                    }
                    None => {}
                }
            }
        }
        for &shard in &changed_shards {
            for after_record in after.set.iter_shard(shard) {
                if self.set.get(after_record.kind, after_record.key).is_none() {
                    changes.push(FactDeltaChange::Added(after_record.clone()));
                }
            }
        }
        changes.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
        Ok(FactDelta {
            base_manifest: self.manifest,
            base_revision: self.revision,
            target_revision: after.revision,
            coverage: after.coverage,
            base_root: self.set.root(),
            target_root: after.set.root(),
            authority: after.authority.clone(),
            changes,
        })
    }

    /// Builds a delta from an authority-supplied exact change set.
    ///
    /// This is the efficient path when the authority has already normalized
    /// its checked relation changes.  The target relation root is retained in
    /// the returned delta and changed rows are checked against both snapshots;
    /// the persistent tree is prepared only when the caller applies the delta.
    /// Use [`FactDelta::prepare`] when the checked target should be committed
    /// immediately. A partial target can therefore never infer deletion from
    /// an omitted row.
    ///
    /// # Errors
    ///
    /// Returns [`FactDeltaError::ScopeMismatch`] for different manifest
    /// scopes, [`FactDeltaError::InvalidSnapshot`] when the supplied changes
    /// do not produce `after`, or the applicable base/change error otherwise.
    pub fn delta_from_changes(
        &self,
        after: &Self,
        changes: Vec<FactDeltaChange<K, V>>,
    ) -> Result<FactDelta<K, V>, FactDeltaError> {
        if self.manifest != after.manifest {
            return Err(FactDeltaError::ScopeMismatch);
        }
        let delta = FactDelta {
            base_manifest: self.manifest,
            base_revision: self.revision,
            target_revision: after.revision,
            coverage: after.coverage,
            base_root: self.set.root(),
            target_root: after.set.root(),
            authority: after.authority.clone(),
            changes,
        };
        delta.validate_against_target(self, after)?;
        Ok(delta)
    }

    /// Builds and checks a prepared delta for immediate O(1) commit.
    ///
    /// This combines [`Self::delta_from_changes`] with one persistent relation
    /// preparation, retaining the resulting path-copy tree in a checked
    /// [`PreparedFactDelta`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::delta_from_changes`] or
    /// [`FactDelta::prepare`].
    pub fn prepare_from_changes(
        &self,
        after: &Self,
        changes: Vec<FactDeltaChange<K, V>>,
    ) -> Result<PreparedFactDelta<K, V>, FactDeltaError> {
        self.delta_from_changes(after, changes)?.prepare(self)
    }
}
