//! Background engine/client actor.

use super::mailbox::{CoalesceKey, Coalescible, CoalescingMailbox, PushResult};
use crate::core::{ErrorValue, VersionedRoot};
use crate::model::snapshot::{DeltaId, ObjectId, ProjectState};
use crate::navigation::RequestId;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

/// Cheap cancellation handle shared by a request and the worker.
#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Creates a live token.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Requests cancellation without waiting for the worker.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Returns whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
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
}

impl EngineRequest {
    fn basis(&self) -> VersionedRoot {
        match self {
            Self::Root { basis, .. } | Self::Object { basis, .. } => *basis,
        }
    }

    pub(crate) fn request(&self) -> RequestId {
        match self {
            Self::Root { request, .. } | Self::Object { request, .. } => *request,
        }
    }

    fn cancelled(&self) -> bool {
        match self {
            Self::Root { cancel, .. } | Self::Object { cancel, .. } => cancel.is_cancelled(),
        }
    }
}

impl Coalescible for EngineRequest {
    fn coalesce_key(&self) -> Option<CoalesceKey> {
        match self {
            Self::Root { .. } => Some(CoalesceKey::Root),
            Self::Object { object, .. } => Some(CoalesceKey::Object(*object)),
        }
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
        /// Optional transition identity.
        delta: Option<DeltaId>,
        /// Optional mapped project read model.
        project: Option<ProjectDto>,
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
}

/// Engine-owned project DTO. It is deliberately not imported by widgets.
#[derive(Clone, Debug)]
pub struct ProjectDto {
    /// Canonical project identity.
    pub id: crate::core::ProjectId,
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
            Err(_) => None,
        })
    }
}

/// Synchronous client implementation executed only on the worker thread.
pub trait EngineClient: Send + 'static {
    /// Executes one typed request. This method may block the worker, never the UI.
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault>;
}

/// Handle for a dedicated background engine actor.
pub struct EngineActor {
    mailbox: CoalescingMailbox<EngineRequest>,
    events: CoalescingMailbox<EngineEvent>,
    join: Option<JoinHandle<()>>,
}

/// Failure to create the dedicated worker thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActorStartError {
    /// Bounded operating-system diagnostic.
    message: Arc<str>,
}

impl ActorStartError {
    fn from_spawn(error: std::io::Error) -> Self {
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
    pub fn start(client: impl EngineClient, capacity: usize) -> Result<Self, ActorStartError> {
        let mailbox = CoalescingMailbox::new(capacity);
        let events = CoalescingMailbox::new(capacity);
        let worker_mailbox = mailbox.clone();
        let worker_events = events.clone();
        let join = thread::Builder::new()
            .name("nudox-engine-actor".to_owned())
            .spawn(move || run_actor(Box::new(client), worker_mailbox, worker_events))
            .map_err(ActorStartError::from_spawn)?;
        Ok(Self {
            mailbox,
            events,
            join: Some(join),
        })
    }

    /// Submits without waiting for worker capacity.
    pub fn try_submit(&self, request: EngineRequest) -> PushResult<EngineRequest> {
        self.try_submit_coalesced(request)
    }

    /// Submits using the request's coalescing key.
    pub fn try_submit_coalesced(&self, request: EngineRequest) -> PushResult<EngineRequest> {
        let key = request.coalesce_key();
        let result = self.mailbox.try_push(request, key);
        if let PushResult::Coalesced(old) = &result {
            match old {
                EngineRequest::Root { cancel, .. } | EngineRequest::Object { cancel, .. } => {
                    cancel.cancel()
                }
            }
        }
        result
    }

    /// Drains currently available events without waiting.
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

    /// Requests worker shutdown and joins it from an owning shutdown phase.
    ///
    /// Call this from a non-UI owner. UI entity teardown uses `Drop`, which
    /// closes both bounded channels and releases the handle without waiting
    /// for a client implementation that may be inside a blocking call.
    pub fn shutdown(mut self) {
        self.mailbox.close();
        self.events.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for EngineActor {
    fn drop(&mut self) {
        self.mailbox.close();
        self.events.close();
        // Dropping a join handle detaches the worker. A UI entity may be
        // released during teardown, so it must never wait for a client call;
        // callers that own a background shutdown phase can use `shutdown`.
        let _ = self.join.take();
    }
}

fn run_actor(
    mut client: Box<dyn EngineClient>,
    mailbox: CoalescingMailbox<EngineRequest>,
    events: CoalescingMailbox<EngineEvent>,
) {
    let mut newest: Option<VersionedRoot> = None;
    while let Some(request) = mailbox.recv() {
        let basis = request.basis();
        let id = request.request();
        let lane = request.coalesce_key();
        if request.cancelled() {
            if !events.push_wait(
                EngineEvent {
                    basis,
                    request: id,
                    lane,
                    result: Err(EngineFault::Cancelled),
                },
                lane,
            ) {
                break;
            }
            continue;
        }
        if newest.is_some_and(|known| basis.is_older_authority(known)) {
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
            continue;
        }
        if newest.is_none_or(|known| known.is_older_authority(basis)) {
            newest = Some(basis);
        }
        let result = client.execute(&request);
        let result = if request.cancelled() {
            Err(EngineFault::Cancelled)
        } else {
            result
        };
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
    }
}

#[cfg(test)]
mod tests {
    use super::ActorStartError;

    #[test]
    fn actor_spawn_failure_is_reported_as_a_typed_bounded_error() {
        let error = ActorStartError::from_spawn(std::io::Error::other("thread spawn failed"));
        assert_eq!(error.message(), "thread spawn failed");

        let long = "x".repeat(1_024);
        let bounded = ActorStartError::from_spawn(std::io::Error::other(long));
        assert!(bounded.message().len() <= 256);
    }
}
