//! Stateless map and filter kernels.

use crate::batch::consolidate_rows;
use crate::{Delta, FlowError, RowKey};

/// Applies a value map while preserving time and signed weight.
///
/// # Errors
///
/// Returns [`FlowError`] when consolidation overflows or receives an invalid
/// update.
pub fn map<V, W: Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
    mut f: impl FnMut(RowKey, &V) -> W,
) -> Result<Vec<Delta<W>>, FlowError> {
    map_checked(input, &mut f)
}

/// Checked value map with consolidation and no-op suppression.
///
/// # Errors
///
/// Returns [`FlowError::ZeroDiff`] for invalid input rows or
/// [`FlowError::Overflow`] when equal mapped rows overflow during
/// consolidation.
pub fn map_checked<V, W: Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
    f: &mut impl FnMut(RowKey, &V) -> W,
) -> Result<Vec<Delta<W>>, FlowError> {
    let rows = input
        .into_iter()
        .map(|row| {
            let value = f(row.key, &row.value);
            Delta {
                key: row.key,
                value,
                time: row.time,
                diff: row.diff,
            }
        })
        .collect();
    consolidate_rows(rows)
}

/// Applies a predicate while preserving signed updates.
///
/// # Errors
///
/// Returns [`FlowError`] when consolidation overflows or receives an invalid
/// update.
pub fn filter<V: Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
    mut keep: impl FnMut(RowKey, &V) -> bool,
) -> Result<Vec<Delta<V>>, FlowError> {
    filter_checked(input, &mut keep)
}

/// Checked filter with consolidation and no-op suppression.
///
/// # Errors
///
/// Returns [`FlowError::ZeroDiff`] for invalid input rows or
/// [`FlowError::Overflow`] when retained rows overflow during consolidation.
pub fn filter_checked<V: Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
    keep: &mut impl FnMut(RowKey, &V) -> bool,
) -> Result<Vec<Delta<V>>, FlowError> {
    consolidate_rows(
        input
            .into_iter()
            .filter(|row| keep(row.key, &row.value))
            .collect(),
    )
}
