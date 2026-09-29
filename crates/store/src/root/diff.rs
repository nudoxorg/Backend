//! Defines diff behavior for `backend-store`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the diff invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{cmp::Ordering, iter::Peekable, ops::Deref};

use crate::root::entry::RootEntry;
use crate::root::packed::{CanonicalRows, GenerationRoot};

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

/// Streaming diff cursor over borrowed canonical rows.
///
/// It owns no change collection. Distinct roots use one unconsumed row from
/// each side; equal identities use one row cursor and still emit all
/// `Unchanged` items. Both paths use O(1) extra memory.
pub struct RootDiff<'older, 'newer, DomainTag> {
    cursor: RootDiffCursor<'older, 'newer, DomainTag>,
    metrics: RootDiffMetrics,
    descriptor_visits: usize,
}

enum RootDiffCursor<'older, 'newer, DomainTag> {
    /// Equal canonical identities let the full diff preserve its per-row
    /// `Unchanged` contract while avoiding a second root cursor and key merge.
    Equal(CanonicalRows<'newer, DomainTag>),
    Merge {
        older_rows: Peekable<CanonicalRows<'older, DomainTag>>,
        newer_rows: Peekable<CanonicalRows<'newer, DomainTag>>,
    },
}

/// Read-only work accounting for one streaming root comparison.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RootDiffMetrics {
    /// Semantic-key comparisons performed so far.
    pub comparisons: usize,
}

impl<DomainTag> RootDiff<'_, '_, DomainTag> {
    /// Returns the number of canonical row descriptors visited so far.
    #[must_use]
    pub const fn descriptor_visits(&self) -> usize {
        self.descriptor_visits
    }
}

impl<DomainTag> Deref for RootDiff<'_, '_, DomainTag> {
    type Target = RootDiffMetrics;

    fn deref(&self) -> &Self::Target {
        &self.metrics
    }
}

/// Changed-only streaming view over a canonical root merge.
///
/// For distinct roots, unchanged rows remain in the underlying merge but
/// never become output items. Equal root IDs finish immediately without
/// visiting rows because the canonical identity already commits their
/// complete semantic descriptors.
pub struct ChangedRootDiff<'older, 'newer, DomainTag> {
    diff: RootDiff<'older, 'newer, DomainTag>,
    finished: bool,
}

impl<DomainTag> Deref for ChangedRootDiff<'_, '_, DomainTag> {
    type Target = RootDiffMetrics;

    fn deref(&self) -> &Self::Target {
        &self.diff.metrics
    }
}

impl<DomainTag> ChangedRootDiff<'_, '_, DomainTag> {
    /// Returns the number of canonical row descriptors visited so far.
    #[must_use]
    pub const fn descriptor_visits(&self) -> usize {
        self.diff.descriptor_visits
    }
}

impl<DomainTag> Iterator for RootDiff<'_, '_, DomainTag> {
    type Item = RootChange<DomainTag>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.cursor {
            RootDiffCursor::Equal(rows) => match rows.next() {
                Some(row) => {
                    self.descriptor_visits += 1;
                    Some(RootChange::Unchanged { entry: row.entry })
                }
                None => None,
            },
            RootDiffCursor::Merge {
                older_rows,
                newer_rows,
            } => match (older_rows.peek(), newer_rows.peek()) {
                (Some(older), Some(newer)) => {
                    self.metrics.comparisons += 1;
                    self.descriptor_visits += 2;
                    match older.entry.key.cmp(&newer.entry.key) {
                        Ordering::Less => older_rows
                            .next()
                            .map(|row| RootChange::Removed { old: row.entry }),
                        Ordering::Greater => newer_rows
                            .next()
                            .map(|row| RootChange::Added { new: row.entry }),
                        Ordering::Equal => {
                            let older = older_rows.next();
                            let newer = newer_rows.next();
                            match (older, newer) {
                                (Some(older), Some(newer)) => {
                                    Some(classify_same_key(older.entry, newer.entry))
                                }
                                (Some(older), None) => {
                                    Some(RootChange::Removed { old: older.entry })
                                }
                                (None, Some(newer)) => Some(RootChange::Added { new: newer.entry }),
                                (None, None) => None,
                            }
                        }
                    }
                }
                (Some(_), None) => older_rows.next().map(|row| {
                    self.descriptor_visits += 1;
                    RootChange::Removed { old: row.entry }
                }),
                (None, Some(_)) => newer_rows.next().map(|row| {
                    self.descriptor_visits += 1;
                    RootChange::Added { new: row.entry }
                }),
                (None, None) => None,
            },
        }
    }
}

impl<DomainTag> Iterator for ChangedRootDiff<'_, '_, DomainTag> {
    type Item = RootChange<DomainTag>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
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
    /// Compares canonical rows without payload reads.
    ///
    /// Equal generation IDs select a single-root cursor that emits one
    /// `Unchanged` item per canonical row. Root identity hashes the row count,
    /// each key and parent, and each object's content ID, length, schema, and
    /// kind. Locality and payload residence are stored separately.
    #[must_use]
    pub fn diff<'older, 'newer>(
        &'older self,
        newer: &'newer Self,
    ) -> RootDiff<'older, 'newer, DomainTag> {
        let cursor = if self.id == newer.id {
            RootDiffCursor::Equal(newer.canonical_rows())
        } else {
            RootDiffCursor::Merge {
                older_rows: self.canonical_rows().peekable(),
                newer_rows: newer.canonical_rows().peekable(),
            }
        };
        RootDiff {
            cursor,
            metrics: RootDiffMetrics::default(),
            descriptor_visits: 0,
        }
    }

    /// Streams only additions, removals, content changes, and reparentings.
    #[must_use]
    pub fn changed_diff<'older, 'newer>(
        &'older self,
        newer: &'newer Self,
    ) -> ChangedRootDiff<'older, 'newer, DomainTag> {
        // GenerationRoot can only be created by the validating builder, and
        // its ID commits the complete semantic descriptor set. Locality and
        // payload residence are a separately bound axis, not root identity.
        let equal_roots = self.id == newer.id;
        ChangedRootDiff {
            diff: self.diff(newer),
            finished: equal_roots,
        }
    }
}
