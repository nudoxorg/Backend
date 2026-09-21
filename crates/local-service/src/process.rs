//! Process argument parsing and explicit startup hooks for locald.
//!
//! # Daemon lifecycle
//!
//! **Who spawns.** A surface spawns `backend-locald` through
//! `backend_runtime::ensure_locald`, detached, with its standard streams
//! closed. The spawning CLI process usually exits within the second.
//!
//! **Who reaps.** Nobody. There is no supervisor and no parent waiting on the
//! child, so the daemon must retire itself or leak for the life of the
//! machine. It retires through [`ListenerConfig::idle_timeout`]: when no
//! client has been connected and no owner work has progressed for that window,
//! the run loop stops, closes the workspace, and unlinks its endpoint.
//!
//! **The default window** is ten minutes with no clients
//! ([`crate::DEFAULT_IDLE_TIMEOUT`]). `--idle-timeout-ms MS` overrides it, and
//! `--idle-timeout-ms 0` disables it for a host that supervises the daemon
//! itself.
//!
//! **How a surface keeps a daemon alive.** By staying connected. The idle
//! window only advances while the connected-client count is zero, so a desktop
//! holding a subscription, or a CLI mid-command, can never be retired out from
//! under itself. A surface that wants a warm daemon between commands either
//! reconnects inside the window or raises it.
//!
//! **How a surface ends one early.** By sending
//! [`crate::EngineRequest::Shutdown`] on the wire. The listener answers it
//! with its own stop capability. The CLI verb that would expose this to a user
//! belongs to the CLI surface and is not defined here.

use crate::listener::{ListenerConfig, ListenerError, RunReport, UnixListenerService};
use crate::protocol::ProtocolError;
use crate::service::{LocaldService, OwnerService};
use backend_engine::advisory::AdvisorySource;
use backend_engine::registry::{
    AcquisitionLimits, AcquisitionPolicy, AuthenticationToken, RegistryEcosystem, RegistryEndpoint,
    RegistrySource, RegistrySourceSet,
};
use backend_engine::UnixEndpointPath;
use backend_engine::{AcquisitionGate, OfflinePolicy};
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
/// Environment variable carrying comma-separated `ecosystem=endpoint` source
/// overrides. It is an optional process adapter; zero-configuration uses the
/// official source set instead.
pub const REGISTRY_SOURCES_ENV: &str = "BACKEND_REGISTRY_SOURCES";
/// Environment variable carrying comma-separated source-authority credentials
/// in `authority=token` form. Values are scoped to the named source authority.
pub const REGISTRY_AUTH_SCOPES_ENV: &str = "BACKEND_REGISTRY_AUTH_SCOPES";
/// Environment variable selecting the advisory evidence policy.
pub const ADVISORY_POLICY_ENV: &str = "BACKEND_ADVISORY_POLICY";
/// Environment variable overriding the maximum admitted registry archive bytes.
pub const REGISTRY_MAX_ARCHIVE_BYTES_ENV: &str = "BACKEND_REGISTRY_MAX_ARCHIVE_BYTES";
/// Environment variable naming an OSV JSON/batch feed path or HTTPS URL.
pub const ADVISORY_OSV_ENV: &str = "BACKEND_ADVISORY_OSV";
/// Environment variable naming a RustSec TOML/tree path or HTTPS URL.
pub const ADVISORY_RUSTSEC_ENV: &str = "BACKEND_ADVISORY_RUSTSEC";
/// Environment variable naming a GHSA JSON/batch feed path or HTTPS URL.
pub const ADVISORY_GHSA_ENV: &str = "BACKEND_ADVISORY_GHSA";
/// Environment variable controlling the accepted advisory freshness window.
pub const ADVISORY_MAX_AGE_ENV: &str = "BACKEND_ADVISORY_MAX_AGE_SECS";
/// Environment variable disabling advisory network refreshes.
pub const ADVISORY_OFFLINE_ENV: &str = "BACKEND_ADVISORY_OFFLINE";

/// Default registry archive admission for a single source archive.
///
/// Source sdists routinely exceed the 64 MiB replication object budget, so the
/// registry cap is deliberately independent of that transport limit and can be
/// raised with [`REGISTRY_MAX_ARCHIVE_BYTES_ENV`]. Values above the ceiling are
/// clamped and a deliberate overrun returns a typed acquisition error.
const REGISTRY_MAX_ARCHIVE_DEFAULT_BYTES: usize = 512 * 1024 * 1024;
const REGISTRY_MAX_ARCHIVE_CEILING_BYTES: usize = 1024 * 1024 * 1024;
const ADVISORY_MAX_FEED_BYTES: usize = 256 * 1024 * 1024;

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
    /// Durable OSV/RustSec/GHSA authority composition.
    pub advisory: AdvisoryConfig,
}

/// One source location admitted by the local-first advisory composition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvisorySourceConfig {
    /// Source parser and identity namespace.
    pub source: AdvisorySource,
    /// Local path or HTTPS URL supplied by the host.
    pub location: String,
}

/// Advisory authority settings persisted independently from registry archives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvisoryConfig {
    /// Configured authorities. Empty is a valid unknown-coverage state.
    pub sources: Vec<AdvisorySourceConfig>,
    /// Maximum age of a source frontier before it becomes stale.
    pub max_age_secs: u64,
    /// Whether refresh network effects are disabled.
    pub offline: bool,
    /// Policy applied before archive staging.
    pub gate: AcquisitionGate,
    /// Maximum one authority body admitted into memory.
    pub max_feed_bytes: usize,
}

/// Registry settings carried by the local process composition.
///
/// `sources` is always populated with the typed official defaults, so a GUI,
/// CLI, or MCP client can submit a package PURL without first learning a
/// registry endpoint. The legacy fields remain optional override adapters for
/// callers that still supply `--registry-endpoint` or the corresponding
/// environment variables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryConfig {
    /// Deterministic per-ecosystem source set.
    pub sources: RegistrySourceSet,
    /// Legacy configured registry endpoint, if supplied by an override.
    pub endpoint: Option<RegistryEndpoint>,
    /// Legacy authorization material scoped to [`Self::endpoint`].
    pub authentication: Option<AuthenticationToken>,
    /// Legacy global policy adapter.
    pub policy: AcquisitionPolicy,
    /// Legacy native-mode adapter.
    pub native: bool,
    /// Resource and timeout bounds passed to the owner and transport.
    pub limits: AcquisitionLimits,
    /// Explicit fail-closed advisory decision policy used before staging archives.
    pub advisory_gate: AcquisitionGate,
}

impl RegistryConfig {
    fn from_options(
        endpoint: Option<String>,
        ecosystem: Option<String>,
        authentication: Option<String>,
        authentication_file: Option<String>,
        native: bool,
        offline: bool,
        source_specs: Vec<String>,
        auth_scopes: Vec<String>,
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
        let limits = registry_limits(listener);
        let mut sources = RegistrySourceSet::defaults()
            .map_err(|error| ProcessError::Profile(error.to_string()))?;
        let mut specs = source_specs;
        if let Ok(value) = std::env::var(REGISTRY_SOURCES_ENV) {
            specs.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
        for spec in specs {
            let (ecosystem, endpoint) = parse_source_spec(&spec)?;
            let endpoint = RegistryEndpoint::new(ecosystem, endpoint).map_err(|_| {
                ProcessError::Usage(
                    "registry source endpoint is not an admitted absolute HTTPS or loopback HTTP URL"
                        .to_owned(),
                )
            })?;
            sources
                .insert(
                    RegistrySource::new(endpoint)
                        .with_priority(0)
                        .with_policy(policy),
                )
                .map_err(|_| ProcessError::Usage("registry source is invalid".to_owned()))?;
        }
        if let Some(endpoint) = endpoint.as_ref() {
            let source = RegistrySource::new(endpoint.clone())
                .with_priority(0)
                .with_policy(policy)
                .with_native(native);
            let source = authentication
                .clone()
                .map_or(source.clone(), |token| source.with_authentication(token));
            sources
                .override_ecosystem(source)
                .map_err(|_| ProcessError::Usage("registry source is invalid".to_owned()))?;
        }
        if matches!(policy, AcquisitionPolicy::Offline) {
            sources = sources.offline();
        }
        let mut auth_specs = auth_scopes;
        if let Ok(value) = std::env::var(REGISTRY_AUTH_SCOPES_ENV) {
            auth_specs.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
        for spec in auth_specs {
            let (endpoint, token) = spec.split_once('=').ok_or_else(|| {
                ProcessError::Usage(
                    "registry auth scope must use endpoint=token spelling".to_owned(),
                )
            })?;
            let token = AuthenticationToken::new(token.to_owned()).map_err(|_| {
                ProcessError::Usage("registry authentication value is invalid".to_owned())
            })?;
            let endpoint = endpoint.trim();
            let matched_authority = sources
                .authenticate_authority(endpoint, token.clone())
                .map_err(|_| ProcessError::Usage("registry auth scope is invalid".to_owned()))?;
            if !(matched_authority
                || sources
                    .authenticate_endpoint(endpoint, token)
                    .map_err(|_| {
                        ProcessError::Usage("registry auth scope is invalid".to_owned())
                    })?)
            {
                return Err(ProcessError::Usage(
                    "registry auth scope must name a configured source authority".to_owned(),
                ));
            }
        }
        let advisory_gate = advisory_gate_from_env()?;
        Ok(Self {
            sources,
            endpoint,
            authentication,
            policy,
            native,
            limits,
            advisory_gate,
        })
    }
}

fn parse_source_spec(spec: &str) -> Result<(RegistryEcosystem, String), ProcessError> {
    let (ecosystem, endpoint) = spec.split_once('=').ok_or_else(|| {
        ProcessError::Usage("registry source must use ecosystem=endpoint spelling".to_owned())
    })?;
    let ecosystem = ecosystem.parse::<RegistryEcosystem>().map_err(|_| {
        ProcessError::Usage(format!(
            "{REGISTRY_SOURCES_ENV} contains an unsupported registry ecosystem"
        ))
    })?;
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(ProcessError::Usage(
            "registry source endpoint is empty".to_owned(),
        ));
    }
    Ok((ecosystem, endpoint.to_owned()))
}

impl AdvisoryConfig {
    fn from_options(
        osv: Option<String>,
        rustsec: Option<String>,
        ghsa: Option<String>,
        offline: bool,
        max_age_secs: Option<usize>,
    ) -> Result<Self, ProcessError> {
        let mut sources = Vec::new();
        for (source, value, env) in [
            (
                AdvisorySource::Osv,
                osv.or_else(|| std::env::var(ADVISORY_OSV_ENV).ok()),
                ADVISORY_OSV_ENV,
            ),
            (
                AdvisorySource::RustSec,
                rustsec.or_else(|| std::env::var(ADVISORY_RUSTSEC_ENV).ok()),
                ADVISORY_RUSTSEC_ENV,
            ),
            (
                AdvisorySource::Ghsa,
                ghsa.or_else(|| std::env::var(ADVISORY_GHSA_ENV).ok()),
                ADVISORY_GHSA_ENV,
            ),
        ] {
            if let Some(location) = value {
                let location = location.trim().to_owned();
                if location.is_empty() || location.len() > 4096 {
                    return Err(ProcessError::Usage(format!(
                        "{env} must be a bounded non-empty path or URL"
                    )));
                }
                sources.push(AdvisorySourceConfig { source, location });
            }
        }
        let max_age_secs = max_age_secs
            .map(u64::try_from)
            .transpose()
            .map_err(|_| ProcessError::Usage("advisory max age is oversized".to_owned()))?
            .or_else(|| {
                std::env::var(ADVISORY_MAX_AGE_ENV)
                    .ok()
                    .and_then(|value| value.trim().parse().ok())
            })
            .unwrap_or(86_400);
        Ok(Self {
            sources,
            max_age_secs,
            offline: offline || env_flag(ADVISORY_OFFLINE_ENV),
            gate: advisory_gate_from_env()?,
            max_feed_bytes: ADVISORY_MAX_FEED_BYTES,
        })
    }
}

fn advisory_gate_from_env() -> Result<AcquisitionGate, ProcessError> {
    // Native registry adapters do not claim advisory authority. Unknown coverage is retained and
    // warned about until a configured authority supplies a complete frontier; operators can still
    // opt into fail-closed mode explicitly for controlled environments.
    let value = std::env::var(ADVISORY_POLICY_ENV).unwrap_or_else(|_| "warn".to_owned());
    let offline = match value.trim().to_ascii_lowercase().as_str() {
        "allow-cached" | "allow_cached" => OfflinePolicy::AllowCached,
        "warn" => OfflinePolicy::Warn,
        "fail-closed" | "fail_closed" => OfflinePolicy::FailClosed,
        _ => {
            return Err(ProcessError::Usage(format!(
                "{ADVISORY_POLICY_ENV} must be allow-cached, warn, or fail-closed"
            )));
        }
    };
    Ok(AcquisitionGate { offline })
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
    let max_archive_bytes = registry_max_archive_bytes();
    let max_page_archive_bytes = max_archive_bytes
        .saturating_mul(max_items)
        .min(REGISTRY_MAX_ARCHIVE_CEILING_BYTES)
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

fn registry_max_archive_bytes() -> usize {
    std::env::var(REGISTRY_MAX_ARCHIVE_BYTES_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(REGISTRY_MAX_ARCHIVE_DEFAULT_BYTES)
        .clamp(1, REGISTRY_MAX_ARCHIVE_CEILING_BYTES)
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
            idle_timeout_ms,
            profile,
            worker_endpoint,
            authority_secret,
            registry_endpoint,
            registry_ecosystem,
            registry_auth,
            registry_auth_file,
            registry_native,
            registry_offline,
            registry_sources,
            registry_auth_scopes,
            advisory_osv,
            advisory_rustsec,
            advisory_ghsa,
            advisory_offline,
            advisory_max_age_secs,
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
            listener.io_timeout =
                Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(u64::MAX));
        }
        if let Some(idle_timeout_ms) = idle_timeout_ms {
            // `0` is the explicit "supervise me yourself" spelling; it is not
            // a zero-length window, which `validate` rejects.
            listener.idle_timeout = (idle_timeout_ms != 0)
                .then(|| Duration::from_millis(u64::try_from(idle_timeout_ms).unwrap_or(u64::MAX)));
        }
        listener.validate().map_err(ProcessError::Listener)?;
        let registry = RegistryConfig::from_options(
            registry_endpoint,
            registry_ecosystem,
            registry_auth,
            registry_auth_file,
            registry_native,
            registry_offline,
            registry_sources,
            registry_auth_scopes,
            &listener,
        )?;
        let advisory = AdvisoryConfig::from_options(
            advisory_osv,
            advisory_rustsec,
            advisory_ghsa,
            advisory_offline,
            advisory_max_age_secs,
        )?;
        Ok(Self {
            endpoint,
            workspace: paths.data().to_path_buf(),
            profile,
            worker_endpoint,
            authority_secret,
            listener,
            registry,
            advisory,
        })
    }
}

struct ParsedOptions {
    endpoint: Option<String>,
    workspace: Option<String>,
    max_frame: Option<usize>,
    max_clients: Option<usize>,
    timeout_ms: Option<usize>,
    idle_timeout_ms: Option<usize>,
    profile: Option<String>,
    worker_endpoint: Option<String>,
    authority_secret: Option<String>,
    registry_endpoint: Option<String>,
    registry_ecosystem: Option<String>,
    registry_auth: Option<String>,
    registry_auth_file: Option<String>,
    registry_native: bool,
    registry_offline: bool,
    registry_sources: Vec<String>,
    registry_auth_scopes: Vec<String>,
    advisory_osv: Option<String>,
    advisory_rustsec: Option<String>,
    advisory_ghsa: Option<String>,
    advisory_offline: bool,
    advisory_max_age_secs: Option<usize>,
    help: bool,
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<ParsedOptions, ProcessError> {
    let mut parsed = ParsedOptions {
        endpoint: None,
        workspace: None,
        max_frame: None,
        max_clients: None,
        timeout_ms: None,
        idle_timeout_ms: None,
        profile: None,
        worker_endpoint: None,
        authority_secret: None,
        registry_endpoint: None,
        registry_ecosystem: None,
        registry_auth: None,
        registry_auth_file: None,
        registry_native: false,
        registry_offline: false,
        registry_sources: Vec::new(),
        registry_auth_scopes: Vec::new(),
        advisory_osv: None,
        advisory_rustsec: None,
        advisory_ghsa: None,
        advisory_offline: false,
        advisory_max_age_secs: None,
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
            "--idle-timeout-ms" => {
                parsed.idle_timeout_ms = Some(parse_count(
                    &next_value(&mut args, "--idle-timeout-ms")?,
                    "--idle-timeout-ms",
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
            "--registry-source" | "--registry-mirror" => {
                parsed
                    .registry_sources
                    .push(next_value(&mut args, argument.as_str())?);
            }
            "--registry-auth-for" => {
                parsed
                    .registry_auth_scopes
                    .push(next_value(&mut args, "--registry-auth-for")?);
            }
            "--advisory-osv" => {
                parsed.advisory_osv = Some(next_value(&mut args, "--advisory-osv")?);
            }
            "--advisory-rustsec" => {
                parsed.advisory_rustsec = Some(next_value(&mut args, "--advisory-rustsec")?);
            }
            "--advisory-ghsa" => {
                parsed.advisory_ghsa = Some(next_value(&mut args, "--advisory-ghsa")?);
            }
            "--advisory-offline" => parsed.advisory_offline = true,
            "--advisory-max-age-secs" => {
                parsed.advisory_max_age_secs = Some(parse_count(
                    &next_value(&mut args, "--advisory-max-age-secs")?,
                    "--advisory-max-age-secs",
                )?);
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

/// Parses an option whose zero value is meaningful.
fn parse_count(value: &str, option: &str) -> Result<usize, ProcessError> {
    value
        .parse::<usize>()
        .map_err(|_| ProcessError::Usage(format!("{option} requires a non-negative integer")))
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
        "usage: backend-locald [--endpoint PATH] [--workspace PATH] [--profile builtin|builtin-echo] [--worker-endpoint PATH] [--authority-secret-file PATH] [--registry-source ECO=URL]... [--registry-auth-for URL=TOKEN]... [--registry-endpoint URL] [--registry-ecosystem NAME] [--registry-auth VALUE|--registry-auth-file PATH] [--registry-native] [--registry-offline] [--advisory-osv PATH|URL] [--advisory-rustsec PATH|URL] [--advisory-ghsa PATH|URL] [--advisory-offline] [--advisory-max-age-secs SECONDS] [--max-frame BYTES] [--max-clients COUNT] [--timeout-ms MS] [--idle-timeout-ms MS]"
    );
    println!(
        "without paths, locald opens .backend/v2 for the current project and derives a short local endpoint"
    );
    println!(
        "locald retires itself after --idle-timeout-ms with no connected client (default 600000); 0 never times out"
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
        let Ok(result) = result else { return };
        assert_eq!(result.registry.sources.len(), 7);
        assert!(result.registry.endpoint.is_none());
    }

    #[test]
    fn source_mirrors_and_authority_credentials_are_optional_adapters() {
        let result = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-registry-router.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-registry-router".to_owned(),
            "--registry-mirror".to_owned(),
            "cargo=https://mirror.example.test/cargo".to_owned(),
            "--registry-auth-for".to_owned(),
            "https://mirror.example.test/cargo/sparse=mirror-token".to_owned(),
        ]);
        assert!(result.is_ok(), "source mirror configuration: {result:?}");
        let Ok(result) = result else { return };
        let cargo = result
            .registry
            .sources
            .for_ecosystem(RegistryEcosystem::Cargo);
        let mirror = cargo
            .iter()
            .find(|source| source.endpoint().as_str() == "https://mirror.example.test/cargo")
            .expect("configured cargo mirror");
        assert_eq!(mirror.priority(), 0);
        assert!(mirror.has_authentication());
    }

    #[test]
    fn offline_adapter_marks_every_official_source_without_network_setup() {
        let result = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-offline-router.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-offline-router".to_owned(),
            "--registry-offline".to_owned(),
        ]);
        assert!(result.is_ok(), "offline source configuration: {result:?}");
        let Ok(result) = result else { return };
        assert_eq!(result.registry.sources.len(), 7);
        assert!(result
            .registry
            .sources
            .sources()
            .all(|source| source.policy() == AcquisitionPolicy::Offline));
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
    fn the_idle_window_defaults_to_ten_minutes_and_zero_disables_it() {
        let base = [
            "--endpoint".to_owned(),
            "/tmp/backend-locald-idle.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-idle".to_owned(),
        ];
        let parsed = ProcessConfig::parse(base.clone());
        assert!(parsed.is_ok(), "default idle window: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(
            parsed.listener.idle_timeout,
            Some(crate::DEFAULT_IDLE_TIMEOUT),
            "a spawned daemon must retire itself by default"
        );

        let explicit = ProcessConfig::parse(
            base.iter()
                .cloned()
                .chain(["--idle-timeout-ms".to_owned(), "300".to_owned()]),
        );
        assert!(explicit.is_ok(), "explicit idle window: {explicit:?}");
        let Ok(explicit) = explicit else { return };
        assert_eq!(
            explicit.listener.idle_timeout,
            Some(Duration::from_millis(300))
        );

        let never = ProcessConfig::parse(
            base.iter()
                .cloned()
                .chain(["--idle-timeout-ms".to_owned(), "0".to_owned()]),
        );
        assert!(never.is_ok(), "disabled idle window: {never:?}");
        let Ok(never) = never else { return };
        assert_eq!(
            never.listener.idle_timeout, None,
            "0 must mean never time out, not a zero-length window"
        );

        let malformed = ProcessConfig::parse(
            base.into_iter()
                .chain(["--idle-timeout-ms".to_owned(), "soon".to_owned()]),
        );
        assert!(
            matches!(malformed, Err(ProcessError::Usage(_))),
            "a malformed idle window must be a usage failure: {malformed:?}"
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

    #[test]
    fn advisory_authorities_are_explicitly_composable_and_bounded() {
        let parsed = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-advisory.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-advisory".to_owned(),
            "--advisory-osv".to_owned(),
            "/tmp/osv.json".to_owned(),
            "--advisory-rustsec".to_owned(),
            "https://example.invalid/rustsec.json".to_owned(),
            "--advisory-offline".to_owned(),
            "--advisory-max-age-secs".to_owned(),
            "42".to_owned(),
        ]);
        assert!(parsed.is_ok(), "advisory composition: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(parsed.advisory.sources.len(), 2);
        assert!(parsed.advisory.offline);
        assert_eq!(parsed.advisory.max_age_secs, 42);
        assert_eq!(parsed.advisory.max_feed_bytes, ADVISORY_MAX_FEED_BYTES);
    }
}
