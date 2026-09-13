//! Worker process startup hooks and explicit exit status mapping.

use crate::listener::{
    TcpExposure, TcpWorkerListener, TcpWorkerListenerConfig, UnixWorkerListener,
    WorkerListenerConfig, WorkerListenerError, WorkerRunReport,
};
use crate::protocol::{WorkerLimits, WorkerProtocolError};
use crate::service::{JobAdmission, WorkerService};
use backend_engine::{PureRecipeExecutor, Relation, UnixEndpointPath, WorkerAttestationSigner};
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const EX_USAGE: u8 = 64;
const EX_UNAVAILABLE: u8 = 69;
const EX_SOFTWARE: u8 = 70;

/// Environment variable naming the worker Unix endpoint.
pub const ENDPOINT_ENV: &str = "BACKEND_WORKER_ENDPOINT";
/// Environment variable naming the compiled worker profile.
pub const PROFILE_ENV: &str = "BACKEND_WORKER_PROFILE";
/// Environment variable naming the private authority credential file used by
/// the production profile. The compatibility echo profile does not require it.
pub const AUTHORITY_SECRET_ENV: &str = "BACKEND_WORKER_AUTHORITY_SECRET_FILE";

/// Parsed worker process configuration. Recipe registration, executor choice,
/// and attestation signing remain composition inputs supplied by the host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerProcessConfig {
    /// Unix endpoint path supplied by the host process.
    pub endpoint: Option<UnixEndpointPath>,
    /// Bounded stream policy.
    pub limits: WorkerLimits,
    /// Explicit compiled recipe profile selected by the host.
    pub profile: Option<String>,
    /// Owner-only 32-byte authority credential for the production profile.
    pub authority_secret: Option<PathBuf>,
    /// Optional authenticated cross-host TCP listener. When set, the Unix
    /// endpoint is not opened and may be left absent in the CLI.
    pub tcp_listen: Option<SocketAddr>,
    /// Explicit acknowledgement that a routable TCP listener is protected by
    /// an outer confidential transport.
    pub tcp_exposure: TcpExposure,
}

impl WorkerProcessConfig {
    /// Parses `--endpoint`, `--max-frame`, and `--timeout-ms` without choosing
    /// an implicit recipe or filesystem.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, WorkerProcessError> {
        let mut endpoint = None;
        let mut max_frame = None;
        let mut timeout_ms = None;
        let mut profile = None;
        let mut authority_secret = None;
        let mut tcp_listen = None;
        let mut tcp_exposure = TcpExposure::LoopbackOnly;
        let mut help = false;
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--help" | "-h" => help = true,
                "--endpoint" => endpoint = Some(next_value(&mut args, "--endpoint")?),
                "--max-frame" => {
                    max_frame = Some(parse_positive(
                        &next_value(&mut args, "--max-frame")?,
                        "--max-frame",
                    )?);
                }
                "--timeout-ms" => {
                    timeout_ms = Some(parse_positive(
                        &next_value(&mut args, "--timeout-ms")?,
                        "--timeout-ms",
                    )?);
                }
                "--profile" => profile = Some(next_value(&mut args, "--profile")?),
                "--authority-secret-file" => {
                    authority_secret = Some(next_value(&mut args, "--authority-secret-file")?);
                }
                "--tcp-listen" => {
                    let value = next_value(&mut args, "--tcp-listen")?;
                    tcp_listen = Some(value.parse::<SocketAddr>().map_err(|_| {
                        WorkerProcessError::Usage(
                            "--tcp-listen requires a valid host:port address".to_owned(),
                        )
                    })?);
                }
                "--external-protected-transport" => {
                    tcp_exposure = TcpExposure::ExternalProtected;
                }
                other => {
                    return Err(WorkerProcessError::Usage(format!(
                        "unknown option: {other}"
                    )));
                }
            }
        }
        if help {
            return Err(WorkerProcessError::Help);
        }
        let endpoint = endpoint
            .or_else(|| std::env::var(ENDPOINT_ENV).ok())
            .map(UnixEndpointPath::new)
            .transpose()
            .map_err(|error| WorkerProcessError::Usage(error.to_string()))?;
        if endpoint.is_none() && tcp_listen.is_none() {
            return Err(WorkerProcessError::Usage(format!(
                "{ENDPOINT_ENV} or --endpoint is required unless --tcp-listen is set"
            )));
        }
        let mut limits = WorkerLimits::default();
        if let Some(max_frame) = max_frame {
            limits.transport.max_frame = max_frame;
            limits.transport.max_chunk = limits.transport.max_chunk.min(max_frame);
        }
        if let Some(timeout_ms) = timeout_ms {
            limits.io_timeout = Duration::from_millis(timeout_ms as u64);
        }
        // The checked builtin is the process' concrete production
        // composition.  Keep the fixture profile opt-in, but make a valid
        // endpoint invocation useful without requiring an undocumented
        // environment switch.
        let profile = profile
            .or_else(|| std::env::var(PROFILE_ENV).ok())
            .or_else(|| Some("builtin".to_owned()));
        if profile
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.len() > 64)
        {
            return Err(WorkerProcessError::Usage(
                "profile name is empty or oversized".to_owned(),
            ));
        }
        let authority_secret = authority_secret
            .or_else(|| std::env::var(AUTHORITY_SECRET_ENV).ok())
            .map(PathBuf::from);
        if authority_secret
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(WorkerProcessError::Usage(
                "authority credential path is empty".to_owned(),
            ));
        }
        limits.validate().map_err(WorkerProcessError::Protocol)?;
        Ok(Self {
            endpoint,
            limits,
            profile,
            authority_secret,
            tcp_listen,
            tcp_exposure,
        })
    }
}

fn next_value(
    args: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String, WorkerProcessError> {
    args.next()
        .ok_or_else(|| WorkerProcessError::Usage(format!("{option} requires a value")))
}

fn parse_positive(value: &str, option: &str) -> Result<usize, WorkerProcessError> {
    let value = value
        .parse::<usize>()
        .map_err(|_| WorkerProcessError::Usage(format!("{option} requires a positive integer")))?;
    if value == 0 {
        return Err(WorkerProcessError::Usage(format!(
            "{option} requires a positive integer"
        )));
    }
    Ok(value)
}

/// Worker process startup or listener failure.
#[derive(Debug)]
pub enum WorkerProcessError {
    /// Invalid command-line or environment configuration.
    Usage(String),
    /// Help was requested.
    Help,
    /// A compiled profile could not construct its checked composition.
    Profile(String),
    /// Worker framing or canonical codec failure.
    Protocol(WorkerProtocolError),
    /// Unix listener startup or lifecycle failure.
    Listener(WorkerListenerError),
}

impl fmt::Display for WorkerProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::Help => formatter.write_str("help requested"),
            Self::Profile(message) => {
                write!(formatter, "compiled worker profile failed: {message}")
            }
            Self::Protocol(error) => error.fmt(formatter),
            Self::Listener(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for WorkerProcessError {}

/// Runs an already-composed worker service on its Unix endpoint.
/// # Errors
/// Returns an error when the input is invalid or durable state cannot be accessed.
pub fn run_with_service<E, S, R, A>(
    worker: WorkerService<E, S>,
    admission: A,
    config: WorkerProcessConfig,
) -> Result<WorkerRunReport, WorkerProcessError>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R> + 'static,
{
    if let Some(address) = config.tcp_listen {
        let authority = config
            .authority_secret
            .as_deref()
            .ok_or_else(|| {
                WorkerProcessError::Profile(
                    "TCP worker listener requires --authority-secret-file".to_owned(),
                )
            })
            .and_then(|path| {
                backend_engine::read_authority_secret(path).map_err(|error| {
                    WorkerProcessError::Profile(format!(
                        "load TCP authority credential {}: {error}",
                        path.display()
                    ))
                })
            })?;
        return run_with_tcp_service(
            worker,
            admission,
            config.limits,
            address,
            backend_engine::TcpAuthority::new(authority),
            config.tcp_exposure,
        );
    }
    let endpoint = config.endpoint.ok_or_else(|| {
        WorkerProcessError::Usage(
            "a checked Unix endpoint is required when --tcp-listen is absent".to_owned(),
        )
    })?;
    let listener_config = WorkerListenerConfig {
        path: endpoint.into_path_buf(),
        limits: config.limits,
    };
    let mut listener = UnixWorkerListener::bind(worker, admission, listener_config)
        .map_err(WorkerProcessError::Listener)?;
    listener.run().map_err(WorkerProcessError::Listener)
}

fn run_with_tcp_service<E, S, R, A>(
    worker: WorkerService<E, S>,
    admission: A,
    limits: WorkerLimits,
    address: SocketAddr,
    authority: backend_engine::TcpAuthority,
    exposure: TcpExposure,
) -> Result<WorkerRunReport, WorkerProcessError>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R> + 'static,
{
    let listener_config = TcpWorkerListenerConfig {
        address,
        limits,
        authority,
        exposure,
    };
    let mut listener = TcpWorkerListener::bind(worker, admission, listener_config)
        .map_err(WorkerProcessError::Listener)?;
    listener.run().map_err(WorkerProcessError::Listener)
}

/// Maps a composed worker's lifecycle to a process exit code.
pub fn run_process<E, S, R, A>(
    worker: WorkerService<E, S>,
    admission: A,
    config: WorkerProcessConfig,
) -> ExitCode
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R> + 'static,
{
    match run_with_service(worker, admission, config) {
        Ok(_) => ExitCode::SUCCESS,
        Err(WorkerProcessError::Help) => {
            print_help();
            ExitCode::SUCCESS
        }
        Err(error @ WorkerProcessError::Usage(_)) => {
            eprintln!("backend-worker: {error}");
            ExitCode::from(EX_USAGE)
        }
        Err(error @ WorkerProcessError::Listener(WorkerListenerError::AlreadyRunning)) => {
            eprintln!("backend-worker: {error}");
            ExitCode::from(EX_UNAVAILABLE)
        }
        Err(error) => {
            eprintln!("backend-worker: {error}");
            ExitCode::from(EX_SOFTWARE)
        }
    }
}

/// Starts the selected compiled worker profile.
#[must_use]
pub fn main_entry() -> ExitCode {
    match WorkerProcessConfig::parse(std::env::args().skip(1)) {
        Err(WorkerProcessError::Help) => {
            print_help();
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("backend-worker: {error}");
            ExitCode::from(EX_USAGE)
        }
        Ok(config) => match config.profile.as_deref() {
            Some(profile) => {
                if profile == "builtin" || profile == "builtin-echo" {
                    crate::builtin::run(config)
                } else {
                    eprintln!("backend-worker: unknown compiled profile {profile}");
                    ExitCode::from(EX_UNAVAILABLE)
                }
            }
            // Keep the defensive branch runnable if a caller constructs the
            // public config directly instead of using `parse`.
            None => crate::builtin::run(config),
        },
    }
}

fn print_help() {
    println!(
        "usage: backend-worker --endpoint PATH | --tcp-listen HOST:PORT [--external-protected-transport] [--profile builtin|builtin-echo] [--authority-secret-file PATH] [--max-frame BYTES] [--timeout-ms MS]"
    );
    println!("builtin is the checked production recipe; builtin-echo is a compatibility fixture");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_requires_an_explicit_endpoint() {
        assert!(matches!(
            WorkerProcessConfig::parse(std::iter::empty::<String>()),
            Err(WorkerProcessError::Usage(_))
        ));
    }

    #[test]
    fn help_is_parseable_without_selecting_a_recipe_or_signer() {
        assert!(matches!(
            WorkerProcessConfig::parse(["--help".to_owned()]),
            Err(WorkerProcessError::Help)
        ));
    }

    #[test]
    fn a_small_explicit_frame_keeps_chunk_limits_consistent() {
        let parsed = WorkerProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-worker-small.sock".to_owned(),
            "--max-frame".to_owned(),
            "1024".to_owned(),
        ]);
        assert!(parsed.is_ok(), "small bounded configuration: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(parsed.limits.transport.max_frame, 1024);
        assert_eq!(parsed.limits.transport.max_chunk, 1024);
    }

    #[test]
    fn authority_files_are_not_constrained_by_unix_socket_path_limits() {
        let authority = format!("/tmp/{}/authority.secret", "nested".repeat(24));
        let parsed = WorkerProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-worker-long-authority.sock".to_owned(),
            "--authority-secret-file".to_owned(),
            authority.clone(),
        ]);
        assert!(parsed.is_ok(), "long authority path: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(parsed.authority_secret, Some(PathBuf::from(authority)));
    }
}
