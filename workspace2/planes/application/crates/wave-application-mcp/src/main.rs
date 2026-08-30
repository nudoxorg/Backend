//! Stateless framed MCP process shell over the in-process application service.

use std::io::{self, BufReader};

use wave_application_core::ApplicationService;
use wave_application_protocol::{
    McpDecode, decode_mcp, mcp_error, mcp_reply, read_frame, write_frame,
};

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = stdout.lock();
    let mut service = ApplicationService::new();
    while let Some(frame) = read_frame(&mut input)? {
        let response = match decode_mcp(&frame) {
            McpDecode::Accepted(envelope) => {
                let reply = service.execute(&envelope.input);
                let Some(id) = envelope.id.as_ref() else {
                    // JSON-RPC notifications, including standard cancellation notifications,
                    // apply their service side effect without producing a response frame.
                    continue;
                };
                mcp_reply(id, reply)
            }
            McpDecode::Rejected(error) => {
                let Some(id) = error.id.as_ref() else {
                    // A failure without a request id cannot be correlated and is a notification-
                    // level error, so it is intentionally not emitted.
                    continue;
                };
                mcp_error(id, &error.error)
            }
        };
        let body = serde_json::to_vec(&response).map_err(io::Error::other)?;
        write_frame(&mut output, &body)?;
    }
    Ok(())
}
