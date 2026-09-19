//! Typestate tickets for durable workspace publication.

use super::head::WorkspaceHead;
use super::transition::{PreparedTransition, TransactionId};
use crate::journal::JournalReceipt;
use crate::schema::WorkspaceLog;
use backend_store::{WorkspaceFileDurable, WorkspaceFilePublished};
use std::marker::PhantomData;
use std::sync::Arc;

/// Durable acknowledgement state for a selected publication.
///
/// The store-owned workspace HEAD is the linearization point.  The engine
/// journal and diagnostic sidecar are acknowledgements only.  A process can
/// lose either after the store has selected a root, so callers receive a
/// successful typed publication with this status instead of an ordinary
/// error that could cause a duplicate commit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PublicationStatus {
    pending: u8,
}

const STORE_PUBLISH: u8 = 1 << 0;
const JOURNAL_SELECT: u8 = 1 << 1;
const JOURNAL_FLUSH: u8 = 1 << 2;
const HEAD_WRITE: u8 = 1 << 3;
const HEAD_SYNC: u8 = 1 << 4;
const HEAD_SELECTION: u8 = 1 << 5;
const JOURNAL_PUBLISHED: u8 = 1 << 6;
const NOTIFICATION: u8 = 1 << 7;

impl PublicationStatus {
    pub(crate) const fn with_store_publish_pending(mut self) -> Self {
        self.pending |= STORE_PUBLISH;
        self
    }

    pub(crate) const fn with_journal_select_pending(mut self) -> Self {
        self.pending |= JOURNAL_SELECT;
        self
    }

    pub(crate) const fn with_journal_flush_pending(mut self) -> Self {
        self.pending |= JOURNAL_FLUSH;
        self
    }

    pub(crate) const fn with_head_write_pending(mut self) -> Self {
        self.pending |= HEAD_WRITE;
        self
    }

    pub(crate) const fn with_head_selection_pending(mut self) -> Self {
        self.pending |= HEAD_SELECTION;
        self
    }

    pub(crate) const fn with_journal_published_pending(mut self) -> Self {
        self.pending |= JOURNAL_PUBLISHED;
        self
    }

    pub(crate) const fn with_notification_pending(mut self) -> Self {
        self.pending |= NOTIFICATION;
        self
    }

    /// Returns whether the diagnostic sidecar directory sync needs recovery.
    #[must_use]
    pub const fn head_sync_pending(self) -> bool {
        self.pending & HEAD_SYNC != 0
    }

    /// Returns whether the physical store publication completed at an
    /// ambiguous filesystem boundary.
    #[must_use]
    pub const fn store_publish_pending(self) -> bool {
        self.pending & STORE_PUBLISH != 0
    }

    /// Returns whether the engine Select append needs reconciliation.
    #[must_use]
    pub const fn journal_select_pending(self) -> bool {
        self.pending & JOURNAL_SELECT != 0
    }

    /// Returns whether the Select frame sync boundary needs reconciliation.
    #[must_use]
    pub const fn journal_flush_pending(self) -> bool {
        self.pending & JOURNAL_FLUSH != 0
    }

    /// Returns whether the diagnostic sidecar write needs reconciliation.
    #[must_use]
    pub const fn head_write_pending(self) -> bool {
        self.pending & HEAD_WRITE != 0
    }

    /// Returns whether the post-selection boundary was interrupted.
    #[must_use]
    pub const fn head_selection_pending(self) -> bool {
        self.pending & HEAD_SELECTION != 0
    }

    /// Returns whether the Published journal marker needs reconciliation.
    #[must_use]
    pub const fn journal_published_pending(self) -> bool {
        self.pending & JOURNAL_PUBLISHED != 0
    }

    /// Returns whether the subscriber notification needs reconciliation.
    #[must_use]
    pub const fn notification_pending(self) -> bool {
        self.pending & NOTIFICATION != 0
    }

    /// Returns whether every post-selection acknowledgement completed.
    #[must_use]
    pub const fn is_confirmed(self) -> bool {
        self.pending == 0
    }
}

/// A checked transition that has not reached durable storage.
#[derive(Debug)]
pub struct Prepared;
/// A transition whose immutable closure and prepare/select record are synced.
#[derive(Debug)]
pub struct Durable;
/// A transition whose selected head is durably visible.
#[derive(Debug)]
pub struct Published;

#[derive(Clone, Debug)]
pub(crate) struct PublicationData {
    pub(crate) transition: Arc<PreparedTransition>,
    pub(crate) base_head: WorkspaceHead,
    /// The lower store owns physical closure/object publication.  Keeping the
    /// typestate capability attached to the engine ticket prevents a caller
    /// from selecting a logical head through a raw closure write.
    pub(crate) store_durable: Option<WorkspaceFileDurable>,
    pub(crate) store_published: Option<WorkspaceFilePublished>,
    /// Receipt naming the exact diagnostic Prepared frame linked by Select.
    pub(crate) prepared_receipt: Option<JournalReceipt<WorkspaceLog>>,
    /// Post-linearization acknowledgement state. Prepared and Durable
    /// tickets always carry the empty state.
    pub(crate) post_selection: PublicationStatus,
    /// An idempotent retry of the currently selected request reuses this
    /// already published head without writing another store generation.
    pub(crate) existing_published: Option<WorkspaceHead>,
}

/// A publication ticket whose state is encoded in `S`.
pub struct Publication<S> {
    pub(crate) data: PublicationData,
    pub(crate) _state: PhantomData<fn() -> S>,
}

impl<S> std::fmt::Debug for Publication<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Publication")
            .field("transaction", &self.data.transition.transaction())
            .field("target", &self.data.transition.target())
            .finish_non_exhaustive()
    }
}

impl Publication<Prepared> {
    pub(crate) fn new(data: PublicationData) -> Self {
        Self {
            data,
            _state: PhantomData,
        }
    }

    /// Returns the deterministic transaction identity.
    #[must_use]
    pub fn transaction(&self) -> TransactionId {
        self.data.transition.transaction()
    }

    /// Returns the checked target workspace root.
    #[must_use]
    pub fn target(&self) -> backend_version::WorkspaceRoot {
        self.data.transition.target()
    }
}

impl Publication<Durable> {
    /// Returns the deterministic transaction identity.
    #[must_use]
    pub fn transaction(&self) -> TransactionId {
        self.data.transition.transaction()
    }

    /// Returns the checked durable target workspace root.
    #[must_use]
    pub fn target(&self) -> backend_version::WorkspaceRoot {
        self.data.transition.target()
    }
}

impl Publication<Published> {
    /// Returns the checked published target workspace root.
    #[must_use]
    pub fn target(&self) -> backend_version::WorkspaceRoot {
        self.data.transition.target()
    }

    /// Returns the durable acknowledgement state after HEAD selection.
    #[must_use]
    pub const fn status(&self) -> PublicationStatus {
        self.data.post_selection
    }
}

/// A prepared publication ticket.
pub type PreparedPublication = Publication<Prepared>;
/// A durable publication ticket.
pub type DurablePublication = Publication<Durable>;
/// A published publication ticket retained for notification bookkeeping.
pub type PublishedPublication = Publication<Published>;
