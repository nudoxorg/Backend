//! Owner-loop service adapters for locald.
//!
//! The service deliberately does not own a workspace. An [`OwnerService`]
//! implementation is the only place allowed to turn a command or engine
//! operation into state. `LocaldService` serializes all calls through one
//! owner loop and is therefore safe to put behind a Unix listener without
//! accidentally creating a second head writer per connection.

use crate::protocol::{CompletionClaim, EngineRequest, EngineStatus, ProtocolError};
use backend_engine::{Cursor, DaemonReply, LocalSubscriptionId, ViewPageCursor};
use std::collections::BTreeMap;
use std::fmt;
use std::time::Instant;

#[path = "service/runtime.rs"]
mod runtime;
#[path = "service/transport.rs"]
mod transport;

pub use transport::{LocaldService, OwnerService};
#[path = "service/subscription.rs"]
mod subscription;

#[path = "service/lease.rs"]
mod lease;

use subscription::decode_cursor_for_service;

pub use runtime::ServiceError;
pub(crate) use runtime::{
    RequestCorrelation, daemon_replicate, error_payload, map_queue_error, wait_for_daemon_reply,
};

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
> where
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    daemon: crate::Locald<M, V, A>,
    command: F,
    completion: C,
    replication: R,
    leases: BTreeMap<LocalSubscriptionId, DurableLease>,
    next_lease_nonce: u64,
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
    expires_at: Instant,
    snapshot: Option<DurableSnapshot>,
}

#[derive(Debug)]
struct DurableSnapshot {
    root: Box<backend_engine::ViewRoot>,
    cursor: Cursor,
    reason: backend_engine::CursorResetReason,
    next: Option<ViewPageCursor>,
    next_token: Option<Box<[u8]>>,
}

#[path = "service/admission.rs"]
mod admission;
pub use admission::{
    CompletionAdmission, NoCompletionAdmission, NoReplicationAdmission, ReplicationAdmission,
};

impl<M, V, A, F, C, R> fmt::Debug for LocaldOwner<M, V, A, F, C, R>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
    F: fmt::Debug,
    C: fmt::Debug,
    R: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocaldOwner")
            .field("daemon", &self.daemon)
            .field("command", &self.command)
            .field("completion", &self.completion)
            .field("replication", &self.replication)
            .field("leases", &self.leases)
            .field("next_lease_nonce", &self.next_lease_nonce)
            .finish()
    }
}

impl<M, V, A, F> LocaldOwner<M, V, A, F, NoCompletionAdmission, NoReplicationAdmission>
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
            leases: BTreeMap::new(),
            next_lease_nonce: 0,
        }
    }
}

impl<M, V, A, F, C> LocaldOwner<M, V, A, F, C, NoReplicationAdmission>
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
            leases: BTreeMap::new(),
            next_lease_nonce: 0,
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
    ) -> LocaldOwner<M, V, A, F, C, R> {
        LocaldOwner {
            daemon,
            command,
            completion,
            replication,
            leases: BTreeMap::new(),
            next_lease_nonce: 0,
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

impl<M, V, A, F, C, R, E> OwnerService for LocaldOwner<M, V, A, F, C, R>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
    F: FnMut(&mut crate::Locald<M, V, A>, &[u8]) -> Result<Vec<u8>, E>,
    E: fmt::Display,
    C: CompletionAdmission<M, V, A>,
    R: ReplicationAdmission<M, V, A>,
{
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        (self.command)(&mut self.daemon, body)
            .map_err(|error| ProtocolError::CommandExecution(error.to_string()))
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
        // Remote transports are daemon-owned, but their socket reads must not
        // monopolize the owner while a client waits for its next frame. Poll
        // the composition's bounded inbox first, then run one fair engine
        // lane. A later client request can therefore observe durable output
        // from a completion that arrived out of order.
        let remote_progress = self.replication.poll(&mut self.daemon);
        remote_progress || self.daemon.serve_one()
    }

    fn close(&mut self) {
        self.leases.clear();
        self.daemon.close();
    }
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
