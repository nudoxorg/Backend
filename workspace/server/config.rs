//! Server configuration.
//!
//! Two fixes over the original design:
//! - endpoints are a **typed struct** ([`Endpoints`]), not a
//!   `HashMap<String, Url>` — a typo can no longer silently return `None`, and
//!   "not configured" is distinct from "misspelled";
//! - configuration is **layered** (defaults ← file ← environment) via `figment`
//!   rather than only constructible from `default()`, so prod/dev differ
//!   without a recompile.

use std::net::SocketAddr;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use url::Url;

/// The fully-resolved, typed configuration a [`crate::Server`] is built from.
///
/// The federation is configured as exactly one [`definitive`](Self::definitive)
/// source plus zero or more [`overlays`](Self::overlays), mirroring the runtime
/// [`heart::Federation`] topology (one base, ordered overlays).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfiguration {
	/// The address the HTTP surface binds to.
	pub serving_address: SocketAddr,

	/// The definitive (centrally-hosted) registry this server fronts.
	pub definitive: SourceConfig,

	/// Self-hosted overlay registries, in precedence order (highest first). Each
	/// extends and overrides the definitive base.
	#[serde(default)]
	pub overlays: Vec<SourceConfig>,

	/// Operational limits (timeouts, body sizes, concurrency).
	pub limits: Limits,
}

/// The configuration of a single federated source: a stable name and its full
/// set of backing-store endpoints. The [`heart::SourceId`] is derived
/// deterministically from the name at assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
	/// A stable, human-readable name (also the seed for the source's id).
	pub name: SmolStr,

	/// This source's backing-store endpoints.
	pub endpoints: Endpoints,
}

/// The typed set of backing-store endpoints. Each is named and required; there
/// is no stringly-typed lookup that can miss.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoints {
	/// The TerminusDB graph store.
	pub terminus: Url,
	/// The Qdrant vector store (gRPC).
	pub qdrant: Url,
	/// The Postgres global index (connection URL; secret).
	#[serde(with = "secret_url")]
	pub postgres: SecretString,
	/// The object-store base (S3/GCS/local) for content-addressed blobs.
	pub object_store: Url,
}

/// Operational limits that bound resource use and blast radius.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Limits {
	/// How long to wait on a single document upload before giving up.
	pub upload_timeout: std::time::Duration,
	/// The largest request body accepted on the ingest/admin plane.
	pub max_request_bytes: u64,
	/// The maximum number of in-flight indexing jobs this replica drives.
	pub max_inflight_jobs: usize,
}

impl Default for ServerConfiguration {
	/// Local-development defaults: a single definitive source on localhost and no
	/// overlays. Production overrides via file/env layers.
	fn default() -> Self {
		Self {
			serving_address: SocketAddr::from(([127, 0, 0, 1], 8080)),
			definitive: SourceConfig {
				name: SmolStr::new_static("definitive"),
				endpoints: Endpoints::localhost_defaults(),
			},
			overlays: Vec::new(),
			limits: Limits::default(),
		}
	}
}

impl SourceConfig {
	/// The deterministic [`heart::SourceId`] for this source, derived from its
	/// name so the same configured source keeps a stable identity.
	pub fn source_id(&self) -> heart::SourceId {
		heart::Id::from_name(&heart::access::source::NAMESPACE, self.name.as_bytes())
	}
}

impl ServerConfiguration {
	/// Resolve configuration by layering: built-in defaults, then an optional
	/// TOML file, then environment variables (`NUDOX_*`). The last layer wins.
	pub fn resolve() -> Result<Self, ConfigError> {
		todo!("figment: Serialized(default) <- Toml(file) <- Env::prefixed(NUDOX_)")
	}
}

impl Default for Limits {
	fn default() -> Self {
		Self {
			upload_timeout: std::time::Duration::from_secs(2),
			max_request_bytes: 256 * 1024 * 1024,
			max_inflight_jobs: 16,
		}
	}
}

impl Endpoints {
	/// The all-localhost endpoint set used for development.
	pub fn localhost_defaults() -> Self { todo!("parse the canonical localhost URLs for each store") }
}

/// Why configuration failed to resolve.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
	/// A layer (file/env) could not be read or parsed.
	#[error("failed to load configuration")]
	Load(#[source] Box<dyn std::error::Error + Send + Sync>),
	/// The merged configuration was structurally invalid.
	#[error("invalid configuration: {0}")]
	Invalid(String),
}

/// Serde adapter so a secret connection URL round-trips as a plain string in
/// config without ever being `Debug`-printed in the clear.
mod secret_url {
	use secrecy::{ExposeSecret, SecretString};
	use serde::{Deserialize, Deserializer, Serializer};

	pub fn serialize<S: Serializer>(value: &SecretString, s: S) -> Result<S::Ok, S::Error> {
		s.serialize_str(value.expose_secret())
	}

	pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SecretString, D::Error> {
		Ok(SecretString::from(String::deserialize(d)?))
	}
}
