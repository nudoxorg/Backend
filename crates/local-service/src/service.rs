//! Owner-loop service adapters for locald.
//!
//! The service deliberately does not own a workspace. An [`OwnerService`]
//! implementation is the only place allowed to turn a command or engine
//! operation into state. `LocaldService` serializes all calls through one
//! owner loop and is therefore safe to put behind a Unix listener without
//! accidentally creating a second head writer per connection.

use crate::protocol::{CompletionClaim, EngineRequest, EngineStatus, ProtocolError};
use backend_engine::{DaemonReply, LocalSubscriptionRequest};
use std::fmt;

#[path = "service/runtime.rs"]
mod runtime;
#[path = "service/transport.rs"]
mod transport;
#[path = "service/lease/mod.rs"]
mod lease;

pub use transport::{CommandOutcome, Handled, LocaldService, OwnerService};
pub use lease::SubscriptionLeaseLimits;

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

    /// Releases one response registration without cancelling the accepted
    /// owner operation that produced it.
    fn abandon_reply(&mut self, _ticket: u64) {}

    /// Cancels and joins any process-local deferred work before the owner
    /// releases its daemon state. Implementations without workers need not
    /// override this hook.
    fn close(&mut self) {}
}
#[path = "service/subscription.rs"]
mod subscription;

pub use runtime::ServiceError;
pub(crate) use runtime::{
    RequestCorrelation, daemon_replicate, error_payload, map_queue_error, wait_for_daemon_reply,
};

use lease::{LeaseHost, LeaseTable, OwnerSource};
#[cfg(test)]
use backend_client::monotonic::MonotonicClock;
#[cfg(test)]
use std::sync::Arc;

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
    leases: lease::LeaseTable,
    deferred: Option<Box<dyn DeferredCommands<M, V, A> + Send>>,
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
            .finish_non_exhaustive()
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
        self.leases.set_limits(limits);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_lease_clock(mut self, clock: Arc<dyn MonotonicClock>) -> Self {
        self.leases.set_clock(clock);
        self
    }

    fn durable_subscription(
        &mut self,
        request_id: u64,
        request: LocalSubscriptionRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        let mut source = OwnerSource::new(&mut self.daemon);
        LeaseHost::new(&mut self.leases, &mut source).handle(request_id, request)
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
            leases: LeaseTable::default(),
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
            leases: LeaseTable::default(),
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
            leases: LeaseTable::default(),
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
            leases: LeaseTable::default(),
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

    fn abandon_deferred_reply(&mut self, ticket: u64) {
        if let Some(deferred) = self.deferred.as_mut() {
            deferred.abandon_reply(ticket);
        }
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
        // reset roots without waiting for a same-lease request. This is
        // housekeeping only; it is not progress reported to idle retirement.
        let now = self.leases.now();
        self.leases.reclaim_due(now);
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
        remote_progress || engine_progress
    }

    fn close(&mut self) {
        self.leases.close();
        if let Some(deferred) = self.deferred.as_mut() {
            deferred.close();
        }
        self.daemon.close();
    }
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
