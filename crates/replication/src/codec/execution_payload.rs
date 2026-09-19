//! Payload encoding for one replication message family.

use crate::codec::primitives::{
    Reader, Writer, read_authority, read_authority_policy, read_coverage, read_identity,
    read_workspace, write_authority, write_authority_policy, write_coverage, write_identity,
    write_workspace,
};
use crate::{
    CancelAttempt, CancellationId, ReplicationError, ResourceEnvelope, TransportLimits,
    WireRecipeRequest, WireRecipeResult, WireSemanticCoverage,
};

pub(super) fn write_resources(
    writer: &mut Writer,
    resources: ResourceEnvelope,
) -> Result<(), ReplicationError> {
    writer.u64(resources.cpu_millis)?;
    writer.u64(resources.memory_bytes)?;
    writer.u64(resources.network_bytes)?;
    writer.u64(resources.storage_bytes)?;
    writer.u64(resources.output_bytes)?;
    writer.u32(resources.processes)?;
    writer.u64(resources.wall_millis)
}

pub(super) fn read_resources(
    reader: &mut Reader<'_>,
) -> Result<ResourceEnvelope, ReplicationError> {
    Ok(ResourceEnvelope {
        cpu_millis: reader.u64()?,
        memory_bytes: reader.u64()?,
        network_bytes: reader.u64()?,
        storage_bytes: reader.u64()?,
        output_bytes: reader.u64()?,
        processes: reader.u32()?,
        wall_millis: reader.u64()?,
    })
}

pub(super) fn write_fence(
    writer: &mut Writer,
    fence: crate::Fence,
) -> Result<(), ReplicationError> {
    writer.fixed(&fence.as_bytes())
}

pub(super) fn read_fence(reader: &mut Reader<'_>) -> Result<crate::Fence, ReplicationError> {
    crate::Fence::from_bytes(reader.fixed()?)
}

pub(super) fn write_cancellation(
    writer: &mut Writer,
    cancellation: CancellationId,
) -> Result<(), ReplicationError> {
    writer.fixed(&cancellation.as_bytes())
}

pub(super) fn read_cancellation(
    reader: &mut Reader<'_>,
) -> Result<CancellationId, ReplicationError> {
    CancellationId::new(reader.fixed()?)
}

pub(super) fn write_cancel_attempt(
    writer: &mut Writer,
    cancel: &CancelAttempt,
) -> Result<(), ReplicationError> {
    writer.u64(cancel.attempt.get())?;
    write_identity(writer, cancel.work_key)?;
    write_cancellation(writer, cancel.cancellation)?;
    write_fence(writer, cancel.fence)
}

pub(super) fn read_cancel_attempt(
    reader: &mut Reader<'_>,
) -> Result<CancelAttempt, ReplicationError> {
    Ok(CancelAttempt {
        attempt: crate::AttemptId::new(reader.u64()?)?,
        work_key: read_identity(reader)?,
        cancellation: read_cancellation(reader)?,
        fence: read_fence(reader)?,
    })
}

pub(super) fn write_recipe_request(
    writer: &mut Writer,
    request: &WireRecipeRequest,
) -> Result<(), ReplicationError> {
    writer.u64(request.attempt.get())?;
    write_identity(writer, request.recipe)?;
    write_identity(writer, request.work_key)?;
    writer
        .u32(u32::try_from(request.inputs.len()).map_err(|_| ReplicationError::MessageTooLarge)?)?;
    for input in &request.inputs {
        write_identity(writer, *input)?;
    }
    write_identity(writer, request.read_manifest)?;
    writer.fixed(&request.scope.as_bytes())?;
    write_authority_policy(writer, request.authority)?;
    write_resources(writer, request.resources)?;
    write_fence(writer, request.fence)?;
    write_cancellation(writer, request.cancellation)?;
    write_workspace(writer, request.input_basis)
}

pub(super) fn read_recipe_request(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<WireRecipeRequest, ReplicationError> {
    let attempt = crate::AttemptId::new(reader.u64()?)?;
    let recipe = read_identity(reader)?;
    let work_key = read_identity(reader)?;
    let count = reader.count(limits.max_inputs)?;
    let mut inputs = Vec::with_capacity(count);
    for _ in 0..count {
        inputs.push(read_identity(reader)?);
    }
    Ok(WireRecipeRequest {
        attempt,
        recipe,
        work_key,
        inputs,
        read_manifest: read_identity(reader)?,
        scope: crate::ExecutionScopeId::new(reader.fixed()?)?,
        authority: read_authority_policy(reader)?,
        resources: read_resources(reader)?,
        fence: read_fence(reader)?,
        cancellation: read_cancellation(reader)?,
        input_basis: read_workspace(reader)?,
    })
}

pub(super) fn write_semantic_coverage(
    writer: &mut Writer,
    coverage: &WireSemanticCoverage,
) -> Result<(), ReplicationError> {
    write_identity(writer, coverage.identity)?;
    writer.fixed(&coverage.scope.as_bytes())?;
    write_identity(writer, coverage.read_manifest)?;
    write_authority(writer, coverage.authority)
}

pub(super) fn read_semantic_coverage(
    reader: &mut Reader<'_>,
) -> Result<WireSemanticCoverage, ReplicationError> {
    Ok(WireSemanticCoverage {
        identity: read_identity(reader)?,
        scope: crate::ExecutionScopeId::new(reader.fixed()?)?,
        read_manifest: read_identity(reader)?,
        authority: read_authority(reader)?,
    })
}

pub(super) fn write_recipe_result(
    writer: &mut Writer,
    result: &WireRecipeResult,
) -> Result<(), ReplicationError> {
    writer.u64(result.attempt.get())?;
    write_identity(writer, result.recipe)?;
    write_identity(writer, result.work_key)?;
    write_workspace(writer, result.input_basis)?;
    writer
        .u32(u32::try_from(result.inputs.len()).map_err(|_| ReplicationError::MessageTooLarge)?)?;
    for input in &result.inputs {
        write_identity(writer, *input)?;
    }
    write_identity(writer, result.read_manifest)?;
    write_identity(writer, result.output)?;
    writer.bytes(&result.output_bytes)?;
    writer.fixed(&result.scope.as_bytes())?;
    write_resources(writer, result.resources)?;
    write_coverage(writer, &result.byte_coverage)?;
    write_semantic_coverage(writer, &result.semantic_coverage)?;
    write_authority(writer, result.authority)?;
    write_authority_policy(writer, result.authority_policy)?;
    writer.u64(result.revocation_version.0)?;
    write_fence(writer, result.fence)?;
    write_cancellation(writer, result.cancellation)?;
    match result.attestation {
        Some(attestation) => {
            writer.u8(1)?;
            writer.push(&attestation.0)?;
        }
        None => writer.u8(0)?,
    }
    write_identity(writer, result.receipt)
}

pub(super) fn read_recipe_result(
    reader: &mut Reader<'_>,
    limits: TransportLimits,
) -> Result<WireRecipeResult, ReplicationError> {
    let attempt = crate::AttemptId::new(reader.u64()?)?;
    let recipe = read_identity(reader)?;
    let work_key = read_identity(reader)?;
    let input_basis = read_workspace(reader)?;
    let count = reader.count(limits.max_inputs)?;
    let mut inputs = Vec::with_capacity(count);
    for _ in 0..count {
        inputs.push(read_identity(reader)?);
    }
    let read_manifest = read_identity(reader)?;
    let output = read_identity(reader)?;
    let output_limit = usize::try_from(limits.max_object).unwrap_or(usize::MAX);
    let output_bytes = std::sync::Arc::new(reader.bytes(output_limit)?);
    let scope = crate::ExecutionScopeId::new(reader.fixed()?)?;
    let resources = read_resources(reader)?;
    let byte_coverage = read_coverage(
        reader,
        limits,
        Some(u64::try_from(output_bytes.len()).map_err(|_| ReplicationError::Overflow)?),
    )?;
    let semantic_coverage = read_semantic_coverage(reader)?;
    let authority = read_authority(reader)?;
    let authority_policy = read_authority_policy(reader)?;
    let revocation_version = crate::RevocationVersion(reader.u64()?);
    let fence = read_fence(reader)?;
    let cancellation = read_cancellation(reader)?;
    let attestation = match reader.u8()? {
        0 => None,
        1 => Some(crate::Attestation(reader.take(64).and_then(|bytes| {
            bytes
                .try_into()
                .map_err(|_| ReplicationError::TruncatedFrame)
        })?)),
        _ => return Err(ReplicationError::InvalidWire),
    };
    Ok(WireRecipeResult {
        attempt,
        recipe,
        work_key,
        input_basis,
        inputs,
        read_manifest,
        output,
        output_bytes,
        scope,
        resources,
        byte_coverage,
        semantic_coverage,
        authority,
        authority_policy,
        revocation_version,
        fence,
        cancellation,
        attestation,
        receipt: read_identity(reader)?,
    })
}
