//! Additive conditional projection of existing owner progress commands.
//! Full requests without conditional options remain immediate full observations.

use super::*;
use backend_client::{IndexObservationFingerprint, IndexProgressRead, IndexProgressUpdate};

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
    ) -> Result<Option<Value>, RpcError> {
        if !arguments.contains_key("after_observation") && !arguments.contains_key("wait_millis") {
            return Ok(None);
        }
        let after = arguments
            .get("after_observation")
            .map(|value| serde_json::from_value::<IndexObservationFingerprint>(value.clone()))
            .transpose()
            .map_err(|error| RpcError::invalid(error.to_string()))?;
        let wait = match arguments.get("wait_millis") {
            None => 0,
            Some(value) => value.as_u64().ok_or_else(|| {
                RpcError::invalid("wait_millis must be an integer between 0 and 10000")
            })?,
        };
        let query = IndexProgressRead::new(command.clone(), after, wait)
            .map_err(|error| RpcError::invalid(error.to_string()))?;
        let request_context = self.request_context.clone().unwrap_or_default();
        let update = backend_client::read_index_progress(&query, &request_context, |command| {
            self.product.surface(command)
        })
        .map_err(|error| {
            RpcError::from_fault(&Fault::from_client_error(
                &error,
                surface_error_operand(command),
            ))
        })?;
        Ok(Some(match update {
            IndexProgressUpdate::Changed(reply) => {
                self.surface_reply_result(command, &reply, detail, context)?
            }
            IndexProgressUpdate::Unchanged(reference) => {
                let action = backend_present::Affordance::ObserveIndexChanges {
                    reference: reference.clone(),
                };
                let index_poll = backend_present::IndexPollDto {
                    shell: action.shell().ok_or_else(|| {
                        RpcError::tool("pending progress has no exact CLI observation")
                    })?,
                    call: action.tool_call().ok_or_else(|| {
                        RpcError::tool("pending progress has no exact MCP observation")
                    })?,
                };
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
