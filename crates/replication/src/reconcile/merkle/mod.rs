//! Bounded Merkle-closure reconciliation.
//!
//! Flat root summaries are useful for tiny manifests, but they force peers to
//! materialize and compare every object even when most of two versions share
//! immutable subtrees.  This module makes the common path explicit: compare
//! node commitments, descend only unequal pairs, and emit changed leaf rows
//! in bounded delta pages.  The cursor is resumable and contains only the
//! outstanding subtree frontier, never a complete object map.

use std::cmp::Ordering;

use crate::ReplicationError;

mod model;
pub use model::*;
mod page;

mod traversal;
use traversal::{
    PageBudget, advance_empty_page, advance_shorter_branch_child, child_end, child_pair_bounds,
    fetch_page, normalize_leaf_index, page_children, page_entries,
};

/// A bounded Merkle reconciler retaining only its unequal subtree frontier.
#[derive(Debug)]
pub struct MerkleReconciler {
    cursor: MerkleDiffCursor,
}
impl MerkleReconciler {
    /// Starts reconciliation between two exact immutable roots.
    ///
    /// # Errors
    ///
    /// Returns the root-schema or budget error reported by
    /// [`MerkleDiffCursor::new`].
    pub fn new(
        local_root: MerkleRoot,
        remote_root: MerkleRoot,
        budget: ReconcileBudget,
    ) -> Result<Self, ReplicationError> {
        budget.validate()?;
        Ok(Self {
            cursor: MerkleDiffCursor::new(local_root, remote_root, budget)?,
        })
    }

    /// Returns a cloneable continuation suitable for durable reconnect state.
    #[must_use]
    pub fn cursor(&self) -> MerkleDiffCursor {
        self.cursor.clone()
    }

    /// Returns whether the traversal has exhausted every unequal subtree.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.cursor.complete
    }

    /// Resumes from a caller-provided cursor after validating its bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::CoverageLimit`] or
    /// [`ReplicationError::InvalidLimits`] when the cursor cannot be retained
    /// within the supplied frontier and key bounds.
    pub fn from_cursor(
        cursor: MerkleDiffCursor,
        budget: ReconcileBudget,
    ) -> Result<Self, ReplicationError> {
        budget.validate()?;
        if cursor.local_root.schema() != cursor.remote_root.schema() {
            return Err(ReplicationError::IdentityContext);
        }
        if cursor.pending.len() > budget.max_pending
            || cursor.pending.iter().any(|pair| {
                pair.range_start
                    .as_ref()
                    .is_some_and(|key| key.len() > budget.max_key_bytes)
                    || pair
                        .range_end
                        .as_ref()
                        .is_some_and(|key| key.len() > budget.max_key_bytes)
            })
            || cursor.complete != cursor.pending.is_empty()
        {
            return Err(ReplicationError::CoverageLimit);
        }
        Ok(Self { cursor })
    }

    /// Compares unequal Merkle nodes and emits at most one bounded delta page.
    ///
    /// Equal node commitments are skipped without fetching children.  Only
    /// unequal branch pairs enter the continuation frontier, so memory is
    /// proportional to the active proof frontier rather than the complete
    /// closure size.
    ///
    /// # Errors
    ///
    /// Returns a source, proof, ordering, identity, or budget error when a
    /// page cannot be admitted or the frontier would exceed its bound.
    // This transition loop deliberately keeps page fetch, interval alignment,
    // and cursor mutation together so a pause can return one transactional
    // continuation without exposing partially advanced state.
    #[allow(clippy::too_many_lines)]
    pub fn step<L: MerklePageSource, R: MerklePageSource>(
        &mut self,
        local: &mut L,
        remote: &mut R,
        budget: ReconcileBudget,
    ) -> Result<MerkleDeltaPage, ReplicationError> {
        budget.validate()?;
        if self.cursor.complete {
            return Ok(MerkleDeltaPage {
                deltas: Vec::new(),
                next: None,
            });
        }
        let mut deltas = Vec::new();
        let mut page_budget = PageBudget::new(self.cursor.pages);
        while !self.cursor.pending.is_empty() && deltas.len() < budget.max_deltas {
            let Some(mut pair) = self.cursor.pending.pop_front() else {
                self.cursor.complete = true;
                break;
            };
            if pair.local == pair.remote {
                continue;
            }

            // Keep page bodies only for the duration of this step.  The
            // cursor stores offsets into them, so a pause at the delta or
            // page budget boundary can refetch exactly the unconsumed suffix
            // without retaining a page-sized allocation in durable state.
            let mut left_page = None;
            let mut right_page = None;
            let mut paused = false;
            let mut finished = false;

            loop {
                if pair.local_exhausted && pair.remote_exhausted {
                    finished = true;
                    break;
                }

                if left_page.is_none() && !pair.local_exhausted {
                    if page_budget.used >= budget.max_pages {
                        paused = true;
                        break;
                    }
                    left_page = Some(fetch_page(
                        local,
                        self.cursor.local_root,
                        pair.local,
                        pair.local_cursor,
                        &mut pair,
                        &mut page_budget,
                        budget,
                    )?);
                }
                if right_page.is_none() && !pair.remote_exhausted {
                    if page_budget.used >= budget.max_pages {
                        paused = true;
                        break;
                    }
                    right_page = Some(fetch_page(
                        remote,
                        self.cursor.remote_root,
                        pair.remote,
                        pair.remote_cursor,
                        &mut pair,
                        &mut page_budget,
                        budget,
                    )?);
                }

                let kind = pair.kind.ok_or(ReplicationError::InvalidWire)?;
                match kind {
                    NodeKind::Branch => {
                        let left_children = page_children(left_page.as_ref())?;
                        let right_children = page_children(right_page.as_ref())?;

                        if advance_empty_page(
                            &mut pair.local_cursor,
                            &mut pair.local_index,
                            &mut pair.local_exhausted,
                            left_page.as_ref(),
                            left_children.len(),
                        ) {
                            left_page = None;
                            continue;
                        }
                        if advance_empty_page(
                            &mut pair.remote_cursor,
                            &mut pair.remote_index,
                            &mut pair.remote_exhausted,
                            right_page.as_ref(),
                            right_children.len(),
                        ) {
                            right_page = None;
                            continue;
                        }
                        if pair.local_exhausted || pair.remote_exhausted {
                            continue;
                        }
                        let left = &left_children[pair.local_index];
                        let right = &right_children[pair.remote_index];
                        if left.digest != right.digest
                            && let Some((range_start, range_end)) = child_pair_bounds(
                                pair.range_start.as_deref(),
                                pair.range_end.as_deref(),
                                &left.first_key,
                                child_end(left_children, pair.local_index),
                                &right.first_key,
                                child_end(right_children, pair.remote_index),
                            )
                        {
                            self.cursor.pending.push_back(SubtreePair {
                                local: left.digest,
                                remote: right.digest,
                                local_cursor: PageCursor::origin(),
                                remote_cursor: PageCursor::origin(),
                                local_index: 0,
                                remote_index: 0,
                                local_exhausted: false,
                                remote_exhausted: false,
                                kind: None,
                                level: None,
                                range_start: Some(range_start),
                                range_end,
                            });
                            if self.cursor.pending.len() > budget.max_pending {
                                return Err(ReplicationError::CoverageLimit);
                            }
                        }
                        advance_shorter_branch_child(&mut pair, left_children, right_children);
                    }
                    NodeKind::Leaf => {
                        let left_entries = page_entries(left_page.as_ref())?;
                        let right_entries = page_entries(right_page.as_ref())?;

                        if advance_empty_page(
                            &mut pair.local_cursor,
                            &mut pair.local_index,
                            &mut pair.local_exhausted,
                            left_page.as_ref(),
                            left_entries.len(),
                        ) {
                            left_page = None;
                            continue;
                        }
                        if advance_empty_page(
                            &mut pair.remote_cursor,
                            &mut pair.remote_index,
                            &mut pair.remote_exhausted,
                            right_page.as_ref(),
                            right_entries.len(),
                        ) {
                            right_page = None;
                            continue;
                        }
                        if pair.local_exhausted && pair.remote_exhausted {
                            continue;
                        }
                        let local_range_done = normalize_leaf_index(
                            left_entries,
                            &mut pair.local_index,
                            pair.range_start.as_deref(),
                            pair.range_end.as_deref(),
                        );
                        if local_range_done {
                            pair.local_exhausted = true;
                            left_page = None;
                            continue;
                        }
                        let remote_range_done = normalize_leaf_index(
                            right_entries,
                            &mut pair.remote_index,
                            pair.range_start.as_deref(),
                            pair.range_end.as_deref(),
                        );
                        if remote_range_done {
                            pair.remote_exhausted = true;
                            right_page = None;
                            continue;
                        }
                        // Skipping a prefix can consume this page just as
                        // completely as emitting its final row. Advance to
                        // its continuation before exposing an entry to the
                        // merge below.
                        if pair.local_index >= left_entries.len()
                            && advance_empty_page(
                                &mut pair.local_cursor,
                                &mut pair.local_index,
                                &mut pair.local_exhausted,
                                left_page.as_ref(),
                                left_entries.len(),
                            )
                        {
                            left_page = None;
                            continue;
                        }
                        if pair.remote_index >= right_entries.len()
                            && advance_empty_page(
                                &mut pair.remote_cursor,
                                &mut pair.remote_index,
                                &mut pair.remote_exhausted,
                                right_page.as_ref(),
                                right_entries.len(),
                            )
                        {
                            right_page = None;
                            continue;
                        }
                        let left = left_entries.get(pair.local_index);
                        let right = right_entries.get(pair.remote_index);
                        match (left, right) {
                            (Some(left), Some(right)) => match left.key.cmp(&right.key) {
                                Ordering::Less => {
                                    deltas.push(MerkleDelta {
                                        key: left.key.clone(),
                                        local: Some(left.clone()),
                                        remote: None,
                                    });
                                    pair.local_index += 1;
                                }
                                Ordering::Greater => {
                                    deltas.push(MerkleDelta {
                                        key: right.key.clone(),
                                        local: None,
                                        remote: Some(right.clone()),
                                    });
                                    pair.remote_index += 1;
                                }
                                Ordering::Equal => {
                                    if left != right {
                                        deltas.push(MerkleDelta {
                                            key: right.key.clone(),
                                            local: Some(left.clone()),
                                            remote: Some(right.clone()),
                                        });
                                    }
                                    pair.local_index += 1;
                                    pair.remote_index += 1;
                                }
                            },
                            (Some(left), None) => {
                                deltas.push(MerkleDelta {
                                    key: left.key.clone(),
                                    local: Some(left.clone()),
                                    remote: None,
                                });
                                pair.local_index += 1;
                            }
                            (None, Some(right)) => {
                                deltas.push(MerkleDelta {
                                    key: right.key.clone(),
                                    local: None,
                                    remote: Some(right.clone()),
                                });
                                pair.remote_index += 1;
                            }
                            (None, None) => continue,
                        }
                        if deltas.len() >= budget.max_deltas {
                            paused = true;
                            break;
                        }
                    }
                }
            }

            if !finished && (paused || !pair.local_exhausted || !pair.remote_exhausted) {
                self.cursor.pending.push_front(pair);
                break;
            }
        }
        self.cursor.pages = page_budget.total;
        self.cursor.complete = self.cursor.pending.is_empty();
        let next = (!self.cursor.complete).then(|| self.cursor.clone());
        Ok(MerkleDeltaPage { deltas, next })
    }
}

#[cfg(test)]
mod tests;
