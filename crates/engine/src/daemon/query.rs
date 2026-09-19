//! Coherent view/query state and bounded cursor subscriptions.

use super::{
    Cursor, CursorRead, CursorResetReason, CursorSub, Daemon, DaemonError, DaemonReply, Library,
    QueueSized, SubscriptionReply, VecDeque, WorkspaceModel, WorkspaceRoot, WorkspaceSnapshot,
};

/// Owner-side durable view sink.
///
/// The engine deliberately does not choose a serialization format for a
/// product's view certificates.  A composition can install a sink that
/// persists the exact checked root and event before [`Daemon::publish_view`]
/// exposes it to clients.  The owner loop is single-threaded, so the sink is
/// mutable and cannot race a workspace publication.
pub trait ViewPersistence: Send {
    /// Commits one checked view snapshot and optional event before it becomes
    /// visible through the daemon's library or notification cursor.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn persist(
        &mut self,
        workspace_root: WorkspaceRoot,
        view: &backend_library::ViewRoot,
        cursor: Cursor,
        event: Option<&backend_library::CursorEvent>,
    ) -> Result<(), String>;
}

/// Whether the selected library view has an engine-owned source binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewBindingState {
    /// No independent authority has admitted a workspace-to-view relation.
    Unbound,
    /// The view was admitted against the current workspace publication.
    Current,
    /// The workspace advanced while the view remained at its prior source.
    Stale,
}

/// Engine-owned proof label tying a library source to a workspace root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewBinding {
    workspace_root: WorkspaceRoot,
    source_root: backend_library::ViewStateRoot,
    state: ViewBindingState,
}

impl ViewBinding {
    pub(super) fn unbound(
        workspace_root: WorkspaceRoot,
        source_root: backend_library::ViewStateRoot,
    ) -> Self {
        Self {
            workspace_root,
            source_root,
            state: ViewBindingState::Unbound,
        }
    }

    pub(super) fn stale(
        workspace_root: WorkspaceRoot,
        source_root: backend_library::ViewStateRoot,
    ) -> Self {
        Self {
            workspace_root,
            source_root,
            state: ViewBindingState::Stale,
        }
    }

    pub(super) fn current(
        workspace_root: WorkspaceRoot,
        source_root: backend_library::ViewStateRoot,
    ) -> Self {
        Self {
            workspace_root,
            source_root,
            state: ViewBindingState::Current,
        }
    }

    /// Returns the workspace root named by the proof.
    #[must_use]
    pub const fn workspace_root(&self) -> WorkspaceRoot {
        self.workspace_root
    }

    /// Returns the exact source root named by the view.
    #[must_use]
    pub const fn source_root(&self) -> backend_library::ViewStateRoot {
        self.source_root
    }

    /// Returns the admission state.
    #[must_use]
    pub const fn state(&self) -> ViewBindingState {
        self.state
    }

    /// Returns whether a query may claim workspace/view coherence.
    #[must_use]
    pub const fn is_current(&self) -> bool {
        matches!(self.state, ViewBindingState::Current)
    }
}

/// Independent engine seam for binding a materialized view to a workspace.
/// A hash equality alone is insufficient: the caller must supply authority
/// evidence from the view producer and exact workspace transition.
pub trait ViewBindingAdmission: Send + Sync {
    /// Admits the exact pair before the daemon exposes it as current.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn admit(
        &self,
        workspace: &WorkspaceSnapshot,
        view: &backend_library::ViewRoot,
    ) -> Result<(), String>;
}

impl<F> ViewBindingAdmission for F
where
    F: Fn(&WorkspaceSnapshot, &backend_library::ViewRoot) -> Result<(), String> + Send + Sync,
{
    fn admit(
        &self,
        workspace: &WorkspaceSnapshot,
        view: &backend_library::ViewRoot,
    ) -> Result<(), String> {
        self(workspace, view)
    }
}

/// A query result tying workspace and library roots together with an explicit
/// proof state. `binding.is_current()` is required for a coherent claim.
#[derive(Clone, Debug)]
pub struct QueryState {
    /// Current checked workspace snapshot.
    pub workspace: WorkspaceSnapshot,
    /// Current coherent library view.
    pub view: backend_library::ViewRoot,
    /// Workspace-to-view admission state.
    pub binding: ViewBinding,
}

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    /// Removes one active subscription and releases its retained cursor
    /// state. This is the explicit lease/close operation for clients that no
    /// longer consume a stream.
    pub fn unsubscribe(&mut self, request_id: u64) -> bool {
        let Some(subscription) = self.subscriptions.get(&request_id) else {
            return false;
        };
        let Some(bytes) = subscription
            .max()
            .checked_mul(16)
            .and_then(|value| 128usize.checked_add(value))
        else {
            return false;
        };
        let Some(next) = self.subscription_bytes.checked_sub(bytes) else {
            return false;
        };
        self.subscriptions.remove(&request_id);
        self.subscription_bytes = next;
        true
    }

    /// Alias for a lease expiry path used by owner loops.
    pub fn expire_subscription(&mut self, request_id: u64) -> bool {
        self.unsubscribe(request_id)
    }
    fn remember_cursor(&mut self, cursor: Cursor) {
        let key = encode_cursor(cursor);
        if !self.cursor_history.contains_key(&key) {
            self.cursor_history.insert(key.clone(), cursor);
            self.cursor_history_order
                .push_back((cursor.sequence(), key));
        }
        self.prune_cursor_history();
    }

    fn prune_cursor_history(&mut self) {
        let mut retained = VecDeque::with_capacity(self.cursor_history_order.len());
        while let Some((sequence, key)) = self.cursor_history_order.pop_front() {
            if sequence >= self.view_events_base_sequence {
                retained.push_back((sequence, key));
            } else {
                self.cursor_history.remove(&key);
            }
        }
        self.cursor_history_order = retained;
        let history_limit = self.protocol.max_subscription_credit.saturating_add(1);
        while self.cursor_history_order.len() > history_limit {
            let Some((_sequence, key)) = self.cursor_history_order.pop_front() else {
                break;
            };
            self.cursor_history.remove(&key);
        }
    }
    pub(super) fn retain_subscription(
        &mut self,
        request_id: u64,
        subscription: CursorSub,
    ) -> Result<(), DaemonError> {
        let incoming = 128usize
            .checked_add(
                subscription
                    .max()
                    .checked_mul(16)
                    .ok_or(DaemonError::Backpressure)?,
            )
            .ok_or(DaemonError::Backpressure)?;
        let outgoing = match self.subscriptions.get(&request_id) {
            Some(old) => 128usize
                .checked_add(old.max().checked_mul(16).ok_or(DaemonError::Backpressure)?)
                .ok_or(DaemonError::Backpressure)?,
            None => 0,
        };
        let without = self
            .subscription_bytes
            .checked_sub(outgoing)
            .ok_or(DaemonError::Backpressure)?;
        let next = without
            .checked_add(incoming)
            .ok_or(DaemonError::Backpressure)?;
        let is_new = outgoing == 0;
        let budget = self.config.subscriptions;
        if is_new && self.subscriptions.len() >= budget.count {
            return Err(DaemonError::Backpressure);
        }
        if next > budget.bytes {
            return Err(DaemonError::Backpressure);
        }
        self.subscriptions.insert(request_id, subscription);
        self.subscription_bytes = next;
        Ok(())
    }
    /// Publishes a checked library view only after an independent source
    /// binding proof has accepted it. This is the materialization seam that
    /// turns a workspace commit into a coherent query pair.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn publish_view(
        &mut self,
        view: backend_library::ViewRoot,
        cursor: Cursor,
        admission: &dyn ViewBindingAdmission,
        event: Option<backend_library::CursorEvent>,
    ) -> Result<(), DaemonError> {
        let workspace = self.owner.snapshot();
        admission
            .admit(&workspace, &view)
            .map_err(DaemonError::ViewAdmission)?;
        if let Some(event) = event.as_ref() {
            let expected_cursor = self
                .library
                .cursor()
                .advance_event(event)
                .map_err(|_| DaemonError::CursorInvalid)?;
            if expected_cursor != cursor {
                return Err(DaemonError::CursorInvalid);
            }
            if let backend_library::CursorEvent::View { delta } = event {
                let expected_view = delta
                    .clone()
                    .apply_to(self.library.view())
                    .map_err(|_| DaemonError::CursorInvalid)?;
                if expected_view != view {
                    return Err(DaemonError::CursorInvalid);
                }
            } else if self.library.view() != &view {
                return Err(DaemonError::CursorInvalid);
            }
        }
        let retained_view = view.clone();
        let projection = backend_library::ViewProjection::admit(view, cursor)
            .map_err(|error| DaemonError::Library(format!("view projection: {error:?}")))?;
        let library = Library::from_projection(projection)
            .map_err(|error| DaemonError::Library(error.to_string()))?;
        if let Some(persistence) = self.view_persistence.as_mut() {
            persistence
                .persist(workspace.root(), &retained_view, cursor, event.as_ref())
                .map_err(DaemonError::ViewAdmission)?;
        }
        self.library = library;
        self.remember_cursor(cursor);
        self.view_binding = ViewBinding::current(workspace.root(), retained_view.basis().root);
        if let Some(event) = event {
            self.view_events.push(event);
            if self.view_events.len() > self.protocol.max_subscription_credit {
                let excess = self
                    .view_events
                    .len()
                    .checked_sub(self.protocol.max_subscription_credit)
                    .ok_or(DaemonError::SubscriptionCredit)?;
                self.view_events.drain(..excess);
                self.view_events_base_sequence = self
                    .view_events_base_sequence
                    .checked_add(
                        u64::try_from(excess).map_err(|_| DaemonError::SubscriptionCredit)?,
                    )
                    .ok_or(DaemonError::SubscriptionCredit)?;
            }
            self.prune_cursor_history();
            // Cursor sequence numbers are the event suffix index. Retain the
            // exact cursor encodings accepted by this owner so the wire
            // boundary never has to reconstruct typed IDs from raw bytes.
            self.remember_cursor(cursor);
        }
        Ok(())
    }

    /// Installs the composition's durable view sink.  Recovery code should
    /// load its state before this setter is called, then use
    /// [`Self::restore_view`] to seed the owner projection without writing a
    /// duplicate record.
    pub fn set_view_persistence(&mut self, persistence: Box<dyn ViewPersistence>) {
        self.view_persistence = Some(persistence);
    }

    fn subscription_reset(
        &self,
        credit: usize,
        cursor: Cursor,
        reason: CursorResetReason,
    ) -> DaemonReply {
        DaemonReply::Subscribed(Ok(SubscriptionReply::ResetWithRoot {
            credit,
            cursor: encode_cursor(cursor).into_boxed_slice(),
            root: Box::new(self.library.view().clone()),
            reason,
        }))
    }

    fn finish_cursor_poll(
        &mut self,
        request_id: u64,
        subscription: CursorSub,
        reply: SubscriptionReply,
    ) -> DaemonReply {
        match self.retain_subscription(request_id, subscription) {
            Ok(()) => {
                let _ = self.unsubscribe(request_id);
                DaemonReply::Subscribed(Ok(reply))
            }
            Err(error) => DaemonReply::Subscribed(Err(error)),
        }
    }

    /// Restores a checked view and its bounded event suffix after a process
    /// restart.  The suffix is retained with its exact base sequence, so a
    /// client can resume from any cursor that survived pruning; older cursors
    /// receive the normal typed reset response.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn restore_view(
        &mut self,
        view: backend_library::ViewRoot,
        cursor: Cursor,
        admission: &dyn ViewBindingAdmission,
        events: Vec<backend_library::CursorEvent>,
        base_sequence: u64,
    ) -> Result<(), DaemonError> {
        let workspace = self.owner.snapshot();
        admission
            .admit(&workspace, &view)
            .map_err(DaemonError::ViewAdmission)?;
        let retained_view = view.clone();
        let projection = backend_library::ViewProjection::admit(view, cursor)
            .map_err(|error| DaemonError::Library(format!("view projection: {error:?}")))?;
        let expected_base = cursor
            .sequence()
            .checked_sub(u64::try_from(events.len()).map_err(|_| DaemonError::SubscriptionCredit)?)
            .ok_or(DaemonError::CursorInvalid)?;
        if events.len() > self.protocol.max_subscription_credit {
            return Err(DaemonError::SubscriptionCredit);
        }
        if expected_base != base_sequence {
            return Err(DaemonError::CursorInvalid);
        }
        // Validate every retained event against the final cursor/root by
        // rewinding the shared cursor kernel. This avoids manufacturing any
        // identity from the durable byte framing. Keep the resulting history
        // in a local buffer so installing it cannot alias the daemon's
        // mutable cursor map.
        let mut cursor_at = cursor;
        let mut history = Vec::with_capacity(events.len().saturating_add(1));
        history.push(cursor);
        for event in events.iter().rev() {
            cursor_at = cursor_at
                .rewind_event(event)
                .map_err(|_| DaemonError::CursorInvalid)?;
            history.push(cursor_at);
        }
        let library = Library::from_projection(projection)
            .map_err(|error| DaemonError::Library(error.to_string()))?;
        self.library = library;
        self.view_binding = ViewBinding::current(workspace.root(), retained_view.basis().root);
        self.view_events = events;
        self.view_events_base_sequence = base_sequence;
        self.cursor_history.clear();
        self.cursor_history_order.clear();
        for cursor in history {
            self.remember_cursor(cursor);
        }
        Ok(())
    }
    pub(super) fn subscribe(
        &mut self,
        request_id: u64,
        cursor_bytes: &[u8],
        credit: usize,
    ) -> DaemonReply {
        if credit == 0 || credit > self.protocol.max_subscription_credit {
            return DaemonReply::Subscribed(Err(DaemonError::SubscriptionCredit));
        }
        let source = self.library.cursor();
        if cursor_bytes.is_empty() {
            return self.subscription_reset(credit, source, CursorResetReason::Gap);
        }
        let Some(cursor) = self.cursor_history.get(cursor_bytes).copied() else {
            return self.subscription_reset(credit, source, CursorResetReason::Gap);
        };
        let Some(start_sequence) = cursor
            .sequence()
            .checked_sub(self.view_events_base_sequence)
        else {
            return self.subscription_reset(credit, source, CursorResetReason::Pruned);
        };
        let start = usize::try_from(start_sequence).unwrap_or(usize::MAX);
        if start > self.view_events.len() {
            return self.subscription_reset(credit, source, CursorResetReason::Pruned);
        }
        let mut sub = CursorSub::from_cursor(cursor, credit);
        let read = sub.read(
            &source,
            &self.view_events[start..],
            self.library.view().clone(),
        );
        match read {
            Ok(CursorRead::Events { cursor: _, events }) if events.is_empty() => {
                // The process façade is a bounded cursor poll: it returns a
                // typed empty batch and the client renews the lease by
                // sending its cursor again. Do not retain a hidden lease for
                // a one-shot request that has no stream handle.
                self.finish_cursor_poll(request_id, sub, SubscriptionReply::Accepted { credit })
            }
            Ok(CursorRead::Events { cursor, events })
                if events.iter().any(|event| {
                    matches!(
                        event,
                        backend_library::CursorEvent::View { delta }
                            if matches!(delta.delta(), backend_library::ViewDelta::Reset { .. })
                    )
                }) || events
                    .iter()
                    .map(|event| match event {
                        backend_library::CursorEvent::Intent { .. } => 0,
                        backend_library::CursorEvent::View { delta } => delta.changed_row_count(),
                    })
                    .sum::<usize>()
                    > backend_library::MAX_SNAPSHOT_PAGE_ROWS =>
            {
                self.subscription_reset(credit, cursor, CursorResetReason::Pruned)
            }
            Ok(CursorRead::Events { cursor, events }) => self.finish_cursor_poll(
                request_id,
                sub,
                SubscriptionReply::Events {
                    credit,
                    cursor: encode_cursor(cursor).into_boxed_slice(),
                    events,
                },
            ),
            Ok(CursorRead::Reset {
                cursor,
                root,
                reason,
            }) => self.finish_cursor_poll(
                request_id,
                sub,
                SubscriptionReply::ResetWithRoot {
                    credit,
                    cursor: encode_cursor(cursor).into_boxed_slice(),
                    root,
                    reason,
                },
            ),
            Err(_) => DaemonReply::Subscribed(Err(DaemonError::CursorInvalid)),
        }
    }
    /// Returns a cursor envelope clients can persist and resume.
    #[must_use]
    pub fn cursor_bytes(&self) -> Box<[u8]> {
        encode_cursor(self.library.cursor()).into_boxed_slice()
    }
}

pub(super) fn encode_cursor(cursor: Cursor) -> Vec<u8> {
    cursor.encode_control().into_vec()
}
