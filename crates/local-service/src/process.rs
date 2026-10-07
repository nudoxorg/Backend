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
use crate::runtime_policy::{
    LocaldRuntimePolicy, MAX_RUNTIME_POLICY_BYTES, RUNTIME_POLICY_ENV, RuntimePolicyError,
};
use crate::service::{LocaldService, OwnerService};
use backend_engine::UnixEndpointPath;
use backend_engine::advisory::AdvisorySource;
use backend_engine::application::{
    ClosedLocalHostEnvironmentSnapshot, MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES,
};
use backend_engine::registry::{
    AcquisitionLimits, AcquisitionPolicy, AuthenticationToken, RegistryEcosystem, RegistryEndpoint,
    RegistrySource, RegistrySourceSet,
};
use backend_engine::{
    AcquisitionGate, ForgeAcquisitionLimits, ForgeAcquisitionPolicy, ForgeAuthToken, ForgeProvider,
    OfflinePolicy,
};
use backend_runtime::WorkspacePaths;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::Read;
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
/// Complete compiler-host environment captured by an embedding desktop owner.
pub const COMPILER_ENVIRONMENT_ENV: &str = "BACKEND_LOCALD_COMPILER_ENVIRONMENT";
/// Maximum encoded bytes accepted for a closed compiler-host environment.
pub const MAX_COMPILER_ENVIRONMENT_BYTES: usize = MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES;
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
/// Optional source-only catalog discovery feeds (`cargo=URL,nuget=URL`).
pub const REGISTRY_DISCOVERY_SOURCES_ENV: &str = "BACKEND_REGISTRY_DISCOVERY_SOURCES";
/// Disable network effects for catalog discovery while retaining cached claims.
pub const REGISTRY_DISCOVERY_OFFLINE_ENV: &str = "BACKEND_REGISTRY_DISCOVERY_OFFLINE";
/// Maximum NuGet catalog pages admitted in one discovery observation.
pub const REGISTRY_DISCOVERY_MAX_PAGES_ENV: &str = "BACKEND_REGISTRY_DISCOVERY_MAX_PAGES";
/// Environment variable naming an OSV JSON/batch feed path or HTTPS URL.
pub const ADVISORY_OSV_ENV: &str = "BACKEND_ADVISORY_OSV";
/// Environment variable selecting `all` or one OSV ecosystem archive scope.
pub const ADVISORY_OSV_SCOPE_ENV: &str = "BACKEND_ADVISORY_OSV_SCOPE";
/// Environment variable naming a RustSec TOML/tree path or HTTPS URL.
pub const ADVISORY_RUSTSEC_ENV: &str = "BACKEND_ADVISORY_RUSTSEC";
/// Environment variable naming a GHSA JSON/batch feed path or HTTPS URL.
pub const ADVISORY_GHSA_ENV: &str = "BACKEND_ADVISORY_GHSA";
/// Environment variable controlling the accepted advisory freshness window.
pub const ADVISORY_MAX_AGE_ENV: &str = "BACKEND_ADVISORY_MAX_AGE_SECS";
/// Environment variable disabling advisory network refreshes.
pub const ADVISORY_OFFLINE_ENV: &str = "BACKEND_ADVISORY_OFFLINE";
/// Environment variable disabling network acquisition for forge sources.
pub const FORGE_OFFLINE_ENV: &str = "BACKEND_FORGE_OFFLINE";
/// Environment variable carrying an optional default forge bearer token.
pub const FORGE_AUTH_ENV: &str = "BACKEND_FORGE_AUTH";
/// Environment variable naming an optional forge bearer token file.
pub const FORGE_AUTH_FILE_ENV: &str = "BACKEND_FORGE_AUTH_FILE";
/// Environment variable carrying comma-separated `provider=token` forge credentials.
pub const FORGE_AUTH_SCOPES_ENV: &str = "BACKEND_FORGE_AUTH_SCOPES";

/// Default registry archive admission for a single source archive.
///
/// Source sdists routinely exceed the 64 MiB replication object budget, so the
/// registry cap is deliberately independent of that transport limit and can be
/// raised with [`REGISTRY_MAX_ARCHIVE_BYTES_ENV`]. Values above the ceiling are
/// clamped and a deliberate overrun returns a typed acquisition error.
const REGISTRY_MAX_ARCHIVE_DEFAULT_BYTES: usize = 512 * 1024 * 1024;
const REGISTRY_MAX_ARCHIVE_CEILING_BYTES: usize = 1024 * 1024 * 1024;
const ADVISORY_MAX_FEED_BYTES: usize = 256 * 1024 * 1024;
const FORGE_MAX_AUTH_BYTES: usize = 4096;

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
    /// Durable code-forge source acquisition authority.
    pub forge: ForgeConfig,
    /// Source-only package catalog discovery, backed by the official public
    /// feed for each ecosystem unless explicit discovery sources are set.
    pub discovery: RegistryDiscoveryConfig,
    /// Aggregate admission policy for resident dependency facts and their index.
    /// These bound counts and copied key payloads, not total process RSS.
    pub package_graph_limits: backend_library::PackageGraphIndexLimits,
    /// Optional compiler-host environment snapshot received from the launcher.
    /// Some is closed, so every omitted role is absent. None asks locald to
    /// capture installed paths once at startup and then close that selection before
    /// the owner serves. The workspace-owned data root is always supplied by the
    /// embedded owner and is not part of this snapshot.
    pub compiler_environment: Option<ClosedLocalHostEnvironmentSnapshot>,
}

/// Source-only catalog discovery configuration. Zero-configuration locald
/// selects bounded public feeds for all seven ecosystems; explicit source
/// settings replace that set, and offline mode retains the durable snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryDiscoveryConfig {
    /// Source endpoints for the seven registry protocols. Each endpoint uses
    /// its ecosystem's discovery surface (for example an index API root or a
    /// catalog/replication feed), not an archive URL.
    pub sources: Vec<RegistryEndpoint>,
    /// Whether refresh requests are disabled.
    pub offline: bool,
    /// Maximum number of source pages or rows admitted in one refresh.
    pub max_pages: usize,
}

impl RegistryDiscoveryConfig {
    fn from_options(
        source_specs: Vec<String>,
        offline: bool,
        max_pages: Option<usize>,
    ) -> Result<Self, ProcessError> {
        let mut specs = source_specs;
        if let Ok(value) = std::env::var(REGISTRY_DISCOVERY_SOURCES_ENV) {
            specs.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
        let sources = if specs.is_empty() {
            official_discovery_sources()?
        } else {
            let mut sources = Vec::new();
            for spec in specs {
                let (ecosystem, endpoint) = parse_source_spec(&spec)?;
                let endpoint = RegistryEndpoint::new(ecosystem, endpoint).map_err(|_| {
                    ProcessError::Usage(
                        "registry discovery source is not an admitted absolute HTTPS or loopback HTTP URL"
                            .to_owned(),
                    )
                })?;
                if sources
                    .iter()
                    .any(|source: &RegistryEndpoint| source.id() == endpoint.id())
                {
                    return Err(ProcessError::Usage(
                        "registry discovery source is repeated".to_owned(),
                    ));
                }
                sources.push(endpoint);
            }
            sources
        };
        let environment_max_pages = std::env::var(REGISTRY_DISCOVERY_MAX_PAGES_ENV)
            .ok()
            .map(|value| {
                value.parse::<usize>().map_err(|_| {
                    ProcessError::Usage(
                        "registry discovery page limit must be an integer".to_owned(),
                    )
                })
            })
            .transpose()?;
        let max_pages = max_pages.or(environment_max_pages).unwrap_or(64);
        if max_pages == 0 || max_pages > 1024 {
            return Err(ProcessError::Usage(
                "registry discovery page limit must be between 1 and 1024".to_owned(),
            ));
        }
        Ok(Self {
            sources,
            offline: offline || env_flag(REGISTRY_DISCOVERY_OFFLINE_ENV),
            max_pages,
        })
    }
}

/// Feed endpoints consumed by the seven bounded source-only adapters. These
/// are catalog APIs, indexes, or source metadata trees; archive acquisition
/// continues to use its separate router and never follows discovery URLs.
fn official_discovery_sources() -> Result<Vec<RegistryEndpoint>, ProcessError> {
    [
        (RegistryEcosystem::Cargo, "https://crates.io"),
        (
            RegistryEcosystem::Npm,
            "https://replicate.npmjs.com/registry",
        ),
        (RegistryEcosystem::Pypi, "https://pypi.org"),
        (RegistryEcosystem::Maven, "https://search.maven.org"),
        (
            RegistryEcosystem::Nuget,
            "https://api.nuget.org/v3/catalog0/index.json",
        ),
        (RegistryEcosystem::Golang, "https://index.golang.org"),
        (
            RegistryEcosystem::Cpp,
            "https://api.github.com/repos/conan-io/conan-center-index",
        ),
    ]
    .into_iter()
    .map(|(ecosystem, endpoint)| {
        RegistryEndpoint::new(ecosystem, endpoint).map_err(|_| {
            ProcessError::Profile(format!(
                "built-in discovery endpoint for {} is invalid",
                ecosystem.as_str()
            ))
        })
    })
    .collect()
}

/// Code-forge source acquisition settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeConfig {
    /// Online or cache-only acquisition policy. Forge-reference is always cache-only.
    pub policy: ForgeAcquisitionPolicy,
    /// Resource limits applied before source and metadata admission.
    pub limits: ForgeAcquisitionLimits,
    /// Optional process-local default and provider-specific bearer credentials.
    pub authentication: ForgeAuthentication,
}

/// Forge credentials kept only in process memory and scoped by provider.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ForgeAuthentication {
    default: Option<ForgeAuthToken>,
    providers: BTreeMap<ForgeProvider, ForgeAuthToken>,
}

impl ForgeAuthentication {
    /// Returns a provider-scoped credential, falling back to the optional default.
    #[must_use]
    pub fn for_provider(&self, provider: ForgeProvider) -> Option<ForgeAuthToken> {
        self.providers
            .get(&provider)
            .cloned()
            .or_else(|| self.default.clone())
    }
}

impl ForgeConfig {
    fn from_options(
        offline: bool,
        auth_file: Option<String>,
        auth_scopes: Vec<String>,
        auth_file_scopes: Vec<String>,
        max_archive_bytes: Option<usize>,
        max_metadata_bytes: Option<usize>,
        max_readme_bytes: Option<usize>,
        max_entries: Option<usize>,
        max_tree_bytes: Option<usize>,
        max_path_bytes: Option<usize>,
        max_entry_bytes: Option<usize>,
    ) -> Result<Self, ProcessError> {
        let defaults = ForgeAcquisitionLimits::default();
        let limits = ForgeAcquisitionLimits {
            max_archive_bytes: max_archive_bytes
                .map_or(Ok(defaults.max_archive_bytes), u64::try_from)
                .map_err(|_| ProcessError::Usage("forge archive limit is oversized".to_owned()))?,
            max_metadata_bytes: max_metadata_bytes.unwrap_or(defaults.max_metadata_bytes),
            max_readme_bytes: max_readme_bytes.unwrap_or(defaults.max_readme_bytes),
            archive_budget: backend_engine::acquisition::ArchiveBudget {
                max_entries: max_entries.unwrap_or(defaults.archive_budget.max_entries),
                max_bytes: max_tree_bytes
                    .map_or(Ok(defaults.archive_budget.max_bytes), u64::try_from)
                    .map_err(|_| ProcessError::Usage("forge tree limit is oversized".to_owned()))?,
                max_path_bytes: max_path_bytes.unwrap_or(defaults.archive_budget.max_path_bytes),
                max_entry_bytes: max_entry_bytes
                    .map_or(Ok(defaults.archive_budget.max_entry_bytes), u64::try_from)
                    .map_err(|_| {
                        ProcessError::Usage("forge entry limit is oversized".to_owned())
                    })?,
            },
        };
        if limits.max_archive_bytes == 0
            || limits.max_archive_bytes > defaults.max_archive_bytes
            || limits.max_metadata_bytes == 0
            || limits.max_metadata_bytes > defaults.max_metadata_bytes
            || limits.max_readme_bytes == 0
            || limits.max_readme_bytes > defaults.max_readme_bytes
            || limits.archive_budget.max_entries == 0
            || limits.archive_budget.max_bytes == 0
            || limits.archive_budget.max_bytes > defaults.archive_budget.max_bytes
            || limits.archive_budget.max_path_bytes == 0
            || limits.archive_budget.max_path_bytes > defaults.archive_budget.max_path_bytes
            || limits.archive_budget.max_entry_bytes == 0
            || limits.archive_budget.max_entry_bytes > defaults.archive_budget.max_entry_bytes
        {
            return Err(ProcessError::Usage(
                "forge limits must be positive and within the built-in acquisition ceilings"
                    .to_owned(),
            ));
        }
        let policy = if offline || env_flag(FORGE_OFFLINE_ENV) {
            ForgeAcquisitionPolicy::Offline
        } else {
            ForgeAcquisitionPolicy::Online
        };
        let authentication_value =
            if let Some(path) = auth_file.or_else(|| std::env::var(FORGE_AUTH_FILE_ENV).ok()) {
                Some(read_forge_authentication_file(&path)?)
            } else {
                std::env::var(FORGE_AUTH_ENV).ok()
            };
        let default = authentication_value.map(forge_auth_token).transpose()?;
        let mut providers = BTreeMap::new();
        let mut auth_scopes = auth_scopes;
        if let Ok(value) = std::env::var(FORGE_AUTH_SCOPES_ENV) {
            auth_scopes.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
        for spec in auth_scopes {
            let (provider, token) = spec.split_once('=').ok_or_else(|| {
                ProcessError::Usage("forge auth scope must use provider=token spelling".to_owned())
            })?;
            let provider = parse_forge_provider(provider.trim())?;
            if providers
                .insert(provider, forge_auth_token(token.trim().to_owned())?)
                .is_some()
            {
                return Err(ProcessError::Usage(
                    "forge auth scope repeats a provider".to_owned(),
                ));
            }
        }
        for spec in auth_file_scopes {
            let (provider, path) = spec.split_once('=').ok_or_else(|| {
                ProcessError::Usage(
                    "forge auth file scope must use provider=path spelling".to_owned(),
                )
            })?;
            let provider = parse_forge_provider(provider.trim())?;
            let path = path.trim();
            if path.is_empty() {
                return Err(ProcessError::Usage(
                    "forge auth file scope path is empty".to_owned(),
                ));
            }
            let token = read_forge_authentication_file(path)?;
            if providers
                .insert(provider, forge_auth_token(token)?)
                .is_some()
            {
                return Err(ProcessError::Usage(
                    "forge auth scope repeats a provider".to_owned(),
                ));
            }
        }
        Ok(Self {
            policy,
            limits,
            authentication: ForgeAuthentication { default, providers },
        })
    }
}

fn parse_forge_provider(value: &str) -> Result<ForgeProvider, ProcessError> {
    match value.to_ascii_lowercase().as_str() {
        "github" => Ok(ForgeProvider::Github),
        "gitlab" => Ok(ForgeProvider::Gitlab),
        "codeberg" => Ok(ForgeProvider::Codeberg),
        "generic-https-git" => Ok(ForgeProvider::GenericHttpsGit),
        _ => Err(ProcessError::Usage(
            "forge auth scope names an unsupported provider".to_owned(),
        )),
    }
}

fn forge_auth_token(value: String) -> Result<ForgeAuthToken, ProcessError> {
    if value.len() > FORGE_MAX_AUTH_BYTES {
        return Err(ProcessError::Usage(
            "forge authentication value is oversized".to_owned(),
        ));
    }
    ForgeAuthToken::new(value)
        .map_err(|_| ProcessError::Usage("forge authentication value is invalid".to_owned()))
}

fn read_forge_authentication_file(path: &str) -> Result<String, ProcessError> {
    #[cfg(unix)]
    let file = {
        let descriptor = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| ProcessError::Usage("could not open forge authentication file".to_owned()))?;
        fs::File::from(descriptor)
    };
    #[cfg(not(unix))]
    let file = fs::File::open(path)
        .map_err(|_| ProcessError::Usage("could not open forge authentication file".to_owned()))?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(ProcessError::Usage(
            "forge authentication path is not a regular file".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(FORGE_MAX_AUTH_BYTES + 1);
    file.take((FORGE_MAX_AUTH_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ProcessError::Usage("could not read forge authentication file".to_owned()))?;
    if bytes.len() > FORGE_MAX_AUTH_BYTES {
        return Err(ProcessError::Usage(
            "forge authentication file is oversized".to_owned(),
        ));
    }
    String::from_utf8(bytes)
        .map(|value| value.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|_| ProcessError::Usage("forge authentication file is not UTF-8".to_owned()))
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
    /// Coverage scope explicitly selected for an OSV ZIP source. JSON inputs
    /// remain incomplete observations because they cannot prove a full dump.
    pub osv_scope: Option<backend_engine::advisory::OsvFeedScope>,
    /// Maximum age of a source frontier before it becomes stale.
    pub max_age_secs: u64,
    /// Whether refresh network effects are disabled.
    pub offline: bool,
    /// Whether an explicit advisory refresh command may update local feeds.
    /// When false, cached advisory facts remain available to reads and policy
    /// checks; the setting only suppresses refresh work.
    pub refresh_enabled: bool,
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
    /// Maximum age in milliseconds for a prior source observation to satisfy
    /// a request. `None` preserves standalone locald's existing behavior:
    /// live adds revalidate, while explicit offline adds may use the durable
    /// cache. A desktop sets `Some(0)` to disable reuse or a positive horizon
    /// to apply its persisted cache preference.
    pub cache_max_age_millis: Option<u64>,
}

/// User-selected policy for registry-backed metadata handled by a desktop
/// embedded owner. Attached owners do not receive this policy: their operator
/// remains authoritative.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryUserPolicy {
    /// Whether registry and registry-discovery network requests are allowed.
    pub allow_remote_metadata: bool,
    /// Whether explicit advisory-feed refresh requests may run.
    pub refresh_advisories: bool,
    /// Whether recent admitted registry results may be reused.
    pub cache_enabled: bool,
    /// Maximum age allowed for a reusable registry result.
    pub cache_max_age_days: u16,
}

impl RegistryUserPolicy {
    const MAX_CACHE_AGE_DAYS: u16 = 90;
    const MILLIS_PER_DAY: u64 = 86_400_000;

    fn cache_max_age_millis(self) -> u64 {
        if !self.cache_enabled {
            return 0;
        }
        u64::from(self.cache_max_age_days.clamp(1, Self::MAX_CACHE_AGE_DAYS))
            .saturating_mul(Self::MILLIS_PER_DAY)
    }
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
            // Standalone locald retains its prior always-revalidate behavior.
            // A desktop applies its explicit persisted cache preference before
            // composing an embedded owner.
            cache_max_age_millis: None,
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
        osv_scope: Option<String>,
        rustsec: Option<String>,
        ghsa: Option<String>,
        offline: bool,
        max_age_secs: Option<usize>,
    ) -> Result<Self, ProcessError> {
        let osv = osv.or_else(|| std::env::var(ADVISORY_OSV_ENV).ok());
        let osv_scope = osv_scope.or_else(|| std::env::var(ADVISORY_OSV_SCOPE_ENV).ok());
        let osv_scope = match osv_scope.as_deref() {
            Some(value) => Some(
                backend_engine::advisory::OsvFeedScope::parse(value).ok_or_else(|| {
                    ProcessError::Usage(format!(
                        "{ADVISORY_OSV_SCOPE_ENV} must be all, cargo, npm, pypi, maven, nuget, or go"
                    ))
                })?,
            ),
            None => None,
        };
        if osv_scope.is_some() && osv.is_none() {
            return Err(ProcessError::Usage(
                "an OSV scope requires an OSV source".to_owned(),
            ));
        }
        let mut sources = Vec::new();
        for (source, value, env) in [
            (AdvisorySource::Osv, osv, ADVISORY_OSV_ENV),
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
            osv_scope,
            max_age_secs,
            offline: offline || env_flag(ADVISORY_OFFLINE_ENV),
            refresh_enabled: true,
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

/// Uses the existing product source-admission slab size for each graph input
/// pool. Operators can scale the independent ceilings for a vertical index;
/// embedded clients need no additional configuration. Fact content and lookup
/// text have independent logical byte ceilings; allocator slack, heap node
/// overhead, and unrelated resident data are excluded.
pub(crate) fn default_package_graph_limits() -> backend_library::PackageGraphIndexLimits {
    let bytes = crate::builtin::MAX_REBUILD_BYTES;
    let rows = bytes / size_of::<backend_library::PackageDependencyRecord>();
    backend_library::PackageGraphIndexLimits {
        max_sources: bytes / size_of::<backend_library::PackageDependencySourceFacts>(),
        max_total_rows: rows,
        max_reverse_edges: rows,
        max_index_key_bytes: bytes,
        max_fact_bytes: bytes,
    }
}

fn package_graph_limits_from_environment()
-> Result<backend_library::PackageGraphIndexLimits, ProcessError> {
    let limit = |name, fallback| package_graph_limit(name, std::env::var(name), fallback);
    let defaults = default_package_graph_limits();
    Ok(backend_library::PackageGraphIndexLimits {
        max_sources: limit("NUDOX_GRAPH_MAX_SOURCES", defaults.max_sources)?,
        max_total_rows: limit("NUDOX_GRAPH_MAX_ROWS", defaults.max_total_rows)?,
        max_reverse_edges: limit("NUDOX_GRAPH_MAX_REVERSE_EDGES", defaults.max_reverse_edges)?,
        max_index_key_bytes: limit("NUDOX_GRAPH_MAX_KEY_BYTES", defaults.max_index_key_bytes)?,
        max_fact_bytes: limit("NUDOX_GRAPH_MAX_FACT_BYTES", defaults.max_fact_bytes)?,
    })
}

fn package_graph_limit(
    name: &str,
    value: Result<String, std::env::VarError>,
    fallback: usize,
) -> Result<usize, ProcessError> {
    let raw = match value {
        Ok(raw) => raw,
        Err(std::env::VarError::NotPresent) => return Ok(fallback),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(ProcessError::Usage(format!(
                "{name} must be a positive integer"
            )));
        }
    };
    raw.trim()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| ProcessError::Usage(format!("{name} must be a positive integer")))
}

impl ProcessConfig {
    /// Captures the effective registry, discovery, advisory, and cache policy
    /// after all process and desktop settings have been applied.
    ///
    /// # Errors
    /// Returns an error when the current cache horizon is outside the shared
    /// runtime-policy bound.
    pub fn runtime_policy(&self) -> Result<LocaldRuntimePolicy, RuntimePolicyError> {
        let registry_network_allowed = matches!(self.registry.policy, AcquisitionPolicy::Online);
        LocaldRuntimePolicy::new(
            registry_network_allowed,
            registry_network_allowed && !self.discovery.offline,
            !self.advisory.offline,
            self.advisory.refresh_enabled,
            self.registry.cache_max_age_millis,
        )
    }

    /// Applies a host policy after normal CLI and environment configuration.
    /// This merge can narrow network access, refresh work, and cache age; it
    /// never re-enables a path already closed by locald's operator settings.
    pub fn apply_runtime_policy(&mut self, policy: LocaldRuntimePolicy) {
        if !policy.registry_network_allowed()
            || matches!(self.registry.policy, AcquisitionPolicy::Offline)
        {
            self.registry.policy = AcquisitionPolicy::Offline;
            self.registry.sources = self.registry.sources.clone().offline();
        }
        if !policy.discovery_network_allowed()
            || matches!(self.registry.policy, AcquisitionPolicy::Offline)
        {
            self.discovery.offline = true;
        }
        if !policy.advisory_network_allowed() {
            self.advisory.offline = true;
        }
        if !policy.advisory_refresh_enabled() {
            self.advisory.refresh_enabled = false;
        }
        self.registry.cache_max_age_millis = match (
            self.registry.cache_max_age_millis,
            policy.registry_cache_max_age_millis(),
        ) {
            (Some(current), Some(incoming)) => Some(current.min(incoming)),
            (Some(current), None) => Some(current),
            (None, Some(incoming)) => Some(incoming),
            (None, None) => None,
        };
    }

    /// Applies preferences read by a desktop before it starts its embedded
    /// service. Existing operator-level offline settings remain restrictive;
    /// user preferences can narrow service behavior but cannot enable a
    /// network path that the process configuration disabled, re-enable a
    /// refresh path already disabled, or widen an explicit cache horizon.
    pub fn apply_registry_user_policy(&mut self, policy: RegistryUserPolicy) {
        let runtime_policy = LocaldRuntimePolicy::new(
            policy.allow_remote_metadata,
            policy.allow_remote_metadata,
            policy.allow_remote_metadata,
            policy.refresh_advisories,
            Some(policy.cache_max_age_millis()),
        )
        .expect("registry user policy clamps cache age to the shared runtime bound");
        self.apply_runtime_policy(runtime_policy);
    }

    /// Parses bounded process arguments and environment fallbacks.
    /// An inherited [`RUNTIME_POLICY_ENV`] value is validated and merged after
    /// ordinary configuration, so it can narrow but never reopen locald paths. An inherited
    /// [`COMPILER_ENVIRONMENT_ENV`] value, when present, closes every compiler-host role not
    /// included in its snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, missing, or unsupported arguments.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, ProcessError> {
        let runtime_policy_value = std::env::var_os(RUNTIME_POLICY_ENV);
        let compiler_environment_value = std::env::var_os(COMPILER_ENVIRONMENT_ENV);
        Self::parse_with_environment_values(
            args,
            runtime_policy_value.as_deref(),
            compiler_environment_value.as_deref(),
        )
    }

    #[cfg(test)]
    fn parse_with_runtime_policy(
        args: impl IntoIterator<Item = String>,
        runtime_policy_value: Option<&OsStr>,
    ) -> Result<Self, ProcessError> {
        Self::parse_with_environment_values(args, runtime_policy_value, None)
    }

    fn parse_with_environment_values(
        args: impl IntoIterator<Item = String>,
        runtime_policy_value: Option<&OsStr>,
        compiler_environment_value: Option<&OsStr>,
    ) -> Result<Self, ProcessError> {
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
            advisory_osv_scope,
            advisory_rustsec,
            advisory_ghsa,
            advisory_offline,
            advisory_max_age_secs,
            forge_offline,
            forge_auth_file,
            forge_auth_scopes,
            forge_auth_file_scopes,
            forge_max_archive_bytes,
            forge_max_metadata_bytes,
            forge_max_readme_bytes,
            forge_max_entries,
            forge_max_tree_bytes,
            forge_max_path_bytes,
            forge_max_entry_bytes,
            registry_discovery_sources,
            registry_discovery_offline,
            registry_discovery_max_pages,
            help,
        } = parse_options(args)?;
        if help {
            return Err(ProcessError::Help);
        }
        let compiler_environment = parse_compiler_environment(compiler_environment_value)?;
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
            advisory_osv_scope,
            advisory_rustsec,
            advisory_ghsa,
            advisory_offline,
            advisory_max_age_secs,
        )?;
        let forge = ForgeConfig::from_options(
            forge_offline,
            forge_auth_file,
            forge_auth_scopes,
            forge_auth_file_scopes,
            forge_max_archive_bytes,
            forge_max_metadata_bytes,
            forge_max_readme_bytes,
            forge_max_entries,
            forge_max_tree_bytes,
            forge_max_path_bytes,
            forge_max_entry_bytes,
        )?;
        let discovery = RegistryDiscoveryConfig::from_options(
            registry_discovery_sources,
            registry_discovery_offline || registry_offline,
            registry_discovery_max_pages,
        )?;
        let mut config = Self {
            endpoint,
            workspace: paths.data().to_path_buf(),
            profile,
            worker_endpoint,
            authority_secret,
            listener,
            registry,
            advisory,
            forge,
            discovery,
            package_graph_limits: package_graph_limits_from_environment()?,
            compiler_environment,
        };
        apply_runtime_policy_environment_value(&mut config, runtime_policy_value)?;
        Ok(config)
    }
}

fn apply_runtime_policy_environment_value(
    config: &mut ProcessConfig,
    value: Option<&OsStr>,
) -> Result<(), ProcessError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.as_encoded_bytes().len() > MAX_RUNTIME_POLICY_BYTES {
        return Err(ProcessError::Usage(format!(
            "{RUNTIME_POLICY_ENV} exceeds its size limit"
        )));
    }
    let value = value.to_str().ok_or_else(|| {
        ProcessError::Usage(format!(
            "{RUNTIME_POLICY_ENV} must contain UTF-8 policy data"
        ))
    })?;
    let policy = LocaldRuntimePolicy::parse(value).map_err(|error| {
        ProcessError::Usage(format!("{RUNTIME_POLICY_ENV} is invalid: {error}"))
    })?;
    config.apply_runtime_policy(policy);
    Ok(())
}

fn parse_compiler_environment(
    value: Option<&OsStr>,
) -> Result<Option<ClosedLocalHostEnvironmentSnapshot>, ProcessError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.as_encoded_bytes().len() > MAX_COMPILER_ENVIRONMENT_BYTES {
        return Err(ProcessError::Usage(format!(
            "{COMPILER_ENVIRONMENT_ENV} exceeds its size limit"
        )));
    }
    let value = value.to_str().ok_or_else(|| {
        ProcessError::Usage(format!(
            "{COMPILER_ENVIRONMENT_ENV} must contain UTF-8 data"
        ))
    })?;
    ClosedLocalHostEnvironmentSnapshot::parse(value)
        .map(Some)
        .map_err(|_| ProcessError::Usage(format!("{COMPILER_ENVIRONMENT_ENV} is invalid")))
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
    advisory_osv_scope: Option<String>,
    advisory_rustsec: Option<String>,
    advisory_ghsa: Option<String>,
    advisory_offline: bool,
    advisory_max_age_secs: Option<usize>,
    forge_offline: bool,
    forge_auth_file: Option<String>,
    forge_auth_scopes: Vec<String>,
    forge_auth_file_scopes: Vec<String>,
    forge_max_archive_bytes: Option<usize>,
    forge_max_metadata_bytes: Option<usize>,
    forge_max_readme_bytes: Option<usize>,
    forge_max_entries: Option<usize>,
    forge_max_tree_bytes: Option<usize>,
    forge_max_path_bytes: Option<usize>,
    forge_max_entry_bytes: Option<usize>,
    registry_discovery_sources: Vec<String>,
    registry_discovery_offline: bool,
    registry_discovery_max_pages: Option<usize>,
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
        advisory_osv_scope: None,
        advisory_rustsec: None,
        advisory_ghsa: None,
        advisory_offline: false,
        advisory_max_age_secs: None,
        forge_offline: false,
        forge_auth_file: None,
        forge_auth_scopes: Vec::new(),
        forge_auth_file_scopes: Vec::new(),
        forge_max_archive_bytes: None,
        forge_max_metadata_bytes: None,
        forge_max_readme_bytes: None,
        forge_max_entries: None,
        forge_max_tree_bytes: None,
        forge_max_path_bytes: None,
        forge_max_entry_bytes: None,
        registry_discovery_sources: Vec::new(),
        registry_discovery_offline: false,
        registry_discovery_max_pages: None,
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
            "--advisory-osv-scope" => {
                parsed.advisory_osv_scope = Some(next_value(&mut args, "--advisory-osv-scope")?);
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
            "--forge-offline" => parsed.forge_offline = true,
            "--forge-auth-file" => {
                parsed.forge_auth_file = Some(next_value(&mut args, "--forge-auth-file")?);
            }
            "--forge-auth-file-for" => {
                parsed
                    .forge_auth_file_scopes
                    .push(next_value(&mut args, "--forge-auth-file-for")?);
            }
            "--forge-max-archive-bytes" => {
                parsed.forge_max_archive_bytes = Some(parse_positive(
                    &next_value(&mut args, "--forge-max-archive-bytes")?,
                    "--forge-max-archive-bytes",
                )?);
            }
            "--forge-max-metadata-bytes" => {
                parsed.forge_max_metadata_bytes = Some(parse_positive(
                    &next_value(&mut args, "--forge-max-metadata-bytes")?,
                    "--forge-max-metadata-bytes",
                )?);
            }
            "--forge-max-readme-bytes" => {
                parsed.forge_max_readme_bytes = Some(parse_positive(
                    &next_value(&mut args, "--forge-max-readme-bytes")?,
                    "--forge-max-readme-bytes",
                )?);
            }
            "--forge-max-entries" => {
                parsed.forge_max_entries = Some(parse_positive(
                    &next_value(&mut args, "--forge-max-entries")?,
                    "--forge-max-entries",
                )?);
            }
            "--forge-max-tree-bytes" => {
                parsed.forge_max_tree_bytes = Some(parse_positive(
                    &next_value(&mut args, "--forge-max-tree-bytes")?,
                    "--forge-max-tree-bytes",
                )?);
            }
            "--forge-max-path-bytes" => {
                parsed.forge_max_path_bytes = Some(parse_positive(
                    &next_value(&mut args, "--forge-max-path-bytes")?,
                    "--forge-max-path-bytes",
                )?);
            }
            "--forge-max-entry-bytes" => {
                parsed.forge_max_entry_bytes = Some(parse_positive(
                    &next_value(&mut args, "--forge-max-entry-bytes")?,
                    "--forge-max-entry-bytes",
                )?);
            }
            "--registry-discovery-source" => {
                parsed
                    .registry_discovery_sources
                    .push(next_value(&mut args, "--registry-discovery-source")?);
            }
            "--registry-discovery-offline" => parsed.registry_discovery_offline = true,
            "--registry-discovery-max-pages" => {
                parsed.registry_discovery_max_pages = Some(parse_positive(
                    &next_value(&mut args, "--registry-discovery-max-pages")?,
                    "--registry-discovery-max-pages",
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
            report_process_failure(&error);
            ExitCode::from(EX_USAGE)
        }
        Err(
            error @ (ProcessError::OwnerContended { .. }
            | ProcessError::Listener(ListenerError::AlreadyRunning)),
        ) => {
            report_process_failure(&error);
            ExitCode::from(backend_runtime::OWNER_CONTENDED_EXIT_CODE)
        }
        Err(error @ ProcessError::Profile(_)) => {
            report_process_failure(&error);
            ExitCode::from(EX_UNAVAILABLE)
        }
        Err(error) => {
            report_process_failure(&error);
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
            report_process_failure(&error);
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
                        report_process_failure(&error);
                        return ExitCode::from(EX_USAGE);
                    }
                }
                crate::builtin::run(config)
            }
            profile => {
                report_process_failure(&ProcessError::Profile(format!(
                    "unknown compiled profile {profile}"
                )));
                ExitCode::from(EX_UNAVAILABLE)
            }
        },
    }
}

/// Reports the original failure without attaching a detached owner's stderr
/// to its launcher or allowing a secondary diagnostic failure to replace it.
pub(crate) fn report_process_failure(error: &dyn fmt::Display) {
    if let Ok(Some(reporter)) = backend_runtime::StartupFailureReporter::from_environment() {
        let _ = reporter.report(error);
    }
    eprintln!("locald: {error}");
}

fn print_help() {
    println!(
        "usage: backend-locald [--endpoint PATH] [--workspace PATH] [--profile builtin|builtin-echo] [--worker-endpoint PATH] [--authority-secret-file PATH] [--registry-source ECO=URL]... [--registry-auth-for URL=TOKEN]... [--registry-endpoint URL] [--registry-ecosystem NAME] [--registry-auth VALUE|--registry-auth-file PATH] [--registry-native] [--registry-offline] [--registry-discovery-source ECO=URL]... [--registry-discovery-offline] [--registry-discovery-max-pages COUNT] [--advisory-osv PATH|URL] [--advisory-osv-scope all|cargo|npm|pypi|maven|nuget|go] [--advisory-rustsec PATH|URL] [--advisory-ghsa PATH|URL] [--advisory-offline] [--advisory-max-age-secs SECONDS] [--forge-offline] [--forge-auth-file PATH] [--forge-auth-file-for PROVIDER=PATH]... [--forge-max-archive-bytes BYTES] [--forge-max-metadata-bytes BYTES] [--forge-max-readme-bytes BYTES] [--forge-max-entries COUNT] [--forge-max-tree-bytes BYTES] [--forge-max-path-bytes BYTES] [--forge-max-entry-bytes BYTES] [--max-frame BYTES] [--max-clients COUNT] [--timeout-ms MS] [--idle-timeout-ms MS]"
    );
    println!(
        "without paths, locald opens this project's private per-user app-data root and derives a short local endpoint"
    );
    println!(
        "locald retires itself after --idle-timeout-ms with no connected client (default 600000); 0 never times out"
    );
    println!(
        "catalog discovery runs asynchronously from official feeds for all seven ecosystems; repeat --registry-discovery-source to select feeds, or use --registry-discovery-offline to read only the cached index"
    );
    println!(
        "--registry-source configures package metadata and archive acquisition; --registry-discovery-source configures the catalog feeds used by index-search"
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
    /// Another process holds the checked workspace lease; its listener may
    /// still be opening. This is not a refused compiler or damaged state.
    OwnerContended {
        /// Exact workspace whose owner lease refused this contender.
        workspace: PathBuf,
    },
    /// The workspace is intact but was written by another build of the
    /// product, in a layout this build does not read. The message names what
    /// was recognised. Nothing here is corrupt: a host that owns its
    /// workspace can set it aside and index again
    /// ([`crate::EmbeddedLocalService::start_replacing_state_from_another_build`]);
    /// one that does not must ask its operator.
    StateFromAnotherBuild(String),
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
            Self::OwnerContended { workspace } => write!(
                formatter,
                "another process owns workspace {}; waiting for its local endpoint is safe, but replacing its state is not",
                workspace.display()
            ),
            Self::StateFromAnotherBuild(message) => {
                write!(
                    formatter,
                    "workspace state is from another build: {message}"
                )
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

    fn parse_config_with_policy_value(value: Option<&OsStr>, extra_args: &[&str]) -> ProcessConfig {
        let mut args = vec![
            "--endpoint".to_owned(),
            "/tmp/backend-locald-runtime-policy.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-runtime-policy".to_owned(),
        ];
        args.extend(extra_args.iter().map(|arg| (*arg).to_owned()));
        ProcessConfig::parse_with_runtime_policy(args, value)
            .expect("parse explicit locald runtime policy fixture")
    }

    fn policy(
        registry_network_allowed: bool,
        discovery_network_allowed: bool,
        advisory_network_allowed: bool,
        advisory_refresh_enabled: bool,
        registry_cache_max_age_millis: Option<u64>,
    ) -> LocaldRuntimePolicy {
        LocaldRuntimePolicy::new(
            registry_network_allowed,
            discovery_network_allowed,
            advisory_network_allowed,
            advisory_refresh_enabled,
            registry_cache_max_age_millis,
        )
        .expect("bounded runtime policy fixture")
    }

    fn parse_config_with_compiler_environment_value(
        value: Option<&OsStr>,
    ) -> Result<ProcessConfig, ProcessError> {
        ProcessConfig::parse_with_environment_values(
            [
                "--endpoint".to_owned(),
                "/tmp/backend-locald-closed-environment.sock".to_owned(),
                "--workspace".to_owned(),
                "/tmp/backend-locald-closed-environment".to_owned(),
            ],
            None,
            value,
        )
    }

    fn empty_compiler_environment_snapshot() -> ClosedLocalHostEnvironmentSnapshot {
        ClosedLocalHostEnvironmentSnapshot::from_paths(std::iter::empty::<(
            backend_engine::application::LocalHostVariable,
            PathBuf,
        )>())
        .expect("empty snapshot")
    }

    #[test]
    fn cold_parse_distinguishes_absent_from_an_empty_closed_snapshot() {
        let absent = parse_config_with_compiler_environment_value(None)
            .expect("absent snapshot keeps operator mode");
        assert_eq!(absent.compiler_environment, None);

        let empty = empty_compiler_environment_snapshot()
            .encode()
            .expect("canonical empty snapshot");
        let closed = parse_config_with_compiler_environment_value(Some(OsStr::new(&empty)))
            .expect("parse empty closed snapshot");
        assert_eq!(
            closed.compiler_environment,
            Some(empty_compiler_environment_snapshot())
        );
        assert_eq!(
            closed.compiler_environment.as_ref().and_then(|snapshot| {
                snapshot.path(backend_engine::application::LocalHostVariable::Home)
            }),
            None,
            "an explicitly present empty snapshot seals all compiler-host roles absent"
        );
    }

    #[test]
    fn malformed_or_oversized_compiler_environment_is_a_value_free_usage_error() {
        let secret_like_input = "{\"version\":1,\"token\":\"private-path-secret\"}";
        let error =
            parse_config_with_compiler_environment_value(Some(OsStr::new(secret_like_input)))
                .expect_err("unknown snapshot fields must fail closed");
        assert!(matches!(error, ProcessError::Usage(_)));
        assert!(!error.to_string().contains(secret_like_input));
        assert!(!error.to_string().contains("private-path-secret"));

        let oversized = "x".repeat(MAX_COMPILER_ENVIRONMENT_BYTES + 1);
        let error = parse_config_with_compiler_environment_value(Some(OsStr::new(&oversized)))
            .expect_err("snapshot byte bound is enforced before parsing");
        assert!(matches!(error, ProcessError::Usage(_)));
        assert!(!error.to_string().contains(&oversized));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_compiler_environment_is_rejected_without_echoing_native_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let value = OsString::from_vec(b"/private/compiler-token-\xff".to_vec());
        let error = parse_config_with_compiler_environment_value(Some(&value))
            .expect_err("the JSON environment protocol is UTF-8");
        assert!(matches!(error, ProcessError::Usage(_)));
        assert!(!error.to_string().contains("compiler-token"));
    }

    #[test]
    fn runtime_policy_cache_ceiling_matches_the_existing_desktop_setting() {
        assert_eq!(
            crate::runtime_policy::MAX_REGISTRY_CACHE_AGE_MILLIS,
            u64::from(RegistryUserPolicy::MAX_CACHE_AGE_DAYS) * RegistryUserPolicy::MILLIS_PER_DAY
        );
    }

    #[test]
    fn cold_parse_applies_canonical_policy_after_local_options() {
        let allowed = policy(true, true, true, true, None);
        let encoded = allowed.encode().expect("encode policy");
        let config = parse_config_with_policy_value(
            Some(OsStr::new(&encoded)),
            &[
                "--registry-offline",
                "--registry-discovery-offline",
                "--advisory-offline",
            ],
        );
        assert_eq!(config.registry.policy, AcquisitionPolicy::Offline);
        assert!(config.discovery.offline);
        assert!(config.advisory.offline);
        assert!(config.advisory.refresh_enabled);

        let mut without_policy = parse_config_with_policy_value(None, &[]);
        let before_absent_policy = without_policy.clone();
        apply_runtime_policy_environment_value(&mut without_policy, None)
            .expect("absence leaves ordinary configuration alone");
        assert_eq!(without_policy, before_absent_policy);
    }

    #[test]
    fn runtime_policy_closes_network_refresh_and_preserves_zero_cache_horizon() {
        let restrictive = policy(false, false, false, false, Some(0));
        let mut config = parse_config_with_policy_value(None, &[]);
        config.apply_runtime_policy(restrictive);

        assert_eq!(config.registry.policy, AcquisitionPolicy::Offline);
        assert!(
            config
                .registry
                .sources
                .sources()
                .all(|source| { source.policy() == AcquisitionPolicy::Offline })
        );
        assert!(config.discovery.offline);
        assert!(config.advisory.offline);
        assert!(!config.advisory.refresh_enabled);
        assert_eq!(config.registry.cache_max_age_millis, Some(0));
        assert_eq!(
            config
                .runtime_policy()
                .expect("snapshot restrictive policy"),
            restrictive
        );
    }

    #[test]
    fn runtime_policy_merge_never_reopens_paths_and_takes_the_tighter_cache_age() {
        let mut config = parse_config_with_policy_value(
            None,
            &[
                "--registry-offline",
                "--registry-discovery-offline",
                "--advisory-offline",
            ],
        );
        config.advisory.refresh_enabled = false;
        config.registry.cache_max_age_millis = Some(100);
        config.apply_runtime_policy(policy(true, true, true, true, Some(500)));

        assert_eq!(config.registry.policy, AcquisitionPolicy::Offline);
        assert!(config.discovery.offline);
        assert!(config.advisory.offline);
        assert!(!config.advisory.refresh_enabled);
        assert_eq!(config.registry.cache_max_age_millis, Some(100));

        let mut config = parse_config_with_policy_value(None, &[]);
        config.registry.cache_max_age_millis = Some(500);
        config.apply_runtime_policy(policy(true, true, true, true, Some(100)));
        assert_eq!(config.registry.cache_max_age_millis, Some(100));

        config.registry.cache_max_age_millis = None;
        config.apply_runtime_policy(policy(true, true, true, true, None));
        assert_eq!(config.registry.cache_max_age_millis, None);
        config.apply_runtime_policy(policy(true, true, true, true, Some(0)));
        assert_eq!(config.registry.cache_max_age_millis, Some(0));
    }

    #[test]
    fn malformed_runtime_policy_is_a_safe_usage_error_and_absence_is_allowed() {
        let secret_like_input = "not-json-/private/path/token";
        let args = [
            "--endpoint".to_owned(),
            "/tmp/backend-locald-runtime-policy-error.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-runtime-policy-error".to_owned(),
        ];
        let error =
            ProcessConfig::parse_with_runtime_policy(args, Some(OsStr::new(secret_like_input)))
                .expect_err("malformed runtime policy must fail closed");
        assert!(matches!(error, ProcessError::Usage(_)));
        assert!(!error.to_string().contains(secret_like_input));

        let args = [
            "--endpoint".to_owned(),
            "/tmp/backend-locald-runtime-policy-absent.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-runtime-policy-absent".to_owned(),
        ];
        assert!(ProcessConfig::parse_with_runtime_policy(args, None).is_ok());

        let oversized = "x".repeat(MAX_RUNTIME_POLICY_BYTES + 1);
        let args = [
            "--endpoint".to_owned(),
            "/tmp/backend-locald-runtime-policy-large.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-runtime-policy-large".to_owned(),
        ];
        let error = ProcessConfig::parse_with_runtime_policy(args, Some(OsStr::new(&oversized)))
            .expect_err("oversized runtime policy must fail before parsing");
        assert!(matches!(error, ProcessError::Usage(_)));
        assert!(!error.to_string().contains(&oversized));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_runtime_policy_is_a_safe_usage_error() {
        use std::os::unix::ffi::OsStrExt;

        let invalid_utf8 = OsStr::from_bytes(b"\xff");
        let config = ProcessConfig::parse_with_runtime_policy(
            [
                "--endpoint".to_owned(),
                "/tmp/backend-locald-runtime-policy-non-utf8.sock".to_owned(),
                "--workspace".to_owned(),
                "/tmp/backend-locald-runtime-policy-non-utf8".to_owned(),
            ],
            Some(invalid_utf8),
        );
        assert!(matches!(config, Err(ProcessError::Usage(message)) if message.contains("UTF-8")));
    }

    #[test]
    fn graph_admission_configuration_rejects_invalid_limits_without_environment_mutation() {
        const NAME: &str = "NUDOX_GRAPH_MAX_ROWS";
        assert_eq!(
            package_graph_limit(NAME, Err(std::env::VarError::NotPresent), 17)
                .expect("absent limit retains the embedding policy"),
            17
        );
        assert_eq!(
            package_graph_limit(NAME, Ok(" 42 ".to_owned()), 17)
                .expect("operator can raise the limit"),
            42
        );
        for value in ["", "0", "-1", "1.5", "unlimited"] {
            assert!(matches!(
                package_graph_limit(NAME, Ok(value.to_owned()), 17),
                Err(ProcessError::Usage(message)) if message.contains(NAME)
            ));
        }
        let overflow = format!("{}0", usize::MAX);
        assert!(matches!(
            package_graph_limit(NAME, Ok(overflow), 17),
            Err(ProcessError::Usage(_))
        ));
        assert!(matches!(
            package_graph_limit(
                NAME,
                Err(std::env::VarError::NotUnicode(std::ffi::OsString::from(
                    "invalid OS value"
                ))),
                17
            ),
            Err(ProcessError::Usage(_))
        ));
    }

    #[test]
    fn configuration_derives_workspace_and_endpoint() {
        let result = ProcessConfig::parse(std::iter::empty());
        assert!(result.is_ok(), "zero-configuration locald: {result:?}");
        let Ok(result) = result else { return };
        assert_eq!(result.registry.sources.len(), 7);
        assert!(result.registry.endpoint.is_none());
        assert_eq!(result.registry.cache_max_age_millis, None);
        assert_eq!(result.discovery.sources.len(), 7);
        assert!(!result.discovery.offline);
        assert_eq!(result.discovery.max_pages, 64);
        assert!(
            result
                .discovery
                .sources
                .iter()
                .all(|source| source.as_str().starts_with("https://"))
        );
        assert_eq!(result.forge.policy, ForgeAcquisitionPolicy::Online);
        assert_eq!(result.forge.limits, ForgeAcquisitionLimits::default());
    }

    #[test]
    fn saved_local_policy_closes_registry_and_advisory_network_paths_and_cache_reuse() {
        let mut config = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-policy.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-policy".to_owned(),
        ])
        .expect("desktop owner configuration");
        config.apply_registry_user_policy(RegistryUserPolicy {
            allow_remote_metadata: false,
            refresh_advisories: false,
            cache_enabled: false,
            cache_max_age_days: 0,
        });

        assert_eq!(config.registry.policy, AcquisitionPolicy::Offline);
        assert!(
            config
                .registry
                .sources
                .sources()
                .all(|source| { source.policy() == AcquisitionPolicy::Offline })
        );
        assert!(config.discovery.offline);
        assert!(config.advisory.offline);
        assert!(!config.advisory.refresh_enabled);
        assert_eq!(config.registry.cache_max_age_millis, Some(0));
    }

    #[test]
    fn saved_preferences_cannot_reopen_disabled_paths_or_widen_cache_age() {
        let mut config = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-policy-offline.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-policy-offline".to_owned(),
            "--registry-offline".to_owned(),
            "--advisory-offline".to_owned(),
        ])
        .expect("offline owner configuration");
        config.advisory.refresh_enabled = false;
        let tighter_cache_age = 7 * RegistryUserPolicy::MILLIS_PER_DAY;
        config.registry.cache_max_age_millis = Some(tighter_cache_age);
        config.apply_registry_user_policy(RegistryUserPolicy {
            allow_remote_metadata: true,
            refresh_advisories: true,
            cache_enabled: true,
            cache_max_age_days: 365,
        });

        assert_eq!(config.registry.policy, AcquisitionPolicy::Offline);
        assert!(config.discovery.offline);
        assert!(
            config
                .registry
                .sources
                .sources()
                .all(|source| { source.policy() == AcquisitionPolicy::Offline })
        );
        assert!(config.advisory.offline);
        assert!(!config.advisory.refresh_enabled);
        assert_eq!(
            config.registry.cache_max_age_millis,
            Some(tighter_cache_age)
        );

        config.registry.cache_max_age_millis = Some(0);
        config.apply_registry_user_policy(RegistryUserPolicy {
            allow_remote_metadata: true,
            refresh_advisories: true,
            cache_enabled: true,
            cache_max_age_days: 30,
        });
        assert_eq!(config.registry.cache_max_age_millis, Some(0));
    }

    #[test]
    fn discovery_sources_are_overridden_explicitly_and_offline_is_network_free() {
        let explicit = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-discovery-source.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-discovery-source".to_owned(),
            "--registry-discovery-source".to_owned(),
            "cargo=http://127.0.0.1:43121".to_owned(),
            "--registry-discovery-offline".to_owned(),
        ])
        .expect("parse local fixture without starting a network worker");
        assert_eq!(explicit.discovery.sources.len(), 1);
        assert_eq!(
            explicit.discovery.sources[0].as_str(),
            "http://127.0.0.1:43121"
        );
        assert!(explicit.discovery.offline);
        assert!(explicit.forge.authentication.default.is_none());
        assert!(explicit.forge.authentication.providers.is_empty());
    }

    #[test]
    fn forge_policy_limits_and_provider_credentials_are_process_local() {
        // Not the thread name: it is the test's path, and `::` is not a valid
        // file name on Windows.
        let token_path = std::env::temp_dir().join(format!(
            "backend-locald-forge-auth-{}-process-local.token",
            std::process::id(),
        ));
        fs::write(&token_path, "forge-secret\n").expect("write scoped forge credential");
        let parsed = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-forge-config.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-forge-config".to_owned(),
            "--forge-offline".to_owned(),
            "--forge-auth-file-for".to_owned(),
            format!("codeberg={}", token_path.display()),
            "--forge-max-entries".to_owned(),
            "64".to_owned(),
            "--forge-max-tree-bytes".to_owned(),
            "1048576".to_owned(),
        ])
        .expect("parse explicit forge configuration");
        let _ = fs::remove_file(&token_path);
        assert_eq!(parsed.forge.policy, ForgeAcquisitionPolicy::Offline);
        assert_eq!(parsed.forge.limits.archive_budget.max_entries, 64);
        assert_eq!(parsed.forge.limits.archive_budget.max_bytes, 1_048_576);
        assert!(
            parsed
                .forge
                .authentication
                .for_provider(ForgeProvider::Github)
                .is_none()
        );
        assert_eq!(
            parsed
                .forge
                .authentication
                .for_provider(ForgeProvider::Codeberg),
            Some(ForgeAuthToken::new("forge-secret").expect("test token"))
        );
        assert!(
            !format!("{:?}", parsed.forge).contains("forge-secret"),
            "Debug must redact process-local forge credentials"
        );
    }

    #[test]
    fn forge_auth_files_are_bounded_and_plaintext_argv_is_not_supported() {
        let token_path = std::env::temp_dir().join(format!(
            "backend-locald-forge-auth-large-{}.token",
            std::process::id()
        ));
        fs::write(&token_path, vec![b'x'; FORGE_MAX_AUTH_BYTES + 1])
            .expect("write oversized forge credential fixture");
        let result = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-forge-auth-large.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-forge-auth-large".to_owned(),
            "--forge-auth-file".to_owned(),
            token_path.to_string_lossy().into_owned(),
        ]);
        let _ = fs::remove_file(&token_path);
        assert!(matches!(result, Err(ProcessError::Usage(_))));

        let result = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-forge-auth-argv.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-forge-auth-argv".to_owned(),
            "--forge-auth".to_owned(),
            "plaintext-secret".to_owned(),
        ]);
        assert!(matches!(result, Err(ProcessError::Usage(_))));
    }

    #[test]
    fn forge_limits_fail_closed_above_the_acquisition_ceiling() {
        let parsed = ProcessConfig::parse([
            "--endpoint".to_owned(),
            "/tmp/backend-locald-forge-limit.sock".to_owned(),
            "--workspace".to_owned(),
            "/tmp/backend-locald-forge-limit".to_owned(),
            "--forge-max-tree-bytes".to_owned(),
            (ForgeAcquisitionLimits::default().archive_budget.max_bytes + 1).to_string(),
        ]);
        assert!(matches!(parsed, Err(ProcessError::Usage(_))));
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
        assert!(
            result
                .registry
                .sources
                .sources()
                .all(|source| source.policy() == AcquisitionPolicy::Offline)
        );
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
            "--advisory-osv-scope".to_owned(),
            "pypi".to_owned(),
            "--advisory-rustsec".to_owned(),
            "https://example.invalid/rustsec.json".to_owned(),
            "--advisory-offline".to_owned(),
            "--advisory-max-age-secs".to_owned(),
            "42".to_owned(),
        ]);
        assert!(parsed.is_ok(), "advisory composition: {parsed:?}");
        let Ok(parsed) = parsed else { return };
        assert_eq!(parsed.advisory.sources.len(), 2);
        assert_eq!(
            parsed.advisory.osv_scope,
            Some(backend_engine::advisory::OsvFeedScope::Ecosystem(
                backend_engine::advisory::OsvEcosystem::Pypi
            ))
        );
        assert!(parsed.advisory.offline);
        assert_eq!(parsed.advisory.max_age_secs, 42);
        assert_eq!(parsed.advisory.max_feed_bytes, ADVISORY_MAX_FEED_BYTES);

        let invalid_scope = AdvisoryConfig::from_options(
            Some("/tmp/osv.zip".to_owned()),
            Some("conan".to_owned()),
            None,
            None,
            false,
            None,
        );
        assert!(matches!(invalid_scope, Err(ProcessError::Usage(_))));
        let orphan_scope =
            AdvisoryConfig::from_options(None, Some("cargo".to_owned()), None, None, false, None);
        assert!(matches!(orphan_scope, Err(ProcessError::Usage(_))));
    }
}
