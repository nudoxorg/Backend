//! Bounded pin leases and indexed retention fences.

use crate::{FlowError, Frontier, Pin, Time};
use std::{collections::BTreeMap, mem::size_of};

const DEFAULT_PIN_LIMIT: usize = 4_096;
const DEFAULT_PIN_BYTE_LIMIT: usize = DEFAULT_PIN_LIMIT * size_of::<Time>();

#[derive(Clone, Debug)]
struct PinRecord {
    frontier: Frontier,
    expiry: Option<Time>,
}

/// Bounded registry for observation leases.
///
/// Singleton frontiers are the common path. Their component-wise minimum is
/// maintained by two ordered count maps, so the retention check is O(1).
/// Recursive antichains retain their exact fences and use a bounded fallback
/// scan; the registry capacity prevents that path from growing without limit.
#[derive(Clone, Debug)]
pub(crate) struct PinRegistry {
    records: BTreeMap<Pin, PinRecord>,
    frontier_counts: BTreeMap<Vec<Time>, usize>,
    bytes: usize,
    epochs: BTreeMap<u64, usize>,
    iterations: BTreeMap<u16, usize>,
    singleton_count: usize,
    antichain_count: usize,
    min_singleton: Option<Time>,
    limit: usize,
    byte_limit: usize,
}

impl Default for PinRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PinRegistry {
    /// Creates a registry with a bounded default lease capacity.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            frontier_counts: BTreeMap::new(),
            bytes: 0,
            epochs: BTreeMap::new(),
            iterations: BTreeMap::new(),
            singleton_count: 0,
            antichain_count: 0,
            min_singleton: None,
            limit: DEFAULT_PIN_LIMIT,
            byte_limit: DEFAULT_PIN_BYTE_LIMIT,
        }
    }

    pub(crate) fn set_limit(&mut self, limit: usize) -> Result<(), FlowError> {
        if limit == 0 || limit < self.records.len() {
            return Err(FlowError::PinCapacity);
        }
        self.limit = limit;
        Ok(())
    }

    pub(crate) fn set_byte_limit(&mut self, limit: usize) -> Result<(), FlowError> {
        if limit == 0 || limit < self.bytes {
            return Err(FlowError::PinCapacity);
        }
        self.byte_limit = limit;
        Ok(())
    }

    #[must_use]
    pub(crate) const fn byte_limit(&self) -> usize {
        self.byte_limit
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub(crate) const fn bytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub(crate) fn admits(&self, frontier: &Frontier) -> bool {
        self.records.len() < self.limit
            && self
                .bytes
                .checked_add(frontier_bytes(frontier))
                .is_some_and(|bytes| bytes <= self.byte_limit)
    }

    pub(crate) fn insert(
        &mut self,
        pin: Pin,
        frontier: Frontier,
        expiry: Option<Time>,
    ) -> Result<(), FlowError> {
        let bytes = frontier_bytes(&frontier);
        let Some(next_bytes) = self.bytes.checked_add(bytes) else {
            return Err(FlowError::Overflow);
        };
        if self.records.len() >= self.limit || next_bytes > self.byte_limit {
            return Err(FlowError::PinCapacity);
        }
        if self.records.contains_key(&pin) {
            return Err(FlowError::InvalidPin);
        }
        self.add_index(&frontier);
        self.bytes = next_bytes;
        self.records.insert(pin, PinRecord { frontier, expiry });
        Ok(())
    }

    pub(crate) fn remove(&mut self, pin: Pin) -> bool {
        let Some(record) = self.records.remove(&pin) else {
            return false;
        };
        self.bytes = self.bytes.saturating_sub(frontier_bytes(&record.frontier));
        self.remove_index(&record.frontier);
        true
    }

    /// Removes leases whose checked expiry is at or before `now`.
    pub(crate) fn expire(&mut self, now: Time) -> usize {
        let expired = self
            .records
            .iter()
            .filter_map(|(pin, record)| {
                record
                    .expiry
                    .is_some_and(|expiry| expiry.less_equal(now))
                    .then_some(*pin)
            })
            .collect::<Vec<_>>();
        let count = expired.len();
        for pin in expired {
            let _ = self.remove(pin);
        }
        count
    }

    /// Returns whether the candidate since frontier would cross any lease.
    pub(crate) fn blocks(&self, candidate: &Frontier) -> bool {
        if self.records.is_empty() {
            return false;
        }
        if self.antichain_count == 0 {
            let Some(minimum) = self.min_singleton else {
                return false;
            };
            return candidate
                .elements()
                .iter()
                .any(|time| !time.less_equal(minimum));
        }
        self.records.values().any(|record| {
            candidate
                .elements()
                .iter()
                .any(|time| !record.frontier.allows_time(*time))
        })
    }

    fn add_index(&mut self, frontier: &Frontier) {
        let key = frontier.elements().to_vec();
        *self.frontier_counts.entry(key).or_default() += 1;
        if frontier.elements().len() == 1 {
            self.singleton_count += 1;
            let time = frontier.elements()[0];
            *self.epochs.entry(time.epoch.0).or_default() += 1;
            *self.iterations.entry(time.iteration).or_default() += 1;
            self.refresh_minimum();
        } else {
            self.antichain_count += 1;
        }
    }

    fn remove_index(&mut self, frontier: &Frontier) {
        let key = frontier.elements().to_vec();
        decrement(&mut self.frontier_counts, &key);
        if frontier.elements().len() == 1 {
            self.singleton_count = self.singleton_count.saturating_sub(1);
            let time = frontier.elements()[0];
            decrement(&mut self.epochs, &time.epoch.0);
            decrement(&mut self.iterations, &time.iteration);
            self.refresh_minimum();
        } else {
            self.antichain_count = self.antichain_count.saturating_sub(1);
        }
    }

    fn refresh_minimum(&mut self) {
        self.min_singleton = if self.singleton_count == 0 {
            None
        } else {
            let epoch = self.epochs.first_key_value().map(|(epoch, _)| *epoch);
            let iteration = self
                .iterations
                .first_key_value()
                .map(|(iteration, _)| *iteration);
            epoch
                .zip(iteration)
                .map(|(epoch, iteration)| Time::new(crate::Epoch(epoch), iteration))
        };
    }
}

fn decrement<K: Ord>(counts: &mut BTreeMap<K, usize>, key: &K) {
    let remove = if let Some(count) = counts.get_mut(key) {
        *count = count.saturating_sub(1);
        *count == 0
    } else {
        false
    };
    if remove {
        counts.remove(key);
    }
}

fn frontier_bytes(frontier: &Frontier) -> usize {
    frontier.elements().len().saturating_mul(size_of::<Time>())
}
