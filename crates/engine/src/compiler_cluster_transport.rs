//! Engine bridge from fenced compiler assignments to private Bao object grants.
//!
//! Placement and attempt deduplication live in `backend-execution`; this module
//! maps their exact assignment into the real Iroh/Bao transfer scope. It does
//! not own network framing or compiler result publication.

use backend_cluster_transport::{
    AssignmentScope, CancelReason, Capability, CapabilityClaims, CapabilityIssuer, ChunkRange,
    ControlAccept, ControlAdmissionPolicy, ControlCancel, ControlChannel, ControlExecutionFailed,
    ControlGrantPage, ControlMessage, ControlNoResultRetireThrough,
    ControlNoResultRetirementApplied, ControlOffer, ControlResultAck, ControlResultReceipt,
    ControlRole, Endpoint, EndpointAddr, EndpointId, ExecutionFailureReason, GrantDirection,
    MAX_CONTROL_GRANT_PAGES, MAX_CONTROL_MESSAGES_PER_STREAM, MAX_OFFER_CAPABILITIES,
    MAX_RANGE_CHUNKS, MaterializedObject, ResultAckDisposition, ResultRejectReason, ResumeState,
    ServerState, StoreBlobCatalog, TransferScope, TransportError, VerifiedStoreClosureMember,
    WorkerReject, WorkerRejectReason, accept_control, connect_control, fetch_range, serve_one,
};
use backend_execution::{
    CompilerAssignment, CompilerAssignmentRoute, CompilerAttemptToken, CompilerClusterScheduler,
    RemoteCompilerCompletionClaim, StoredCompilerCandidate,
};
use backend_store::{
    ArtifactClosureClaim, ArtifactSession, ClosureId, StoredClosureReceipt, UntrustedObjectId,
};
use std::num::TryFromIntError;
use std::path::Path;
use thiserror::Error;

pub use backend_execution::compiler_full_workspace_transfer_work_id;

pub use crate::compiler_input_capture_v2::{
    CaptureWorkspaceIdentityV2, CapturedFullWorkspaceV2, CapturedFullWorkspaceV2Expectation,
    CompilerInputCaptureCacheKeyV2, CompilerInputCaptureUpdateModeV2,
    CompilerInputCaptureUpdateStatsV2, CompilerInputCaptureV2Error, CompilerWorkspaceEntryKindV2,
    CompilerWorkspaceEntryV2, CompilerWorkspaceFileV2Schema, VerifiedFullWorkspaceClosureV2,
    WorkspaceSnapshotSourceV2, capture_full_workspace_v2, capture_full_workspace_v2_with_prior,
    verify_full_workspace_closure_v2,
};
pub use crate::compiler_input_manifest::{
    CompilerInputEntry, CompilerInputFileRole, CompilerInputManifestError,
    CompilerInputManifestSchema, CompilerInputManifestV1, CompilerReadClaim, InputCompleteness,
    untrusted_object_id, validate_compiler_input_path,
};
pub use crate::compiler_input_manifest_v2::{
    CompilationUnitKeyV2, CompilerInputManifestV2, CompilerInputManifestV2Error,
    CompilerInputManifestV2Schema, CompilerInvocationRecipeV2, CompilerInvocationRecipeV2Error,
    CompilerPackageTargetV2, CompilerPackageTargetV2Error, CompilerReadFrontierStatusV2,
    CompilerWorkspaceSnapshotIdV2, compiler_input_manifest_v2_schema,
};
pub use crate::compiler_input_tree_v2::{
    CompilerInputMerklePageSchema, CompilerInputMerklePageV2, CompilerInputMerkleTreeV2,
    CompilerInputPageDeltaV2, CompilerInputTreeKindV2, CompilerInputTreeRecordV2,
    CompilerInputTreeReplacementV2, CompilerInputTreeUpdateStatsV2, CompilerInputTreeV2Error,
    CompilerWorkspaceFileRoleV2,
};
pub use crate::compiler_unit_read_closure_v2::{
    CompilerReadAdapterProtocolIdentityV2, CompilerReadClosureIncompleteReasonV2,
    CompilerUnitReadClosureErrorV2, CompilerUnitReadClosureStatsV2,
    CompilerWorkspaceCaptureIdentityV2, PureUnitKey, VerifiedUnitReadClosure,
};
pub use backend_execution::CompilerPeerId;

/// Grant pages sent on one control stream before opening a new authenticated stream.
pub const COMPILER_GRANT_PAGES_PER_CONTROL_STREAM: usize = MAX_CONTROL_MESSAGES_PER_STREAM as usize;
/// Hard object-count bound implied by transport's page and per-page capability caps.
pub const MAX_COMPILER_RESULT_OBJECTS: u32 =
    MAX_CONTROL_GRANT_PAGES * MAX_OFFER_CAPABILITIES as u32;
/// Bao chunk size used to bound how many nonempty result ranges can fit in a closure.
const COMPILER_BAO_CHUNK_BYTES: u64 = 1_024;

/// Failure while binding a scheduler assignment to an Iroh/Bao capability.
#[derive(Debug, Error)]
pub enum CompilerTransportBridgeError {
    /// Only a remote assignment can authorize a network transfer.
    #[error("compiler assignment is not remote")]
    WrongRoute,
    /// Iroh rejected the scheduler's peer identity bytes.
    #[error("compiler peer identity is not a valid Iroh endpoint id")]
    InvalidPeer,
    /// The transport rejected this object range or signed capability.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The control receipt or range grant belongs to a different assignment scope.
    #[error("compiler transport message has a different assignment scope")]
    ScopeMismatch,
    /// A control result disagrees with the locally stored closure facts.
    #[error("compiler result receipt differs from its stored closure")]
    ResultMetadataMismatch,
    /// The exact materialized object was not proved to belong to the requested closure.
    #[error("compiler object is not a verified member of the requested closure")]
    ClosureMemberMismatch,
    /// Worker-reported bytes, objects, or grant pages exceed the assignment's hard bounds.
    #[error("compiler result receipt exceeds its output or grant-page budget")]
    ResultBudgetExceeded,
    /// Scheduler rejected a stale, cancelled, or mismatched exact assignment.
    #[error(transparent)]
    Assignment(#[from] backend_execution::CompilerAssignmentError),
    /// The authenticated peer sent a message that is not expected at this protocol stage.
    #[error("unexpected compiler control message for this protocol stage")]
    UnexpectedControlMessage,
    /// The authenticated Iroh peer is not the peer assigned by the scheduler.
    #[error("authenticated control peer differs from the exact compiler assignment")]
    ControlPeerMismatch,
    /// Capability signer or grant endpoint roles do not match this cluster side.
    #[error("compiler grant signer or endpoint roles differ from the transfer direction")]
    GrantEndpointMismatch,
    /// Grant page series is incomplete, reordered, or exceeds transport limits.
    #[error("compiler grant page series is incomplete or outside transport limits")]
    InvalidGrantPageSeries,
    /// Page count cannot be represented by the transport protocol.
    #[error("compiler grant page count is outside transport limits")]
    GrantPageCount(#[source] TryFromIntError),
    /// A typed manifest differs from the assigned package, input, or exact invocation scope.
    #[error("compiler input manifest does not match the assigned work")]
    InputManifestMismatch,
    /// The typed compiler input manifest could not be admitted.
    #[error(transparent)]
    InputManifest(#[from] CompilerInputManifestError),
    /// The typed V2 full-workspace compiler input manifest could not be admitted.
    #[error(transparent)]
    InputManifestV2(#[from] CompilerInputManifestV2Error),
}

/// Builds the exact transport scope for a remote compiler assignment and closure.
///
/// The scope binds work ID, authority attempt ordinal, all 32 fence bytes, and the
/// backend-store closure identity. Transport admission requires exact equality at
/// both endpoints and on every resumed Bao range.
///
/// # Errors
///
/// Returns [`CompilerTransportBridgeError::WrongRoute`] for local assignments or a
/// transport rejection for an invalid closure scope.
pub fn compiler_assignment_scope(
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
) -> Result<AssignmentScope, CompilerTransportBridgeError> {
    if !matches!(assignment.route(), CompilerAssignmentRoute::Remote(_)) {
        return Err(CompilerTransportBridgeError::WrongRoute);
    }
    let token = assignment.token();
    Ok(AssignmentScope::new(
        namespace_id,
        assignment.work().transfer_work_id(),
        token.attempt().get(),
        token.fence().as_bytes(),
    )?)
}

/// Builds a bounded Offer for one remote compile assignment.
///
/// The number of input-grant pages is declared up front. Each page carries at most
/// the transport's fixed grant bound; Bao object bytes stay on separate range streams.
pub fn compiler_control_offer(
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    input_closure: ClosureId,
    input_manifest: &CompilerInputManifestV1,
    input_grant_pages: u32,
    deadline_unix_ms: u64,
) -> Result<ControlOffer, CompilerTransportBridgeError> {
    let work = assignment.work();
    let identity = work.input_identity();
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    let input_manifest_object_id = input_manifest.object_id()?;
    if work.input().full_workspace().is_some_and(|input| {
        let claim = input.claim();
        claim.input_closure_id != *input_closure.as_bytes()
            || claim.manifest_object_id != *input_manifest_object_id.as_bytes()
    }) {
        return Err(CompilerTransportBridgeError::InputManifestMismatch);
    }
    let offer = ControlOffer {
        scope,
        package_lineage: work.package().as_bytes(),
        target: *work.target().as_ref(),
        recipe: *work.recipe().as_ref(),
        input_root: *identity.input_root.as_ref(),
        read_manifest: *identity.manifest.as_bytes(),
        input_closure_id: *input_closure.as_bytes(),
        input_manifest_object_id: *input_manifest_object_id.as_bytes(),
        selected_base: work.selected_base().map(|base| *base.as_ref()),
        max_output_bytes: work.max_output_bytes(),
        deadline_unix_ms,
        input_grant_pages,
    };
    if !input_manifest.matches_offer(&offer) {
        return Err(CompilerTransportBridgeError::InputManifestMismatch);
    }
    Ok(offer)
}

/// Builds a bounded Offer for one exact V2 full-workspace compile assignment.
///
/// V2 binds a complete immutable workspace capture and always describes a fresh compile. Its
/// manifest object and checked outer closure are included in the transfer work ID, so the worker
/// can recompute and admit the exact offered snapshot before materializing a sandbox.
pub fn compiler_control_offer_v2(
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    input_closure: ClosureId,
    input_manifest: &CompilerInputManifestV2,
    input_grant_pages: u32,
    deadline_unix_ms: u64,
) -> Result<ControlOffer, CompilerTransportBridgeError> {
    let work = assignment.work();
    let Some(full_workspace) = work.input().full_workspace() else {
        return Err(CompilerTransportBridgeError::InputManifestMismatch);
    };
    let claim = full_workspace.claim();
    let identity = work.input_identity();
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    let input_manifest_object_id = input_manifest.object_id()?;
    if claim.input_closure_id != *input_closure.as_bytes()
        || claim.manifest_object_id != *input_manifest_object_id.as_bytes()
        || input_manifest.selected_base().is_some()
    {
        return Err(CompilerTransportBridgeError::InputManifestMismatch);
    }
    let offer = ControlOffer {
        scope,
        package_lineage: work.package().as_bytes(),
        target: *work.target().as_ref(),
        recipe: *work.recipe().as_ref(),
        input_root: *identity.input_root.as_ref(),
        read_manifest: *identity.manifest.as_bytes(),
        input_closure_id: *input_closure.as_bytes(),
        input_manifest_object_id: *input_manifest_object_id.as_bytes(),
        selected_base: None,
        max_output_bytes: work.max_output_bytes(),
        deadline_unix_ms,
        input_grant_pages,
    };
    if !input_manifest.matches_offer(&offer, input_closure)? {
        return Err(CompilerTransportBridgeError::InputManifestMismatch);
    }
    Ok(offer)
}

/// Opens an authenticated coordinator control stream to the assigned Iroh peer.
pub async fn connect_compiler_control(
    scheduler: &CompilerClusterScheduler,
    endpoint: &Endpoint,
    peer_addr: EndpointAddr,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
) -> Result<ControlChannel, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    let peer = remote_endpoint_id(assignment)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    Ok(connect_control(endpoint, peer_addr, peer, scope, ControlRole::Coordinator).await?)
}

/// Opens a worker-side authenticated control stream to the index owner for one adopted Offer.
pub async fn connect_worker_control(
    endpoint: &Endpoint,
    coordinator_addr: EndpointAddr,
    coordinator_id: EndpointId,
    scope: AssignmentScope,
) -> Result<ControlChannel, CompilerTransportBridgeError> {
    Ok(connect_control(
        endpoint,
        coordinator_addr,
        coordinator_id,
        scope,
        ControlRole::Worker,
    )
    .await?)
}

/// Accepts a subsequent worker control stream at the coordinator for one active assignment.
pub async fn accept_coordinator_control(
    scheduler: &CompilerClusterScheduler,
    endpoint: &Endpoint,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
) -> Result<ControlChannel, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    let policy = ControlAdmissionPolicy::new(
        endpoint.id(),
        [remote_endpoint_id(assignment)?],
        compiler_assignment_scope(assignment, namespace_id)?,
        ControlRole::Coordinator,
    );
    Ok(accept_control(endpoint, &policy).await?)
}

/// Sends one bounded Offer over an already authenticated coordinator stream.
pub async fn send_compiler_offer(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    input_closure: ClosureId,
    input_manifest: &CompilerInputManifestV1,
    input_grant_pages: u32,
    deadline_unix_ms: u64,
) -> Result<(), CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let offer = compiler_control_offer(
        assignment,
        namespace_id,
        input_closure,
        input_manifest,
        input_grant_pages,
        deadline_unix_ms,
    )?;
    channel.send(&ControlMessage::Offer(offer)).await?;
    Ok(())
}

/// Sends one bounded V2 full-workspace Offer over an authenticated coordinator stream.
pub async fn send_compiler_offer_v2(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    input_closure: ClosureId,
    input_manifest: &CompilerInputManifestV2,
    input_grant_pages: u32,
    deadline_unix_ms: u64,
) -> Result<(), CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let offer = compiler_control_offer_v2(
        assignment,
        namespace_id,
        input_closure,
        input_manifest,
        input_grant_pages,
        deadline_unix_ms,
    )?;
    channel.send(&ControlMessage::Offer(offer)).await?;
    Ok(())
}

/// Sends the worker's accept or decline after it received the first Offer and adopted its scope.
pub async fn send_worker_accept(
    channel: &mut ControlChannel,
    scope: AssignmentScope,
    accepted: bool,
) -> Result<(), CompilerTransportBridgeError> {
    if channel.scope() != Some(scope) {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    channel
        .send(&ControlMessage::Accept(ControlAccept { scope, accepted }))
        .await?;
    Ok(())
}

/// Reports that an accepted worker could not admit the exact input closure or invocation.
pub async fn send_worker_input_reject(
    channel: &mut ControlChannel,
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    reason: WorkerRejectReason,
) -> Result<(), CompilerTransportBridgeError> {
    if channel.scope() != Some(scope) || input_closure_id == [0; 32] {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    channel
        .send(&ControlMessage::WorkerReject(WorkerReject {
            scope,
            input_closure_id,
            reason,
        }))
        .await?;
    Ok(())
}

/// Receives a worker's post-Accept rejection and binds it to the live assignment and input closure.
pub async fn receive_compiler_input_reject(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    input_closure_id: [u8; 32],
) -> Result<FencedCompilerInputReject, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let ControlMessage::WorkerReject(reject) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    if channel.scope() != Some(compiler_assignment_scope(assignment, namespace_id)?)
        || reject.scope != compiler_assignment_scope(assignment, namespace_id)?
        || reject.input_closure_id != input_closure_id
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(FencedCompilerInputReject {
        assignment,
        reason: reject.reason,
    })
}

/// Sends a terminal failure after the worker admitted the input closure, using a fresh
/// authenticated control stream. The owner still decides whether to mint a replacement attempt.
pub async fn send_worker_execution_failure(
    endpoint: &Endpoint,
    coordinator_addr: EndpointAddr,
    coordinator_id: EndpointId,
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    reason: ExecutionFailureReason,
) -> Result<(), CompilerTransportBridgeError> {
    if input_closure_id == [0; 32] {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    let mut channel =
        connect_worker_control(endpoint, coordinator_addr, coordinator_id, scope).await?;
    channel
        .send(&ControlMessage::ExecutionFailed(ControlExecutionFailed {
            scope,
            input_closure_id,
            reason,
        }))
        .await?;
    channel.finish().await?;
    Ok(())
}

/// Receives a terminal execution failure on a new authenticated stream and binds it to the
/// exact active assignment and the input closure named by its Offer.
pub async fn receive_compiler_execution_failure(
    scheduler: &CompilerClusterScheduler,
    endpoint: &Endpoint,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    expected_input_closure_id: [u8; 32],
) -> Result<FencedCompilerExecutionFailure, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    if expected_input_closure_id == [0; 32] {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    let mut channel =
        accept_coordinator_control(scheduler, endpoint, assignment, namespace_id).await?;
    let terminal = receive_compiler_terminal_message(
        &mut channel,
        scheduler,
        assignment,
        namespace_id,
        expected_input_closure_id,
    )
    .await;
    // Complete the short-lived stream even for a closure mismatch or an unexpected terminal
    // message so the worker can release its retained connection state.
    let finished = channel.finish().await;
    let terminal = match (terminal, finished) {
        (Ok(terminal), Ok(())) => terminal,
        (Err(error), _) => return Err(error),
        (Ok(_), Err(error)) => return Err(error.into()),
    };
    match terminal {
        FencedCompilerTerminalMessage::ExecutionFailed(failure) => Ok(failure),
        FencedCompilerTerminalMessage::Result(_) => {
            Err(CompilerTransportBridgeError::UnexpectedControlMessage)
        }
    }
}

/// Receives the first terminal result/failure message from one already accepted coordinator
/// stream. The caller owns `channel.finish().await` after dispatch, including error paths.
pub async fn receive_compiler_terminal_message(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    expected_input_closure_id: [u8; 32],
) -> Result<FencedCompilerTerminalMessage, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    if channel
        .scope()
        .is_some_and(|channel_scope| channel_scope != scope)
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    let message = channel.receive().await?;
    admit_compiler_terminal_message(
        channel,
        scheduler,
        assignment,
        namespace_id,
        expected_input_closure_id,
        message,
    )
}

/// Admits a terminal message that a shared listener dispatcher already decoded.
///
/// The dispatcher may use the channel's adopted scope and authenticated peer to look up the
/// durable live assignment before calling this function. The scheduler still rechecks the exact
/// current attempt here, before returning result metadata or a fallback-eligible failure.
pub fn admit_compiler_terminal_message(
    channel: &ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    expected_input_closure_id: [u8; 32],
    message: ControlMessage,
) -> Result<FencedCompilerTerminalMessage, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    if channel.scope() != Some(scope) {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    match message {
        ControlMessage::ResultReceipt(receipt) => Ok(FencedCompilerTerminalMessage::Result(
            admit_compiler_result(scheduler, assignment, namespace_id, receipt)?,
        )),
        ControlMessage::ExecutionFailed(failure) => Ok(
            FencedCompilerTerminalMessage::ExecutionFailed(admit_compiler_execution_failure(
                channel,
                scheduler,
                assignment,
                namespace_id,
                expected_input_closure_id,
                failure,
            )?),
        ),
        _ => Err(CompilerTransportBridgeError::UnexpectedControlMessage),
    }
}

/// Admits a worker failure already decoded by a shared terminal-message dispatcher.
///
/// This validates the active scheduler attempt, exact authenticated peer, transport assignment
/// scope/fence, and the input ClosureId from the Offer. It returns storage-free data only.
pub fn admit_compiler_execution_failure(
    channel: &ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    expected_input_closure_id: [u8; 32],
    failure: ControlExecutionFailed,
) -> Result<FencedCompilerExecutionFailure, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    if expected_input_closure_id == [0; 32]
        || channel.scope() != Some(scope)
        || failure.scope != scope
        || failure.input_closure_id != expected_input_closure_id
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(FencedCompilerExecutionFailure {
        assignment,
        reason: failure.reason,
    })
}

/// A result or terminal execution failure admitted from the first worker message on a control
/// stream. Neither variant grants publication or selected-head authority.
#[derive(Clone, Copy, Debug)]
pub enum FencedCompilerTerminalMessage {
    /// The worker claims a completed output closure; the owner must still store and verify it.
    Result(FencedCompilerResult),
    /// The worker reports a terminal failure after input admission.
    ExecutionFailed(FencedCompilerExecutionFailure),
}

/// Starts deterministic local fallback after the owner mints a strictly newer attempt token.
///
/// The database-minted replacement token is mandatory. This retires the rejected remote
/// assignment before returning its local replacement, so late worker output remains stale.
pub fn fallback_after_worker_input_reject(
    scheduler: &CompilerClusterScheduler,
    rejected: FencedCompilerInputReject,
    replacement: CompilerAttemptToken,
) -> Result<CompilerAssignment, CompilerTransportBridgeError> {
    Ok(scheduler.fallback_to_local(rejected.assignment, replacement)?)
}

/// Starts local fallback after the owner mints a strictly newer attempt token for a terminal
/// worker execution failure. The fenced remote assignment is retired before returning local work.
pub fn fallback_after_worker_execution_failure(
    scheduler: &CompilerClusterScheduler,
    failed: FencedCompilerExecutionFailure,
    replacement: CompilerAttemptToken,
) -> Result<CompilerAssignment, CompilerTransportBridgeError> {
    Ok(scheduler.fallback_to_local(failed.assignment, replacement)?)
}

/// Sends the next bounded page of object grants for an exact accepted assignment.
pub async fn send_compiler_grant_page(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    page_index: u32,
    page_count: u32,
    grants: Vec<Capability>,
) -> Result<(), CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    if channel.scope() != Some(scope)
        || grants
            .iter()
            .any(|grant| grant.claims.scope.assignment() != scope)
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    channel
        .send(&ControlMessage::GrantPage(ControlGrantPage {
            scope,
            direction: GrantDirection::InputsToWorker,
            page_index,
            page_count,
            grants,
        }))
        .await?;
    Ok(())
}

/// Sends one exact result-grant page from the assigned worker to the index owner.
///
/// Every capability is bound to the worker result's claimed closure. The receiving owner still
/// verifies the capabilities, Bao payloads, typed objects, and completed CAS closure.
pub async fn send_worker_result_grant_page(
    channel: &mut ControlChannel,
    assignment_scope: AssignmentScope,
    closure: ClosureId,
    page_index: u32,
    page_count: u32,
    grants: Vec<Capability>,
) -> Result<(), CompilerTransportBridgeError> {
    let transfer_scope = TransferScope::from_store_closure(assignment_scope, closure)?;
    if channel.scope() != Some(assignment_scope)
        || grants
            .iter()
            .any(|grant| grant.claims.scope != transfer_scope)
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    channel
        .send(&ControlMessage::GrantPage(ControlGrantPage {
            scope: assignment_scope,
            direction: GrantDirection::ResultsToCoordinator,
            page_index,
            page_count,
            grants,
        }))
        .await?;
    Ok(())
}

/// Sends a complete owner-to-worker input grant series, reopening the exact fenced stream
/// after every [`COMPILER_GRANT_PAGES_PER_CONTROL_STREAM`] pages.
///
/// The matching worker receiver is [`receive_compiler_grant_pages`]. The initial Offer/Accept
/// stream stays separate, so a large closure can use the full 4,096-page transport budget.
pub async fn send_compiler_grant_pages(
    scheduler: &CompilerClusterScheduler,
    endpoint: &Endpoint,
    peer_addr: EndpointAddr,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    input_closure: ClosureId,
    pages: &[ControlGrantPage],
) -> Result<(), CompilerTransportBridgeError> {
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    let transfer_scope = TransferScope::from_store_closure(scope, input_closure)?;
    validate_grant_page_series(
        pages,
        scope,
        GrantDirection::InputsToWorker,
        Some(transfer_scope),
    )?;
    for batch in pages.chunks(COMPILER_GRANT_PAGES_PER_CONTROL_STREAM) {
        let mut channel = connect_compiler_control(
            scheduler,
            endpoint,
            peer_addr.clone(),
            assignment,
            namespace_id,
        )
        .await?;
        for page in batch {
            send_compiler_grant_page(
                &mut channel,
                scheduler,
                assignment,
                namespace_id,
                page.page_index,
                page.page_count,
                page.grants.clone(),
            )
            .await?;
        }
        channel.finish().await?;
    }
    Ok(())
}

/// Sends the worker's complete result grant series to the owner using bounded exact-scope
/// streams. Each grant must name the receipt's one result closure and the correct endpoints.
pub async fn send_worker_result_grant_pages(
    endpoint: &Endpoint,
    coordinator_addr: EndpointAddr,
    coordinator_id: EndpointId,
    assignment_scope: AssignmentScope,
    closure: ClosureId,
    pages: &[ControlGrantPage],
) -> Result<(), CompilerTransportBridgeError> {
    let transfer_scope = TransferScope::from_store_closure(assignment_scope, closure)?;
    validate_grant_page_series(
        pages,
        assignment_scope,
        GrantDirection::ResultsToCoordinator,
        Some(transfer_scope),
    )?;
    for batch in pages.chunks(COMPILER_GRANT_PAGES_PER_CONTROL_STREAM) {
        let mut channel = connect_worker_control(
            endpoint,
            coordinator_addr.clone(),
            coordinator_id,
            assignment_scope,
        )
        .await?;
        for page in batch {
            if page.grants.iter().any(|grant| {
                grant.claims.client != coordinator_id || grant.claims.server != endpoint.id()
            }) {
                return Err(CompilerTransportBridgeError::GrantEndpointMismatch);
            }
            send_worker_result_grant_page(
                &mut channel,
                assignment_scope,
                closure,
                page.page_index,
                page.page_count,
                page.grants.clone(),
            )
            .await?;
        }
        channel.finish().await?;
    }
    Ok(())
}

fn validate_grant_page_series(
    pages: &[ControlGrantPage],
    scope: AssignmentScope,
    direction: GrantDirection,
    transfer_scope: Option<TransferScope>,
) -> Result<(), CompilerTransportBridgeError> {
    let page_count =
        u32::try_from(pages.len()).map_err(CompilerTransportBridgeError::GrantPageCount)?;
    if page_count > MAX_CONTROL_GRANT_PAGES {
        return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
    }
    if pages.iter().enumerate().any(|(index, page)| {
        let expected_index = u32::try_from(index).ok();
        page.scope != scope
            || page.direction != direction
            || expected_index != Some(page.page_index)
            || page.page_count != page_count
            || page.grants.is_empty()
            || page.grants.iter().any(|grant| {
                grant.claims.scope.assignment() != scope
                    || transfer_scope.is_some_and(|expected| grant.claims.scope != expected)
            })
    }) {
        return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
    }
    Ok(())
}

/// Sends an exact-attempt cancellation to the assigned peer.
pub async fn send_compiler_cancel(
    channel: &mut ControlChannel,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    reason: CancelReason,
) -> Result<(), CompilerTransportBridgeError> {
    verify_control_peer(channel, assignment)?;
    channel
        .send(&ControlMessage::Cancel(ControlCancel {
            scope: compiler_assignment_scope(assignment, namespace_id)?,
            reason,
        }))
        .await?;
    Ok(())
}

/// Persists and acknowledges a worker no-result fence floor after Turso issues an exact
/// maintenance barrier for one terminal assignment. The terminal and barrier scopes must share
/// the same canonical namespace; no time-based reclamation is safe for this protocol.
pub async fn retire_compiler_no_result_through(
    endpoint: &Endpoint,
    worker_addr: EndpointAddr,
    expected_worker: EndpointId,
    terminal_scope: AssignmentScope,
    barrier_scope: AssignmentScope,
    retired_through_epoch: u64,
) -> Result<ControlNoResultRetirementApplied, CompilerTransportBridgeError> {
    let request = ControlNoResultRetireThrough::new(
        terminal_scope,
        barrier_scope,
        retired_through_epoch,
        endpoint.id(),
    )?;
    let mut channel = connect_control(
        endpoint,
        worker_addr,
        expected_worker,
        barrier_scope,
        ControlRole::Coordinator,
    )
    .await?;
    if channel.peer() != expected_worker || channel.scope() != Some(barrier_scope) {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    channel
        .send(&ControlMessage::NoResultRetireThrough(request))
        .await?;
    let ControlMessage::NoResultRetirementApplied(applied) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    if !applied.matches_retirement(&request, expected_worker) {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    channel.finish().await?;
    Ok(applied)
}

/// Worker-side listener bootstrap. It authenticates a preconfigured coordinator, then
/// learns the exact assignment scope from that coordinator's first valid Offer.
pub async fn accept_compiler_control(
    endpoint: &Endpoint,
    allowed_coordinators: impl IntoIterator<Item = CompilerPeerId>,
) -> Result<ControlChannel, CompilerTransportBridgeError> {
    let coordinators = endpoint_ids(allowed_coordinators)?;
    let policy = ControlAdmissionPolicy::worker(endpoint.id(), coordinators);
    Ok(accept_control(endpoint, &policy).await?)
}

/// Accepts a subsequent page/result control stream after the worker adopted the Offer scope.
pub async fn accept_compiler_control_for_scope(
    endpoint: &Endpoint,
    allowed_coordinators: impl IntoIterator<Item = CompilerPeerId>,
    scope: AssignmentScope,
) -> Result<ControlChannel, CompilerTransportBridgeError> {
    let coordinators = endpoint_ids(allowed_coordinators)?;
    let policy =
        ControlAdmissionPolicy::new(endpoint.id(), coordinators, scope, ControlRole::Worker);
    Ok(accept_control(endpoint, &policy).await?)
}

/// Receives the first exact Offer and returns the worker task descriptor.
pub async fn receive_compiler_offer(
    channel: &mut ControlChannel,
) -> Result<ControlOffer, CompilerTransportBridgeError> {
    let ControlMessage::Offer(offer) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    if channel.scope() != Some(offer.scope) {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(offer)
}

/// Admits the manifest object fetched through the input closure named by the Offer.
///
/// The caller must obtain `object` from its verified `input_closure` view; this function then
/// checks the closure receipt, deterministic manifest ObjectId, invocation fields, and work ID.
pub fn admit_compiler_input_manifest(
    offer: &ControlOffer,
    input_closure_id: [u8; 32],
    object: &backend_store::TypedObject,
) -> Result<CompilerInputManifestV1, CompilerTransportBridgeError> {
    if input_closure_id != offer.input_closure_id {
        return Err(CompilerTransportBridgeError::InputManifestMismatch);
    }
    let manifest = CompilerInputManifestV1::from_typed_object(object)?;
    if *object.id().as_bytes() != offer.input_manifest_object_id || !manifest.matches_offer(offer) {
        return Err(CompilerTransportBridgeError::InputManifestMismatch);
    }
    Ok(manifest)
}

/// Admits a V2 manifest fetched from the exact, already-reopened input closure named by an Offer.
pub fn admit_compiler_input_manifest_v2(
    offer: &ControlOffer,
    checked_input_closure: ClosureId,
    object: &backend_store::TypedObject,
) -> Result<CompilerInputManifestV2, CompilerTransportBridgeError> {
    Ok(CompilerInputManifestV2::admit_for_offer(
        object,
        offer,
        checked_input_closure,
    )?)
}

/// Receives the worker's admission decision on the coordinator side.
pub async fn receive_compiler_accept(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
) -> Result<bool, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let ControlMessage::Accept(accept) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    if accept.scope != compiler_assignment_scope(assignment, namespace_id)? {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(accept.accepted)
}

/// Receives a cancellation on the worker side, bound to the adopted Offer scope.
pub async fn receive_compiler_cancel(
    channel: &mut ControlChannel,
    scope: AssignmentScope,
) -> Result<CancelReason, CompilerTransportBridgeError> {
    let ControlMessage::Cancel(cancel) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    if cancel.scope != scope || channel.scope() != Some(scope) {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(cancel.reason)
}

/// Receives one exact page of input or result capabilities on the worker side.
pub async fn receive_compiler_grant_page(
    channel: &mut ControlChannel,
    scope: AssignmentScope,
    direction: GrantDirection,
    page_index: u32,
) -> Result<ControlGrantPage, CompilerTransportBridgeError> {
    let ControlMessage::GrantPage(page) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    if page.scope != scope || page.direction != direction || page.page_index != page_index {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(page)
}

/// Receives every owner-to-worker input page, accepting a new authenticated stream at each
/// transport message limit. `page_count` comes from the exact Offer.
pub async fn receive_compiler_grant_pages(
    endpoint: &Endpoint,
    coordinator: CompilerPeerId,
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    page_count: u32,
) -> Result<Vec<ControlGrantPage>, CompilerTransportBridgeError> {
    if page_count > MAX_CONTROL_GRANT_PAGES {
        return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
    }
    let capacity =
        usize::try_from(page_count).map_err(CompilerTransportBridgeError::GrantPageCount)?;
    let mut pages = Vec::with_capacity(capacity);
    let transfer_scope = transfer_scope_from_claim(scope, input_closure_id)?;
    let mut next_index = 0_u32;
    let pages_per_stream = u32::try_from(COMPILER_GRANT_PAGES_PER_CONTROL_STREAM)
        .map_err(CompilerTransportBridgeError::GrantPageCount)?;
    while next_index < page_count {
        let mut channel = accept_compiler_control_for_scope(endpoint, [coordinator], scope).await?;
        let batch_end = next_index.saturating_add(pages_per_stream).min(page_count);
        while next_index < batch_end {
            let page = receive_compiler_grant_page(
                &mut channel,
                scope,
                GrantDirection::InputsToWorker,
                next_index,
            )
            .await?;
            if page.page_count != page_count
                || page
                    .grants
                    .iter()
                    .any(|grant| grant.claims.scope != transfer_scope)
            {
                return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
            }
            pages.push(page);
            next_index += 1;
        }
        channel.finish().await?;
    }
    Ok(pages)
}

/// Receives one result page only from the active assigned peer and exact result closure.
pub async fn receive_compiler_result_grant_page(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    closure_id: [u8; 32],
    page_index: u32,
) -> Result<ControlGrantPage, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    if channel
        .scope()
        .is_some_and(|channel_scope| channel_scope != scope)
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    let ControlMessage::GrantPage(page) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    admit_compiler_result_grant_page(
        channel,
        scheduler,
        assignment,
        namespace_id,
        closure_id,
        page_index,
        page,
    )
}

/// Admits a result page already decoded by a shared listener dispatcher.
pub fn admit_compiler_result_grant_page(
    channel: &ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    closure_id: [u8; 32],
    page_index: u32,
    page: ControlGrantPage,
) -> Result<ControlGrantPage, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    let transfer_scope = transfer_scope_from_claim(scope, closure_id)?;
    if channel.scope() != Some(scope)
        || page.scope != scope
        || page.direction != GrantDirection::ResultsToCoordinator
        || page.page_index != page_index
        || page.grants.is_empty()
        || page
            .grants
            .iter()
            .any(|grant| grant.claims.scope != transfer_scope)
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(page)
}

/// Collects one receipt's result-grant pages across bounded control streams.
///
/// Coordinator runtimes that own a [`backend_cluster_transport::ClusterListener`] feed each
/// accepted control channel to [`Self::receive_channel`]. This keeps endpoint acceptance
/// centralized so result, reject, failure, cancellation, and Bao connections can be demultiplexed
/// without competing `Endpoint::accept` loops.
#[derive(Debug)]
pub struct CompilerResultGrantPageReceiver {
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    closure_id: [u8; 32],
    page_count: u32,
    next_index: u32,
    pages: Vec<ControlGrantPage>,
}

impl CompilerResultGrantPageReceiver {
    /// Bind a page series to the exact admitted receipt and active scheduler assignment.
    pub fn new(
        scheduler: &CompilerClusterScheduler,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        result: FencedCompilerResult,
    ) -> Result<Self, CompilerTransportBridgeError> {
        if result.assignment != assignment
            || result.receipt.scope != compiler_assignment_scope(assignment, namespace_id)?
        {
            return Err(CompilerTransportBridgeError::ScopeMismatch);
        }
        scheduler.validate_remote_completion(assignment, result.completion)?;
        let page_count = result.receipt.result_grant_pages;
        if page_count == 0 || page_count > MAX_CONTROL_GRANT_PAGES {
            return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
        }
        let capacity =
            usize::try_from(page_count).map_err(CompilerTransportBridgeError::GrantPageCount)?;
        Ok(Self {
            assignment,
            namespace_id,
            closure_id: result.receipt.closure_id,
            page_count,
            next_index: 0,
            pages: Vec::with_capacity(capacity),
        })
    }

    /// Number of contiguous pages still required by the admitted result receipt.
    #[must_use]
    pub const fn remaining_pages(&self) -> u32 {
        self.page_count - self.next_index
    }

    /// Whether all receipt-declared result pages have been admitted.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.next_index == self.page_count
    }

    /// Receives the next contiguous bounded batch on one already accepted control channel.
    ///
    /// The channel must be admitted by the transport listener. This method checks its
    /// authenticated peer and any prebound scope, then verifies the exact scope on each decoded
    /// page. A batch advances the receiver only after the stream's bounded finish handshake, so
    /// the same deterministic page indexes can be replayed after a lost connection.
    pub async fn receive_channel(
        &mut self,
        scheduler: &CompilerClusterScheduler,
        mut channel: ControlChannel,
    ) -> Result<(), CompilerTransportBridgeError> {
        if self.is_complete() {
            return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
        }
        scheduler.validate_assignment(self.assignment)?;
        verify_control_peer(&channel, self.assignment)?;
        let expected_scope = compiler_assignment_scope(self.assignment, self.namespace_id)?;
        if channel.scope().is_some_and(|scope| scope != expected_scope) {
            return Err(CompilerTransportBridgeError::ScopeMismatch);
        }
        let ControlMessage::GrantPage(first_page) = channel.receive().await? else {
            return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
        };
        self.receive_channel_with_first(scheduler, channel, first_page)
            .await
    }

    /// Receives the rest of a page batch after a shared ingress dispatcher decoded and routed its
    /// first page by the now-adopted assignment scope.
    pub async fn receive_channel_with_first(
        &mut self,
        scheduler: &CompilerClusterScheduler,
        mut channel: ControlChannel,
        first_page: ControlGrantPage,
    ) -> Result<(), CompilerTransportBridgeError> {
        if self.is_complete() {
            return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
        }
        scheduler.validate_assignment(self.assignment)?;
        verify_control_peer(&channel, self.assignment)?;
        let expected_scope = compiler_assignment_scope(self.assignment, self.namespace_id)?;
        if channel.scope() != Some(expected_scope) {
            return Err(CompilerTransportBridgeError::ScopeMismatch);
        }
        let pages_per_stream = u32::try_from(COMPILER_GRANT_PAGES_PER_CONTROL_STREAM)
            .map_err(CompilerTransportBridgeError::GrantPageCount)?;
        let batch_start = self.next_index;
        let batch_end = batch_start
            .saturating_add(pages_per_stream)
            .min(self.page_count);
        let mut next_index = batch_start;
        let mut batch = Vec::with_capacity((batch_end - batch_start) as usize);
        let first_page = admit_compiler_result_grant_page(
            &channel,
            scheduler,
            self.assignment,
            self.namespace_id,
            self.closure_id,
            next_index,
            first_page,
        )?;
        if first_page.page_count != self.page_count {
            return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
        }
        batch.push(first_page);
        next_index += 1;
        while next_index < batch_end {
            let page = receive_compiler_result_grant_page(
                &mut channel,
                scheduler,
                self.assignment,
                self.namespace_id,
                self.closure_id,
                next_index,
            )
            .await?;
            if page.page_count != self.page_count {
                return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
            }
            batch.push(page);
            next_index += 1;
        }
        channel.finish().await?;
        self.pages.extend(batch);
        self.next_index = batch_end;
        Ok(())
    }

    /// Returns the complete contiguous page series after every declared page was received.
    pub fn into_pages(self) -> Result<Vec<ControlGrantPage>, CompilerTransportBridgeError> {
        if !self.is_complete() || self.pages.len() != self.page_count as usize {
            return Err(CompilerTransportBridgeError::InvalidGrantPageSeries);
        }
        Ok(self.pages)
    }
}

/// Receives all result grant pages from the exact assigned worker, reopening control streams
/// after each bounded batch and checking every grant against the receipt closure.
///
/// Multi-assignment coordinators should instead feed channels from their one shared listener to
/// [`CompilerResultGrantPageReceiver::receive_channel`].
pub async fn receive_compiler_result_grant_pages(
    scheduler: &CompilerClusterScheduler,
    endpoint: &Endpoint,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    result: FencedCompilerResult,
) -> Result<Vec<ControlGrantPage>, CompilerTransportBridgeError> {
    let mut receiver =
        CompilerResultGrantPageReceiver::new(scheduler, assignment, namespace_id, result)?;
    while !receiver.is_complete() {
        let mut channel =
            accept_coordinator_control(scheduler, endpoint, assignment, namespace_id).await?;
        receiver.receive_channel(scheduler, channel).await?;
    }
    receiver.into_pages()
}

/// Sends the worker's result receipt after its output closure has been finalized.
pub async fn send_worker_result_receipt(
    channel: &mut ControlChannel,
    receipt: ControlResultReceipt,
) -> Result<(), CompilerTransportBridgeError> {
    if channel.scope() != Some(receipt.scope) {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    channel
        .send(&ControlMessage::ResultReceipt(receipt))
        .await?;
    Ok(())
}

/// Opens a fresh coordinator stream for a durable stored-result acknowledgement.
///
/// Unlike placement, result admission, and rejection, this operation remains valid after the
/// scheduler retires the completed assignment: a lost ACK must be safe to retry from the opaque
/// stored candidate and its checked closure receipt.
pub async fn connect_stored_compiler_result_ack(
    endpoint: &Endpoint,
    peer_addr: EndpointAddr,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    result: StoredRemoteCompilerResult,
) -> Result<ControlChannel, CompilerTransportBridgeError> {
    let scope = validate_stored_result_for_ack(assignment, namespace_id, result)?;
    Ok(connect_control(
        endpoint,
        peer_addr,
        remote_endpoint_id(assignment)?,
        scope,
        ControlRole::Coordinator,
    )
    .await?)
}

/// Opens a result-ACK stream for an exact durable superseded-attempt proof.
///
/// This deliberately does not require the volatile scheduler slot to remain live. The
/// local-service caller must validate the persisted Turso proof and bind it to the worker's
/// pending result row before opening this stream.
pub async fn connect_superseded_compiler_result_ack(
    endpoint: &Endpoint,
    peer_addr: EndpointAddr,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
) -> Result<ControlChannel, CompilerTransportBridgeError> {
    let peer = remote_endpoint_id(assignment)?;
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    Ok(connect_control(endpoint, peer_addr, peer, scope, ControlRole::Coordinator).await?)
}

/// Sends the stored acknowledgement only for a result whose opaque candidate matches this exact
/// assignment and whose checked storage-only closure receipt matches the worker's result claim.
/// The assignment may already have completed in the scheduler; this ACK is storage-only and can
/// be retried without selecting a head.
pub async fn send_stored_compiler_result_ack(
    channel: &mut ControlChannel,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    result: StoredRemoteCompilerResult,
) -> Result<(), CompilerTransportBridgeError> {
    verify_control_peer(channel, assignment)?;
    let scope = validate_stored_result_for_ack(assignment, namespace_id, result)?;
    let receipt = result.receipt();
    send_result_ack(
        channel,
        scope,
        receipt.closure_id,
        ResultAckDisposition::Stored,
    )
    .await
}

fn validate_stored_result_for_ack(
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    result: StoredRemoteCompilerResult,
) -> Result<AssignmentScope, CompilerTransportBridgeError> {
    let candidate = result.candidate();
    let receipt = result.receipt();
    if candidate.work() != assignment.work()
        || candidate.token() != assignment.token()
        || candidate.peer() != remote_peer(assignment)?
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    let scope = compiler_assignment_scope(assignment, namespace_id)?;
    if receipt.scope != scope
        || receipt.target_root == [0; 32]
        || receipt.pack_id.is_some_and(|pack_id| pack_id == [0; 32])
        || receipt.closure_id != *candidate.closure_receipt().closure().as_bytes()
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    if u64::from(receipt.object_count) != candidate.closure_receipt().object_count()
        || receipt.payload_bytes != candidate.closure_receipt().payload_bytes()
        || candidate.closure_receipt().bytes_verified() < receipt.payload_bytes
    {
        return Err(CompilerTransportBridgeError::ResultMetadataMismatch);
    }
    Ok(scope)
}

/// Sends a terminal rejection after the owner cannot admit the worker's exact result closure.
pub async fn send_rejected_compiler_result_ack(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    closure_id: [u8; 32],
    reason: ResultRejectReason,
) -> Result<(), CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    verify_control_peer(channel, assignment)?;
    send_result_ack(
        channel,
        compiler_assignment_scope(assignment, namespace_id)?,
        closure_id,
        ResultAckDisposition::Rejected(reason),
    )
    .await
}

/// Sends the terminal stale-scope rejection after the owner has verified a durable
/// superseded-attempt proof for this exact result.
///
/// This transport-only primitive deliberately does not consult the scheduler's in-memory active
/// map: a worker may replay a retained result after the owner restarted, when that map is empty.
/// The local-service authority adapter must first re-read Turso's opaque superseded-attempt proof
/// and bind its namespace, attempt epoch, fence, input digest, and newer current epoch to the
/// durable result row. This function then binds the rejection to the exact assigned peer,
/// scheduler-derived assignment scope, and worker-reported result closure. It grants no storage,
/// candidate, or head-selection authority.
///
/// # Errors
///
/// Returns an error for local assignments, a mismatched authenticated peer/scope, or a zero
/// closure ID. Call [`send_rejected_compiler_result_ack`] for rejections of a still-live result.
pub async fn send_superseded_compiler_result_ack(
    channel: &mut ControlChannel,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    closure_id: [u8; 32],
) -> Result<(), CompilerTransportBridgeError> {
    verify_control_peer(channel, assignment)?;
    if closure_id == [0; 32] {
        return Err(CompilerTransportBridgeError::ResultMetadataMismatch);
    }
    send_result_ack(
        channel,
        compiler_assignment_scope(assignment, namespace_id)?,
        closure_id,
        ResultAckDisposition::Rejected(ResultRejectReason::Scope),
    )
    .await
}

async fn send_result_ack(
    channel: &mut ControlChannel,
    scope: AssignmentScope,
    closure_id: [u8; 32],
    disposition: ResultAckDisposition,
) -> Result<(), CompilerTransportBridgeError> {
    if channel.scope() != Some(scope) {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    channel
        .send(&ControlMessage::ResultAck(ControlResultAck {
            scope,
            closure_id,
            disposition,
        }))
        .await?;
    Ok(())
}

/// Receives the owner's terminal result disposition before the worker releases retained bytes.
pub async fn receive_worker_result_ack(
    channel: &mut ControlChannel,
    expected_coordinator: EndpointId,
    scope: AssignmentScope,
    closure: ClosureId,
) -> Result<ResultAckDisposition, CompilerTransportBridgeError> {
    if channel.peer() != expected_coordinator {
        return Err(CompilerTransportBridgeError::ControlPeerMismatch);
    }
    let ControlMessage::ResultAck(ack) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    if channel.scope() != Some(scope) || ack.scope != scope || ack.closure_id != *closure.as_bytes()
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(ack.disposition)
}

/// Receives and admits one worker result receipt before any result bytes enter the CAS sink.
pub async fn receive_compiler_result(
    channel: &mut ControlChannel,
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
) -> Result<FencedCompilerResult, CompilerTransportBridgeError> {
    verify_control_peer(channel, assignment)?;
    let ControlMessage::ResultReceipt(receipt) = channel.receive().await? else {
        return Err(CompilerTransportBridgeError::UnexpectedControlMessage);
    };
    admit_compiler_result(scheduler, assignment, namespace_id, receipt)
}

/// Fetches one authorized Bao range only when its full scope matches the active assignment.
pub async fn fetch_compiler_object_range(
    scheduler: &CompilerClusterScheduler,
    endpoint: &Endpoint,
    server_addr: EndpointAddr,
    trusted_issuer: EndpointId,
    capability: Capability,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    closure: ClosureId,
    checkpoint_path: impl AsRef<Path>,
) -> Result<ResumeState, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    let assignment_scope = compiler_assignment_scope(assignment, namespace_id)?;
    fetch_granted_object_range(
        endpoint,
        server_addr,
        trusted_issuer,
        capability,
        assignment_scope,
        closure,
        checkpoint_path,
    )
    .await
}

/// Serves one admitted Bao range after checking that the listener policy is fenced to the
/// same assignment, closure, local endpoint, and assigned worker.
pub async fn serve_compiler_object_range(
    scheduler: &CompilerClusterScheduler,
    endpoint: &Endpoint,
    state: &ServerState,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    closure: ClosureId,
) -> Result<(), CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    let expected_scope = compiler_transfer_scope(assignment, namespace_id, closure)?;
    serve_granted_object_range(
        endpoint,
        state,
        expected_scope,
        remote_endpoint_id(assignment)?,
    )
    .await
}

/// Fetches one Bao grant from either side of a compiler assignment using the Offer's exact
/// assignment scope. This is the worker-side counterpart to the coordinator convenience API.
pub async fn fetch_granted_object_range(
    endpoint: &Endpoint,
    server_addr: EndpointAddr,
    trusted_issuer: EndpointId,
    capability: Capability,
    assignment_scope: AssignmentScope,
    closure: ClosureId,
    checkpoint_path: impl AsRef<Path>,
) -> Result<ResumeState, CompilerTransportBridgeError> {
    let expected_scope = TransferScope::from_store_closure(assignment_scope, closure)?;
    if capability.claims.scope != expected_scope
        || capability.claims.client != endpoint.id()
        || capability.claims.server != server_addr.id
        || trusted_issuer != server_addr.id
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(fetch_range(
        endpoint,
        server_addr,
        trusted_issuer,
        capability,
        expected_scope,
        checkpoint_path,
    )
    .await?)
}

/// Fetches a signed Bao range and feeds the complete, reverified payload into an exact
/// backend-store artifact session. Partial ranges remain checkpointed and do not enter CAS.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_object_into_artifact_session(
    endpoint: &Endpoint,
    server_addr: EndpointAddr,
    trusted_issuer: EndpointId,
    capability: Capability,
    assignment_scope: AssignmentScope,
    closure: ClosureId,
    checkpoint_path: impl AsRef<Path>,
    object_index: usize,
    session: &mut ArtifactSession,
) -> Result<ResumeState, CompilerTransportBridgeError> {
    let resume = fetch_granted_object_range(
        endpoint,
        server_addr,
        trusted_issuer,
        capability,
        assignment_scope,
        closure,
        &checkpoint_path,
    )
    .await?;
    if resume.is_complete() {
        resume
            .feed_to_artifact_session(checkpoint_path, object_index, session)
            .await?;
    }
    Ok(resume)
}

/// Serves one Bao grant using an assignment scope learned from the worker Offer or held by the
/// coordinator. The expected requester is still the Iroh-authenticated allowlisted endpoint.
pub async fn serve_granted_object_range(
    endpoint: &Endpoint,
    state: &ServerState,
    expected_scope: TransferScope,
    expected_peer: EndpointId,
) -> Result<(), CompilerTransportBridgeError> {
    if state.policy.scope != expected_scope
        || state.policy.server != endpoint.id()
        || state.policy.issuer != endpoint.id()
        || !state.policy.allowed_peers.contains(&expected_peer)
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    Ok(serve_one(endpoint, state).await?)
}

/// Fenced control result admitted by the scheduler before opening its CAS sink.
///
/// Its receipt fields remain untrusted claims. After Bao and `ArtifactSink` finish,
/// call [`store_compiler_result`] to bind them to the durable closure receipt.
#[derive(Clone, Copy, Debug)]
pub struct FencedCompilerResult {
    assignment: CompilerAssignment,
    completion: RemoteCompilerCompletionClaim,
    receipt: ControlResultReceipt,
}

/// Worker-side terminal rejection tied to the active remote attempt and exact input closure.
#[derive(Clone, Copy, Debug)]
pub struct FencedCompilerInputReject {
    assignment: CompilerAssignment,
    reason: WorkerRejectReason,
}

/// Terminal worker execution failure admitted against an exact active assignment and input
/// closure. It carries no result closure and grants no publication or selection authority.
#[derive(Clone, Copy, Debug)]
pub struct FencedCompilerExecutionFailure {
    assignment: CompilerAssignment,
    reason: ExecutionFailureReason,
}

impl FencedCompilerExecutionFailure {
    /// Returns the exact remote assignment that failed after input admission.
    #[must_use]
    pub const fn assignment(self) -> CompilerAssignment {
        self.assignment
    }

    /// Returns the worker's closed execution failure reason.
    #[must_use]
    pub const fn reason(self) -> ExecutionFailureReason {
        self.reason
    }
}

impl FencedCompilerInputReject {
    /// Returns the exact remote assignment that the worker could not admit.
    #[must_use]
    pub const fn assignment(self) -> CompilerAssignment {
        self.assignment
    }

    /// Returns the worker's closed rejection reason.
    #[must_use]
    pub const fn reason(self) -> WorkerRejectReason {
        self.reason
    }
}

impl FencedCompilerResult {
    /// The worker's typed but untrusted result metadata.
    #[must_use]
    pub const fn receipt(self) -> ControlResultReceipt {
        self.receipt
    }
}

/// Stored remote result after exact scheduler and local CAS closure checks.
#[derive(Clone, Copy, Debug)]
pub struct StoredRemoteCompilerResult {
    candidate: StoredCompilerCandidate,
    receipt: ControlResultReceipt,
}

impl StoredRemoteCompilerResult {
    /// Exact remote candidate and storage-only closure receipt.
    #[must_use]
    pub const fn candidate(self) -> StoredCompilerCandidate {
        self.candidate
    }

    /// Untrusted target root, pack ID, and closure totals from the worker.
    #[must_use]
    pub const fn receipt(self) -> ControlResultReceipt {
        self.receipt
    }
}

/// Fences a worker ResultReceipt before the owner accepts any result bytes.
pub fn admit_compiler_result(
    scheduler: &CompilerClusterScheduler,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    receipt: ControlResultReceipt,
) -> Result<FencedCompilerResult, CompilerTransportBridgeError> {
    let completion = admitted_result_completion(assignment, namespace_id, receipt)?;
    scheduler.validate_remote_completion(assignment, completion)?;
    Ok(FencedCompilerResult {
        assignment,
        completion,
        receipt,
    })
}

fn admitted_result_completion(
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    receipt: ControlResultReceipt,
) -> Result<RemoteCompilerCompletionClaim, CompilerTransportBridgeError> {
    if receipt.scope != compiler_assignment_scope(assignment, namespace_id)?
        || receipt.target_root == [0; 32]
        || receipt.pack_id.is_some_and(|pack_id| pack_id == [0; 32])
        || receipt.closure_id == [0; 32]
        || receipt.object_count == 0
        || receipt.payload_bytes == 0
    {
        return Err(CompilerTransportBridgeError::ScopeMismatch);
    }
    let payload_chunks = receipt.payload_bytes / COMPILER_BAO_CHUNK_BYTES
        + u64::from(receipt.payload_bytes % COMPILER_BAO_CHUNK_BYTES != 0);
    // At most one extra terminal chunk is needed for every object after the first. Every grant
    // page must contain at least one nonempty range, so this gives a strict upper bound on pages
    // while allowing one large object to span pages.
    let maximum_ranges =
        payload_chunks.saturating_add(u64::from(receipt.object_count.saturating_sub(1)));
    let maximum_pages = maximum_ranges.min(u64::from(MAX_CONTROL_GRANT_PAGES));
    let minimum_pages_for_objects =
        (u64::from(receipt.object_count) - 1) / MAX_OFFER_CAPABILITIES as u64 + 1;
    let max_bytes_per_page = MAX_RANGE_CHUNKS
        .saturating_mul(COMPILER_BAO_CHUNK_BYTES)
        .saturating_mul(MAX_OFFER_CAPABILITIES as u64);
    let minimum_pages_for_bytes = receipt.payload_bytes / max_bytes_per_page
        + u64::from(receipt.payload_bytes % max_bytes_per_page != 0);
    let minimum_pages = minimum_pages_for_objects.max(minimum_pages_for_bytes);
    if receipt.payload_bytes > assignment.work().max_output_bytes()
        || receipt.object_count > MAX_COMPILER_RESULT_OBJECTS
        || receipt.result_grant_pages == 0
        || receipt.result_grant_pages > MAX_CONTROL_GRANT_PAGES
        || u64::from(receipt.result_grant_pages) > maximum_pages
        || u64::from(receipt.result_grant_pages) < minimum_pages
    {
        return Err(CompilerTransportBridgeError::ResultBudgetExceeded);
    }
    let peer = remote_peer(assignment)?;
    let completion = RemoteCompilerCompletionClaim {
        work: assignment.work(),
        token: assignment.token(),
        peer,
        closure: ArtifactClosureClaim::from_bytes(receipt.closure_id),
    };
    Ok(completion)
}

/// Binds a received result's claimed counts and closure to its locally durable CAS receipt.
///
/// This returns a stored candidate only. The owning application still asks Turso to create
/// the exact `CandidateGeneration`, verifies the closure, and compare-and-selects it.
pub fn store_compiler_result(
    scheduler: &CompilerClusterScheduler,
    result: FencedCompilerResult,
    stored: StoredClosureReceipt,
) -> Result<StoredRemoteCompilerResult, CompilerTransportBridgeError> {
    if u64::from(result.receipt.object_count) != stored.object_count()
        || result.receipt.payload_bytes != stored.payload_bytes()
        || result.receipt.closure_id != *stored.closure().as_bytes()
    {
        return Err(CompilerTransportBridgeError::ResultMetadataMismatch);
    }
    let candidate = scheduler.complete_remote(result.assignment, result.completion, stored)?;
    Ok(StoredRemoteCompilerResult {
        candidate,
        receipt: result.receipt,
    })
}

/// Rebuilds a stored remote result after restart from its exact worker receipt and a pinned,
/// independently verified CAS closure. This bypasses only the ephemeral scheduler active-row
/// lookup; it retains exact remote assignment, peer, scope, closure, count, payload, output-bound,
/// and result-page checks. The application must still run full semantic/input admission and its
/// Turso authority must prove the exact current attempt before selection.
pub fn store_recovered_compiler_result(
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    receipt: ControlResultReceipt,
    stored: StoredClosureReceipt,
) -> Result<StoredRemoteCompilerResult, CompilerTransportBridgeError> {
    let completion = admitted_result_completion(assignment, namespace_id, receipt)?;
    if u64::from(receipt.object_count) != stored.object_count()
        || receipt.payload_bytes != stored.payload_bytes()
        || receipt.payload_bytes != stored.bytes_verified()
        || receipt.closure_id != *stored.closure().as_bytes()
    {
        return Err(CompilerTransportBridgeError::ResultMetadataMismatch);
    }
    let candidate = assignment.admit_recovered_completion(completion, stored)?;
    Ok(StoredRemoteCompilerResult { candidate, receipt })
}

fn remote_peer(
    assignment: CompilerAssignment,
) -> Result<CompilerPeerId, CompilerTransportBridgeError> {
    let CompilerAssignmentRoute::Remote(peer) = assignment.route() else {
        return Err(CompilerTransportBridgeError::WrongRoute);
    };
    Ok(peer)
}

fn remote_endpoint_id(
    assignment: CompilerAssignment,
) -> Result<EndpointId, CompilerTransportBridgeError> {
    peer_endpoint_id(remote_peer(assignment)?)
}

fn transfer_scope_from_claim(
    assignment: AssignmentScope,
    closure_id: [u8; 32],
) -> Result<TransferScope, CompilerTransportBridgeError> {
    Ok(TransferScope::new(
        assignment.namespace_id,
        assignment.work_id,
        assignment.attempt,
        assignment.fence,
        closure_id,
    )?)
}

fn verify_control_peer(
    channel: &ControlChannel,
    assignment: CompilerAssignment,
) -> Result<(), CompilerTransportBridgeError> {
    if channel.peer() == remote_endpoint_id(assignment)? {
        Ok(())
    } else {
        Err(CompilerTransportBridgeError::ControlPeerMismatch)
    }
}

fn peer_endpoint_id(peer: CompilerPeerId) -> Result<EndpointId, CompilerTransportBridgeError> {
    EndpointId::from_bytes(&peer.as_bytes()).map_err(|_| CompilerTransportBridgeError::InvalidPeer)
}

fn endpoint_ids(
    peers: impl IntoIterator<Item = CompilerPeerId>,
) -> Result<Vec<EndpointId>, CompilerTransportBridgeError> {
    peers.into_iter().map(peer_endpoint_id).collect()
}

/// Builds the exact object-transfer scope for one remote assignment and stored closure.
pub fn compiler_transfer_scope(
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    closure: ClosureId,
) -> Result<TransferScope, CompilerTransportBridgeError> {
    Ok(TransferScope::from_store_closure(
        compiler_assignment_scope(assignment, namespace_id)?,
        closure,
    )?)
}

/// Issues one bounded, signed Bao range grant for an exact object already materialized from CAS.
///
/// The caller must use an object from the exact admitted input closure. The receiving
/// `ArtifactSink` remains responsible for recomputing typed identities and checking complete
/// closure membership.
///
/// # Errors
///
/// Returns a typed error for local assignments, invalid peer IDs, invalid range
/// sizes, or transport capability validation failures.
#[allow(clippy::too_many_arguments)]
pub fn issue_compiler_object_range(
    scheduler: &CompilerClusterScheduler,
    issuer: &CapabilityIssuer,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    local_endpoint: EndpointId,
    closure: ClosureId,
    member: VerifiedStoreClosureMember,
    range: ChunkRange,
    byte_budget: u32,
    expires_at_unix_ms: u64,
    nonce: [u8; 16],
) -> Result<Capability, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    if issuer.public_key() != local_endpoint {
        return Err(CompilerTransportBridgeError::GrantEndpointMismatch);
    }
    let CompilerAssignmentRoute::Remote(peer) = assignment.route() else {
        return Err(CompilerTransportBridgeError::WrongRoute);
    };
    if member.closure() != closure {
        return Err(CompilerTransportBridgeError::ClosureMemberMismatch);
    }
    let object = member.materialized();
    let peer = EndpointId::from_bytes(&peer.as_bytes())
        .map_err(|_| CompilerTransportBridgeError::InvalidPeer)?;
    let (client, server) = (peer, local_endpoint);
    let payload_length = object.object.payload_length;
    range
        .validate(payload_length)
        .map_err(|code| TransportError::Rejected(code))?;
    let scope = compiler_transfer_scope(assignment, namespace_id, closure)?;
    let claims = CapabilityClaims {
        client,
        server,
        scope,
        blob_hash: object.blob_hash,
        object: object.object,
        range,
        byte_budget,
        expires_at_unix_ms,
        nonce,
    };
    Ok(issuer.issue(claims)?)
}

/// Issues one result range grant on the worker after the owner has admitted its exact Offer.
///
/// The worker signs its own result grants. The coordinator pins that authenticated worker key
/// as the trusted issuer and additionally requires every capability's closure to match the
/// worker's `ControlResultReceipt`.
#[allow(clippy::too_many_arguments)]
pub fn issue_worker_result_object_range(
    issuer: &CapabilityIssuer,
    assignment_scope: AssignmentScope,
    owner_endpoint: EndpointId,
    worker_endpoint: EndpointId,
    closure: ClosureId,
    object: MaterializedObject,
    range: ChunkRange,
    byte_budget: u32,
    expires_at_unix_ms: u64,
    nonce: [u8; 16],
) -> Result<Capability, CompilerTransportBridgeError> {
    if issuer.public_key() != worker_endpoint {
        return Err(CompilerTransportBridgeError::GrantEndpointMismatch);
    }
    let payload_length = object.object.payload_length;
    range
        .validate(payload_length)
        .map_err(|code| TransportError::Rejected(code))?;
    let scope = TransferScope::from_store_closure(assignment_scope, closure)?;
    let claims = CapabilityClaims {
        client: owner_endpoint,
        server: worker_endpoint,
        scope,
        blob_hash: object.blob_hash,
        object: object.object,
        range,
        byte_budget,
        expires_at_unix_ms,
        nonce,
    };
    Ok(issuer.issue(claims)?)
}

/// Materializes one checked CAS object to a bounded Bao catalog, then issues its signed range.
///
/// This keeps large compiler objects on disk throughout Bao outboard construction and transfer.
#[allow(clippy::too_many_arguments)]
pub async fn issue_compiler_range_from_store(
    scheduler: &CompilerClusterScheduler,
    catalog: &StoreBlobCatalog,
    object_id: UntrustedObjectId,
    issuer: &CapabilityIssuer,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    local_endpoint: EndpointId,
    closure: ClosureId,
    range: ChunkRange,
    byte_budget: u32,
    expires_at_unix_ms: u64,
    nonce: [u8; 16],
) -> Result<Capability, CompilerTransportBridgeError> {
    scheduler.validate_assignment(assignment)?;
    let Some(member) = catalog.register_closure_member(closure, object_id).await? else {
        return Err(CompilerTransportBridgeError::Transport(
            TransportError::ObjectUnavailable,
        ));
    };
    issue_compiler_object_range(
        scheduler,
        issuer,
        assignment,
        namespace_id,
        local_endpoint,
        closure,
        member,
        range,
        byte_budget,
        expires_at_unix_ms,
        nonce,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_replication::{AttemptId, Fence};

    #[test]
    fn authority_fence_is_preserved_as_a_full_transport_attempt_token() {
        let mut fence = [0; 32];
        fence[0] = 0xA5;
        fence[31] = 0x5A;
        let token = CompilerAttemptToken::new(
            AttemptId::new(17).expect("nonzero authority attempt"),
            Fence::from_bytes(fence).expect("valid authority fence"),
        );
        assert_eq!(token.attempt().get(), 17);
        assert_eq!(token.fence().as_bytes(), fence);
        assert!(AttemptId::new(0).is_err());
        assert!(Fence::from_bytes([0; 32]).is_err());
    }
}
