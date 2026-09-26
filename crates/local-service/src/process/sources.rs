//! Registry source set and advisory authority configuration for locald.
//!
//! Official ecosystem sources are always present. Endpoint, mirror, and
//! credential flags are optional adapters on top of that set. Advisory
//! authorities stay empty until a host names them.

use super::{
    ADVISORY_GHSA_ENV, ADVISORY_MAX_AGE_ENV, ADVISORY_OFFLINE_ENV, ADVISORY_OSV_ENV,
    ADVISORY_POLICY_ENV, ADVISORY_RUSTSEC_ENV, ProcessError, REGISTRY_AUTH_ENV,
    REGISTRY_AUTH_FILE_ENV, REGISTRY_AUTH_SCOPES_ENV, REGISTRY_ECOSYSTEM_ENV,
    REGISTRY_ENDPOINT_ENV, REGISTRY_MAX_ARCHIVE_BYTES_ENV, REGISTRY_NATIVE_ENV,
    REGISTRY_OFFLINE_ENV, REGISTRY_SOURCES_ENV,
};
use crate::listener::ListenerConfig;
use backend_engine::advisory::AdvisorySource;
use backend_engine::registry::{
    AcquisitionLimits, AcquisitionPolicy, AuthenticationToken, RegistryEcosystem, RegistryEndpoint,
    RegistrySource, RegistrySourceSet,
};
use backend_engine::{AcquisitionGate, OfflinePolicy};
use std::fs;
use std::num::NonZeroU8;

/// Default registry archive admission for a single source archive.
///
/// Source sdists routinely exceed the 64 MiB replication object budget, so the
/// registry cap is deliberately independent of that transport limit and can be
/// raised with [`REGISTRY_MAX_ARCHIVE_BYTES_ENV`]. Values above the ceiling are
/// clamped and a deliberate overrun returns a typed acquisition error.
const REGISTRY_MAX_ARCHIVE_DEFAULT_BYTES: usize = 512 * 1024 * 1024;
const REGISTRY_MAX_ARCHIVE_CEILING_BYTES: usize = 1024 * 1024 * 1024;
pub(super) const ADVISORY_MAX_FEED_BYTES: usize = 256 * 1024 * 1024;

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
    pub(super) fn from_options(
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
    pub(super) fn from_options(
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
