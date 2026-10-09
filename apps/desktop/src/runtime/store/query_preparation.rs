//! One UI-owned preparation retry lifecycle. An immediate preparation refusal
//! releases the ordinary read permit; only a visible reader keeps a retry.
//! One cancellable GPUI task owns adaptive delays and the original deadline.

use super::{OwnerAttachment, PageKey};
use crate::core::{QueryPreparation, VersionedRoot};
use crate::model::pages::{Generation, Stamp};
use crate::runtime::actor::CancellationToken;
use crate::runtime::reads::ReadRequest;
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub(super) const READ_BUDGET: Duration = crate::runtime::reads::READ_TIMEOUT;

#[derive(Clone, Copy, Debug)]
pub(super) struct Budget {
    started: Instant,
    retries: u8,
}

impl Budget {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started: now,
            retries: 0,
        }
    }
    fn deadline(self) -> Instant {
        self.started + READ_BUDGET
    }
    fn next(self, now: Instant) -> Option<Instant> {
        let delay = Duration::from_millis(250_u64 << self.retries.min(3));
        let due = now + delay;
        (due < self.deadline()).then_some(due)
    }
    pub(super) fn advanced(mut self) -> Self {
        self.retries = self.retries.saturating_add(1);
        self
    }
    pub(super) fn allows(self, now: Instant) -> bool {
        now < self.deadline()
    }
}

/// A check belongs to the exact landed generation, visible stamp, full
/// producer authority and admitted owner attachment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparationToken {
    pub(super) generation: Generation,
    pub(super) stamp: Stamp,
    pub(super) root: VersionedRoot,
    pub(super) owner: OwnerAttachment,
}

#[derive(Clone, Debug)]
pub(super) struct Attempt {
    pub(super) generation: Generation,
    pub(super) root: VersionedRoot,
    pub(super) owner: OwnerAttachment,
    pub(super) request: ReadRequest,
    pub(super) affinity: Option<usize>,
    pub(super) budget: Budget,
    pub(super) preparation: Option<QueryPreparation>,
    pub(super) cancel: CancellationToken,
    pub(super) deadline_expired: Arc<AtomicBool>,
    pending: Option<Stamp>,
    due: Option<Instant>,
    deadline_armed: bool,
}

impl Attempt {
    pub(super) fn new(
        generation: Generation,
        root: VersionedRoot,
        owner: OwnerAttachment,
        request: ReadRequest,
        affinity: Option<usize>,
        budget: Budget,
        preparation: Option<QueryPreparation>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            generation,
            root,
            owner,
            request,
            affinity,
            budget,
            preparation,
            cancel,
            deadline_expired: Arc::new(AtomicBool::new(false)),
            pending: None,
            due: None,
            deadline_armed: preparation.is_some(),
        }
    }
    fn token(&self) -> Option<PreparationToken> {
        Some(PreparationToken {
            generation: self.generation,
            stamp: self.pending?,
            root: self.root,
            owner: self.owner.clone(),
        })
    }
}

pub(super) enum Due {
    Retry(PageKey, PreparationToken),
    Deadline(PageKey, Attempt),
}

#[derive(Default)]
pub(super) struct PreparationReads(BTreeMap<PageKey, Attempt>);

impl PreparationReads {
    pub(super) fn record(&mut self, key: PageKey, attempt: Attempt) {
        self.0.insert(key, attempt);
    }
    pub(super) fn remove(&mut self, key: &PageKey) {
        self.0.remove(key);
    }
    pub(super) fn remove_generation(&mut self, key: &PageKey, generation: Generation) {
        if self.0.get(key).is_some_and(|attempt| attempt.generation == generation) {
            self.0.remove(key);
        }
    }
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }
    pub(super) fn keys(&self) -> Vec<PageKey> {
        self.0.keys().cloned().collect()
    }
    pub(super) fn awaiting(
        &mut self,
        key: &PageKey,
        generation: Generation,
        stamp: Stamp,
        preparation: QueryPreparation,
        now: Instant,
    ) {
        if let Some(attempt) = self
            .0
            .get_mut(key)
            .filter(|attempt| attempt.generation == generation)
        {
            attempt.pending = Some(stamp);
            attempt.preparation = Some(preparation);
            attempt.deadline_armed = true;
            attempt.due = attempt.budget.next(now);
        }
    }
    /// The original budget has expired. Keep the exact manual check, with no
    /// further timer; no cancellation or refusal silently renews the deadline.
    pub(super) fn exhausted(&mut self, key: PageKey, mut attempt: Attempt, stamp: Stamp) {
        attempt.pending = Some(stamp);
        attempt.due = None;
        attempt.deadline_armed = false;
        self.record(key, attempt);
    }

    pub(super) fn awaiting_terminal(&mut self, key: PageKey, mut attempt: Attempt) {
        attempt.due = None;
        attempt.deadline_armed = false;
        self.record(key, attempt);
    }
    pub(super) fn expired_result(
        &self,
        key: &PageKey,
        generation: Generation,
    ) -> Option<QueryPreparation> {
        let attempt = self.0.get(key)?;
        (attempt.generation == generation && attempt.deadline_expired.load(Ordering::Acquire))
            .then_some(attempt.preparation)
            .flatten()
    }

    pub(super) fn deadline_cancellations(
        &self,
    ) -> Vec<(Instant, Arc<AtomicBool>, CancellationToken)> {
        self.0
            .values()
            .filter(|attempt| attempt.deadline_armed)
            .map(|attempt| {
                (
                    attempt.budget.deadline(),
                    attempt.deadline_expired.clone(),
                    attempt.cancel.clone(),
                )
            })
            .collect()
    }

    pub(super) fn token(&self, key: &PageKey) -> Option<PreparationToken> {
        self.0.get(key)?.token()
    }
    pub(super) fn take(&mut self, key: &PageKey, token: &PreparationToken) -> Option<Attempt> {
        if self.token(key).as_ref() != Some(token) {
            return None;
        }
        self.0.remove(key)
    }
    pub(super) fn next_delay(&self, now: Instant) -> Option<Duration> {
        self.0
            .values()
            .flat_map(|attempt| {
                [
                    attempt.due,
                    attempt.deadline_armed.then(|| attempt.budget.deadline()),
                ]
            })
            .flatten()
            .min()
            .map(|due| due.saturating_duration_since(now))
    }
    pub(super) fn due(&mut self, now: Instant) -> Vec<Due> {
        self.due_at(now)
    }
    fn due_at(&mut self, now: Instant) -> Vec<Due> {
        let expired = self
            .0
            .iter()
            .filter(|(_, attempt)| attempt.deadline_armed && !attempt.budget.allows(now))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut events = expired
            .into_iter()
            .filter_map(|key| {
                self.0
                    .remove(&key)
                    .map(|attempt| Due::Deadline(key, attempt))
            })
            .collect::<Vec<_>>();
        events.extend(self.0.iter_mut().filter_map(|(key, attempt)| {
            if !attempt.due.is_some_and(|due| due <= now) {
                return None;
            }
            attempt.due = None;
            attempt.token().map(|token| Due::Retry(key.clone(), token))
        }));
        events
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use crate::model::pages::{PageStore, ReadFailure, SearchQuery};

    fn prepared() -> (PageKey, Attempt, Stamp) {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("query".into(), "owner".into())]),
            1,
        );
        let Some(query) = SearchQuery::new("RelationLabel", 50) else { panic!("the authored query must parse"); };
        let key = PageKey::Search(query.clone());
        let preparation = QueryPreparation {
            basis: root.root().into(),
            state: backend_library::QueryPreparationState::Retiring,
        };
        let Some(owner) = super::super::OwnerLink::serving().current_attachment() else { panic!("the authored owner must be serving"); };
        let mut pages = PageStore::default();
        let Ok(Some(generation)) = pages.begin(&key, root) else { panic!("the authored read must mint a generation"); };
        pages.land(
            &key,
            generation,
            Err(ReadFailure::QueryPreparation(preparation)),
        );
        let attempt = Attempt::new(
            generation,
            root,
            owner,
            ReadRequest::Search(query),
            None,
            Budget::new(Instant::now()),
            Some(preparation),
            CancellationToken::new(),
        );
        let stamp = pages.stamp(&key);
        (key, attempt, stamp)
    }

    #[test]
    fn adaptive_retries_continue_near_the_original_deadline_without_renewing_it() {
        let mut budget = Budget::new(Instant::now());
        let first = budget.started;
        let mut now = first;
        let mut due = Vec::new();
        while let Some(next) = budget.next(now) {
            due.push(next.duration_since(first));
            budget = budget.advanced();
            now = next;
            assert_eq!(budget.started, first);
        }
        assert_eq!(due.len(), 17, "no arbitrary early cap");
        assert_eq!(due.last(), Some(&Duration::from_millis(29_750)));
        assert!(budget.allows(first + Duration::from_millis(29_999)));
        assert!(!budget.allows(first + READ_BUDGET));
        assert!(budget.next(first + READ_BUDGET).is_none());
    }

    #[test]
    fn original_deadline_cancels_running_retry_but_keeps_a_manual_check() {
        let (key, mut attempt, stamp) = prepared();
        let deadline = attempt.budget.deadline();
        attempt.pending = None; // retry really owns a running generation
        let mut reads = PreparationReads::default();
        reads.record(key.clone(), attempt);
        assert!(reads.due_at(deadline - Duration::from_millis(1)).is_empty());
        let events = reads.due_at(deadline);
        assert_eq!(events.len(), 1);
        let Some(Due::Deadline(expired_key, attempt)) = events.into_iter().next() else {
            panic!("deadline owns cancellation")
        };
        assert_eq!(expired_key, key);
        reads.exhausted(key.clone(), attempt, stamp);
        assert!(reads.token(&key).is_some());
        assert!(reads.next_delay(Instant::now()).is_none());
        assert!(reads.due_at(deadline + READ_BUDGET).is_empty());
    }

    #[test]
    fn late_success_and_failure_remove_the_deadline_and_stale_checks_cannot_take_a_successor() {
        let (key, mut attempt, stamp) = prepared();
        let mut reads = PreparationReads::default();
        reads.record(key.clone(), attempt.clone());
        let Some(preparation) = attempt.preparation else { panic!("the authored refusal must be typed"); };
        reads.awaiting(&key, attempt.generation, stamp, preparation, Instant::now());
        let Some(old) = reads.token(&key) else { panic!("the pending refusal must own a token"); };
        let deadline = attempt.budget.deadline();
        reads.remove(&key); // a landed success or real failure terminates ownership
        assert!(reads.due_at(deadline - Duration::from_millis(1)).is_empty());
        assert!(reads.next_delay(Instant::now()).is_none());
        attempt.root = attempt.root.with_generation(2);
        attempt.affinity = Some(1);
        reads.record(key.clone(), attempt.clone());
        reads.awaiting(&key, attempt.generation, stamp, preparation, Instant::now());
        let Some(current) = reads.token(&key) else { panic!("the replacement must own a token"); };
        assert!(reads.take(&key, &old).is_none());
        assert_eq!(reads.token(&key), Some(current.clone()));
        assert_eq!(reads.take(&key, &current).map(|attempt| attempt.affinity), Some(Some(1)));
    }

    #[test]
    fn a_refused_admission_removes_only_its_exact_attempt_generation() {
        let (key, mut attempt, _) = prepared();
        let old_generation = attempt.generation;
        let Some(new_generation) = Generation::new(99) else { panic!("the authored successor generation"); };
        assert_ne!(old_generation, new_generation);
        attempt.generation = new_generation;
        let mut reads = PreparationReads::default();
        reads.record(key.clone(), attempt);
        reads.remove_generation(&key, old_generation);
        assert_eq!(reads.0.get(&key).map(|attempt| attempt.generation), Some(new_generation));
        reads.remove_generation(&key, new_generation);
        assert!(reads.0.get(&key).is_none());
    }
}
