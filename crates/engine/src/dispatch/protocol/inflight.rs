//! Affine remote admission, in-flight tickets, and terminal outcomes.

use super::super::DispatchError;
use super::super::authority::AdmissionReservation;
use super::super::coverage::CompleteSemanticCoverage;
use super::super::retention::RetainedOutput;
use super::contract::{FenceBinding, RemoteDispatchContract};
use backend_execution::{Interned, ScheduleReceipt, Scheduled};
use backend_execution::{ReusableOutput, WorkKey};
use backend_replication::{
    AttemptId, AttestationClass, CancelAttempt, CancellationId, ExecutionRequestExpectation, Fence,
    PublishableExecutionResult, WireIdentity, WireRecipeRequest, WireRecipeResult,
};
use backend_version::Relation;
use std::collections::BTreeMap;
use std::fmt;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod completion;

/// Remote result after exact admission and authority verification.
///
/// Admission retains rollback reservations until this value is consumed by
/// [`crate::dispatch::Dispatcher::complete_remote`]. Dropping it releases replay and authority
/// reservations without changing the accepted result map.
#[must_use = "consume the remote admission with complete_remote or drop it to roll back"]
#[derive(Debug)]
pub(crate) struct RemoteAdmission {
    pub(crate) publishable: PublishableExecutionResult,
    pub(crate) semantic: CompleteSemanticCoverage,
    pub(crate) fence: FenceBinding,
    pub(crate) class: AttestationClass,
    pub(crate) retained_output: RetainedOutput,
    pub(crate) reservation: AdmissionReservation,
}

impl PartialEq for RemoteAdmission {
    fn eq(&self, other: &Self) -> bool {
        self.publishable.wire() == other.publishable.wire()
            && self.semantic == other.semantic
            && self.fence == other.fence
            && self.class == other.class
            && self.retained_output == other.retained_output
    }
}

impl Eq for RemoteAdmission {}

impl RemoteAdmission {
    /// Returns the verified authority class.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn authority_class(&self) -> AttestationClass {
        self.class
    }

    /// Returns the admitted wire result.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn wire(&self) -> &WireRecipeResult {
        self.publishable.wire()
    }
}

/// Reused output or a scheduled attempt retaining scheduler reservations,
/// interner ownership, cancellation, and publication fencing.
#[must_use]
#[allow(
    clippy::large_enum_variant,
    reason = "the scheduled variant intentionally owns affine scheduler guards"
)]
pub enum DispatchPlan<R: Relation> {
    /// Exact output already admitted under the expected authority.
    Reused(ReusableOutput),
    /// New schedule whose Scheduled value must remain alive.
    Scheduled(Scheduled<R>),
    /// A bounded follower demand sharing a live leader's eventual output.
    Waiting(Interned),
}

impl<R: Relation> fmt::Debug for DispatchPlan<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reused(output) => f.debug_tuple("Reused").field(output).finish(),
            Self::Scheduled(schedule) => f.debug_tuple("Scheduled").field(schedule).finish(),
            Self::Waiting(waiter) => f.debug_tuple("Waiting").field(waiter).finish(),
        }
    }
}
/// The sealed affine owner of one scheduled remote attempt.
///
/// A ticket is created only by [`crate::dispatch::Dispatcher::dispatch_ticket`] (or by
/// the daemon's remote dispatch method). It keeps the scheduler guard, exact
/// request contract, derived request expectation, and cancellation identity
/// together until one consuming operation completes, falls back, or cancels
/// the attempt. The fields and constructor are private so callers cannot pair
/// a plan from one attempt with a contract from another.
#[must_use = "consume the ticket with completion, fallback, or cancellation"]
pub struct InFlightRemote<R: Relation> {
    parts: Option<DispatchTicketParts<R>>,
    semantic_cleanup: ActiveSemanticTicketCleanup,
}

struct DispatchTicketParts<R: Relation> {
    schedule: Scheduled<R>,
    contract: RemoteDispatchContract,
    expected: ExecutionRequestExpectation,
    cancellation: CancellationId,
}

/// Drop guard for the active semantic registration associated with a ticket.
/// The guard is disarmed only when ownership moves into a completion path;
/// dropping a ticket directly therefore cannot leak a reverse-index row.
struct ActiveSemanticTicketCleanup {
    active_semantic: Arc<Mutex<BTreeMap<WorkKey, super::super::ActiveSemantic>>>,
    key: WorkKey,
    generation: Option<u64>,
    armed: bool,
}

impl ActiveSemanticTicketCleanup {
    fn new(
        active_semantic: Arc<Mutex<BTreeMap<WorkKey, super::super::ActiveSemantic>>>,
        key: WorkKey,
    ) -> Self {
        let generation = active_semantic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .map(super::super::ActiveSemantic::generation);
        Self {
            active_semantic,
            key,
            armed: generation.is_some(),
            generation,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ActiveSemanticTicketCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(generation) = self.generation else {
            return;
        };
        let removed = {
            let mut active = self
                .active_semantic
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if active
                .get(&self.key)
                .is_some_and(|candidate| candidate.generation() == generation)
            {
                active.remove(&self.key)
            } else {
                None
            }
        };
        // Drop outside the map lock because releasing the reservation takes
        // the semantic coordinator lock.
        drop(removed);
    }
}

impl<R: Relation> fmt::Debug for InFlightRemote<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("InFlightRemote");
        match self.parts.as_ref() {
            Some(parts) => {
                debug
                    .field("work_key", &parts.schedule.key())
                    .field("attempt", &parts.expected.attempt)
                    .field("fence", &parts.expected.fence)
                    .field("cancellation", &parts.cancellation);
            }
            None => {
                debug.field("state", &"consumed");
            }
        }
        debug.finish_non_exhaustive()
    }
}

impl<R: Relation> InFlightRemote<R> {
    pub(crate) fn new(
        schedule: Scheduled<R>,
        contract: RemoteDispatchContract,
        expected: ExecutionRequestExpectation,
        active_semantic: Arc<Mutex<BTreeMap<WorkKey, super::super::ActiveSemantic>>>,
    ) -> Self {
        let key = schedule.key();
        Self {
            parts: Some(DispatchTicketParts {
                cancellation: expected.cancellation,
                schedule,
                contract,
                expected,
            }),
            semantic_cleanup: ActiveSemanticTicketCleanup::new(active_semantic, key),
        }
    }

    fn parts(&self) -> Result<&DispatchTicketParts<R>, DispatchError> {
        self.parts.as_ref().ok_or(DispatchError::TicketConsumed)
    }

    /// Returns the stable work key bound to this attempt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn work_key(&self) -> Result<WorkKey, DispatchError> {
        Ok(self.parts()?.schedule.key())
    }

    /// Returns the one-based remote attempt identity.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn attempt(&self) -> Result<AttemptId, DispatchError> {
        Ok(self.parts()?.expected.attempt)
    }

    /// Returns the exact publication fence bound to this attempt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fence(&self) -> Result<Fence, DispatchError> {
        Ok(self.parts()?.expected.fence)
    }

    /// Returns the exact cancellation identity carried by the request.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn cancellation(&self) -> Result<CancellationId, DispatchError> {
        Ok(self.parts()?.cancellation)
    }

    /// Returns the complete cancellation command for this exact attempt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn cancel_command(&self) -> Result<CancelAttempt, DispatchError> {
        let parts = self.parts()?;
        let identity = parts.schedule.identity();
        Ok(CancelAttempt {
            attempt: parts.expected.attempt,
            work_key: WireIdentity::from_typed(&identity.work_key()),
            cancellation: parts.cancellation,
            fence: parts.expected.fence,
        })
    }

    /// Returns the caller-owned request expectation retained by this ticket.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn expected(&self) -> Result<&ExecutionRequestExpectation, DispatchError> {
        Ok(&self.parts()?.expected)
    }

    /// Returns the exact remote contract retained by this ticket.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn contract(&self) -> Result<&RemoteDispatchContract, DispatchError> {
        Ok(&self.parts()?.contract)
    }

    /// Builds the one wire request admitted by this ticket.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn wire_request(&self) -> Result<WireRecipeRequest, DispatchError> {
        let parts = self.parts()?;
        Ok(super::wire::wire_request(
            &parts.schedule,
            &parts.contract,
            &parts.expected,
        ))
    }

    /// Checks the cheap correlation fields before a result enters admission.
    /// Full identity, semantic, coverage, authority, and attestation checks
    /// remain in the consuming dispatcher operation.
    pub(crate) fn matches_wire(&self, wire: &WireRecipeResult) -> Result<bool, DispatchError> {
        let parts = self.parts()?;
        let identity = parts.schedule.identity();
        let expected_work_key = WireIdentity::from_typed(&identity.work_key());
        Ok(wire.attempt == parts.expected.attempt
            && wire.work_key == expected_work_key
            && wire.fence == parts.expected.fence
            && wire.cancellation == parts.cancellation)
    }

    pub(crate) fn take_parts(
        &mut self,
    ) -> Result<
        (
            DispatchPlan<R>,
            RemoteDispatchContract,
            ExecutionRequestExpectation,
            CancellationId,
        ),
        DispatchError,
    > {
        let parts = self.parts.take().ok_or(DispatchError::TicketConsumed)?;
        self.semantic_cleanup.disarm();
        Ok((
            DispatchPlan::Scheduled(parts.schedule),
            parts.contract,
            parts.expected,
            parts.cancellation,
        ))
    }

    pub(crate) fn into_parts(
        mut self,
    ) -> Result<
        (
            DispatchPlan<R>,
            RemoteDispatchContract,
            ExecutionRequestExpectation,
            CancellationId,
        ),
        DispatchError,
    > {
        self.take_parts()
    }

    pub(crate) fn is_consumed(&self) -> bool {
        self.parts.is_none()
    }

    pub(crate) fn scheduled(&self) -> Result<&Scheduled<R>, DispatchError> {
        Ok(&self.parts()?.schedule)
    }

    pub(crate) fn scheduled_mut(&mut self) -> Result<&mut Scheduled<R>, DispatchError> {
        Ok(&mut self
            .parts
            .as_mut()
            .ok_or(DispatchError::TicketConsumed)?
            .schedule)
    }
}

/// Compatibility name for older integrations. New composition code should
/// use [`InFlightRemote`], whose private affine state prevents callers from
/// pairing a plan, contract, fence, or cancellation identity from another
/// attempt.
pub type DispatchTicket<R> = InFlightRemote<R>;
/// Owner-versioned key for a type-erased pending remote envelope.
///
/// [`Self::new`] creates an unbound request identity for relation admission;
/// the daemon returns a key with a nonzero owner generation. That generation
/// prevents a stale caller-held key from cancelling or completing a later
/// replacement that happens to reuse the same work/attempt pair.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PendingRemoteKey {
    work_key: WorkKey,
    attempt: AttemptId,
    /// Owner generation bound when the daemon relation admits the envelope.
    /// A key assembled by a caller has generation zero and cannot address a
    /// relation-owned replacement.
    generation: u64,
}

/// Fixed-width wire correlation used to route a result without scanning every
/// in-flight ticket. The envelope still rechecks fence and cancellation after
/// this lookup, so the index is only a routing aid and never an admission
/// proof.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RemoteCorrelationKey {
    work_key: WireIdentity,
    attempt: AttemptId,
}

impl RemoteCorrelationKey {
    /// Creates a correlation key from the exact wire work claim and attempt.
    #[must_use]
    pub const fn new(work_key: WireIdentity, attempt: AttemptId) -> Self {
        Self { work_key, attempt }
    }

    /// Returns the wire work identity.
    #[must_use]
    pub const fn work_key(self) -> WireIdentity {
        self.work_key
    }

    /// Returns the attempt identity.
    #[must_use]
    pub const fn attempt(self) -> AttemptId {
        self.attempt
    }
}

impl PendingRemoteKey {
    /// Creates a key from a retained dispatch ticket.
    #[must_use]
    pub const fn new(work_key: WorkKey, attempt: AttemptId) -> Self {
        Self {
            work_key,
            attempt,
            generation: 0,
        }
    }

    /// Returns the work key portion of the pending key.
    #[must_use]
    pub const fn work_key(self) -> WorkKey {
        self.work_key
    }

    /// Returns the attempt portion of the pending key.
    #[must_use]
    pub const fn attempt(self) -> AttemptId {
        self.attempt
    }

    /// Returns the daemon-owner generation bound to this pending attempt.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    pub(crate) const fn with_generation(self, generation: u64) -> Self {
        Self { generation, ..self }
    }
}
enum RemoteCompletionInput {
    Wire(Box<WireRecipeResult>),
    Fallback {
        bytes: Arc<Vec<u8>>,
        remote_failed: bool,
    },
}

type RemoteCompletionCallback<V, A> = Box<
    dyn FnMut(
            &super::super::Dispatcher<V, A>,
            RemoteCompletionInput,
            u64,
        ) -> Result<DispatchCompletion, DispatchError>
        + Send,
>;

fn retained_size<V, A, R: Relation>(
    request: &WireRecipeRequest,
    contract: &RemoteDispatchContract,
    expected: &ExecutionRequestExpectation,
) -> Result<usize, DispatchError> {
    // Charge actual owned vectors and immutable byte allocations. The wire
    // request is retained for reconnect while the callback owns the typed
    // ticket/contract/expectation. Shared semantic bytes are charged once
    // through the contract capability.
    let request_inputs = request
        .inputs
        .capacity()
        .checked_mul(size_of::<WireIdentity>())
        .ok_or(DispatchError::OutputContract)?;
    let expected_inputs = expected
        .inputs
        .capacity()
        .checked_mul(size_of::<backend_replication::ExpectedIdentity>())
        .ok_or(DispatchError::OutputContract)?;
    size_of::<PendingRemoteEnvelope<V, A>>()
        .checked_add(size_of::<DispatchTicket<R>>())
        .and_then(|size| size.checked_add(size_of::<WireRecipeRequest>()))
        .and_then(|size| size.checked_add(request_inputs))
        .and_then(|size| size.checked_add(expected_inputs))
        .and_then(|size| {
            // `DispatchTicket` already contains the inline contract record;
            // retain only the contract's heap payload here. The checked
            // subtraction also detects an accounting implementation drift.
            let contract_total = contract.retained_size()?;
            let contract_inline = size_of::<RemoteDispatchContract>();
            let contract_payload = contract_total.checked_sub(contract_inline)?;
            size.checked_add(contract_payload)
        })
        .ok_or(DispatchError::OutputContract)
}

/// Type-erased daemon-owned remote work envelope.
///
/// The relation marker is erased from the map key while the callback keeps
/// the original typed ticket and invokes the typed dispatcher completion. A
/// result is first checked against work key, attempt, fence, and cancellation
/// before the callback is consumed, so a late or cross-ticket frame cannot
/// remove the pending operation.
pub struct PendingRemoteEnvelope<V, A> {
    key: PendingRemoteKey,
    /// Exact request retained for reconnect/retry. Inputs are small fixed
    /// identities; retaining this claim keeps the affine fallback ticket
    /// live when a transport generation disappears.
    request: Arc<WireRecipeRequest>,
    /// Canonical logical owner charge for the request, contract, semantic
    /// witness/manifest, and affine envelope metadata. Result payload bytes
    /// are charged separately when a frame enters admission.
    retained_size: usize,
    work_claim: WireIdentity,
    fence: Fence,
    cancellation: CancellationId,
    callback: Option<RemoteCompletionCallback<V, A>>,
    /// Scheduler-terminal completion retained until journal and workspace
    /// publication both succeed. The affine callback is consumed exactly
    /// once, while a failed publication can retry without recreating work.
    prepared: Option<(DispatchCompletion, bool)>,
    cancel_command: CancelAttempt,
    /// Advisory peer cancellation is emitted at most once after the durable
    /// fallback fence commits. Publication retries reuse the prepared local
    /// completion without generating another control message.
    fallback_cancel_attempted: bool,
    /// Shared with the dispatcher so dropping an envelope also releases the
    /// active semantic reader reservation retained by its affine ticket.
    active_semantic: Arc<Mutex<BTreeMap<WorkKey, super::super::ActiveSemantic>>>,
    semantic_generation: Option<u64>,
    terminal: Arc<AtomicBool>,
}

impl<V, A> Drop for PendingRemoteEnvelope<V, A> {
    fn drop(&mut self) {
        let removed = self.semantic_generation.and_then(|generation| {
            let mut active = self
                .active_semantic
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if active
                .get(&self.key.work_key())
                .is_some_and(|candidate| candidate.generation() == generation)
            {
                active.remove(&self.key.work_key())
            } else {
                None
            }
        });
        // A semantic reservation takes the coordinator lock when dropped, so
        // release it after the active-map lock is no longer held.
        drop(removed);
    }
}

impl<V, A> PendingRemoteEnvelope<V, A> {
    pub(crate) fn bind_generation(&mut self, generation: u64) {
        self.key = self.key.with_generation(generation);
    }

    /// Returns the key used to register this pending envelope.
    #[must_use]
    pub const fn key(&self) -> PendingRemoteKey {
        self.key
    }

    /// Returns the fixed-width wire correlation index key.
    #[must_use]
    pub const fn correlation(&self) -> RemoteCorrelationKey {
        RemoteCorrelationKey::new(self.work_claim, self.key.attempt)
    }

    /// Returns the cancellation identity owned by this envelope.
    #[must_use]
    pub const fn cancellation(&self) -> CancellationId {
        self.cancellation
    }

    /// Returns the exact publication fence retained by this envelope.
    #[must_use]
    pub const fn fence(&self) -> Fence {
        self.fence
    }

    /// Returns the complete cancellation command for this exact attempt.
    #[must_use]
    pub const fn cancel_command(&self) -> CancelAttempt {
        self.cancel_command
    }

    /// Returns the exact request retained for a transport rebind.
    #[must_use]
    pub(crate) fn request(&self) -> &WireRecipeRequest {
        self.request.as_ref()
    }

    /// Returns the exact initial owner charge for this pending envelope.
    #[must_use]
    pub(crate) const fn retained_size(&self) -> usize {
        self.retained_size
    }
}

impl<V, A> fmt::Debug for PendingRemoteEnvelope<V, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active_semantic = self.active_semantic.lock().map_or(0, |active| active.len());
        f.debug_struct("PendingRemoteEnvelope")
            .field("key", &self.key)
            .field("request", &self.request)
            .field("retained_size", &self.retained_size)
            .field("work_claim", &self.work_claim)
            .field("fence", &self.fence)
            .field("cancellation", &self.cancellation)
            .field("armed", &self.callback.is_some())
            .field("prepared", &self.prepared.is_some())
            .field("cancel_command", &self.cancel_command)
            .field("fallback_cancel_attempted", &self.fallback_cancel_attempted)
            .field("active_semantic", &active_semantic)
            .field("semantic_generation", &self.semantic_generation)
            .field("terminal", &self.terminal.load(Ordering::Acquire))
            .finish()
    }
}


/// Completion returned after a local or remote result wins publication.
#[derive(Debug)]
pub enum DispatchCompletion {
    /// Existing exact output was reused.
    Reused(ReusableOutput),
    /// A newly accepted output was published by the scheduler.
    Accepted(ScheduleReceipt),
    /// A caller demand joined an existing live attempt.
    Waiting(Interned),
}
