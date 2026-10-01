//! Background engine/client actor.

use super::mailbox::{CoalesceKey, Coalescible, CoalescingMailbox, PushResult};
use super::wake::{WakeReceiver, WakeSender, wake_channel};
use crate::core::{ErrorValue, LocalProjectId, VersionedRoot};
use crate::model::local_package::{LocalPackage, LocalPackageLoader};
use crate::model::snapshot::{DeltaId, ObjectId, PackageSummary, ProjectState};
use crate::navigation::RequestId;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};

/// Cheap cancellation handle shared by a request and the worker.
#[derive(Clone)]
pub struct CancellationToken(Arc<CancellationState>);

struct CancellationState {
    cancelled: AtomicBool,
    next: AtomicU64,
    waiters: Mutex<Vec<(u64, Arc<dyn Fn() + Send + Sync>)>>,
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken").field("cancelled", &self.is_cancelled()).finish()
    }
}

/// A worker wait removes its wakeup when it ends. A token held through a
/// long page composition therefore cannot accumulate one callback per probe.
pub(crate) struct CancellationWake {
    state: Weak<CancellationState>,
    id: u64,
}

impl Drop for CancellationWake {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            state.waiters.lock().unwrap_or_else(PoisonError::into_inner)
                .retain(|(id, _)| *id != self.id);
        }
    }
}

impl CancellationToken {
    /// Creates a live token.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            next: AtomicU64::new(1),
            waiters: Mutex::new(Vec::new()),
        }))
    }

    /// Requests cancellation without waiting for the worker.
    pub fn cancel(&self) {
        if !self.0.cancelled.swap(true, Ordering::AcqRel) {
            let waiters = self.0.waiters.lock().unwrap_or_else(PoisonError::into_inner)
                .iter().map(|(_, wake)| Arc::clone(wake)).collect::<Vec<_>>();
            for wake in waiters { wake(); }
        }
    }

    /// Returns whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Registers one condition-variable wakeup for a worker wait. The
    /// callback fires immediately when cancellation already happened.
    pub(crate) fn on_cancel(&self, wake: impl Fn() + Send + Sync + 'static) -> CancellationWake {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let mut waiters = self.0.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        if self.is_cancelled() {
            drop(waiters);
            wake();
        } else {
            waiters.push((id, wake));
        }
        CancellationWake { state: Arc::downgrade(&self.0), id }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Typed work executed by the background engine/client actor.
#[derive(Clone, Debug)]
pub enum EngineRequest {
    /// Read a versioned root.
    Root {
        /// Request identity.
        request: RequestId,
        /// Producer root basis.
        basis: VersionedRoot,
        /// Cancellation state.
        cancel: CancellationToken,
    },
    /// Read one object at one exact delta.
    Object {
        /// Request identity.
        request: RequestId,
        /// Object identity.
        object: ObjectId,
        /// Delta identity.
        delta: DeltaId,
        /// Producer root basis.
        basis: VersionedRoot,
        /// Cancellation state.
        cancel: CancellationToken,
    },
    /// Read one daemon-owned product surface at the caller's exact root.
    Surface {
        /// Request identity.
        request: RequestId,
        /// Typed surface command.
        command: backend_library::SurfaceCommand,
        /// Producer root basis.
        basis: VersionedRoot,
        /// Cancellation state.
        cancel: CancellationToken,
    },
    /// Submit one project to the canonical service index command while the
    /// shared `ProjectIngest` receipt transport is unavailable.
    IndexProject {
        /// Request identity retained until the owner replies.
        request: RequestId,
        /// Native project identity admitted by the picker boundary.
        project: LocalProjectId,
        /// Producer root basis captured before submission.
        basis: VersionedRoot,
        /// Cancellation request; terminal state is still owner-driven.
        cancel: CancellationToken,
    },
}

impl EngineRequest {
    fn basis(&self) -> VersionedRoot {
        match self {
            Self::Root { basis, .. }
            | Self::Object { basis, .. }
            | Self::Surface { basis, .. }
            | Self::IndexProject { basis, .. } => *basis,
        }
    }

    pub(crate) fn request(&self) -> RequestId {
        match self {
            Self::Root { request, .. }
            | Self::Object { request, .. }
            | Self::Surface { request, .. }
            | Self::IndexProject { request, .. } => *request,
        }
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancellation().is_cancelled()
    }

    pub(crate) fn cancellation(&self) -> &CancellationToken {
        match self {
            Self::Root { cancel, .. }
            | Self::Object { cancel, .. }
            | Self::Surface { cancel, .. }
            | Self::IndexProject { cancel, .. } => cancel,
        }
    }
}

impl Coalescible for EngineRequest {
    fn coalesce_key(&self) -> Option<CoalesceKey> {
        match self {
            Self::Root { .. } => Some(CoalesceKey::Root),
            Self::Object { object, .. } => Some(CoalesceKey::Object(*object)),
            Self::Surface { command, .. } => Some(CoalesceKey::Surface(command.id())),
            Self::IndexProject { project, .. } => Some(CoalesceKey::Index(project.key())),
        }
    }
}

/// Filesystem-only work executed on the actor's local-read lane.
///
/// A local read never reaches the [`EngineClient`]: it is not producer work,
/// and running a bounded `cargo metadata` subprocess on the producer lane
/// would stall root and surface reads behind it.
#[derive(Clone, Debug)]
pub struct LocalRead {
    /// Request identity.
    pub request: RequestId,
    /// Local project whose manifests are read.
    pub project: LocalProjectId,
    /// Root current when the read was requested.
    pub basis: VersionedRoot,
    /// Cancellation state.
    pub cancel: CancellationToken,
}

impl Coalescible for LocalRead {
    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::LocalPackage(self.project.key()))
    }
}

/// DTO returned by an engine client. Mapping into [`AppSnapshot`](crate::model::AppSnapshot)
/// happens in `runtime::mapping`, outside widgets.
#[derive(Clone, Debug)]
pub enum EngineDto {
    /// A root response with a producer identity.
    Root {
        /// Request identity.
        request: RequestId,
        /// Basis that was requested.
        basis: VersionedRoot,
        /// New root key.
        key: VersionedRoot,
        /// Exact service cursor that issued `key`. UI code never advances
        /// this identity locally.
        revision: backend_library::Cursor,
        /// Optional transition identity.
        delta: Option<DeltaId>,
        /// Optional mapped project read model.
        project: Option<ProjectDto>,
        /// Live registry catalog, when the owner exposes it.
        catalog: Option<Arc<[PackageSummary]>>,
    },
    /// An object response tied to one delta.
    Object {
        /// Request identity.
        request: RequestId,
        /// Basis that was requested.
        basis: VersionedRoot,
        /// Object identity.
        object: ObjectId,
        /// Delta identity.
        delta: DeltaId,
    },
    /// A typed product surface response mapped at the runtime boundary.
    Surface {
        /// Request identity.
        request: RequestId,
        /// Basis that was requested.
        basis: VersionedRoot,
        /// Original typed command.
        command: backend_library::SurfaceCommand,
        /// Producer reply.
        reply: backend_library::SurfaceReply,
    },
    /// A committed project index and the exact post-commit root returned by
    /// the canonical service command.
    Index {
        /// Request identity.
        request: RequestId,
        /// Root authority used to submit the command.
        basis: VersionedRoot,
        /// New root key from the service revision.
        key: VersionedRoot,
        /// Exact service cursor that issued `key`.
        revision: backend_library::Cursor,
        /// Optional transition identity.
        delta: Option<DeltaId>,
        /// Local project whose served workspace changed.
        project: LocalProjectId,
        /// Project read model from the committed root.
        project_state: Option<ProjectDto>,
        /// Registry catalog from the same service session.
        catalog: Option<Arc<[PackageSummary]>>,
        /// Per-project file count when the service exposes one.
        files_indexed: Option<u64>,
    },
    /// Package facts read from a local project's own manifests.
    LocalPackage {
        /// Request identity.
        request: RequestId,
        /// Basis that was requested.
        basis: VersionedRoot,
        /// Loaded facts, keyed by their project.
        package: Arc<LocalPackage>,
    },
}

/// Engine-owned project DTO. It is deliberately not imported by widgets.
#[derive(Clone, Debug)]
pub struct ProjectDto {
    /// Canonical project identity.
    pub id: LocalProjectId,
    /// Display label.
    pub label: Arc<str>,
    /// Canonical package identities.
    pub packages: Arc<[crate::core::PackageId]>,
}

impl ProjectDto {
    /// Maps this transport DTO into the immutable read model.
    #[must_use]
    pub fn into_model(self) -> ProjectState {
        ProjectState {
            id: self.id,
            label: self.label,
            packages: self.packages,
        }
    }
}

/// Typed actor failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineFault {
    /// Request was cancelled or superseded.
    Cancelled,
    /// Worker refused an older producer root.
    Superseded,
    /// Client/transport failure with a typed, bounded diagnostic.
    Failed(ErrorValue),
    /// The compatibility ingest request failed before a committed root was
    /// admitted.
    IndexFailed {
        /// Project whose attempt failed.
        project: LocalProjectId,
        /// Bounded service/transport diagnostic.
        error: ErrorValue,
    },
    /// The compatibility ingest producer returned after a cancellation
    /// request; this is terminal owner observation, not a local paint event.
    IndexCancelled {
        /// Project whose attempt reached its terminal cancellation boundary.
        project: LocalProjectId,
    },
    /// The request may have reached the owner, but its terminal reply was
    /// interrupted; only a fresh owner observation can settle its outcome.
    IndexUnconfirmed {
        /// Project whose index outcome needs an owner refresh.
        project: LocalProjectId,
    },
    /// A mutating surface command may have reached the owner, but its reply
    /// was interrupted. It is never replayed automatically.
    MutationUnconfirmed,
}

/// Worker event consumed by the UI coordinator.
#[derive(Clone, Debug)]
pub struct EngineEvent {
    /// The original request basis.
    pub basis: VersionedRoot,
    /// Request identity.
    pub request: RequestId,
    /// Request lane used to coalesce a paused UI's result backlog.
    pub lane: Option<CoalesceKey>,
    /// DTO or typed rejection.
    pub result: Result<EngineDto, EngineFault>,
}

impl Coalescible for EngineEvent {
    fn coalesce_key(&self) -> Option<CoalesceKey> {
        self.lane.or_else(|| match &self.result {
            Ok(EngineDto::Root { .. }) => Some(CoalesceKey::Root),
            Ok(EngineDto::Object { object, .. }) => Some(CoalesceKey::Object(*object)),
            Ok(EngineDto::Surface { command, .. }) => Some(CoalesceKey::Surface(command.id())),
            Ok(EngineDto::Index { project, .. }) => Some(CoalesceKey::Index(project.key())),
            Ok(EngineDto::LocalPackage { package, .. }) => {
                Some(CoalesceKey::LocalPackage(package.project.key()))
            }
            Err(_) => None,
        })
    }
}

/// Synchronous client implementation executed only on the worker thread.
pub trait EngineClient: Send + 'static {
    /// Executes one typed request. This method may block the worker, never the UI.
    ///
    /// # Errors
    /// Returns the typed [`EngineFault`] the producer or transport reported.
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault>;
}

/// Handle for a dedicated background engine actor.
pub struct EngineActor {
    mailbox: CoalescingMailbox<EngineRequest>,
    local: CoalescingMailbox<LocalRead>,
    events: CoalescingMailbox<EngineEvent>,
    join: Option<JoinHandle<()>>,
    local_join: Option<JoinHandle<()>>,
    /// The synchronous producer request currently owned by the worker.
    /// Shutdown revokes it before joining, including when it awaits startup.
    active: Arc<Mutex<ActiveRequest>>,
    /// The local Cargo read currently held by the second worker lane.
    local_active: Arc<Mutex<ActiveRequest>>,
    /// Wakes the UI after every delivered event; closed on shutdown.
    wake: WakeSender,
    /// The UI half, taken once by the entity that drains events.
    wake_receiver: Option<WakeReceiver>,
}

#[derive(Default)]
struct ActiveRequest {
    closed: bool,
    cancel: Option<CancellationToken>,
}

/// Failure to create the dedicated worker thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActorStartError {
    /// Bounded operating-system diagnostic.
    message: Arc<str>,
}

impl ActorStartError {
    /// Bounds one thread-spawn failure into a typed error.
    pub(crate) fn from_spawn_error(error: &std::io::Error) -> Self {
        Self::from_spawn(error)
    }

    fn from_spawn(error: &std::io::Error) -> Self {
        let mut message = error.to_string();
        if message.len() > 256 {
            let mut end = 256;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        Self {
            message: Arc::from(message),
        }
    }

    /// Returns the bounded startup diagnostic.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ActorStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ActorStartError {}

impl std::fmt::Debug for EngineActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineActor")
            .field("queued", &self.mailbox.len())
            .finish_non_exhaustive()
    }
}

impl EngineActor {
    /// Starts a worker with bounded request and result mailboxes.
    ///
    /// # Errors
    /// Returns [`ActorStartError`] when a worker thread cannot start.
    pub fn start(client: impl EngineClient, capacity: usize) -> Result<Self, ActorStartError> {
        Self::start_with_loader(client, capacity, LocalPackageLoader::default())
    }

    /// Starts the producer lane and the local-read lane.
    ///
    /// Both lanes have their own bounded request mailbox and deliver into one
    /// bounded result mailbox, so the UI polls a single event stream.
    ///
    /// # Errors
    /// Returns [`ActorStartError`] when either worker thread cannot start.
    pub fn start_with_loader(
        client: impl EngineClient,
        capacity: usize,
        loader: LocalPackageLoader,
    ) -> Result<Self, ActorStartError> {
        let mailbox = CoalescingMailbox::new(capacity);
        let local = CoalescingMailbox::new(capacity);
        let events = CoalescingMailbox::new(capacity);
        let (wake, wake_receiver) = wake_channel();
        let worker_mailbox = mailbox.clone();
        let worker_events = events.clone();
        let worker_wake = wake.clone();
        let active = Arc::new(Mutex::new(ActiveRequest::default()));
        let worker_active = Arc::clone(&active);
        let local_active = Arc::new(Mutex::new(ActiveRequest::default()));
        let worker_local_active = Arc::clone(&local_active);
        let join = thread::Builder::new()
            .name("nudox-engine-actor".to_owned())
            .spawn(move || {
                run_actor(Box::new(client), &worker_mailbox, &worker_events, &worker_wake, &worker_active);
            })
            .map_err(|error| ActorStartError::from_spawn(&error))?;
        let local_mailbox = local.clone();
        let local_events = events.clone();
        let local_wake = wake.clone();
        let local_join = thread::Builder::new()
            .name("nudox-local-reads".to_owned())
            .spawn(move || run_local_reads(&loader, &local_mailbox, &local_events, &local_wake, &worker_local_active));
        let mut actor = Self {
            mailbox,
            local,
            events,
            join: Some(join),
            local_join: None,
            active,
            local_active,
            wake,
            wake_receiver: Some(wake_receiver),
        };
        match local_join {
            Ok(join) => actor.local_join = Some(join),
            // Dropping the actor closes and joins the producer lane.
            Err(error) => return Err(ActorStartError::from_spawn(&error)),
        }
        Ok(actor)
    }

    /// Submits one local read without waiting for lane capacity.
    #[must_use]
    pub fn try_submit_local(&self, read: LocalRead) -> PushResult<LocalRead> {
        let key = read.coalesce_key();
        let result = self.local.try_push(read, key);
        if let PushResult::Coalesced(old) = &result {
            old.cancel.cancel();
        }
        result
    }

    /// Submits without waiting for worker capacity.
    #[must_use]
    pub fn try_submit(&self, request: EngineRequest) -> PushResult<EngineRequest> {
        self.try_submit_coalesced(request)
    }

    /// Submits using the request's coalescing key.
    #[must_use]
    pub fn try_submit_coalesced(&self, request: EngineRequest) -> PushResult<EngineRequest> {
        let key = request.coalesce_key();
        let result = self.mailbox.try_push(request, key);
        if let PushResult::Coalesced(old) = &result {
            match old {
                EngineRequest::Root { cancel, .. }
                | EngineRequest::Object { cancel, .. }
                | EngineRequest::Surface { cancel, .. }
                | EngineRequest::IndexProject { cancel, .. } => cancel.cancel(),
            }
        }
        result
    }

    /// Drains currently available events without waiting.
    #[must_use]
    pub fn drain_events(&self) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// Returns the bounded number of results waiting for the UI.
    #[must_use]
    pub fn queued_events(&self) -> usize {
        self.events.len()
    }

    /// Takes the UI half of the event wake signal. The owner awaits it in one
    /// task and drains [`Self::drain_events`] when it resolves, so no frame is
    /// requested while work is merely in flight.
    pub fn take_wake(&mut self) -> Option<WakeReceiver> {
        self.wake_receiver.take()
    }

    /// Requests worker shutdown and joins it from the owning shutdown phase.
    ///
    /// Closing the bounded request and event lanes is the actor's one
    /// shutdown signal. The join is retained by the owner so an entity drop
    /// cannot leave a worker, client session, or socket closure alive after
    /// the UI state has gone away.
    pub fn shutdown(mut self) {
        self.close_and_join();
    }

    fn close_and_join(&mut self) {
        for state in [&self.active, &self.local_active] {
            let cancel = {
                let mut active = state.lock().unwrap_or_else(PoisonError::into_inner);
                active.closed = true;
                active.cancel.clone()
            };
            if let Some(cancel) = cancel { cancel.cancel(); }
        }
        self.mailbox.close();
        self.local.close();
        self.events.close();
        self.wake.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        if let Some(join) = self.local_join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for EngineActor {
    fn drop(&mut self) {
        self.close_and_join();
    }
}

fn run_actor(
    mut client: Box<dyn EngineClient>,
    mailbox: &CoalescingMailbox<EngineRequest>,
    events: &CoalescingMailbox<EngineEvent>,
    wake: &WakeSender,
    active: &Mutex<ActiveRequest>,
) {
    let mut newest: Option<VersionedRoot> = None;
    while let Some(request) = mailbox.recv() {
        let basis = request.basis();
        let id = request.request();
        let lane = request.coalesce_key();
        let index_lane = matches!(&request, EngineRequest::IndexProject { .. });
        if request.cancelled() {
            if !events.push_wait(
                EngineEvent {
                    basis,
                    request: id,
                    lane,
                    result: Err(cancelled_fault(&request)),
                },
                lane,
            ) {
                break;
            }
            wake.wake();
            continue;
        }
        if !index_lane && newest.is_some_and(|known| basis.is_older_authority(known)) {
            if !events.push_wait(
                EngineEvent {
                    basis,
                    request: id,
                    lane,
                    result: Err(EngineFault::Superseded),
                },
                lane,
            ) {
                break;
            }
            wake.wake();
            continue;
        }
        if !index_lane && newest.is_none_or(|known| known.is_older_authority(basis)) {
            newest = Some(basis);
        }
        // A read (not an index) counts as one the owner should answer before
        // the next package compile (`traffic`).
        let _reading = (!index_lane).then(super::traffic::Reading::begin);
        {
            let mut active = active.lock().unwrap_or_else(PoisonError::into_inner);
            if active.closed { request.cancellation().cancel(); }
            active.cancel = Some(request.cancellation().clone());
        }
        let result = if request.cancelled() {
            Err(cancelled_fault(&request))
        } else { match client.execute(&request) {
            // A cancellation request cannot revoke a synchronous producer
            // commit after it has returned. Admit that committed result; a
            // producer error after cancellation is terminal cancellation.
            Ok(dto) => Ok(dto),
            Err(error @ (EngineFault::IndexUnconfirmed { .. } | EngineFault::MutationUnconfirmed)) => Err(error),
            Err(_error) if request.cancelled() => Err(cancelled_fault(&request)),
            Err(error) => Err(error),
        }};
        active.lock().unwrap_or_else(PoisonError::into_inner).cancel = None;
        let event = EngineEvent {
            basis,
            request: id,
            lane,
            result,
        };
        let key = event.coalesce_key().or(lane);
        if !events.push_wait(event, key) {
            break;
        }
        wake.wake();
    }
}

/// Serves local reads until the lane closes.
///
/// Local facts are not producer-root state, so this lane applies no root
/// supersession; the coordinator still discards results it no longer owns.
fn run_local_reads(
    loader: &LocalPackageLoader,
    mailbox: &CoalescingMailbox<LocalRead>,
    events: &CoalescingMailbox<EngineEvent>,
    wake: &WakeSender,
    active: &Mutex<ActiveRequest>,
) {
    while let Some(read) = mailbox.recv() {
        {
            let mut active = active.lock().unwrap_or_else(PoisonError::into_inner);
            if active.closed { read.cancel.cancel(); }
            active.cancel = Some(read.cancel.clone());
        }
        let lane = read.coalesce_key();
        let result = if read.cancel.is_cancelled() {
            Err(EngineFault::Cancelled)
        } else {
            let package = loader.load_with_cancel(&read.project, &|| read.cancel.is_cancelled());
            match package {
                Some(package) if !read.cancel.is_cancelled() => Ok(EngineDto::LocalPackage {
                    request: read.request,
                    basis: read.basis,
                    package: Arc::new(package),
                }),
                Some(_) | None => Err(EngineFault::Cancelled),
            }
        };
        let event = EngineEvent {
            basis: read.basis,
            request: read.request,
            lane,
            result,
        };
        active.lock().unwrap_or_else(PoisonError::into_inner).cancel = None;
        if !events.push_wait(event, lane) {
            break;
        }
        wake.wake();
    }
}

fn cancelled_fault(request: &EngineRequest) -> EngineFault {
    if let EngineRequest::IndexProject { project, .. } = request {
        return EngineFault::IndexCancelled {
            project: project.clone(),
        };
    }
    EngineFault::Cancelled
}

#[cfg(test)]
mod tests {
    use super::{ActorStartError, CancellationToken, EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest, LocalRead};
    use crate::core::{LocalProjectId, VersionedRoot};
    use crate::model::local_package::LocalPackageLoader;
    use crate::navigation::RequestId;
    use crate::runtime::client::LocalEngineClient;
    use crate::runtime::owner::OwnerGate;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn actor_spawn_failure_is_reported_as_a_typed_bounded_error() {
        let error = ActorStartError::from_spawn(&std::io::Error::other("thread spawn failed"));
        assert_eq!(error.message(), "thread spawn failed");

        let long = "x".repeat(1_024);
        let bounded = ActorStartError::from_spawn(&std::io::Error::other(long));
        assert!(bounded.message().len() <= 256);
    }

    #[test]
    fn closing_the_actor_wakes_a_request_waiting_for_owner_startup() {
        let gate = OwnerGate::starting();
        let project = LocalProjectId::from_path(std::path::Path::new("/tmp")).expect("project identity");
        let client = LocalEngineClient::gated("/tmp/nudox-no-owner-for-cancellation.sock", project, gate.clone());
        let actor = EngineActor::start(client, 4).expect("actor");
        let request = EngineRequest::Root {
            request: RequestId::new(1),
            basis: VersionedRoot::unserved(),
            cancel: CancellationToken::new(),
        };
        assert!(matches!(actor.try_submit(request), super::PushResult::Enqueued));
        crate::runtime::wait::until("actor entered cancellable owner wait", || {
            actor.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel.is_some()
        });
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || sent.send(drop(actor)).expect("actor closed"));
        received.recv_timeout(Duration::from_secs(1)).expect("actor shutdown did not wait for the owner's 60-second patience");
        assert_eq!(gate.state(), crate::runtime::owner::OwnerState::Starting);
    }

    #[test]
    fn closing_the_actor_never_executes_a_queued_index_mutation() {
        struct Stalled {
            gate: OwnerGate,
            started: mpsc::Sender<()>,
            mutations: Arc<AtomicUsize>,
        }

        impl EngineClient for Stalled {
            fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
                match request {
                    EngineRequest::Root { cancel, .. } => {
                        self.started.send(()).expect("root entered its owner wait");
                        let _ = self.gate.wait_cancelled(cancel);
                        Err(EngineFault::Cancelled)
                    }
                    EngineRequest::IndexProject { .. } => {
                        self.mutations.fetch_add(1, Ordering::SeqCst);
                        Err(EngineFault::Cancelled)
                    }
                    _ => Err(EngineFault::Cancelled),
                }
            }
        }

        let gate = OwnerGate::starting();
        let mutations = Arc::new(AtomicUsize::new(0));
        let (started, entered) = mpsc::channel();
        let actor = EngineActor::start(Stalled {
            gate,
            started,
            mutations: Arc::clone(&mutations),
        }, 4).expect("actor");
        assert!(matches!(actor.try_submit(EngineRequest::Root {
            request: RequestId::new(1),
            basis: VersionedRoot::unserved(),
            cancel: CancellationToken::new(),
        }), super::PushResult::Enqueued));
        entered.recv_timeout(Duration::from_secs(1)).expect("root entered");

        let project = LocalProjectId::from_path(std::path::Path::new("/tmp"))
            .expect("project identity");
        assert!(matches!(actor.try_submit(EngineRequest::IndexProject {
            request: RequestId::new(2),
            project,
            basis: VersionedRoot::unserved(),
            cancel: CancellationToken::new(),
        }), super::PushResult::Enqueued));

        let (closed, finished) = mpsc::channel();
        std::thread::spawn(move || closed.send(drop(actor)).expect("actor closed"));
        finished.recv_timeout(Duration::from_secs(1)).expect("closing the actor released its root wait");
        assert_eq!(mutations.load(Ordering::SeqCst), 0, "a queued mutation cannot reach the client after close");
    }

    #[cfg(unix)]
    #[test]
    fn closing_the_actor_retires_an_active_local_cargo_process() {
        use std::os::unix::fs::PermissionsExt as _;

        struct Idle;
        impl EngineClient for Idle {
            fn execute(&mut self, _: &EngineRequest) -> Result<EngineDto, EngineFault> {
                Err(EngineFault::Cancelled)
            }
        }

        let folder = std::env::temp_dir().join(format!(
            "nudox-cancel-local-cargo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos(),
        ));
        std::fs::create_dir_all(&folder).expect("fixture directory");
        std::fs::write(folder.join("Cargo.toml"), "[package]\nname='cancel-fixture'\nversion='0.1.0'\n")
            .expect("fixture manifest");
        let entered = folder.join("cargo-entered");
        let escaped = folder.join("descendant-escaped");
        let script = folder.join("fake-cargo");
        std::fs::write(&script, format!(
            "#!/bin/sh\nprintf entered > '{}'\n(sleep 1; printf escaped > '{}') &\nexec sleep 30\n",
            entered.display(), escaped.display(),
        )).expect("fake cargo");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("make executable");
        let project = LocalProjectId::from_path(&folder).expect("project identity");
        let actor = EngineActor::start_with_loader(
            Idle, 4, LocalPackageLoader::default().with_cargo(script).with_timeout(Duration::from_secs(30)),
        ).expect("actor");
        assert!(matches!(actor.try_submit_local(LocalRead {
            request: RequestId::new(1),
            project,
            basis: VersionedRoot::unserved(),
            cancel: CancellationToken::new(),
        }), super::PushResult::Enqueued));
        crate::runtime::wait::until("local Cargo child entered", || entered.exists());
        let (closed, finished) = mpsc::channel();
        std::thread::spawn(move || closed.send(drop(actor)).expect("actor closed"));
        finished.recv_timeout(Duration::from_secs(2)).expect("local Cargo held actor shutdown");
        std::thread::sleep(Duration::from_millis(1_200));
        assert!(!escaped.exists(), "local child descendant escaped process-group retirement");
        std::fs::remove_dir_all(folder).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn closing_the_actor_interrupts_an_active_authenticated_socket_read() {
        use std::io::Read as _;
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::UnixListener;

        let path = std::path::PathBuf::from(format!(
            "/tmp/nudox-actor-interrupt-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos(),
        ));
        let listener = UnixListener::bind(&path).expect("private socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("private endpoint");
        let (entered, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accepted client");
            let mut frame_length = [0_u8; 4];
            socket.read_exact(&mut frame_length).expect("client sent request");
            entered.send(()).expect("request reached server");
            let _ = released.recv_timeout(Duration::from_secs(3));
        });
        let project = LocalProjectId::from_path(std::path::Path::new("/tmp")).expect("project identity");
        let actor = EngineActor::start(LocalEngineClient::new(&path, project), 4).expect("actor");
        assert!(matches!(actor.try_submit(EngineRequest::Surface {
            request: RequestId::new(1),
            command: backend_library::SurfaceCommand::Explore { query: None, limit: 1 },
            basis: VersionedRoot::unserved(),
            cancel: CancellationToken::new(),
        }), super::PushResult::Enqueued));
        received.recv_timeout(Duration::from_secs(2)).expect("read request entered socket");
        let (closed, finished) = mpsc::channel();
        std::thread::spawn(move || closed.send(drop(actor)).expect("actor closed"));
        finished.recv_timeout(Duration::from_secs(1)).expect("active socket read held actor shutdown");
        release.send(()).expect("release server");
        server.join().expect("server stopped");
        std::fs::remove_file(path).expect("remove endpoint");
    }
}
