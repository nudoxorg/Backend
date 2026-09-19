//! Bounded subscriber queues and complete-view cursors.

use crate::{ArrangementRoot, CanonicalValue, Delta, Frontier, RowRef};
use std::{collections::VecDeque, fmt::Debug, marker::PhantomData, mem::size_of};

/// A bounded page from a complete arrangement snapshot.
///
/// Pages own only the rows requested by the caller.  The root and sequence
/// fence make it possible for a consumer to retain the page while checking
/// that subsequent pages belong to the same immutable view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotPage<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    root: ArrangementRoot<V>,
    sequence: u64,
    offset: usize,
    next_offset: Option<usize>,
    rows: Vec<Delta<V>>,
}

/// A checked description of a complete reset view.
///
/// The descriptor carries the immutable root and sequence fence plus the
/// maintained visible-row count.  It intentionally owns no rows; a consumer
/// must request pages through the arrangement's budgeted reset API.
#[derive(Debug, Eq, PartialEq)]
pub struct SnapshotDescriptor<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    root: ArrangementRoot<V>,
    sequence: u64,
    len: usize,
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Copy for SnapshotDescriptor<V> {}

#[allow(
    clippy::expl_impl_clone_on_copy,
    reason = "the descriptor's generic marker does not own a value and clones by copying its fence"
)]
impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Clone for SnapshotDescriptor<V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> SnapshotDescriptor<V> {
    pub(crate) const fn new(root: ArrangementRoot<V>, sequence: u64, len: usize) -> Self {
        Self {
            root,
            sequence,
            len,
        }
    }

    /// Returns the immutable root represented by this descriptor.
    #[must_use]
    pub const fn root(&self) -> ArrangementRoot<V> {
        self.root
    }

    /// Returns the producer sequence represented by this descriptor.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the maintained number of rows in the complete view.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the complete view contains no visible rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> SnapshotPage<V> {
    pub(crate) fn new(
        root: ArrangementRoot<V>,
        sequence: u64,
        offset: usize,
        next_offset: Option<usize>,
        rows: Vec<Delta<V>>,
    ) -> Self {
        Self {
            root,
            sequence,
            offset,
            next_offset,
            rows,
        }
    }

    /// Returns the immutable root represented by this page.
    #[must_use]
    pub const fn root(&self) -> ArrangementRoot<V> {
        self.root
    }

    /// Returns the producer sequence represented by this page.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the zero-based ordinal of the first row in this page.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the next page offset, or `None` when this page is complete.
    #[must_use]
    pub const fn next_offset(&self) -> Option<usize> {
        self.next_offset
    }

    /// Borrows the rows owned by this page.
    #[must_use]
    pub fn rows(&self) -> &[Delta<V>] {
        &self.rows
    }

    /// Consumes the page and returns its bounded row payload.
    #[must_use]
    pub fn into_rows(self) -> Vec<Delta<V>> {
        self.rows
    }
}

/// A lazy complete-view cursor.
///
/// The cursor retains the authenticated root/sequence fence and walks the
/// persistent relation only as pages are requested.  It never allocates the
/// complete visible row set.
pub struct SnapshotCursor<'a, V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    rows: Box<dyn Iterator<Item = RowRef<'a, V>> + 'a>,
    root: ArrangementRoot<V>,
    sequence: u64,
    total_len: usize,
    offset: usize,
}

impl<'a, V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> SnapshotCursor<'a, V> {
    pub(crate) fn new(
        rows: impl Iterator<Item = RowRef<'a, V>> + 'a,
        root: ArrangementRoot<V>,
        sequence: u64,
        total_len: usize,
    ) -> Self {
        Self {
            rows: Box::new(rows),
            root,
            sequence,
            total_len,
            offset: 0,
        }
    }
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> SnapshotCursor<'_, V> {
    /// Returns the immutable root represented by this cursor.
    #[must_use]
    pub const fn root(&self) -> ArrangementRoot<V> {
        self.root
    }

    /// Returns the producer sequence represented by this cursor.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the offset of the next row that will be emitted.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    pub(crate) fn skip_rows(
        &mut self,
        count: usize,
        scope: &mut crate::WorkScope,
    ) -> Result<(), crate::FlowError> {
        let needed = count.min(self.total_len.saturating_sub(self.offset));
        if u64::try_from(needed).map_err(|_| crate::FlowError::Overflow)? > scope.remaining_probes()
        {
            return Err(crate::FlowError::RecursionWorkLimit);
        }
        for _ in 0..needed {
            let Some(_) = self.rows.next() else {
                break;
            };
            scope.charge_probes(1)?;
            self.offset = self
                .offset
                .checked_add(1)
                .ok_or(crate::FlowError::Overflow)?;
        }
        Ok(())
    }

    /// Reads one bounded page and charges emitted rows and bytes to `scope`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::FlowError::InvalidSubscriptionLimit`] for a zero page
    /// size, or [`crate::FlowError::RecursionWorkLimit`] when the page exceeds
    /// the supplied work envelope.
    pub fn next_page(
        &mut self,
        limit: usize,
        scope: &mut crate::WorkScope,
    ) -> Result<SnapshotPage<V>, crate::FlowError> {
        if limit == 0 {
            return Err(crate::FlowError::InvalidSubscriptionLimit);
        }
        let offset = self.offset;
        let remaining = self.total_len.saturating_sub(offset);
        let needed = limit.min(remaining);
        if u64::try_from(needed).map_err(|_| crate::FlowError::Overflow)? > scope.remaining_rows() {
            return Err(crate::FlowError::RecursionWorkLimit);
        }
        let mut rows = Vec::with_capacity(needed);
        for _ in 0..needed {
            let Some(row) = self.rows.next() else {
                break;
            };
            let bytes = size_of::<Delta<V>>()
                .checked_add(row.value.owned_bytes())
                .ok_or(crate::FlowError::Overflow)?;
            scope.charge_row_bytes(1, bytes)?;
            rows.push(Delta {
                key: row.key,
                value: row.value.clone(),
                time: row.time,
                diff: row.diff,
            });
            self.offset = self
                .offset
                .checked_add(1)
                .ok_or(crate::FlowError::Overflow)?;
        }
        let next_offset = (self.offset < self.total_len).then_some(self.offset);
        Ok(SnapshotPage::new(
            self.root,
            self.sequence,
            offset,
            next_offset,
            rows,
        ))
    }
}

/// Immutable event delivered to a root/sequence subscriber.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionEvent<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    /// Exact transition from one observed root to the next.
    Delta {
        /// Monotone output sequence.
        sequence: u64,
        /// Target root after applying the transition.
        root: ArrangementRoot<V>,
        /// Consolidated changes.
        deltas: Vec<Delta<V>>,
    },
    /// Complete replacement required after a gap or root mismatch.
    Reset {
        /// Root, sequence, and row-count fence for the replacement view.
        snapshot: SnapshotDescriptor<V>,
    },
    /// Progress update without row changes.
    Progress {
        /// Current execution frontier.
        frontier: Frontier,
    },
}

/// A bounded subscription queue.
#[derive(Clone, Debug)]
pub struct Subscription<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    pub(crate) events: VecDeque<SubscriptionEvent<V>>,
    pub(crate) root: ArrangementRoot<V>,
    pub(crate) sequence: u64,
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Subscription<V> {
    /// Returns the root represented by all events that have been consumed.
    #[must_use]
    pub const fn root(&self) -> ArrangementRoot<V> {
        self.root
    }

    /// Returns the sequence represented by all events that have been consumed.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Pops one bounded event.
    pub fn poll(&mut self) -> Option<SubscriptionEvent<V>> {
        let event = self.events.pop_front()?;
        match &event {
            SubscriptionEvent::Delta { sequence, root, .. } => {
                self.sequence = *sequence;
                self.root = *root;
            }
            SubscriptionEvent::Reset { snapshot } => {
                self.sequence = snapshot.sequence();
                self.root = snapshot.root();
            }
            SubscriptionEvent::Progress { .. } => {}
        }
        Some(event)
    }

    /// Returns the number of queued events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns whether no event is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// A root and sequence fenced complete-view cursor.
pub struct SubscriberCursor<'a, V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> {
    pub(crate) rows: Box<dyn Iterator<Item = RowRef<'a, V>> + 'a>,
    pub(crate) root: ArrangementRoot<V>,
    pub(crate) sequence: u64,
    pub(crate) pos: usize,
    pub(crate) _marker: PhantomData<&'a V>,
}

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> SubscriberCursor<'_, V> {
    /// Returns the complete root represented by this cursor.
    #[must_use]
    pub const fn root(&self) -> ArrangementRoot<V> {
        self.root
    }

    /// Returns the sequence represented by this cursor.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Borrows the next complete-view row.
    pub fn next_row(&mut self) -> Option<RowRef<'_, V>> {
        let row = self.rows.next()?;
        self.pos = self.pos.checked_add(1)?;
        Some(row)
    }
}
