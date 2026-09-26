//! Mutations of the daemon pending-attempt relation.

use super::{
    DaemonError, Delta, DeltaWork, MapChange, PendingAttempt, PendingAttemptDelta,
    PendingAttemptRelation, PendingAttemptRelationSchema, PendingAttemptRelationState,
    PendingAttemptSnapshot, PendingAttemptStateRoot, PendingAttemptValue, PendingRemoteEnvelope,
    PendingRemoteKey, RemoteCorrelationKey, StateRoot, prepare_delta_with_state,
};

impl<V, A> PendingAttemptRelation<V, A> {
    pub(in crate::daemon) const fn len(&self) -> u64 {
        self.state_root.count
    }

    /// Captures the exact owner state root and accounting values at one
    /// linearization point. The count is maintained by the owner and does not
    /// require traversing the relation tree.
    pub(in crate::daemon) fn snapshot(&self) -> PendingAttemptSnapshot {
        PendingAttemptSnapshot {
            revision: self.state_root.revision,
            root: self.state_root,
            count: self.state_root.count,
            retained_bytes: self.state_root.retained_bytes,
        }
    }

    pub(in crate::daemon) fn get(
        &self,
        key: &PendingRemoteKey,
    ) -> Option<&PendingRemoteEnvelope<V, A>> {
        self.entries.get(key).map(|attempt| &attempt.envelope)
    }

    pub(in crate::daemon) fn get_mut(
        &mut self,
        key: &PendingRemoteKey,
    ) -> Option<&mut PendingRemoteEnvelope<V, A>> {
        self.entries
            .get_mut(key)
            .map(|attempt| &mut attempt.envelope)
    }

    pub(in crate::daemon) fn key_for_correlation(
        &self,
        correlation: &RemoteCorrelationKey,
    ) -> Option<PendingRemoteKey> {
        self.by_correlation.get(correlation).copied()
    }

    /// Reserves exact result bytes while admission is in flight. The metadata
    /// row is changed with one exact-base delta; no unrelated rows are cloned
    /// or rebuilt.
    pub(in crate::daemon) fn charge_result_bytes(
        &mut self,
        key: PendingRemoteKey,
        bytes: usize,
        max_bytes: usize,
    ) -> Result<(), DaemonError> {
        let current = self
            .metadata
            .get(&key)
            .cloned()
            .ok_or(DaemonError::RemoteResultUnmatched)?;
        if !self.entries.contains_key(&key) {
            return Err(DaemonError::RemoteResultUnmatched);
        }
        let bytes_u64 = checked_u64(bytes)?;
        let max_bytes_u64 = checked_u64(max_bytes)?;
        let next_attempt_bytes = current
            .retained_bytes
            .checked_add(bytes_u64)
            .ok_or(DaemonError::Backpressure)?;
        let next_value = PendingAttemptValue {
            correlation: current.correlation,
            retained_bytes: next_attempt_bytes,
            generation: current.generation,
        };
        let current_bytes = self.retained_bytes_usize()?;
        let current_count = self.count_usize()?;
        let next_bytes = project_retained_bytes(current_bytes, Some(&current), Some(&next_value))?;
        let next_count = project_count(current_count, Some(&current), Some(&next_value))?;
        if next_bytes > max_bytes || checked_u64(next_bytes)? > max_bytes_u64 {
            return Err(DaemonError::Backpressure);
        }
        let next_revision = checked_increment(self.state_root.revision)?;
        let (metadata, relation, work) = Self::prepare_change(
            &self.metadata,
            MapChange {
                key,
                before: Some(current),
                after: Some(next_value),
            },
        )?;
        let target = Self::target_root(
            metadata.root(),
            next_revision,
            self.state_root.next_generation,
            next_count,
            next_bytes,
        )?;
        self.commit_metadata(metadata, relation, work, target);
        self.debug_assert_invariants();
        Ok(())
    }

    /// Rolls back a result-byte reservation while retaining the affine
    /// envelope for fallback or retry.
    pub(in crate::daemon) fn release_result_bytes(
        &mut self,
        key: PendingRemoteKey,
        bytes: usize,
    ) -> Result<(), DaemonError> {
        let current = self
            .metadata
            .get(&key)
            .cloned()
            .ok_or(DaemonError::RemoteResultUnmatched)?;
        if !self.entries.contains_key(&key) {
            return Err(DaemonError::RemoteResultUnmatched);
        }
        let bytes_u64 = checked_u64(bytes)?;
        let current_bytes = self.retained_bytes_usize()?;
        let current_count = self.count_usize()?;
        if current.retained_bytes < bytes_u64 || current_bytes < bytes {
            return Err(DaemonError::Accounting);
        }
        let next_attempt_bytes = current.retained_bytes - bytes_u64;
        let next_value = PendingAttemptValue {
            correlation: current.correlation,
            retained_bytes: next_attempt_bytes,
            generation: current.generation,
        };
        let next_bytes = project_retained_bytes(current_bytes, Some(&current), Some(&next_value))?;
        let next_count = project_count(current_count, Some(&current), Some(&next_value))?;
        let next_revision = checked_increment(self.state_root.revision)?;
        let (metadata, relation, work) = Self::prepare_change(
            &self.metadata,
            MapChange {
                key,
                before: Some(current),
                after: Some(next_value),
            },
        )?;
        let target = Self::target_root(
            metadata.root(),
            next_revision,
            self.state_root.next_generation,
            next_count,
            next_bytes,
        )?;
        self.commit_metadata(metadata, relation, work, target);
        self.debug_assert_invariants();
        Ok(())
    }

    /// Inserts an attempt and its materialized indexes as one owner
    /// operation. All fallible identity, capacity, and relation checks run
    /// before either index or callback is changed.
    pub(in crate::daemon) fn insert(
        &mut self,
        key: PendingRemoteKey,
        envelope: PendingRemoteEnvelope<V, A>,
        retained_bytes: usize,
        max_count: usize,
        max_bytes: usize,
    ) -> Result<PendingRemoteKey, DaemonError> {
        if key.generation() != 0 {
            return Err(DaemonError::Accounting);
        }
        let current_count = self.count_usize()?;
        let current_bytes = self.retained_bytes_usize()?;
        if current_count >= max_count {
            return Err(DaemonError::Backpressure);
        }
        let correlation = envelope.correlation();
        if self.entries.contains_key(&key) || self.by_correlation.contains_key(&correlation) {
            return Err(DaemonError::Dispatch("duplicate remote attempt".into()));
        }
        let retained_bytes_u64 = checked_u64(retained_bytes)?;
        let max_bytes_u64 = checked_u64(max_bytes)?;
        let next_count = project_count(
            current_count,
            None,
            Some(&PendingAttemptValue {
                correlation,
                retained_bytes: retained_bytes_u64,
                generation: self.state_root.next_generation,
            }),
        )?;
        let next_bytes = project_retained_bytes(
            current_bytes,
            None,
            Some(&PendingAttemptValue {
                correlation,
                retained_bytes: retained_bytes_u64,
                generation: self.state_root.next_generation,
            }),
        )?;
        if next_bytes > max_bytes || checked_u64(next_bytes)? > max_bytes_u64 {
            return Err(DaemonError::Backpressure);
        }
        let generation = self.state_root.next_generation;
        if generation == 0 {
            return Err(DaemonError::Accounting);
        }
        let next_generation = checked_increment(generation)?;
        let next_revision = checked_increment(self.state_root.revision)?;
        let bound_key = key.with_generation(generation);
        let (metadata, relation, work) = Self::prepare_change(
            &self.metadata,
            MapChange {
                key: bound_key,
                before: None,
                after: Some(PendingAttemptValue {
                    correlation,
                    retained_bytes: retained_bytes_u64,
                    generation,
                }),
            },
        )?;
        let target = Self::target_root(
            metadata.root(),
            next_revision,
            next_generation,
            next_count,
            next_bytes,
        )?;

        // The checks above make these map operations infallible in the owner
        // model. If an internal invariant is nevertheless broken, restore the
        // changed entry before returning an accounting error and leave the
        // checked relation untouched.
        let mut envelope = envelope;
        envelope.bind_generation(generation);
        if let Some(previous) = self.entries.insert(bound_key, PendingAttempt { envelope }) {
            self.entries.insert(bound_key, previous);
            return Err(DaemonError::Accounting);
        }
        if let Some(previous) = self.by_correlation.insert(correlation, bound_key) {
            self.by_correlation.insert(correlation, previous);
            let _ = self.entries.remove(&bound_key);
            return Err(DaemonError::Accounting);
        }
        self.commit_metadata(metadata, relation, work, target);
        self.debug_assert_invariants();
        Ok(bound_key)
    }

    /// Removes one attempt and its materialized indexes together. The
    /// relation transition is prepared before either callback index changes.
    pub(in crate::daemon) fn remove(
        &mut self,
        key: PendingRemoteKey,
    ) -> Result<PendingRemoteEnvelope<V, A>, DaemonError> {
        let current = self
            .metadata
            .get(&key)
            .cloned()
            .ok_or(DaemonError::RemoteResultUnmatched)?;
        if self.by_correlation.get(&current.correlation) != Some(&key) {
            return Err(DaemonError::Accounting);
        }
        if !self.entries.contains_key(&key) {
            return Err(DaemonError::RemoteResultUnmatched);
        }
        let current_bytes = self.retained_bytes_usize()?;
        let current_count = self.count_usize()?;
        let next_bytes = project_retained_bytes(current_bytes, Some(&current), None)?;
        let next_count = project_count(current_count, Some(&current), None)?;
        let next_revision = checked_increment(self.state_root.revision)?;
        let (metadata, relation, work) = Self::prepare_change(
            &self.metadata,
            MapChange {
                key,
                before: Some(current.clone()),
                after: None,
            },
        )?;
        let target = Self::target_root(
            metadata.root(),
            next_revision,
            self.state_root.next_generation,
            next_count,
            next_bytes,
        )?;

        let Some(attempt) = self.entries.remove(&key) else {
            return Err(DaemonError::RemoteResultUnmatched);
        };
        let Some(index_key) = self.by_correlation.remove(&current.correlation) else {
            self.entries.insert(key, attempt);
            return Err(DaemonError::Accounting);
        };
        if index_key != key {
            self.by_correlation.insert(current.correlation, index_key);
            self.entries.insert(key, attempt);
            return Err(DaemonError::Accounting);
        }
        self.commit_metadata(metadata, relation, work, target);
        self.debug_assert_invariants();
        Ok(attempt.envelope)
    }

    /// Drops all pending callbacks through the same one-row exact-base path.
    /// This is lifecycle cleanup and may perform one bounded tree update per
    /// retained row; ordinary admission never materializes unrelated rows.
    pub(in crate::daemon) fn clear(&mut self) -> Result<(), DaemonError> {
        loop {
            let next_key = self.metadata.iter().next().map(|(key, _)| *key);
            let Some(key) = next_key else {
                break;
            };
            let envelope = self.remove(key)?;
            drop(envelope);
        }
        self.debug_assert_invariants();
        Ok(())
    }

    fn count_usize(&self) -> Result<usize, DaemonError> {
        usize::try_from(self.state_root.count).map_err(|_| DaemonError::Accounting)
    }

    fn retained_bytes_usize(&self) -> Result<usize, DaemonError> {
        usize::try_from(self.state_root.retained_bytes).map_err(|_| DaemonError::Accounting)
    }

    fn prepare_change(
        base: &PendingAttemptRelationState,
        change: MapChange<PendingAttemptRelationSchema>,
    ) -> Result<
        (
            PendingAttemptRelationState,
            Delta<PendingAttemptRelationSchema>,
            DeltaWork,
        ),
        DaemonError,
    > {
        let (prepared, work) =
            prepare_delta_with_state(base, vec![change]).map_err(|_| DaemonError::Accounting)?;
        let relation = prepared.delta().clone();
        let metadata = prepared.commit(base).map_err(|_| DaemonError::Accounting)?;
        Ok((metadata, relation, work))
    }

    fn target_root(
        relation: StateRoot<PendingAttemptRelationSchema>,
        revision: u64,
        next_generation: u64,
        count: usize,
        retained_bytes: usize,
    ) -> Result<PendingAttemptStateRoot, DaemonError> {
        Ok(PendingAttemptStateRoot {
            relation,
            revision,
            next_generation,
            count: checked_u64(count)?,
            retained_bytes: checked_u64(retained_bytes)?,
        })
    }

    fn commit_metadata(
        &mut self,
        metadata: PendingAttemptRelationState,
        relation: Delta<PendingAttemptRelationSchema>,
        work: DeltaWork,
        target: PendingAttemptStateRoot,
    ) {
        let base = self.state_root;
        self.metadata = metadata;
        self.state_root = target;
        self.last_delta = Some(PendingAttemptDelta {
            base,
            target,
            relation,
            work,
        });
    }

    #[cfg(test)]
    fn debug_assert_invariants(&self) {
        debug_assert_eq!(
            u64::try_from(self.entries.len()).ok(),
            Some(self.state_root.count)
        );
        debug_assert_eq!(self.entries.len(), self.by_correlation.len());
        debug_assert_eq!(self.state_root.relation, self.metadata.root());
        if let Some(delta) = &self.last_delta {
            debug_assert_eq!(delta.relation.base(), delta.base.relation);
            debug_assert_eq!(delta.relation.target(), delta.target.relation);
            debug_assert_eq!(delta.target, self.state_root);
            debug_assert_eq!(
                delta.base.revision.checked_add(1),
                Some(delta.target.revision)
            );
        }
    }

    #[cfg(not(test))]
    const fn debug_assert_invariants(&self) {
        let _ = self.state_root;
    }
}

fn checked_u64(value: usize) -> Result<u64, DaemonError> {
    u64::try_from(value).map_err(|_| DaemonError::Accounting)
}

fn project_retained_bytes(
    total: usize,
    before: Option<&PendingAttemptValue>,
    after: Option<&PendingAttemptValue>,
) -> Result<usize, DaemonError> {
    let before = before
        .map(|value| usize::try_from(value.retained_bytes))
        .transpose()
        .map_err(|_| DaemonError::Accounting)?
        .unwrap_or(0);
    let after = after
        .map(|value| usize::try_from(value.retained_bytes))
        .transpose()
        .map_err(|_| DaemonError::Accounting)?
        .unwrap_or(0);
    total
        .checked_sub(before)
        .and_then(|value| value.checked_add(after))
        .ok_or(DaemonError::Accounting)
}

fn project_count(
    count: usize,
    before: Option<&PendingAttemptValue>,
    after: Option<&PendingAttemptValue>,
) -> Result<usize, DaemonError> {
    let count = if before.is_some() && after.is_none() {
        count.checked_sub(1).ok_or(DaemonError::Accounting)?
    } else if before.is_none() && after.is_some() {
        count.checked_add(1).ok_or(DaemonError::Accounting)?
    } else {
        count
    };
    Ok(count)
}

fn checked_increment(value: u64) -> Result<u64, DaemonError> {
    value.checked_add(1).ok_or(DaemonError::Accounting)
}
