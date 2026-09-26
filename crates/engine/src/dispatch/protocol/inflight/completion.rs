//! Typed completion of one pending remote envelope.

use super::super::super::coverage::SemanticCoverageValidator;
use super::super::super::retention::OutputAdmissionValidator;
use super::{
    DispatchCompletion, DispatchError, DispatchTicket, PendingRemoteEnvelope, PendingRemoteKey,
    RemoteCompletionInput,
};
use backend_execution::WorkKey;
use backend_replication::{CancelAttempt, WireIdentity, WireRecipeResult};
use backend_version::Relation;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

impl<V, A> PendingRemoteEnvelope<V, A>
where
    V: OutputAdmissionValidator + SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    pub(crate) fn from_ticket<R: Relation + Send>(
        ticket: DispatchTicket<R>,
        active_semantic: Arc<Mutex<BTreeMap<WorkKey, super::super::super::ActiveSemantic>>>,
    ) -> Result<Self, DispatchError> {
        let request = ticket.wire_request()?;
        let parts = ticket.parts()?;
        let retained_size =
            super::retained_size::<V, A, R>(&request, &parts.contract, &parts.expected)?;
        let key = PendingRemoteKey::new(parts.schedule.key(), parts.expected.attempt);
        let semantic_generation = active_semantic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key.work_key())
            .map(super::super::super::ActiveSemantic::generation);
        let identity = parts.schedule.identity();
        let work_claim = WireIdentity::from_typed(&identity.work_key());
        let fence = parts.expected.fence;
        let cancellation = parts.cancellation;
        let cancel_command = CancelAttempt {
            attempt: parts.expected.attempt,
            work_key: work_claim,
            cancellation,
            fence,
        };
        let mut ticket = ticket;
        let terminal = Arc::new(AtomicBool::new(false));
        let callback_terminal = Arc::clone(&terminal);
        let callback = Box::new(
            move |dispatcher: &super::super::super::Dispatcher<V, A>, input, now| match input {
                RemoteCompletionInput::Wire(wire) => {
                    if ticket.is_consumed() {
                        return Err(DispatchError::TicketConsumed);
                    }
                    let result = dispatcher.complete_remote_ticket_checked(&mut ticket, *wire, now);
                    if ticket.is_consumed() {
                        callback_terminal.store(true, Ordering::Release);
                    }
                    result
                }
                RemoteCompletionInput::Fallback {
                    bytes,
                    remote_failed,
                } => {
                    let result = dispatcher.fallback_remote_ticket_checked_with(
                        &mut ticket,
                        bytes,
                        now,
                        remote_failed,
                    );
                    if ticket.is_consumed() {
                        callback_terminal.store(true, Ordering::Release);
                    }
                    result
                }
            },
        );
        Ok(Self {
            key,
            request: Arc::new(request),
            retained_size,
            work_claim,
            fence,
            cancellation,
            callback: Some(callback),
            prepared: None,
            cancel_command,
            fallback_cancel_attempted: false,
            active_semantic,
            semantic_generation,
            terminal,
        })
    }

    /// Cancels and consumes the pending ticket, releasing its scheduler
    /// reservation and retained interner state.
    pub fn cancel(mut self) {
        self.callback.take();
    }

    /// Checks the result correlation without consuming the typed callback.
    #[must_use]
    pub fn matches(&self, wire: &WireRecipeResult) -> bool {
        wire.attempt == self.key.attempt
            && wire.work_key == self.work_claim
            && wire.fence == self.fence
            && wire.cancellation == self.cancellation
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    /// Invokes the original typed completion. A mismatched frame leaves the
    /// callback armed so the owner can retain this envelope and choose an
    /// explicit fallback later.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError::TicketMismatch`] when the frame does not
    /// belong to this pending work key, attempt, or cancellation identity,
    /// or [`DispatchError::TicketConsumed`] when this envelope was already
    /// completed or cancelled.
    pub fn complete(
        &mut self,
        dispatcher: &super::super::super::Dispatcher<V, A>,
        wire: WireRecipeResult,
        now: u64,
    ) -> Result<&DispatchCompletion, DispatchError> {
        if !self.matches(&wire) {
            return Err(DispatchError::TicketMismatch);
        }
        if self.prepared.is_none() {
            let Some(callback) = self.callback.as_mut() else {
                return Err(DispatchError::TicketConsumed);
            };
            let completion =
                callback(dispatcher, RemoteCompletionInput::Wire(Box::new(wire)), now)?;
            self.prepared = Some((completion, false));
        }
        self.prepared
            .as_ref()
            .map(|(completion, _)| completion)
            .ok_or(DispatchError::TicketConsumed)
    }

    /// Activates the scheduler-owned local fallback once and retains its
    /// terminal completion until durable publication succeeds.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fallback(
        &mut self,
        dispatcher: &super::super::super::Dispatcher<V, A>,
        bytes: Arc<Vec<u8>>,
        now: u64,
    ) -> Result<&DispatchCompletion, DispatchError> {
        self.prepare_fallback(dispatcher, bytes, now, false)
    }

    /// Activates the same retained fallback after an observed terminal
    /// transport failure, without waiting for the latency deadline.
    pub(crate) fn fail_remote(
        &mut self,
        dispatcher: &super::super::super::Dispatcher<V, A>,
        bytes: Arc<Vec<u8>>,
        now: u64,
    ) -> Result<&DispatchCompletion, DispatchError> {
        self.prepare_fallback(dispatcher, bytes, now, true)
    }

    fn prepare_fallback(
        &mut self,
        dispatcher: &super::super::super::Dispatcher<V, A>,
        bytes: Arc<Vec<u8>>,
        now: u64,
        remote_failed: bool,
    ) -> Result<&DispatchCompletion, DispatchError> {
        if self.prepared.is_none() {
            let Some(callback) = self.callback.as_mut() else {
                return Err(DispatchError::TicketConsumed);
            };
            let completion = callback(
                dispatcher,
                RemoteCompletionInput::Fallback {
                    bytes,
                    remote_failed,
                },
                now,
            )?;
            self.prepared = Some((completion, true));
        }
        self.prepared
            .as_ref()
            .map(|(completion, _)| completion)
            .ok_or(DispatchError::TicketConsumed)
    }

    pub(crate) fn take_prepared(&mut self) -> Option<(DispatchCompletion, bool)> {
        self.prepared.take()
    }

    pub(crate) fn prepared_kind(&self) -> Option<bool> {
        self.prepared.as_ref().map(|(_, fallback)| *fallback)
    }

    pub(crate) const fn fallback_cancel_attempted(&self) -> bool {
        self.fallback_cancel_attempted
    }

    pub(crate) fn mark_fallback_cancel_attempted(&mut self) {
        self.fallback_cancel_attempted = true;
    }

    pub(crate) fn restore_prepared(&mut self, completion: DispatchCompletion, fallback: bool) {
        debug_assert!(self.prepared.is_none());
        self.prepared = Some((completion, fallback));
    }
}
