//! Owner admission seams for completion and replication.

use super::{CompletionClaim, EngineStatus, ProtocolError, daemon_replicate};
use std::fmt;

/// Completion admission seam supplied by the engine composition root.
pub trait CompletionAdmission<M: backend_engine::WorkspaceModel, V, A>
where
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    /// Compares a wire claim with an owner-retained scheduler ticket and, on
    /// success, submits the resulting capability through the owner loop.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim fails exact owner admission.
    fn admit(
        &mut self,
        daemon: &mut crate::Locald<M, V, A>,
        request_id: u64,
        claim: CompletionClaim,
    ) -> Result<EngineStatus, ProtocolError>;
}

/// Explicit default that refuses completion claims when no owner admission
/// capability has been wired. This prevents a process from treating raw
/// digest fields as a trusted completion.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCompletionAdmission;

impl<M, V, A> CompletionAdmission<M, V, A> for NoCompletionAdmission
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    fn admit(
        &mut self,
        _daemon: &mut crate::Locald<M, V, A>,
        _request_id: u64,
        _claim: CompletionClaim,
    ) -> Result<EngineStatus, ProtocolError> {
        Err(ProtocolError::InvalidControl(
            "completion admission is not configured",
        ))
    }
}

/// Replication admission seam supplied by the engine composition root.
///
/// `WireRecipeResult` is never accepted merely because the daemon buffered its
/// bytes. The composition root must correlate the result with the retained
/// [`backend_engine::DispatchPlan`] and [`backend_engine::RemoteDispatchContract`],
/// call the dispatcher admission API, and only then return [`EngineStatus::Accepted`].
pub trait ReplicationAdmission<M: backend_engine::WorkspaceModel, V, A>
where
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    /// Admits one replication result or delegates a non-result message to the
    /// ordinary daemon replication lane.
    ///
    /// # Errors
    ///
    /// Returns an error when the message fails exact owner admission.
    fn admit(
        &mut self,
        daemon: &mut crate::Locald<M, V, A>,
        request_id: u64,
        message: backend_engine::TransportMessage,
    ) -> Result<EngineStatus, ProtocolError>;

    /// Advances daemon-owned remote I/O without waiting on a client frame.
    ///
    /// A process composition can use this hook to drain a bounded transport
    /// inbox, complete retained tickets, and reap expired work between client
    /// requests. The default is deliberately empty for compositions that do
    /// not install a remote transport.
    fn poll(&mut self, _daemon: &mut crate::Locald<M, V, A>) -> bool {
        false
    }
}

/// Explicit default that refuses worker results without a retained dispatch
/// plan and contract. Other replication messages still use the daemon lane.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoReplicationAdmission;

impl<M, V, A> ReplicationAdmission<M, V, A> for NoReplicationAdmission
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    fn admit(
        &mut self,
        daemon: &mut crate::Locald<M, V, A>,
        request_id: u64,
        message: backend_engine::TransportMessage,
    ) -> Result<EngineStatus, ProtocolError> {
        if matches!(
            message,
            backend_engine::TransportMessage::WireRecipeResult(_)
        ) {
            return Err(ProtocolError::InvalidControl(
                "worker result admission is not configured",
            ));
        }
        daemon_replicate(daemon, request_id, message)
    }
}

impl<M, V, A, F, E> ReplicationAdmission<M, V, A> for F
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
    F: FnMut(
        &mut crate::Locald<M, V, A>,
        u64,
        backend_engine::TransportMessage,
    ) -> Result<EngineStatus, E>,
    E: fmt::Display,
{
    fn admit(
        &mut self,
        daemon: &mut crate::Locald<M, V, A>,
        request_id: u64,
        message: backend_engine::TransportMessage,
    ) -> Result<EngineStatus, ProtocolError> {
        self(daemon, request_id, message)
            .map_err(|_error| ProtocolError::InvalidControl("replication admission failed"))
    }
}

impl<M, V, A, F, E> CompletionAdmission<M, V, A> for F
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
    F: FnMut(&mut crate::Locald<M, V, A>, u64, CompletionClaim) -> Result<EngineStatus, E>,
    E: fmt::Display,
{
    fn admit(
        &mut self,
        daemon: &mut crate::Locald<M, V, A>,
        request_id: u64,
        claim: CompletionClaim,
    ) -> Result<EngineStatus, ProtocolError> {
        self(daemon, request_id, claim)
            .map_err(|_error| ProtocolError::InvalidControl("completion admission failed"))
    }
}
