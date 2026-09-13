//! Process argument parsing and explicit startup hooks for locald.

use crate::listener::{ListenerConfig, ListenerError, RunReport, UnixListenerService};
use crate::protocol::ProtocolError;
use crate::service::{LocaldService, OwnerService};
use backend_engine::UnixEndpointPath;
use backend_engine::registry::{
    AcquisitionLimits, AcquisitionPolicy, AuthenticationToken, RegistryEcosystem, RegistryEndpoint,
};
use backend_runtime::WorkspacePaths;
use std::fmt;
use std::fs;
use std::num::NonZeroU8;
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
/// Environment variable naming the optional registry endpoint.
pub const REGISTRY_ENDPOINT_ENV: &str = "BACKEND_REGISTRY_ENDPOINT";
/// Environment variable naming the registry ecosystem.
pub const REGISTRY_ECOSYSTEM_ENV: &str = "BACKEND_REGISTRY_ECOSYSTEM";
/// Environment variable carrying the optional registry authorization value.
pub const REGISTRY_AUTH_ENV: &str = "BACKEND_REGISTRY_AUTH";
/// Environment variable naming the optional registry authorization file.
pub const REGISTRY_AUTH_FILE_ENV: &str = "BACKEND_REGISTRY_AUTH_FILE";
/// Environment variable selecting native ecosystem metadata instead of the
/// canonical feed grammar.
pub const REGISTRY_NATIVE_ENV: &str = "BACKEND_REGISTRY_NATIVE";
/// Environment variable disabling registry network effects.
pub const REGISTRY_OFFLINE_ENV: &str = "BACKEND_REGISTRY_OFFLINE";

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
    /// Optional durable remote registry composition.
    pub registry: RegistryConfig,
}

/// Registry settings carried by the local process composition.
///
/// The endpoint is optional so a local-only process has no registry startup
/// dependency. When present, the endpoint, authentication, policy, and
/// bounded transport settings are admitted before the owner is composed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryConfig {
    /// Configured registry endpoint, if remote acquisition is enabled.
    pub endpoint: Option<RegistryEndpoint>,
    /// Optional authorization material for registry requests.
    pub authentication: Option<AuthenticationToken>,
    /// Whether network effects are permitted.
    pub policy: AcquisitionPolicy,
    /// Use the configured ecosystem's native metadata grammar.
    pub native: bool,
    /// Resource and timeout bounds passed to the owner and transport.
    pub limits: AcquisitionLimits,
}

impl RegistryConfig {
    fn from_options(
        endpoint: Option<String>,
        ecosystem: Option<String>,
        authentication: Option<String>,
        authentication_file: Option<String>,
        native: bool,
        offline: bool,
        listener: &ListenerConfig,
    ) -> Result<Self, ProcessError> {
        let endpoint_text = endpoint.or_else(|| std::env::var(REGISTRY_ENDPOINT_ENV).ok());
        let ecosystem = ecosystem
            .or_else(|| std::env::var(REGISTRY_ECOSYSTEM_ENV).ok())
            .unwrap_or_else(|| "cargo".to_owned());
        let endpoint = endpoint_text
            .map(|value| {
                let ecosystem = ecosystem.parse::<RegistryEcosystem>().map_err(|_| {
                    ProcessError::Usage(format!(
                        "{REGISTRY_ECOSYSTEM_ENV} is not a supported registry ecosystem"
                    ))
                })?;
                RegistryEndpoint::new(ecosystem, value).map_err(|_| {
                    ProcessError::Usage(
                        "registry endpoint is not an admitted absolute HTTPS or loopback HTTP URL"
                            .to_owned(),
                    )
                })
            })
            .transpose()?;
        let authentication_value = if let Some(value) = authentication {
            Some(value)
        } else if let Some(path) =
            authentication_file.or_else(|| std::env::var(REGISTRY_AUTH_FILE_ENV).ok())
        {
            Some(read_authentication_file(&path)?)
        } else {
            std::env::var(REGISTRY_AUTH_ENV).ok()
        };
        let authentication = match authentication_value {
            Some(value) => Some(AuthenticationToken::new(value).map_err(|_| {
                ProcessError::Usage("registry authentication value is invalid".to_owned())
            })?),
            None => None,
        };
        let native = native || env_flag(REGISTRY_NATIVE_ENV);
        let policy = if offline || env_flag(REGISTRY_OFFLINE_ENV) {
            AcquisitionPolicy::Offline
        } else {
            AcquisitionPolicy::Online
        };
        Ok(Self {
            endpoint,
            authentication,
            policy,
            native,
            limits: registry_limits(listener),
        })
    }
}

fn read_authentication_file(path: &str) -> Result<String, ProcessError> {
    let bytes = fs::read(path).map_err(|error| {
        ProcessError::Usage(format!("read registry authentication file: {error}"))
    })?;
    if bytes.len() > 4096 {
        return Err(ProcessError::Usage(
            "registry authentication file is oversized".to_owned(),
        ));
    }
    String::from_utf8(bytes)
        .map(|value| value.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|_| ProcessError::Usage("registry authentication file is not UTF-8".to_owned()))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn registry_limits(listener: &ListenerConfig) -> AcquisitionLimits {
    let transport = listener.limits.transport;
    let max_items = transport.max_inputs.clamp(1, 4096);
    let max_feed_bytes = listener.limits.max_frame.clamp(1, 16 * 1024 * 1024);
    let max_archive_bytes = usize::try_from(transport.max_object)
        .unwrap_or(usize::MAX)
        .clamp(1, 1024 * 1024 * 1024);
    let max_page_archive_bytes = max_archive_bytes
        .saturating_mul(max_items)
        .min(1024 * 1024 * 1024)
        .max(max_archive_bytes);
    AcquisitionLimits {
        max_items,
        max_feed_bytes,
        max_archive_bytes,
        max_page_archive_bytes,
        max_catalog_items: transport.max_objects.clamp(1, 10_000_000),
        connect_timeout: listener.io_timeout,
        read_timeout: listener.io_timeout,
        attempts: NonZeroU8::MIN,
    }
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
            registry_endpoint,
            registry_ecosystem,
            registry_auth,
            registry_auth_file,
            registry_native,
            registry_offline,
            help,
        } = parse_options(args)?;
        if help {
            return Err(ProcessError::Help);
        }
        let paths = WorkspacePaths::discover(
            None,
            workspace
                .or_else(|| std::env::var(WORKSPACE_ENV).ok())
                .map(PathBuf::from),
            endpoint
                .or_else(|| std::env::var(ENDPOINT_ENV).ok())
                .map(PathBuf::from),
        )
        .map_err(|error| ProcessError::Usage(error.to_string()))?;
        let endpoint = UnixEndpointPath::new(paths.endpoint())
            .map_err(|error| ProcessError::Usage(error.to_string()))?;
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
            .map(PathBuf::from)
            .or_else(|| Some(paths.authority_secret().to_path_buf()));
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
        let registry = RegistryConfig::from_options(
            registry_endpoint,
            registry_ecosystem,
            registry_auth,
            registry_auth_file,
            registry_native,
            registry_offline,
            &listener,
        )?;
        Ok(Self {
            endpoint,
            workspace: paths.data().to_path_buf(),
            profile,
            worker_endpoint,
            authority_secret,
            listener,
            registry,
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
    registry_endpoint: Option<String>,
    registry_ecosystem: Option<String>,
    registry_auth: Option<String>,
    registry_auth_file: Option<String>,
    registry_native: bool,
    registry_offline: bool,
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
        registry_endpoint: None,
        registry_ecosystem: None,
        registry_auth: None,
        registry_auth_file: None,
        registry_native: false,
        registry_offline: false,
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
            "--registry-endpoint" => {
                parsed.registry_endpoint = Some(next_value(&mut args, "--registry-endpoint")?);
            }
            "--registry-ecosystem" => {
                parsed.registry_ecosystem = Some(next_value(&mut args, "--registry-ecosystem")?);
            }
            "--registry-auth" => {
                parsed.registry_auth = Some(next_value(&mut args, "--registry-auth")?);
            }
            "--registry-auth-file" => {
                parsed.registry_auth_file = Some(next_value(&mut args, "--registry-auth-file")?);
            }
            "--registry-native" => parsed.registry_native = true,
            "--registry-offline" => parsed.registry_offline = true,
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
            "builtin" | "builtin-echo" => {
                let default_authority = config.workspace.join("authority.secret");
                if config.authority_secret.as_ref() == Some(&default_authority) {
                    let paths = WorkspacePaths::discover(
                        None,
                        Some(config.workspace.clone()),
                        Some(config.endpoint.as_path().to_path_buf()),
                    );
                    if let Err(error) = paths.and_then(|paths| paths.initialize()) {
                        eprintln!("locald: {error}");
                        return ExitCode::from(EX_USAGE);
                    }
                }
                crate::builtin::run(config)
            }
            profile => {
                eprintln!("locald: unknown compiled profile {profile}");
                ExitCode::from(EX_UNAVAILABLE)
            }
        },
    }
}

fn print_help() {
    println!(
        "usage: backend-locald [--endpoint PATH] [--workspace PATH] [--profile builtin|builtin-echo] [--worker-endpoint PATH] [--authority-secret-file PATH] [--registry-endpoint URL] [--registry-ecosystem NAME] [--registry-auth VALUE|--registry-auth-file PATH] [--registry-native] [--registry-offline] [--max-frame BYTES] [--max-clients COUNT] [--timeout-ms MS]"
    );
    println!(
        "without paths, locald opens .backend/v2 for the current project and derives a short local endpoint"
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
    fn configuration_derives_workspace_and_endpoint() {
        let result = ProcessConfig::parse(std::iter::empty());
        assert!(result.is_ok(), "zero-configuration locald: {result:?}");
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
