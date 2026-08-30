//! Stateless framed MCP process shell over the in-process application service.

use std::io::{self, BufReader};

use wave_application_core::ApplicationService;
use wave_application_protocol::{decode_mcp, mcp_error, mcp_reply, read_frame, write_frame};

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = stdout.lock();
    let mut service = ApplicationService::new();
    while let Some(frame) = read_frame(&mut input)? {
        let response = match decode_mcp(&frame) {
            Ok(envelope) => mcp_reply(&envelope.id, service.execute(&envelope.input)),
            Err(error) => mcp_error(&serde_json::Value::Null, &error),
        };
        let body = serde_json::to_vec(&response).map_err(io::Error::other)?;
        write_frame(&mut output, &body)?;
    }
    Ok(())
}
