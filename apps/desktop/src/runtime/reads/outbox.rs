//! Finished work the UI has not taken yet.
//!
//! Every entry belongs to one admitted read, and a read has at most one
//! entry: a newer partial page replaces an undelivered older one, and the
//! read's terminal outcome replaces its undelivered partial in place. The
//! outbox is therefore bounded by the read ledger rather than by a limit of
//! its own, so posting never blocks a worker and never refuses a terminal
//! outcome.
//!
//! Whatever posting or withdrawing displaces is handed back to the caller,
//! which drops it after the lock is released: freeing a large page, and
//! returning its capacity, never happens while a worker waits to post.

use super::permit::ReadId;
use super::{Delivery, Priority, ReadOutcome};
use crate::model::pages::PageKey;
use crate::runtime::actor::CancellationToken;
use std::collections::{BTreeSet, VecDeque};
use std::num::NonZeroUsize;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Undelivered outcomes, oldest first.
#[derive(Debug, Default)]
pub(super) struct Outbox {
    entries: Mutex<VecDeque<ReadOutcome>>,
}

/// One landing turn's outcomes and what still waits after them.
#[derive(Debug, Default)]
pub struct Batch {
    /// Outcomes to land now: reads a view waits on first, then prefetches,
    /// oldest first within each class.
    pub outcomes: Vec<ReadOutcome>,
    /// Outcomes still undelivered after this batch.
    pub remaining: usize,
}

/// Posting distinguishes a replacement (still posted) from a refusal.
/// The displaced value always returns to the caller for an unlocked drop.
#[derive(Debug)]
pub(super) struct Posted {
    pub(super) posted: bool,
    pub(super) unused: Option<ReadOutcome>,
}

impl Outbox {
    fn lock(&self) -> MutexGuard<'_, VecDeque<ReadOutcome>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues a partial page unless its read was cancelled. The cancellation
    /// is checked under the outbox lock: a canceller sets the token before it
    /// withdraws the read's outcomes, so no partial page of a cancelled read
    /// survives the cancel.
    ///
    /// Returns whether a page was posted and the outcome nobody will land
    /// (replaced or refused); the caller drops it unlocked.
    #[must_use]
    pub(super) fn post_partial(&self, outcome: ReadOutcome, cancel: &CancellationToken) -> Posted {
        debug_assert!(matches!(outcome.delivery, Delivery::Partial(_)));
        let mut entries = self.lock();
        if cancel.is_cancelled() {
            return Posted {
                posted: false,
                unused: Some(outcome),
            };
        }
        let terminal = entries
            .iter()
            .any(|entry| entry.permit.id() == outcome.permit.id() && entry.is_terminal());
        Posted {
            posted: !terminal,
            unused: place(&mut entries, outcome),
        }
    }

    /// Queues a read's terminal outcome, replacing its undelivered partial.
    /// A terminal outcome is never refused.
    ///
    /// Returns the replaced partial, which the caller drops unlocked.
    #[must_use]
    pub(super) fn post_terminal(&self, outcome: ReadOutcome) -> Option<ReadOutcome> {
        debug_assert!(matches!(outcome.delivery, Delivery::Terminal(_)));
        place(&mut self.lock(), outcome)
    }

    /// Takes one batch using the current visible keys, rather than the
    /// priority the worker observed at dispatch. The eighth slot of a full
    /// mixed batch advances a nonvisible prefetch, avoiding starvation.
    pub(super) fn take_for(&self, limit: NonZeroUsize, visible: &BTreeSet<PageKey>) -> Batch {
        let mut entries = self.lock();
        let mut outcomes = Vec::with_capacity(limit.get().min(entries.len()));
        while outcomes.len() < limit.get() && !entries.is_empty() {
            let prefetch_turn = limit.get() == 8 && outcomes.len() == 7;
            let preferred = entries.iter().position(|entry| {
                let foreground = entry.priority == Priority::Normal || visible.contains(&entry.key);
                if prefetch_turn {
                    !foreground
                } else {
                    foreground
                }
            });
            outcomes.extend(entries.remove(preferred.unwrap_or(0)));
        }
        Batch {
            outcomes,
            remaining: entries.len(),
        }
    }

    pub(super) fn take(&self, limit: NonZeroUsize) -> Batch {
        self.take_for(limit, &BTreeSet::new())
    }

    /// Capture exact started-read identities before cancellation callbacks
    /// can admit another generation of the same key.
    pub(super) fn ids(&self, matches: impl Fn(&ReadOutcome) -> bool) -> BTreeSet<ReadId> {
        self.lock()
            .iter()
            .filter(|outcome| matches(outcome))
            .map(|outcome| outcome.permit.id())
            .collect()
    }

    /// Removes every undelivered outcome `matches` selects. The caller drops
    /// them unlocked.
    #[must_use]
    pub(super) fn withdraw(&self, matches: impl Fn(&ReadOutcome) -> bool) -> Vec<ReadOutcome> {
        let mut entries = self.lock();
        let mut withdrawn = Vec::new();
        let mut index = 0;
        while index < entries.len() {
            if entries.get(index).is_some_and(&matches) {
                withdrawn.extend(entries.remove(index));
            } else {
                index += 1;
            }
        }
        withdrawn
    }

    /// Outcomes waiting for the UI.
    pub(super) fn len(&self) -> usize {
        self.lock().len()
    }

    /// Deterministic test barrier: block terminal publication while proving
    /// the worker still owns the queue's handoff lock.
    #[cfg(test)]
    pub(super) fn hold_for_test(&self) -> MutexGuard<'_, VecDeque<ReadOutcome>> {
        self.lock()
    }

    /// What is undelivered, for the schedule oracles.
    #[cfg(test)]
    pub(super) fn audit(&self) -> Vec<Undelivered> {
        self.lock()
            .iter()
            .map(|outcome| Undelivered {
                read: outcome.permit.id(),
                key: outcome.key.clone(),
                partial: matches!(outcome.delivery, Delivery::Partial(_)),
            })
            .collect()
    }
}

/// One undelivered outcome, as the schedule oracles see it.
#[cfg(test)]
#[derive(Debug)]
pub(super) struct Undelivered {
    pub(super) read: super::permit::ReadId,
    pub(super) key: crate::model::pages::PageKey,
    pub(super) partial: bool,
}

/// Puts `outcome` in its read's single entry. A read publishes partial pages
/// and then one terminal outcome, and the worker's terminal consumes the
/// read, so an entry already holding a terminal outcome can only meet a
/// stray: the terminal stays and the stray is returned.
fn place(entries: &mut VecDeque<ReadOutcome>, outcome: ReadOutcome) -> Option<ReadOutcome> {
    let read = outcome.permit.id();
    match entries.iter_mut().find(|entry| entry.permit.id() == read) {
        None => {
            entries.push_back(outcome);
            None
        }
        Some(entry) => match entry.delivery {
            Delivery::Partial(_) => Some(std::mem::replace(entry, outcome)),
            Delivery::Terminal(_) => Some(outcome),
        },
    }
}
