//! Correlation and deadline index for owner-retained remote dispatch tickets.
//!
//! A remote completion is routed by its exact work key, while expiry is driven
//! by a generation-safe min-heap. Heap entries are allowed to become stale on
//! replacement/removal; the live map and exact `(deadline, generation)` token
//! check discard them in bounded amortized time without scanning product
//! payloads.

use super::{PendingProduct, PendingRemoteKey};
use backend_engine::WorkKey;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

const MAX_PENDING_TICKETS: usize = 1_024;
const DEADLINE_HEAP_FACTOR: usize = 4;
const DEADLINE_HEAP_HEADROOM: usize = 32;

#[derive(Debug)]
pub(super) struct PendingIndex<T = PendingProduct> {
    by_key: BTreeMap<PendingRemoteKey, T>,
    by_work: BTreeMap<WorkKey, PendingRemoteKey>,
    work_by_key: BTreeMap<PendingRemoteKey, WorkKey>,
    deadlines: BinaryHeap<Reverse<(Instant, PendingRemoteKey, u64)>>,
    current_deadlines: BTreeMap<PendingRemoteKey, (Instant, u64)>,
    next_generation: u64,
}

impl<T> Default for PendingIndex<T> {
    fn default() -> Self {
        Self {
            by_key: BTreeMap::new(),
            by_work: BTreeMap::new(),
            work_by_key: BTreeMap::new(),
            deadlines: BinaryHeap::new(),
            current_deadlines: BTreeMap::new(),
            next_generation: 0,
        }
    }
}

impl<T> PendingIndex<T> {
    pub(super) fn can_accept(&self, work_key: WorkKey) -> bool {
        self.by_work.contains_key(&work_key) || self.by_key.len() < MAX_PENDING_TICKETS
    }

    pub(super) fn insert(
        &mut self,
        key: PendingRemoteKey,
        work_key: WorkKey,
        deadline: Instant,
        pending: T,
    ) -> bool {
        if !self.can_accept(work_key) {
            return false;
        }
        let Some(generation) = self.next_generation.checked_add(1) else {
            // The index cannot safely distinguish a reused timer token after
            // the fixed-width generation space is exhausted.  Refuse the
            // admission instead of allowing an old timer to expire a newer
            // ticket.
            return false;
        };
        self.next_generation = generation;
        if self.by_key.insert(key, pending).is_some() {
            // A key replacement can carry a new work identity. The direct
            // reverse map makes this removal logarithmic instead of scanning
            // every live ticket.
            if let Some(previous_work) = self.work_by_key.remove(&key)
                && self.by_work.get(&previous_work).copied() == Some(key)
            {
                self.by_work.remove(&previous_work);
            }
        }
        self.work_by_key.insert(key, work_key);
        if let Some(previous_key) = self.by_work.insert(work_key, key)
            && previous_key != key
        {
            // One work key has at most one live remote attempt. Retire the
            // replaced product metadata before a new result can be admitted.
            let _ = self.remove(previous_key);
        }
        self.current_deadlines.insert(key, (deadline, generation));
        self.deadlines.push(Reverse((deadline, key, generation)));
        self.rebuild_deadlines_if_needed();
        true
    }

    pub(super) fn remove(&mut self, key: PendingRemoteKey) -> Option<T> {
        let pending = self.by_key.remove(&key)?;
        self.current_deadlines.remove(&key);
        if let Some(work_key) = self.work_by_key.remove(&key)
            && self.by_work.get(&work_key).copied() == Some(key)
        {
            self.by_work.remove(&work_key);
        }
        self.rebuild_deadlines_if_needed();
        Some(pending)
    }

    pub(super) fn remove_for_work(&mut self, work_key: WorkKey) -> Option<T> {
        let key = self.by_work.get(&work_key).copied()?;
        self.remove(key)
    }

    pub(super) fn len(&self) -> usize {
        self.by_key.len()
    }

    pub(super) fn get(&self, key: PendingRemoteKey) -> Option<&T> {
        self.by_key.get(&key)
    }

    pub(super) fn keys(&self) -> Vec<PendingRemoteKey> {
        self.by_key.keys().copied().collect()
    }

    fn rebuild_deadlines_if_needed(&mut self) {
        let live = self.by_key.len();
        if live == 0 {
            self.deadlines.clear();
            return;
        }
        let limit = live
            .saturating_mul(DEADLINE_HEAP_FACTOR)
            .saturating_add(DEADLINE_HEAP_HEADROOM);
        if self.deadlines.len() <= limit {
            return;
        }
        self.deadlines = self
            .current_deadlines
            .iter()
            .map(|(key, (deadline, generation))| Reverse((*deadline, *key, *generation)))
            .collect();
    }

    /// Removes all live tickets in correlation order and resets stale heap
    /// entries. Callers own fallback/recovery of the returned product plans.
    #[cfg(test)]
    pub(super) fn take_all(&mut self) -> Vec<(PendingRemoteKey, T)> {
        self.by_work.clear();
        self.work_by_key.clear();
        self.deadlines.clear();
        self.current_deadlines.clear();
        std::mem::take(&mut self.by_key).into_iter().collect()
    }

    /// Pops one due live ticket. Stale heap records are discarded until the
    /// earliest live generation is found, so a replaced attempt cannot expire
    /// its successor.
    #[cfg(test)]
    pub(super) fn pop_expired(&mut self, now: Instant) -> Option<(PendingRemoteKey, T)> {
        loop {
            let Reverse((deadline, key, generation)) = self.deadlines.peek().copied()?;
            if deadline > now {
                return None;
            }
            let _ = self.deadlines.pop();
            if self.current_deadlines.get(&key) != Some(&(deadline, generation)) {
                continue;
            }
            return self.remove(key).map(|pending| (key, pending));
        }
    }

    /// Returns the earliest due live key without releasing its product
    /// metadata. The caller removes it only after engine publication commits.
    pub(super) fn next_expired(&mut self, now: Instant) -> Option<PendingRemoteKey> {
        loop {
            let Reverse((deadline, key, generation)) = self.deadlines.peek().copied()?;
            if deadline > now {
                return None;
            }
            if self.current_deadlines.get(&key) == Some(&(deadline, generation)) {
                return Some(key);
            }
            let _ = self.deadlines.pop();
        }
    }

    #[cfg(test)]
    pub(super) fn deadline_len(&self) -> usize {
        self.deadlines.len()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use backend_engine::{
        AttemptId, AuthorityVersion, OutputEquivalence, ReadManifestId, RecipeId,
        VersionedWorkIdentity,
    };

    fn key(number: u64) -> (PendingRemoteKey, WorkKey) {
        let relation = backend_engine::product_source_fixture(false)
            .unwrap_or_else(|_| panic!("test relation should be canonical"));
        let bytes = number.to_be_bytes();
        let identity = VersionedWorkIdentity::new(
            RecipeId::from_value(&bytes),
            relation.root(),
            ReadManifestId::from_value(&bytes),
            AuthorityVersion::from_value(&bytes),
            OutputEquivalence::from_value(&bytes),
        );
        let work_key = identity.work_key();
        let attempt = AttemptId::new(number.saturating_add(1)).unwrap_or_else(|_| {
            // The test range is strictly positive and bounded, so this branch
            // is unreachable for the values below; return a valid fallback if
            // a future identifier policy changes.
            AttemptId::new(1).unwrap_or_else(|_| panic!("test attempt id policy"))
        });
        (PendingRemoteKey::new(work_key, attempt), work_key)
    }

    #[test]
    fn routes_many_out_of_order_completions_without_scanning_payloads() {
        let start = Instant::now();
        let mut index = PendingIndex::<u64>::default();
        let mut keys = Vec::new();
        for number in 1..=128_u64 {
            let (key, work_key) = key(number);
            keys.push((key, work_key));
            assert!(index.insert(key, work_key, start + Duration::from_secs(number), number));
        }
        assert_eq!(index.len(), 128);
        for number in (1..=128_u64).rev() {
            let (_, work_key) = keys[usize::try_from(number - 1).unwrap_or(0)];
            assert_eq!(index.remove_for_work(work_key), Some(number));
        }
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn stale_deadline_generation_cannot_expire_replacement() {
        let start = Instant::now();
        let mut index = PendingIndex::<u64>::default();
        let (key, work_key) = key(1);
        assert!(index.insert(key, work_key, start + Duration::from_secs(1), 10));
        assert!(index.insert(key, work_key, start + Duration::from_secs(100), 20));
        assert!(index.pop_expired(start + Duration::from_secs(2)).is_none());
        assert_eq!(
            index.pop_expired(start + Duration::from_secs(101)),
            Some((key, 20))
        );
        assert!(
            index
                .pop_expired(start + Duration::from_secs(101))
                .is_none()
        );
    }

    #[test]
    fn equal_deadline_replacement_still_has_a_distinct_timer_token() {
        let start = Instant::now();
        let mut index = PendingIndex::<u64>::default();
        let (key, work_key) = key(1);
        let deadline = start + Duration::from_secs(1);
        assert!(index.insert(key, work_key, deadline, 10));
        assert!(index.insert(key, work_key, deadline, 20));
        assert_eq!(index.pop_expired(start), None);
        assert_eq!(index.pop_expired(deadline), Some((key, 20)));
    }

    #[test]
    fn deadline_heap_rebuild_bounds_replacement_churn() {
        let start = Instant::now();
        let mut index = PendingIndex::<u64>::default();
        let (key, work_key) = key(1);
        for number in 1..=10_000_u64 {
            assert!(index.insert(key, work_key, start + Duration::from_secs(number), number,));
        }
        assert_eq!(index.len(), 1);
        assert!(index.deadline_len() <= DEADLINE_HEAP_FACTOR + DEADLINE_HEAP_HEADROOM);
    }

    #[test]
    fn deadline_heap_stays_bounded_through_reschedule_and_cancel_churn() {
        let start = Instant::now();
        let mut index = PendingIndex::<u64>::default();
        for number in 1..=100_000_u64 {
            let (key, work_key) = key(number);
            assert!(index.insert(key, work_key, start + Duration::from_secs(number), number,));
            assert_eq!(index.remove(key), Some(number));
            assert!(index.deadline_len() <= DEADLINE_HEAP_HEADROOM);
        }
        assert_eq!(index.len(), 0);
        assert_eq!(index.deadline_len(), 0);
    }

    #[test]
    fn disconnect_take_all_clears_reverse_and_deadline_indexes() {
        let start = Instant::now();
        let mut index = PendingIndex::<u64>::default();
        for number in 1..=4_u64 {
            let (key, work_key) = key(number);
            assert!(index.insert(key, work_key, start + Duration::from_secs(number), number));
        }
        let all = index.take_all();
        assert_eq!(all.len(), 4);
        assert_eq!(index.len(), 0);
        assert!(index.pop_expired(start + Duration::from_secs(10)).is_none());
    }
}
