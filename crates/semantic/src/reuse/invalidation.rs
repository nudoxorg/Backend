//! Bounded delta invalidation and the full-scan reference oracle.

use super::checked_add;
use super::index::{
    ReaderBucket, RetainedReaders, SemanticReaderKey, SemanticWorkKey, selector_interval,
};
use crate::{AuthorityVersion, ReadSelector, RecipeVersion, ScopedRead, SemanticError};
use std::collections::BTreeSet;

/// Bounded work counters for reverse-index invalidation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InvalidationCounters {
    /// Number of changed selectors admitted to the query.
    pub changed_reads: u64,
    /// Exact-key reverse probes.
    pub exact_probes: u64,
    /// Interval-tree nodes inspected.
    pub interval_probes: u64,
    /// Distinct reader shards represented by reverse-index candidates.
    pub candidate_readers: u64,
    /// Registration/change pairs returned by the reverse arrangements.
    ///
    /// One retained registration can be counted more than once when several
    /// changed selectors intersect it. Counting each hit bounds the temporary
    /// work and memory needed to verify a batch.
    pub candidate_registrations: u64,
    /// Stored observations checked after each candidate lookup.
    pub verified_reads: u64,
    /// Readers whose dependency actually intersects a change.
    pub invalidated_readers: u64,
    /// Reader observations inspected by the independent full-scan oracle.
    pub full_scan_reads: u64,
}

/// Work limits for one delta invalidation query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidationBudget {
    /// Maximum changed selectors accepted.
    pub max_changed_reads: u64,
    /// Maximum reverse-index probes.
    pub max_probes: u64,
    /// Maximum registration/change pairs returned by reverse arrangements.
    pub max_candidates: u64,
}

impl Default for InvalidationBudget {
    fn default() -> Self {
        Self {
            max_changed_reads: 65_536,
            max_probes: 1_000_000,
            max_candidates: 1_000_000,
        }
    }
}

/// Sorted result of an invalidation query and its measured work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidationReport<K: SemanticReaderKey = SemanticWorkKey> {
    /// Readers that must be reconsidered, sorted by immutable work key.
    pub readers: Vec<K>,
    /// Measured reverse-index work.
    pub counters: InvalidationCounters,
}

/// A changed dependency selector, recipe, or authority revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DependencyChange {
    /// One changed exact/range/prefix read selector.
    Read(ScopedRead),
    /// A changed recipe version.
    Recipe(RecipeVersion),
    /// A changed authority revision.
    Authority(AuthorityVersion),
}

impl<K: SemanticReaderKey> RetainedReaders<K> {
    /// Invalidates readers touched by changed selectors with default limits.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ReuseWorkLimit`] when the changed set or
    /// reverse-index work exceeds the default envelope.
    pub fn invalidate(
        &self,
        changed: &[ScopedRead],
    ) -> Result<InvalidationReport<K>, SemanticError> {
        self.invalidate_budgeted(changed, InvalidationBudget::default())
    }

    /// Invalidates readers using explicit checked work limits.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ReuseWorkLimit`] when any configured work
    /// limit is exceeded or [`SemanticError::Overflow`] on counter overflow.
    pub fn invalidate_budgeted(
        &self,
        changed: &[ScopedRead],
        budget: InvalidationBudget,
    ) -> Result<InvalidationReport<K>, SemanticError> {
        validate_changed_read_count(changed.len(), budget)?;
        let changed_reads = sorted_read_refs(changed.iter());
        self.invalidate_read_refs_budgeted(&changed_reads, budget)
    }

    /// Independent reference oracle that scans all retained observations.
    #[must_use]
    pub fn full_scan_invalidation(&self, changed: &[ScopedRead]) -> InvalidationReport<K> {
        let mut readers = BTreeSet::new();
        let mut counters = InvalidationCounters {
            changed_reads: changed.len().try_into().unwrap_or(u64::MAX),
            ..InvalidationCounters::default()
        };
        for (reader, observation) in self.relation_observations() {
            counters.full_scan_reads = counters.full_scan_reads.saturating_add(1);
            if changed
                .iter()
                .any(|candidate| observation.read().intersects(candidate))
            {
                readers.insert(reader);
            }
        }
        counters.invalidated_readers = readers.len().try_into().unwrap_or(u64::MAX);
        InvalidationReport {
            readers: readers.into_iter().collect(),
            counters,
        }
    }

    /// Checks indexed invalidation against the full-scan oracle.
    ///
    /// # Errors
    ///
    /// Returns the bounded read invalidation error from [`Self::invalidate`].
    pub fn equivalent_to_full_scan(&self, changed: &[ScopedRead]) -> Result<bool, SemanticError> {
        Ok(self.invalidate(changed)?.readers == self.full_scan_invalidation(changed).readers)
    }

    /// Invalidates readers for a mixed selector, recipe, and authority delta.
    ///
    /// Recipe and authority reverse arrangements are exact-key joins.  Read
    /// changes use the interval tree above and are verified against the
    /// retained observations before publication.
    ///
    /// # Errors
    ///
    /// Returns the bounded read invalidation error or a counter overflow.
    pub fn invalidate_changes(
        &self,
        changes: &[DependencyChange],
    ) -> Result<InvalidationReport<K>, SemanticError> {
        self.invalidate_changes_budgeted(changes, InvalidationBudget::default())
    }

    /// Invalidates readers for a mixed delta under an explicit work budget.
    ///
    /// Read selectors use the interval/exact arrangements. Recipe and
    /// authority changes use their exact reverse arrangements, with each
    /// bucket membership counted before it is joined into the result. This
    /// keeps a large fan-out bounded even when the changed selector list is
    /// small.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ReuseWorkLimit`] when a changed set, reverse
    /// probe, or candidate membership exceeds `budget`, or
    /// [`SemanticError::Overflow`] when a work counter cannot be represented.
    pub fn invalidate_changes_budgeted(
        &self,
        changes: &[DependencyChange],
        budget: InvalidationBudget,
    ) -> Result<InvalidationReport<K>, SemanticError> {
        let read_count = changes
            .iter()
            .filter(|change| matches!(change, DependencyChange::Read(_)))
            .count();
        validate_changed_read_count(read_count, budget)?;
        let reads = changes
            .iter()
            .filter_map(|change| match change {
                DependencyChange::Read(read) => Some(read),
                DependencyChange::Recipe(_) | DependencyChange::Authority(_) => None,
            })
            .collect::<Vec<_>>();
        let reads = sorted_read_refs(reads.into_iter());
        let mut report = self.invalidate_read_refs_budgeted(&reads, budget)?;
        let mut readers = report.readers;
        for change in changes {
            let users = match change {
                DependencyChange::Read(_) => None,
                DependencyChange::Recipe(recipe) => {
                    report.counters.exact_probes = checked_add(report.counters.exact_probes, 1)?;
                    if report.counters.exact_probes > budget.max_probes {
                        return Err(SemanticError::ReuseWorkLimit);
                    }
                    self.recipe_users(*recipe)
                }
                DependencyChange::Authority(authority) => {
                    report.counters.exact_probes = checked_add(report.counters.exact_probes, 1)?;
                    if report.counters.exact_probes > budget.max_probes {
                        return Err(SemanticError::ReuseWorkLimit);
                    }
                    self.authority_users(*authority)
                }
            };
            if let Some(users) = users {
                let next_candidate_count = checked_add(
                    report.counters.candidate_registrations,
                    u64::try_from(users.len()).map_err(|_| SemanticError::Overflow)?,
                )?;
                if next_candidate_count > budget.max_candidates {
                    return Err(SemanticError::ReuseWorkLimit);
                }
                readers.extend(users.iter().copied());
                report.counters.candidate_registrations = next_candidate_count;
            }
        }
        readers.sort_unstable();
        readers.dedup();
        report.counters.candidate_readers =
            u64::try_from(readers.len()).map_err(|_| SemanticError::Overflow)?;
        report.counters.invalidated_readers = report.counters.candidate_readers;
        report.readers = readers;
        Ok(report)
    }

    fn invalidate_read_refs_budgeted(
        &self,
        changed: &[&ScopedRead],
        budget: InvalidationBudget,
    ) -> Result<InvalidationReport<K>, SemanticError> {
        let mut counters = InvalidationCounters {
            changed_reads: validate_changed_read_count(changed.len(), budget)?,
            ..InvalidationCounters::default()
        };

        // Process one sorted selector at a time. This avoids retaining the
        // registration × changed-selector Cartesian product in nested trees;
        // the cumulative hit budget bounds both verification work and these
        // flat result vectors.
        let mut candidate_readers = Vec::new();
        let mut invalidated = Vec::new();
        for read in changed {
            let bucket = ReaderBucket {
                facet: read.facet(),
                scope: read.scope_root(),
            };
            let mut candidates = BTreeSet::new();
            if let ReadSelector::Exact(key) = read.selector() {
                counters.exact_probes = checked_add(counters.exact_probes, 1)?;
                if counters.exact_probes > budget.max_probes {
                    return Err(SemanticError::ReuseWorkLimit);
                }
                if let Some(readers) = self.exact_candidates(&bucket, key) {
                    let candidate_limit =
                        usize::try_from(budget.max_candidates).unwrap_or(usize::MAX);
                    for registration in readers {
                        if !candidates.contains(registration) && candidates.len() >= candidate_limit
                        {
                            return Err(SemanticError::ReuseWorkLimit);
                        }
                        candidates.insert(registration.clone());
                    }
                }
            }

            let (interval_start, interval_end) = selector_interval(read.selector());
            let probes_used = counters
                .exact_probes
                .checked_add(counters.interval_probes)
                .ok_or(SemanticError::Overflow)?;
            if probes_used > budget.max_probes {
                return Err(SemanticError::ReuseWorkLimit);
            }
            let probe_limit = budget.max_probes.saturating_sub(probes_used);
            let candidate_limit = usize::try_from(budget.max_candidates).unwrap_or(usize::MAX);
            let (matched, interval_probes) = match self.range_candidates(&bucket) {
                Some(tree) => tree.query(
                    &interval_start,
                    interval_end.as_deref(),
                    // Keep the per-query limit at the full budget because
                    // exact hits may be repeated in the interval arrangement.
                    candidate_limit,
                    probe_limit,
                )?,
                None => (BTreeSet::new(), 0),
            };
            counters.interval_probes = checked_add(counters.interval_probes, interval_probes)?;
            candidates.extend(matched);

            let hit_count = u64::try_from(candidates.len()).map_err(|_| SemanticError::Overflow)?;
            counters.candidate_registrations =
                checked_add(counters.candidate_registrations, hit_count)?;
            if counters.candidate_registrations > budget.max_candidates {
                return Err(SemanticError::ReuseWorkLimit);
            }

            for registration in candidates {
                candidate_readers.push(registration.reader);
                if let Some(observation) = self.observation(&registration) {
                    counters.verified_reads = checked_add(counters.verified_reads, 1)?;
                    if observation.read().intersects(read) {
                        invalidated.push(registration.reader);
                    }
                }
            }
            if counters
                .exact_probes
                .saturating_add(counters.interval_probes)
                > budget.max_probes
            {
                return Err(SemanticError::ReuseWorkLimit);
            }
        }

        candidate_readers.sort_unstable();
        candidate_readers.dedup();
        invalidated.sort_unstable();
        invalidated.dedup();
        counters.candidate_readers =
            u64::try_from(candidate_readers.len()).map_err(|_| SemanticError::Overflow)?;
        counters.invalidated_readers =
            u64::try_from(invalidated.len()).map_err(|_| SemanticError::Overflow)?;
        Ok(InvalidationReport {
            readers: invalidated,
            counters,
        })
    }

    /// Alias using the wording from the dependency planner.
    ///
    /// # Errors
    ///
    /// Returns the same error as [`Self::invalidate_changes`].
    pub fn invalidated_by(
        &self,
        changes: &[DependencyChange],
    ) -> Result<InvalidationReport<K>, SemanticError> {
        self.invalidate_changes(changes)
    }
}

/// Sorts borrowed read keys once so reverse lookups visit adjacent facet/scope
/// buckets together without cloning their selector bytes.
fn sorted_read_refs<'a>(reads: impl Iterator<Item = &'a ScopedRead>) -> Vec<&'a ScopedRead> {
    let mut reads = reads.collect::<Vec<_>>();
    reads.sort_unstable();
    reads
}

fn validate_changed_read_count(
    count: usize,
    budget: InvalidationBudget,
) -> Result<u64, SemanticError> {
    let count = u64::try_from(count).map_err(|_| SemanticError::Overflow)?;
    if count > budget.max_changed_reads {
        return Err(SemanticError::ReuseWorkLimit);
    }
    Ok(count)
}
