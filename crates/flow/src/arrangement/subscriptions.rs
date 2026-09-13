//! Arrangement checkpoints and version-fenced subscriptions.

use super::Arrangement;
use super::checkpoint::TraceCheckpoint;
use super::subscription::{
    SnapshotCursor, SnapshotDescriptor, SnapshotPage, SubscriberCursor, Subscription,
    SubscriptionEvent,
};
use crate::{ArrangementRoot, CanonicalValue, Delta, FlowError, LayoutId, WorkScope};
use std::{collections::VecDeque, fmt::Debug, marker::PhantomData, mem::size_of};

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Arrangement<V> {
    /// Returns a physical checkpoint descriptor for the current trace.
    #[must_use]
    pub fn checkpoint(&self, layout: LayoutId) -> TraceCheckpoint<V> {
        let retained_rows = self
            .levels
            .iter()
            .flat_map(|level| level.iter())
            .fold(0_usize, |rows, run| rows.saturating_add(run.len()));
        let base_segments = self
            .levels
            .iter()
            .skip(1)
            .flat_map(|level| level.iter())
            .map(|run| run.root())
            .collect();
        let delta_runs = self
            .levels
            .first()
            .into_iter()
            .flat_map(|level| level.iter())
            .map(|run| run.root())
            .collect();
        TraceCheckpoint {
            layout,
            base_segments,
            delta_runs,
            upper: self.trace.upper(),
            since: self.trace.since(),
            since_frontier: self.trace.since_frontier(),
            logical_root: self.root(),
            retained_rows: u64::try_from(retained_rows).map_or(u64::MAX, |rows| rows),
            _marker: PhantomData,
        }
    }

    /// Starts a snapshot cursor at the current root and output sequence.
    #[must_use]
    pub fn subscribe(&self) -> SubscriberCursor<'_, V> {
        SubscriberCursor {
            rows: Box::new(self.visible_rows_iter()),
            root: self.root(),
            sequence: self.next_batch,
            pos: 0,
            _marker: PhantomData,
        }
    }

    /// Opens a lazy cursor over the current complete view.
    ///
    /// The cursor retains only the root/sequence fence and a persistent-tree
    /// iterator.  Rows are borrowed and cloned into bounded pages on demand.
    #[must_use]
    pub fn snapshot_cursor(&self) -> SnapshotCursor<'_, V> {
        SnapshotCursor::new(
            self.visible_rows_iter(),
            self.root(),
            self.next_batch,
            self.visible_len,
        )
    }

    /// Reads one bounded page of the current complete view.
    ///
    /// This convenience form charges a page-sized row/byte envelope and an
    /// offset-sized probe envelope.  Call [`Self::snapshot_cursor`] when a
    /// consumer is fetching many pages so skipped prefixes are not revisited.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidSubscriptionLimit`] for a zero page size,
    /// or [`FlowError::RecursionWorkLimit`] when the requested page or offset
    /// exceeds the implicit envelope.
    pub fn snapshot_page(&self, offset: usize, limit: usize) -> Result<SnapshotPage<V>, FlowError> {
        let bytes = limit.saturating_mul(size_of::<Delta<V>>());
        let mut scope = WorkScope::with_probes(limit, bytes, offset);
        self.snapshot_page_budgeted(offset, limit, &mut scope)
    }

    /// Reads one bounded page while charging all emitted rows and prefix
    /// probes to `scope`.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidSubscriptionLimit`] for a zero page size,
    /// or [`FlowError::RecursionWorkLimit`] when the page exceeds `scope`.
    pub fn snapshot_page_budgeted(
        &self,
        offset: usize,
        limit: usize,
        scope: &mut WorkScope,
    ) -> Result<SnapshotPage<V>, FlowError> {
        let mut cursor = self.snapshot_cursor();
        cursor.skip_rows(offset, scope)?;
        cursor.next_page(limit, scope)
    }

    /// Alias used by reset protocols that fetch replacement pages lazily.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidSubscriptionLimit`] for a zero page size or
    /// [`FlowError::RecursionWorkLimit`] when the implicit page envelope is
    /// exceeded.
    pub fn reset_subscription_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<SnapshotPage<V>, FlowError> {
        self.snapshot_page(offset, limit)
    }

    /// Budgeted alias used by reset protocols that fetch replacement pages
    /// lazily.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::InvalidSubscriptionLimit`] for a zero page size or
    /// the supplied scope's bounded-work error when the page exceeds it.
    pub fn reset_subscription_page_budgeted(
        &self,
        offset: usize,
        limit: usize,
        scope: &mut WorkScope,
    ) -> Result<SnapshotPage<V>, FlowError> {
        self.snapshot_page_budgeted(offset, limit, scope)
    }

    /// Returns a root-fenced descriptor for a complete reset view.
    ///
    /// Creating the descriptor is O(1) and retains no visible rows.  Use
    /// [`Self::reset_snapshot_page_budgeted`] for each bounded payload page.
    #[must_use]
    pub fn reset_snapshot(&self) -> SnapshotDescriptor<V> {
        SnapshotDescriptor::new(self.root(), self.next_batch, self.visible_len)
    }

    /// Opens a lazy cursor for a previously negotiated reset descriptor.
    ///
    /// The cursor keeps the descriptor's root/sequence fence and borrows the
    /// persistent relation.  Pages fetched from it are charged by
    /// [`SnapshotCursor::next_page`], so sequential reset delivery never
    /// needs to rescan or retain the complete view.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ResetRequired`] when the descriptor is stale or
    /// belongs to another arrangement.
    pub fn reset_snapshot_cursor(
        &self,
        snapshot: SnapshotDescriptor<V>,
    ) -> Result<SnapshotCursor<'_, V>, FlowError> {
        if snapshot.root() != self.root() || snapshot.sequence() != self.next_batch {
            return Err(FlowError::ResetRequired);
        }
        if snapshot.len() != self.visible_len {
            return Err(FlowError::ResetRequired);
        }
        Ok(self.snapshot_cursor())
    }

    /// Reads one page for a previously negotiated reset descriptor.
    ///
    /// The root and sequence fence are checked before any cursor work.  The
    /// supplied scope charges prefix probes, emitted rows, and their encoded
    /// bytes atomically, so a caller cannot obtain an over-budget page.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::ResetRequired`] when the descriptor is stale or
    /// belongs to another arrangement, then the usual page budget errors.
    pub fn reset_snapshot_page_budgeted(
        &self,
        snapshot: SnapshotDescriptor<V>,
        offset: usize,
        limit: usize,
        scope: &mut WorkScope,
    ) -> Result<SnapshotPage<V>, FlowError> {
        if snapshot.root() != self.root() || snapshot.sequence() != self.next_batch {
            return Err(FlowError::ResetRequired);
        }
        if snapshot.len() != self.visible_len {
            return Err(FlowError::ResetRequired);
        }
        self.snapshot_page_budgeted(offset, limit, scope)
    }

    /// Creates a delta subscription from a root/sequence fence.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Gap`] when `sequence` is ahead of the producer.
    /// Stale roots or history gaps are represented by a queued reset event.
    pub fn subscribe_from(
        &self,
        root: ArrangementRoot<V>,
        sequence: u64,
    ) -> Result<Subscription<V>, FlowError> {
        if sequence > self.next_batch {
            return Err(FlowError::Gap);
        }
        if sequence == self.next_batch && root == self.root() {
            return Ok(Subscription {
                events: VecDeque::new(),
                root,
                sequence,
            });
        }
        let mut events = VecDeque::new();
        let mut expected_root = root;
        let mut expected_sequence = sequence;
        let mut queued_rows = 0_usize;
        for record in self
            .history
            .iter()
            .filter(|record| record.sequence > sequence)
        {
            if record.before != expected_root
                || expected_sequence.checked_add(1) != Some(record.sequence)
            {
                return Ok(self.reset_subscription());
            }
            let Some(next_rows) = queued_rows.checked_add(record.deltas.len()) else {
                return Ok(self.reset_subscription());
            };
            if events.len() == self.max_subscription_events
                || next_rows > self.max_subscription_rows
            {
                return Ok(self.reset_subscription());
            }
            events.push_back(SubscriptionEvent::Delta {
                sequence: record.sequence,
                root: record.after,
                deltas: record.deltas.to_vec(),
            });
            expected_root = record.after;
            expected_sequence = record.sequence;
            queued_rows = next_rows;
        }
        if expected_root != self.root() || expected_sequence != self.next_batch {
            return Ok(self.reset_subscription());
        }
        Ok(Subscription {
            events,
            root,
            sequence,
        })
    }

    /// Creates a complete reset event for slow or incompatible subscribers.
    #[must_use]
    pub fn reset_subscription(&self) -> Subscription<V> {
        let snapshot = self.reset_snapshot();
        let root = snapshot.root();
        let sequence = snapshot.sequence();
        Subscription {
            events: VecDeque::from([SubscriptionEvent::Reset { snapshot }]),
            root,
            sequence,
        }
    }

    /// Creates a complete reset descriptor while retaining the compatibility
    /// `scope` argument.
    ///
    /// Descriptor creation is O(1) and does not consume row, byte, or probe
    /// credit.  Call [`Self::reset_snapshot_page_budgeted`] for every page;
    /// that method applies the negotiated envelope to the actual payload.
    ///
    /// The argument remains in this method so existing callers can migrate to
    /// the page protocol without changing their setup path in one step.
    ///
    /// # Errors
    ///
    /// This method currently returns an error only if reset descriptor
    /// construction cannot preserve the arrangement fence.
    pub fn reset_subscription_budgeted(
        &self,
        _scope: &mut WorkScope,
    ) -> Result<Subscription<V>, FlowError> {
        // A descriptor has no row payload.  Row/byte/probe credits are
        // consumed by `reset_snapshot_page_budgeted`, once the consumer has
        // negotiated its page size.
        let snapshot = self.reset_snapshot();
        let root = snapshot.root();
        let sequence = snapshot.sequence();
        Ok(Subscription {
            events: VecDeque::from([SubscriptionEvent::Reset { snapshot }]),
            root,
            sequence,
        })
    }

    /// Retains a cursor only when its root and sequence are still valid.
    ///
    /// # Errors
    ///
    /// Returns [`FlowError::Gap`] when the requested sequence is in the
    /// future, or [`FlowError::ResetRequired`] for an old sequence/root.
    pub fn cursor_from(
        &self,
        root: ArrangementRoot<V>,
        sequence: u64,
    ) -> Result<SubscriberCursor<'_, V>, FlowError> {
        if sequence > self.next_batch {
            return Err(FlowError::Gap);
        }
        if sequence < self.next_batch {
            return Err(FlowError::ResetRequired);
        }
        if root != self.root() {
            return Err(FlowError::ResetRequired);
        }
        Ok(self.subscribe())
    }

    pub(crate) fn trim_history(&mut self) {
        let mut drop_count = 0;
        let mut dropped_rows = 0_usize;
        let mut dropped_bytes = 0_usize;
        while self.history.len().saturating_sub(drop_count) > self.max_history
            || self.history_rows.saturating_sub(dropped_rows) > self.max_history_rows
            || self.history_bytes.saturating_sub(dropped_bytes) > self.max_history_bytes
        {
            let Some(record) = self.history.get(drop_count) else {
                break;
            };
            dropped_rows = dropped_rows.saturating_add(record.deltas.len());
            dropped_bytes = dropped_bytes.saturating_add(record.bytes);
            drop_count += 1;
        }
        if drop_count > 0 {
            self.history.drain(..drop_count);
            self.history_rows = self.history_rows.saturating_sub(dropped_rows);
            self.history_bytes = self.history_bytes.saturating_sub(dropped_bytes);
        }
    }
}
