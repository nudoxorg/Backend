//! Deterministic workload helpers for the performance contract suite.
//!
//! The integration tests and the executable harness share only fixture and
//! reporting helpers. Their expected values are computed by independent
//! models in the test and benchmark code, so a production root or counter
//! cannot define its own correctness oracle.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use backend_store::{OrderedMap, StoreError, StoredValue};

/// A small deterministic generator used for reproducible workload classes.
#[derive(Clone, Copy, Debug)]
pub struct Lcg {
    state: u64,
}

impl Lcg {
    /// Creates a generator with a fixed nonzero seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    /// Returns the next deterministic 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    /// Returns a deterministic value in `0..upper`, or zero for an empty
    /// range.
    #[must_use]
    pub fn below(&mut self, upper: usize) -> usize {
        if upper == 0 {
            return 0;
        }
        let upper = u64::try_from(upper).unwrap_or(u64::MAX);
        usize::try_from(self.next_u64() % upper).unwrap_or_default()
    }
}

/// Returns a fixed-width ordered key for a fixture row.
#[must_use]
pub fn key(index: usize) -> Vec<u8> {
    format!("k{index:08}").into_bytes()
}

/// Returns a complete value version for a fixture row.
#[must_use]
pub fn value(seed: u64) -> StoredValue {
    StoredValue::new(seed.to_le_bytes().to_vec(), 1, vec![])
}

/// Builds a canonical map fixture with unique ordered logical keys.
///
/// # Errors
///
/// Returns the store error raised when a fixture key or value cannot be
/// admitted by the canonical map.
pub fn map_fixture(rows: usize) -> Result<OrderedMap, StoreError> {
    OrderedMap::try_from_iter(
        (0..rows).map(|index| (key(index), value(u64::try_from(index).unwrap_or(u64::MAX)))),
    )
}

/// Returns the visible map entries in independent, owned form for comparisons.
#[must_use]
pub fn map_entries(map: &OrderedMap) -> Vec<(Vec<u8>, StoredValue)> {
    map.iter()
        .map(|(entry_key, entry_value)| (entry_key.clone(), entry_value.clone()))
        .collect()
}

/// Returns a percentile using nearest-rank indexing over deterministic samples.
#[must_use]
pub fn percentile(mut samples: Vec<u128>, percentile: usize) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    samples[(samples.len() * percentile / 100).min(samples.len() - 1)]
}
