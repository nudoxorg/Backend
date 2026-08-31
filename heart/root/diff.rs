//! Defines diff behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the diff invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{cmp::Ordering, iter::Peekable, ops::Deref};

use crate::entry::RootEntry;
use crate::packed::{CanonicalRows, GenerationRoot};

/// One exact structural classification from a canonical linear root merge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootChange<DomainTag> {
    /// Semantic key appeared.
    Added {
        /// New entry.
        new: RootEntry<DomainTag>,
    },
    /// Semantic key disappeared.
    Removed {
        /// Old entry.
        old: RootEntry<DomainTag>,
    },
    /// Key remained but logical content descriptor changed.
    ContentChanged {
        /// Previous entry.
        old: RootEntry<DomainTag>,
        /// Replacement entry.
        new: RootEntry<DomainTag>,
    },
    /// Key and object remained but checked parent changed.
    Reparented {
        /// Previous entry.
        old: RootEntry<DomainTag>,
        /// Moved entry.
        new: RootEntry<DomainTag>,
    },
    /// Key, parent and object descriptor remained; residence changes do not alter semantics.
    Unchanged {
        /// Canonical semantic entry.
        entry: RootEntry<DomainTag>,
    },
}

/// Streaming linear merge cursor over two borrowed canonical row cursors.
///
/// It owns no change collection. Its only state is one unconsumed borrowed
/// canonical row from each root, so comparison remains O(1) extra memory.
pub struct RootDiff<'older, 'newer, DomainTag> {
    older_rows: Peekable<CanonicalRows<'older, DomainTag>>,
    newer_rows: Peekable<CanonicalRows<'newer, DomainTag>>,
    metrics: RootDiffMetrics,
}

/// Read-only work accounting for one streaming root comparison.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RootDiffMetrics {
    /// Semantic-key comparisons performed so far.
    pub comparisons: usize,
}

impl<DomainTag> Deref for RootDiff<'_, '_, DomainTag> {
    type Target = RootDiffMetrics;

    fn deref(&self) -> &Self::Target {
        &self.metrics
    }
}

/// Changed-only streaming view over a canonical root merge.
///
/// Unchanged rows remain in the underlying merge for its exact linear
/// comparison accounting, but never become output allocations or items.
pub struct ChangedRootDiff<'older, 'newer, DomainTag> {
    diff: RootDiff<'older, 'newer, DomainTag>,
}

impl<DomainTag> Deref for ChangedRootDiff<'_, '_, DomainTag> {
    type Target = RootDiffMetrics;

    fn deref(&self) -> &Self::Target {
        &self.diff.metrics
    }
}

impl<DomainTag> Iterator for RootDiff<'_, '_, DomainTag> {
    type Item = RootChange<DomainTag>;

    fn next(&mut self) -> Option<Self::Item> {
        match (self.older_rows.peek(), self.newer_rows.peek()) {
            (Some(older), Some(newer)) => {
                self.metrics.comparisons += 1;
                match older.entry.key.cmp(&newer.entry.key) {
                    Ordering::Less => self
                        .older_rows
                        .next()
                        .map(|row| RootChange::Removed { old: row.entry }),
                    Ordering::Greater => self
                        .newer_rows
                        .next()
                        .map(|row| RootChange::Added { new: row.entry }),
                    Ordering::Equal => {
                        let older = self.older_rows.next();
                        let newer = self.newer_rows.next();
                        match (older, newer) {
                            (Some(older), Some(newer)) => {
                                Some(classify_same_key(older.entry, newer.entry))
                            }
                            (Some(older), None) => Some(RootChange::Removed { old: older.entry }),
                            (None, Some(newer)) => Some(RootChange::Added { new: newer.entry }),
                            (None, None) => None,
                        }
                    }
                }
            }
            (Some(_), None) => self
                .older_rows
                .next()
                .map(|row| RootChange::Removed { old: row.entry }),
            (None, Some(_)) => self
                .newer_rows
                .next()
                .map(|row| RootChange::Added { new: row.entry }),
            (None, None) => None,
        }
    }
}

impl<DomainTag> Iterator for ChangedRootDiff<'_, '_, DomainTag> {
    type Item = RootChange<DomainTag>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let change = self.diff.next()?;
            if !matches!(change, RootChange::Unchanged { .. }) {
                return Some(change);
            }
        }
    }
}

fn classify_same_key<DomainTag>(
    older: RootEntry<DomainTag>,
    newer: RootEntry<DomainTag>,
) -> RootChange<DomainTag> {
    if older.object != newer.object {
        RootChange::ContentChanged {
            old: older,
            new: newer,
        }
    } else if older.parent != newer.parent {
        RootChange::Reparented {
            old: older,
            new: newer,
        }
    } else {
        RootChange::Unchanged { entry: newer }
    }
}

impl<DomainTag> GenerationRoot<DomainTag> {
    /// Compares canonical rows in O(old + new) descriptor work and no payload reads.
    #[must_use]
    pub fn diff<'older, 'newer>(
        &'older self,
        newer: &'newer Self,
    ) -> RootDiff<'older, 'newer, DomainTag> {
        RootDiff {
            older_rows: self.canonical_rows().peekable(),
            newer_rows: newer.canonical_rows().peekable(),
            metrics: RootDiffMetrics::default(),
        }
    }

    /// Streams only additions, removals, content changes, and reparentings.
    #[must_use]
    pub fn changed_diff<'older, 'newer>(
        &'older self,
        newer: &'newer Self,
    ) -> ChangedRootDiff<'older, 'newer, DomainTag> {
        ChangedRootDiff {
            diff: self.diff(newer),
        }
    }
}
