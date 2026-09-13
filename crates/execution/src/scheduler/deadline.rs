//! Fixed-memory deadline ordering for runtime work.

use std::collections::TryReserveError;

const DEFAULT_DEADLINE_CAPACITY: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DeadlineItem<K> {
    deadline: u64,
    key: K,
}

/// Failure to construct or extend a bounded deadline queue.
#[derive(Debug)]
pub enum DeadlineQueueError {
    /// A deadline queue cannot have zero live entries.
    ZeroCapacity,
    /// Reserving the complete fixed backing allocation failed.
    Allocation(TryReserveError),
    /// A new key would exceed the configured live-deadline bound.
    Capacity,
}

impl std::fmt::Display for DeadlineQueueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroCapacity => formatter.write_str("deadline capacity must be non-zero"),
            Self::Allocation(_) => formatter.write_str("deadline queue allocation failed"),
            Self::Capacity => formatter.write_str("deadline queue capacity is exhausted"),
        }
    }
}

impl std::error::Error for DeadlineQueueError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::ZeroCapacity | Self::Capacity => None,
        }
    }
}

/// Deadline queue whose complete backing allocation is reserved at construction.
///
/// There is exactly one row per live key: rescheduling replaces it and cancellation removes it.
/// The queue therefore remains bounded under arbitrary reschedule and cancellation churn.
pub struct DeadlineQueue<K: Ord + Copy> {
    entries: Vec<DeadlineItem<K>>,
    capacity: usize,
}

impl<K: Ord + Copy> Default for DeadlineQueue<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord + Copy> DeadlineQueue<K> {
    /// Creates a queue with the scheduler's fixed default live-deadline limit.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(DEFAULT_DEADLINE_CAPACITY),
            capacity: DEFAULT_DEADLINE_CAPACITY,
        }
    }

    /// Reserves all storage for an explicit live-deadline limit.
    ///
    /// # Errors
    ///
    /// Returns [`DeadlineQueueError::ZeroCapacity`] for zero or
    /// [`DeadlineQueueError::Allocation`] when the fixed reservation cannot be made.
    pub fn with_capacity(capacity: usize) -> Result<Self, DeadlineQueueError> {
        if capacity == 0 {
            return Err(DeadlineQueueError::ZeroCapacity);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(DeadlineQueueError::Allocation)?;
        Ok(Self { entries, capacity })
    }

    /// Schedules or replaces one key's deadline without allocating.
    ///
    /// # Errors
    ///
    /// Returns [`DeadlineQueueError::Capacity`] when every admitted slot belongs to another key.
    pub fn schedule(&mut self, key: K, deadline: u64) -> Result<(), DeadlineQueueError> {
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            self.entries.remove(index);
        } else if self.entries.len() == self.capacity {
            return Err(DeadlineQueueError::Capacity);
        }
        let item = DeadlineItem { deadline, key };
        let index = self.entries.partition_point(|entry| *entry <= item);
        self.entries.insert(index, item);
        Ok(())
    }

    /// Cancels one key, immediately reclaiming its live slot.
    pub fn cancel(&mut self, key: K) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.key == key) else {
            return false;
        };
        self.entries.remove(index);
        true
    }

    /// Removes and returns the next live key whose deadline has elapsed.
    pub fn pop_due(&mut self, now: u64) -> Option<K> {
        if self.entries.first()?.deadline > now {
            return None;
        }
        Some(self.entries.remove(0).key)
    }

    /// Number of live keys currently tracked.
    #[must_use]
    pub fn live_len(&self) -> usize {
        self.entries.len()
    }

    /// Number of allocated deadline rows. Kept for compatibility with existing diagnostics.
    #[must_use]
    pub fn heap_len(&self) -> usize {
        self.entries.len()
    }

    /// Maximum number of live deadlines.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::{DeadlineQueue, DeadlineQueueError};

    #[test]
    fn reschedule_and_cancel_churn_keeps_storage_bounded() -> Result<(), DeadlineQueueError> {
        let mut queue = DeadlineQueue::with_capacity(17)?;
        for round in 0..10_000_u64 {
            queue.schedule(round % 17, round)?;
            if round % 3 == 0 {
                queue.cancel((round / 3) % 17);
            }
            assert!(queue.live_len() <= queue.capacity());
            assert_eq!(queue.heap_len(), queue.live_len());
        }
        while queue.pop_due(u64::MAX).is_some() {}
        assert_eq!(queue.live_len(), 0);
        Ok(())
    }

    #[test]
    fn a_new_key_fails_synchronously_at_capacity() -> Result<(), DeadlineQueueError> {
        let mut queue = DeadlineQueue::with_capacity(1)?;
        queue.schedule(1_u64, 10)?;
        assert!(matches!(
            queue.schedule(2, 11),
            Err(DeadlineQueueError::Capacity)
        ));
        queue.schedule(1, 12)?;
        assert_eq!(queue.pop_due(11), None);
        assert_eq!(queue.pop_due(12), Some(1));
        Ok(())
    }
}
