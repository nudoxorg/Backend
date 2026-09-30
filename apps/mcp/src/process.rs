//! MCP process startup, stdio framing, and endpoint composition.

#[cfg(any(unix, windows))]
use crate::UnixCommandTransport;
use crate::{
    MAX_ENDPOINT_PATH, decode_request, dispatch_frame_with_transport, error_frame,
    error_frame_for_input,
};
use backend_replication::{LocalControlError, read_frame as read_local_frame};
use std::io::{self, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Mcp,
    Framed,
    #[cfg(any(unix, windows))]
    Http(crate::http::LoopbackBind),
}

struct Options {
    paths: backend_runtime::WorkspacePaths,
    mode: Mode,
}

fn read_stdio_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let body = match read_local_frame(reader, crate::protocol::control_limits()) {
        Ok(body) => body,
        Err(LocalControlError::Closed) => return Ok(None),
        Err(error) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                error.to_string(),
            ));
        }
    };
    crate::protocol::frame(&body).map(Some)
}

/// Runs the standalone MCP process against the configured endpoint.
///
/// The process consumes a sequence of bounded frames from stdin and emits one
/// correlated bounded reply for every complete input frame. A malformed frame
/// (truncated or oversized length/body) produces a request-id-zero error frame
/// and terminates the stream. A complete frame whose DTO is malformed produces
/// a request-id-zero error and keeps the stream open; the final process status
/// is still failure. An endpoint startup failure produces a correlated error
/// for each complete input and also yields a failure status.
#[must_use]
pub fn main_entry() -> ExitCode {
    let options = match options_from_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("backend-mcp: {error}");
            return ExitCode::FAILURE;
        }
    };

    match options.mode {
        Mode::Mcp => json_rpc_main(&options.paths),
        Mode::Framed => framed_main(&options.paths),
        #[cfg(any(unix, windows))]
        Mode::Http(bind) => crate::http::main_entry(&options.paths, bind),
    }
}

fn json_rpc_main(paths: &backend_runtime::WorkspacePaths) -> ExitCode {
    #[cfg(any(unix, windows))]
    let result = (|| {
        let stdin = io::stdin();
        let stdout = io::stdout();
        serve_json_rpc_stdio(paths, &mut BufReader::new(stdin.lock()), &mut stdout.lock())
    })();
    #[cfg(not(any(unix, windows)))]
    let result: Result<(), String> =
        Err("local Unix endpoint transport is unavailable on this platform".to_owned());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("backend-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(any(unix, windows))]
fn serve_json_rpc_stdio(
    paths: &backend_runtime::WorkspacePaths,
    reader: &mut impl io::BufRead,
    writer: &mut impl Write,
) -> Result<(), String> {
    let project = backend_runtime::normalize_surface_path(paths.project())
        .to_string_lossy()
        .into_owned();
    let cursor_secret = crate::jsonrpc::cursor_secret(paths)?;
    // The connection is deliberately deferred until an owner-backed request.
    // MCP initialize and tools/list are local protocol operations, so an
    // unavailable owner must still receive a correlated tool error instead of
    // preventing the process from reading any request at all. Indexing remains
    // an explicit operation exposed as `backend.index`.
    crate::jsonrpc::serve_stdio(paths, project, cursor_secret, reader, writer)
        .map_err(|error| error.to_string())
}

fn framed_main(session: &backend_runtime::WorkspacePaths) -> ExitCode {
    #[cfg(any(unix, windows))]
    let (mut transport, connect_error) = match backend_runtime::ensure_locald(session) {
        Ok(path) => match UnixCommandTransport::connect(path) {
            Ok(transport) => (Some(transport), None),
            Err(error) => (None, Some(error.to_string())),
        },
        Err(error) => (None, Some(error.to_string())),
    };
    #[cfg(not(any(unix, windows)))]
    let (mut transport, connect_error): (Option<()>, Option<String>) = (
        None,
        Some("local Unix endpoint transport is unavailable on this platform".to_owned()),
    );

    let mut failed = connect_error.is_some();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    loop {
        let input = match read_stdio_frame(&mut stdin) {
            Ok(Some(input)) => input,
            Ok(None) => break,
            Err(error) => {
                let _ = stdout.write_all(&error_frame(0, error.to_string()));
                let _ = stdout.flush();
                failed = true;
                break;
            }
        };
        let request_is_valid = decode_request(&input).is_ok();
        let (output, dispatched) =
            framed_reply(transport.as_mut(), &input, connect_error.as_deref());
        if !request_is_valid || !dispatched {
            failed = true;
        }
        if stdout.write_all(&output).is_err() || stdout.flush().is_err() {
            failed = true;
            break;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Answers one framed request, saying whether the daemon actually answered it.
///
/// The two platform arms differ only in whether a transport can exist at all,
/// so the decision that matters — a dispatch failure and a missing endpoint are
/// both failures, and both still owe the caller a correlated reply — is written
/// once here rather than twice inside the loop.
#[cfg(any(unix, windows))]
fn framed_reply(
    transport: Option<&mut UnixCommandTransport>,
    input: &[u8],
    connect_error: Option<&str>,
) -> (Vec<u8>, bool) {
    let Some(transport) = transport else {
        return (
            error_frame_for_input(input, connect_error.unwrap_or(NO_ENDPOINT)),
            false,
        );
    };
    match dispatch_frame_with_transport(transport, input) {
        Ok(output) => (output, true),
        Err(error) => (error_frame_for_input(input, error.to_string()), false),
    }
}

#[cfg(not(any(unix, windows)))]
fn framed_reply(
    transport: Option<&mut ()>,
    input: &[u8],
    connect_error: Option<&str>,
) -> (Vec<u8>, bool) {
    let _ = transport;
    (
        error_frame_for_input(input, connect_error.unwrap_or(NO_ENDPOINT)),
        false,
    )
}

const NO_ENDPOINT: &str = "local endpoint unavailable";

fn options_from_args(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut endpoint = None;
    let mut workspace = None;
    let mut project = None;
    let mut mode = Mode::Mcp;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        if argument == "--framed" {
            mode = Mode::Framed;
            continue;
        }
        if argument == "--http" {
            #[cfg(any(unix, windows))]
            {
                let address = args
                    .next()
                    .ok_or_else(|| "--http requires a loopback socket address".to_owned())?;
                let address = address
                    .parse::<SocketAddr>()
                    .map_err(|error| format!("--http: invalid socket address: {error}"))?;
                mode = Mode::Http(crate::http::LoopbackBind::new(address)?);
                continue;
            }
            #[cfg(not(any(unix, windows)))]
            return Err("--http requires local endpoint transport support".to_owned());
        }
        if matches!(argument.as_str(), "--help" | "-h") {
            println!(
                "usage: backend-mcp [--project PATH] [--workspace PATH] [--endpoint PATH] [--framed | --http 127.0.0.1:PORT]"
            );
            std::process::exit(0);
        }
        let (slot, label) = match argument.as_str() {
            "--endpoint" => (&mut endpoint, "--endpoint"),
            "--workspace" => (&mut workspace, "--workspace"),
            "--project" => (&mut project, "--project"),
            _ => return Err(format!("unknown option: {argument}")),
        };
        if slot.is_some() {
            return Err(format!("{label} may only be supplied once"));
        }
        *slot = Some(
            args.next()
                .ok_or_else(|| format!("{label} requires a path"))?,
        );
    }
    if endpoint
        .as_ref()
        .is_some_and(|value| value.len() > MAX_ENDPOINT_PATH)
    {
        return Err("endpoint path is too long".to_owned());
    }
    let paths = backend_runtime::WorkspacePaths::discover(
        project.map(PathBuf::from),
        workspace.map(PathBuf::from),
        endpoint.map(PathBuf::from),
    )
    .map_err(|error| error.to_string())?;
    Ok(Options { paths, mode })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn stdio_returns_correlated_mcp_error_when_owner_startup_fails() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        // Nix's TMPDIR can itself exceed macOS's Unix-socket path budget.
        // Keep the entire private fixture under the short Unix temporary root.
        let root =
            PathBuf::from("/tmp").join(format!("bmcp-outage-{}-{nonce:x}", std::process::id()));
        let project = root.join("project");
        let data = root.join("state");
        fs::create_dir_all(&project).expect("fixture project");
        fs::create_dir_all(&data).expect("fixture state");

        // Keep the authority secret readable, but make the state directory's
        // parent fail the same private-parent admission that prevented the
        // reported process from starting. That failure now occurs only after
        // initialize, initialized, and a complete status request have reached
        // the normal JSON-RPC processor.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
            .expect("set deliberately invalid parent mode");
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700))
            .expect("keep state directory private");
        fs::write(data.join("authority.secret"), [0x41; 32]).expect("fixture authority secret");
        fs::set_permissions(
            data.join("authority.secret"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("keep authority secret private");

        let paths = backend_runtime::WorkspacePaths::discover(
            Some(project),
            Some(data),
            Some(root.join("owner.sock")),
        )
        .expect("fixture workspace paths");
        let requests = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "outage-fixture", "version": "1" }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "backend.status", "arguments": {} }
            }),
        ];
        let input = requests
            .iter()
            .map(|request| {
                format!(
                    "{}\n",
                    serde_json::to_string(request).expect("request JSON")
                )
            })
            .collect::<String>();
        let mut input = io::Cursor::new(input.into_bytes());
        let mut output = Vec::new();

        serve_json_rpc_stdio(&paths, &mut input, &mut output)
            .expect("stdio processes complete JSON-RPC requests");

        let replies = String::from_utf8(output)
            .expect("JSON-RPC output is UTF-8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("reply JSON"))
            .collect::<Vec<_>>();
        assert_eq!(replies.len(), 2, "notification has no JSON-RPC reply");
        assert_eq!(replies[0]["id"], 1);
        assert!(replies[0]["result"]["serverInfo"].is_object());
        assert_eq!(replies[1]["id"], 2);
        assert_eq!(replies[1]["result"]["isError"], true);
        assert_eq!(replies[1]["result"]["structuredContent"]["answer"], "fault");
        assert!(replies[1].to_string().contains("private state parent"));

        fs::remove_dir_all(root).expect("remove outage fixture");
    }

    #[test]
    fn stdio_initializes_a_new_workspace_before_any_owner_request() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root =
            PathBuf::from("/tmp").join(format!("bmcp-fresh-{}-{nonce:x}", std::process::id()));
        let project = root.join("project");
        fs::create_dir_all(&project).expect("fixture project");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("keep workspace parent private");
        let data = root.join("state");
        let paths = backend_runtime::WorkspacePaths::discover(
            Some(project),
            Some(data.clone()),
            Some(root.join("owner.sock")),
        )
        .expect("fixture workspace paths");
        let requests = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "fresh-workspace-fixture", "version": "1" }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }),
        ];
        let input = requests
            .iter()
            .map(|request| {
                format!(
                    "{}\n",
                    serde_json::to_string(request).expect("request JSON")
                )
            })
            .collect::<String>();
        let mut input = io::Cursor::new(input.into_bytes());
        let mut output = Vec::new();

        serve_json_rpc_stdio(&paths, &mut input, &mut output)
            .expect("stdio initializes without an owner connection");

        let replies = String::from_utf8(output)
            .expect("JSON-RPC output is UTF-8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("reply JSON"))
            .collect::<Vec<_>>();
        assert_eq!(replies.len(), 2, "notification has no JSON-RPC reply");
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[1]["id"], 2);
        assert!(replies[1]["result"]["tools"].is_array());
        assert!(data.join("authority.secret").is_file());

        fs::remove_dir_all(root).expect("remove fresh workspace fixture");
    }
}
