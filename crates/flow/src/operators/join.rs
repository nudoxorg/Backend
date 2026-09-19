//! Exact and simultaneous weighted join kernels.

use crate::batch::consolidate_rows;
use crate::{CanonicalValue, Delta, FlowError, WorkCounters, WorkScope};
use std::{mem::size_of, ops::Range};

#[path = "join_arrangement.rs"]
mod join_arrangement;
pub use join_arrangement::*;

/// Hard output fence for the compatibility Vec-returning join facades. Large
/// producers should use the segmented facade and publish each segment.
const MAX_JOIN_OUTPUT_ROWS: usize = 1_048_576;

/// Performs a reference-scale exact weighted join over matching batch keys.
///
/// This helper intentionally builds a temporary borrowed index for its small
/// slice inputs. Retained relations should use [`incremental_arrangement_join`]
/// so large unchanged sides stay in ordered runs.
///
/// # Errors
///
/// Returns [`FlowError`] when signed multiplication or consolidation
/// overflows, or an invalid update is supplied.
pub fn join<L, R, O: Ord + CanonicalValue>(
    left: &[Delta<L>],
    right: &[Delta<R>],
    left_key: impl FnMut(&L) -> u64,
    right_key: impl FnMut(&R) -> u64,
    combine: impl FnMut(&L, &R) -> O,
) -> Result<Vec<Delta<O>>, FlowError> {
    join_checked(left, right, left_key, right_key, combine)
}

/// Checked exact join; output work is proportional to matching fan-out.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when signed multiplication or output
/// consolidation exceeds its checked range.
pub fn join_checked<L, R, O: Ord + CanonicalValue>(
    left: &[Delta<L>],
    right: &[Delta<R>],
    left_key: impl FnMut(&L) -> u64,
    right_key: impl FnMut(&R) -> u64,
    combine: impl FnMut(&L, &R) -> O,
) -> Result<Vec<Delta<O>>, FlowError> {
    join_checked_impl(left, right, left_key, right_key, combine, None)
}

fn join_checked_impl<L, R, O: Ord + CanonicalValue>(
    left: &[Delta<L>],
    right: &[Delta<R>],
    left_key: impl FnMut(&L) -> u64,
    right_key: impl FnMut(&R) -> u64,
    combine: impl FnMut(&L, &R) -> O,
    mut counters: Option<&mut WorkCounters>,
) -> Result<Vec<Delta<O>>, FlowError> {
    let right_index = build_join_index(right, right_key);
    if let Some(counters) = counters.as_deref_mut() {
        counters.join_index_rows = counters
            .join_index_rows
            .checked_add(u64::try_from(right.len()).map_err(|_| FlowError::Overflow)?)
            .ok_or(FlowError::Overflow)?;
    }
    let mut out = Vec::new();
    append_join_matches(
        &mut out,
        left,
        &right_index,
        left_key,
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

/// One cache-friendly borrowed index shared by all exact join kernels.
///
/// Rows with the same key occupy a contiguous span. This avoids one allocation
/// per distinct key and lets cursors retain only two range offsets instead of
/// cloning the complete fanout for every left row.
struct FlatJoinIndex<'a, R> {
    rows: Vec<(u64, &'a Delta<R>)>,
}

impl<'a, R> FlatJoinIndex<'a, R> {
    fn new(rows: &'a [Delta<R>], mut right_key: impl FnMut(&R) -> u64) -> Self {
        let mut rows = rows
            .iter()
            .map(|row| (right_key(&row.value), row))
            .collect::<Vec<_>>();
        rows.sort_by_key(|(key, _)| *key);
        Self { rows }
    }

    fn span(&self, key: u64) -> Range<usize> {
        let start = self.rows.partition_point(|(candidate, _)| *candidate < key);
        let width = self.rows[start..].partition_point(|(candidate, _)| *candidate == key);
        start..start.saturating_add(width)
    }

    fn matches(&self, key: u64) -> &[(u64, &'a Delta<R>)] {
        &self.rows[self.span(key)]
    }
}

/// Builds one deterministic probe index for a join's right-hand side.
///
/// The index borrows the input rows, so callers can use it for several terms
/// of a simultaneous transition without cloning either input relation.
fn build_join_index<R>(
    right: &[Delta<R>],
    right_key: impl FnMut(&R) -> u64,
) -> FlatJoinIndex<'_, R> {
    FlatJoinIndex::new(right, right_key)
}

/// Stateful simultaneous-join cursor. The index borrows the right input and
/// each call owns at most one bounded output segment; no complete join result
/// is materialized before the caller can publish progress.
pub struct JoinCursor<'a, L, R, O, LK, RK, C>
where
    LK: Fn(&L) -> u64 + Copy,
    RK: Fn(&R) -> u64 + Copy,
    C: Fn(&L, &R) -> O + Copy,
{
    left_terms: [&'a [Delta<L>]; 3],
    right_indexes: [FlatJoinIndex<'a, R>; 2],
    left_key: LK,
    combine: C,
    term: usize,
    left: usize,
    current: Option<&'a Delta<L>>,
    match_at: usize,
    match_end: usize,
    counters: WorkCounters,
    finished: bool,
    marker: std::marker::PhantomData<(O, RK)>,
}

impl<'a, L, R, O, LK, RK, C> JoinCursor<'a, L, R, O, LK, RK, C>
where
    O: CanonicalValue,
    LK: Fn(&L) -> u64 + Copy,
    RK: Fn(&R) -> u64 + Copy,
    C: Fn(&L, &R) -> O + Copy,
{
    fn new(
        old_left: &'a [Delta<L>],
        delta_left: &'a [Delta<L>],
        old_right: &'a [Delta<R>],
        delta_right: &'a [Delta<R>],
        left_key: LK,
        right_key: RK,
        combine: C,
    ) -> Self {
        let right_indexes = [
            build_join_index(old_right, right_key),
            build_join_index(delta_right, right_key),
        ];
        let counters = WorkCounters {
            join_index_rows: u64::try_from(old_right.len().saturating_add(delta_right.len()))
                .unwrap_or(u64::MAX),
            ..WorkCounters::default()
        };
        Self {
            left_terms: [delta_left, old_left, delta_left],
            right_indexes,
            left_key,
            combine,
            term: 0,
            left: 0,
            current: None,
            match_at: 0,
            match_end: 0,
            counters,
            finished: false,
            marker: std::marker::PhantomData,
        }
    }

    const fn target_index(&self) -> usize {
        if self.term == 0 { 0 } else { 1 }
    }

    fn advance_left(&mut self, scope: &mut WorkScope) -> Result<bool, FlowError> {
        loop {
            if self.term == 3 {
                self.finished = true;
                return Ok(false);
            }
            if self.left >= self.left_terms[self.term].len() {
                self.term = self.term.saturating_add(1);
                self.left = 0;
                self.current = None;
                self.match_at = 0;
                self.match_end = 0;
                continue;
            }
            let row = &self.left_terms[self.term][self.left];
            scope.charge_probes(1)?;
            self.counters.join_probes = self
                .counters
                .join_probes
                .checked_add(1)
                .ok_or(FlowError::Overflow)?;
            let target = self.target_index();
            let span = self.right_indexes[target].span((self.left_key)(&row.value));
            self.match_at = span.start;
            self.match_end = span.end;
            self.current = Some(row);
            return Ok(true);
        }
    }

    /// Emits the next bounded output segment. A segment charges its row bytes
    /// before invoking the combine closure, so rejected work cannot allocate
    /// an unaccounted payload.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::RecursionWorkLimit`] when the supplied scope is
    /// exhausted, or a checked arithmetic error for invalid weights.
    pub fn next_segment(
        &mut self,
        max_rows: usize,
        scope: &mut WorkScope,
    ) -> Result<Option<Vec<Delta<O>>>, FlowError> {
        if max_rows == 0 {
            return Err(FlowError::InvalidCompactionBudget);
        }
        if max_rows > MAX_JOIN_OUTPUT_ROWS {
            return Err(FlowError::CompactionBackpressure);
        }
        let mut output = Vec::with_capacity(max_rows);
        while output.len() < max_rows && !self.finished {
            if self.current.is_none() && !self.advance_left(scope)? {
                break;
            }
            let Some(left) = self.current else {
                continue;
            };
            let target = self.target_index();
            while self.match_at < self.match_end {
                let right = self.right_indexes[target].rows[self.match_at].1;
                scope.charge_row_bytes(1, size_of::<Delta<O>>())?;
                let value = (self.combine)(&left.value, &right.value);
                // The inline row slot was reserved before invoking user
                // code. Heap payload ownership is measured from the actual
                // result before it enters the yielded segment.
                scope.charge_bytes(value.owned_bytes())?;
                self.match_at += 1;
                self.counters.join_fanout = self
                    .counters
                    .join_fanout
                    .checked_add(1)
                    .ok_or(FlowError::Overflow)?;
                self.counters.join_bytes = self
                    .counters
                    .join_bytes
                    .checked_add(
                        u64::try_from(size_of::<Delta<O>>())
                            .map_err(|_| FlowError::Overflow)?
                            .checked_add(
                                u64::try_from(value.owned_bytes())
                                    .map_err(|_| FlowError::Overflow)?,
                            )
                            .ok_or(FlowError::Overflow)?,
                    )
                    .ok_or(FlowError::Overflow)?;
                output.push(Delta {
                    key: left.key,
                    value,
                    time: left.time.join(right.time),
                    diff: left.diff.checked_mul(right.diff)?,
                });
                if output.len() == max_rows {
                    break;
                }
            }
            if self.match_at >= self.match_end {
                self.left = self.left.saturating_add(1);
                self.current = None;
                self.match_at = 0;
                self.match_end = 0;
            }
        }
        if output.is_empty() && self.finished {
            Ok(None)
        } else {
            Ok(Some(output))
        }
    }

    /// Copies accumulated probe/fanout counters into the caller's counters.
    pub fn finish_counters(&self, counters: &mut WorkCounters) {
        counters.join_index_rows = counters
            .join_index_rows
            .saturating_add(self.counters.join_index_rows);
        counters.join_probes = counters
            .join_probes
            .saturating_add(self.counters.join_probes);
        counters.join_fanout = counters
            .join_fanout
            .saturating_add(self.counters.join_fanout);
        counters.join_bytes = counters.join_bytes.saturating_add(self.counters.join_bytes);
    }
}

/// Creates a stateful simultaneous join cursor over borrowed inputs.
#[allow(clippy::too_many_arguments)]
pub fn incremental_join_cursor<'a, L, R, O, LK, RK, C>(
    old_left: &'a [Delta<L>],
    delta_left: &'a [Delta<L>],
    old_right: &'a [Delta<R>],
    delta_right: &'a [Delta<R>],
    left_key: LK,
    right_key: RK,
    combine: C,
) -> JoinCursor<'a, L, R, O, LK, RK, C>
where
    O: CanonicalValue,
    LK: Fn(&L) -> u64 + Copy,
    RK: Fn(&R) -> u64 + Copy,
    C: Fn(&L, &R) -> O + Copy,
{
    JoinCursor::new(
        old_left,
        delta_left,
        old_right,
        delta_right,
        left_key,
        right_key,
        combine,
    )
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

/// Appends one join term to an output buffer.  Consolidation is deliberately
/// left to the caller so a multi-term delta pays for one ordering pass.
fn append_join_matches<L, R, O>(
    out: &mut Vec<Delta<O>>,
    left: &[Delta<L>],
    right_index: &FlatJoinIndex<'_, R>,
    mut left_key: impl FnMut(&L) -> u64,
    mut combine: impl FnMut(&L, &R) -> O,
    counters: &mut Option<&mut WorkCounters>,
) -> Result<(), FlowError> {
    for left_row in left {
        count_probe(counters)?;
        let matches = right_index.matches(left_key(&left_row.value));
        if !matches.is_empty() {
            for &(_, right_row) in matches {
                count_candidate::<O>(counters)?;
                let diff = left_row.diff.checked_mul(right_row.diff)?;
                if out.len() >= MAX_JOIN_OUTPUT_ROWS {
                    return Err(FlowError::CompactionBackpressure);
                }
                out.push(Delta {
                    key: left_row.key,
                    value: combine(&left_row.value, &right_row.value),
                    time: left_row.time.join(right_row.time),
                    diff,
                });
            }
        }
    }
    Ok(())
}

/// Checked join variant that exposes the actual retained output work.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when join arithmetic, consolidation, or
/// the output counter exceeds its checked range.
pub fn join_checked_counted<L, R, O: Ord + CanonicalValue>(
    left: &[Delta<L>],
    right: &[Delta<R>],
    left_key: impl FnMut(&L) -> u64,
    right_key: impl FnMut(&R) -> u64,
    combine: impl FnMut(&L, &R) -> O,
    counters: &mut WorkCounters,
) -> Result<Vec<Delta<O>>, FlowError> {
    join_checked_impl(left, right, left_key, right_key, combine, Some(counters))
}

/// Computes a reference-scale simultaneous batch join delta with the cross term exactly
/// once: `ΔL⋈R + L⋈ΔR + ΔL⋈ΔR`.
///
/// Retained large sides should use [`incremental_arrangement_join`] instead of
/// rebuilding a batch index for every transition.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when any signed cross term or final
/// consolidation exceeds its checked range.
pub fn incremental_join<L, R, O: Ord + CanonicalValue>(
    old_left: &[Delta<L>],
    delta_left: &[Delta<L>],
    old_right: &[Delta<R>],
    delta_right: &[Delta<R>],
    left_key: impl Fn(&L) -> u64 + Copy,
    right_key: impl Fn(&R) -> u64 + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
) -> Result<Vec<Delta<O>>, FlowError> {
    incremental_join_impl(
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

/// Computes the simultaneous join delta and records retained output work.
///
/// The counter is updated only after the exact three-term result has been
/// consolidated, so it describes observable output rows rather than the
/// temporary cross-term fan-out.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when signed join arithmetic,
/// consolidation, or the output counter exceeds its checked range.
#[allow(
    clippy::too_many_arguments,
    reason = "the counted facade mirrors the four input lanes and three join laws"
)]
pub fn incremental_join_counted<L, R, O: Ord + CanonicalValue>(
    old_left: &[Delta<L>],
    delta_left: &[Delta<L>],
    old_right: &[Delta<R>],
    delta_right: &[Delta<R>],
    left_key: impl Fn(&L) -> u64 + Copy,
    right_key: impl Fn(&R) -> u64 + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
    counters: &mut WorkCounters,
) -> Result<Vec<Delta<O>>, FlowError> {
    incremental_join_impl(
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

/// Computes a simultaneous join and partitions its owned result into bounded
/// immutable output segments.  Consumers can publish each segment before
/// requesting the next one, which keeps the arrangement admission boundary
/// explicit for high fanout joins.
///
/// # Errors
///
/// Returns [`FlowError::InvalidCompactionBudget`] for a zero segment size and
/// propagates the checked arithmetic errors from [`incremental_join_counted`].
#[allow(
    clippy::too_many_arguments,
    reason = "the segmented facade mirrors the simultaneous join inputs"
)]
pub fn incremental_join_counted_segmented<L, R, O: Ord + CanonicalValue>(
    old_left: &[Delta<L>],
    delta_left: &[Delta<L>],
    old_right: &[Delta<R>],
    delta_right: &[Delta<R>],
    left_key: impl Fn(&L) -> u64 + Copy,
    right_key: impl Fn(&R) -> u64 + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
    counters: &mut WorkCounters,
    max_segment_rows: usize,
) -> Result<Vec<Vec<Delta<O>>>, FlowError> {
    if max_segment_rows == 0 {
        return Err(FlowError::InvalidCompactionBudget);
    }
    let mut cursor = incremental_join_cursor(
        old_left,
        delta_left,
        old_right,
        delta_right,
        left_key,
        right_key,
        combine,
    );
    let mut scope = WorkScope::new(MAX_JOIN_OUTPUT_ROWS, usize::MAX);
    let mut segments = Vec::new();
    while let Some(segment) = cursor.next_segment(max_segment_rows, &mut scope)? {
        segments.push(segment);
    }
    cursor.finish_counters(counters);
    counters.join_rows = counters
        .join_rows
        .checked_add(
            u64::try_from(segments.iter().map(Vec::len).sum::<usize>())
                .map_err(|_| FlowError::Overflow)?,
        )
        .ok_or(FlowError::Overflow)?;
    Ok(segments)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the implementation keeps each simultaneous transition lane explicit"
)]
fn incremental_join_impl<L, R, O: Ord + CanonicalValue>(
    old_left: &[Delta<L>],
    delta_left: &[Delta<L>],
    old_right: &[Delta<R>],
    delta_right: &[Delta<R>],
    left_key: impl Fn(&L) -> u64 + Copy,
    right_key: impl Fn(&R) -> u64 + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
    mut counters: Option<&mut WorkCounters>,
) -> Result<Vec<Delta<O>>, FlowError> {
    // Compatibility callers receive a checked collection of bounded pages.
    // The cursor itself owns only the right index and the current fanout;
    // this hard fence prevents a Vec facade from becoming an unbounded
    // production path.
    let mut cursor = incremental_join_cursor(
        old_left,
        delta_left,
        old_right,
        delta_right,
        left_key,
        right_key,
        combine,
    );
    let mut scope = WorkScope::new(MAX_JOIN_OUTPUT_ROWS, usize::MAX);
    let mut out = Vec::new();
    while let Some(segment) = cursor.next_segment(2_048, &mut scope)? {
        let next_len = out
            .len()
            .checked_add(segment.len())
            .ok_or(FlowError::Overflow)?;
        if next_len > MAX_JOIN_OUTPUT_ROWS {
            return Err(FlowError::CompactionBackpressure);
        }
        out.extend(segment);
    }
    if let Some(counters) = counters.as_deref_mut() {
        cursor.finish_counters(counters);
    }
    let output = consolidate_rows(out)?;
    if let Some(counters) = counters.take() {
        counters.join_rows = counters
            .join_rows
            .checked_add(u64::try_from(output.len()).map_err(|_| FlowError::Overflow)?)
            .ok_or(FlowError::Overflow)?;
    }
    Ok(output)
}

/// Alias making the simultaneous transition convention explicit.
///
/// # Errors
///
/// Propagates checked arithmetic and consolidation failures from the three
/// exact join terms.
pub fn join_delta<L, R, O: Ord + CanonicalValue>(
    old_left: &[Delta<L>],
    delta_left: &[Delta<L>],
    old_right: &[Delta<R>],
    delta_right: &[Delta<R>],
    left_key: impl Fn(&L) -> u64 + Copy,
    right_key: impl Fn(&R) -> u64 + Copy,
    combine: impl Fn(&L, &R) -> O + Copy,
) -> Result<Vec<Delta<O>>, FlowError> {
    incremental_join(
        old_left,
        delta_left,
        old_right,
        delta_right,
        left_key,
        right_key,
        combine,
    )
}
