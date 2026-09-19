//! CLI process startup and local endpoint composition.

#[cfg(unix)]
use crate::UnixCommandTransport;
use crate::command::{certificate_for_command, needs_basis, parse, parse_with_basis};
use crate::{
    Command, CommandDto, CommandTransport, MAX_ENDPOINT_PATH, ReplyDto, execute_dto_with_transport,
    format_human, run_json,
};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

const HELP: &str = "\
backend — local-first, versioned code intelligence

Usage: backend [OPTIONS] <COMMAND>

Commands:
  index [PATH]       Index PATH, or the current project when omitted
  packages           List indexed projects
  outline <PROJECT>  Show declarations in a project
  document <SYMBOL>  Show the document containing a symbol
  show <SYMBOL>      Show one symbol
  graph <SYMBOL>     Show relationships for one symbol
  resolve <TEXT>     Resolve an exact symbol name
  name <TEXT>        Find symbols by name
  search <TEXT>      Search indexed source
  health             Show the selected version and freshness

Options:
  --project PATH     Select the project; defaults to the current directory
  --workspace PATH   Select durable state; defaults to <project>/.backend/v2
  --endpoint PATH    Connect to a specific local daemon
  --limit COUNT      Bound name/search results
  --json             Emit a machine-readable reply
  -h, --help         Show this help
  -V, --version      Show the version

The CLI starts or reuses the local daemon automatically. Query commands bind
the current admitted view, so callers do not need to manage root tokens.";

fn print_help() {
    println!("{HELP}");
}

fn session_from_args(args: &mut Vec<String>) -> Result<backend_runtime::WorkspacePaths, String> {
    let mut endpoint = None;
    let mut workspace = None;
    let mut project = None;
    let mut limit = None;
    let mut retained = Vec::with_capacity(args.len());
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--json" => at += 1,
            "--endpoint" => {
                if endpoint.is_some() {
                    return Err("--endpoint may only be supplied once".to_owned());
                }
                let value = args
                    .get(at + 1)
                    .ok_or_else(|| "--endpoint requires a path".to_owned())?;
                if value.len() > MAX_ENDPOINT_PATH {
                    return Err("endpoint path is too long".to_owned());
                }
                endpoint = Some(value.clone());
                at += 2;
            }
            "--workspace" => {
                if workspace.is_some() {
                    return Err("--workspace may only be supplied once".to_owned());
                }
                workspace = Some(
                    args.get(at + 1)
                        .ok_or_else(|| "--workspace requires a path".to_owned())?
                        .clone(),
                );
                at += 2;
            }
            "--project" => {
                if project.is_some() {
                    return Err("--project may only be supplied once".to_owned());
                }
                project = Some(
                    args.get(at + 1)
                        .ok_or_else(|| "--project requires a path".to_owned())?
                        .clone(),
                );
                at += 2;
            }
            "--limit" => {
                if limit.is_some() {
                    return Err("--limit may only be supplied once".to_owned());
                }
                limit = Some(
                    args.get(at + 1)
                        .ok_or_else(|| "--limit requires a positive integer".to_owned())?
                        .clone(),
                );
                at += 2;
            }
            _ => {
                retained.push(args[at].clone());
                at += 1;
            }
        }
    }
    if let Some(limit) = limit {
        retained.push("--limit".to_owned());
        retained.push(limit);
    }
    *args = retained;
    if endpoint
        .as_ref()
        .is_some_and(|value| value.len() > MAX_ENDPOINT_PATH)
    {
        return Err("endpoint path is too long".to_owned());
    }
    backend_runtime::WorkspacePaths::discover(
        project.map(PathBuf::from),
        workspace.map(PathBuf::from),
        endpoint.map(PathBuf::from),
    )
    .map_err(|error| error.to_string())
}

fn emit_error(json: bool, request_id: u64, message: impl Into<String>) {
    let message = message.into();
    if json {
        print!("{}", run_json(&ReplyDto::error(request_id, message)));
    } else {
        eprintln!("error: {message}");
    }
}

#[cfg(unix)]
fn command_from_endpoint(
    client: &mut UnixCommandTransport,
    args: &[String],
    json: bool,
) -> Option<CommandDto> {
    if !needs_basis(args) {
        return match parse(args) {
            Ok(command) => match certificate_for_command(args, &command, None) {
                Ok(certificate) => Some(match certificate {
                    Some(certificate) => CommandDto::new(1, command).with_certificate(certificate),
                    None => CommandDto::new(1, command),
                }),
                Err(error) => {
                    emit_error(json, 1, error);
                    None
                }
            },
            Err(error) => {
                emit_error(json, 1, error);
                None
            }
        };
    }

    let revision_request = CommandDto::new(1, Command::Revision);
    let revision = match client.request(revision_request) {
        Ok(reply) => reply,
        Err(error) => {
            emit_error(json, 1, error.to_string());
            return None;
        }
    };
    let accepted = match &revision.reply {
        backend_library::CommandReply::Revision(receipt) => receipt.root(),
        backend_library::CommandReply::Error(message) => {
            emit_error(json, 1, format!("daemon revision: {message}"));
            return None;
        }
        _ => {
            emit_error(json, 1, "daemon revision returned an invalid reply");
            return None;
        }
    };
    let Some(basis_certificate) = revision.certificate().cloned() else {
        emit_error(
            json,
            1,
            "daemon revision reply omitted its identity certificate",
        );
        return None;
    };
    let mut bound_args = args.to_vec();
    if !bound_args.iter().any(|argument| argument == "--basis") {
        bound_args.push("--basis".to_owned());
        bound_args.push(backend_library::encode_id(accepted.as_bytes()));
    }
    match parse_with_basis(&bound_args, Some(accepted)) {
        Ok(command) => match certificate_for_command(args, &command, Some(&basis_certificate)) {
            Ok(certificate) => Some(match certificate {
                Some(certificate) => CommandDto::new(2, command).with_certificate(certificate),
                None => CommandDto::new(2, command),
            }),
            Err(error) => {
                emit_error(json, 2, error);
                None
            }
        },
        Err(error) => {
            emit_error(json, 2, error);
            None
        }
    }
}

#[cfg(unix)]
fn run_endpoint(endpoint: String, args: &[String], json: bool) -> ExitCode {
    let mut client = match UnixCommandTransport::connect(endpoint) {
        Ok(client) => client,
        Err(error) => {
            emit_error(json, 1, error.to_string());
            return ExitCode::FAILURE;
        }
    };
    let Some(request) = command_from_endpoint(&mut client, args, json) else {
        return ExitCode::FAILURE;
    };
    let request_id = request.request_id;
    match execute_dto_with_transport(&mut client, &request) {
        Ok(reply) => {
            let command_failed = matches!(&reply.reply, backend_library::CommandReply::Error(_));
            let output = if json {
                run_json(&reply)
            } else {
                format_human(&reply)
            };
            let mut stdout = std::io::stdout().lock();
            if stdout
                .write_all(output.as_bytes())
                .and_then(|()| stdout.flush())
                .is_err()
            {
                return ExitCode::FAILURE;
            }
            if command_failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            emit_error(json, request_id, error.to_string());
            ExitCode::FAILURE
        }
    }
}

/// Runs the CLI process against the configured daemon endpoint.
///
/// Basis-bearing commands first obtain the daemon's accepted view basis from a
/// health reply, then admit the user claim against that exact identity before
/// sending the requested command.
#[must_use]
pub fn main_entry() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty()
        || args
            .iter()
            .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
        || (args.len() == 1 && args[0] == "help")
    {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args
        .iter()
        .any(|argument| matches!(argument.as_str(), "--version" | "-V"))
    {
        println!("backend {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let json = args.iter().any(|arg| arg == "--json");
    let session = match session_from_args(&mut args) {
        Ok(value) => value,
        Err(error) => {
            emit_error(json, 1, error);
            return ExitCode::FAILURE;
        }
    };
    if args.first().is_some_and(|command| command == "index") {
        match args.len() {
            1 => args.push(session.project().to_string_lossy().into_owned()),
            2 => {
                let selected = PathBuf::from(&args[1]);
                match selected.canonicalize() {
                    Ok(path) => args[1] = path.to_string_lossy().into_owned(),
                    Err(error) => {
                        emit_error(
                            json,
                            1,
                            format!("open project {}: {error}", selected.display()),
                        );
                        return ExitCode::FAILURE;
                    }
                }
            }
            _ => {}
        }
    }
    #[cfg(unix)]
    {
        let endpoint = match backend_runtime::ensure_locald(&session) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                emit_error(json, 1, error.to_string());
                return ExitCode::FAILURE;
            }
        };
        run_endpoint(endpoint.to_string_lossy().into_owned(), &args, json)
    }
    #[cfg(not(unix))]
    {
        let _ = session;
        emit_error(
            json,
            1,
            "local Unix endpoint transport is unavailable on this platform",
        );
        ExitCode::FAILURE
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::session_from_args;

    #[test]
    fn global_limit_is_lowered_to_the_query_grammar() {
        let mut args = ["--limit", "1", "name", "ferris", "--json"]
            .map(str::to_owned)
            .to_vec();
        session_from_args(&mut args).expect("parse session options");
        assert_eq!(args, ["name", "ferris", "--limit", "1"]);
    }
}
