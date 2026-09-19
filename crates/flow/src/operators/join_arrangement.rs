//! Arrangement-backed join kernels.

use super::MAX_JOIN_OUTPUT_ROWS;
use crate::batch::consolidate_rows;
use crate::{Arrangement, CanonicalValue, Delta, FlowError, WorkCounters};
use std::mem::size_of;

fn count_candidate<O>(counters: &mut Option<&mut WorkCounters>) -> Result<(), FlowError> {
    if let Some(counters) = counters.as_deref_mut() {
        counters.join_fanout = counters
            .join_fanout
            .checked_add(1)
            .ok_or(FlowError::Overflow)?;
        counters.join_bytes = counters
            .join_bytes
            .checked_add(u64::try_from(size_of::<Delta<O>>()).map_err(|_| FlowError::Overflow)?)
            .ok_or(FlowError::Overflow)?;
    }
    Ok(())
}

fn count_probe(counters: &mut Option<&mut WorkCounters>) -> Result<(), FlowError> {
    if let Some(counters) = counters.as_deref_mut() {
        counters.join_probes = counters
            .join_probes
            .checked_add(1)
            .ok_or(FlowError::Overflow)?;
    }
    Ok(())
}

fn count_payload<O: CanonicalValue>(
    counters: &mut Option<&mut WorkCounters>,
    value: &O,
) -> Result<(), FlowError> {
    if let Some(counters) = counters.as_deref_mut() {
        counters.join_bytes = counters
            .join_bytes
            .checked_add(u64::try_from(value.owned_bytes()).map_err(|_| FlowError::Overflow)?)
            .ok_or(FlowError::Overflow)?;
    }
    Ok(())
}

/// Computes a simultaneous join delta by probing retained arrangements.
///
/// The arrangement order is the stable [`crate::RowKey`]. A delta row probes the
/// exact same key in the opposite arrangement, so the retained rows are
/// borrowed directly from their ordered runs. This path is useful when one
/// side is large and mostly unchanged: it does not materialize or index that
/// side for each transition. The three output terms are still the exact
/// simultaneous law: `ΔL⋈R + L⋈ΔR + ΔL⋈ΔR`.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when signed join arithmetic or output
/// consolidation overflows.
pub fn incremental_arrangement_join<
    L: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    R: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    O: Ord + CanonicalValue,
>(
    old_left: &Arrangement<L>,
    delta_left: &[Delta<L>],
    old_right: &Arrangement<R>,
    delta_right: &[Delta<R>],
    combine: impl Fn(&L, &R) -> O + Copy,
) -> Result<Vec<Delta<O>>, FlowError> {
    incremental_arrangement_join_impl(
        old_left,
        delta_left,
        old_right,
        delta_right,
        |row| row.key,
        |row| row.key,
        combine,
        None,
    )
}

/// Computes [`incremental_arrangement_join`] and records probe/output work.
///
/// Probe counts include one ordered-run seek for each delta row and output
/// counts are updated only after the three terms have been consolidated.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when signed join arithmetic, output
/// consolidation, or checked work accounting overflows.
pub fn incremental_arrangement_join_counted<
    L: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    R: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    O: Ord + CanonicalValue,
>(
    old_left: &Arrangement<L>,
    delta_left: &[Delta<L>],
    old_right: &Arrangement<R>,
    delta_right: &[Delta<R>],
    combine: impl Fn(&L, &R) -> O + Copy,
    counters: &mut WorkCounters,
) -> Result<Vec<Delta<O>>, FlowError> {
    incremental_arrangement_join_impl(
        old_left,
        delta_left,
        old_right,
        delta_right,
        |row| row.key,
        |row| row.key,
        combine,
        Some(counters),
    )
}

/// Computes a simultaneous arrangement join with explicit row-key
/// projections.
///
/// The projections select the ordered key used by each retained arrangement;
/// they allow a relation-specific row identity to be normalized before the
/// range probe. Retained rows remain borrowed from their ordered runs.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when signed join arithmetic or output
/// consolidation overflows.
#[allow(
    clippy::too_many_arguments,
    reason = "the facade keeps both arrangements, lanes, projections, and recipe explicit"
)]
pub fn incremental_arrangement_join_by_key<
    L: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    R: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    O: Ord + CanonicalValue,
>(
    old_left: &Arrangement<L>,
    delta_left: &[Delta<L>],
    old_right: &Arrangement<R>,
    delta_right: &[Delta<R>],
    left_key: impl Fn(&Delta<L>) -> crate::RowKey + Copy,
    right_key: impl Fn(&Delta<R>) -> crate::RowKey + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
) -> Result<Vec<Delta<O>>, FlowError> {
    incremental_arrangement_join_impl(
        old_left,
        delta_left,
        old_right,
        delta_right,
        left_key,
        right_key,
        combine,
        None,
    )
}

/// Computes [`incremental_arrangement_join_by_key`] and records probe/output
/// work.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when join arithmetic, consolidation, or
/// checked work accounting overflows.
#[allow(
    clippy::too_many_arguments,
    reason = "the counted facade keeps both arrangements, lanes, projections, recipe, and counters explicit"
)]
pub fn incremental_arrangement_join_by_key_counted<
    L: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    R: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    O: Ord + CanonicalValue,
>(
    old_left: &Arrangement<L>,
    delta_left: &[Delta<L>],
    old_right: &Arrangement<R>,
    delta_right: &[Delta<R>],
    left_key: impl Fn(&Delta<L>) -> crate::RowKey + Copy,
    right_key: impl Fn(&Delta<R>) -> crate::RowKey + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
    counters: &mut WorkCounters,
) -> Result<Vec<Delta<O>>, FlowError> {
    incremental_arrangement_join_impl(
        old_left,
        delta_left,
        old_right,
        delta_right,
        left_key,
        right_key,
        combine,
        Some(counters),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the implementation keeps both lanes, ordered projections, recipe, and accounting explicit"
)]
fn incremental_arrangement_join_impl<
    L: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    R: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    O: Ord + CanonicalValue,
>(
    old_left: &Arrangement<L>,
    delta_left: &[Delta<L>],
    old_right: &Arrangement<R>,
    delta_right: &[Delta<R>],
    left_key: impl Fn(&Delta<L>) -> crate::RowKey + Copy,
    right_key: impl Fn(&Delta<R>) -> crate::RowKey + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
    mut counters: Option<&mut WorkCounters>,
) -> Result<Vec<Delta<O>>, FlowError> {
    let mut out = Vec::new();
    append_arrangement_left_matches(
        &mut out,
        delta_left,
        old_right,
        left_key,
        combine,
        &mut counters,
    )?;
    append_arrangement_right_matches(
        &mut out,
        old_left,
        delta_right,
        right_key,
        combine,
        &mut counters,
    )?;
    append_arrangement_cross_matches(
        &mut out,
        delta_left,
        delta_right,
        left_key,
        right_key,
        combine,
        &mut counters,
    )?;
    let output = consolidate_rows(out)?;
    if let Some(counters) = counters {
        counters.join_rows = counters
            .join_rows
            .checked_add(u64::try_from(output.len()).map_err(|_| FlowError::Overflow)?)
            .ok_or(FlowError::Overflow)?;
    }
    Ok(output)
}

fn add_arrangement_probe(
    counters: &mut Option<&mut WorkCounters>,
    runs: usize,
) -> Result<(), FlowError> {
    let runs = u64::try_from(runs).map_err(|_| FlowError::Overflow)?;
    if let Some(counters) = counters.as_deref_mut() {
        counters.join_probes = counters
            .join_probes
            .checked_add(runs)
            .ok_or(FlowError::Overflow)?;
        counters.seek_probes = counters
            .seek_probes
            .checked_add(runs)
            .ok_or(FlowError::Overflow)?;
    }
    Ok(())
}

fn append_arrangement_left_matches<
    L,
    R: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    O: CanonicalValue,
>(
    out: &mut Vec<Delta<O>>,
    left: &[Delta<L>],
    right: &Arrangement<R>,
    left_key: impl Fn(&Delta<L>) -> crate::RowKey + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
    counters: &mut Option<&mut WorkCounters>,
) -> Result<(), FlowError> {
    for left_row in left {
        add_arrangement_probe(counters, right.run_count())?;
        let probe_key = left_key(left_row);
        for right_row in right.iter_range(probe_key..=probe_key) {
            count_candidate::<O>(counters)?;
            if out.len() >= MAX_JOIN_OUTPUT_ROWS {
                return Err(FlowError::CompactionBackpressure);
            }
            let value = combine(&left_row.value, right_row.value);
            count_payload(counters, &value)?;
            out.push(Delta {
                key: left_row.key,
                value,
                time: left_row.time.join(right_row.time),
                diff: left_row.diff.checked_mul(right_row.diff)?,
            });
        }
    }
    Ok(())
}

fn append_arrangement_right_matches<
    L: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + 'static,
    R,
    O: CanonicalValue,
>(
    out: &mut Vec<Delta<O>>,
    left: &Arrangement<L>,
    right: &[Delta<R>],
    right_key: impl Fn(&Delta<R>) -> crate::RowKey + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
    counters: &mut Option<&mut WorkCounters>,
) -> Result<(), FlowError> {
    for right_row in right {
        add_arrangement_probe(counters, left.run_count())?;
        let probe_key = right_key(right_row);
        for left_row in left.iter_range(probe_key..=probe_key) {
            count_candidate::<O>(counters)?;
            if out.len() >= MAX_JOIN_OUTPUT_ROWS {
                return Err(FlowError::CompactionBackpressure);
            }
            let value = combine(left_row.value, &right_row.value);
            count_payload(counters, &value)?;
            out.push(Delta {
                key: left_row.key,
                value,
                time: left_row.time.join(right_row.time),
                diff: left_row.diff.checked_mul(right_row.diff)?,
            });
        }
    }
    Ok(())
}

fn append_arrangement_cross_matches<L, R, O: CanonicalValue>(
    out: &mut Vec<Delta<O>>,
    left: &[Delta<L>],
    right: &[Delta<R>],
    left_key: impl Fn(&Delta<L>) -> crate::RowKey + Copy,
    right_key: impl Fn(&Delta<R>) -> crate::RowKey + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
    counters: &mut Option<&mut WorkCounters>,
) -> Result<(), FlowError> {
    // The cross term is a keyed intersection. Sort borrowed row references
    // once and merge the two key streams; a disjoint 100k/100k transition now
    // performs O(n log n) work instead of a quadratic nested loop.
    let mut left_rows = left
        .iter()
        .map(|row| (left_key(row), row))
        .collect::<Vec<_>>();
    let mut right_rows = right
        .iter()
        .map(|row| (right_key(row), row))
        .collect::<Vec<_>>();
    left_rows.sort_unstable_by_key(|(key, _)| *key);
    right_rows.sort_unstable_by_key(|(key, _)| *key);
    let mut left_at = 0;
    let mut right_at = 0;
    while left_at < left_rows.len() && right_at < right_rows.len() {
        count_probe(counters)?;
        match left_rows[left_at].0.cmp(&right_rows[right_at].0) {
            std::cmp::Ordering::Less => left_at += 1,
            std::cmp::Ordering::Greater => right_at += 1,
            std::cmp::Ordering::Equal => {
                let key = left_rows[left_at].0;
                let left_end = left_rows[left_at..]
                    .partition_point(|(candidate, _)| *candidate == key)
                    + left_at;
                let right_end = right_rows[right_at..]
                    .partition_point(|(candidate, _)| *candidate == key)
                    + right_at;
                for (_, left_row) in left_rows[left_at..left_end].iter().copied() {
                    for (_, right_row) in right_rows[right_at..right_end].iter().copied() {
                        count_candidate::<O>(counters)?;
                        if out.len() >= MAX_JOIN_OUTPUT_ROWS {
                            return Err(FlowError::CompactionBackpressure);
                        }
                        let value = combine(&left_row.value, &right_row.value);
                        count_payload(counters, &value)?;
                        out.push(Delta {
                            key: left_row.key,
                            value,
                            time: left_row.time.join(right_row.time),
                            diff: left_row.diff.checked_mul(right_row.diff)?,
                        });
                    }
                }
                left_at = left_end;
                right_at = right_end;
            }
        }
    }
    Ok(())
}
