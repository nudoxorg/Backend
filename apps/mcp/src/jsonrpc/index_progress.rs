//! Additive conditional projection of existing owner progress commands.
//! Full requests without conditional options remain immediate full observations.

use super::*;
use backend_client::{
    IndexCompilerProgressUpdate, IndexObservationFingerprint, IndexProgressRead,
    IndexProgressUpdate,
};

#[derive(Serialize)]
#[serde(tag = "state", content = "reference", rename_all = "kebab-case")]
enum ProgressProjection<'a> {
    PendingUnchanged(&'a backend_client::IndexProgressReference),
}

#[derive(Serialize)]
#[serde(tag = "result", content = "data", rename_all = "kebab-case")]
enum ConditionalSurface<'a> {
    IndexProgressUnchanged(ProgressProjection<'a>),
}

#[derive(Serialize)]
struct PendingProgressBody<'a> {
    surface: ConditionalSurface<'a>,
    index_poll: backend_present::IndexPollDto,
}

impl<P: Product> Server<P> {
    pub(super) fn conditional_index_progress(
        &mut self,
        arguments: &Map<String, Value>,
        command: &SurfaceCommand,
        detail: Detail,
        context: &[u8],
        exposure: McpExposure,
        compact_receipt: bool,
    ) -> Result<Option<Value>, RpcError> {
        let operation = matches!(command, SurfaceCommand::IndexOperationStatus { .. });
        if !operation
            && !arguments.contains_key("after_observation")
            && !arguments.contains_key("wait_millis")
        {
            return Ok(None);
        }
        let after = arguments
            .get("after_observation")
            .map(|value| serde_json::from_value::<IndexObservationFingerprint>(value.clone()))
            .transpose()
            .map_err(|error| RpcError::invalid(error.to_string()))?;
        let wait = match arguments.get("wait_millis") {
            None if after.is_some() => backend_client::MAX_INDEX_PROGRESS_WAIT_MILLIS,
            None => 0,
            Some(value) => value.as_u64().ok_or_else(|| {
                RpcError::invalid("wait_millis must be an integer between 0 and 10000")
            })?,
        };
        let query = IndexProgressRead::new(command.clone(), after, wait)
            .map_err(|error| RpcError::invalid(error.to_string()))?;
        let request_context = self.request_context.clone().unwrap_or_default();
        let update = if operation {
            backend_client::read_index_operation_progress(&query, &request_context, |command| {
                self.product.surface(command)
            })
        } else {
            let update = backend_client::read_index_progress(&query, &request_context, |command| {
                self.product.surface(command)
            })
            .map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    surface_error_operand(command),
                ))
            })?;
            match update {
                IndexProgressUpdate::Changed(reply) => {
                    return self
                        .surface_reply_result_with_compact_receipt(
                            command,
                            &reply,
                            detail,
                            context,
                            exposure,
                            compact_receipt,
                        )
                        .map(Some);
                }
                IndexProgressUpdate::Unchanged(reference) => {
                    Ok(IndexCompilerProgressUpdate::Unchanged(reference))
                }
            }
        }
        .map_err(|error| {
            RpcError::from_fault(&Fault::from_client_error(
                &error,
                surface_error_operand(command),
            ))
        })?;
        Ok(Some(match update {
            IndexCompilerProgressUpdate::Changed(observed) => self
                .surface_reply_with_profile_progress(
                    command,
                    &SurfaceReply::IndexOperationStatus(observed.observation),
                    observed.profile_progress,
                    detail,
                    context,
                    exposure,
                    compact_receipt,
                )?,
            IndexCompilerProgressUpdate::Unchanged(reference) => {
                let mut index_poll = backend_present::IndexPollDto::for_reference_in_context(
                    &reference,
                    detail,
                    self.product.cli_context().as_ref(),
                )
                .ok_or_else(|| RpcError::tool("pending progress has no exact observation"))?;
                preserve_compact_receipt_in_poll(Some(&mut index_poll), command, compact_receipt);
                let text = format!(
                    "Operation remains pending; the referenced observation is unchanged.\n\nObserve changes with `{}`. Omit the conditional options to read the complete current receipt.",
                    index_poll.call
                );
                let payload = encode_serializable(
                    "index-progress",
                    detail,
                    None,
                    PendingProgressBody {
                        surface: ConditionalSurface::IndexProgressUnchanged(
                            ProgressProjection::PendingUnchanged(&reference),
                        ),
                        index_poll,
                    },
                    DEFAULT_RESPONSE_BUDGET_BYTES,
                )
                .map_err(|error| RpcError::from_fault(&oversized_fault(error)))?;
                let structured = serde_json::from_slice(&payload.bytes).map_err(|error| {
                    RpcError::tool(format!(
                        "conditional progress projection decode failed: {error}"
                    ))
                })?;
                tool_result(&text, structured, false)
            }
        }))
    }
}
