//! Retained support, distinct, and grouped aggregate kernels.

use crate::batch::consolidate_rows;
use crate::{Delta, FlowError, RowKey, Weight};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

/// Consolidates support by exact `(row key, value)` identity.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when support arithmetic exceeds `i64`.
pub fn reduce_rows<V: Clone + Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
) -> Result<BTreeMap<(RowKey, V), i64>, FlowError> {
    let mut out: BTreeMap<(RowKey, V), i64> = BTreeMap::new();
    for row in input {
        let key = (row.key, row.value);
        let diff = row.diff.value();
        match out.entry(key) {
            Entry::Vacant(entry) if diff != 0 => {
                entry.insert(diff);
            }
            Entry::Vacant(_) => {}
            Entry::Occupied(mut entry) => {
                let next = entry.get().checked_add(diff).ok_or(FlowError::Overflow)?;
                if next == 0 {
                    entry.remove();
                } else {
                    *entry.get_mut() = next;
                }
            }
        }
    }
    Ok(out)
}

/// Reduces rows sharing one row key. Conflicting values are rejected rather
/// than silently selecting one arbitrary payload.
///
/// # Errors
///
/// Returns [`FlowError::ConflictingReduceValue`] for distinct payloads under
/// one key or [`FlowError::Overflow`] for checked support arithmetic.
pub fn reduce_checked<V: Clone + Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
) -> Result<BTreeMap<RowKey, (V, i64)>, FlowError> {
    let mut out = BTreeMap::new();
    for row in input {
        if let Some((value, _)) = out.get(&row.key)
            && *value != row.value
        {
            return Err(FlowError::ConflictingReduceValue);
        }
        let key = row.key;
        let zero = {
            let entry = out.entry(key).or_insert((row.value.clone(), 0_i64));
            entry.1 = entry
                .1
                .checked_add(row.diff.value())
                .ok_or(FlowError::Overflow)?;
            entry.1 == 0
        };
        if zero {
            out.remove(&key);
        }
    }
    Ok(out)
}

/// Reduces rows sharing one row key.
///
/// # Errors
///
/// Returns [`FlowError::ConflictingReduceValue`] when one row key carries
/// distinct values, or another checked arithmetic error.
pub fn reduce<V: Clone + Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
) -> Result<BTreeMap<RowKey, (V, i64)>, FlowError> {
    reduce_checked(input)
}

/// Emits exact membership crossings for a stream of weighted updates.
///
/// # Errors
///
/// Returns [`FlowError`] when support arithmetic or consolidation fails.
pub fn distinct<V: Clone + Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
) -> Result<Vec<Delta<V>>, FlowError> {
    distinct_checked(input)
}

/// Checked distinct/set-membership maintenance.
///
/// # Errors
///
/// Returns [`FlowError::ZeroDiff`] for invalid input rows or
/// [`FlowError::Overflow`] during support consolidation.
pub fn distinct_checked<V: Clone + Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
) -> Result<Vec<Delta<V>>, FlowError> {
    let mut state = DistinctState::new();
    state.apply(input)
}

/// Retained support state for an incremental distinct operator.
#[derive(Clone, Debug, Default)]
pub struct DistinctState<V> {
    support: BTreeMap<(RowKey, V), i64>,
}

impl<V: Clone + Ord> DistinctState<V> {
    /// Creates empty support state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            support: BTreeMap::new(),
        }
    }

    /// Applies a delta and emits only zero/non-zero crossings.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ZeroDiff`] for invalid input rows or
    /// [`FlowError::Overflow`] during support arithmetic.
    pub fn apply(
        &mut self,
        input: impl IntoIterator<Item = Delta<V>>,
    ) -> Result<Vec<Delta<V>>, FlowError> {
        // A call is one logical input batch. Consolidating equal coordinates
        // first prevents a +1/-1 pair at the same time from publishing a
        // transient membership crossing that no downstream reader can
        // observe.
        let input = consolidate_rows(input.into_iter().collect())?;
        let mut out = Vec::new();
        for row in input {
            let key = (row.key, row.value.clone());
            let diff = row.diff.value();
            let entry = self.support.entry(key);
            let (old, new) = match entry {
                Entry::Vacant(entry) => {
                    entry.insert(diff);
                    (0, diff)
                }
                Entry::Occupied(mut entry) => {
                    let old = *entry.get();
                    let new = old.checked_add(diff).ok_or(FlowError::Overflow)?;
                    if new == 0 {
                        entry.remove();
                    } else {
                        *entry.get_mut() = new;
                    }
                    (old, new)
                }
            };
            if old <= 0 && new > 0 {
                out.push(Delta {
                    key: row.key,
                    value: row.value.clone(),
                    time: row.time,
                    diff: Weight::one(),
                });
            } else if old > 0 && new <= 0 {
                out.push(Delta {
                    key: row.key,
                    value: row.value.clone(),
                    time: row.time,
                    diff: Weight::minus_one(),
                });
            }
        }
        Ok(out)
    }

    /// Returns current positive memberships.
    #[must_use]
    pub fn members(&self) -> BTreeSet<(RowKey, V)> {
        self.support
            .iter()
            .filter(|(_, support)| **support > 0)
            .map(|(key, _)| key.clone())
            .collect()
    }
}

/// Counts weighted rows by a caller-selected group key.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when a group's support exceeds `i64`.
pub fn group_count<K: Clone + Ord, V: Clone>(
    input: impl IntoIterator<Item = Delta<V>>,
    mut group_key: impl FnMut(&Delta<V>) -> K,
) -> Result<BTreeMap<K, i64>, FlowError> {
    let mut groups: BTreeMap<K, i64> = BTreeMap::new();
    for row in input {
        let key = group_key(&row);
        let diff = row.diff.value();
        match groups.entry(key) {
            Entry::Vacant(entry) if diff != 0 => {
                entry.insert(diff);
            }
            Entry::Vacant(_) => {}
            Entry::Occupied(mut entry) => {
                let next = entry.get().checked_add(diff).ok_or(FlowError::Overflow)?;
                if next == 0 {
                    entry.remove();
                } else {
                    *entry.get_mut() = next;
                }
            }
        }
    }
    Ok(groups)
}

/// Sums a caller-selected signed value by group.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when a contribution or group total exceeds
/// `i64`.
pub fn group_sum<K: Clone + Ord, V: Clone>(
    input: impl IntoIterator<Item = Delta<V>>,
    mut group_key: impl FnMut(&Delta<V>) -> K,
    mut value: impl FnMut(&V) -> i64,
) -> Result<BTreeMap<K, i64>, FlowError> {
    let mut groups: BTreeMap<K, i64> = BTreeMap::new();
    for row in input {
        let key = group_key(&row);
        let contribution = value(&row.value)
            .checked_mul(row.diff.value())
            .ok_or(FlowError::Overflow)?;
        match groups.entry(key) {
            Entry::Vacant(entry) if contribution != 0 => {
                entry.insert(contribution);
            }
            Entry::Vacant(_) => {}
            Entry::Occupied(mut entry) => {
                let next = entry
                    .get()
                    .checked_add(contribution)
                    .ok_or(FlowError::Overflow)?;
                if next == 0 {
                    entry.remove();
                } else {
                    *entry.get_mut() = next;
                }
            }
        }
    }
    Ok(groups)
}

/// Alias for grouped count used by query planners.
///
/// # Errors
///
/// Propagates [`FlowError::Overflow`] from [`group_count`].
pub fn group<K: Clone + Ord, V: Clone>(
    input: impl IntoIterator<Item = Delta<V>>,
    group_key: impl FnMut(&Delta<V>) -> K,
) -> Result<BTreeMap<K, i64>, FlowError> {
    group_count(input, group_key)
}
