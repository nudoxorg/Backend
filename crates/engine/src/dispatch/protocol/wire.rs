//! Wire expectation, cancellation, and worker receipt helpers.

use super::super::DispatchError;
use super::contract::{FenceBinding, RemoteDispatchContract, WorkerReceiptId};
use backend_execution::{AttemptLease, OutputVersion, Scheduled};
use backend_replication::{
    AttemptId, AuthorityExpectation, CancellationId, ExecutionRequestExpectation, ExecutionScopeId,
    ExpectedIdentity, ResourceEnvelope, WireAuthorityPolicy, WireIdentity, WireRecipeRequest,
};
use backend_version::Relation;

/// Returns an exact expected request from a scheduled lease and contract.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn request_expectation<R: Relation>(
    schedule: &Scheduled<R>,
    contract: &RemoteDispatchContract,
) -> Result<ExecutionRequestExpectation, DispatchError> {
    let identity = schedule.identity();
    if !contract.semantic.binds(&identity)
        || contract.authority_epoch != contract.semantic.authority_epoch()
        || contract.revocation_version != contract.semantic.revocation_version()
    {
        return Err(DispatchError::IncompleteSemanticCoverage);
    }
    let attempt_number = u64::from(schedule.lease().ordinal())
        .checked_add(1)
        .ok_or(DispatchError::InvalidAttempt)?;
    let attempt = AttemptId::new(attempt_number).map_err(|_| DispatchError::InvalidAttempt)?;
    let scope = ExecutionScopeId::try_from(contract.semantic.scope())
        .map_err(|_| DispatchError::IncompleteSemanticCoverage)?;
    let fence = FenceBinding::from_lease(schedule.lease())?;
    let cancellation = cancellation_id(schedule.lease())?;
    let inputs = contract.inputs.iter().map(|input| input.expected).collect();
    Ok(ExecutionRequestExpectation {
        attempt,
        recipe: ExpectedIdentity::from_typed(&identity.recipe),
        work_key: ExpectedIdentity::from_typed(&identity.work_key()),
        inputs,
        read_manifest: ExpectedIdentity::from_typed(&identity.read_manifest),
        scope,
        authority: AuthorityExpectation::from_typed(
            &identity.authority,
            contract.authority_epoch,
            contract.revocation_version,
        ),
        resources: contract.resources,
        fence: fence.wire(),
        input_basis: contract.input_basis,
        cancellation,
    })
}

fn cancellation_id(lease: &AttemptLease) -> Result<CancellationId, DispatchError> {
    cancellation_id_for_lease(lease)
}

/// Computes the exact cancellation identity bound to a scheduled attempt.
/// The full fence and ordinal are included so a late cancellation cannot
/// target a replacement attempt.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn cancellation_id_for<R: Relation>(
    schedule: &Scheduled<R>,
) -> Result<CancellationId, DispatchError> {
    cancellation_id_for_lease(schedule.lease())
}

fn cancellation_id_for_lease(lease: &AttemptLease) -> Result<CancellationId, DispatchError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.engine.cancellation.v1\0");
    hasher.update(lease.fence().as_bytes());
    hasher.update(&lease.ordinal().to_be_bytes());
    CancellationId::new(*hasher.finalize().as_bytes()).map_err(|_| DispatchError::InvalidAttempt)
}

/// Builds the canonical request envelope from a retained schedule and exact
/// remote contract. The schedule remains owned by the caller so its
/// cancellation, interner, reservation, and full-fence guards cannot be
/// dropped while the request is in flight.
#[must_use]
pub fn wire_request<R: Relation>(
    schedule: &Scheduled<R>,
    contract: &RemoteDispatchContract,
    expected: &ExecutionRequestExpectation,
) -> WireRecipeRequest {
    let identity = schedule.identity();
    WireRecipeRequest {
        attempt: expected.attempt,
        recipe: WireIdentity::from_typed(&identity.recipe),
        work_key: WireIdentity::from_typed(&identity.work_key()),
        inputs: contract.inputs.iter().map(|input| input.wire).collect(),
        read_manifest: WireIdentity::from_typed(&identity.read_manifest),
        scope: expected.scope,
        authority: WireAuthorityPolicy {
            id: WireIdentity::from_typed(&identity.authority),
            minimum_epoch: contract.authority_epoch,
            revocation_version: contract.revocation_version,
        },
        resources: contract.resources,
        fence: expected.fence,
        cancellation: expected.cancellation,
        input_basis: backend_replication::WorkspaceRootClaim::from_bytes(
            *contract.input_basis.as_bytes(),
        ),
    }
}

/// Computes a receipt identity from complete request material and actual bytes.
#[must_use]
pub fn worker_receipt_id(
    request: &WireRecipeRequest,
    output: OutputVersion,
    bytes: &[u8],
) -> WorkerReceiptId {
    let mut material = Vec::new();
    material.extend_from_slice(b"backend.engine.worker-receipt.v3\0");
    append_wire_identity(&mut material, request.recipe);
    append_wire_identity(&mut material, request.work_key);
    material.extend_from_slice(&request.input_basis.as_bytes());
    material.extend_from_slice(&(request.inputs.len() as u64).to_be_bytes());
    for input in &request.inputs {
        append_wire_identity(&mut material, *input);
    }
    append_wire_identity(&mut material, request.read_manifest);
    material.extend_from_slice(&request.attempt.get().to_be_bytes());
    material.extend_from_slice(&request.fence.as_bytes());
    material.extend_from_slice(&request.cancellation.as_bytes());
    material.extend_from_slice(&request.scope.as_bytes());
    append_wire_identity(&mut material, request.authority.id);
    material.extend_from_slice(&request.authority.minimum_epoch.0.to_be_bytes());
    material.extend_from_slice(&request.authority.revocation_version.0.to_be_bytes());
    append_resources(&mut material, request.resources);
    material.extend_from_slice(&output.to_bytes());
    material.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    material.extend_from_slice(bytes);
    WorkerReceiptId::from_value(material.as_slice())
}

fn append_wire_identity(output: &mut Vec<u8>, identity: WireIdentity) {
    output.extend_from_slice(&identity.as_bytes());
    output.push(identity.context().class());
    output.push(identity.context().domain());
    output.extend_from_slice(&identity.context().ty().to_be_bytes());
    output.push(identity.context().version());
}

fn append_resources(output: &mut Vec<u8>, resources: ResourceEnvelope) {
    output.extend_from_slice(&resources.cpu_millis.to_be_bytes());
    output.extend_from_slice(&resources.memory_bytes.to_be_bytes());
    output.extend_from_slice(&resources.network_bytes.to_be_bytes());
    output.extend_from_slice(&resources.storage_bytes.to_be_bytes());
    output.extend_from_slice(&resources.output_bytes.to_be_bytes());
    output.extend_from_slice(&resources.processes.to_be_bytes());
    output.extend_from_slice(&resources.wall_millis.to_be_bytes());
}
