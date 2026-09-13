//! Retained top-k support and boundary kernels.

use crate::batch::consolidate_rows;
use crate::{Delta, EquivalenceIdentity, FlowError, RowKey, Time, Weight, WorkScope};
use std::cmp::Ordering;
use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
};

/// A deterministic ranked candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate<V> {
    /// Score used for ranking.
    pub score: i64,
    /// Stable candidate row key.
    pub key: RowKey,
    /// Candidate payload.
    pub value: V,
}

/// Retained support and ordered boundary state for top-k.
#[derive(Clone, Debug)]
pub struct TopKState<V> {
    support: BTreeMap<(RowKey, V), i64>,
    ordered: BTreeSet<(std::cmp::Reverse<i64>, RowKey, V)>,
    score_dependency: Option<EquivalenceIdentity>,
    scores_valid: bool,
}

impl<V: Clone + Ord> TopKState<V> {
    /// Creates an empty top-k state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            support: BTreeMap::new(),
            ordered: BTreeSet::new(),
            score_dependency: None,
            scores_valid: true,
        }
    }

    /// Creates a top-k state whose ranking depends on a global statistic or
    /// other explicitly versioned equivalence input.
    #[must_use]
    pub fn with_score_dependency(dependency: EquivalenceIdentity) -> Self {
        Self {
            score_dependency: Some(dependency),
            ..Self::new()
        }
    }

    /// Records a new score dependency generation.
    pub fn set_score_dependency(&mut self, dependency: EquivalenceIdentity) {
        if self.score_dependency == Some(dependency) {
            return;
        }
        self.score_dependency = Some(dependency);
        self.ordered.clear();
        self.scores_valid = false;
    }

    /// Returns the exact global dependency that must invalidate scores.
    #[must_use]
    pub const fn score_dependency(&self) -> Option<EquivalenceIdentity> {
        self.score_dependency
    }

    /// Rebuilds only the ordered score index after a global statistic change.
    /// Retained support is reused, so this work is proportional to the
    /// demanded candidate state rather than the source relation.
    pub fn refresh_scores(&mut self, mut score: impl FnMut(&V) -> i64) {
        let mut budget = WorkScope::new(usize::MAX, usize::MAX);
        let _ = self.refresh_scores_budgeted(&mut score, &mut budget);
    }

    /// Rebuilds the ordered score index under an explicit work envelope.
    ///
    /// The replacement index is prepared off to the side, so a budget failure
    /// leaves the prior score index and validity marker intact.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::RecursionWorkLimit`] when retained support exceeds
    /// `scope`, or [`FlowError::Overflow`] when accounting wraps.
    pub fn refresh_scores_budgeted(
        &mut self,
        mut score: impl FnMut(&V) -> i64,
        budget: &mut WorkScope,
    ) -> Result<(), FlowError> {
        let mut ordered = BTreeSet::new();
        for ((key, value), support) in &self.support {
            budget.charge_rows(1)?;
            budget.charge_bytes(size_of::<(std::cmp::Reverse<i64>, RowKey, V)>())?;
            if *support > 0 {
                ordered.insert((std::cmp::Reverse(score(value)), *key, value.clone()));
            }
        }
        self.ordered = ordered;
        self.scores_valid = true;
        Ok(())
    }

    /// Applies only changed rows and returns the current top-k boundary.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Overflow`] when support arithmetic overflows.
    pub fn apply(
        &mut self,
        input: impl IntoIterator<Item = Delta<V>>,
        k: usize,
        mut score: impl FnMut(&V) -> i64,
    ) -> Result<Vec<Candidate<V>>, FlowError> {
        if !self.scores_valid {
            self.refresh_scores(&mut score);
        }
        let input = consolidate_rows(input.into_iter().collect())?;
        for row in input {
            let key = (row.key, row.value.clone());
            // Scores are pure recipe output. Cache the value for this changed
            // row so a replacement does one score evaluation, even when it
            // removes and reinserts the same candidate in the ordered set.
            let row_score = score(&row.value);
            let old = self.support.get(&key).copied().map_or(0, |support| support);
            if old > 0 {
                self.ordered
                    .remove(&(std::cmp::Reverse(row_score), row.key, row.value.clone()));
            }
            let new = old
                .checked_add(row.diff.value())
                .ok_or(FlowError::Overflow)?;
            match new.cmp(&0) {
                Ordering::Equal => {
                    self.support.remove(&key);
                }
                Ordering::Greater => {
                    self.support.insert(key.clone(), new);
                    self.ordered
                        .insert((std::cmp::Reverse(row_score), row.key, row.value));
                }
                Ordering::Less => {
                    self.support.insert(key.clone(), new);
                }
            }
        }
        Ok(self.current(k))
    }

    /// Returns the current top-k candidates in score/key order.
    #[must_use]
    pub fn current(&self, k: usize) -> Vec<Candidate<V>> {
        self.ordered
            .iter()
            .take(k)
            .map(|(score, key, value)| Candidate {
                score: score.0,
                key: *key,
                value: value.clone(),
            })
            .collect()
    }
}

impl<V: Clone + Ord> Default for TopKState<V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Computes a top-k snapshot using retained support semantics.
///
/// # Errors
///
/// Returns [`FlowError`] when support arithmetic overflows or an invalid
/// update is supplied.
pub fn top_k<V: Clone + Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
    k: usize,
    score: impl FnMut(&V) -> i64,
) -> Result<Vec<Candidate<V>>, FlowError> {
    top_k_checked(input, k, score)
}

/// Checked top-k snapshot.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] when support arithmetic overflows.
pub fn top_k_checked<V: Clone + Ord>(
    input: impl IntoIterator<Item = Delta<V>>,
    k: usize,
    score: impl FnMut(&V) -> i64,
) -> Result<Vec<Candidate<V>>, FlowError> {
    let mut state = TopKState::new();
    state.apply(input, k, score)
}

/// Computes boundary changes between two ranked snapshots.
///
/// A rank or score change is represented as a retraction followed by an
/// insertion at the next logical time. This keeps the transition observable
/// after ordinary delta consolidation, whose identity includes time.
///
/// # Errors
///
/// Returns [`FlowError::Overflow`] if the transition time cannot advance.
pub fn top_k_changes<V: Clone + Ord>(
    before: &[Candidate<V>],
    after: &[Candidate<V>],
    time: Time,
) -> Result<Vec<Delta<V>>, FlowError> {
    let before_positions = before
        .iter()
        .enumerate()
        .map(|(position, candidate)| {
            (
                (candidate.key, candidate.value.clone()),
                (candidate.score, position),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let after_positions = after
        .iter()
        .enumerate()
        .map(|(position, candidate)| {
            (
                (candidate.key, candidate.value.clone()),
                (candidate.score, position),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let changed_time = time.successor()?;
    let mut out = Vec::new();
    for candidate in before {
        let key = (candidate.key, candidate.value.clone());
        if after_positions.get(&key) != Some(&(candidate.score, before_positions[&key].1)) {
            out.push(Delta {
                key: candidate.key,
                value: candidate.value.clone(),
                time,
                diff: Weight::minus_one(),
            });
        }
    }
    for candidate in after {
        let key = (candidate.key, candidate.value.clone());
        if before_positions.get(&key) != Some(&(candidate.score, after_positions[&key].1)) {
            let transition_time = if before_positions.contains_key(&key) {
                changed_time
            } else {
                time
            };
            out.push(Delta {
                key: candidate.key,
                value: candidate.value.clone(),
                time: transition_time,
                diff: Weight::one(),
            });
        }
    }
    Ok(out)
}
