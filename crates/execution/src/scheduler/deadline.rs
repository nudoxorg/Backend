//! Bounded deadline ordering for runtime work.
//!
//! Rescheduling with a heap normally leaves one tombstone per update.  This
//! queue keeps the live key in a map and periodically rebuilds the heap, so a
//! cancellation or reschedule storm cannot turn stale entries into an
//! unbounded allocation.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeadlineItem<K> {
    deadline: u64,
    key: K,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiveDeadline {
    deadline: u64,
    generation: u64,
}

/// Failure to allocate another stable ordering generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineQueueError {
    /// The queue's monotonic generation exhausted its representable range.
    GenerationOverflow,
}

impl std::fmt::Display for DeadlineQueueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GenerationOverflow => formatter.write_str("deadline generation overflowed"),
        }
    }
}

impl std::error::Error for DeadlineQueueError {}

/// A keyed deadline queue with bounded stale heap entries.
pub struct DeadlineQueue<K: Ord + Copy> {
    heap: BinaryHeap<Reverse<DeadlineItem<K>>>,
    live: BTreeMap<K, LiveDeadline>,
    next_generation: u64,
}

impl<K: Ord + Copy> Default for DeadlineQueue<K> {
    fn default() -> Self {
        Self {
            heap: BinaryHeap::new(),
            live: BTreeMap::new(),
            next_generation: 0,
        }
    }
}

impl<K: Ord + Copy> DeadlineQueue<K> {
    /// Creates an empty deadline queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedules or replaces one key's deadline.
    ///
    /// # Errors
    ///
    /// Returns [`DeadlineQueueError::GenerationOverflow`] when the queue's
    /// monotonic generation cannot advance.
    pub fn schedule(&mut self, key: K, deadline: u64) -> Result<(), DeadlineQueueError> {
        let generation = self
            .next_generation
            .checked_add(1)
            .ok_or(DeadlineQueueError::GenerationOverflow)?;
        self.next_generation = generation;
        self.live.insert(
            key,
            LiveDeadline {
                deadline,
                generation,
            },
        );
        self.heap.push(Reverse(DeadlineItem {
            deadline,
            key,
            generation,
        }));
        self.reclaim_stale();
        Ok(())
    }

    /// Cancels one key, returning whether it was live.
    pub fn cancel(&mut self, key: K) -> bool {
        let removed = self.live.remove(&key).is_some();
        self.reclaim_stale();
        removed
    }

    /// Removes and returns the next live key whose deadline has elapsed.
    pub fn pop_due(&mut self, now: u64) -> Option<K> {
        loop {
            let Reverse(item) = self.heap.peek().copied()?;
            let current = self.live.get(&item.key).copied();
            if current.is_none_or(|entry| entry.generation != item.generation) {
                self.heap.pop();
                continue;
            }
            if item.deadline > now {
                return None;
            }
            self.heap.pop();
            self.live.remove(&item.key);
            self.reclaim_stale();
            return Some(item.key);
        }
    }

    /// Number of live keys currently tracked.
    #[must_use]
    pub fn live_len(&self) -> usize {
        self.live.len()
    }

    /// Number of heap entries, including any stale entries awaiting cleanup.
    #[must_use]
    pub fn heap_len(&self) -> usize {
        self.heap.len()
    }

    /// Rebuilds the heap whenever tombstones exceed a small multiple of live
    /// entries. This bound remains meaningful for an empty queue as well.
    fn reclaim_stale(&mut self) {
        if self.live.is_empty() {
            self.heap.clear();
            return;
        }
        let bound = self.live.len().saturating_mul(2).saturating_add(1);
        if self.heap.len() <= bound {
            return;
        }
        self.heap = self
            .live
            .iter()
            .map(|(&key, &entry)| {
                Reverse(DeadlineItem {
                    deadline: entry.deadline,
                    key,
                    generation: entry.generation,
                })
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::DeadlineQueue;

    #[test]
    fn reschedule_and_cancel_churn_keeps_heap_bounded() {
        let mut queue = DeadlineQueue::new();
        for round in 0..10_000_u64 {
            assert!(queue.schedule(round % 17, round).is_ok());
            if round % 3 == 0 {
                queue.cancel((round / 3) % 17);
            }
            assert!(queue.heap_len() <= queue.live_len() * 2 + 1);
        }
        while queue.pop_due(u64::MAX).is_some() {}
        assert_eq!(queue.live_len(), 0);
        assert!(queue.heap_len() <= 1);
    }
}
