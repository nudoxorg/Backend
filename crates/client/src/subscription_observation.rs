//! Proof-admitted state of one durable local publication lease. Socket work
//! belongs to the caller's worker; cancellation is checked between bounded
//! frames. No partial reset can replace the retained complete root.

use crate::subscription::{
    snapshot_page_from_bytes_with_verifier, subscription_read_from_bytes_against,
};
use crate::{ClientError, LocalSubscriptionExchangeError, LocalSubscriptionTransport};
use backend_library::{Cursor, CursorEvent, CursorRead, SnapshotHydrator, ViewRoot};
use backend_replication::{
    LocalControlExchangeDecision, LocalControlExchangeError, LocalControlExchangeProgress,
    LocalSubscriptionId, LocalSubscriptionOperation, LocalSubscriptionResponse,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CREDIT: usize = 64;
const LEASE_MS: u64 = 10_000;
const MAX_RESET_ROWS: u64 = 131_072;
const RESET_TIME: Duration = Duration::from_secs(10);
const PAGE_TIME: Duration = Duration::from_secs(2);
const MAX_RESET_PAGES: u64 = MAX_RESET_ROWS / CREDIT as u64;

/// Which fixed budget currently governs one observation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationBudgetKind {
    /// Caller recovery deadline, or the initial ordinary admission window.
    Ordinary,
    /// Authenticated descriptor fixed the complete reset's larger allowance.
    AuthenticatedReset { rows: u64, pages: u64 },
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

/// Caller-owned controls for one publication acquisition/resume/renew.
///
/// The callback receives a copyable progress snapshot, so it can inspect
/// current state without trying to borrow the transport while it is mutably
/// driving a frame. Its work must remain short and nonblocking.
pub struct ObservedPublicationControl<'a> {
    recovery_deadline: Instant,
    cancelled: &'a dyn Fn() -> bool,
    tick: &'a mut dyn FnMut(PublicationObservationProgress) -> PublicationObservationDecision,
    budget: PublicationExchangeBudget,
}

impl<'a> ObservedPublicationControl<'a> {
    /// Creates a control using the caller's absolute ordinary recovery deadline.
    #[must_use]
    pub fn new(
        recovery_deadline: Instant,
        cancelled: &'a dyn Fn() -> bool,
        tick: &'a mut dyn FnMut(PublicationObservationProgress) -> PublicationObservationDecision,
    ) -> Self {
        let started = Instant::now();
        Self {
            recovery_deadline,
            cancelled,
            tick,
            budget: PublicationExchangeBudget {
                started,
                deadline: recovery_deadline,
                kind: PublicationBudgetKind::Ordinary,
            },
        }
    }

    /// Current fixed budget snapshot, useful to schedule caller-side retries.
    #[must_use]
    pub const fn budget(&self) -> PublicationExchangeBudget {
        self.budget
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

/// The first authenticated descriptor fixes the whole reset's allowance.
/// Progress never moves this deadline; socket I/O limits remain independent.
struct PublicationBudget {
    started: Instant,
    deadline: Instant,
    rows: Option<u64>,
    pages: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationBudgetFailure {
    RowsExceeded,
    RowsChanged,
    PageLimitExceeded,
    Expired,
}

impl PublicationBudget {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            deadline: now + RESET_TIME,
            rows: None,
            pages: 0,
        }
    }
    fn page(&mut self, rows: u64, now: Instant) -> Result<(), PublicationBudgetFailure> {
        if rows > MAX_RESET_ROWS {
            return Err(PublicationBudgetFailure::RowsExceeded);
        }
        if let Some(expected) = self.rows {
            if expected != rows {
                return Err(PublicationBudgetFailure::RowsChanged);
            }
        } else {
            let pages = rows.div_ceil(CREDIT as u64).max(1);
            self.deadline = self.started + RESET_TIME + PAGE_TIME * pages as u32;
            self.rows = Some(rows);
        }
        self.pages += 1;
        if self.pages > MAX_RESET_PAGES {
            return Err(PublicationBudgetFailure::PageLimitExceeded);
        }
        self.check(now)
    }
    fn check(&self, now: Instant) -> Result<(), PublicationBudgetFailure> {
        if now >= self.deadline {
            Err(PublicationBudgetFailure::Expired)
        } else {
            Ok(())
        }
    }
}

enum PublicationObserver<'control, 'callback> {
    Legacy(&'callback dyn Fn() -> bool),
    Observed(&'control mut ObservedPublicationControl<'callback>),
}

impl PublicationObserver<'_, '_> {
    fn check(&self) -> Result<(), PublicationExchangeError> {
        match self {
            Self::Legacy(cancelled) if cancelled() => Err(PublicationExchangeError::Invalid(
                protocol("publication observation withdrawn"),
            )),
            Self::Legacy(_) => Ok(()),
            Self::Observed(control) if (control.cancelled)() => {
                Err(PublicationExchangeError::Cancelled)
            }
            Self::Observed(control) if Instant::now() >= control.budget.deadline => {
                Err(PublicationExchangeError::BudgetExpired(control.budget))
            }
            Self::Observed(_) => Ok(()),
        }
    }

    fn check_budget(&self, budget: &PublicationBudget) -> Result<(), PublicationExchangeError> {
        self.check()?;
        budget
            .check(Instant::now())
            .map_err(|failure| self.budget_error(budget, failure))
    }

    fn begin_admission(&mut self, budget: &PublicationBudget) {
        if let Self::Observed(control) = self {
            control.budget = PublicationExchangeBudget {
                started: budget.started,
                deadline: control.recovery_deadline.min(budget.started + RESET_TIME),
                kind: PublicationBudgetKind::Ordinary,
            };
        }
    }

    fn bind_reset(&mut self, budget: &PublicationBudget) {
        if let Self::Observed(control) = self
            && let Some(rows) = budget.rows
        {
            control.budget = PublicationExchangeBudget {
                started: budget.started,
                deadline: budget.deadline,
                kind: PublicationBudgetKind::AuthenticatedReset {
                    rows,
                    pages: rows.div_ceil(CREDIT as u64).max(1),
                },
            };
        }
    }

    fn budget_error(
        &self,
        _budget: &PublicationBudget,
        failure: PublicationBudgetFailure,
    ) -> PublicationExchangeError {
        if failure == PublicationBudgetFailure::Expired {
            if let Self::Observed(control) = self {
                return PublicationExchangeError::BudgetExpired(control.budget);
            }
            return PublicationExchangeError::Invalid(protocol(
                "publication reset exceeded its time budget",
            ));
        }
        let message = match failure {
            PublicationBudgetFailure::RowsExceeded => "publication reset exceeds its row budget",
            PublicationBudgetFailure::RowsChanged => "publication reset changed its row budget",
            PublicationBudgetFailure::PageLimitExceeded => {
                "publication reset exceeds its page budget"
            }
            PublicationBudgetFailure::Expired => "publication reset exceeded its time budget",
        };
        PublicationExchangeError::Invalid(protocol(message))
    }

    fn request(
        &mut self,
        transport: &mut LocalSubscriptionTransport,
        operation: LocalSubscriptionOperation,
        tag: PublicationOperation,
    ) -> Result<LocalSubscriptionResponse, PublicationExchangeError> {
        self.check()?;
        match self {
            Self::Legacy(_) => transport
                .lease_request(operation)
                .map_err(PublicationExchangeError::Invalid),
            Self::Observed(control) => {
                let budget = control.budget;
                let cancelled = control.cancelled;
                let tick = &mut control.tick;
                transport
                    .lease_request_with_tick(operation, budget.deadline, |exchange| {
                        if cancelled() {
                            return LocalControlExchangeDecision::Cancel;
                        }
                        match tick(PublicationObservationProgress { exchange, budget }) {
                            PublicationObservationDecision::Continue if !cancelled() => {
                                LocalControlExchangeDecision::Continue
                            }
                            PublicationObservationDecision::Continue
                            | PublicationObservationDecision::Cancel => {
                                LocalControlExchangeDecision::Cancel
                            }
                        }
                    })
                    .map_err(|error| match error {
                        LocalSubscriptionExchangeError::Setup(error) => {
                            PublicationExchangeError::Setup(error)
                        }
                        LocalSubscriptionExchangeError::Rejected {
                            request_id,
                            message,
                        } => PublicationExchangeError::ProducerRejected {
                            operation: tag,
                            request_id,
                            message,
                        },
                        LocalSubscriptionExchangeError::Invalid(error) => {
                            PublicationExchangeError::Invalid(error)
                        }
                        LocalSubscriptionExchangeError::Exchange(error) => {
                            PublicationExchangeError::Exchange(error)
                        }
                    })
            }
        }
    }
}

/// Exact producer state retained across socket reconnects. The lease is an
/// owner-issued identity, never reconstructed from a path or a clock.
pub struct PublicationLease {
    lease: LocalSubscriptionId,
    cursor: Cursor,
    root: Arc<ViewRoot>,
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
        )?;
        let lease = response.lease();
        let mut state = PublicationLease {
            lease,
            cursor,
            root,
        };
        if let Err(error) = self.admit_publications(&mut state, response, &mut observer) {
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
        )?;
        self.admit_publications(state, response, &mut observer)
    }

    /// Fences a quiet lease to its exact admitted cursor before an idle pause.
    /// # Errors
    /// Returns an error if the owner no longer retains this exact lease.
    pub fn renew_publications(&mut self, state: &PublicationLease) -> Result<(), ClientError> {
        match self.renew_lease(state.lease, state.cursor, CREDIT, LEASE_MS)? {
            LocalSubscriptionResponse::Renewed { cursor, .. }
                if cursor.as_ref() == state.cursor.encode_control().as_ref() =>
            {
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
        state: &PublicationLease,
        control: &mut ObservedPublicationControl<'_>,
    ) -> Result<(), PublicationExchangeError> {
        let mut observer = PublicationObserver::Observed(control);
        let response = observer.request(
            self,
            LocalSubscriptionOperation::Renew {
                lease: state.lease,
                cursor: state.cursor.encode_control().into_vec().into_boxed_slice(),
                credit: CREDIT,
                lease_ms: LEASE_MS,
            },
            PublicationOperation::Renew,
        )?;
        match response {
            LocalSubscriptionResponse::Renewed { cursor, .. }
                if cursor.as_ref() == state.cursor.encode_control().as_ref() => {}
            _ => {
                return Err(PublicationExchangeError::Invalid(protocol(
                    "publication renewal did not fence the admitted cursor",
                )));
            }
        }
        observer.check()?;
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
        self.cancel_lease_current(state.lease, Duration::from_millis(50))
    }

    fn admit_publications(
        &mut self,
        state: &mut PublicationLease,
        mut response: LocalSubscriptionResponse,
        observer: &mut PublicationObserver<'_, '_>,
    ) -> Result<(), PublicationExchangeError> {
        let mut budget = PublicationBudget::new(Instant::now());
        observer.begin_admission(&budget);
        let previous = state.cursor;
        let mut hydrator: Option<SnapshotHydrator> = None;
        let mut expected_page: Box<[u8]> = Box::new([]);
        let (root, cursor) = loop {
            observer.check_budget(&budget)?;
            if response.lease() != state.lease {
                return Err(invalid("publication response changed lease identity"));
            }
            match response {
                LocalSubscriptionResponse::Opened { cursor, .. }
                | LocalSubscriptionResponse::Resumed { cursor, .. } => {
                    if hydrator.is_some() || cursor.as_ref() != previous.encode_control().as_ref() {
                        return Err(invalid("publication acknowledgement changed its cursor"));
                    }
                    break (Arc::clone(&state.root), previous);
                }
                LocalSubscriptionResponse::Batch {
                    previous: predecessor,
                    cursor: target,
                    payload,
                    ..
                } => {
                    if hydrator.is_some()
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
                    if page != expected_page {
                        return Err(invalid("publication reset page mismatch"));
                    }
                    let peer = self.authenticated_peer().ok_or_else(|| {
                        protocol("publication reset requires authenticated producer")
                    })?;
                    let claim =
                        snapshot_page_from_bytes_with_verifier(&payload, previous, None, peer)?;
                    let first_descriptor = budget.rows.is_none();
                    let page_result = budget.page(claim.descriptor().row_count(), Instant::now());
                    if first_descriptor {
                        observer.bind_reset(&budget);
                    }
                    page_result.map_err(|failure| observer.budget_error(&budget, failure))?;
                    let admitted_next = claim.next_token().map_err(ClientError::Protocol)?;
                    if next.as_deref() == Some(page.as_ref()) {
                        return Err(invalid("publication reset repeated its continuation"));
                    }
                    if admitted_next.as_deref() != next.as_deref() {
                        return Err(invalid(
                            "publication reset continuation is not authenticated",
                        ));
                    }
                    let current = match hydrator.take() {
                        Some(mut current) => {
                            current.push_page(claim).map_err(ClientError::Protocol)?;
                            current
                        }
                        None => SnapshotHydrator::start(previous, claim)
                            .map_err(ClientError::Protocol)?,
                    };
                    if current.is_complete() {
                        let CursorRead::Reset { cursor, root, .. } =
                            current.finish().map_err(ClientError::Protocol)?
                        else {
                            return Err(invalid("publication hydration did not reset"));
                        };
                        break (Arc::from(root), cursor);
                    }
                    expected_page =
                        next.ok_or_else(|| protocol("publication reset omitted continuation"))?;
                    hydrator = Some(current);
                    observer.check()?;
                    response = observer.request(
                        self,
                        LocalSubscriptionOperation::Page {
                            lease: state.lease,
                            page: expected_page.clone(),
                            credit: CREDIT,
                        },
                        PublicationOperation::Page,
                    )?;
                }
                _ => return Err(invalid("unexpected publication lifecycle response")),
            }
        };
        observer.check_budget(&budget)?;
        // Quiet Resume has already fenced this exact durable cursor; it
        // does not need another frame or a duplicate root publication.
        if cursor == previous {
            return Ok(());
        }
        match observer.request(
            self,
            LocalSubscriptionOperation::Ack {
                lease: state.lease,
                cursor: cursor.encode_control().into_vec().into_boxed_slice(),
            },
            PublicationOperation::Ack,
        )? {
            LocalSubscriptionResponse::Acked {
                cursor: admitted, ..
            } if admitted.as_ref() == cursor.encode_control().as_ref() => {}
            _ => return Err(invalid("publication acknowledgement cursor mismatch")),
        }
        observer.check_budget(&budget)?;
        state.cursor = cursor;
        state.root = root;
        Ok(())
    }
}

fn protocol(message: &str) -> ClientError {
    ClientError::Protocol(message.to_owned())
}
fn invalid(message: &str) -> PublicationExchangeError {
    PublicationExchangeError::Invalid(protocol(message))
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use backend_library::{
        AuthorityScopeClaim, Basis, CoverageCapability, Frontier, ProducerObservationClaims,
        ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation, admit_complete_scope,
        admit_producer_observation, object_version, view_key, view_state_root,
    };
    use backend_replication::{
        LocalControlRequest, LocalControlResponse, LocalSubscriptionOperation, decode_request,
        encode_response, frame, read_frame, write_frame,
    };
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    use std::thread::JoinHandle;

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
        transport.renew_publications(&state).expect("renew");
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
        let state = transport
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
            .renew_publications_observed(&state, &mut control)
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
    fn observed_resume_keeps_server_rejection_distinct_from_local_invalid_reply() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let mut state = PublicationLease {
            lease,
            cursor,
            root: Arc::clone(&root),
        };
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
                    next: Some(Box::new([1])),
                    credit: CREDIT,
                    payload: Box::new([]),
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
        let error = transport
            .acquire_publications_observed(root, cursor, &mut control)
            .expect_err("cancelled incomplete request");
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
    fn expired_or_dropped_lease_never_advances_the_admitted_state() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        for message in ["subscription lease expired", "unknown subscription lease"] {
            let lease = LocalSubscriptionId::from_bytes([3; 16]);
            let mut state = PublicationLease {
                lease,
                root: Arc::clone(&root),
                cursor,
            };
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
            let mut state = PublicationLease {
                lease,
                cursor,
                root: Arc::clone(&root),
            };
            let (mut transport, owner) = pair(|_| {});
            let cancelled = || false;
            let mut observer = PublicationObserver::Legacy(&cancelled);
            assert!(
                transport
                    .admit_publications(&mut state, response, &mut observer)
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
        let mut budget = PublicationBudget::new(started);
        budget.page(256, started).expect("descriptor");
        let fixed = budget.deadline;
        // More than the negotiated 10s in aggregate, but each page's
        // socket waits can still remain within the caller's 1s I/O limit.
        for elapsed in [5, 10, 15] {
            budget
                .page(256, started + Duration::from_secs(elapsed))
                .expect("bounded delayed progress");
            assert_eq!(budget.deadline, fixed);
        }
        assert!(budget.check(started + Duration::from_secs(18)).is_err());
        assert!(budget.page(257, started).is_err());
        let mut oversized = PublicationBudget::new(started);
        assert!(oversized.page(MAX_RESET_ROWS + 1, started).is_err());
        let mut excessive = PublicationBudget::new(started);
        for _ in 0..MAX_RESET_PAGES {
            excessive.page(MAX_RESET_ROWS, started).expect("page cap");
        }
        assert!(excessive.page(MAX_RESET_ROWS, started).is_err());
    }

    #[test]
    fn terminal_cancel_uses_the_exact_existing_socket() {
        let root = root();
        let cursor = Cursor::for_view_root_at(&root, 0);
        let lease = LocalSubscriptionId::from_bytes([3; 16]);
        let state = PublicationLease {
            lease,
            cursor,
            root,
        };
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
        transport
            .cancel_publications_current(&state)
            .expect("terminal cancel");
        owner.join().expect("owner");
        // A socket already interrupted by cancellation fails promptly; it
        // must never dial an endpoint to perform terminal cleanup.
        let (mut transport, owner) = pair(|_| {});
        transport.interrupt_handle().expect("socket").interrupt();
        assert!(transport.cancel_publications_current(&state).is_err());
        owner.join().expect("owner");
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
