//! MCP process startup, stdio framing, and endpoint composition.

#[cfg(unix)]
use crate::UnixCommandTransport;
use crate::{
    MAX_ENDPOINT_PATH, decode_request, dispatch_frame_with_transport, error_frame,
    error_frame_for_input,
};
use backend_replication::{LocalControlError, read_frame as read_local_frame};
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Mcp,
    Framed,
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
    }
}

fn json_rpc_main(paths: &backend_runtime::WorkspacePaths) -> ExitCode {
    #[cfg(unix)]
    let result = (|| {
        let endpoint = backend_runtime::ensure_locald(paths).map_err(|error| error.to_string())?;
        let project = paths
            .project()
            .canonicalize()
            .unwrap_or_else(|_| paths.project().to_path_buf())
            .to_string_lossy()
            .into_owned();
        let mut session =
            backend_client::Session::connect(&endpoint).map_err(|error| error.to_string())?;
        session.index(&project).map_err(|error| error.to_string())?;
        let stdin = io::stdin();
        let stdout = io::stdout();
        crate::jsonrpc::serve_stdio(
            session,
            project,
            &mut BufReader::new(stdin.lock()),
            &mut stdout.lock(),
        )
        .map_err(|error| error.to_string())
    })();
    #[cfg(not(unix))]
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

fn framed_main(session: &backend_runtime::WorkspacePaths) -> ExitCode {
    #[cfg(unix)]
    let (mut transport, connect_error) = match backend_runtime::ensure_locald(session) {
        Ok(path) => match UnixCommandTransport::connect(path) {
            Ok(transport) => (Some(transport), None),
            Err(error) => (None, Some(error.to_string())),
        },
        Err(error) => (None, Some(error.to_string())),
    };
    #[cfg(not(unix))]
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
        #[cfg(unix)]
        let output = match transport.as_mut() {
            Some(transport) => match dispatch_frame_with_transport(transport, &input) {
                Ok(output) => output,
                Err(error) => {
                    failed = true;
                    error_frame_for_input(&input, error.to_string())
                }
            },
            None => error_frame_for_input(
                &input,
                connect_error
                    .as_deref()
                    .unwrap_or("local endpoint unavailable"),
            ),
        };
        #[cfg(not(unix))]
        let output = {
            let _ = &mut transport;
            error_frame_for_input(
                &input,
                connect_error
                    .as_deref()
                    .unwrap_or("local endpoint unavailable"),
            )
        };
        if !request_is_valid {
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
        if matches!(argument.as_str(), "--help" | "-h") {
            println!(
                "usage: backend-mcp [--project PATH] [--workspace PATH] [--endpoint PATH] [--framed]"
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
