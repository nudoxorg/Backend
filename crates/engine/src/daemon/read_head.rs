//! Worker-owned coherent workspace and view publication.
//!
//! The serving daemon retains its admitted immutable library. The single
//! writer also owns the durable view sink, so preparing, selecting and syncing
//! a replacement never borrows the serving daemon. Only a fully persisted pair
//! can return for fixed-size owner installation.

use super::{
    Cursor, Daemon, DaemonError, Library, QueueSized, ViewBinding, ViewBindingAdmission,
    ViewPersistence, WorkspaceModel,
};
use crate::{
    PreparedWorkspaceCandidate, PublicationStatus, PublishGrant, PublishedWorkspaceWriter,
    RetiredWorkspaceHead, UnselectedWorkspaceCandidate, WorkspaceError,
    WorkspacePublicationFailure, WorkspaceSnapshot, WorkspaceWriter,
};
use backend_library::{CursorEvent, ViewRoot};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone)]
pub(super) struct NotificationState {
    pub events: Vec<CursorEvent>,
    pub base_sequence: u64,
    pub history: BTreeMap<Vec<u8>, Cursor>,
    pub order: VecDeque<(u64, Vec<u8>)>,
}

impl NotificationState {
    fn remember(&mut self, cursor: Cursor, credit: usize) {
        let key = super::query::encode_cursor(cursor);
        if !self.history.contains_key(&key) {
            self.history.insert(key.clone(), cursor);
            self.order.push_back((cursor.sequence(), key));
        }
        self.order.retain(|(sequence, key)| {
            if *sequence >= self.base_sequence {
                true
            } else {
                self.history.remove(key);
                false
            }
        });
        while self.order.len() > credit.saturating_add(1) {
            if let Some((_, key)) = self.order.pop_front() {
                self.history.remove(&key);
            }
        }
    }

    fn advance(
        &mut self,
        cursor: Cursor,
        event: Option<CursorEvent>,
        credit: usize,
    ) -> Result<(), DaemonError> {
        self.remember(cursor, credit);
        if let Some(event) = event {
            self.events.push(event);
            if self.events.len() > credit {
                let excess = self
                    .events
                    .len()
                    .checked_sub(credit)
                    .ok_or(DaemonError::SubscriptionCredit)?;
                self.base_sequence = self
                    .base_sequence
                    .checked_add(
                        u64::try_from(excess).map_err(|_| DaemonError::SubscriptionCredit)?,
                    )
                    .ok_or(DaemonError::SubscriptionCredit)?;
                self.events.drain(..excess);
            }
            self.remember(cursor, credit);
        }
        Ok(())
    }
}

pub(super) struct RetiredView {
    _library: Library,
    _notifications: NotificationState,
}

pub(super) struct PreparedView {
    pub(super) library: Library,
    notifications: NotificationState,
    pub(super) event: Option<CursorEvent>,
}

/// The one checked grammar shared by synchronous publication and private
/// worker preparation. Persistence is deliberately separate and runs only
/// after the workspace candidate has actually selected its physical HEAD.
pub(super) fn prepare_view(
    workspace: &WorkspaceSnapshot,
    base: &Library,
    mut notifications: NotificationState,
    credit: usize,
    view: ViewRoot,
    cursor: Cursor,
    admission: &dyn ViewBindingAdmission,
    event: Option<CursorEvent>,
) -> Result<PreparedView, DaemonError> {
    admission
        .admit(workspace, &view)
        .map_err(DaemonError::ViewAdmission)?;
    if let Some(event) = event.as_ref() {
        if base
            .cursor()
            .advance_event(event)
            .map_err(|_| DaemonError::CursorInvalid)?
            != cursor
        {
            return Err(DaemonError::CursorInvalid);
        }
        if let CursorEvent::View { delta } = event {
            if delta
                .clone()
                .apply_to(base.view())
                .map_err(|_| DaemonError::CursorInvalid)?
                != view
            {
                return Err(DaemonError::CursorInvalid);
            }
        } else if base.view() != &view {
            return Err(DaemonError::CursorInvalid);
        }
    }
    let projection = backend_library::ViewProjection::admit(view, cursor)
        .map_err(|error| DaemonError::Library(format!("view projection: {error:?}")))?;
    let library = Library::from_projection(projection)
        .map_err(|error| DaemonError::Library(error.to_string()))?;
    notifications.advance(cursor, event.clone(), credit)?;
    Ok(PreparedView {
        library,
        notifications,
        event,
    })
}

struct ViewWriter {
    base: Library,
    notifications: NotificationState,
    credit: usize,
    persistence: Option<Box<dyn ViewPersistence>>,
    prepared: Option<(crate::WorkspaceCandidateClaim, PreparedView)>,
}

/// The unique workspace writer and view sink for one exact admitted read head.
/// No mutable serving owner or library is shared with the worker.
#[must_use = "return or settle both the workspace writer and durable view sink"]
pub struct ReadHeadWriter<M: WorkspaceModel> {
    workspace: WorkspaceWriter<M>,
    view: ViewWriter,
}

/// A physical workspace selection whose view persistence is still pending.
/// It cannot install or become a successful product reply until retry succeeds.
#[must_use = "retry persistence; a selected workspace cannot be cancelled"]
pub struct SelectedReadHead<M: WorkspaceModel> {
    workspace: PublishedWorkspaceWriter<M>,
    view: ViewWriter,
}

/// One inseparable, durable workspace/library/cursor publication.
#[must_use = "install the coherent selected read head or retain it for retry"]
pub struct PublishedReadHead<M: WorkspaceModel> {
    selected: SelectedReadHead<M>,
}

/// Every refusal retains the unique authority at its actual publication stage.
#[must_use = "the retained writer must be settled or installed"]
pub enum ReadHeadPublicationFailure<M: WorkspaceModel> {
    /// Physical publication was not attempted; all supplied tokens survive.
    Rejected {
        writer: ReadHeadWriter<M>,
        candidate: PreparedWorkspaceCandidate,
        grant: PublishGrant,
        error: DaemonError,
    },
    /// Physical publication must be reconciled on this same writer.
    Unsettled {
        writer: ReadHeadWriter<M>,
        error: WorkspaceError,
    },
    /// HEAD selected, but the durable view sink has not acknowledged success.
    SelectedPending {
        selected: SelectedReadHead<M>,
        error: DaemonError,
    },
}

macro_rules! debug_capability {
    ($name:ident) => {
        impl<M: WorkspaceModel> std::fmt::Debug for $name<M> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }
    };
}
debug_capability!(ReadHeadWriter);
debug_capability!(SelectedReadHead);
debug_capability!(PublishedReadHead);
impl<M: WorkspaceModel> std::fmt::Debug for ReadHeadPublicationFailure<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected { error, .. } => f
                .debug_struct("Rejected")
                .field("error", error)
                .finish_non_exhaustive(),
            Self::Unsettled { error, .. } => f
                .debug_struct("Unsettled")
                .field("error", error)
                .finish_non_exhaustive(),
            Self::SelectedPending { error, .. } => f
                .debug_struct("SelectedPending")
                .field("error", error)
                .finish_non_exhaustive(),
        }
    }
}

/// Large prior projections and unused private preparations retire off-thread.
#[must_use]
pub struct RetiredReadHead {
    _workspace: Option<RetiredWorkspaceHead>,
    _view: Option<RetiredView>,
    _reservation: Option<ViewWriter>,
}

impl std::fmt::Debug for RetiredReadHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetiredReadHead").finish_non_exhaustive()
    }
}

impl<M: WorkspaceModel> ReadHeadWriter<M> {
    /// Prepares privately durable workspace bytes on the worker.
    pub fn prepare(
        &mut self,
        intent: M::Intent,
    ) -> Result<PreparedWorkspaceCandidate, WorkspaceError> {
        self.workspace.prepare(intent)
    }

    /// Borrows the checked private workspace snapshot for product projection.
    pub fn candidate_snapshot(
        &self,
        candidate: &PreparedWorkspaceCandidate,
    ) -> Result<WorkspaceSnapshot, WorkspaceError> {
        self.workspace.candidate_snapshot(candidate)
    }

    /// Builds and admits the replacement library before requesting a grant.
    /// Neither persistence nor visible selection occurs here.
    pub fn prepare_view(
        &mut self,
        candidate: &PreparedWorkspaceCandidate,
        view: ViewRoot,
        cursor: Cursor,
        admission: &dyn ViewBindingAdmission,
        event: Option<CursorEvent>,
    ) -> Result<(), DaemonError> {
        if self.view.prepared.is_some() {
            return Err(DaemonError::ViewAdmission(
                "a read-head view is already prepared".to_owned(),
            ));
        }
        let snapshot = self
            .workspace
            .candidate_snapshot(candidate)
            .map_err(DaemonError::from)?;
        let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prepare_view(
                &snapshot,
                &self.view.base,
                self.view.notifications.clone(),
                self.view.credit,
                view,
                cursor,
                admission,
                event,
            )
        }))
        .map_err(|_| {
            DaemonError::ViewAdmission(
                "private view preparation panicked; writer remains unselected".to_owned(),
            )
        })??;
        self.view.prepared = Some((candidate.claim(), prepared));
        Ok(())
    }

    /// Selects the workspace with its one grant, then persists the exact view.
    /// Post-selection failure is always explicitly pending and retains both
    /// writers; it is never converted to cancellation or an unselected error.
    pub fn publish(
        self,
        candidate: PreparedWorkspaceCandidate,
        grant: PublishGrant,
    ) -> Result<PublishedReadHead<M>, ReadHeadPublicationFailure<M>> {
        if self
            .view
            .prepared
            .as_ref()
            .is_none_or(|(claim, _)| *claim != candidate.claim())
        {
            return Err(ReadHeadPublicationFailure::Rejected {
                writer: self,
                candidate,
                grant,
                error: DaemonError::ViewAdmission(
                    "the prepared view does not name this workspace candidate".to_owned(),
                ),
            });
        }
        let Self { workspace, view } = self;
        match workspace.publish(candidate, grant) {
            Ok(workspace) => SelectedReadHead { workspace, view }.persist(),
            Err(WorkspacePublicationFailure::Rejected {
                writer,
                candidate,
                grant,
                error,
            }) => Err(ReadHeadPublicationFailure::Rejected {
                writer: Self {
                    workspace: writer,
                    view,
                },
                candidate,
                grant,
                error: DaemonError::from(error),
            }),
            Err(WorkspacePublicationFailure::Unsettled { writer, error }) => {
                Err(ReadHeadPublicationFailure::Unsettled {
                    writer: Self {
                        workspace: writer,
                        view,
                    },
                    error,
                })
            }
        }
    }

    /// Proves unchanged physical selection without consulting the actor.
    pub fn prove_unselected(&mut self) -> Result<UnselectedWorkspaceCandidate, WorkspaceError> {
        self.workspace.prove_unselected()
    }

    /// Retries recovery with the same lease and prepared view after a pending
    /// physical selection. It never grants again or repeats publication.
    pub fn reconcile_publication(
        self,
    ) -> Result<PublishedReadHead<M>, ReadHeadPublicationFailure<M>> {
        let Self { workspace, view } = self;
        match workspace.reconcile_publication() {
            Ok(workspace) => SelectedReadHead { workspace, view }.persist(),
            Err((workspace, error)) => Err(ReadHeadPublicationFailure::Unsettled {
                writer: Self { workspace, view },
                error,
            }),
        }
    }
}

impl<M: WorkspaceModel> SelectedReadHead<M> {
    /// Existing workspace acknowledgement flags remain truthful independently
    /// of this capability's explicitly pending view acknowledgement.
    #[must_use]
    pub fn workspace_status(&self) -> PublicationStatus {
        self.workspace.status()
    }

    /// Retries only the exact selected view/event, retaining the sink even if
    /// its implementation panics. The serving read head remains unchanged.
    pub fn persist(mut self) -> Result<PublishedReadHead<M>, ReadHeadPublicationFailure<M>> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_, prepared) = self.view.prepared.as_ref().ok_or_else(|| {
                DaemonError::ViewAdmission("selected workspace has no prepared view".to_owned())
            })?;
            if let Some(persistence) = self.view.persistence.as_mut() {
                persistence
                    .persist(
                        self.workspace.snapshot().root(),
                        prepared.library.view(),
                        prepared.library.cursor(),
                        prepared.event.as_ref(),
                    )
                    .map_err(DaemonError::ViewAdmission)?;
            }
            Ok(())
        }))
        .unwrap_or_else(|_| {
            Err(DaemonError::ViewAdmission(
                "selected view persistence panicked; acknowledgement remains pending".to_owned(),
            ))
        });
        match result {
            Ok(()) => Ok(PublishedReadHead { selected: self }),
            Err(error) => Err(ReadHeadPublicationFailure::SelectedPending {
                selected: self,
                error,
            }),
        }
    }
}

impl<M: WorkspaceModel> PublishedReadHead<M> {
    /// Returns the actual workspace acknowledgement status.
    #[must_use]
    pub fn workspace_status(&self) -> PublicationStatus {
        self.selected.workspace.status()
    }
}

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    pub(super) fn notification_state(&self) -> NotificationState {
        NotificationState {
            events: self.view_events.clone(),
            base_sequence: self.view_events_base_sequence,
            history: self.cursor_history.clone(),
            order: self.cursor_history_order.clone(),
        }
    }

    pub(super) fn install_prepared_view(
        &mut self,
        prepared: PreparedView,
        workspace: backend_version::WorkspaceRoot,
    ) -> RetiredView {
        let PreparedView {
            library,
            notifications,
            event: _,
        } = prepared;
        self.view_binding = ViewBinding::current(workspace, library.view().basis().root);
        let retired_notifications = NotificationState {
            events: std::mem::replace(&mut self.view_events, notifications.events),
            base_sequence: std::mem::replace(
                &mut self.view_events_base_sequence,
                notifications.base_sequence,
            ),
            history: std::mem::replace(&mut self.cursor_history, notifications.history),
            order: std::mem::replace(&mut self.cursor_history_order, notifications.order),
        };
        RetiredView {
            _library: std::mem::replace(&mut self.library, library),
            _notifications: retired_notifications,
        }
    }
    /// Transfers the workspace writer and the existing durable sink together.
    /// The captured library is immutable; bounded cursor history is copied at
    /// reservation, while corpus construction and disk work run on the worker.
    pub fn reserve_read_head_writer(&mut self) -> Result<ReadHeadWriter<M>, WorkspaceError> {
        let workspace = self.owner.reserve_writer()?;
        let view = ViewWriter {
            base: self.library.clone(),
            notifications: self.notification_state(),
            credit: self.protocol.max_subscription_credit,
            persistence: self.view_persistence.take(),
            prepared: None,
        };
        Ok(ReadHeadWriter { workspace, view })
    }

    /// Installs the inseparable durable workspace/view result in one actor
    /// turn. Checks use exact fixed-width view commitments and cursor identity;
    /// no CAS reads, relation traversal, serialization or fsync occurs here.
    pub fn install_read_head(
        &mut self,
        published: PublishedReadHead<M>,
    ) -> Result<RetiredReadHead, (PublishedReadHead<M>, DaemonError)> {
        let base = &published.selected.view.base;
        if self.view_persistence.is_some()
            || self.library.cursor() != base.cursor()
            || self.library.view().version() != base.view().version()
            || self.library.view().root() != base.view().root()
            || published.selected.view.prepared.is_none()
        {
            return Err((published, DaemonError::CursorInvalid));
        }
        let SelectedReadHead {
            workspace,
            mut view,
        } = published.selected;
        let retired = match self.owner.install_candidate(workspace) {
            Ok(retired) => retired,
            Err((workspace, error)) => {
                return Err((
                    PublishedReadHead {
                        selected: SelectedReadHead { workspace, view },
                    },
                    DaemonError::from(error),
                ));
            }
        };
        // Existence was checked before consuming any authority. No fallible
        // operation remains between workspace and view installation.
        let (_, prepared) = view
            .prepared
            .take()
            .expect("checked private prepared read head");
        let old = self.install_prepared_view(prepared, self.owner.head().root());
        self.view_persistence = view.persistence.take();
        Ok(RetiredReadHead {
            _workspace: Some(retired),
            _view: Some(old),
            _reservation: Some(view),
        })
    }

    /// Returns an ungranted writer and its original sink without discarding a
    /// potentially large private projection on the actor thread.
    pub fn return_unselected_read_head_writer(
        &mut self,
        writer: ReadHeadWriter<M>,
    ) -> Result<RetiredReadHead, (ReadHeadWriter<M>, WorkspaceError)> {
        self.return_read_head_writer(writer, None)
    }

    /// Returns a granted attempt only with its same-writer physical-base proof.
    pub fn return_failed_read_head_writer(
        &mut self,
        writer: ReadHeadWriter<M>,
        proof: UnselectedWorkspaceCandidate,
    ) -> Result<RetiredReadHead, (ReadHeadWriter<M>, WorkspaceError)> {
        self.return_read_head_writer(writer, Some(proof))
    }

    fn return_read_head_writer(
        &mut self,
        writer: ReadHeadWriter<M>,
        proof: Option<UnselectedWorkspaceCandidate>,
    ) -> Result<RetiredReadHead, (ReadHeadWriter<M>, WorkspaceError)> {
        if self.view_persistence.is_some() {
            return Err((writer, WorkspaceError::WriterReserved));
        }
        let ReadHeadWriter {
            workspace,
            mut view,
        } = writer;
        let returned = match proof {
            Some(proof) => self.owner.return_failed_writer(workspace, proof),
            None => self.owner.return_unselected_writer(workspace),
        };
        if let Err((workspace, error)) = returned {
            return Err((ReadHeadWriter { workspace, view }, error));
        }
        self.view_persistence = view.persistence.take();
        Ok(RetiredReadHead {
            _workspace: None,
            _view: None,
            _reservation: Some(view),
        })
    }
}
