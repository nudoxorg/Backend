//! Owner-loop service adapters for locald.
//!
//! The service deliberately does not own a workspace. An [`OwnerService`]
//! implementation is the only place allowed to turn a command or engine
//! operation into state. `LocaldService` serializes all calls through one
//! owner loop and is therefore safe to put behind a Unix listener without
//! accidentally creating a second head writer per connection.

use crate::protocol::{CompletionClaim, EngineRequest, EngineStatus, ProtocolError};
use backend_engine::{
    Cursor, CursorEvent, DaemonReply, LocalSubscriptionId, LocalSubscriptionOperation,
    LocalSubscriptionRequest, LocalSubscriptionResponse, SubscriptionReply, ViewPageCursor,
    ViewSnapshotPage,
};
use std::collections::BTreeMap;
use std::fmt;
#[cfg(unix)]
use std::io::Read;
use std::num::NonZeroU16;
use std::time::{Duration, Instant};

#[path = "service/runtime.rs"]
mod runtime;
#[path = "service/transport.rs"]
mod transport;

pub use transport::{CommandOutcome, Handled, LocaldService, OwnerService};

/// Commands an owner can take now and answer later: an index job hands its
/// compile off the owner loop, and the loop answers reads from the last
/// publication meanwhile. Installed with [`LocaldOwner::with_deferred_commands`].
pub trait DeferredCommands<M, V, A>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    /// Handles one command body now, or takes it under `ticket`.
    ///
    /// # Errors
    ///
    /// The owner's refusal, in its words.
    fn command(
        &mut self,
        daemon: &mut crate::Locald<M, V, A>,
        body: &[u8],
        ticket: u64,
    ) -> Result<CommandOutcome, String>;

    /// Finishes whatever deferred work is ready, on the owner loop, and
    /// returns the reply bodies by ticket.
    fn poll(&mut self, daemon: &mut crate::Locald<M, V, A>) -> Vec<(u64, Result<Vec<u8>, String>)>;

    /// Cancels and joins any process-local deferred work before the owner
    /// releases its daemon state. Implementations without workers need not
    /// override this hook.
    fn close(&mut self) {}
}
#[path = "service/subscription.rs"]
mod subscription;

use subscription::decode_cursor_for_service;

pub use runtime::ServiceError;
pub(crate) use runtime::{
    RequestCorrelation, daemon_replicate, error_payload, map_queue_error, wait_for_daemon_reply,
};

const MAX_CONFIGURED_SUBSCRIPTION_LEASES: usize = 1024;
const MAX_CONFIGURED_SUBSCRIPTION_LEASE_DURATION: Duration = Duration::from_secs(60 * 60);
const MAX_CONFIGURED_RESET_PAGES: usize = 4096;
const MAX_CONFIGURED_RESET_DURATION: Duration = Duration::from_secs(4 * 60 * 60);
const MAX_CONSECUTIVE_REMOTE_PROGRESS_TICKS: u8 = 4;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum OwnerLaneTurn {
    #[default]
    Remote,
    Engine,
}

/// Bounds how many productive remote polls may run before one engine-lane
/// turn. A remote poll is still synchronous and cannot be preempted once it
/// starts; this only prevents a continuously productive remote inbox from
/// winning every successive owner-loop turn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct OwnerLaneSchedule {
    consecutive_remote_progress: u8,
}

impl OwnerLaneSchedule {
    const fn next_turn(self) -> OwnerLaneTurn {
        if self.consecutive_remote_progress >= MAX_CONSECUTIVE_REMOTE_PROGRESS_TICKS {
            OwnerLaneTurn::Engine
        } else {
            OwnerLaneTurn::Remote
        }
    }

    fn remote_polled(&mut self, progressed: bool) {
        self.consecutive_remote_progress = if progressed {
            self.consecutive_remote_progress
                .saturating_add(1)
                .min(MAX_CONSECUTIVE_REMOTE_PROGRESS_TICKS)
        } else {
            0
        };
    }

    fn engine_polled(&mut self) {
        self.consecutive_remote_progress = 0;
    }
}

/// Owner-local bounds on retained subscription roots and lease lifetimes.
/// Successful exact snapshot-page progress may renew one lease for its
/// negotiated duration; an abandoned page stops renewing and expires.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionLeaseLimits {
    max_active: usize,
    max_duration: Duration,
    max_reset_pages: usize,
    max_reset_duration: Duration,
}

impl Default for SubscriptionLeaseLimits {
    fn default() -> Self {
        Self {
            max_active: 256,
            max_duration: Duration::from_secs(5 * 60),
            // The observer's 131_072-row, credit-64 reset takes at most 2048
            // pages. Its fixed 10s + 2s/page logical budget fits within this
            // producer-side ceiling; this is a budget, not a timing claim.
            max_reset_pages: 2048,
            max_reset_duration: Duration::from_secs(90 * 60),
        }
    }
}

impl SubscriptionLeaseLimits {
    /// Sets finite owner retention bounds within the protocol's hard ceiling.
    ///
    /// # Errors
    /// Returns [`ProtocolError::InvalidLimits`] for zero or excessive bounds.
    pub fn new(
        max_active: usize,
        max_duration: Duration,
        max_reset_pages: usize,
        max_reset_duration: Duration,
    ) -> Result<Self, ProtocolError> {
        if max_active == 0
            || max_active > MAX_CONFIGURED_SUBSCRIPTION_LEASES
            || max_duration.is_zero()
            || max_duration > MAX_CONFIGURED_SUBSCRIPTION_LEASE_DURATION
            || max_reset_pages == 0
            || max_reset_pages > MAX_CONFIGURED_RESET_PAGES
            || max_reset_duration.is_zero()
            || max_reset_duration > MAX_CONFIGURED_RESET_DURATION
        {
            return Err(ProtocolError::InvalidLimits);
        }
        Ok(Self {
            max_active,
            max_duration,
            max_reset_pages,
            max_reset_duration,
        })
    }

    fn duration(self, lease_ms: u64) -> Result<Duration, ProtocolError> {
        let duration = Duration::from_millis(lease_ms);
        if duration.is_zero() || duration > self.max_duration {
            return Err(ProtocolError::InvalidControl(
                "subscription lease duration bounds",
            ));
        }
        Ok(duration)
    }
}

/// A typed adapter that wires the existing [`crate::Locald`] owner into the
/// service. The command closure is supplied by the engine composition root so
/// this application crate never creates a parallel library state owner.
pub struct LocaldOwner<
    M: backend_engine::WorkspaceModel,
    V = backend_engine::UnconfiguredOutputValidator,
    A = backend_engine::UnconfiguredAuthorityVerifier,
    F = (),
    C = NoCompletionAdmission,
    R = NoReplicationAdmission,
    S = NoSemanticRangeAdmission,
> where
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    daemon: crate::Locald<M, V, A>,
    command: F,
    completion: C,
    replication: R,
    semantic_ranges: S,
    owner_lane_schedule: OwnerLaneSchedule,
    leases: BTreeMap<LocalSubscriptionId, DurableLease>,
    next_lease_expiry: Option<Instant>,
    lease_limits: SubscriptionLeaseLimits,
    lease_identity: OwnerLeaseIdentity,
    deferred: Option<Box<dyn DeferredCommands<M, V, A> + Send>>,
}

/// Owner-retained state for one leased subscription.
///
/// The lease keeps only the exact cursor and, during reset hydration, a
/// shallow persistent-root handle plus one page continuation.  It never
/// retains a materialized copy of the complete visible relation.
#[derive(Debug)]
struct DurableLease {
    cursor: Box<[u8]>,
    credit: usize,
    duration: Duration,
    expires_at: Instant,
    snapshot: Option<DurableSnapshot>,
}

#[derive(Debug)]
struct DurableSnapshot {
    root: Box<backend_engine::ViewRoot>,
    cursor: Cursor,
    reason: backend_engine::CursorResetReason,
    next: SnapshotContinuation,
    /// Absolute hydration limit, independent of per-page lease renewal.
    expires_at: Instant,
    /// Initial page counts against the total reset-page budget.
    pages_remaining: NonZeroU16,
}

/// A live reset always has both an admitted cursor and its exact opaque token.
#[derive(Clone, Debug)]
struct SnapshotContinuation {
    cursor: ViewPageCursor,
    token: Box<[u8]>,
}

/// One process-local lease namespace. It is minted from the OS before the
/// first Open, never serialized, and discarded when this owner stops. A peer
/// reconnecting to the same endpoint path receives a new random namespace;
/// reusing an old 128-bit lease ID would require a cryptographic collision.
struct OwnerBootNonce([u8; 32]);

impl OwnerBootNonce {
    fn fresh() -> std::io::Result<Self> {
        let mut bytes = [0_u8; 32];
        #[cfg(unix)]
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        #[cfg(windows)]
        backend_platform::win32::random::fill(&mut bytes)?;
        #[cfg(not(any(unix, windows)))]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "owner lease entropy is unavailable on this platform",
        ));
        Ok(Self(bytes))
    }

    fn lease_id(&self, nonce: u64, request_id: u64, cursor: &[u8]) -> Option<LocalSubscriptionId> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend-locald-subscription-lease.v2\0");
        hasher.update(&self.0);
        hasher.update(&nonce.to_be_bytes());
        hasher.update(&request_id.to_be_bytes());
        hasher.update(cursor);
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        (bytes != [0_u8; 16]).then_some(LocalSubscriptionId::from_bytes(bytes))
    }
}

impl Drop for OwnerBootNonce {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Default)]
struct OwnerLeaseIdentity {
    boot_nonce: Option<OwnerBootNonce>,
    next_nonce: u64,
}

impl OwnerLeaseIdentity {
    fn allocate(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        active: &BTreeMap<LocalSubscriptionId, DurableLease>,
    ) -> Result<LocalSubscriptionId, ProtocolError> {
        if self.boot_nonce.is_none() {
            self.boot_nonce =
                Some(OwnerBootNonce::fresh().map_err(|_| ProtocolError::LeaseEntropyUnavailable)?);
        }
        let boot_nonce = self
            .boot_nonce
            .as_ref()
            .ok_or(ProtocolError::LeaseEntropyUnavailable)?;
        loop {
            self.next_nonce = self
                .next_nonce
                .checked_add(1)
                .ok_or(ProtocolError::LeaseIdsExhausted)?;
            if let Some(lease) = boot_nonce.lease_id(self.next_nonce, request_id, cursor)
                && !active.contains_key(&lease)
            {
                return Ok(lease);
            }
        }
    }
}

fn retained_lease(
    active: &BTreeMap<LocalSubscriptionId, DurableLease>,
    lease: LocalSubscriptionId,
) -> Result<&DurableLease, ProtocolError> {
    active
        .get(&lease)
        .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))
}

fn pending_page_cursor(
    state: &DurableLease,
    page_token: &[u8],
) -> Result<ViewPageCursor, ProtocolError> {
    let snapshot = state
        .snapshot
        .as_ref()
        .ok_or(ProtocolError::InvalidControl(
            "subscription has no pending snapshot",
        ))?;
    if snapshot.next.token.as_ref() != page_token {
        return Err(ProtocolError::InvalidControl(
            "snapshot page continuation mismatch or replay",
        ));
    }
    Ok(snapshot.next.cursor)
}

fn page_advances(page: &ViewSnapshotPage, requested: ViewPageCursor) -> bool {
    page.cursor() == requested && !page.rows().is_empty() && page.next() != Some(requested)
}

#[derive(Debug)]
struct PreparedSnapshotPage {
    cursor: Cursor,
    continuation: Option<SnapshotContinuation>,
    pages_remaining: Option<NonZeroU16>,
    payload: Box<[u8]>,
    expires_at: Instant,
    reset_expires_at: Instant,
}

/// Builds a reset page without mutating its retained lease. The snapshot is
/// committed only after this function has validated the exact continuation,
/// proved forward progress, encoded the page, and checked deadlines again.
fn prepare_snapshot_page<F, C>(
    lease: &DurableLease,
    page_token: &[u8],
    credit: usize,
    mut now: C,
    encode: F,
) -> Result<PreparedSnapshotPage, ProtocolError>
where
    F: FnOnce(&backend_engine::SnapshotPageDto) -> Result<Box<[u8]>, ProtocolError>,
    C: FnMut() -> Instant,
{
    if credit == 0 || credit > backend_engine::MAX_SNAPSHOT_PAGE_ROWS {
        return Err(ProtocolError::InvalidControl("snapshot page credit bounds"));
    }
    let opened = now();
    if lease.expires_at <= opened {
        return Err(ProtocolError::InvalidControl("subscription lease expired"));
    }
    let snapshot = lease
        .snapshot
        .as_ref()
        .ok_or(ProtocolError::InvalidControl(
            "subscription has no pending snapshot",
        ))?;
    if snapshot.expires_at <= opened {
        return Err(ProtocolError::ResetDeadlineExceeded);
    }
    let requested = pending_page_cursor(lease, page_token)?;
    let page = subscription::snapshot_page(
        &snapshot.root,
        snapshot.cursor,
        snapshot.reason,
        requested,
        credit,
    )?;
    let next = page.page().next();
    if !page_advances(page.page(), requested) {
        return Err(ProtocolError::InvalidControl(
            "snapshot page made no progress",
        ));
    }
    let next_token = page
        .next_token()
        .map_err(|_| ProtocolError::InvalidControl("snapshot page token"))?;
    let continuation = snapshot_continuation(next, next_token)?;
    let pages_remaining = continuation
        .as_ref()
        .map(|_| {
            snapshot
                .pages_remaining
                .get()
                .checked_sub(1)
                .and_then(NonZeroU16::new)
                .ok_or(ProtocolError::ResetPageBudgetExhausted)
        })
        .transpose()?;
    let payload = encode(&page)?;

    // Encoding can be expensive. A page completed after either finite lease
    // bound must not revive the retained root or move its deadline.
    let completed = now();
    if lease.expires_at <= completed {
        return Err(ProtocolError::InvalidControl("subscription lease expired"));
    }
    if snapshot.expires_at <= completed {
        return Err(ProtocolError::ResetDeadlineExceeded);
    }
    let expires_at = lease_deadline(completed, lease.duration)?;
    Ok(PreparedSnapshotPage {
        cursor: snapshot.cursor,
        continuation,
        pages_remaining,
        payload,
        expires_at,
        reset_expires_at: snapshot.expires_at,
    })
}

fn commit_snapshot_page(
    state: &mut DurableLease,
    prepared: PreparedSnapshotPage,
    credit: usize,
) -> Result<(Option<Box<[u8]>>, Box<[u8]>, Instant), ProtocolError> {
    enum CommitContinuation {
        Finished,
        Continue(SnapshotContinuation, NonZeroU16),
    }
    let continuation = match (prepared.continuation, prepared.pages_remaining) {
        (Some(continuation), Some(pages_remaining)) => {
            CommitContinuation::Continue(continuation, pages_remaining)
        }
        (None, None) => CommitContinuation::Finished,
        _ => {
            return Err(ProtocolError::ResetPageBudgetExhausted);
        }
    };
    let next_token = match &continuation {
        CommitContinuation::Finished => None,
        CommitContinuation::Continue(next, _) => Some(next.token.clone()),
    };
    let snapshot = state.snapshot.take().ok_or(ProtocolError::InvalidControl(
        "subscription has no pending snapshot",
    ))?;
    state.cursor = prepared.cursor.encode_control();
    state.credit = credit;
    state.expires_at = prepared.expires_at;
    state.snapshot = match continuation {
        CommitContinuation::Finished => None,
        CommitContinuation::Continue(next, pages_remaining) => Some(DurableSnapshot {
            root: snapshot.root,
            cursor: prepared.cursor,
            reason: snapshot.reason,
            next,
            expires_at: prepared.reset_expires_at,
            pages_remaining,
        }),
    };
    Ok((next_token, prepared.payload, retention_deadline(state)))
}

fn increased_subscription_credit(
    current: usize,
    additional: usize,
) -> Result<usize, ProtocolError> {
    let next = current
        .checked_add(additional)
        .ok_or(ProtocolError::InvalidControl(
            "subscription credit overflow",
        ))?;
    if next > backend_engine::MAX_SUBSCRIPTION_EVENTS {
        return Err(ProtocolError::InvalidControl("subscription credit bounds"));
    }
    Ok(next)
}

fn apply_subscription_credit(
    state: &mut DurableLease,
    additional: usize,
) -> Result<(), ProtocolError> {
    let credit = increased_subscription_credit(state.credit, additional)?;
    state.credit = credit;
    Ok(())
}

fn lease_deadline(now: Instant, duration: Duration) -> Result<Instant, ProtocolError> {
    now.checked_add(duration)
        .ok_or(ProtocolError::LeaseDeadlineUnavailable)
}

fn retained_reset_snapshot(
    root: backend_engine::ViewRoot,
    cursor: Cursor,
    reason: backend_engine::CursorResetReason,
    continuation: Option<SnapshotContinuation>,
    started_at: Instant,
    now: Instant,
    limits: SubscriptionLeaseLimits,
) -> Result<Option<DurableSnapshot>, ProtocolError> {
    let Some(next) = continuation else {
        return Ok(None);
    };
    let pages_remaining = limits
        .max_reset_pages
        .checked_sub(1)
        .and_then(|remaining| u16::try_from(remaining).ok())
        .and_then(NonZeroU16::new)
        .ok_or(ProtocolError::ResetPageBudgetExhausted)?;
    let expires_at = lease_deadline(started_at, limits.max_reset_duration)?;
    if expires_at <= now {
        return Err(ProtocolError::ResetDeadlineExceeded);
    }
    Ok(Some(DurableSnapshot {
        root: Box::new(root),
        cursor,
        reason,
        next,
        expires_at,
        pages_remaining,
    }))
}

fn snapshot_continuation(
    cursor: Option<ViewPageCursor>,
    token: Option<Box<[u8]>>,
) -> Result<Option<SnapshotContinuation>, ProtocolError> {
    match (cursor, token) {
        (Some(cursor), Some(token)) => Ok(Some(SnapshotContinuation { cursor, token })),
        (None, None) => Ok(None),
        _ => Err(ProtocolError::InvalidControl(
            "reset continuation cursor and token disagree",
        )),
    }
}

fn retention_deadline(state: &DurableLease) -> Instant {
    state
        .snapshot
        .as_ref()
        .map_or(state.expires_at, |snapshot| {
            state.expires_at.min(snapshot.expires_at)
        })
}

fn note_lease_expiry(next: &mut Option<Instant>, expires_at: Instant) {
    *next = Some(next.map_or(expires_at, |current| current.min(expires_at)));
}

/// Called from the existing owner poll, even while no client has a request.
/// The hint makes a normal poll O(1); at a due deadline the active set is
/// bounded by `SubscriptionLeaseLimits` and is scanned once to drop expired
/// roots and recompute the next deadline. An old hint after renewal/removal is
/// safe: it causes one early scan, never premature reclamation.
fn sweep_expired_leases(
    leases: &mut BTreeMap<LocalSubscriptionId, DurableLease>,
    next_expiry: &mut Option<Instant>,
    now: Instant,
) -> usize {
    if next_expiry.is_none_or(|deadline| deadline > now) {
        return 0;
    }
    let before = leases.len();
    leases.retain(|_, state| retention_deadline(state) > now);
    *next_expiry = leases.values().map(retention_deadline).min();
    before - leases.len()
}

fn admit_subscription_open(
    leases: &mut BTreeMap<LocalSubscriptionId, DurableLease>,
    next_expiry: &mut Option<Instant>,
    limits: SubscriptionLeaseLimits,
    now: Instant,
) -> Result<(), ProtocolError> {
    sweep_expired_leases(leases, next_expiry, now);
    if leases.len() >= limits.max_active {
        return Err(ProtocolError::Backpressure);
    }
    Ok(())
}

fn release_all_leases(
    leases: &mut BTreeMap<LocalSubscriptionId, DurableLease>,
    next_expiry: &mut Option<Instant>,
) {
    leases.clear();
    *next_expiry = None;
}

#[path = "service/admission.rs"]
mod admission;
pub use admission::{
    CompletionAdmission, NoCompletionAdmission, NoReplicationAdmission, NoSemanticRangeAdmission,
    ReplicationAdmission, SemanticRangeAdmission,
};

impl<M, V, A, F, C, R, S> fmt::Debug for LocaldOwner<M, V, A, F, C, R, S>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
    F: fmt::Debug,
    C: fmt::Debug,
    R: fmt::Debug,
    S: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocaldOwner")
            .field("daemon", &self.daemon)
            .field("command", &self.command)
            .field("completion", &self.completion)
            .field("replication", &self.replication)
            .field("semantic_ranges", &self.semantic_ranges)
            .field("leases", &self.leases)
            .field("next_lease_expiry", &self.next_lease_expiry)
            .field("lease_limits", &self.lease_limits)
            .field(
                "boot_nonce_initialized",
                &self.lease_identity.boot_nonce.is_some(),
            )
            .field("next_lease_nonce", &self.lease_identity.next_nonce)
            .finish()
    }
}

impl<M, V, A, F, C, R, S> LocaldOwner<M, V, A, F, C, R, S>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
    C: CompletionAdmission<M, V, A>,
    R: ReplicationAdmission<M, V, A>,
    S: SemanticRangeAdmission,
{
    /// Lets `deferred` take commands now and answer them later, so the owner
    /// loop answers other requests while their long part runs.
    #[must_use]
    pub fn with_deferred_commands(
        mut self,
        deferred: Box<dyn DeferredCommands<M, V, A> + Send>,
    ) -> Self {
        self.deferred = Some(deferred);
        self
    }

    /// Configures the finite owner-side retention budget before serving.
    #[must_use]
    pub fn with_subscription_lease_limits(mut self, limits: SubscriptionLeaseLimits) -> Self {
        self.lease_limits = limits;
        self
    }

    fn sweep_leases(&mut self, now: Instant) -> usize {
        sweep_expired_leases(&mut self.leases, &mut self.next_lease_expiry, now)
    }

    fn allocate_lease(
        &mut self,
        request_id: u64,
        cursor: &[u8],
    ) -> Result<LocalSubscriptionId, ProtocolError> {
        self.lease_identity
            .allocate(request_id, cursor, &self.leases)
    }

    fn request_subscription(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        credit: usize,
    ) -> Result<SubscriptionReply, ProtocolError> {
        let receiver = self
            .daemon
            .client()
            .request(
                request_id,
                crate::Request::Subscribe {
                    cursor: cursor.to_vec().into_boxed_slice(),
                    credit,
                },
            )
            .map_err(|error| map_queue_error(&error))?;
        if !self.daemon.serve_one() {
            return Err(ProtocolError::Closed);
        }
        match wait_for_daemon_reply(&mut self.daemon, &receiver)? {
            DaemonReply::Subscribed(Ok(reply)) => Ok(reply),
            DaemonReply::Subscribed(Err(error)) => Err(ProtocolError::InvalidControl(
                subscription_error_text(&error),
            )),
            DaemonReply::Commit(_)
            | DaemonReply::Query(_)
            | DaemonReply::Replicated(_)
            | DaemonReply::Completed(_) => Err(ProtocolError::InvalidControl(
                "subscription reply used the wrong engine lane",
            )),
        }
    }

    fn encode_reply_payload(
        &mut self,
        reply: SubscriptionReply,
        requested_cursor: &[u8],
    ) -> Result<Box<[u8]>, ProtocolError> {
        match subscription::subscription_status(&self.daemon, reply, Some(requested_cursor))? {
            EngineStatus::AcceptedPayload(payload) => Ok(payload),
            EngineStatus::Accepted
            | EngineStatus::Queued { .. }
            | EngineStatus::Rejected(_)
            | EngineStatus::Subscription(_)
            | EngineStatus::SemanticRangeChunk(_)
            | EngineStatus::SemanticMetadataChunk(_)
            | EngineStatus::SemanticStaleSelection => Err(ProtocolError::InvalidControl(
                "subscription reply omitted its typed payload",
            )),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "open response handling preserves one lease transition"
    )]
    fn open_subscription(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        credit: usize,
        lease_ms: u64,
    ) -> Result<EngineStatus, ProtocolError> {
        if credit == 0 || credit > backend_engine::MAX_SUBSCRIPTION_EVENTS {
            return Err(ProtocolError::InvalidControl("subscription lease bounds"));
        }
        let duration = self.lease_limits.duration(lease_ms)?;
        admit_subscription_open(
            &mut self.leases,
            &mut self.next_lease_expiry,
            self.lease_limits,
            Instant::now(),
        )?;
        let lease = self.allocate_lease(request_id, cursor)?;
        let reply = self.request_subscription(request_id, cursor, credit)?;
        let opened_at = Instant::now();
        let expires_at = lease_deadline(opened_at, duration)?;
        let response = match reply {
            SubscriptionReply::Accepted { credit } => {
                let current = subscription::owner_cursor(&self.daemon)?;
                let current_bytes = current.encode_control();
                self.leases.insert(
                    lease,
                    DurableLease {
                        cursor: current_bytes.clone(),
                        credit,
                        duration,
                        expires_at,
                        snapshot: None,
                    },
                );
                note_lease_expiry(&mut self.next_lease_expiry, expires_at);
                LocalSubscriptionResponse::Opened {
                    request_id,
                    lease,
                    cursor: current_bytes,
                    credit,
                    lease_ms,
                }
            }
            SubscriptionReply::Events {
                credit,
                cursor: target,
                events,
            } => {
                let payload = self.encode_reply_payload(
                    SubscriptionReply::Events {
                        credit,
                        cursor: target.clone(),
                        events: events.clone(),
                    },
                    cursor,
                )?;
                let root = self.daemon.engine().daemon().library().view();
                let target_cursor = Cursor::decode_control_for_root(&target, root)
                    .map_err(|_| ProtocolError::InvalidControl("subscription target cursor"))?;
                let previous = event_previous_cursor(cursor, target_cursor, &events)
                    .unwrap_or_else(|| cursor.to_vec().into_boxed_slice());
                self.leases.insert(
                    lease,
                    DurableLease {
                        cursor: target.clone(),
                        credit,
                        duration,
                        expires_at,
                        snapshot: None,
                    },
                );
                note_lease_expiry(&mut self.next_lease_expiry, expires_at);
                LocalSubscriptionResponse::Batch {
                    request_id,
                    lease,
                    previous,
                    cursor: target,
                    credit,
                    payload,
                }
            }
            SubscriptionReply::ResetWithRoot {
                credit,
                cursor: target,
                root,
                reason,
            } => {
                let target_cursor = decode_cursor_for_service(&target, &root)?;
                let page = subscription::snapshot_page(
                    &root,
                    target_cursor,
                    reason,
                    ViewPageCursor::first(&root),
                    credit.min(backend_engine::MAX_SNAPSHOT_PAGE_ROWS),
                )?;
                let next_cursor = page.page().next();
                let next_token = page
                    .next_token()
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page token"))?;
                let continuation = snapshot_continuation(next_cursor, next_token.clone())?;
                let payload = backend_engine::encode_snapshot_page_dto(&page)
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page encoding"))?;
                let state = DurableLease {
                    cursor: target.clone(),
                    credit,
                    duration,
                    expires_at,
                    snapshot: retained_reset_snapshot(
                        *root,
                        target_cursor,
                        reason,
                        continuation,
                        opened_at,
                        Instant::now(),
                        self.lease_limits,
                    )?,
                };
                let next_deadline = retention_deadline(&state);
                self.leases.insert(lease, state);
                note_lease_expiry(&mut self.next_lease_expiry, next_deadline);
                LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page: Box::new([]),
                    next: next_token,
                    credit,
                    payload: payload.into_boxed_slice(),
                }
            }
            SubscriptionReply::Reset { .. } => {
                return Err(ProtocolError::InvalidControl(
                    "subscription reset omitted its replacement root",
                ));
            }
        };
        Ok(EngineStatus::Subscription(response))
    }

    fn durable_subscription(
        &mut self,
        request_id: u64,
        request: LocalSubscriptionRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        if request.request_id != request_id {
            return Err(ProtocolError::InvalidControl(
                "subscription request correlation mismatch",
            ));
        }
        self.sweep_leases(Instant::now());
        match request.operation {
            LocalSubscriptionOperation::Open {
                cursor,
                credit,
                lease_ms,
            } => self.open_subscription(request_id, cursor.as_ref(), credit, lease_ms),
            LocalSubscriptionOperation::Resume {
                lease,
                cursor,
                credit,
                lease_ms,
            } => self.resume_subscription(request_id, lease, cursor.as_ref(), credit, lease_ms),
            LocalSubscriptionOperation::Credit { lease, credit } => {
                self.credit_subscription(request_id, lease, credit)
            }
            LocalSubscriptionOperation::Ack { lease, cursor } => {
                self.ack_subscription(request_id, lease, cursor)
            }
            LocalSubscriptionOperation::Renew {
                lease,
                cursor,
                credit,
                lease_ms,
            } => self.renew_subscription(request_id, lease, cursor, credit, lease_ms),
            LocalSubscriptionOperation::Cancel { lease } => {
                if self.leases.remove(&lease).is_some() {
                    Ok(EngineStatus::Subscription(
                        LocalSubscriptionResponse::Cancelled { request_id, lease },
                    ))
                } else {
                    Err(ProtocolError::InvalidControl("unknown subscription lease"))
                }
            }
            LocalSubscriptionOperation::Page {
                lease,
                page,
                credit,
            } => self.page_subscription(request_id, lease, page, credit),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "resume response handling preserves one lease transition"
    )]
    fn resume_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: &[u8],
        credit: usize,
        lease_ms: u64,
    ) -> Result<EngineStatus, ProtocolError> {
        let state = retained_lease(&self.leases, lease)?;
        if state.expires_at <= Instant::now() {
            self.leases.remove(&lease);
            return Err(ProtocolError::InvalidControl("subscription lease expired"));
        }
        if state.snapshot.is_some() || state.cursor.as_ref() != cursor {
            return Err(ProtocolError::InvalidControl(
                "subscription resume cursor is not the lease cursor",
            ));
        }
        if credit == 0 || credit > backend_engine::MAX_SUBSCRIPTION_EVENTS {
            return Err(ProtocolError::InvalidControl("subscription lease bounds"));
        }
        let duration = self.lease_limits.duration(lease_ms)?;
        let reply = self.request_subscription(request_id, cursor, credit)?;
        let renewed_at = Instant::now();
        if self
            .leases
            .get(&lease)
            .is_none_or(|state| state.expires_at <= renewed_at)
        {
            self.leases.remove(&lease);
            return Err(ProtocolError::InvalidControl("subscription lease expired"));
        }
        let expires_at = lease_deadline(renewed_at, duration)?;
        let mut next_deadline = expires_at;
        let response = match reply {
            SubscriptionReply::Accepted { credit } => {
                let current = subscription::owner_cursor(&self.daemon)?;
                let current_bytes = current.encode_control();
                let state = self
                    .leases
                    .get_mut(&lease)
                    .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
                state.cursor.clone_from(&current_bytes);
                state.credit = credit;
                state.duration = duration;
                state.expires_at = expires_at;
                LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: current_bytes,
                    credit,
                    lease_ms,
                }
            }
            SubscriptionReply::Events {
                credit,
                cursor: target,
                events,
            } => {
                let payload = self.encode_reply_payload(
                    SubscriptionReply::Events {
                        credit,
                        cursor: target.clone(),
                        events: events.clone(),
                    },
                    cursor,
                )?;
                let root = self.daemon.engine().daemon().library().view();
                let target_cursor = Cursor::decode_control_for_root(&target, root)
                    .map_err(|_| ProtocolError::InvalidControl("subscription target cursor"))?;
                let previous = event_previous_cursor(cursor, target_cursor, &events)
                    .unwrap_or_else(|| cursor.to_vec().into_boxed_slice());
                let state = self
                    .leases
                    .get_mut(&lease)
                    .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
                state.cursor.clone_from(&target);
                state.credit = credit;
                state.duration = duration;
                state.expires_at = expires_at;
                LocalSubscriptionResponse::Batch {
                    request_id,
                    lease,
                    previous,
                    cursor: target,
                    credit,
                    payload,
                }
            }
            SubscriptionReply::ResetWithRoot {
                credit,
                cursor: target,
                root,
                reason,
            } => {
                let target_cursor = decode_cursor_for_service(&target, &root)?;
                let page = subscription::snapshot_page(
                    &root,
                    target_cursor,
                    reason,
                    ViewPageCursor::first(&root),
                    credit.min(backend_engine::MAX_SNAPSHOT_PAGE_ROWS),
                )?;
                let next_cursor = page.page().next();
                let next_token = page
                    .next_token()
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page token"))?;
                let continuation = snapshot_continuation(next_cursor, next_token.clone())?;
                let payload = backend_engine::encode_snapshot_page_dto(&page)
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page encoding"))?;
                let retained = retained_reset_snapshot(
                    *root,
                    target_cursor,
                    reason,
                    continuation,
                    renewed_at,
                    Instant::now(),
                    self.lease_limits,
                )?;
                let state = self
                    .leases
                    .get_mut(&lease)
                    .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
                state.cursor.clone_from(&target);
                state.credit = credit;
                state.duration = duration;
                state.expires_at = expires_at;
                state.snapshot = retained;
                next_deadline = retention_deadline(state);
                LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page: Box::new([]),
                    next: next_token,
                    credit,
                    payload: payload.into_boxed_slice(),
                }
            }
            SubscriptionReply::Reset { .. } => {
                return Err(ProtocolError::InvalidControl(
                    "subscription reset omitted its replacement root",
                ));
            }
        };
        note_lease_expiry(&mut self.next_lease_expiry, next_deadline);
        Ok(EngineStatus::Subscription(response))
    }

    fn credit_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        credit: usize,
    ) -> Result<EngineStatus, ProtocolError> {
        if credit == 0 || credit > backend_engine::MAX_SUBSCRIPTION_EVENTS {
            return Err(ProtocolError::InvalidControl("subscription credit bounds"));
        }
        if self
            .leases
            .get(&lease)
            .is_none_or(|state| state.expires_at <= Instant::now())
        {
            self.leases.remove(&lease);
            return Err(ProtocolError::InvalidControl("subscription lease expired"));
        }
        let state = self
            .leases
            .get_mut(&lease)
            .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
        apply_subscription_credit(state, credit)?;
        Ok(EngineStatus::Subscription(
            LocalSubscriptionResponse::Renewed {
                request_id,
                lease,
                cursor: state.cursor.clone(),
                credit: state.credit,
                lease_ms: state
                    .expires_at
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            },
        ))
    }

    fn ack_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: Box<[u8]>,
    ) -> Result<EngineStatus, ProtocolError> {
        let state = self
            .leases
            .get(&lease)
            .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
        if state.expires_at <= Instant::now() {
            self.leases.remove(&lease);
            return Err(ProtocolError::InvalidControl("subscription lease expired"));
        }
        if state.cursor.as_ref() != cursor.as_ref() {
            return Err(ProtocolError::InvalidControl(
                "subscription acknowledgement cursor mismatch",
            ));
        }
        Ok(EngineStatus::Subscription(
            LocalSubscriptionResponse::Acked {
                request_id,
                lease,
                cursor,
            },
        ))
    }

    fn renew_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: Box<[u8]>,
        credit: usize,
        lease_ms: u64,
    ) -> Result<EngineStatus, ProtocolError> {
        if credit == 0 || credit > backend_engine::MAX_SUBSCRIPTION_EVENTS {
            return Err(ProtocolError::InvalidControl("subscription lease bounds"));
        }
        let duration = self.lease_limits.duration(lease_ms)?;
        let now = Instant::now();
        let state = retained_lease(&self.leases, lease)?;
        if state.expires_at <= now || state.cursor.as_ref() != cursor.as_ref() {
            return Err(ProtocolError::InvalidControl(
                "subscription renewal cursor or lease mismatch",
            ));
        }
        let expires_at = lease_deadline(now, duration)?;
        let state = self
            .leases
            .get_mut(&lease)
            .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
        state.credit = credit;
        state.duration = duration;
        state.expires_at = expires_at;
        let next_deadline = retention_deadline(state);
        note_lease_expiry(&mut self.next_lease_expiry, next_deadline);
        Ok(EngineStatus::Subscription(
            LocalSubscriptionResponse::Renewed {
                request_id,
                lease,
                cursor,
                credit,
                lease_ms,
            },
        ))
    }

    fn page_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        page_token: Box<[u8]>,
        credit: usize,
    ) -> Result<EngineStatus, ProtocolError> {
        let prepared = {
            let state = retained_lease(&self.leases, lease)?;
            prepare_snapshot_page(state, page_token.as_ref(), credit, Instant::now, |page| {
                backend_engine::encode_snapshot_page_dto(page)
                    .map(Vec::into_boxed_slice)
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page encoding"))
            })
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error @ ProtocolError::ResetDeadlineExceeded)
            | Err(error @ ProtocolError::ResetPageBudgetExhausted)
            | Err(error @ ProtocolError::InvalidControl("subscription lease expired")) => {
                self.leases.remove(&lease);
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let state = self
            .leases
            .get_mut(&lease)
            .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
        // All fallible work is complete; now atomically consume the old page
        // continuation and retain only the exact next one, if any.
        let (next_token, payload, next_deadline) = commit_snapshot_page(state, prepared, credit)?;
        note_lease_expiry(&mut self.next_lease_expiry, next_deadline);
        Ok(EngineStatus::Subscription(
            LocalSubscriptionResponse::SnapshotPage {
                request_id,
                lease,
                page: page_token,
                next: next_token,
                credit,
                payload,
            },
        ))
    }
}

fn event_previous_cursor(
    requested: &[u8],
    mut target: Cursor,
    events: &[CursorEvent],
) -> Option<Box<[u8]>> {
    if !requested.is_empty() {
        return Some(requested.to_vec().into_boxed_slice());
    }
    for event in events.iter().rev() {
        target = target.rewind_event(event).ok()?;
    }
    Some(target.encode_control())
}

fn subscription_error_text(error: &backend_engine::DaemonError) -> &'static str {
    match error {
        backend_engine::DaemonError::SubscriptionCredit => "subscription credit rejected",
        backend_engine::DaemonError::CursorInvalid => "subscription cursor rejected",
        _ => "subscription request rejected",
    }
}

impl<M, V, A, F>
    LocaldOwner<M, V, A, F, NoCompletionAdmission, NoReplicationAdmission, NoSemanticRangeAdmission>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    /// Creates an owner adapter. `command` receives serialized command bytes
    /// and must return a serialized strict `ReplyDto` body.
    pub fn new(daemon: crate::Locald<M, V, A>, command: F) -> Self {
        Self {
            daemon,
            command,
            completion: NoCompletionAdmission,
            replication: NoReplicationAdmission,
            semantic_ranges: NoSemanticRangeAdmission,
            owner_lane_schedule: OwnerLaneSchedule::default(),
            leases: BTreeMap::new(),
            next_lease_expiry: None,
            lease_limits: SubscriptionLeaseLimits::default(),
            lease_identity: OwnerLeaseIdentity::default(),
            deferred: None,
        }
    }
}

impl<M, V, A, F, C> LocaldOwner<M, V, A, F, C, NoReplicationAdmission, NoSemanticRangeAdmission>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    /// Creates an owner adapter with an explicit completion admission seam.
    /// The seam receives only untrusted fields and must compare them against
    /// a scheduler-issued completion ticket before submitting to the engine.
    pub fn with_completion(daemon: crate::Locald<M, V, A>, command: F, completion: C) -> Self {
        Self {
            daemon,
            command,
            completion,
            replication: NoReplicationAdmission,
            semantic_ranges: NoSemanticRangeAdmission,
            owner_lane_schedule: OwnerLaneSchedule::default(),
            leases: BTreeMap::new(),
            next_lease_expiry: None,
            lease_limits: SubscriptionLeaseLimits::default(),
            lease_identity: OwnerLeaseIdentity::default(),
            deferred: None,
        }
    }

    /// Creates an owner adapter with explicit completion and worker-result
    /// admission seams. The replication seam owns the retained dispatch plan
    /// and contract needed for actual dispatcher admission.
    pub fn with_admission<R>(
        daemon: crate::Locald<M, V, A>,
        command: F,
        completion: C,
        replication: R,
    ) -> LocaldOwner<M, V, A, F, C, R, NoSemanticRangeAdmission> {
        LocaldOwner {
            daemon,
            command,
            completion,
            replication,
            semantic_ranges: NoSemanticRangeAdmission,
            owner_lane_schedule: OwnerLaneSchedule::default(),
            leases: BTreeMap::new(),
            next_lease_expiry: None,
            lease_limits: SubscriptionLeaseLimits::default(),
            lease_identity: OwnerLeaseIdentity::default(),
            deferred: None,
        }
    }

    /// Creates an owner adapter with an explicit semantic range handler.
    pub fn with_semantic_range_admission<R, S>(
        daemon: crate::Locald<M, V, A>,
        command: F,
        completion: C,
        replication: R,
        semantic_ranges: S,
    ) -> LocaldOwner<M, V, A, F, C, R, S>
    where
        R: ReplicationAdmission<M, V, A>,
        S: SemanticRangeAdmission,
    {
        LocaldOwner {
            daemon,
            command,
            completion,
            replication,
            semantic_ranges,
            owner_lane_schedule: OwnerLaneSchedule::default(),
            leases: BTreeMap::new(),
            next_lease_expiry: None,
            lease_limits: SubscriptionLeaseLimits::default(),
            lease_identity: OwnerLeaseIdentity::default(),
            deferred: None,
        }
    }

    /// Returns the embedded daemon.
    #[must_use]
    pub const fn daemon(&self) -> &crate::Locald<M, V, A> {
        &self.daemon
    }

    /// Returns the embedded daemon mutably.
    #[must_use]
    pub const fn daemon_mut(&mut self) -> &mut crate::Locald<M, V, A> {
        &mut self.daemon
    }
}

impl<M, V, A, F, C, R, S, E> OwnerService for LocaldOwner<M, V, A, F, C, R, S>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
    F: FnMut(&mut crate::Locald<M, V, A>, &[u8]) -> Result<Vec<u8>, E>,
    E: fmt::Display,
    C: CompletionAdmission<M, V, A>,
    R: ReplicationAdmission<M, V, A>,
    S: SemanticRangeAdmission,
{
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        (self.command)(&mut self.daemon, body)
            .map_err(|error| ProtocolError::CommandExecution(error.to_string()))
    }

    fn command_or_defer(
        &mut self,
        body: &[u8],
        ticket: u64,
    ) -> Result<CommandOutcome, ProtocolError> {
        match self.deferred.as_mut() {
            Some(deferred) => deferred
                .command(&mut self.daemon, body, ticket)
                .map_err(ProtocolError::CommandExecution),
            None => self.command(body).map(CommandOutcome::Reply),
        }
    }

    fn poll_deferred(&mut self) -> Vec<(u64, Result<Vec<u8>, ProtocolError>)> {
        let Some(deferred) = self.deferred.as_mut() else {
            return Vec::new();
        };
        deferred
            .poll(&mut self.daemon)
            .into_iter()
            .map(|(ticket, reply)| (ticket, reply.map_err(ProtocolError::CommandExecution)))
            .collect()
    }

    fn engine(
        &mut self,
        request_id: u64,
        request: EngineRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        let subscription_requested = match &request {
            EngineRequest::Subscribe { cursor, .. } => Some(cursor.clone()),
            EngineRequest::Replicate(_)
            | EngineRequest::Complete(_)
            | EngineRequest::Subscription(_)
            | EngineRequest::SemanticRangeGet(_)
            | EngineRequest::SemanticMetadataGet(_)
            | EngineRequest::Shutdown => None,
        };
        let request = match request {
            EngineRequest::Replicate(message) => {
                return self
                    .replication
                    .admit(&mut self.daemon, request_id, *message);
            }
            EngineRequest::Subscribe { cursor, credit } => {
                crate::Request::Subscribe { cursor, credit }
            }
            EngineRequest::Complete(claim) => {
                return self.completion.admit(&mut self.daemon, request_id, claim);
            }
            EngineRequest::Subscription(subscription) => {
                return self.durable_subscription(request_id, subscription);
            }
            EngineRequest::SemanticRangeGet(request) => {
                return match self.semantic_ranges.serve(request_id, request) {
                    Ok(chunk) => Ok(EngineStatus::SemanticRangeChunk(chunk)),
                    Err(ProtocolError::SemanticStaleSelection) => {
                        Ok(EngineStatus::SemanticStaleSelection)
                    }
                    Err(error) => Err(error),
                };
            }
            EngineRequest::SemanticMetadataGet(request) => {
                return match self.semantic_ranges.serve(request_id, request) {
                    Ok(chunk) => Ok(EngineStatus::SemanticMetadataChunk(chunk)),
                    Err(ProtocolError::SemanticStaleSelection) => {
                        Ok(EngineStatus::SemanticStaleSelection)
                    }
                    Err(error) => Err(error),
                };
            }
            // The listener answers a lifecycle request before it reaches any
            // owner. Reaching here means a host wired a service without one.
            EngineRequest::Shutdown => {
                return Err(ProtocolError::InvalidControl(
                    "shutdown is a listener lifecycle request",
                ));
            }
        };
        let receiver = self
            .daemon
            .client()
            .request(request_id, request)
            .map_err(|error| map_queue_error(&error))?;
        if !self.daemon.serve_one() {
            return Err(ProtocolError::Closed);
        }
        let reply = wait_for_daemon_reply(&mut self.daemon, &receiver)?;
        Ok(match reply {
            DaemonReply::Replicated(Ok(_)) | DaemonReply::Completed(Ok(())) => {
                EngineStatus::Accepted
            }
            DaemonReply::Subscribed(Ok(subscription)) => subscription::subscription_status(
                &self.daemon,
                subscription,
                subscription_requested.as_deref(),
            )?,
            DaemonReply::Replicated(Err(error))
            | DaemonReply::Completed(Err(error))
            | DaemonReply::Subscribed(Err(error)) => EngineStatus::Rejected(error.to_string()),
            DaemonReply::Commit(_) | DaemonReply::Query(_) => {
                EngineStatus::Rejected("operation was sent to the wrong engine lane".to_owned())
            }
        })
    }

    fn serve_one(&mut self) -> bool {
        // The listener calls this on every poll even when no client is
        // connected or sending frames. Expiry therefore releases abandoned
        // reset roots without waiting for a same-lease request.
        let expired = self.sweep_leases(Instant::now()) != 0;
        // Remote transports are daemon-owned, but their socket reads must not
        // monopolize the owner while a client waits for its next frame. Poll
        // the composition's bounded inbox first, then run one fair engine
        // lane. A later client request can therefore observe durable output
        // from a completion that arrived out of order.
        let (remote_progress, engine_progress) = match self.owner_lane_schedule.next_turn() {
            OwnerLaneTurn::Remote => {
                let remote_progress = self.replication.poll(&mut self.daemon);
                if remote_progress {
                    self.owner_lane_schedule.remote_polled(true);
                    (true, false)
                } else {
                    self.owner_lane_schedule.remote_polled(false);
                    (false, self.daemon.serve_one())
                }
            }
            OwnerLaneTurn::Engine => {
                self.owner_lane_schedule.engine_polled();
                (false, self.daemon.serve_one())
            }
        };
        expired || remote_progress || engine_progress
    }

    fn close(&mut self) {
        release_all_leases(&mut self.leases, &mut self.next_lease_expiry);
        if let Some(deferred) = self.deferred.as_mut() {
            deferred.close();
        }
        self.daemon.close();
    }
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
