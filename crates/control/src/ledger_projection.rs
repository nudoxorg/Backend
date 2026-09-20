//! Output-sensitive scheduler projections.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::AgentWorkKey;
use crate::record::{WorkRecord, WorkStatus};

/// Output-sensitive projections maintained alongside the canonical relation.
/// The relation root remains the authority; these indexes are derived from
/// each exact change set and are rebuilt only once when a state is recovered.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ProjectionIndex {
    ready: BTreeSet<crate::record::RecordKey>,
    active: BTreeSet<crate::record::RecordKey>,
    expiry: BTreeMap<u64, BTreeSet<crate::record::RecordKey>>,
    dependents: BTreeMap<AgentWorkKey, BTreeSet<crate::record::RecordKey>>,
}

impl ProjectionIndex {
    fn add(&mut self, key: crate::record::RecordKey, record: &WorkRecord) {
        if matches!(record.status(), WorkStatus::Ready | WorkStatus::Queued) {
            self.ready.insert(key);
        }
        if record.status().is_active() {
            self.active.insert(key);
        }
        if record.status().has_expiring_lease()
            && let Some(attempt) = record.attempt_data()
        {
            self.expiry
                .entry(attempt.expires_at())
                .or_default()
                .insert(key);
        }
        for dependency in record.spec().dependencies() {
            self.dependents.entry(dependency).or_default().insert(key);
        }
    }

    fn remove(&mut self, key: crate::record::RecordKey, record: &WorkRecord) {
        self.ready.remove(&key);
        self.active.remove(&key);
        if let Some(attempt) = record.attempt_data()
            && let Some(keys) = self.expiry.get_mut(&attempt.expires_at())
        {
            keys.remove(&key);
            if keys.is_empty() {
                self.expiry.remove(&attempt.expires_at());
            }
        }
        for dependency in record.spec().dependencies() {
            if let Some(keys) = self.dependents.get_mut(&dependency) {
                keys.remove(&key);
                if keys.is_empty() {
                    self.dependents.remove(&dependency);
                }
            }
        }
    }

    pub(super) fn apply_change(
        &mut self,
        key: crate::record::RecordKey,
        before: Option<&WorkRecord>,
        after: Option<&WorkRecord>,
    ) {
        if let Some(before) = before {
            self.remove(key, before);
        }
        if let Some(after) = after {
            self.add(key, after);
        }
    }

    pub(super) fn active_count(&self) -> usize {
        self.active.len()
    }

    pub(super) fn ready_count(&self) -> usize {
        self.ready.len()
    }

    pub(super) fn expired_keys(
        &self,
        now: u64,
    ) -> impl Iterator<Item = crate::record::RecordKey> + '_ {
        self.expiry
            .range(..=now)
            .flat_map(|(_, keys)| keys.iter().copied())
    }

    pub(super) fn dependent_keys(
        &self,
        dependency: AgentWorkKey,
    ) -> impl Iterator<Item = crate::record::RecordKey> + '_ {
        self.dependents
            .get(&dependency)
            .into_iter()
            .flat_map(|keys| keys.iter().copied())
    }
}
