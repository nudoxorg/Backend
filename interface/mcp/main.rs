//! Runs the `nudox-mcp` executable, which exists to serve the one shared local library to agents over MCP.
//! Process setup is kept here while product policy remains in library crates.
//! Every external failure crosses this boundary as a structured diagnostic.
//! Frames in, one dispatch, Markdown out: the process shell owns nothing else.

use std::io::{self, BufReader, Write as _};

use interface_library::{CompilerAttachment, Library, OpenOptions, WorkspaceRoot};
use interface_mcp::server::{Server, fault_response};
use interface_protocol::{
    mcp::{MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES, decode},
    read_frame_bounded, write_frame_bounded,
};

fn main() -> io::Result<()> {
    let root = WorkspaceRoot::resolve().map_err(|error| io::Error::other(format!("{error:?}")))?;
    let library = Library::open(OpenOptions {
        root,
        compiler: attachment(),
    })
    .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let mut server = Server::new(library);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = stdout.lock();
    loop {
        let frame = match read_frame_bounded(&mut input, MAX_REQUEST_FRAME_BYTES) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                // A frame that never parsed carries no request identity, so there is nobody to
                // answer. The process reports it once and stops rather than resynchronising onto
                // a byte stream whose boundaries it no longer trusts.
                return Err(error);
            }
        };
        let response = match decode(&frame) {
            Ok(envelope) => server.handle(envelope, &mut output)?,
            Err(fault) => fault_response(&fault),
        };
        if let Some(response) = response {
            let body = serde_json::to_vec(&response).map_err(io::Error::other)?;
            write_frame_bounded(&mut output, &body, MAX_RESPONSE_FRAME_BYTES)?;
        }
        output.flush()?;
    }
    Ok(())
}

/// Whether this process brings a compiler.
///
/// A server started without one is not broken: it reads everything already on the shelf and refuses
/// `add` with `compiler-detached`, which `health` then explains. That is the honest read-only mode,
/// and it is what the integration tests exercise.
fn attachment() -> CompilerAttachment {
    if std::env::var_os("NUDOX_MCP_DETACHED").is_some() {
        CompilerAttachment::Detached
    } else {
        CompilerAttachment::Production
    }
}
