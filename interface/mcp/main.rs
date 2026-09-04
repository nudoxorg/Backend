//! Runs the `interface-mcp` executable, which exists to serve the unified application protocol over framed MCP JSON-RPC.
//! Process setup is kept here while product policy remains in library crates.
//! Every external failure crosses this boundary as a structured diagnostic.
//! Stateless framed MCP process shell over the in-process application service.

use std::io::{self, BufReader};

use interface_core::{
    ApplicationInput, ApplicationOutcome, ApplicationReply, ApplicationService, CorrelationId,
    ExecutionReply, OperationKey, ReplyBody,
};
use interface_protocol::{
    CancellationTarget, McpDecode, McpRequest, McpRequestId, decode_mcp, mcp_error, mcp_initialize,
    mcp_pong, mcp_reply, mcp_tools_list, read_frame, write_frame,
};

/// The concrete application service permits one active adaptive effect, so one inline mapping is
/// the complete request-id cancellation table for this process.
struct ActiveRequest {
    request_id: McpRequestId,
    operation: OperationKey,
    correlation: CorrelationId,
}

#[derive(Default)]
struct ActiveRequests(Option<ActiveRequest>);

impl ActiveRequests {
    fn resolve(&self, target: &CancellationTarget) -> Option<(OperationKey, CorrelationId)> {
        self.0.as_ref().and_then(|active| {
            (active.request_id == target.request_id)
                .then_some((active.operation, active.correlation))
        })
    }

    fn observe(
        &mut self,
        request_id: Option<McpRequestId>,
        input: &ApplicationInput,
        reply: &ApplicationReply,
    ) {
        if starts_effect(input) {
            if let (
                Some(request_id),
                ApplicationOutcome::Resolved(ReplyBody::ExecutionStarted { operation, .. }),
            ) = (request_id, &reply.outcome)
            {
                self.0 = Some(ActiveRequest {
                    request_id,
                    operation: *operation,
                    correlation: reply.correlation,
                });
            }
            return;
        }

        let Some(operation) = named_operation(input) else {
            return;
        };
        if !matches!(
            &reply.outcome,
            ApplicationOutcome::Resolved(
                ReplyBody::ExecutionStarted { .. }
                    | ReplyBody::Execution(ExecutionReply::Pending { .. }),
            )
        ) {
            self.0 = self.0.take().filter(|active| active.operation != operation);
        }
    }
}

fn starts_effect(input: &ApplicationInput) -> bool {
    matches!(
        input,
        ApplicationInput::RecoverLocal { .. }
            | ApplicationInput::RecoverInconsistent(_)
            | ApplicationInput::ReleaseLocal { .. }
    )
}

fn named_operation(input: &ApplicationInput) -> Option<OperationKey> {
    match input {
        ApplicationInput::PollExecution { operation, .. }
        | ApplicationInput::Cancel { operation, .. } => Some(*operation),
        _ => None,
    }
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = stdout.lock();
    let mut service = ApplicationService::new();
    let mut active_requests = ActiveRequests::default();
    while let Some(frame) = read_frame(&mut input)? {
        let body = match decode_mcp(&frame) {
            McpDecode::Accepted(envelope) => {
                match envelope.request {
                    McpRequest::Application(input) => {
                        let reply = service.execute(&input);
                        active_requests.observe(envelope.request_id, &input, &reply);
                        let Some(id) = envelope.id.as_ref() else {
                            // JSON-RPC notifications apply their service side effect without
                            // producing a response frame.
                            continue;
                        };
                        serde_json::to_vec(&mcp_reply(id, reply)).map_err(io::Error::other)?
                    }
                    McpRequest::Initialize(_) => {
                        let Some(id) = envelope.id.as_ref() else {
                            continue;
                        };
                        serde_json::to_vec(&mcp_initialize(id)).map_err(io::Error::other)?
                    }
                    McpRequest::Initialized => {
                        let Some(id) = envelope.id.as_ref() else {
                            continue;
                        };
                        serde_json::to_vec(&mcp_pong(id)).map_err(io::Error::other)?
                    }
                    McpRequest::ListTools => {
                        let Some(id) = envelope.id.as_ref() else {
                            continue;
                        };
                        serde_json::to_vec(&mcp_tools_list(id)).map_err(io::Error::other)?
                    }
                    McpRequest::Ping => {
                        let Some(id) = envelope.id.as_ref() else {
                            continue;
                        };
                        serde_json::to_vec(&mcp_pong(id)).map_err(io::Error::other)?
                    }
                    McpRequest::Cancellation(target) => {
                        let Some((operation, correlation)) = active_requests.resolve(&target)
                        else {
                            // An unknown request id cannot name an operation. It remains a
                            // notification with no response or fabricated fallback operation.
                            continue;
                        };
                        let input = ApplicationInput::Cancel {
                            correlation,
                            operation,
                        };
                        let reply = service.execute(&input);
                        active_requests.observe(None, &input, &reply);
                        // Standard cancellation is always a no-id notification.
                        continue;
                    }
                }
            }
            McpDecode::Rejected(error) => {
                let Some(id) = error.id.as_ref() else {
                    // A failure without a request id cannot be correlated and is a notification-
                    // level error, so it is intentionally not emitted.
                    continue;
                };
                serde_json::to_vec(&mcp_error(id, &error.error)).map_err(io::Error::other)?
            }
        };
        write_frame(&mut output, &body)?;
    }
    Ok(())
}
