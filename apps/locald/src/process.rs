//! Process argument parsing and explicit startup hooks for locald.

use crate::listener::{ListenerConfig, ListenerError, RunReport, UnixListenerService};
use crate::protocol::ProtocolError;
use crate::service::{LocaldService, OwnerService};
use backend_engine::UnixEndpointPath;
use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// Environment variable naming the local daemon endpoint.
pub const ENDPOINT_ENV: &str = "BACKEND_LOCALD_ENDPOINT";
/// Environment variable naming the workspace directory.
pub const WORKSPACE_ENV: &str = "BACKEND_LOCALD_WORKSPACE";
/// Environment variable naming the compiled locald profile.
pub const PROFILE_ENV: &str = "BACKEND_LOCALD_PROFILE";
/// Environment variable naming the optional pure worker endpoint.
pub const WORKER_ENDPOINT_ENV: &str = "BACKEND_LOCALD_WORKER_ENDPOINT";
/// Environment variable naming the owner-only authority verifier credential.
pub const AUTHORITY_SECRET_ENV: &str = "BACKEND_LOCALD_AUTHORITY_SECRET_FILE";

const EX_USAGE: u8 = 64;
const EX_UNAVAILABLE: u8 = 69;
const EX_SOFTWARE: u8 = 70;

/// Parsed locald process configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessConfig {
    /// Unix endpoint path.
    pub endpoint: UnixEndpointPath,
    /// Durable workspace directory selected by the host.
    pub workspace: PathBuf,
    /// Compiled owner composition selected by the host.
    pub profile: String,
    /// Optional worker endpoint used by the configured local-first profile.
    pub worker_endpoint: Option<UnixEndpointPath>,
    /// Owner-only 32-byte authority credential used to verify product results.
    pub authority_secret: Option<PathBuf>,
    /// Endpoint limits.
    pub listener: ListenerConfig,
}

impl ProcessConfig {
    /// Parses bounded process arguments and environment fallbacks.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, missing, or unsupported arguments.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, ProcessError> {
        let ParsedOptions {
            endpoint,
            workspace,
            max_frame,
            max_clients,
            timeout_ms,
            profile,
            worker_endpoint,
            authority_secret,
            help,
        } = parse_options(args)?;
        if help {
            return Err(ProcessError::Help);
        }
        let endpoint = endpoint
            .or_else(|| std::env::var(ENDPOINT_ENV).ok())
            .ok_or_else(|| {
                ProcessError::Usage(format!("{ENDPOINT_ENV} or --endpoint is required"))
            })?;
        let workspace = workspace
            .or_else(|| std::env::var(WORKSPACE_ENV).ok())
            .ok_or_else(|| {
                ProcessError::Usage(format!("{WORKSPACE_ENV} or --workspace is required"))
            })?;
        let endpoint = UnixEndpointPath::new(endpoint)
            .map_err(|error| ProcessError::Usage(error.to_string()))?;
        if workspace.is_empty() {
            return Err(ProcessError::Usage("workspace path is empty".to_owned()));
        }
        let profile = match profile.or_else(|| std::env::var(PROFILE_ENV).ok()) {
            Some(profile) => profile,
            None => "builtin".to_owned(),
        };
        if profile.is_empty() || profile.len() > 64 {
            return Err(ProcessError::Usage(
                "profile name is empty or oversized".to_owned(),
            ));
        }
        let worker_endpoint = worker_endpoint
            .or_else(|| std::env::var(WORKER_ENDPOINT_ENV).ok())
            .map(UnixEndpointPath::new)
            .transpose()
            .map_err(|error| ProcessError::Usage(error.to_string()))?;
        let authority_secret = authority_secret
            .or_else(|| std::env::var(AUTHORITY_SECRET_ENV).ok())
            .map(PathBuf::from);
        if authority_secret
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ProcessError::Usage(
                "authority credential path is empty".to_owned(),
            ));
        }
        let mut listener = ListenerConfig::new(endpoint.as_path());
        if let Some(max_frame) = max_frame {
            listener.limits.max_frame = max_frame;
            listener.limits.max_cursor = listener.limits.max_cursor.min(max_frame);
            listener.limits.transport.max_frame = max_frame;
            listener.limits.transport.max_chunk =
                listener.limits.transport.max_chunk.min(max_frame);
        }
        if let Some(max_clients) = max_clients {
            listener.max_clients = max_clients;
        }
        if let Some(timeout_ms) = timeout_ms {
            listener.io_timeout = Duration::from_millis(timeout_ms as u64);
        }
        listener.validate().map_err(ProcessError::Listener)?;
        Ok(Self {
            endpoint,
            workspace: PathBuf::from(workspace),
            profile,
            worker_endpoint,
            authority_secret,
            listener,
        })
    }
}

struct ParsedOptions {
    endpoint: Option<String>,
    workspace: Option<String>,
    max_frame: Option<usize>,
    max_clients: Option<usize>,
    timeout_ms: Option<usize>,
    profile: Option<String>,
    worker_endpoint: Option<String>,
    authority_secret: Option<String>,
    help: bool,
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<ParsedOptions, ProcessError> {
    let mut parsed = ParsedOptions {
        endpoint: None,
        workspace: None,
        max_frame: None,
        max_clients: None,
        timeout_ms: None,
        profile: None,
        worker_endpoint: None,
        authority_secret: None,
        help: false,
    };
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => parsed.help = true,
            "--endpoint" => parsed.endpoint = Some(next_value(&mut args, "--endpoint")?),
            "--workspace" => parsed.workspace = Some(next_value(&mut args, "--workspace")?),
            "--max-frame" => {
                parsed.max_frame = Some(parse_positive(
                    &next_value(&mut args, "--max-frame")?,
                    "--max-frame",
                )?);
            }
            "--max-clients" => {
                parsed.max_clients = Some(parse_positive(
                    &next_value(&mut args, "--max-clients")?,
                    "--max-clients",
                )?);
            }
            "--timeout-ms" => {
                parsed.timeout_ms = Some(parse_positive(
                    &next_value(&mut args, "--timeout-ms")?,
                    "--timeout-ms",
                )?);
            }
            "--profile" => parsed.profile = Some(next_value(&mut args, "--profile")?),
            "--worker-endpoint" => {
                parsed.worker_endpoint = Some(next_value(&mut args, "--worker-endpoint")?);
            }
            "--authority-secret-file" => {
                parsed.authority_secret = Some(next_value(&mut args, "--authority-secret-file")?);
            }
            other => return Err(ProcessError::Usage(format!("unknown option: {other}"))),
        }
    }
    Ok(parsed)
}

fn next_value(
    args: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String, ProcessError> {
    args.next()
        .ok_or_else(|| ProcessError::Usage(format!("{option} requires a value")))
}

fn parse_positive(value: &str, option: &str) -> Result<usize, ProcessError> {
    let value = value
        .parse::<usize>()
        .map_err(|_| ProcessError::Usage(format!("{option} requires a positive integer")))?;
    if value == 0 {
        return Err(ProcessError::Usage(format!(
            "{option} requires a positive integer"
        )));
    }
    Ok(value)
}

/// Runs an already-composed owner service until shutdown.
///
/// # Errors
///
/// Returns an error when the service or listener cannot be started.
pub fn run_with_owner<O: OwnerService + 'static>(
    owner: O,
    config: ProcessConfig,
) -> Result<RunReport, ProcessError> {
    let service =
        LocaldService::new(owner, config.listener.limits).map_err(ProcessError::Protocol)?;
    let mut listener =
        UnixListenerService::bind(service, config.listener).map_err(ProcessError::Listener)?;
    listener.run().map_err(ProcessError::Listener)
}

/// Returns the process exit status for an already-composed owner service.
pub fn run_process<O: OwnerService + 'static>(owner: O, config: ProcessConfig) -> ExitCode {
    match run_with_owner(owner, config) {
        Ok(_) => ExitCode::SUCCESS,
        Err(ProcessError::Help) => {
            print_help();
            ExitCode::SUCCESS
        }
        Err(error @ ProcessError::Usage(_)) => {
            eprintln!("locald: {error}");
            ExitCode::from(EX_USAGE)
        }
        Err(
            error @ (ProcessError::Profile(_)
            | ProcessError::Listener(ListenerError::AlreadyRunning)),
        ) => {
            eprintln!("locald: {error}");
            ExitCode::from(EX_UNAVAILABLE)
        }
        Err(error) => {
            eprintln!("locald: {error}");
            ExitCode::from(EX_SOFTWARE)
        }
    }
}

/// Starts the selected compiled locald profile after bounded argument
/// validation. The default profile is a concrete durable composition; a host
/// can select it explicitly with `--profile builtin` or [`PROFILE_ENV`].
#[must_use]
pub fn main_entry() -> ExitCode {
    match ProcessConfig::parse(std::env::args().skip(1)) {
        Err(ProcessError::Help) => {
            print_help();
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("locald: {error}");
            ExitCode::from(EX_USAGE)
        }
        Ok(config) => match config.profile.as_str() {
            "builtin" | "builtin-echo" => crate::builtin::run(config),
            profile => {
                eprintln!("locald: unknown compiled profile {profile}");
                ExitCode::from(EX_UNAVAILABLE)
            }
        },
    }
}

fn print_help() {
    println!(
        "usage: backend-locald --endpoint PATH --workspace PATH [--profile builtin|builtin-echo] [--worker-endpoint PATH] [--authority-secret-file PATH] [--max-frame BYTES] [--max-clients COUNT] [--timeout-ms MS]"
    );
    println!(
        "the builtin profile opens durable state and optionally dispatches pure work to a configured worker"
    );
}

/// Process startup or listener failure.
#[derive(Debug)]
pub enum ProcessError {
    /// Invalid command-line or environment configuration.
    Usage(String),
    /// Help was requested.
    Help,
    /// A compiled profile could not construct its checked composition.
    Profile(String),
    /// Framing/transport limits failed validation.
    Protocol(ProtocolError),
    /// Unix listener startup or lifecycle failure.
    Listener(ListenerError),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::Help => formatter.write_str("help requested"),
            Self::Profile(message) => {
                write!(formatter, "compiled locald profile failed: {message}")
            }
            Self::Protocol(error) => error.fmt(formatter),
            Self::Listener(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProcessError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_requires_explicit_workspace_and_endpoint() {
        let result = ProcessConfig::parse(["--endpoint".to_owned(), "/tmp/x".to_owned()]);
        assert!(matches!(result, Err(ProcessError::Usage(_))));
    }

    #[test]
    fn help_is_successfully_parseable_without_fake_defaults() {
        assert!(matches!(
            ProcessConfig::parse(["--help".to_owned()]),
            Err(ProcessError::Help)
        ));
    }

    #[test]
    fn the_compiled_profile_is_selected_by_default_or_explicitly() {
        let parsed = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-test.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-test".to_owned(),
        ]);
        assert!(parsed.is_ok(), "bounded process configuration: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(
            parsed.profile,
            std::env::var(PROFILE_ENV).unwrap_or_else(|_| "builtin".to_owned())
        );

        let parsed = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-test.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-test".to_owned(),
            "--profile".to_owned(),
            "builtin".to_owned(),
        ]);
        assert!(parsed.is_ok(), "explicit compiled profile: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(parsed.profile, "builtin");
    }

    #[test]
    fn a_small_explicit_frame_keeps_nested_replication_limits_consistent() {
        let parsed = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-small.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-small".to_owned(),
            "--max-frame".to_owned(),
            "1024".to_owned(),
        ]);
        assert!(parsed.is_ok(), "small bounded configuration: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(parsed.listener.limits.max_frame, 1024);
        assert_eq!(parsed.listener.limits.transport.max_frame, 1024);
        assert_eq!(parsed.listener.limits.transport.max_chunk, 1024);
    }

    #[test]
    fn worker_endpoint_can_be_selected_without_unbounded_path_defaults() {
        let parsed = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-test.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-test".to_owned(),
            "--worker-endpoint".to_owned(),
            "/tmp/backend-worker-test.sock".to_owned(),
        ]);
        assert!(parsed.is_ok(), "worker endpoint configuration: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(
            parsed
                .worker_endpoint
                .as_ref()
                .map(UnixEndpointPath::as_path),
            Some(std::path::Path::new("/tmp/backend-worker-test.sock"))
        );
    }

    #[test]
    fn authority_files_are_not_constrained_by_unix_socket_path_limits() {
        let authority = format!("/tmp/{}/authority.secret", "nested".repeat(24));
        let parsed = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-long-authority.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-long-authority".to_owned(),
            "--authority-secret-file".to_owned(),
            authority.clone(),
        ]);
        assert!(parsed.is_ok(), "long authority path: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(parsed.authority_secret, Some(PathBuf::from(authority)));
    }
}
