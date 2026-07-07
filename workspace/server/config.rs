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
use std::path::PathBuf;

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

	/// Named custom registries clients may address packages against (the lookup
	/// table behind an `origin` field on the add-package surface).
	#[serde(default)]
	pub custom_registries: Vec<CustomRegistry>,

	/// Operational limits (timeouts, body sizes, concurrency).
	pub limits: Limits,
}

/// A named, operator-configured registry origin (self-hosted npm proxy,
/// corporate crates mirror, ...). Clients reference it by `name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomRegistry {
	/// The stable name clients pass as the `origin` of an add-package request.
	pub name: SmolStr,

	/// The registry's base URL.
	pub url: Url,
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

	/// Where this source's replica-local state (tantivy indexes, watermarks)
	/// lives. When unset, a per-source directory under the OS temp dir is used —
	/// fine for development, explicit for production.
	#[serde(default)]
	pub data_directory: Option<PathBuf>,
}

/// The typed set of backing-store endpoints. Each is named and required; there
/// is no stringly-typed lookup that can miss.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoints {
	/// The TerminusDB graph store.
	pub terminus: Url,
	/// The TerminusDB organization the graph database lives under.
	#[serde(default = "defaults::terminus_organization")]
	pub terminus_organization: SmolStr,
	/// The TerminusDB database name — also the instance token every
	/// deterministic symbol id is salted with, so it must be stable.
	#[serde(default = "defaults::terminus_database")]
	pub terminus_database: SmolStr,
	/// The TerminusDB basic-auth user.
	#[serde(default = "defaults::terminus_user")]
	pub terminus_user: SmolStr,
	/// The TerminusDB basic-auth password (secret).
	#[serde(default = "defaults::terminus_password", with = "secret_url")]
	pub terminus_password: SecretString,
	/// The Qdrant vector store (gRPC).
	pub qdrant: Url,
	/// The Qdrant collection symbol vectors live in.
	#[serde(default = "defaults::qdrant_collection")]
	pub qdrant_collection: SmolStr,
	/// The Postgres global index (connection URL; secret).
	#[serde(with = "secret_url")]
	pub postgres: SecretString,
	/// The object-store base (S3/GCS/local) for content-addressed blobs.
	pub object_store: Url,
	/// The embedding endpoint (OpenAI-compatible `/v1/embeddings` shape) the
	/// gated semantic path embeds queries against.
	#[serde(default = "defaults::embeddings")]
	pub embeddings: Url,
	/// The bearer token presented to the embedding endpoint, if it wants one.
	#[serde(default, with = "optional_secret")]
	pub embeddings_api_key: Option<SecretString>,
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
	/// How often the background pollers (queue, outbox, index sync) tick.
	#[serde(default = "defaults::poll_interval")]
	pub poll_interval: std::time::Duration,
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
				data_directory: None,
			},
			overlays: Vec::new(),
			custom_registries: Vec::new(),
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

	/// The root of this source's replica-local state: the configured directory,
	/// or a per-source directory under the OS temp dir for development.
	pub fn data_directory(&self) -> PathBuf {
		self.data_directory
			.clone()
			.unwrap_or_else(|| std::env::temp_dir().join("nudox").join(self.name.as_str()))
	}

	/// This source's replica-local tantivy *symbol* index directory.
	pub fn text_index_directory(&self) -> PathBuf { self.data_directory().join("text") }

	/// This source's replica-local tantivy *package* index directory.
	pub fn package_index_directory(&self) -> PathBuf { self.data_directory().join("packages") }
}

impl ServerConfiguration {
	/// Resolve configuration by layering: built-in defaults, then an optional
	/// TOML file (`nudox.toml`, or the path in `NUDOX_CONFIG`), then environment
	/// variables (`NUDOX_*`, `__`-separated nesting). The last layer wins.
	///
	/// The vendored `figment` build carries no `toml`/`env` provider features,
	/// so the file layer is parsed with the `toml` crate and the environment
	/// layer is folded by [`environment_fragment`]; both merge as serialized
	/// fragments — semantically identical to `Toml::file` + `Env::prefixed`.
	pub fn resolve() -> Result<Self, ConfigError> {
		use figment::{Figment, providers::Serialized};

		let mut figment = Figment::from(Serialized::defaults(Self::default()));

		let path = std::env::var_os("NUDOX_CONFIG")
			.map(PathBuf::from)
			.unwrap_or_else(|| PathBuf::from("nudox.toml"));
		if path.is_file() {
			let text = std::fs::read_to_string(&path)
				.map_err(|error| ConfigError::Load(figment::Error::from(error.to_string())))?;
			let fragment: toml::Value = toml::from_str(&text)
				.map_err(|error| ConfigError::Load(figment::Error::from(error.to_string())))?;
			figment = figment.merge(Serialized::defaults(fragment));
		}
		if let Some(fragment) = environment_fragment() {
			figment = figment.merge(Serialized::defaults(fragment));
		}

		let configuration: Self = figment.extract().map_err(ConfigError::Load)?;
		configuration.validate()?;
		Ok(configuration)
	}

	/// The definitive source's terminus endpoint — the common single-source case.
	pub fn terminus_endpoint(&self) -> &Url { &self.definitive.endpoints.terminus }

	/// Structural validation of the merged configuration: source names must be
	/// non-empty and unique (they seed the deterministic source identities).
	pub fn validate(&self) -> Result<(), ConfigError> {
		let mut names = std::collections::HashSet::new();
		for source in std::iter::once(&self.definitive).chain(&self.overlays) {
			if source.name.trim().is_empty() {
				return Err(ConfigError::Invalid("a source has an empty name".into()));
			}
			if !names.insert(source.name.clone()) {
				return Err(ConfigError::Invalid(format!(
					"duplicate source name {:?}: source identities are derived from names",
					source.name
				)));
			}
		}
		Ok(())
	}
}

impl Default for Limits {
	fn default() -> Self {
		Self {
			upload_timeout: std::time::Duration::from_secs(2),
			max_request_bytes: 256 * 1024 * 1024,
			max_inflight_jobs: 16,
			poll_interval: defaults::poll_interval(),
		}
	}
}

impl Endpoints {
	/// The all-localhost endpoint set used for development.
	pub fn localhost_defaults() -> Self {
		Self {
			terminus: parse_static("http://127.0.0.1:6363"),
			terminus_organization: defaults::terminus_organization(),
			terminus_database: defaults::terminus_database(),
			terminus_user: defaults::terminus_user(),
			terminus_password: defaults::terminus_password(),
			qdrant: parse_static("http://127.0.0.1:6334"),
			qdrant_collection: defaults::qdrant_collection(),
			postgres: SecretString::from("postgres://nudox:nudox@127.0.0.1:5432/nudox"),
			object_store: object_store_default(),
			embeddings: defaults::embeddings(),
			embeddings_api_key: None,
		}
	}

	/// The `{org}/{db}` terminus-instance token this endpoint set names — the
	/// salt for every deterministic symbol id minted against this source.
	pub fn terminus_instance(&self) -> String {
		format!("{}/{}", self.terminus_organization, self.terminus_database)
	}
}

/// The `NUDOX_*` environment variables as one nested configuration fragment:
/// `__` separates nesting (`NUDOX_LIMITS__MAX_INFLIGHT_JOBS=4`), keys fold to
/// the lowercase field names, and values parse as JSON scalars where they can
/// (numbers, booleans) with a string fallback — mirroring what figment's own
/// `Env` provider would have produced.
fn environment_fragment() -> Option<serde_json::Value> {
	let mut root = serde_json::Map::new();
	for (key, value) in std::env::vars() {
		let Some(path) = key.strip_prefix("NUDOX_") else { continue };
		// `NUDOX_CONFIG` names the file layer's path; it is not a field.
		if path.eq_ignore_ascii_case("CONFIG") {
			continue;
		}
		let segments: Vec<String> = path.split("__").map(str::to_lowercase).collect();
		let leaf = serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value));
		insert_nested(&mut root, &segments, leaf);
	}
	(!root.is_empty()).then_some(serde_json::Value::Object(root))
}

/// Thread one `a__b__c = leaf` path into the nested fragment.
fn insert_nested(
	node: &mut serde_json::Map<String, serde_json::Value>,
	segments: &[String],
	leaf: serde_json::Value,
) {
	match segments {
		[] => {}
		[last] => {
			node.insert(last.clone(), leaf);
		}
		[head, rest @ ..] => {
			let child = node
				.entry(head.clone())
				.or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
			if let serde_json::Value::Object(map) = child {
				insert_nested(map, rest, leaf);
			}
		}
	}
}

/// Parse a compile-time URL literal (provably infallible).
fn parse_static(url: &'static str) -> Url {
	Url::parse(url).expect("static endpoint literals are valid URLs")
}

/// The development blob store: a `file://` URL under the OS temp dir.
fn object_store_default() -> Url {
	Url::from_file_path(std::env::temp_dir().join("nudox").join("blobs"))
		.expect("the OS temp dir is absolute, so the file URL is well-formed")
}

/// Serde `default =` targets for the optional endpoint/limit fields.
mod defaults {
	use secrecy::SecretString;
	use smol_str::SmolStr;
	use url::Url;

	pub(super) fn terminus_organization() -> SmolStr { SmolStr::new_static("nudox") }
	pub(super) fn terminus_database() -> SmolStr { SmolStr::new_static("registry") }
	pub(super) fn terminus_user() -> SmolStr { SmolStr::new_static("admin") }
	pub(super) fn terminus_password() -> SecretString { SecretString::from("root") }
	pub(super) fn qdrant_collection() -> SmolStr { SmolStr::new_static("symbols") }
	pub(super) fn embeddings() -> Url { super::parse_static("http://127.0.0.1:11434/v1/embeddings") }
	pub(super) fn poll_interval() -> std::time::Duration { std::time::Duration::from_secs(2) }
}

/// Why configuration failed to resolve.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
	/// A layer (file/env) could not be read or parsed.
	#[error("failed to load configuration")]
	Load(#[source] figment::Error),
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

/// The `Option<SecretString>` sibling of [`secret_url`].
mod optional_secret {
	use secrecy::{ExposeSecret, SecretString};
	use serde::{Deserialize, Deserializer, Serializer};

	pub fn serialize<S: Serializer>(
		value: &Option<SecretString>,
		s: S,
	) -> Result<S::Ok, S::Error> {
		match value {
			Some(secret) => s.serialize_some(secret.expose_secret()),
			None => s.serialize_none(),
		}
	}

	pub fn deserialize<'de, D: Deserializer<'de>>(
		d: D,
	) -> Result<Option<SecretString>, D::Error> {
		Ok(Option::<String>::deserialize(d)?.map(SecretString::from))
	}
}
