//! Proof-admitted state of one durable local publication lease. Socket work
//! belongs to the caller's worker; cancellation is checked between bounded
//! frames. No partial reset can replace the retained complete root.

use crate::lease_contract::{LeaseMs, PUBLICATION_CREDIT, PUBLICATION_LEASE};
use crate::monotonic::{MonotonicClock, SystemClock};
use crate::reset_budget::{ResetBudget, ResetFault};
use crate::reset_hydration::{
    ResetBinding, ResetHydration, ResetHydrationError, ResetPageProgress,
};
use crate::subscription::subscription_read_from_bytes_against;
use crate::subscription_local::ConnectionId;
use crate::{ClientError, LocalSubscriptionExchangeError, LocalSubscriptionTransport};
use backend_library::{Cursor, CursorEvent, CursorRead, ViewRoot};
use backend_replication::{
    LocalControlExchangeDecision, LocalControlExchangeError, LocalControlExchangeProgress,
    LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionResponse,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CREDIT: usize = PUBLICATION_CREDIT;
const LEASE_MS: u64 = PUBLICATION_LEASE.get();

/// Which fixed budget currently governs one observation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationBudgetKind {
    /// Caller recovery deadline, or the initial ordinary admission window.
    Ordinary,
    /// Authenticated descriptor fixed the complete reset's larger allowance.
    AuthenticatedReset {
        /// Maximum event rows authorized by the reset descriptor.
        rows: u64,
        /// Maximum pages authorized by the reset descriptor.
        pages: u64,
    },
}

/// Fixed timing context exposed to the short observation callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationExchangeBudget {
    started: Instant,
    deadline: Instant,
    kind: PublicationBudgetKind,
}

impl PublicationExchangeBudget {
    /// Start of the currently active fixed budget.
    #[must_use]
    pub const fn started(self) -> Instant {
        self.started
    }

    /// Absolute deadline shared by all requests in this operation.
    #[must_use]
    pub const fn deadline(self) -> Instant {
        self.deadline
    }

    /// Budget class and, for a reset, authenticated row/page limits.
    #[must_use]
    pub const fn kind(self) -> PublicationBudgetKind {
        self.kind
    }

    /// Fixed total allowance represented by this budget.
    #[must_use]
    pub fn allowance(self) -> Duration {
        self.deadline
            .checked_duration_since(self.started)
            .unwrap_or(Duration::ZERO)
    }
}

/// Progress from the exact in-flight frame plus its publication-wide deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationObservationProgress {
    /// Current phase and byte offsets of the one request/reply frame.
    pub exchange: LocalControlExchangeProgress,
    /// Fixed operation budget; this deadline does not move with progress.
    pub budget: PublicationExchangeBudget,
}

/// Decision made by a short desktop freshness/cancellation tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationObservationDecision {
    /// Continue this exact request at its existing byte offsets.
    Continue,
    /// Stop this request and retire its socket.
    Cancel,
}

/// Caller-owned progress and cancellation controls for publication operations.
///
/// The callback receives a copyable progress snapshot, so it can inspect
/// current state without trying to borrow the transport while it is mutably
/// driving a frame. Each operation resets its budget at entry; callback work
/// must remain short and nonblocking.
pub struct ObservedPublicationControl<'a> {
    recovery_deadline: Instant,
    clock: Arc<dyn MonotonicClock>,
    cancelled: &'a dyn Fn() -> bool,
    tick: &'a mut dyn FnMut(PublicationObservationProgress) -> PublicationObservationDecision,
    budget: PublicationExchangeBudget,
    #[cfg(test)]
    after_proof: Option<Box<dyn FnMut()>>,
}

impl<'a> ObservedPublicationControl<'a> {
    /// Creates a control using the caller's absolute ordinary recovery deadline.
    #[must_use]
    pub fn new(
        recovery_deadline: Instant,
        cancelled: &'a dyn Fn() -> bool,
        tick: &'a mut dyn FnMut(PublicationObservationProgress) -> PublicationObservationDecision,
    ) -> Self {
        Self::new_with_clock(recovery_deadline, Arc::new(SystemClock), cancelled, tick)
    }

    /// Creates a control against an injected monotonic clock. Use the same
    /// clock for the transport when testing absolute reset deadlines.
    #[must_use]
    pub fn new_with_clock(
        recovery_deadline: Instant,
        clock: Arc<dyn MonotonicClock>,
        cancelled: &'a dyn Fn() -> bool,
        tick: &'a mut dyn FnMut(PublicationObservationProgress) -> PublicationObservationDecision,
    ) -> Self {
        let started = clock.now();
        Self {
            recovery_deadline,
            clock,
            cancelled,
            tick,
            budget: PublicationExchangeBudget {
                started,
                deadline: recovery_deadline,
                kind: PublicationBudgetKind::Ordinary,
            },
            #[cfg(test)]
            after_proof: None,
        }
    }

    /// Current fixed budget snapshot, useful to schedule caller-side retries.
    #[must_use]
    pub const fn budget(&self) -> PublicationExchangeBudget {
        self.budget
    }

    #[cfg(test)]
    fn set_after_proof_for_test(&mut self, hook: impl FnMut() + 'static) {
        self.after_proof = Some(Box::new(hook));
    }
}

/// Publication operation associated with a producer's explicit rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationOperation {
    /// New producer lease request.
    Open,
    /// Reattach the caller's exact lease and cursor.
    Resume,
    /// Quiet cursor-fencing lease renewal.
    Renew,
    /// Next authenticated reset page.
    Page,
    /// Durable acknowledgement after complete admission.
    Ack,
}

/// Typed terminal result from observed publication processing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationExchangeError {
    /// Endpoint setup or request encoding failed before an exchange began.
    Setup(ClientError),
    /// The in-flight frame stalled, was cancelled, closed, or violated framing.
    Exchange(LocalControlExchangeError),
    /// A complete correlated server response refused this operation.
    ProducerRejected {
        /// Operation that the producer refused; rejection alone is not proof
        /// that a lease expired.
        operation: PublicationOperation,
        /// Correlation identity of the refused operation.
        request_id: u64,
        /// Producer-provided refusal detail.
        message: String,
    },
    /// A complete response or local proof failed publication admission.
    Invalid(ClientError),
    /// Cancellation arrived between complete frames.
    Cancelled,
    /// The fixed publication budget expired during local validation/work.
    BudgetExpired(PublicationExchangeBudget),
}

impl std::fmt::Display for PublicationExchangeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setup(error) => write!(formatter, "publication setup: {error}"),
            Self::Exchange(error) => error.fmt(formatter),
            Self::ProducerRejected {
                operation,
                request_id,
                message,
            } => write!(
                formatter,
                "publication {operation:?} request {request_id} rejected: {message}"
            ),
            Self::Invalid(error) => write!(formatter, "publication admission: {error}"),
            Self::Cancelled => formatter.write_str("publication observation withdrawn"),
            Self::BudgetExpired(budget) => write!(
                formatter,
                "publication {:?} budget expired at {:?}",
                budget.kind, budget.deadline
            ),
        }
    }
}

impl std::error::Error for PublicationExchangeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Setup(error) | Self::Invalid(error) => Some(error),
            Self::Exchange(error) => Some(error),
            Self::ProducerRejected { .. } | Self::Cancelled | Self::BudgetExpired(_) => None,
        }
    }
}

impl PublicationExchangeError {
    fn into_client_error(self) -> ClientError {
        match self {
            Self::Setup(error) | Self::Invalid(error) => error,
            Self::Exchange(error) => ClientError::Protocol(error.to_string()),
            Self::ProducerRejected { message, .. } => {
                ClientError::Protocol(format!("locald subscription: {message}"))
            }
            Self::Cancelled => protocol("publication observation withdrawn"),
            Self::BudgetExpired(_) => protocol("publication observation exceeded its time budget"),
        }
    }
}

impl From<ClientError> for PublicationExchangeError {
    fn from(error: ClientError) -> Self {
        Self::Invalid(error)
    }
}

enum PublicationObserver<'control, 'callback> {
    Legacy(&'callback dyn Fn() -> bool),
    Observed(&'control mut ObservedPublicationControl<'callback>),
}

impl PublicationObserver<'_, '_> {
    fn clock<'a>(&'a self, transport_clock: &'a dyn MonotonicClock) -> &'a dyn MonotonicClock {
        match self {
            Self::Legacy(_) => transport_clock,
            Self::Observed(control) => control.clock.as_ref(),
        }
    }

    #[cfg(test)]
    fn take_after_proof_for_test(&mut self) -> Option<Box<dyn FnMut()>> {
        match self {
            Self::Legacy(_) => None,
            Self::Observed(control) => control.after_proof.take(),
        }
    }

    fn begin_hydration(
        &mut self,
        previous: Cursor,
        transport: &LocalSubscriptionTransport,
    ) -> Result<(ResetHydration, PublicationExchangeBudget), PublicationExchangeError> {
        let (started, initial_deadline) = match self {
            Self::Legacy(_) => {
                let started = transport.now();
                let deadline = transport.request_deadline_at(started).ok_or_else(|| {
                    PublicationExchangeError::Invalid(ResetFault::ClockUnavailable.into())
                })?;
                (started, deadline)
            }
            Self::Observed(control) => (control.clock.now(), control.recovery_deadline),
        };
        let hydration = match ResetHydration::begin_with_initial_deadline(
            previous,
            started,
            initial_deadline,
        ) {
            Ok(hydration) => hydration,
            Err(failure) => return Err(self.budget_error(failure)),
        };
        #[cfg(test)]
        let hydration = {
            let mut hydration = hydration;
            if let Some(hook) = self.take_after_proof_for_test() {
                hydration.set_after_proof_for_test(hook);
            }
            hydration
        };
        self.begin_reset_operation(hydration.budget());
        let initial_budget = reset_exchange_budget(hydration.budget());
        Ok((hydration, initial_budget))
    }

    fn check(&self) -> Result<(), PublicationExchangeError> {
        match self {
            Self::Legacy(cancelled) if cancelled() => Err(PublicationExchangeError::Invalid(
                protocol("publication observation withdrawn"),
            )),
            Self::Legacy(_) => Ok(()),
            Self::Observed(control) if (control.cancelled)() => {
                Err(PublicationExchangeError::Cancelled)
            }
            Self::Observed(control) if control.clock.now() >= control.budget.deadline => {
                Err(PublicationExchangeError::BudgetExpired(control.budget))
            }
            Self::Observed(_) => Ok(()),
        }
    }

    fn check_budget(
        &self,
        budget: &ResetBudget,
        fallback_now: Instant,
    ) -> Result<(), PublicationExchangeError> {
        self.check()?;
        let now = match self {
            Self::Legacy(_) => fallback_now,
            Self::Observed(control) => control.clock.now(),
        };
        budget
            .check(now)
            .map_err(|failure| self.budget_error(failure))
    }

    /// Resets this mutable control for one new Open/Resume operation. The
    /// provisional reset deadline starts at the operation boundary, before
    /// the first request or response decoding.
    fn begin_reset_operation(&mut self, budget: &ResetBudget) {
        if let Self::Observed(control) = self {
            control.budget = PublicationExchangeBudget {
                started: budget.started(),
                deadline: control.recovery_deadline.min(budget.deadline()),
                kind: PublicationBudgetKind::Ordinary,
            };
        }
    }

    /// Starts a fresh ordinary operation on a reusable observed control.
    fn begin_ordinary_operation(&mut self) {
        if let Self::Observed(control) = self {
            control.budget = PublicationExchangeBudget {
                started: control.clock.now(),
                deadline: control.recovery_deadline,
                kind: PublicationBudgetKind::Ordinary,
            };
        }
    }

    fn bind_reset(&mut self, binding: ResetBinding) {
        if let Self::Observed(control) = self {
            control.budget = PublicationExchangeBudget {
                started: binding.started,
                deadline: binding.deadline,
                kind: PublicationBudgetKind::AuthenticatedReset {
                    rows: binding.rows,
                    pages: u64::from(binding.pages),
                },
            };
        }
    }

    fn budget_error(&self, failure: ResetFault) -> PublicationExchangeError {
        if failure == ResetFault::TimeBudget {
            if let Self::Observed(control) = self {
                return PublicationExchangeError::BudgetExpired(control.budget);
            }
        }
        PublicationExchangeError::Invalid(protocol(failure.message()))
    }

    fn request(
        &mut self,
        transport: &mut LocalSubscriptionTransport,
        operation: LocalSubscriptionOperation,
        tag: PublicationOperation,
        reset_budget: Option<PublicationExchangeBudget>,
    ) -> Result<LocalSubscriptionResponse, PublicationExchangeError> {
        self.check()?;
        let (budget, clock) = match self {
            Self::Legacy(_) => {
                let budget = match reset_budget {
                    Some(budget) => budget,
                    None => PublicationExchangeBudget {
                        started: transport.now(),
                        deadline: transport.request_deadline().ok_or_else(|| {
                            PublicationExchangeError::Invalid(ResetFault::ClockUnavailable.into())
                        })?,
                        kind: PublicationBudgetKind::Ordinary,
                    },
                };
                (budget, transport.monotonic_clock())
            }
            Self::Observed(control) => (control.budget, Arc::clone(&control.clock)),
        };
        let mut stopped_for = None;
        let response = transport.lease_request_with_tick(operation, budget.deadline, |exchange| {
            if let Some(stop) = self.stop_at(budget.deadline, clock.as_ref()) {
                stopped_for = Some(stop);
                return LocalControlExchangeDecision::Cancel;
            }
            let decision = match self {
                Self::Legacy(_) => PublicationObservationDecision::Continue,
                Self::Observed(control) => {
                    (control.tick)(PublicationObservationProgress { exchange, budget })
                }
            };
            // A callback can advance time or withdraw this operation. Recheck
            // both before letting this exact frame take another byte step.
            stopped_for = self.stop_at(budget.deadline, clock.as_ref()).or_else(|| {
                (decision == PublicationObservationDecision::Cancel)
                    .then_some(PublicationTickStop::Cancelled)
            });
            if stopped_for.is_some() {
                LocalControlExchangeDecision::Cancel
            } else {
                LocalControlExchangeDecision::Continue
            }
        });
        match response {
            Ok(response) => Ok(response),
            Err(error) => {
                if let LocalSubscriptionExchangeError::Exchange(exchange) = &error {
                    if exchange.failure
                        == backend_replication::LocalControlExchangeFailure::Cancelled
                    {
                        match stopped_for.or_else(|| self.stop_at(budget.deadline, clock.as_ref()))
                        {
                            Some(PublicationTickStop::Cancelled) => {
                                return Err(PublicationExchangeError::Cancelled);
                            }
                            Some(PublicationTickStop::BudgetExpired) => {
                                return Err(PublicationExchangeError::BudgetExpired(budget));
                            }
                            None => {}
                        }
                    }
                    if exchange.failure == backend_replication::LocalControlExchangeFailure::Stalled
                        && clock.now() >= budget.deadline
                    {
                        return Err(PublicationExchangeError::BudgetExpired(budget));
                    }
                }
                if matches!(error, LocalSubscriptionExchangeError::Setup(_))
                    && clock.now() >= budget.deadline
                {
                    return Err(PublicationExchangeError::BudgetExpired(budget));
                }
                Err(map_exchange_error(error, tag))
            }
        }
    }

    fn stop_at(
        &self,
        deadline: Instant,
        clock: &dyn MonotonicClock,
    ) -> Option<PublicationTickStop> {
        let cancelled = match self {
            Self::Legacy(cancelled) => cancelled(),
            Self::Observed(control) => (control.cancelled)(),
        };
        if cancelled {
            Some(PublicationTickStop::Cancelled)
        } else if clock.now() >= deadline {
            Some(PublicationTickStop::BudgetExpired)
        } else {
            None
        }
    }
}

fn map_exchange_error(
    error: LocalSubscriptionExchangeError,
    operation: PublicationOperation,
) -> PublicationExchangeError {
    match error {
        LocalSubscriptionExchangeError::Setup(error) => PublicationExchangeError::Setup(error),
        LocalSubscriptionExchangeError::Rejected {
            request_id,
            message,
        } => PublicationExchangeError::ProducerRejected {
            operation,
            request_id,
            message,
        },
        LocalSubscriptionExchangeError::Invalid(error) => PublicationExchangeError::Invalid(error),
        LocalSubscriptionExchangeError::Exchange(error) => {
            PublicationExchangeError::Exchange(error)
        }
    }
}

fn reset_exchange_budget(budget: &ResetBudget) -> PublicationExchangeBudget {
    let kind = match budget.descriptor() {
        Some((rows, pages)) => PublicationBudgetKind::AuthenticatedReset {
            rows,
            pages: u64::from(pages),
        },
        None => PublicationBudgetKind::Ordinary,
    };
    PublicationExchangeBudget {
        started: budget.started(),
        deadline: budget.deadline(),
        kind,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationTickStop {
    Cancelled,
    BudgetExpired,
}

/// Exact producer state retained across socket reconnects. The lease is an
/// owner-issued identity, never reconstructed from a path or a clock.
pub struct PublicationLease {
    lease: LocalSubscriptionId,
    cursor: Cursor,
    root: Arc<ViewRoot>,
    /// Socket carrying the latest fully correlated response, even when its
    /// page or cursor has not been admitted into the retained root yet.
    cleanup_on: ConnectionId,
}

impl PublicationLease {
    /// Complete root whose producer proofs have been admitted.
    pub fn root(&self) -> Arc<ViewRoot> {
        Arc::clone(&self.root)
    }
    /// Exact admitted producer cursor.
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// How long a quiet holder may wait between renewals: half the term the
    /// owner granted, so one lost renewal still leaves another chance.
    #[must_use]
    pub fn renew_after(&self) -> Duration {
        PUBLICATION_LEASE.renewal_interval()
    }

    fn cleanup_connection(&self) -> ConnectionId {
        self.cleanup_on.clone()
    }
}

impl LocalSubscriptionTransport {
    /// Acquires a lease from a previously admitted exact root/cursor.
    /// # Errors
    /// Returns a transport, proof, cancellation, or bounded-reset error.
    pub fn acquire_publications(
        &mut self,
        root: Arc<ViewRoot>,
        cursor: Cursor,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<PublicationLease, ClientError> {
        self.acquire_publications_impl(root, cursor, PublicationObserver::Legacy(cancelled))
            .map_err(PublicationExchangeError::into_client_error)
    }

    /// Acquires a lease while exposing bounded frame progress and the fixed
    /// publication deadline to the caller's short observation tick.
    ///
    /// # Errors
    /// Returns a typed setup, exchange, producer-refusal, or proof-admission
    /// failure. No incomplete root is installed in a returned lease.
    pub fn acquire_publications_observed(
        &mut self,
        root: Arc<ViewRoot>,
        cursor: Cursor,
        control: &mut ObservedPublicationControl<'_>,
    ) -> Result<PublicationLease, PublicationExchangeError> {
        self.acquire_publications_impl(root, cursor, PublicationObserver::Observed(control))
    }

    fn acquire_publications_impl(
        &mut self,
        root: Arc<ViewRoot>,
        cursor: Cursor,
        mut observer: PublicationObserver<'_, '_>,
    ) -> Result<PublicationLease, PublicationExchangeError> {
        let (hydration, initial_budget) = observer.begin_hydration(cursor, self)?;
        observer.check()?;
        if root.root() != cursor.root() || !root.is_coherent() || root.capability().is_none() {
            return Err(PublicationExchangeError::Invalid(protocol(
                "publication base root/cursor mismatch",
            )));
        }
        let response = observer.request(
            self,
            LocalSubscriptionOperation::Open {
                cursor: cursor.encode_control().into_vec().into_boxed_slice(),
                credit: CREDIT,
                lease_ms: LEASE_MS,
            },
            PublicationOperation::Open,
            Some(initial_budget),
        )?;
        let lease = response.lease();
        let mut state = PublicationLease {
            lease,
            cursor,
            root,
            cleanup_on: self.connection(),
        };
        if let Err(error) = self.admit_publications(&mut state, response, &mut observer, hydration)
        {
            let _ = self.cancel_publications_current(&state);
            return Err(error);
        }
        Ok(state)
    }

    /// Requests a bounded suffix, also extending the retained lease. The
    /// same operation resumes after reconnect; a lost reply is never guessed.
    /// # Errors
    /// A rejected/expired lease leaves the last admitted state unchanged.
    pub fn resume_publications(
        &mut self,
        state: &mut PublicationLease,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), ClientError> {
        self.resume_publications_impl(state, PublicationObserver::Legacy(cancelled))
            .map_err(PublicationExchangeError::into_client_error)
    }

    /// Resumes the exact retained lease while exposing bounded frame progress
    /// and the fixed publication deadline to the caller's observation tick.
    ///
    /// # Errors
    /// Returns a typed setup, exchange, producer-refusal, cancellation, or
    /// proof-admission failure. The retained root and cursor stay unchanged.
    pub fn resume_publications_observed(
        &mut self,
        state: &mut PublicationLease,
        control: &mut ObservedPublicationControl<'_>,
    ) -> Result<(), PublicationExchangeError> {
        self.resume_publications_impl(state, PublicationObserver::Observed(control))
    }

    fn resume_publications_impl(
        &mut self,
        state: &mut PublicationLease,
        mut observer: PublicationObserver<'_, '_>,
    ) -> Result<(), PublicationExchangeError> {
        let (hydration, initial_budget) = observer.begin_hydration(state.cursor, self)?;
        observer.check()?;
        let response = observer.request(
            self,
            LocalSubscriptionOperation::Resume {
                lease: state.lease,
                cursor: state.cursor.encode_control().into_vec().into_boxed_slice(),
                credit: CREDIT,
                lease_ms: LEASE_MS,
            },
            PublicationOperation::Resume,
            Some(initial_budget),
        )?;
        self.admit_publications(state, response, &mut observer, hydration)
    }

    /// Fences a quiet lease to its exact admitted cursor before an idle pause.
    /// # Errors
    /// Returns an error if the owner no longer retains this exact lease.
    pub fn renew_publications(&mut self, state: &mut PublicationLease) -> Result<(), ClientError> {
        let response = self.renew_lease(state.lease, state.cursor, CREDIT, LEASE_MS)?;
        state.cleanup_on = self.connection();
        match response {
            LocalSubscriptionResponse::Renewed {
                cursor, lease_ms, ..
            } if cursor.as_ref() == state.cursor.encode_control().as_ref() => {
                granted_term(lease_ms)?;
                Ok(())
            }
            _ => Err(protocol(
                "publication renewal did not fence the admitted cursor",
            )),
        }
    }

    /// Renews a quiet lease through the same observed frame driver.
    ///
    /// # Errors
    /// Returns a typed setup, exchange, producer-refusal, cancellation, or
    /// cursor-fence failure.
    pub fn renew_publications_observed(
        &mut self,
        state: &mut PublicationLease,
        control: &mut ObservedPublicationControl<'_>,
    ) -> Result<(), PublicationExchangeError> {
        let mut observer = PublicationObserver::Observed(control);
        observer.begin_ordinary_operation();
        let response = observer.request(
            self,
            LocalSubscriptionOperation::Renew {
                lease: state.lease,
                cursor: state.cursor.encode_control().into_vec().into_boxed_slice(),
                credit: CREDIT,
                lease_ms: LEASE_MS,
            },
            PublicationOperation::Renew,
            None,
        )?;
        state.cleanup_on = self.connection();
        let lease_ms = match response {
            LocalSubscriptionResponse::Renewed {
                cursor, lease_ms, ..
            } if cursor.as_ref() == state.cursor.encode_control().as_ref() => lease_ms,
            _ => {
                return Err(PublicationExchangeError::Invalid(protocol(
                    "publication renewal did not fence the admitted cursor",
                )));
            }
        };
        observer.check()?;
        granted_term(lease_ms)?;
        Ok(())
    }

    /// Releases owner-side retention. Socket I/O remains subject to this
    /// transport's timeout; a caller closing a cancelled socket can instead
    /// drop it and let the finite producer lease expire.
    pub fn cancel_publications(&mut self, state: &PublicationLease) -> Result<(), ClientError> {
        match self.cancel_lease(state.lease)? {
            LocalSubscriptionResponse::Cancelled { .. } => Ok(()),
            _ => Err(protocol("publication cancellation was not acknowledged")),
        }
    }

    /// Terminal best-effort release without opening a replacement socket.
    /// Read and write each have a 50ms socket timeout; this does not promise
    /// preemption of native syscalls, scheduling, or decoding.
    /// # Errors
    /// Returns an I/O or protocol error when this exact socket cannot release
    /// the lease; the caller must not assume successful producer cleanup.
    pub fn cancel_publications_current(
        &mut self,
        state: &PublicationLease,
    ) -> Result<(), ClientError> {
        self.cancel_lease_current(
            state.lease,
            state.cleanup_connection(),
            Duration::from_millis(50),
        )
    }

    fn admit_publications(
        &mut self,
        state: &mut PublicationLease,
        mut response: LocalSubscriptionResponse,
        observer: &mut PublicationObserver<'_, '_>,
        mut hydration: ResetHydration,
    ) -> Result<(), PublicationExchangeError> {
        // `response` arrived as a fully correlated frame. Cleanup may use its
        // exact socket even if local proof or cursor admission fails below.
        state.cleanup_on = self.connection();
        let previous = state.cursor;
        // Batch and reset-page replies omit the term, so their successful
        // Resume carries the exact term requested above. Quiet replies carry
        // the grant explicitly and it must match that same fixed term.
        let (root, cursor) = loop {
            observer.check_budget(hydration.budget(), self.now())?;
            if response.lease() != state.lease {
                return Err(invalid("publication response changed lease identity"));
            }
            match response {
                LocalSubscriptionResponse::Opened {
                    cursor, lease_ms, ..
                }
                | LocalSubscriptionResponse::Resumed {
                    cursor, lease_ms, ..
                } => {
                    if hydration.budget().descriptor().is_some()
                        || cursor.as_ref() != previous.encode_control().as_ref()
                    {
                        return Err(invalid("publication acknowledgement changed its cursor"));
                    }
                    granted_term(lease_ms)?;
                    break (Arc::clone(&state.root), previous);
                }
                LocalSubscriptionResponse::Batch {
                    previous: predecessor,
                    cursor: target,
                    payload,
                    ..
                } => {
                    if hydration.budget().descriptor().is_some()
                        || predecessor.as_ref() != previous.encode_control().as_ref()
                    {
                        return Err(invalid("publication batch predecessor mismatch"));
                    }
                    let read = subscription_read_from_bytes_against(
                        &payload,
                        previous,
                        &state.root,
                        state.root.capability(),
                    )?;
                    let CursorRead::Events { cursor, events } = read else {
                        return Err(invalid("publication batch carried a reset"));
                    };
                    if events.len() > CREDIT || target.as_ref() != cursor.encode_control().as_ref()
                    {
                        return Err(invalid("publication batch credit/cursor mismatch"));
                    }
                    let mut root = Arc::clone(&state.root);
                    for event in events {
                        if let CursorEvent::View { delta } = event {
                            root = Arc::new(delta.target_view().clone());
                        }
                    }
                    if root.root() != cursor.root() {
                        return Err(invalid("publication batch target mismatch"));
                    }
                    break (root, cursor);
                }
                LocalSubscriptionResponse::SnapshotPage {
                    page,
                    next,
                    payload,
                    ..
                } => {
                    let peer = self.authenticated_peer().ok_or_else(|| {
                        protocol("publication reset requires authenticated producer")
                    })?;
                    let progress = hydration
                        .admit_page(
                            &page,
                            next.as_deref(),
                            &payload,
                            peer,
                            observer.clock(self.monotonic_clock().as_ref()),
                        )
                        .map_err(|failure| match failure {
                            ResetHydrationError::Budget(fault) => observer.budget_error(fault),
                            ResetHydrationError::Invalid(error) => {
                                PublicationExchangeError::Invalid(error)
                            }
                        })?;
                    let newly_bound = match &progress {
                        ResetPageProgress::Continue { newly_bound, .. }
                        | ResetPageProgress::Complete { newly_bound, .. } => newly_bound,
                    };
                    if let Some(binding) = newly_bound {
                        observer.bind_reset(*binding);
                    }
                    match progress {
                        ResetPageProgress::Complete { cursor, root, .. } => {
                            break (Arc::from(root), cursor);
                        }
                        ResetPageProgress::Continue { continuation, .. } => {
                            observer.check_budget(hydration.budget(), self.now())?;
                            response = observer.request(
                                self,
                                LocalSubscriptionOperation::Page {
                                    lease: state.lease,
                                    page: continuation,
                                    credit: CREDIT,
                                },
                                PublicationOperation::Page,
                                Some(reset_exchange_budget(hydration.budget())),
                            )?;
                            state.cleanup_on = self.connection();
                        }
                    }
                }
                _ => return Err(invalid("unexpected publication lifecycle response")),
            }
        };
        observer.check_budget(hydration.budget(), self.now())?;
        // Quiet Resume has already fenced this exact durable cursor; it
        // does not need another frame or a duplicate root publication.
        if cursor == previous {
            return Ok(());
        }
        let acknowledgement = observer.request(
            self,
            LocalSubscriptionOperation::Ack {
                lease: state.lease,
                cursor: cursor.encode_control().into_vec().into_boxed_slice(),
            },
            PublicationOperation::Ack,
            Some(reset_exchange_budget(hydration.budget())),
        )?;
        state.cleanup_on = self.connection();
        match acknowledgement {
            LocalSubscriptionResponse::Acked {
                cursor: admitted, ..
            } if admitted.as_ref() == cursor.encode_control().as_ref() => {}
            _ => return Err(invalid("publication acknowledgement cursor mismatch")),
        }
        observer.check_budget(hydration.budget(), self.now())?;
        state.cursor = cursor;
        state.root = root;
        Ok(())
    }
}

fn protocol(message: &str) -> ClientError {
    ClientError::Protocol(message.to_owned())
}

fn granted_term(lease_ms: u64) -> Result<LeaseMs, ClientError> {
    let term =
        LeaseMs::new(lease_ms).ok_or_else(|| protocol("publication lease term is invalid"))?;
    if term != PUBLICATION_LEASE {
        return Err(protocol(
            "publication lease term does not match the term requested",
        ));
    }
    Ok(term)
}

fn invalid(message: &str) -> PublicationExchangeError {
    PublicationExchangeError::Invalid(protocol(message))
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::lease_contract::{
        BOOTSTRAP_LEASE, MAX_RESET_ROWS, RESET_BASE_TIME, RESET_PAGE_TIME, ResetPages,
    };
    use backend_library::{
        AuthorityScopeClaim, Basis, Coverage, CoverageCapability, CursorResetReason, Frontier,
        ProducerObservationClaims, ProducerObservationVerifier, Row, RowId, ScopeRoot,
        SnapshotPageDto, UntrustedProducerObservation, ViewPageCursor, ViewRoot, WireCertificate,
        WireClaim, WireSchema, admit_complete_scope, admit_producer_observation,
        empty_view_relation_preimage, encode_id, object_version, symbol_key, view_key,
        view_state_root, view_version_preimage,
    };
    use backend_replication::{
        LocalControlRequest, LocalControlResponse, LocalSubscriptionOperation, decode_request,
        encode_response, frame, read_frame, write_frame,
    };
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixStream;
    use std::thread::JoinHandle;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn explicit_publication_grants_must_match_the_requested_wire_term() {
        assert_eq!(granted_term(LEASE_MS), Ok(PUBLICATION_LEASE));
        assert_eq!(
            granted_term(LEASE_MS - 1),
            Err(protocol(
                "publication lease term does not match the term requested"
            ))
        );
        assert_eq!(
            granted_term(LEASE_MS + 1),
            Err(protocol(
                "publication lease term does not match the term requested"
            ))
        );
        assert_eq!(
            granted_term(0),
            Err(protocol("publication lease term is invalid"))
        );
    }

    struct FixtureVerifier;
    impl ProducerObservationVerifier for FixtureVerifier {
        type Error = &'static str;
        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.evidence() != b"publication-fixture" {
                return Err("invalid fixture");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }

    fn root() -> Arc<ViewRoot> {
        let basis = Basis::new(view_state_root(&[]), object_version(b"publication-source"));
        let scope = ScopeRoot::from_bytes(basis.object.to_bytes());
        let observation = admit_producer_observation(
            UntrustedProducerObservation::new(
                [7; 32],
                scope,
                [9; 32],
                b"publication-fixture".to_vec(),
            ),
            &FixtureVerifier,
        )
        .expect("observation");
        let capability = CoverageCapability::from_authorized_with_evidence(
            admit_complete_scope(
                AuthorityScopeClaim::from_object_version(basis.object),
                observation,
            )
            .expect("source scope"),
            b"publication-fixture".to_vec(),
        )
        .expect("coverage");
        Arc::new(
            ViewRoot::empty_checked(
                view_key(b"publication-view"),
                basis,
                Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
                capability,
            )
            .expect("root"),
        )
    }
    fn pair(
        serve: impl FnOnce(&mut UnixStream) + Send + 'static,
    ) -> (LocalSubscriptionTransport, JoinHandle<()>) {
        let (client, mut server) = UnixStream::pair().expect("pair");
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("server bound");
        server
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("server bound");
        let join = std::thread::spawn(move || serve(&mut server));
        (LocalSubscriptionTransport::from_stream(client), join)
    }
    fn lease_on(
        transport: &LocalSubscriptionTransport,
        lease: LocalSubscriptionId,
        root: Arc<ViewRoot>,
        cursor: Cursor,
    ) -> PublicationLease {
        PublicationLease {
            lease,
            cursor,
            root,
            cleanup_on: transport.connection(),
        }
    }
    fn authenticated_pair(
        serve: impl FnOnce(&mut UnixStream) + Send + 'static,
    ) -> (
        LocalSubscriptionTransport,
        std::path::PathBuf,
        JoinHandle<()>,
    ) {
        let path = std::path::PathBuf::from(format!(
            "/tmp/nudox-pub-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path)
            .expect("bind authenticated local endpoint");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict authenticated local endpoint");
        let join = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept authenticated client");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("owner read deadline");
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .expect("owner write deadline");
            serve(&mut stream);
        });
        let transport = LocalSubscriptionTransport::connect_with_timeouts(
            &path,
            Duration::from_secs(1),
            Duration::from_millis(200),
        )
        .expect("connect and authenticate local producer");
        (transport, path, join)
    }
    fn request(stream: &mut UnixStream) -> (u64, LocalSubscriptionOperation) {
        let body = read_frame(stream, crate::limits()).expect("request frame");
        let LocalControlRequest::Subscription(request) =
            decode_request(&body, crate::limits()).expect("request")
        else {
            panic!("wrong lane")
        };
        (request.request_id, request.operation)
    }
    fn reply(stream: &mut UnixStream, response: LocalControlResponse) {
        let body = encode_response(&response, crate::limits()).expect("response");
        write_frame(stream, &body, crate::limits()).expect("response frame");
    }
    fn reply_fragmented(stream: &mut UnixStream, response: LocalControlResponse) {
        let body = encode_response(&response, crate::limits()).expect("response");
        let wire = frame(&body, crate::limits()).expect("response frame");
        for chunk in wire.chunks(2) {
            stream.write_all(chunk).expect("fragmented response");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    type EncodedSnapshotPage = (Box<[u8]>, Box<[u8]>, Option<Box<[u8]>>);

    fn multi_page_reset_fixture() -> (
        Arc<ViewRoot>,
        Arc<ViewRoot>,
        Cursor,
        Cursor,
        Vec<EncodedSnapshotPage>,
    ) {
        multi_page_reset_fixture_from(None)
    }

    fn multi_page_bootstrap_fixture() -> (
        Arc<ViewRoot>,
        Arc<ViewRoot>,
        Cursor,
        Cursor,
        Vec<EncodedSnapshotPage>,
    ) {
        multi_page_reset_fixture_from(Some(Cursor::new()))
    }

    fn multi_page_reset_fixture_from(
        bootstrap_previous: Option<Cursor>,
    ) -> (
        Arc<ViewRoot>,
        Arc<ViewRoot>,
        Cursor,
        Cursor,
        Vec<EncodedSnapshotPage>,
    ) {
        let initial = root();
        let basis = initial.basis();
        let labels = (0..130)
            .map(|index| format!("ObservedItem{index:03}"))
            .collect::<Vec<_>>();
        let rows = labels
            .iter()
            .map(|label| Row::new(RowId::Symbol(symbol_key(label)), basis, label.clone()))
            .collect::<Vec<_>>();
        let target = Arc::new(
            ViewRoot::new_checked(
                view_key(b"publication-view"),
                basis,
                initial.frontier(),
                rows,
                vec![Coverage::Complete],
                initial.capability().expect("complete fixture capability"),
            )
            .expect("multi-page target root"),
        );
        let previous = bootstrap_previous.unwrap_or_else(|| Cursor::for_view_root_at(&initial, 0));
        let cursor = Cursor::for_view_root_at(&target, 1);
        let capability = target.capability().expect("target capability");
        let source_id = encode_id(basis.object.as_bytes());
        let mut certificate = WireCertificate::new()
            .with_claim(WireClaim::KeyBytes {
                schema: WireSchema::ViewRecipe,
                id: encode_id(target.recipe().as_bytes()),
                value: b"publication-view".to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Version {
                schema: WireSchema::ViewVersion,
                id: encode_id(target.version().as_bytes()),
                value: view_version_preimage(
                    target.recipe(),
                    basis,
                    target.frontier(),
                    target.root(),
                    target.coverage(),
                )
                .into_boxed_slice(),
            })
            .with_claim(WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: encode_id(target.root().as_bytes()),
            })
            .with_claim(WireClaim::Root {
                schema: WireSchema::ViewRelation,
                id: encode_id(basis.root.as_bytes()),
                canonical: empty_view_relation_preimage().into_boxed_slice(),
            })
            .with_claim(WireClaim::Version {
                schema: WireSchema::Object,
                id: source_id.clone(),
                value: b"publication-source".to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Coverage {
                scope: source_id.clone(),
                observed: source_id,
                producer: encode_id(&capability.producer_identity()),
                context: encode_id(&capability.context()),
                evidence: capability.evidence().to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Key {
                schema: WireSchema::Branch,
                id: encode_id(basis.branch.as_bytes()),
                value: "main".to_owned(),
            })
            .with_claim(WireClaim::Key {
                schema: WireSchema::Log,
                id: encode_id(basis.log.as_bytes()),
                value: "library".to_owned(),
            })
            .with_claim(WireClaim::Cursor {
                recipe: encode_id(cursor.recipe().as_bytes()),
                version: encode_id(cursor.version().as_bytes()),
                branch: encode_id(cursor.branch().as_bytes()),
                log: encode_id(cursor.log().as_bytes()),
                schema: cursor.schema(),
                root: encode_id(cursor.root().as_bytes()),
                sequence: cursor.sequence(),
            });
        for label in &labels {
            certificate = certificate.with_claim(WireClaim::Key {
                schema: WireSchema::Symbol,
                id: encode_id(symbol_key(label).as_bytes()),
                value: label.clone(),
            });
        }

        let mut request_page = Vec::<u8>::new().into_boxed_slice();
        let mut page_cursor = ViewPageCursor::first(&target);
        let mut pages = Vec::new();
        loop {
            let page = target.page(page_cursor, CREDIT).expect("bounded view page");
            let following_cursor = page.next();
            let dto = SnapshotPageDto::try_new(
                previous,
                cursor,
                target.descriptor(),
                page,
                CursorResetReason::Pruned,
            )
            .expect("checked snapshot page")
            .with_certificate(certificate.clone());
            let next = dto.next_token().expect("continuation token");
            let payload = serde_json::to_vec(&dto)
                .expect("encode authenticated snapshot page")
                .into_boxed_slice();
            pages.push((request_page, payload, next.clone()));
            match (following_cursor, next) {
                (Some(following_cursor), Some(next)) => {
                    page_cursor = following_cursor;
                    request_page = next;
                }
                (None, None) => break,
                _ => panic!("fixture page token and page cursor disagree"),
            }
        }
        assert_eq!(pages.len(), 3);
        (initial, target, previous, cursor, pages)
    }

    fn serve_snapshot_reset(
        stream: &mut UnixStream,
        previous: Cursor,
        target_cursor: Cursor,
        lease: LocalSubscriptionId,
        pages: Vec<EncodedSnapshotPage>,
        corrupt_page: Option<usize>,
        reject_ack_cursor: bool,
        close_before_ack: bool,
        acknowledged: Option<std::sync::mpsc::Sender<u64>>,
    ) {
        let (request_id, operation) = request(stream);
        let request_cursor = match operation {
            LocalSubscriptionOperation::Open { cursor, .. }
            | LocalSubscriptionOperation::Resume { cursor, .. } => cursor,
            _ => panic!("snapshot reset must begin with Open or Resume"),
        };
        assert_eq!(request_cursor.as_ref(), previous.encode_control().as_ref());
        let (page, payload, next) = &pages[0];
        let payload = if corrupt_page == Some(0) {
            let mut malformed = payload.to_vec();
            if let Some(first) = malformed.first_mut() {
                *first = b'!';
            }
            malformed.into_boxed_slice()
        } else {
            payload.clone()
        };
        reply(
            stream,
            LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                request_id,
                lease,
                page: page.clone(),
                next: next.clone(),
                credit: CREDIT,
                payload,
            }),
        );
        if corrupt_page == Some(0) {
            return;
        }
        for (index, (page, payload, next)) in pages.iter().enumerate().skip(1) {
            let (request_id, operation) = request(stream);
            let LocalSubscriptionOperation::Page {
                lease: requested_lease,
                page: requested_page,
                credit: CREDIT,
            } = operation
            else {
                panic!("reset continuation must be a page request");
            };
            assert_eq!(requested_lease, lease);
            assert_eq!(requested_page.as_ref(), page.as_ref());
            let payload = if corrupt_page == Some(index) {
                let mut malformed = payload.to_vec();
                if let Some(first) = malformed.first_mut() {
                    *first = b'!';
                }
                malformed.into_boxed_slice()
            } else {
                payload.clone()
            };
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page: requested_page,
                    next: next.clone(),
                    credit: CREDIT,
                    payload,
                }),
            );
            if corrupt_page == Some(index) {
                return;
            }
        }

        if close_before_ack {
            return;
        }

        let (ack_id, operation) = request(stream);
        let LocalSubscriptionOperation::Ack {
            lease: ack_lease,
            cursor: ack_cursor,
        } = operation
        else {
            panic!("complete reset must finish with one Ack");
        };
        assert_eq!(ack_lease, lease);
        let requested_cursor = target_cursor.encode_control();
        assert_eq!(ack_cursor.as_ref(), requested_cursor.as_ref());
        let acknowledged_cursor = if reject_ack_cursor {
            previous.encode_control()
        } else {
            requested_cursor
        };
        if let Some(acknowledged) = acknowledged {
            acknowledged.send(ack_id).expect("report Ack correlation");
        }
        reply(
            stream,
            LocalControlResponse::Subscription(LocalSubscriptionResponse::Acked {
                request_id: ack_id,
                lease,
                cursor: acknowledged_cursor,
            }),
        );
    }

    #[test]
    fn quiet_acquire_reconnect_resume_and_renew_keep_the_exact_shared_root() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Open { cursor: sent, credit: CREDIT, lease_ms: LEASE_MS } if sent.as_ref() == cursor.encode_control().as_ref())
            );
            reply_fragmented(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Opened {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
        });
        let mut state = transport
            .acquire_publications(Arc::clone(&root), cursor, &|| false)
            .expect("acquire");
        owner.join().expect("owner");
        drop(transport);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Resume { lease: sent, cursor: sent_cursor, .. } if sent == lease && sent_cursor.as_ref() == cursor.encode_control().as_ref())
            );
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Renew { lease: sent, .. } if sent == lease)
            );
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Renewed {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Cancel { lease: sent } if sent == lease)
            );
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                    request_id,
                    lease,
                }),
            );
        });
        transport
            .resume_publications(&mut state, &|| false)
            .expect("resume");
        assert!(Arc::ptr_eq(&state.root(), &root));
        assert_eq!(state.cursor(), cursor);
        transport.renew_publications(&mut state).expect("renew");
        transport.cancel_publications(&state).expect("cancel");
        owner.join().expect("owner");
    }

    #[test]
    fn observed_open_and_renew_expose_the_callers_fixed_deadline() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(matches!(operation, LocalSubscriptionOperation::Open { .. }));
            reply_fragmented(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Opened {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
        });
        let recovery_deadline = Instant::now() + Duration::from_secs(3);
        let cancelled = || false;
        let mut progress = Vec::new();
        let mut tick = |item| {
            progress.push(item);
            PublicationObservationDecision::Continue
        };
        let mut control = ObservedPublicationControl::new(recovery_deadline, &cancelled, &mut tick);
        let mut state = transport
            .acquire_publications_observed(Arc::clone(&root), cursor, &mut control)
            .expect("observed open");
        drop(control);
        assert_eq!(state.cursor(), cursor);
        owner.join().expect("owner");
        assert!(!progress.is_empty());
        assert!(progress.len() > 3, "fragmented reply spans multiple ticks");
        assert!(progress.iter().all(|step| {
            step.exchange.deadline == recovery_deadline
                && step.budget.deadline() == recovery_deadline
                && step.budget.kind() == PublicationBudgetKind::Ordinary
        }));
        assert!(
            progress
                .windows(2)
                .all(|pair| pair[1].exchange.elapsed >= pair[0].exchange.elapsed)
        );

        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Renew { .. }
            ));
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Renewed {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
        });
        let recovery_deadline = Instant::now() + Duration::from_secs(3);
        let cancelled = || false;
        let mut progress = Vec::new();
        let mut tick = |item| {
            progress.push(item);
            PublicationObservationDecision::Continue
        };
        let mut control = ObservedPublicationControl::new(recovery_deadline, &cancelled, &mut tick);
        transport
            .renew_publications_observed(&mut state, &mut control)
            .expect("observed renewal");
        drop(control);
        owner.join().expect("owner");
        assert!(!progress.is_empty());
        assert!(progress.iter().all(|step| {
            step.exchange.deadline == recovery_deadline
                && step.budget.deadline() == recovery_deadline
                && step.budget.kind() == PublicationBudgetKind::Ordinary
        }));
    }

    #[test]
    fn observed_ordinary_admission_uses_the_minimum_fixed_deadline() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        for caller_window in [Duration::from_secs(2), Duration::from_secs(30)] {
            let (mut transport, owner) = pair(move |stream| {
                let (request_id, operation) = request(stream);
                assert!(matches!(operation, LocalSubscriptionOperation::Open { .. }));
                reply(
                    stream,
                    LocalControlResponse::Subscription(LocalSubscriptionResponse::Opened {
                        request_id,
                        lease,
                        cursor: cursor.encode_control(),
                        credit: CREDIT,
                        lease_ms: LEASE_MS,
                    }),
                );
            });
            let recovery_deadline = Instant::now() + caller_window;
            let cancelled = || false;
            let mut progress = Vec::new();
            let mut tick = |item| {
                progress.push(item);
                PublicationObservationDecision::Continue
            };
            let mut control =
                ObservedPublicationControl::new(recovery_deadline, &cancelled, &mut tick);
            let state = transport
                .acquire_publications_observed(Arc::clone(&root), cursor, &mut control)
                .expect("quiet observed open");
            let budget = control.budget();
            assert_eq!(state.cursor(), cursor);
            drop(control);
            drop(tick);
            assert_eq!(budget.kind(), PublicationBudgetKind::Ordinary);
            assert_eq!(
                budget.deadline(),
                recovery_deadline.min(budget.started() + RESET_BASE_TIME)
            );
            assert!(progress.iter().all(|step| {
                step.budget.started() == budget.started()
                    && step.exchange.deadline == budget.deadline()
                    && step.budget.deadline() == budget.deadline()
            }));
            drop(transport);
            owner.join().expect("owner");
        }
    }

    #[test]
    fn delayed_initial_open_uses_the_pre_request_provisional_deadline() {
        use crate::monotonic::{ManualClock, MonotonicClock};

        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let clock = ManualClock::new();
        let (mut transport, owner) = pair(move |stream| {
            let (_request_id, operation) = request(stream);
            assert!(matches!(operation, LocalSubscriptionOperation::Open { .. }));
            assert!(read_frame(stream, crate::limits()).is_err());
        });
        transport = transport.with_clock(Arc::<ManualClock>::clone(&clock));
        let started = clock.now();
        let recovery_deadline = started + Duration::from_secs(30);
        let cancelled = || false;
        let mut advanced = false;
        let mut tick = |progress: PublicationObservationProgress| {
            if !advanced
                && progress.exchange.phase
                    == backend_replication::LocalControlExchangePhase::ReadingHeader
            {
                clock.advance(RESET_BASE_TIME);
                advanced = true;
            }
            PublicationObservationDecision::Continue
        };
        let mut control = ObservedPublicationControl::new_with_clock(
            recovery_deadline,
            Arc::<ManualClock>::clone(&clock),
            &cancelled,
            &mut tick,
        );

        let error = match transport.acquire_publications_observed(
            Arc::clone(&root),
            cursor,
            &mut control,
        ) {
            Err(error) => error,
            Ok(_) => panic!("the delayed initial Open exceeded the provisional window"),
        };
        assert_eq!(
            error,
            PublicationExchangeError::BudgetExpired(PublicationExchangeBudget {
                started,
                deadline: started + RESET_BASE_TIME,
                kind: PublicationBudgetKind::Ordinary,
            })
        );
        assert_eq!(clock.now(), started + RESET_BASE_TIME);
        assert_eq!(control.budget().started(), started);
        assert_eq!(control.budget().deadline(), started + RESET_BASE_TIME);
        assert_eq!(control.budget().kind(), PublicationBudgetKind::Ordinary);
        drop(control);
        drop(tick);
        assert!(advanced, "the clock advanced during the delayed Open reply");
        drop(transport);
        owner.join().expect("owner");
    }

    #[test]
    fn delayed_initial_resume_uses_the_pre_request_provisional_deadline() {
        use crate::monotonic::{ManualClock, MonotonicClock};

        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([42; 16]);
        let clock = ManualClock::new();
        let (mut transport, owner) = pair(move |stream| {
            let (_request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Resume { lease: sent, .. } if sent == lease)
            );
            assert!(read_frame(stream, crate::limits()).is_err());
        });
        transport = transport.with_clock(Arc::<ManualClock>::clone(&clock));
        let state = lease_on(&transport, lease, Arc::clone(&root), cursor);
        let mut state = state;
        let started = clock.now();
        let recovery_deadline = started + Duration::from_secs(30);
        let cancelled = || false;
        let mut advanced = false;
        let mut tick = |progress: PublicationObservationProgress| {
            if !advanced
                && progress.exchange.phase
                    == backend_replication::LocalControlExchangePhase::ReadingHeader
            {
                clock.advance(RESET_BASE_TIME);
                advanced = true;
            }
            PublicationObservationDecision::Continue
        };
        let mut control = ObservedPublicationControl::new_with_clock(
            recovery_deadline,
            Arc::<ManualClock>::clone(&clock),
            &cancelled,
            &mut tick,
        );

        let error = transport
            .resume_publications_observed(&mut state, &mut control)
            .expect_err("the delayed initial Resume exhausted the provisional reset window");
        assert_eq!(
            error,
            PublicationExchangeError::BudgetExpired(PublicationExchangeBudget {
                started,
                deadline: started + RESET_BASE_TIME,
                kind: PublicationBudgetKind::Ordinary,
            })
        );
        assert_eq!(clock.now(), started + RESET_BASE_TIME);
        assert_eq!(control.budget().started(), started);
        assert_eq!(control.budget().deadline(), started + RESET_BASE_TIME);
        assert_eq!(control.budget().kind(), PublicationBudgetKind::Ordinary);
        assert_eq!(state.cursor(), cursor);
        assert_eq!(state.root().root(), root.root());
        drop(control);
        drop(tick);
        assert!(
            advanced,
            "the clock advanced during the delayed Resume reply"
        );
        drop(transport);
        owner.join().expect("owner");
    }

    #[test]
    fn authenticated_page_proof_cannot_cross_the_shorter_caller_deadline() {
        use crate::monotonic::{ManualClock, MonotonicClock};

        let (initial, _target, previous, _target_cursor, pages) = multi_page_reset_fixture();
        let (page, payload, next) = pages.into_iter().next().expect("first reset page");
        let lease = LocalSubscriptionId::from_bytes([44; 16]);
        let (mut transport, path, owner) = authenticated_pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Resume { lease: sent, cursor, .. }
                    if sent == lease && cursor.as_ref() == previous.encode_control().as_ref()
            ));
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page,
                    next,
                    credit: CREDIT,
                    payload,
                }),
            );
        });
        let clock = ManualClock::new();
        transport = transport.with_clock(Arc::<ManualClock>::clone(&clock));
        let mut state = lease_on(&transport, lease, Arc::clone(&initial), previous);
        let started = clock.now();
        let recovery_deadline = started + Duration::from_secs(2);
        let cancelled = || false;
        let mut tick = |_| PublicationObservationDecision::Continue;
        let mut control = ObservedPublicationControl::new_with_clock(
            recovery_deadline,
            Arc::<ManualClock>::clone(&clock),
            &cancelled,
            &mut tick,
        );
        let proof_clock = Arc::clone(&clock);
        control.set_after_proof_for_test(move || proof_clock.advance(Duration::from_secs(2)));

        let error = transport
            .resume_publications_observed(&mut state, &mut control)
            .expect_err("proof crossed the shorter caller recovery deadline");
        assert_eq!(
            error,
            PublicationExchangeError::BudgetExpired(PublicationExchangeBudget {
                started,
                deadline: recovery_deadline,
                kind: PublicationBudgetKind::Ordinary,
            })
        );
        assert_eq!(clock.now(), recovery_deadline);
        assert_eq!(control.budget().started(), started);
        assert_eq!(control.budget().deadline(), recovery_deadline);
        assert_eq!(control.budget().kind(), PublicationBudgetKind::Ordinary);
        assert_eq!(state.cursor(), previous);
        assert_eq!(state.root().root(), initial.root());
        drop(control);
        drop(tick);
        drop(transport);
        owner.join().expect("authenticated owner");
        std::fs::remove_file(path).expect("remove test socket");
    }

    #[test]
    fn legacy_open_io_deadline_is_the_proof_admission_ceiling() {
        use crate::monotonic::{ManualClock, MonotonicClock};

        let (_initial, _target, previous, _target_cursor, pages) = multi_page_reset_fixture();
        let (page, payload, next) = pages.into_iter().next().expect("first reset page");
        let lease = LocalSubscriptionId::from_bytes([45; 16]);
        let (mut transport, path, owner) = authenticated_pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Open { cursor, .. }
                    if cursor.as_ref() == previous.encode_control().as_ref()
            ));
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page,
                    next,
                    credit: CREDIT,
                    payload,
                }),
            );
        });
        let clock = ManualClock::new();
        transport = transport.with_clock(Arc::<ManualClock>::clone(&clock));
        let started = clock.now();
        let io_deadline = started + Duration::from_millis(200);
        let cancelled = || false;
        let mut observer = PublicationObserver::Legacy(&cancelled);
        let (mut hydration, initial_budget) = observer
            .begin_hydration(previous, &transport)
            .expect("legacy operation captures one initial deadline");
        assert_eq!(initial_budget.started(), started);
        assert_eq!(initial_budget.deadline(), io_deadline);

        let response = observer
            .request(
                &mut transport,
                LocalSubscriptionOperation::Open {
                    cursor: previous.encode_control().into_vec().into_boxed_slice(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                },
                PublicationOperation::Open,
                Some(initial_budget),
            )
            .expect("authenticated initial page response");
        let LocalSubscriptionResponse::SnapshotPage {
            page,
            next,
            payload,
            ..
        } = response
        else {
            panic!("Open returned a snapshot page");
        };
        let proof_clock = Arc::clone(&clock);
        hydration.set_after_proof_for_test(Box::new(move || {
            proof_clock.advance(Duration::from_millis(200));
        }));

        assert!(matches!(
            hydration.admit_page(
                &page,
                next.as_deref(),
                &payload,
                transport.authenticated_peer().expect("authenticated peer"),
                clock.as_ref(),
            ),
            Err(ResetHydrationError::Budget(ResetFault::TimeBudget))
        ));
        assert_eq!(hydration.budget().descriptor(), None);
        assert_eq!(hydration.budget().deadline(), io_deadline);
        assert_eq!(clock.now(), io_deadline);

        drop(observer);
        drop(transport);
        owner.join().expect("authenticated owner");
        std::fs::remove_file(path).expect("remove test socket");
    }

    #[test]
    fn bootstrap_hydrates_a_valid_multi_page_root_and_cancels_on_its_correlated_socket() {
        let (_initial, target, _previous, target_cursor, pages) = multi_page_bootstrap_fixture();
        let lease = LocalSubscriptionId::from_bytes([31; 16]);
        let (mut transport, path, owner) = authenticated_pair(move |stream| {
            let (open_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Open { cursor, credit: PUBLICATION_CREDIT, lease_ms }
                    if cursor.is_empty() && lease_ms == BOOTSTRAP_LEASE.get()
            ));

            let mut expected_page = Box::<[u8]>::default();
            for (index, (page, payload, next)) in pages.into_iter().enumerate() {
                let request_id = if index == 0 {
                    assert!(page.is_empty(), "bootstrap starts at the first page");
                    open_id
                } else {
                    let (request_id, operation) = request(stream);
                    assert!(matches!(
                        operation,
                        LocalSubscriptionOperation::Page {
                            lease: requested_lease,
                            page: requested_page,
                            credit: PUBLICATION_CREDIT,
                        } if requested_lease == lease && requested_page == expected_page
                    ));
                    request_id
                };
                expected_page = next.clone().unwrap_or_default();
                reply(
                    stream,
                    LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                        request_id,
                        lease,
                        page,
                        next,
                        credit: PUBLICATION_CREDIT,
                        payload,
                    }),
                );
            }

            let (cancel_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Cancel { lease: requested_lease }
                    if requested_lease == lease
            ));
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                    request_id: cancel_id,
                    lease,
                }),
            );
        });

        let (root, cursor) = transport.bootstrap_root().expect("admitted bootstrap");
        assert_eq!(root.root(), target.root());
        assert_eq!(cursor, target_cursor);
        owner.join().expect("one authenticated owner connection");
        std::fs::remove_file(path).expect("remove authenticated test socket");
    }

    #[test]
    fn observed_authenticated_reset_pages_and_ack_share_one_expanded_deadline() {
        use crate::monotonic::{ManualClock, MonotonicClock};

        let (initial, target, previous, target_cursor, pages) = multi_page_reset_fixture();
        let lease = LocalSubscriptionId::from_bytes([11; 16]);
        let (ack_sent, ack_received) = std::sync::mpsc::channel();
        let (mut transport, path, owner) = authenticated_pair(move |stream| {
            serve_snapshot_reset(
                stream,
                previous,
                target_cursor,
                lease,
                pages,
                None,
                false,
                false,
                Some(ack_sent),
            );
        });
        let clock = ManualClock::new();
        transport = transport.with_clock(Arc::<ManualClock>::clone(&clock));
        // The authenticated reset budget must expand beyond this caller's
        // short ordinary recovery window, from the original Open start, but
        // only after proof admission completes before that initial ceiling.
        let started = clock.now();
        let recovery_deadline = started + Duration::from_secs(2);
        let cancelled = || false;
        let mut progress = Vec::new();
        let mut tick = |item| {
            progress.push(item);
            PublicationObservationDecision::Continue
        };
        let mut control = ObservedPublicationControl::new_with_clock(
            recovery_deadline,
            Arc::<ManualClock>::clone(&clock),
            &cancelled,
            &mut tick,
        );
        let proof_clock = Arc::clone(&clock);
        control.set_after_proof_for_test(move || proof_clock.advance(Duration::from_secs(1)));
        let state = transport
            .acquire_publications_observed(Arc::clone(&initial), previous, &mut control)
            .expect("fully admitted multi-page reset");
        let ack_id = ack_received
            .recv_timeout(Duration::from_secs(1))
            .expect("server observed final Ack");
        assert_eq!(state.cursor(), target_cursor);
        assert_eq!(state.root().root(), target.root());
        assert_eq!(clock.now(), started + Duration::from_secs(1));
        let final_budget = control.budget();
        drop(control);
        drop(tick);
        drop(transport);
        owner.join().expect("authenticated owner");
        std::fs::remove_file(path).expect("remove test socket");

        let reset_steps = progress
            .iter()
            .filter(|step| {
                matches!(
                    step.budget.kind(),
                    PublicationBudgetKind::AuthenticatedReset {
                        rows: 130,
                        pages: 3
                    }
                )
            })
            .collect::<Vec<_>>();
        assert!(
            !reset_steps.is_empty(),
            "page and Ack ticks observe reset budget"
        );
        let fixed = reset_steps[0].budget;
        assert_eq!(fixed.started(), started);
        assert_eq!(fixed.allowance(), RESET_BASE_TIME + RESET_PAGE_TIME * 3);
        assert!(fixed.deadline() > recovery_deadline);
        assert!(reset_steps.iter().all(|step| {
            step.budget.started() == fixed.started()
                && step.budget.deadline() == fixed.deadline()
                && step.exchange.deadline == fixed.deadline()
        }));
        assert!(
            reset_steps
                .iter()
                .any(|step| step.exchange.request_id == ack_id)
        );
        let initial_open = progress
            .iter()
            .find(|step| {
                step.exchange.request_id == 1
                    && step.budget.kind() == PublicationBudgetKind::Ordinary
            })
            .expect("initial Open progress");
        assert_eq!(initial_open.budget.started(), fixed.started());
        assert_eq!(initial_open.exchange.deadline, recovery_deadline);
        assert!(
            progress
                .iter()
                .filter(|step| step.exchange.request_id != ack_id)
                .all(|step| step.exchange.deadline == step.budget.deadline())
        );
        assert!(progress.iter().any(|step| {
            step.budget.kind() == PublicationBudgetKind::Ordinary
                && step.exchange.deadline == recovery_deadline
        }));
        assert_eq!(
            final_budget.kind(),
            PublicationBudgetKind::AuthenticatedReset {
                rows: 130,
                pages: 3
            }
        );
        assert_eq!(final_budget.deadline(), fixed.deadline());
    }

    #[test]
    fn a_reused_observed_control_starts_fresh_renew_and_resume_budgets() {
        use crate::monotonic::{ManualClock, MonotonicClock};

        let (initial, _target, previous, target_cursor, pages) = multi_page_reset_fixture();
        let lease = LocalSubscriptionId::from_bytes([43; 16]);
        let (mut transport, path, owner) = authenticated_pair(move |stream| {
            serve_snapshot_reset(
                stream,
                previous,
                target_cursor,
                lease,
                pages,
                None,
                false,
                false,
                None,
            );
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Renew { lease: sent, .. } if sent == lease
            ));
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Renewed {
                    request_id,
                    lease,
                    cursor: target_cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Resume { lease: sent, .. } if sent == lease
            ));
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: target_cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
        });
        let clock = ManualClock::new();
        transport = transport.with_clock(Arc::<ManualClock>::clone(&clock));
        let mut state = lease_on(&transport, lease, Arc::clone(&initial), previous);
        let operation_started = clock.now();
        let recovery_deadline = operation_started + Duration::from_secs(60);
        let cancelled = || false;
        let mut tick = |_| PublicationObservationDecision::Continue;
        let mut control = ObservedPublicationControl::new_with_clock(
            recovery_deadline,
            Arc::<ManualClock>::clone(&clock),
            &cancelled,
            &mut tick,
        );

        transport
            .resume_publications_observed(&mut state, &mut control)
            .expect("initial reset and Ack complete");
        let reset_budget = control.budget();
        assert!(matches!(
            reset_budget.kind(),
            PublicationBudgetKind::AuthenticatedReset {
                rows: 130,
                pages: 3
            }
        ));
        assert_eq!(reset_budget.started(), operation_started);
        assert_eq!(
            reset_budget.deadline(),
            operation_started + RESET_BASE_TIME + RESET_PAGE_TIME * 3
        );
        assert_eq!(state.cursor(), target_cursor);

        // The completed reset deadline is now elapsed. A new quiet operation
        // still uses the original, caller-owned recovery ceiling.
        clock.advance(reset_budget.allowance() + Duration::from_secs(1));
        let renew_started = clock.now();
        transport
            .renew_publications_observed(&mut state, &mut control)
            .expect("renew starts an ordinary operation");
        let renew_budget = control.budget();
        assert_eq!(renew_budget.kind(), PublicationBudgetKind::Ordinary);
        assert_eq!(renew_budget.started(), renew_started);
        assert_eq!(renew_budget.deadline(), recovery_deadline);
        assert!(renew_budget.deadline() > reset_budget.deadline());

        clock.advance(Duration::from_secs(1));
        let resume_started = clock.now();
        transport
            .resume_publications_observed(&mut state, &mut control)
            .expect("quiet resume starts another ordinary operation");
        let resume_budget = control.budget();
        assert_eq!(resume_budget.kind(), PublicationBudgetKind::Ordinary);
        assert_eq!(resume_budget.started(), resume_started);
        assert_eq!(
            resume_budget.deadline(),
            recovery_deadline.min(resume_started + RESET_BASE_TIME)
        );
        assert_eq!(state.cursor(), target_cursor);
        drop(control);
        drop(transport);
        owner.join().expect("authenticated owner");
        std::fs::remove_file(path).expect("remove test socket");
    }

    #[test]
    fn invalid_authenticated_reset_page_or_ack_never_replaces_retained_root() {
        for (corrupt_page, reject_ack_cursor) in [(Some(1), false), (None, true)] {
            let (initial, _target, previous, target_cursor, pages) = multi_page_reset_fixture();
            let lease = LocalSubscriptionId::from_bytes([12; 16]);
            let (mut transport, path, owner) = authenticated_pair(move |stream| {
                serve_snapshot_reset(
                    stream,
                    previous,
                    target_cursor,
                    lease,
                    pages,
                    corrupt_page,
                    reject_ack_cursor,
                    false,
                    None,
                );
            });
            let mut state = lease_on(&transport, lease, Arc::clone(&initial), previous);
            let recovery_deadline = Instant::now() + Duration::from_secs(30);
            let cancelled = || false;
            let mut tick = |_| PublicationObservationDecision::Continue;
            let mut control =
                ObservedPublicationControl::new(recovery_deadline, &cancelled, &mut tick);
            let error = transport
                .resume_publications_observed(&mut state, &mut control)
                .expect_err("invalid reset page or acknowledgement");
            assert!(matches!(error, PublicationExchangeError::Invalid(_)));
            assert_eq!(state.cursor(), previous);
            assert_eq!(state.root().root(), initial.root());
            drop(control);
            drop(transport);
            owner.join().expect("authenticated owner");
            std::fs::remove_file(path).expect("remove test socket");
        }
    }

    #[test]
    fn observed_cancellation_before_reset_ack_preserves_the_retained_state() {
        let (initial, _target, previous, target_cursor, pages) = multi_page_reset_fixture();
        let lease = LocalSubscriptionId::from_bytes([13; 16]);
        let (mut transport, path, owner) = authenticated_pair(move |stream| {
            serve_snapshot_reset(
                stream,
                previous,
                target_cursor,
                lease,
                pages,
                None,
                false,
                true,
                None,
            );
        });
        let mut state = lease_on(&transport, lease, Arc::clone(&initial), previous);
        let connection = transport.connection();
        // Resume, two continuation pages and then Ack. The owner helper closes
        // after the final page; this test specifically exercises a caller
        // cancellation at Ack's first byte boundary, before any Ack I/O.
        let cancelled = || false;
        let mut ack_start = None;
        let mut tick = |progress: PublicationObservationProgress| {
            if progress.exchange.request_id == 4 && ack_start.is_none() {
                ack_start = Some(progress.exchange);
            }
            if progress.exchange.request_id == 4
                && progress.exchange.phase
                    == backend_replication::LocalControlExchangePhase::Sending
                && progress.exchange.write_offset == 0
            {
                PublicationObservationDecision::Cancel
            } else {
                PublicationObservationDecision::Continue
            }
        };
        let mut control = ObservedPublicationControl::new(
            Instant::now() + Duration::from_secs(30),
            &cancelled,
            &mut tick,
        );
        let error = transport
            .resume_publications_observed(&mut state, &mut control)
            .expect_err("observation cancels before Ack I/O");
        assert_eq!(error, PublicationExchangeError::Cancelled);
        drop(control);
        drop(tick);
        let ack_start = ack_start.expect("tick observed the first Ack byte boundary");
        assert_eq!(ack_start.request_id, 4);
        assert_eq!(
            ack_start.phase,
            backend_replication::LocalControlExchangePhase::Sending
        );
        assert_eq!(ack_start.write_offset, 0);
        assert_eq!(transport.connection(), connection);
        assert_eq!(state.cursor(), previous);
        assert_eq!(state.root().root(), initial.root());
        drop(transport);
        owner.join().expect("authenticated owner");
        std::fs::remove_file(path).expect("remove test socket");
    }

    #[test]
    fn a_closed_owner_before_reset_ack_returns_the_real_exchange_failure_without_replay() {
        let (initial, _target, previous, target_cursor, pages) = multi_page_reset_fixture();
        let lease = LocalSubscriptionId::from_bytes([14; 16]);
        let path = std::path::PathBuf::from(format!(
            "/tmp/nudox-pub-closed-ack-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path)
            .expect("bind authenticated local endpoint");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict authenticated local endpoint");
        let owner = std::thread::spawn(move || {
            let (first, _) = listener.accept().expect("accept original connection");
            drop(first);
            let (mut replacement, _) = listener.accept().expect("accept replacement connection");
            replacement
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("bounded replacement read");
            replacement
                .set_write_timeout(Some(Duration::from_secs(3)))
                .expect("bounded replacement write");
            serve_snapshot_reset(
                &mut replacement,
                previous,
                target_cursor,
                lease,
                pages,
                None,
                false,
                true,
                None,
            );
            // Both socket directions are bounded to 3s, and `close_before_ack`
            // returns after the last page without waiting for Ack. The client
            // can therefore join this terminal owner from Ack's first tick.
        });
        let mut transport = LocalSubscriptionTransport::connect_with_timeouts(
            &path,
            Duration::from_secs(1),
            Duration::from_millis(200),
        )
        .expect("connect and authenticate local producer");
        let mut state = lease_on(&transport, lease, Arc::clone(&initial), previous);
        let original_connection = transport.connection();
        transport.exhaust_connection_budget_for_test();

        let mut owner = Some(owner);
        let mut last_ack_tick = None;
        let cancelled = || false;
        let mut tick = |progress: PublicationObservationProgress| {
            if progress.exchange.request_id == 4 {
                last_ack_tick = Some(progress.exchange);
                if progress.exchange.phase
                    == backend_replication::LocalControlExchangePhase::Sending
                    && progress.exchange.write_offset == 0
                {
                    // The server has already returned from the no-Ack path.
                    // Join it before allowing the real socket write, proving
                    // the peer is closed rather than merely scheduled to close.
                    owner
                        .take()
                        .expect("owner closes exactly once before Ack")
                        .join()
                        .expect("bounded page-serving owner exited before Ack");
                }
            }
            PublicationObservationDecision::Continue
        };
        let mut control = ObservedPublicationControl::new(
            Instant::now() + Duration::from_secs(30),
            &cancelled,
            &mut tick,
        );
        let error = transport
            .resume_publications_observed(&mut state, &mut control)
            .expect_err("the closed peer cannot acknowledge the reset");
        let exchange = match error {
            PublicationExchangeError::Exchange(error) => error,
            other => panic!("closed peer must remain an exchange failure: {other:?}"),
        };
        drop(control);
        drop(tick);
        assert!(owner.is_none(), "owner was joined before Ack I/O");
        assert_eq!(
            exchange.failure,
            backend_replication::LocalControlExchangeFailure::Closed
        );
        assert_eq!(exchange.progress.request_id, 4);
        assert_eq!(exchange.progress.header_offset, 0);
        assert_eq!(exchange.progress.body_offset, 0);
        assert!(matches!(
            exchange.progress.phase,
            backend_replication::LocalControlExchangePhase::Sending
                | backend_replication::LocalControlExchangePhase::Flushing
                | backend_replication::LocalControlExchangePhase::ReadingHeader
        ));
        let last_ack_tick = last_ack_tick.expect("observation tick saw Ack progress");
        assert_eq!(exchange.progress.write_offset, last_ack_tick.write_offset);
        assert_eq!(exchange.progress.phase, last_ack_tick.phase);
        let replacement_connection = transport.connection();
        assert_ne!(replacement_connection, original_connection);
        assert_eq!(state.cleanup_connection(), replacement_connection);
        assert_eq!(state.cursor(), previous);
        assert_eq!(state.root().root(), initial.root());
        drop(transport);
        std::fs::remove_file(path).expect("remove test socket");
    }

    #[test]
    fn observed_resume_keeps_server_rejection_distinct_from_local_invalid_reply() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Resume { .. }
            ));
            reply(
                stream,
                LocalControlResponse::Rejected {
                    request_id,
                    message: "lease not retained".into(),
                },
            );
        });
        let mut state = lease_on(&transport, lease, Arc::clone(&root), cursor);
        let cancelled = || false;
        let mut tick = |_| PublicationObservationDecision::Continue;
        let mut control = ObservedPublicationControl::new(
            Instant::now() + Duration::from_secs(2),
            &cancelled,
            &mut tick,
        );
        let error = transport
            .resume_publications_observed(&mut state, &mut control)
            .expect_err("producer rejection");
        assert!(matches!(
            error,
            PublicationExchangeError::ProducerRejected {
                operation: PublicationOperation::Resume,
                message,
                ..
            } if message == "lease not retained"
        ));
        assert_eq!(state.cursor(), cursor);
        assert!(Arc::ptr_eq(&state.root(), &root));
        drop(control);
        owner.join().expect("owner");

        let page = root
            .page(ViewPageCursor::first(&root), CREDIT)
            .expect("invalid-proof fixture page");
        let page_payload =
            SnapshotPageDto::from_owner(cursor, root.descriptor(), page, CursorResetReason::Pruned)
                .expect("valid page without producer certificate");
        let next = page_payload.next_token().expect("continuation");
        let payload = serde_json::to_vec(&page_payload)
            .expect("encode valid page without producer certificate")
            .into_boxed_slice();
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Resume { .. }
            ));
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page: Box::new([]),
                    next,
                    credit: CREDIT,
                    payload,
                }),
            );
        });
        let cancelled = || false;
        let mut tick = |_| PublicationObservationDecision::Continue;
        let mut control = ObservedPublicationControl::new(
            Instant::now() + Duration::from_secs(2),
            &cancelled,
            &mut tick,
        );
        let error = transport
            .resume_publications_observed(&mut state, &mut control)
            .expect_err("invalid local proof");
        assert!(matches!(error, PublicationExchangeError::Invalid(_)));
        assert_eq!(state.cursor(), cursor);
        assert!(Arc::ptr_eq(&state.root(), &root));
        drop(control);
        owner.join().expect("owner");
    }

    #[test]
    fn observed_cancel_after_request_write_is_terminal_and_never_replays() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let (mut transport, owner) = pair(|stream| {
            let _ = request(stream);
            assert!(read_frame(stream, crate::limits()).is_err());
        });
        let cancelled = || false;
        let mut tick = |progress: PublicationObservationProgress| {
            if progress.exchange.write_offset > 0 {
                PublicationObservationDecision::Cancel
            } else {
                PublicationObservationDecision::Continue
            }
        };
        let mut control = ObservedPublicationControl::new(
            Instant::now() + Duration::from_secs(2),
            &cancelled,
            &mut tick,
        );
        let error = match transport.acquire_publications_observed(root, cursor, &mut control) {
            Err(error) => error,
            Ok(_) => panic!("cancelled incomplete request returned a lease"),
        };
        assert!(matches!(
            error,
            PublicationExchangeError::Exchange(LocalControlExchangeError {
                failure: backend_replication::LocalControlExchangeFailure::Cancelled,
                progress: LocalControlExchangeProgress {
                    write_offset,
                    ..
                },
            }) if write_offset > 0
        ));
        drop(control);
        owner.join().expect("owner");
    }

    #[test]
    fn injected_clock_expiry_during_a_partial_response_frame_is_typed() {
        use crate::monotonic::ManualClock;
        use backend_replication::{LocalControlExchangePhase, frame};
        use std::io::Read as _;

        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([38; 16]);
        let clock = ManualClock::new();
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Renew { .. }
            ));
            let response = LocalControlResponse::Subscription(LocalSubscriptionResponse::Renewed {
                request_id,
                lease,
                cursor: cursor.encode_control(),
                credit: CREDIT,
                lease_ms: LEASE_MS,
            });
            let wire = frame(
                &encode_response(&response, crate::limits()).expect("response"),
                crate::limits(),
            )
            .expect("response frame");
            stream
                .write_all(&wire[..1])
                .expect("partial response header");
            let mut byte = [0_u8; 1];
            assert_eq!(stream.read(&mut byte).expect("observe retired socket"), 0);
        });
        transport = transport.with_clock(Arc::<ManualClock>::clone(&clock));
        let mut state = lease_on(&transport, lease, root, cursor);
        let recovery_deadline = clock.now() + Duration::from_secs(1);
        let cancelled = || false;
        let mut advanced = false;
        let mut tick = |progress: PublicationObservationProgress| {
            if !advanced
                && progress.exchange.phase == LocalControlExchangePhase::ReadingHeader
                && progress.exchange.header_offset == 1
            {
                clock.advance(Duration::from_secs(1));
                advanced = true;
            }
            PublicationObservationDecision::Continue
        };
        let mut control = ObservedPublicationControl::new_with_clock(
            recovery_deadline,
            Arc::<ManualClock>::clone(&clock),
            &cancelled,
            &mut tick,
        );
        let error = transport
            .renew_publications_observed(&mut state, &mut control)
            .expect_err("the injected deadline expires during the response frame");
        assert!(matches!(
            error,
            PublicationExchangeError::BudgetExpired(budget)
                if budget.deadline() == recovery_deadline
        ));
        drop(control);
        owner.join().expect("owner");
    }

    #[test]
    fn expired_or_dropped_lease_never_advances_the_admitted_state() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        for message in ["subscription lease expired", "unknown subscription lease"] {
            let lease = LocalSubscriptionId::from_bytes([3; 16]);
            let (mut transport, owner) = pair(move |stream| {
                let (request_id, _) = request(stream);
                reply(
                    stream,
                    LocalControlResponse::Rejected {
                        request_id,
                        message: message.into(),
                    },
                );
            });
            let mut state = lease_on(&transport, lease, Arc::clone(&root), cursor);
            assert!(
                transport
                    .resume_publications(&mut state, &|| false)
                    .is_err()
            );
            assert_eq!(state.cursor(), cursor);
            assert!(Arc::ptr_eq(&state.root(), &root));
            owner.join().expect("owner");
            // Reacquisition uses the admitted cursor, never the uncertain
            // server cursor from a failed/lost reply.
            let fresh = LocalSubscriptionId::from_bytes([4; 16]);
            let (mut transport, owner) = pair(move |stream| {
                let (request_id, operation) = request(stream);
                assert!(
                    matches!(operation, LocalSubscriptionOperation::Open { cursor: sent, .. } if sent.as_ref() == cursor.encode_control().as_ref())
                );
                reply(
                    stream,
                    LocalControlResponse::Subscription(LocalSubscriptionResponse::Opened {
                        request_id,
                        lease: fresh,
                        cursor: cursor.encode_control(),
                        credit: CREDIT,
                        lease_ms: LEASE_MS,
                    }),
                );
            });
            let fresh = transport
                .acquire_publications(state.root(), state.cursor(), &|| false)
                .expect("reacquire");
            assert_eq!(fresh.cursor(), cursor);
            owner.join().expect("owner");
        }
    }

    #[test]
    fn incomplete_or_misbound_publications_cannot_replace_a_complete_root() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let wrong = LocalSubscriptionId::from_bytes([4; 16]);
        let responses = [
            LocalSubscriptionResponse::Resumed {
                request_id: 1,
                lease: wrong,
                cursor: cursor.encode_control(),
                credit: CREDIT,
                lease_ms: LEASE_MS,
            },
            LocalSubscriptionResponse::Batch {
                request_id: 1,
                lease,
                previous: Cursor::new().encode_control(),
                cursor: cursor.encode_control(),
                credit: CREDIT,
                payload: Box::new([]),
            },
            // A frame received on an unauthenticated fixture transport
            // cannot admit even a first reset page, much less partial Ready.
            LocalSubscriptionResponse::SnapshotPage {
                request_id: 1,
                lease,
                page: Box::new([]),
                next: Some(Box::new([1])),
                credit: CREDIT,
                payload: Box::new([]),
            },
        ];
        for response in responses {
            let (mut transport, owner) = pair(|_| {});
            let mut state = lease_on(&transport, lease, Arc::clone(&root), cursor);
            let hydration =
                ResetHydration::begin(cursor, transport.now()).expect("initial reset budget");
            let cancelled = || false;
            let mut observer = PublicationObserver::Legacy(&cancelled);
            assert!(
                transport
                    .admit_publications(&mut state, response, &mut observer, hydration)
                    .is_err()
            );
            assert_eq!(state.cursor(), cursor);
            assert!(Arc::ptr_eq(&state.root(), &root));
            owner.join().expect("fixture");
        }
    }

    #[test]
    fn reset_budget_allows_delayed_progress_but_never_moves_its_deadline() {
        let started = Instant::now();
        let mut budget = ResetBudget::begin(started).expect("base deadline");
        budget.admit_page(256, started).expect("descriptor");
        let fixed = budget.deadline();
        // More than the negotiated 10s in aggregate, but each page's
        // socket waits can still remain within the caller's 1s I/O limit.
        for elapsed in [5, 10, 15] {
            budget
                .admit_page(256, started + Duration::from_secs(elapsed))
                .expect("bounded delayed progress");
            assert_eq!(budget.deadline(), fixed);
        }
        assert!(budget.check(started + Duration::from_secs(18)).is_err());
        assert!(budget.admit_page(257, started).is_err());
        let mut oversized = ResetBudget::begin(started).expect("base deadline");
        assert!(oversized.admit_page(MAX_RESET_ROWS + 1, started).is_err());
        let mut excessive = ResetBudget::begin(started).expect("base deadline");
        for _ in 0..ResetPages::LIMIT.get() {
            excessive
                .admit_page(MAX_RESET_ROWS, started)
                .expect("page cap");
        }
        assert!(excessive.admit_page(MAX_RESET_ROWS, started).is_err());
    }

    #[test]
    fn terminal_cancel_uses_the_exact_existing_socket() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(
                matches!(operation, LocalSubscriptionOperation::Cancel { lease: sent } if sent == lease)
            );
            reply(
                stream,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                    request_id,
                    lease,
                }),
            );
        });
        let state = lease_on(&transport, lease, Arc::clone(&root), cursor);
        transport
            .cancel_publications_current(&state)
            .expect("terminal cancel");
        owner.join().expect("owner");
        // A socket already interrupted by cancellation fails promptly; it
        // must never dial an endpoint to perform terminal cleanup.
        let (mut transport, owner) = pair(|_| {});
        let state = lease_on(&transport, lease, root, cursor);
        transport.interrupt_handle().expect("socket").interrupt();
        assert!(transport.cancel_publications_current(&state).is_err());
        owner.join().expect("owner");
    }

    #[test]
    fn terminal_cancel_releases_the_transport_even_when_the_owner_does_not_ack() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([31; 16]);
        let (mut transport, owner) = pair(move |stream| {
            let (request_id, operation) = request(stream);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Cancel { lease: sent } if sent == lease
            ));
            let _ = request_id;
            // The owner closes without an acknowledgement.
        });
        let state = lease_on(&transport, lease, root, cursor);
        assert!(transport.cancel_publications_current(&state).is_err());
        assert!(matches!(
            transport.renew_lease(lease, cursor, CREDIT, LEASE_MS),
            Err(ClientError::Io(message))
                if message.contains("released by a terminal lease cancellation")
        ));
        owner.join().expect("owner");
    }

    #[test]
    fn a_lease_from_another_socket_is_never_cancelled_here() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([32; 16]);
        let (mut here, here_owner) = pair(|stream| {
            assert!(read_frame(stream, crate::limits()).is_err());
        });
        let (elsewhere, elsewhere_owner) = pair(|_| {});
        let state = lease_on(&elsewhere, lease, root, cursor);
        assert_eq!(
            here.cancel_publications_current(&state),
            Err(protocol("publication lease is not held on this socket"))
        );
        drop(here);
        here_owner.join().expect("no cancel crossed this socket");
        drop(elsewhere);
        elsewhere_owner.join().expect("no work on the lease socket");
    }

    #[test]
    fn a_fully_correlated_renewal_updates_cleanup_to_the_replacement_socket() {
        let path = std::path::PathBuf::from(format!(
            "/tmp/nudox-pub-rotate-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener =
            std::os::unix::net::UnixListener::bind(&path).expect("bind reused endpoint path");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict reused endpoint path");
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([33; 16]);
        let server = std::thread::spawn(move || {
            let (_first, _) = listener.accept().expect("first endpoint connection");
            let (mut replacement, _) = listener.accept().expect("replacement connection");
            replacement
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("bounded replacement read");
            let (request_id, operation) = request(&mut replacement);
            let LocalSubscriptionOperation::Renew {
                lease: renewed,
                cursor: renewed_cursor,
                ..
            } = operation
            else {
                panic!("rotation must carry the explicit renew request");
            };
            assert_eq!(renewed, lease);
            assert_eq!(renewed_cursor.as_ref(), cursor.encode_control().as_ref());
            reply(
                &mut replacement,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Renewed {
                    request_id,
                    lease,
                    cursor: cursor.encode_control(),
                    credit: CREDIT,
                    lease_ms: LEASE_MS,
                }),
            );
            let (cancel_id, operation) = request(&mut replacement);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Cancel { lease: cancelled } if cancelled == lease
            ));
            reply(
                &mut replacement,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                    request_id: cancel_id,
                    lease,
                }),
            );
            assert!(read_frame(&mut replacement, crate::limits()).is_err());
        });
        let mut transport = LocalSubscriptionTransport::connect_with_timeouts(
            &path,
            Duration::from_secs(1),
            Duration::from_millis(200),
        )
        .expect("connect to endpoint");
        let mut state = lease_on(&transport, lease, root, cursor);
        let original = state.cleanup_connection();
        transport.exhaust_connection_budget_for_test();
        transport
            .renew_publications(&mut state)
            .expect("the new socket can serve an ordinary renewal");
        assert_ne!(transport.connection(), original);
        assert_eq!(state.cleanup_connection(), transport.connection());
        transport
            .cancel_publications_current(&state)
            .expect("the correlated replacement response binds cleanup to its socket");
        drop(transport);
        server.join().expect("replacement owner");
        std::fs::remove_file(path).expect("remove endpoint path");
    }

    #[test]
    fn a_rotated_authenticated_reset_commits_only_after_ack_and_cleans_up_on_that_socket() {
        let (initial, target, previous, target_cursor, pages) = multi_page_reset_fixture();
        let lease = LocalSubscriptionId::from_bytes([34; 16]);
        let path = std::path::PathBuf::from(format!(
            "/tmp/nudox-pub-rotate-reset-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener =
            std::os::unix::net::UnixListener::bind(&path).expect("bind reused endpoint path");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict reused endpoint path");
        let (acknowledged_tx, acknowledged_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (first, _) = listener.accept().expect("first endpoint connection");
            drop(first);
            let (mut replacement, _) = listener.accept().expect("replacement connection");
            replacement
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("bounded replacement read");
            replacement
                .set_write_timeout(Some(Duration::from_secs(3)))
                .expect("bounded replacement write");
            serve_snapshot_reset(
                &mut replacement,
                previous,
                target_cursor,
                lease,
                pages,
                None,
                false,
                false,
                Some(acknowledged_tx),
            );
            let (cancel_id, operation) = request(&mut replacement);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Cancel { lease: cancelled } if cancelled == lease
            ));
            reply(
                &mut replacement,
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                    request_id: cancel_id,
                    lease,
                }),
            );
            assert!(matches!(
                read_frame(&mut replacement, crate::limits()),
                Err(backend_replication::LocalControlError::Closed)
            ));
        });
        let mut transport = LocalSubscriptionTransport::connect_with_timeouts(
            &path,
            Duration::from_secs(1),
            Duration::from_millis(200),
        )
        .expect("connect and authenticate original endpoint");
        let mut state = lease_on(&transport, lease, Arc::clone(&initial), previous);
        let original_connection = transport.connection();
        transport.exhaust_connection_budget_for_test();

        let cancelled = || false;
        let mut progress = Vec::new();
        let mut tick = |step: PublicationObservationProgress| {
            progress.push(step);
            PublicationObservationDecision::Continue
        };
        let mut control = ObservedPublicationControl::new(
            Instant::now() + Duration::from_secs(30),
            &cancelled,
            &mut tick,
        );
        transport
            .resume_publications_observed(&mut state, &mut control)
            .expect("replacement socket admits the complete reset after exact Ack");
        drop(control);
        drop(tick);

        let replacement_connection = transport.connection();
        assert_ne!(replacement_connection, original_connection);
        assert_eq!(state.cleanup_connection(), replacement_connection);
        assert_eq!(state.cursor(), target_cursor);
        assert_eq!(state.root().root(), target.root());
        assert_ne!(state.root().root(), initial.root());
        assert_eq!(
            acknowledged_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("server reported its correlated Ack"),
            4,
            "Resume, two Page requests, and Ack have consecutive request IDs"
        );
        assert!(progress.iter().any(|step| {
            step.exchange.request_id == 4
                && step.budget.kind()
                    == PublicationBudgetKind::AuthenticatedReset {
                        rows: 130,
                        pages: 3,
                    }
        }));

        transport
            .cancel_publications_current(&state)
            .expect("cleanup is sent on the latest exact replacement socket");
        drop(transport);
        server.join().expect("replacement owner receives cleanup");
        std::fs::remove_file(path).expect("remove endpoint path");
    }

    #[test]
    fn cancellation_interrupts_the_exact_pending_lease_socket() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let (begun, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (mut transport, owner) = pair(move |stream| {
            let _ = request(stream);
            begun.send(()).expect("pending");
            released
                .recv_timeout(Duration::from_secs(2))
                .expect("release fixture");
        });
        let interrupt = transport.interrupt_handle().expect("exact socket");
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancelled);
        let worker = std::thread::spawn(move || {
            transport.acquire_publications(root, cursor, &|| worker_cancel.load(Ordering::Acquire))
        });
        received
            .recv_timeout(Duration::from_secs(2))
            .expect("read pending");
        let started = Instant::now();
        cancelled.store(true, Ordering::Release);
        interrupt.interrupt();
        assert!(worker.join().expect("worker").is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        release.send(()).expect("release");
        owner.join().expect("owner");
    }
}
