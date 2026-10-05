//! The index owner as a data event, never a wait (W-Open I1, window first).
//!
//! `main` opens the window at once. The owner — embedded, or an attached one
//! that is already live — starts on its own thread (`host::owner`), and its
//! answer arrives through one [`OwnerGate`]:
//!
//! - worker threads (the read pool's sessions, the engine actor) block in
//!   [`OwnerGate::wait`] before their first connect. The UI thread never
//!   calls it;
//! - the UI observes the gate through [`watch`]. `Ready` becomes the
//!   owner's root (`Intent::OwnerReady`) and releases the reads the store
//!   held (`DataStore::owner_ready`). `Failed` becomes a fault on every page
//!   the window shows, in the owner's own words (`DataStore::owner_failed`).
//!   The page's "Try again" restarts the owner ([`OwnerGate::restart`]).

use crate::core::VersionedRoot;
use crate::model::ServiceMode;
use backend_client::{ClientError, PublicationBudgetKind, PublicationOperation};
use backend_replication::{LocalControlError, LocalControlExchangePhase};
use gpui::{App, Entity};
use std::fmt;
use std::future::Future;
use std::io::ErrorKind;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

/// How long an owner may take to answer before the window says it did not.
/// An embedded start takes seconds; this bounds a host that hangs, so a page
/// never waits on it for ever and "Try again" has something to retry.
pub const PATIENCE: Duration = Duration::from_mins(1);

/// Terminal reason the publication observer withdrew a serving attachment.
/// A delayed reply or closed socket does not, by itself, prove owner death.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationFailure {
    /// The actor did not install its first complete root before the limit.
    InitialRoot(Duration),
    /// Dial or authentication failed before a publication frame began.
    Setup(ClientError),
    /// The exact socket could not be interrupted on shutdown.
    MissingInterrupt,
    /// One request reached its fixed deadline; it may have been admitted.
    ResponseStalled {
        /// Last frame phase before the deadline.
        phase: LocalControlExchangePhase,
        /// Time spent in that exact exchange.
        elapsed: Duration,
    },
    /// The peer closed or reset the in-flight stream.
    PeerClosed {
        /// Last in-flight frame phase.
        phase: LocalControlExchangePhase,
    },
    /// Other socket I/O failed in the named exchange phase.
    TransportIo {
        /// Last in-flight frame phase.
        phase: LocalControlExchangePhase,
        /// Native I/O failure category.
        kind: ErrorKind,
    },
    /// The complete frame failed bounded grammar or correlation.
    InvalidFrame {
        /// Last in-flight frame phase.
        phase: LocalControlExchangePhase,
        /// Bounded shared frame/control grammar failure.
        error: LocalControlError,
    },
    /// A complete reply failed local producer/root/cursor admission.
    InvalidAuthority(ClientError),
    /// The producer explicitly refused an operation other than recoverable Resume.
    ProducerRejected {
        /// Operation the producer refused.
        operation: PublicationOperation,
        /// Bounded producer diagnostic.
        detail: Arc<str>,
    },
    /// Ordinary freshness recovery consumed its one absolute window.
    RecoveryExpired(Duration),
    /// The one fresh Open after a rejected Resume did not certify a root.
    ReacquisitionFailed(Box<ObservationFailure>),
    /// A finite number of reconnects ended without a certified reply.
    ReconnectsExhausted(Box<ObservationFailure>),
    /// The fixed ordinary or authenticated reset budget expired.
    BudgetExpired {
        /// Budget class, including authenticated reset size when present.
        kind: PublicationBudgetKind,
        /// Fixed budget duration.
        allowance: Duration,
    },
    /// A callback withdrew a frame although its owner scope was still live.
    UnexpectedCancellation,
}

impl ObservationFailure {
    /// A fixed trace vocabulary; producer text and filesystem paths stay out
    /// of the lifecycle ledger.
    pub(crate) fn category(&self) -> &'static str {
        match self {
            Self::InitialRoot(_) => "initial-root",
            Self::Setup(_) => "setup",
            Self::MissingInterrupt => "missing-interrupt",
            Self::ResponseStalled { .. } => "response-stalled",
            Self::PeerClosed { .. } => "peer-closed",
            Self::TransportIo { .. } => "transport-io",
            Self::InvalidFrame { .. } => "invalid-frame",
            Self::InvalidAuthority(_) => "invalid-authority",
            Self::ProducerRejected { .. } => "producer-rejected",
            Self::RecoveryExpired(_) => "recovery-expired",
            Self::ReacquisitionFailed(_) => "reacquisition-failed",
            Self::ReconnectsExhausted(_) => "reconnects-exhausted",
            Self::BudgetExpired {
                kind: PublicationBudgetKind::Ordinary,
                ..
            } => "ordinary-budget",
            Self::BudgetExpired { .. } => "authenticated-reset-budget",
            Self::UnexpectedCancellation => "unexpected-cancellation",
        }
    }
}

impl fmt::Display for ObservationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitialRoot(waited) => write!(formatter, "the initial complete root was not admitted within {} s", waited.as_secs()),
            Self::Setup(detail) => write!(formatter, "could not establish a publication connection: {detail}"),
            Self::MissingInterrupt => formatter.write_str("publication transport has no cancellation handle"),
            Self::ResponseStalled { phase, elapsed } => write!(formatter, "publication response stalled in {phase:?} for {} s", elapsed.as_secs()),
            Self::PeerClosed { phase } => write!(formatter, "publication peer closed during {phase:?}"),
            Self::TransportIo { phase, kind } => write!(formatter, "publication I/O failed during {phase:?}: {kind:?}"),
            Self::InvalidFrame { phase, error } => write!(formatter, "publication frame failed validation during {phase:?}: {error}"),
            Self::InvalidAuthority(detail) => write!(formatter, "publication proof failed: {detail}"),
            Self::ProducerRejected { operation, detail } => write!(formatter, "publication {operation:?} was rejected: {detail}"),
            Self::RecoveryExpired(waited) => write!(formatter, "publication recovery exceeded {} s", waited.as_secs()),
            Self::ReacquisitionFailed(cause) => write!(formatter, "fresh publication acquisition failed: {cause}"),
            Self::ReconnectsExhausted(cause) => write!(formatter, "publication reconnects were exhausted: {cause}"),
            Self::BudgetExpired { kind, allowance } => write!(formatter, "publication {kind:?} budget expired after {} s", allowance.as_secs()),
            Self::UnexpectedCancellation => formatter.write_str("publication exchange cancelled without owner withdrawal"),
        }
    }
}

/// Why the owner is not answering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnerFault {
    /// The host said it could not start, in its own words.
    Host(Arc<str>),
    /// The owner's thread panicked, and said so.
    Panicked(Arc<str>),
    /// An attached owner that had answered stopped answering a fresh probe.
    Lost(Arc<str>),
    /// The publication observer could not maintain certified producer freshness.
    Observation(ObservationFailure),
    /// Nothing answered within the patience.
    Silent(Duration),
    /// The window closed before the owner answered.
    Closed,
    /// The request was revoked while the owner was starting.
    Cancelled,
}

impl fmt::Display for OwnerFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host(words) => formatter.write_str(words),
            Self::Panicked(what) => write!(formatter, "the index's thread panicked: {what}"),
            Self::Lost(what) => write!(formatter, "the attached index stopped answering: {what}"),
            Self::Observation(what) => write!(
                formatter,
                "the index publication subscription is unavailable: {what}"
            ),
            Self::Silent(waited) => write!(formatter, "the index did not answer within {} s", waited.as_secs()),
            Self::Closed => formatter.write_str("the window closed before the index answered"),
            Self::Cancelled => formatter.write_str("the request was cancelled while the index was starting"),
        }
    }
}

impl From<&str> for OwnerFault {
    fn from(words: &str) -> Self {
        Self::Host(Arc::from(words))
    }
}

impl From<String> for OwnerFault {
    fn from(words: String) -> Self {
        Self::Host(Arc::from(words))
    }
}

/// Which publish a state was. The gate counts them, so a watcher can say
/// "after the last one I saw" and a patience can say "the start that began
/// at this one".
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Epoch(u64);

impl Epoch {
    const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A capability for exactly one currently failed, retryable publication.
/// Serving attachment and publication epochs cannot construct this token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetryGeneration(Epoch);

/// Index mutations belong to an owner lifetime, not a read-freshness lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MutationGeneration(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MutationAdmission {
    Ready(VersionedRoot),
    Observing,
    Replaced,
    Unavailable(OwnerFault),
}

/// What the window knows about its owner.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant, reason = "a handful are published per process")]
pub enum OwnerState {
    /// Starting, or attaching: nothing has answered yet.
    Starting,
    /// Answering: the root it answered at, and how this window reached it.
    Ready {
        /// The owner's root when it first answered.
        key: VersionedRoot,
        /// Embedded in this process, or attached to a live one.
        mode: ServiceMode,
    },
    /// Could not start, or stopped answering.
    Failed(OwnerFault),
}

/// Terminal admission of a shared, complete worker root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationAdmission {
    Admitted,
    Obsolete,
    Withdrawn,
    Invalid,
}

/// The one place the owner's state lives; cheap to clone and `Send`.
#[derive(Clone)]
pub struct OwnerGate(Arc<Shared>);

impl fmt::Debug for OwnerGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OwnerGate").field(&self.state()).finish()
    }
}

struct Shared {
    inner: Mutex<Inner>,
    changed: Condvar,
}

struct Inner {
    state: OwnerState,
    /// Bumps on every publish; the UI watcher remembers the last it saw.
    epoch: Epoch,
    /// Stable attachment identity; root publications only bump `epoch`.
    attachment: Epoch,
    /// Freshness recovery rotates read attachments but retains this lifetime.
    /// Exhaustion permanently disables new mutation capabilities.
    mutation_generation: Option<MutationGeneration>,
    mutation_cancel: super::actor::CancellationToken,
    /// One latest complete admitted worker root, shared with the actor.
    publication: Option<(Arc<backend_library::ViewRoot>, backend_library::Cursor)>,
    observation_cancel: super::actor::CancellationToken,
    observation_suspended: bool,
    /// Set only by fresh certified admission for this serving attachment.
    fresh_publication: Option<Epoch>,
    /// The UI watcher, parked until the next publish.
    waker: Option<Waker>,
    /// A restart was asked for (the page's "Try again" after a failure).
    restart: bool,
    /// A live host producer exists to consume Retry.
    retry_enabled: bool,
    /// When the state last changed: how long a start has been waited for.
    since: Instant,
    /// The app is quitting: the owner thread lets its host go.
    closed: bool,
}

impl OwnerGate {
    /// A gate whose owner has not answered yet.
    #[must_use]
    pub fn starting() -> Self {
        Self::with(OwnerState::Starting)
    }

    /// A gate whose owner already answered (tests; the harness's fixture
    /// owner is ready before its window mounts).
    #[must_use]
    pub fn ready(key: VersionedRoot, mode: ServiceMode) -> Self {
        Self::with(OwnerState::Ready { key, mode })
    }

    fn with(state: OwnerState) -> Self {
        Self(Arc::new(Shared {
            inner: Mutex::new(Inner {
                state,
                epoch: Epoch::default(),
                attachment: Epoch::default(),
                mutation_generation: Some(MutationGeneration(0)),
                mutation_cancel: super::actor::CancellationToken::new(),
                publication: None,
                observation_cancel: super::actor::CancellationToken::new(),
                observation_suspended: false,
                fresh_publication: None,
                waker: None,
                restart: false,
                retry_enabled: true,
                since: Instant::now(),
                closed: false,
            }),
            changed: Condvar::new(),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The owner's state now.
    #[must_use]
    pub fn state(&self) -> OwnerState {
        self.lock().state.clone()
    }

    /// Identity of this gate, independent of diagnostic root observations.
    pub(crate) fn same_gate(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn mutation_generation(&self, expected: Option<Epoch>) -> Option<(MutationGeneration, super::actor::CancellationToken)> {
        let inner = self.lock();
        (!inner.closed && !inner.observation_suspended && Some(inner.attachment) == expected
            && matches!(inner.state, OwnerState::Ready { .. }))
            .then(|| inner.mutation_generation.map(|generation| (generation, inner.mutation_cancel.clone()))).flatten()
    }

    /// Read lifetime, readiness and its certified root under one lock.
    pub(crate) fn admit_mutation(&self, expected: MutationGeneration) -> MutationAdmission {
        let inner = self.lock();
        if inner.closed { return MutationAdmission::Unavailable(OwnerFault::Closed); }
        if let OwnerState::Failed(fault) = &inner.state {
            return MutationAdmission::Unavailable(fault.clone());
        }
        if inner.mutation_generation != Some(expected) { return MutationAdmission::Replaced; }
        match inner.state {
            OwnerState::Ready { .. } if inner.observation_suspended => MutationAdmission::Observing,
            OwnerState::Ready { key, .. } => MutationAdmission::Ready(key),
            OwnerState::Starting => MutationAdmission::Replaced,
            OwnerState::Failed(_) => unreachable!("failure returned above"),
        }
    }

    pub(crate) fn wait_mutation(&self, expected: MutationGeneration, cancel: &super::actor::CancellationToken) -> Result<VersionedRoot, Arc<str>> {
        let weak = Arc::downgrade(&self.0);
        let _wake = cancel.on_cancel(move || {
            if let Some(shared) = weak.upgrade() {
                let _inner = shared.inner.lock().unwrap_or_else(PoisonError::into_inner);
                shared.changed.notify_all();
            }
        });
        let waiting = Instant::now();
        let mut inner = self.lock();
        loop {
            if cancel.is_cancelled() || inner.closed {
                return Err("This index request stopped before its first send. Nothing was sent.".into());
            }
            if inner.mutation_generation != Some(expected) {
                return Err("The local service owner was replaced before this request's first send. Nothing was sent.".into());
            }
            match &inner.state {
                OwnerState::Ready { key, .. } if !inner.observation_suspended => return Ok(*key),
                OwnerState::Ready { .. } => {}
                OwnerState::Starting => return Err("The local service owner is starting again. Nothing was sent.".into()),
                OwnerState::Failed(fault) => return Err(format!("The local service is unavailable: {fault}. Nothing was sent.").into()),
            }
            let left = PATIENCE.saturating_sub(waiting.elapsed());
            if left.is_zero() { return Err("The index observation did not become ready before the first-send wait expired. Nothing was sent.".into()); }
            inner = self.0.changed.wait_timeout(inner, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Generation of the attached owner currently answering. A page worker
    /// must retain this before its request so an old failure cannot fail a
    /// newly attached owner with the same root.
    #[must_use]
    pub(crate) fn attached_ready_epoch(&self) -> Option<Epoch> {
        let inner = self.lock();
        (!inner.observation_suspended && matches!(inner.state, OwnerState::Ready { mode: ServiceMode::Attached, .. })).then_some(inner.attachment)
    }

    /// Read readiness and attachment together. Separate state/epoch reads
    /// could combine two different owners during a rapid same-root restart.
    pub(crate) fn serves_attachment(&self, expected: Option<Epoch>) -> bool {
        let inner = self.lock();
        !inner.closed && !inner.observation_suspended
            && matches!(inner.state, OwnerState::Ready { .. }) && Some(inner.attachment) == expected
    }

    /// Reports confirmed endpoint loss only for the attached generation that
    /// issued the failed read. This is atomic with the epoch/state check.
    pub(crate) fn attached_lost_at(&self, expected: Epoch, reason: Arc<str>) -> bool {
        let (waker, cancel, mutation_cancel) = {
            let mut inner = self.lock();
            if inner.closed || inner.attachment != expected
                || !matches!(inner.state, OwnerState::Ready { mode: ServiceMode::Attached, .. })
            {
                return false;
            }
            inner.state = OwnerState::Failed(OwnerFault::Lost(Arc::clone(&reason)));
            inner.since = Instant::now();
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.mutation_generation = inner.mutation_generation
                .and_then(|generation| generation.0.checked_add(1).map(MutationGeneration));
            let mutation_cancel = std::mem::replace(&mut inner.mutation_cancel, super::actor::CancellationToken::new());
            inner.publication = None;
            let cancel = inner.observation_cancel.clone();
            (inner.waker.take(), cancel, mutation_cancel)
        };
        // Callbacks may reenter the gate; never invoke them under its mutex.
        cancel.cancel();
        mutation_cancel.cancel();
        self.0.changed.notify_all();
        if let Some(waker) = waker { waker.wake(); }
        crate::runtime::trace::mark("owner.failed", OwnerFault::Lost(reason));
        true
    }

    /// Publishes a new state: wakes every waiting worker and the UI.
    pub fn publish(&self, state: OwnerState) {
        let (waker, cancel, mutation_cancel) = {
            let mut inner = self.lock();
            if inner.closed {
                return;
            }
            match &state {
                OwnerState::Ready { .. } => crate::runtime::trace::mark("owner.ready", "gate"),
                OwnerState::Failed(fault) => crate::runtime::trace::mark("owner.failed", fault),
                OwnerState::Starting => crate::runtime::trace::mark("owner.starting", "gate"),
            }
            let cancel = inner.observation_cancel.clone();
            inner.observation_cancel = super::actor::CancellationToken::new();
            inner.publication = None;
            inner.observation_suspended = false;
            inner.fresh_publication = None;
            inner.state = state;
            inner.since = Instant::now();
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.mutation_generation = inner.mutation_generation
                .and_then(|generation| generation.0.checked_add(1).map(MutationGeneration));
            let mutation_cancel = std::mem::replace(&mut inner.mutation_cancel, super::actor::CancellationToken::new());
            (inner.waker.take(), cancel, mutation_cancel)
        };
        // Callbacks may reenter the gate; never invoke them under its mutex.
        cancel.cancel();
        mutation_cancel.cancel();
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn event_is_current(&self, event: Epoch) -> bool {
        self.lock().epoch == event
    }

    /// Admits a UI wake only while that exact publication is still current.
    fn ready_at(&self, publication: Epoch, key: VersionedRoot) -> Option<Epoch> {
        let inner = self.lock();
        if inner.closed || inner.observation_suspended || inner.epoch != publication {
            return None;
        }
        matches!(inner.state, OwnerState::Ready { key: current, .. } if current.same_authority(key))
            .then_some(inner.attachment)
    }

    /// A complete root proof for this exact UI wake and attachment.
    fn certified_at(&self, publication: Epoch, key: VersionedRoot, attachment: Epoch) -> bool {
        let inner = self.lock();
        !inner.closed
            && !inner.observation_suspended
            && inner.epoch == publication
            && inner.attachment == attachment
            && inner.fresh_publication == Some(attachment)
            && matches!(inner.state, OwnerState::Ready { key: current, .. } if current.same_authority(key))
            && inner.publication.as_ref().is_some_and(|(view, cursor)| {
                *cursor == key.revision()
                    && view.root() == cursor.root()
                    && view.is_coherent()
                    && view.capability().is_some()
            })
    }

    /// Current serving attachment, independent of publication wakes.
    pub(crate) fn ready_epoch(&self) -> Option<Epoch> {
        let inner = self.lock();
        (!inner.closed
            && !inner.observation_suspended
            && matches!(inner.state, OwnerState::Ready { .. }))
        .then_some(inner.attachment)
    }

    /// Configuration export belongs to this process's live embedded owner.
    /// Read mode, readiness and closure together so replacement cannot combine
    /// an old embedded mode with a new attached owner's readiness.
    pub(crate) fn embedded_ready(&self) -> bool {
        let inner = self.lock();
        !inner.closed && !inner.observation_suspended
            && matches!(inner.state, OwnerState::Ready { mode: ServiceMode::Embedded, .. })
    }

    pub(crate) fn observation_scope(
        &self,
        expected: Epoch,
    ) -> Option<super::actor::CancellationToken> {
        let inner = self.lock();
        (!inner.closed
            && inner.attachment == expected
            && matches!(inner.state, OwnerState::Ready { .. }))
        .then(|| inner.observation_cancel.clone())
    }

    /// A complete root shared by the actor and observer. No UI serialization.
    pub(crate) fn publication(
        &self,
        expected: Epoch,
    ) -> Option<(Arc<backend_library::ViewRoot>, backend_library::Cursor)> {
        let inner = self.lock();
        if inner.closed
            || inner.attachment != expected
            || !matches!(inner.state, OwnerState::Ready { .. })
        {
            return None;
        }
        inner
            .publication
            .as_ref()
            .map(|(root, cursor)| (Arc::clone(root), *cursor))
    }

    /// Publishes only admitted producer state for the exact serving attachment.
    /// One slot coalesces updates; repeated authority keeps resources healthy.
    pub(crate) fn publish_view(
        &self,
        expected: Epoch,
        view: Arc<backend_library::ViewRoot>,
        cursor: backend_library::Cursor,
    ) -> PublicationAdmission {
        let waker = {
            let mut inner = self.lock();
            if inner.closed || inner.attachment != expected {
                return PublicationAdmission::Withdrawn;
            }
            if view.root() != cursor.root() || !view.is_coherent() || view.capability().is_none() {
                return PublicationAdmission::Invalid;
            }
            let OwnerState::Ready { key, mode } = inner.state else {
                return PublicationAdmission::Withdrawn;
            };
            let next =
                VersionedRoot::from_revision(key.producer_epoch(), cursor, key.observation());
            // Only the admitted same-stream lease may advance this attachment.
            if cursor.recipe() != key.revision().recipe()
                || cursor.branch() != key.revision().branch()
                || cursor.log() != key.revision().log()
                || cursor.schema() != key.revision().schema()
            {
                return PublicationAdmission::Invalid;
            }
            if cursor.sequence() < key.revision().sequence() {
                return PublicationAdmission::Obsolete;
            }
            if cursor.sequence() == key.revision().sequence() && cursor != key.revision() {
                return PublicationAdmission::Invalid;
            }
            let changed = !next.same_authority(key);
            let first_proof = inner.fresh_publication != Some(expected);
            inner.fresh_publication = Some(expected);
            inner.publication = Some((view, cursor));
            if !changed && !first_proof {
                self.0.changed.notify_all();
                return PublicationAdmission::Admitted;
            }
            inner.state = OwnerState::Ready { key: next, mode };
            if inner.observation_suspended {
                return PublicationAdmission::Admitted;
            }
            inner.epoch = inner.epoch.next();
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
        PublicationAdmission::Admitted
    }

    /// Withdraws serving reads when the current producer exchange has not
    /// confirmed freshness, while allowing that one exchange to finish.
    /// The attachment is the read fence; the observation cancellation token
    /// belongs to the in-flight socket and is deliberately retained. Only a
    /// fresh certified publication may reopen the new attachment.
    pub(crate) fn suspend_observation(&self, expected: Epoch) -> Option<Epoch> {
        let (attachment, waker) = {
            let mut inner = self.lock();
            if inner.closed || inner.attachment != expected
                || inner.observation_cancel.is_cancelled()
                || !matches!(inner.state, OwnerState::Ready { .. })
            {
                return None;
            }
            if inner.observation_suspended {
                return Some(expected);
            }
            inner.observation_suspended = true;
            inner.fresh_publication = None;
            inner.since = Instant::now();
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            (inner.attachment, inner.waker.take())
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker { waker.wake(); }
        Some(attachment)
    }

    /// A rejected producer lease immediately withdraws readiness and rotates
    /// the attachment. Conservatively fence in-flight reads, preserving the exact
    /// admitted root and all healthy values already loaded at that authority.
    pub(crate) fn replace_observation(
        &self,
        expected: Epoch,
    ) -> Option<(Epoch, super::actor::CancellationToken)> {
        let (attachment, cancel, waker, retired) = {
            let mut inner = self.lock();
            if inner.closed
                || inner.attachment != expected
                || !matches!(inner.state, OwnerState::Ready { .. })
            {
                return None;
            }
            let retired = inner.observation_cancel.clone();
            inner.observation_cancel = super::actor::CancellationToken::new();
            inner.observation_suspended = true;
            inner.fresh_publication = None;
            inner.since = Instant::now();
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            (
                inner.attachment,
                inner.observation_cancel.clone(),
                inner.waker.take(),
                retired,
            )
        };
        retired.cancel();
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
        Some((attachment, cancel))
    }

    /// Fresh complete admission reopens the suspended serving attachment.
    pub(crate) fn complete_observation(&self, expected: Epoch) -> bool {
        let waker = {
            let mut inner = self.lock();
            if inner.closed
                || inner.attachment != expected
                || !inner.observation_suspended
                || inner.fresh_publication != Some(expected)
                || !matches!(inner.state, OwnerState::Ready { .. })
            {
                return false;
            }
            inner.observation_suspended = false;
            inner.fresh_publication = None;
            inner.epoch = inner.epoch.next();
            inner.waker.take()
        };
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    /// Worker-only timed wait, interruptible by closure or attachment change.
    pub(crate) fn observation_pause(&self, expected: Epoch, duration: Duration) -> bool {
        let inner = self.lock();
        if inner.closed || inner.attachment != expected || inner.observation_cancel.is_cancelled() {
            return false;
        }
        let (inner, _) = self
            .0
            .changed
            .wait_timeout(inner, duration)
            .unwrap_or_else(PoisonError::into_inner);
        !inner.closed && inner.attachment == expected && !inner.observation_cancel.is_cancelled()
    }

    pub(crate) fn observation_failed(&self, expected: Epoch, reason: ObservationFailure) -> bool {
        // The publication worker is the only writer until this attachment is
        // withdrawn. Use one lock rather than a check followed by `publish`.
        let category = reason.category();
        let (waker, cancel, mutation_cancel) = {
            let mut inner = self.lock();
            if inner.closed
                || inner.attachment != expected
                || !matches!(inner.state, OwnerState::Ready { .. })
            {
                return false;
            }
            inner.state = OwnerState::Failed(OwnerFault::Observation(reason));
            let cancel = inner.observation_cancel.clone();
            inner.publication = None;
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.mutation_generation = inner.mutation_generation
                .and_then(|generation| generation.0.checked_add(1).map(MutationGeneration));
            let mutation_cancel = std::mem::replace(&mut inner.mutation_cancel, super::actor::CancellationToken::new());
            (inner.waker.take(), cancel, mutation_cancel)
        };
        crate::runtime::trace::mark("observation.failed", category);
        // Callbacks may reenter the gate; never invoke them under its mutex.
        cancel.cancel();
        mutation_cancel.cancel();
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    /// Blocks the calling **worker** thread until the owner answered.
    ///
    /// # Errors
    /// The owner's failure, in its own words; also when the app quits
    /// before it answered.
    pub fn wait(&self) -> Result<(), OwnerFault> {
        self.wait_while(PATIENCE, None)
    }

    /// Waits on a worker for the owner, waking immediately when this exact
    /// request is cancelled. The callback locks the gate before signalling
    /// its condition variable, so cancellation cannot fall between the
    /// predicate check and the wait.
    pub(crate) fn wait_cancelled(&self, cancel: &super::actor::CancellationToken) -> Result<(), OwnerFault> {
        let weak = Arc::downgrade(&self.0);
        let _wake = cancel.on_cancel(move || {
            if let Some(shared) = weak.upgrade() {
                let _inner = shared.inner.lock().unwrap_or_else(PoisonError::into_inner);
                shared.changed.notify_all();
            }
        });
        self.wait_while(PATIENCE, Some(cancel))
    }

    /// [`Self::wait`] with its own patience: gives up with
    /// [`OwnerFault::Silent`] once the owner has been starting for `patience`.
    ///
    /// # Errors
    /// The owner's fault; the window closing; or the patience running out.
    pub fn wait_for(&self, patience: Duration) -> Result<(), OwnerFault> {
        self.wait_while(patience, None)
    }

    fn wait_while(&self, patience: Duration, cancel: Option<&super::actor::CancellationToken>) -> Result<(), OwnerFault> {
        let mut inner = self.lock();
        loop {
            if cancel.is_some_and(super::actor::CancellationToken::is_cancelled) {
                return Err(OwnerFault::Cancelled);
            }
            match &inner.state {
                OwnerState::Ready { .. } if !inner.observation_suspended => return Ok(()),
                OwnerState::Failed(fault) => return Err(fault.clone()),
                OwnerState::Starting if inner.closed => return Err(OwnerFault::Closed),
                OwnerState::Starting | OwnerState::Ready { .. } => {
                    let left = patience.saturating_sub(inner.since.elapsed());
                    if left.is_zero() {
                        return Err(OwnerFault::Silent(patience));
                    }
                    inner = self
                        .0
                        .changed
                        .wait_timeout(inner, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
    }

    /// Withdraw retry capability when no producer can consume it. All cloned
    /// handles observe this under the same lock as the owner state/epoch.
    pub(crate) fn disable_restart(&self) {
        let mut inner = self.lock();
        inner.retry_enabled = false;
        inner.restart = false;
    }

    /// Current capability and failure generation, admitted under one lock.
    pub(crate) fn retry_generation(&self) -> Option<RetryGeneration> {
        let inner = self.lock();
        (inner.retry_enabled && !inner.closed && matches!(inner.state, OwnerState::Failed(_)))
            .then_some(RetryGeneration(inner.epoch))
    }

    pub(crate) fn can_retry_current(&self) -> bool { self.retry_generation().is_some() }

    /// A delayed UI callback cannot retry a later same-root failure.
    pub(crate) fn restart_at(&self, expected: RetryGeneration) -> bool {
        self.restart_current(Some(expected))
    }

    /// Asks a failed owner to try again. Returns whether it was asked: a
    /// starting or answering owner is left alone.
    #[must_use]
    pub fn restart(&self) -> bool { self.restart_current(None) }

    fn restart_current(&self, expected: Option<RetryGeneration>) -> bool {
        let (cancel, waker, mutation_cancel) = {
            let mut inner = self.lock();
            if !inner.retry_enabled || inner.closed || !matches!(inner.state, OwnerState::Failed(_))
                || expected.is_some_and(|expected| expected.0 != inner.epoch) {
                return false;
            }
            // Capability, request, and Starting are one admission. A cloned
            // handle cannot disable the producer between checking and publish.
            inner.restart = true;
            let cancel = std::mem::replace(&mut inner.observation_cancel, super::actor::CancellationToken::new());
            inner.publication = None;
            inner.observation_suspended = false;
            inner.state = OwnerState::Starting;
            inner.since = Instant::now();
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.mutation_generation = inner.mutation_generation
                .and_then(|generation| generation.0.checked_add(1).map(MutationGeneration));
            let mutation_cancel = std::mem::replace(&mut inner.mutation_cancel, super::actor::CancellationToken::new());
            (cancel, inner.waker.take(), mutation_cancel)
        };
        // Cancellation may synchronously call back into this same gate.
        // Keep the atomic admission above, then release the lock first.
        cancel.cancel();
        mutation_cancel.cancel();
        crate::runtime::trace::mark("owner.starting", "retry");
        self.0.changed.notify_all();
        if let Some(waker) = waker { waker.wake(); }
        true
    }

    /// The owner thread, after a failure: blocks until a restart is asked
    /// for (`true`) or the app quits (`false`).
    pub(crate) fn await_restart(&self) -> bool {
        let mut inner = self.lock();
        loop {
            if inner.closed {
                return false;
            }
            if std::mem::take(&mut inner.restart) {
                return true;
            }
            inner = self
                .0
                .changed
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The owner thread, once serving: returns `true` when a failed attached
    /// owner was explicitly retried, or `false` when the app closes.
    pub(crate) fn await_close_or_restart(&self) -> bool {
        let mut inner = self.lock();
        loop {
            if inner.closed { return false; }
            if std::mem::take(&mut inner.restart) { return true; }
            inner = self
                .0
                .changed
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The app is quitting: releases the owner thread and every waiter.
    pub fn close(&self) {
        let (waker, cancel, mutation_cancel) = {
            let mut inner = self.lock();
            inner.closed = true;
            let cancel = inner.observation_cancel.clone();
            inner.publication = None;
            inner.state = OwnerState::Failed(OwnerFault::Closed);
            inner.epoch = inner.epoch.next();
            inner.attachment = inner.epoch;
            inner.mutation_generation = inner.mutation_generation
                .and_then(|generation| generation.0.checked_add(1).map(MutationGeneration));
            let mutation_cancel = std::mem::replace(&mut inner.mutation_cancel, super::actor::CancellationToken::new());
            (inner.waker.take(), cancel, mutation_cancel)
        };
        // Callbacks may reenter the gate; never invoke them under its mutex.
        cancel.cancel();
        mutation_cancel.cancel();
        self.0.changed.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Resolves with the first state published after epoch `seen` (at once
    /// when one already was), and that state's epoch.
    pub fn next(&self, seen: Epoch) -> impl Future<Output = (Epoch, OwnerState)> + 'static {
        let gate = self.clone();
        std::future::poll_fn(move |task| {
            let mut inner = gate.lock();
            if inner.epoch > seen {
                Poll::Ready((inner.epoch, inner.state.clone()))
            } else {
                inner.waker = Some(task.waker().clone());
                Poll::Pending
            }
        })
    }
}

impl OwnerGate {
    /// The current state's epoch and whether it is a start still being
    /// waited for.
    fn starting_at(&self) -> Option<Epoch> {
        let inner = self.lock();
        matches!(inner.state, OwnerState::Starting).then_some(inner.epoch)
    }

    /// Publishes [`OwnerFault::Silent`] if the start that began at `epoch`
    /// is still the state (a later publish is another start, or an answer).
    fn give_up_on(&self, epoch: Epoch, patience: Duration) {
        if self.starting_at() == Some(epoch) {
            self.publish(OwnerState::Failed(OwnerFault::Silent(patience)));
        }
    }
}

/// After `patience`, a start nobody answered is a fault on the gate itself:
/// the store shows it and "Try again" restarts it.
fn watch_patience(gate: &OwnerGate, patience: Duration, cx: &mut App) {
    let Some(epoch) = gate.starting_at() else { return };
    let gate = gate.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(patience).await;
        gate.give_up_on(epoch, patience);
    })
    .detach();
}

/// Turns every published owner state into the window's data events, on the
/// UI thread, for as long as the window's root and store live (D2). The task
/// holds them weakly: a window that is let go is not kept alive by its owner's
/// watch, and an app that is dropped leaks no handle.
pub(crate) fn watch(
    gate: OwnerGate,
    root: &Entity<super::UiRootEntity>,
    store: &Entity<super::store::DataStore>,
    cx: &mut App,
) {
    watch_patience(&gate, PATIENCE, cx);
    let (root, store) = (root.downgrade(), store.downgrade());
    cx.spawn(async move |cx| {
        let mut seen = Epoch::default();
        let mut serving = None;
        let mut resources_serving = None;
        let mut recovery_requires_proof = false;
        loop {
            let (epoch, state) = gate.next(seen).await;
            seen = epoch;
            let alive = cx.update(|cx| {
                let (Some(root), Some(store)) = (root.upgrade(), store.upgrade()) else {
                    return false;
                };
                if !gate.event_is_current(epoch) {
                    return true;
                }
                match state {
                    OwnerState::Starting => {
                        // A restart is a new wait: its patience starts now.
                        watch_patience(&gate, PATIENCE, cx);
                        root.update(cx, |root, cx| root.owner_starting(cx));
                        store.update(cx, super::store::DataStore::owner_starting);
                    }
                    OwnerState::Ready { key, mode } => {
                        let Some(attachment) = gate.ready_at(epoch, key) else {
                            return true;
                        };
                        if serving.is_some() {
                            root.update(cx, |root, cx| root.renew_owner(key, mode, Some(attachment) != serving, cx));
                        } else {
                            root.update(cx, |root, cx| root.admit_owner(key, mode, cx));
                        }
                        // Responding to a revision probe does not clear a
                        // previous failed reading. Recovery needs a complete
                        // root proved at this exact wake and attachment.
                        if Some(attachment) != resources_serving
                            && (!recovery_requires_proof || gate.certified_at(epoch, key, attachment))
                        {
                            store.update(cx, super::store::DataStore::owner_ready);
                            resources_serving = Some(attachment);
                            recovery_requires_proof = false;
                        }
                        serving = Some(attachment);
                    }
                    OwnerState::Failed(OwnerFault::Closed) => return false,
                    OwnerState::Failed(fault) => {
                        recovery_requires_proof = true;
                        resources_serving = None;
                        root.update(cx, |root, cx| root.owner_unavailable(cx));
                        store.update(cx, |store, cx| store.owner_failed(&fault, cx));
                    }
                }
                true
            });
            if !alive {
                break;
            }
        }
    })
    .detach();
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_worker_waits_for_the_owner_and_learns_its_answer() {
        let gate = OwnerGate::starting();
        let (sent, received) = mpsc::channel();
        let worker = gate.clone();
        let thread = std::thread::spawn(move || sent.send(worker.wait()).expect("send"));
        assert!(
            received.recv_timeout(Duration::from_millis(80)).is_err(),
            "a worker must not pass the gate while the owner is starting"
        );
        gate.publish(OwnerState::Failed("could not own the workspace".into()));
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).expect("released"),
            Err(OwnerFault::from("could not own the workspace"))
        );
        thread.join().expect("join");
        assert!(gate.restart(), "a failed owner can be asked again");
        assert_eq!(gate.state(), OwnerState::Starting);
        assert!(gate.await_restart(), "the owner thread sees the restart");
        assert!(!gate.restart(), "a starting owner is left alone");
        gate.publish(OwnerState::Ready {
            key: VersionedRoot::unserved(),
            mode: ServiceMode::Attached,
        });
        assert_eq!(gate.wait(), Ok(()));
    }

    #[test]
    fn cancelling_a_worker_wakes_the_owner_gate_without_waiting_for_patience() {
        let gate = OwnerGate::starting();
        let cancel = super::super::actor::CancellationToken::new();
        let worker_gate = gate.clone();
        let worker_cancel = cancel.clone();
        let (sent, received) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            sent.send(worker_gate.wait_cancelled(&worker_cancel)).expect("send wait result");
        });
        cancel.cancel();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).expect("cancellation woke worker"), Err(OwnerFault::Cancelled));
        thread.join().expect("join worker");
        assert_eq!(gate.state(), OwnerState::Starting, "one revoked request cannot fail the owner");
    }

    #[test]
    fn retry_cancellation_can_reenter_the_same_gate_after_atomic_starting_admission() {
        let gate = OwnerGate::starting();
        gate.publish(OwnerState::Failed("restart this worker".into()));
        let cancellation = gate.lock().observation_cancel.clone();
        let callback_gate = gate.clone();
        let (observed, observation) = mpsc::channel();
        let _wake = cancellation.on_cancel(move || {
            // These take the actual OwnerGate mutex synchronously. A try_lock
            // assertion would not exercise the production callback contract.
            observed.send((callback_gate.state(), callback_gate.ready_epoch())).expect("callback observation");
        });
        let retry_gate = gate.clone();
        let (done, result) = mpsc::channel();
        let retry = std::thread::spawn(move || { done.send(retry_gate.restart()).expect("restart result"); });
        let (state, serving) = observation.recv_timeout(Duration::from_secs(1)).expect("cancellation reentered without deadlocking");
        assert_eq!(state, OwnerState::Starting);
        assert_eq!(serving, None);
        assert!(result.recv_timeout(Duration::from_secs(1)).expect("restart completed"));
        retry.join().expect("restart thread");
    }

    #[test]
    fn closing_releases_the_owner_thread_and_every_waiter() {
        let gate = OwnerGate::starting();
        let owner = gate.clone();
        let thread = std::thread::spawn(move || owner.await_close_or_restart());
        gate.close();
        assert!(!thread.join().expect("the owner thread lets go"));
        assert!(gate.wait().is_err(), "nobody waits on an owner after quit");
        assert!(!gate.await_restart());
    }

    #[test]
    fn the_ui_future_resolves_on_each_new_state_only() {
        let gate = OwnerGate::starting();
        let waker = Waker::noop();
        let mut task = std::task::Context::from_waker(waker);
        let mut first = Box::pin(gate.next(Epoch::default()));
        assert!(first.as_mut().poll(&mut task).is_pending(), "nothing was published yet");
        gate.publish(OwnerState::Failed("no".into()));
        let Poll::Ready((epoch, state)) = first.as_mut().poll(&mut task) else {
            panic!("a publish resolves the watcher");
        };
        assert_eq!(state, OwnerState::Failed("no".into()));
        let mut second = Box::pin(gate.next(epoch));
        assert!(second.as_mut().poll(&mut task).is_pending(), "a seen state never resolves twice");
    }

    #[test]
    fn an_owner_that_never_answers_is_a_typed_fault_after_the_patience_not_a_wait_for_ever() {
        let gate = OwnerGate::starting();
        let started = Instant::now();
        assert_eq!(gate.wait_for(Duration::from_millis(60)), Err(OwnerFault::Silent(Duration::from_millis(60))));
        assert!(started.elapsed() >= Duration::from_millis(60), "it waited the patience, no less");
        assert_eq!(
            OwnerFault::Silent(Duration::from_mins(1)).to_string(),
            "the index did not answer within 60 s",
            "and the window says it in words"
        );
        // A start that answers inside the patience is not a fault.
        let answering = OwnerGate::starting();
        let owner = answering.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            owner.publish(OwnerState::Ready { key: VersionedRoot::unserved(), mode: ServiceMode::Attached });
        });
        assert_eq!(answering.wait_for(Duration::from_secs(5)), Ok(()));
        thread.join().expect("join");
    }

    #[test]
    fn giving_up_names_only_the_start_it_was_waiting_for() {
        let gate = OwnerGate::starting();
        let epoch = gate.starting_at().expect("starting");
        gate.publish(OwnerState::Ready { key: VersionedRoot::unserved(), mode: ServiceMode::Attached });
        gate.give_up_on(epoch, PATIENCE);
        assert!(matches!(gate.state(), OwnerState::Ready { .. }), "an owner that answered is never given up on");
        gate.publish(OwnerState::Failed("gone".into()));
        assert!(gate.restart());
        gate.give_up_on(epoch, PATIENCE);
        assert_eq!(gate.state(), OwnerState::Starting, "the old wait does not fail the new start");
        let now = gate.starting_at().expect("starting again");
        gate.give_up_on(now, PATIENCE);
        assert_eq!(gate.state(), OwnerState::Failed(OwnerFault::Silent(PATIENCE)));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod publication_tests {
    use super::*;
    use backend_library::{
        AuthorityScopeClaim, Basis, CoverageCapability, Cursor, Frontier,
        ProducerObservationClaims, ProducerObservationVerifier, Row, RowId, ScopeRoot,
        UntrustedProducerObservation, ViewDelta, ViewRoot, admit_complete_scope,
        admit_producer_observation, object_version, symbol_key, view_key, view_state_root,
    };

    struct FixtureVerifier;
    impl ProducerObservationVerifier for FixtureVerifier {
        type Error = &'static str;
        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.evidence() != b"publication-fixture" {
                return Err("wrong fixture evidence");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }
    pub(crate) fn view() -> Arc<ViewRoot> {
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
        .expect("fixture observation");
        let coverage = CoverageCapability::from_authorized_with_evidence(
            admit_complete_scope(
                AuthorityScopeClaim::from_object_version(basis.object),
                observation,
            )
            .expect("fixture complete scope"),
            b"publication-fixture".to_vec(),
        )
        .expect("fixture coverage");
        Arc::new(
            ViewRoot::empty_checked(
                view_key(b"publication-view"),
                basis,
                Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
                coverage,
            )
            .expect("complete root"),
        )
    }
    fn attached(root: &ViewRoot) -> (OwnerGate, Epoch, Cursor) {
        let cursor = Cursor::for_view_root_at(root, 0);
        let gate = OwnerGate::ready(
            VersionedRoot::from_revision(1, cursor, 0),
            ServiceMode::Attached,
        );
        let attachment = gate.ready_epoch().expect("serving attachment");
        (gate, attachment, cursor)
    }

    #[test]
    fn external_publications_coalesce_without_reattaching_and_cannot_roll_back() {
        let first = view();
        let (gate, attachment, cursor) = attached(&first);
        assert_eq!(
            gate.publish_view(attachment, Arc::clone(&first), cursor),
            PublicationAdmission::Admitted
        );
        let initial_wake = gate.lock().epoch;
        let row = Row::new(
            RowId::Symbol(symbol_key("external")),
            first.basis(),
            "external publication",
        );
        let prepared = first
            .prepare(
                ViewDelta::Upsert { row },
                first.capability().expect("complete source"),
            )
            .expect("prepared external delta");
        let (second, _) = prepared.commit(&first).expect("committed external delta");
        let second = Arc::new(second);
        let published = Cursor::for_view_root_at(&second, 1);
        assert_eq!(
            gate.publish_view(attachment, Arc::clone(&second), published),
            PublicationAdmission::Admitted
        );
        assert!(gate.lock().epoch > initial_wake);
        assert_eq!(gate.attached_ready_epoch(), Some(attachment));
        assert!(
            gate.serves_attachment(Some(attachment)),
            "publication advances the observation epoch, not its read attachment"
        );
        let (kept, kept_cursor) = gate.publication(attachment).expect("latest shared root");
        assert!(Arc::ptr_eq(&kept, &second));
        assert_eq!(kept_cursor, published);
        assert_eq!(
            gate.publish_view(attachment, first, cursor),
            PublicationAdmission::Obsolete
        );
        assert_eq!(
            gate.publication(attachment).expect("not rolled back").1,
            published
        );
        assert_eq!(
            gate.ready_at(initial_wake, VersionedRoot::from_revision(1, cursor, 0)),
            None,
            "superseded UI wake is fenced"
        );
    }

    #[test]
    fn first_complete_root_wakes_recovery_without_accepting_a_stale_wake() {
        let root = view();
        let (gate, attachment, cursor) = attached(&root);
        let key = VersionedRoot::from_revision(1, cursor, 0);
        let responding_wake = gate.lock().epoch;
        assert_eq!(gate.ready_at(responding_wake, key), Some(attachment));
        assert!(!gate.certified_at(responding_wake, key, attachment));
        assert_eq!(
            gate.publish_view(attachment, Arc::clone(&root), cursor),
            PublicationAdmission::Admitted
        );
        let proof_wake = gate.lock().epoch;
        assert!(proof_wake > responding_wake);
        assert!(!gate.certified_at(responding_wake, key, attachment));
        assert!(gate.certified_at(proof_wake, key, attachment));
        assert!(gate.observation_failed(
            attachment,
            ObservationFailure::InvalidAuthority(ClientError::Protocol("fixture loss".into()))
        ));
        assert!(!gate.certified_at(proof_wake, key, attachment));
        assert!(gate.restart());
        gate.publish(OwnerState::Ready {
            key,
            mode: ServiceMode::Attached,
        });
        let replacement = gate.ready_epoch().expect("replacement attachment");
        let responding_wake = gate.lock().epoch;
        assert_ne!(replacement, attachment);
        assert!(!gate.certified_at(responding_wake, key, replacement));
        assert_eq!(
            gate.publish_view(attachment, root, cursor),
            PublicationAdmission::Withdrawn
        );
        assert!(!gate.certified_at(responding_wake, key, replacement));
    }

    #[test]
    fn same_authority_observation_does_not_wake_or_cancel_resources() {
        let root = view();
        let (gate, attachment, cursor) = attached(&root);
        let cancel = gate
            .observation_scope(attachment)
            .expect("observation scope");
        assert_eq!(gate.publish_view(attachment, Arc::clone(&root), cursor),
            PublicationAdmission::Admitted);
        let wake = gate.lock().epoch;
        for _ in 0..16 {
            assert_eq!(
                gate.publish_view(attachment, Arc::clone(&root), cursor),
                PublicationAdmission::Admitted
            );
        }
        assert_eq!(gate.lock().epoch, wake);
        assert_eq!(gate.attached_ready_epoch(), Some(attachment));
        assert!(!cancel.is_cancelled());
        let key = VersionedRoot::from_revision(1, cursor, 0).observed_at(99);
        assert_eq!(
            gate.ready_at(wake, key),
            Some(attachment),
            "observation metadata is not authority"
        );
    }

    #[test]
    fn daemon_restart_fences_pending_work_even_when_the_root_is_identical() {
        let root = view();
        let (gate, old, cursor) = attached(&root);
        let pending = gate.observation_scope(old).expect("old scope");
        assert!(gate.observation_failed(old, ObservationFailure::InvalidAuthority(ClientError::Protocol("expired publication lease after reconnect".into()))));
        assert!(pending.is_cancelled());
        assert!(gate.restart());
        gate.publish(OwnerState::Ready {
            key: VersionedRoot::from_revision(1, cursor, 0),
            mode: ServiceMode::Attached,
        });
        let new = gate.ready_epoch().expect("new attachment");
        assert_ne!(old, new);
        assert_eq!(
            gate.publish_view(old, Arc::clone(&root), cursor),
            PublicationAdmission::Withdrawn
        );
        assert!(!gate.observation_failed(old, ObservationFailure::InvalidAuthority(ClientError::Protocol("late failure".into()))));
        assert!(matches!(gate.state(), OwnerState::Ready { .. }));
        assert_eq!(
            gate.publish_view(new, root, cursor),
            PublicationAdmission::Admitted
        );
    }

    #[test]
    fn stalled_observation_fences_reads_without_cancelling_its_one_live_exchange() {
        let root = view();
        let (gate, old, cursor) = attached(&root);
        let io = gate.observation_scope(old).expect("active observation socket");
        assert_eq!(gate.publish_view(old, Arc::clone(&root), cursor), PublicationAdmission::Admitted);

        let suspended = gate.suspend_observation(old).expect("current attachment");
        assert_ne!(suspended, old);
        assert!(!io.is_cancelled(), "the one in-flight response must remain readable");
        assert_eq!(gate.ready_epoch(), None);
        assert_eq!(gate.attached_ready_epoch(), None);
        assert!(!gate.serves_attachment(Some(old)));
        assert!(!gate.serves_attachment(Some(suspended)));
        let (retained, retained_cursor) = gate.publication(suspended).expect("display-only complete root");
        assert!(Arc::ptr_eq(&retained, &root));
        assert_eq!(retained_cursor, cursor);
        assert_eq!(gate.suspend_observation(suspended), Some(suspended), "a second tick must not rotate again");
        assert!(!gate.complete_observation(suspended), "retained root alone is not fresh producer proof");
        assert_eq!(gate.publish_view(old, Arc::clone(&root), cursor), PublicationAdmission::Withdrawn);

        assert_eq!(gate.publish_view(suspended, root, cursor), PublicationAdmission::Admitted);
        assert!(gate.complete_observation(suspended), "a certified same-root response reopens this attachment");
        assert_eq!(gate.attached_ready_epoch(), Some(suspended));
        assert!(!io.is_cancelled());
    }

    #[test]
    fn suspended_observation_terminal_failure_cancels_its_socket_and_cannot_reopen() {
        let root = view();
        let (gate, old, cursor) = attached(&root);
        let io = gate.observation_scope(old).expect("active observation socket");
        assert_eq!(gate.publish_view(old, Arc::clone(&root), cursor), PublicationAdmission::Admitted);
        let suspended = gate.suspend_observation(old).expect("current attachment");
        assert!(gate.observation_failed(suspended, ObservationFailure::ResponseStalled {
            phase: LocalControlExchangePhase::ReadingHeader,
            elapsed: Duration::from_secs(1),
        }));
        assert!(io.is_cancelled(), "terminal failure must interrupt the socket");
        assert_eq!(gate.attached_ready_epoch(), None);
        assert!(gate.publication(suspended).is_none());
        assert_eq!(gate.publish_view(suspended, root, cursor), PublicationAdmission::Withdrawn);
        assert!(!gate.complete_observation(suspended));
    }

    #[test]
    fn replacement_socket_fences_pending_reads_but_keeps_healthy_authority() {
        let root = view();
        let (gate, old, cursor) = attached(&root);
        let pending = gate.observation_scope(old).expect("scope");
        assert_eq!(
            gate.publish_view(old, Arc::clone(&root), cursor),
            PublicationAdmission::Admitted
        );
        let (new, active) = gate
            .replace_observation(old)
            .expect("fresh admitted connection");
        assert_ne!(old, new);
        assert!(pending.is_cancelled());
        assert!(!active.is_cancelled());
        assert_eq!(gate.attached_ready_epoch(), None);
        assert!(!gate.complete_observation(new), "retained basis is not a fresh admission");
        assert_eq!(gate.publish_view(new, Arc::clone(&root), cursor), PublicationAdmission::Admitted);
        assert!(gate.complete_observation(new));
        assert_eq!(gate.attached_ready_epoch(), Some(new));
        let (kept, current) = gate.publication(new).expect("same healthy root");
        assert!(Arc::ptr_eq(&kept, &root));
        assert_eq!(current, cursor);
        assert!(
            matches!(gate.state(), OwnerState::Ready { key, .. } if key.same_authority(VersionedRoot::from_revision(1, cursor, 0)))
        );
        assert_eq!(
            gate.publish_view(old, root, cursor),
            PublicationAdmission::Withdrawn
        );
    }

    #[test]
    fn regressed_replacement_cannot_restore_serving_readiness() {
        let root = view();
        let cursor = Cursor::for_view_root_at(&root, 5);
        let gate = OwnerGate::ready(VersionedRoot::from_revision(7, cursor, 0), ServiceMode::Attached);
        let old = gate.ready_epoch().expect("attachment");
        assert_eq!(gate.publish_view(old, Arc::clone(&root), cursor), PublicationAdmission::Admitted);
        let (new, _) = gate.replace_observation(old).expect("replacement");
        assert!(!gate.complete_observation(new));
        let regressed = Cursor::for_view_root_at(&root, 4);
        assert_eq!(gate.publish_view(new, Arc::clone(&root), regressed), PublicationAdmission::Obsolete);
        assert!(!gate.complete_observation(new));
        assert_eq!(gate.ready_epoch(), None);
        assert_eq!(gate.publish_view(new, root, cursor), PublicationAdmission::Admitted);
        assert!(gate.complete_observation(new));
        assert!(matches!(gate.state(), OwnerState::Ready { key, .. } if key.producer_epoch() == 7));
    }

    #[test]
    fn cancellation_callbacks_can_reenter_the_gate() {
        let root = view();
        for transition in 0..5 {
            let (gate, attachment, _) = attached(&root);
            let scope = gate.observation_scope(attachment).expect("scope");
            let reentrant = gate.clone();
            let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let observed = Arc::clone(&called);
            let _wake = scope.on_cancel(move || {
                let _ = reentrant.state();
                observed.store(true, std::sync::atomic::Ordering::Release);
            });
            match transition {
                0 => gate.close(),
                1 => gate.publish(OwnerState::Starting),
                2 => { let _ = gate.replace_observation(attachment); }
                3 => { let _ = gate.observation_failed(attachment, ObservationFailure::InvalidAuthority(ClientError::Protocol("fixture".into()))); }
                _ => { let _ = gate.attached_lost_at(attachment, "fixture".into()); }
            }
            assert!(called.load(std::sync::atomic::Ordering::Acquire));
        }
    }

    #[test]
    fn teardown_releases_latest_root_and_cancels_worker_scope() {
        let root = view();
        let (gate, attachment, cursor) = attached(&root);
        let cancel = gate.observation_scope(attachment).expect("scope");
        assert_eq!(
            gate.publish_view(attachment, Arc::clone(&root), cursor),
            PublicationAdmission::Admitted
        );
        gate.close();
        assert!(cancel.is_cancelled());
        assert!(gate.publication(attachment).is_none());
        assert!(!gate.observation_pause(attachment, Duration::from_secs(10)));
        assert_eq!(
            gate.publish_view(attachment, root, cursor),
            PublicationAdmission::Withdrawn
        );
        gate.publish(OwnerState::Ready {
            key: VersionedRoot::from_revision(1, cursor, 0),
            mode: ServiceMode::Attached,
        });
        assert_eq!(
            gate.state(),
            OwnerState::Failed(OwnerFault::Closed),
            "late starter cannot revive a closed owner"
        );
    }
}
