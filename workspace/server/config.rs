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

use secrecy::{ExposeSecret, SecretString};
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

	/// This node's role in a horizontally-scaled deployment (`NUDOX_ROLE`).
	/// Governs which background loops run; the HTTP surface (health/metrics)
	/// is always served. Defaults to [`Role::All`] (single-node).
	#[serde(default)]
	pub role: Role,

	/// The deployment tier (`NUDOX_DEPLOYMENT`). When `production`, the boot
	/// guard rejects default/well-known credentials before any network
	/// connection is opened. Defaults to [`Deployment::Development`].
	#[serde(default)]
	pub deployment: Deployment,

	/// The HTTP endpoint of the compiler daemon.
	/// Defaults to `http://127.0.0.1:9090`.
	#[serde(default = "defaults::compiler_endpoint")]
	pub compiler_endpoint: url::Url,

	/// Directory that contains the metadata heuristics data files:
	/// `tag-synonyms.csv`, `specific-keywords.txt`, and `bland-keywords.txt`.
	///
	/// When `Some`, [`registry::metadata::heuristics::Synonyms`] and
	/// [`registry::metadata::heuristics::Specifics`] are loaded **once** at
	/// startup and threaded into every `rich::extract` call, enabling full
	/// keyword normalization. Loading fails **loudly** at startup if the
	/// directory is present but the files cannot be parsed — no silent
	/// half-wired state.
	///
	/// When `None` (the default), heuristics are disabled and the extractor
	/// receives `(None, None)` as today.
	///
	/// Configure via `NUDOX_METADATA_DATA_DIR=/path/to/workspace/data` or the
	/// `metadata_data_dir` key in the TOML config file.
	#[serde(default)]
	pub metadata_data_dir: Option<PathBuf>,

	/// Catalog-mirror configuration: which ecosystems to actively follow and
	/// the backpressure ceiling. Default: no active followers (demand-pull
	/// only).
	#[serde(default)]
	pub mirror: MirrorConfig,
}

/// The deployment tier this node is running in.
///
/// In `Production` the boot guard enforces that default / well-known
/// credentials are rejected before the server opens any network connection.
/// `Development` (the default) relaxes that check so a `cargo run` / `buck2
/// run` with stock localhost services works out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Deployment {
	/// Local-development / CI: default credentials are allowed.
	#[default]
	Development,
	/// Production: default credentials are a hard boot error.
	Production,
}

/// A node's role in the daemon fleet. Every node consumes the same postgres
/// queue and writes the same CAS; a role only selects which background loops
/// this process runs (DAEMON-PLAN §Phase 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
	/// HTTP ingress + derived-store fan-out consumers + index pollers.
	/// Serves reads and keeps search stores fresh; does not compile.
	Gateway,
	/// The compile worker: drains the indexing queue and produces IR/blobs.
	/// Does not run the fan-out consumers or index pollers.
	Forge,
	/// Everything — the default single-node deployment.
	#[default]
	All,
}

impl Role {
	/// Whether this role runs the indexing queue compile worker.
	pub fn runs_forge(self) -> bool {
		matches!(self, Role::Forge | Role::All)
	}

	/// Whether this role runs the fan-out consumers + replica-local index
	/// sync/watermark pollers (the read-serving/materialization loops).
	pub fn runs_gateway(self) -> bool {
		matches!(self, Role::Gateway | Role::All)
	}
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

/// Mirror-catalog follower configuration. When `follow` is non-empty, the
/// gateway spawns one `catalog_follower_worker` per language. Demand-pull (the
/// existing on-request resolve path) remains the default when `follow` is empty.
///
/// Configure via `[mirror]` in `nudox.toml`, e.g.:
/// ```toml
/// [mirror]
/// follow = ["csharp", "rust"]
/// ```
/// or via environment variables:
/// ```text
/// NUDOX_MIRROR__FOLLOW='["csharp", "rust"]'
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MirrorConfig {
	/// Languages whose upstream catalogs should be actively followed.
	/// Default: `[]` (demand-pull only).
	#[serde(default)]
	pub follow: Vec<smol_str::SmolStr>,

	/// Pause mirror ingestion while the indexing queue depth exceeds this
	/// ceiling (number of pending jobs). Default: 1000. This prevents the
	/// catalog follower from outrunning the compile workers.
	#[serde(default = "defaults::mirror_queue_ceiling")]
	pub queue_ceiling: usize,
}

/// Operational limits that bound resource use and blast radius.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

	/// How long a freshly-claimed job's lease lasts before the reclaimer may
	/// return it to the runnable set. A live worker beats a heartbeat every
	/// `job_lease / 3` (see [`crate::coordination::indexing`]) so it never lapses,
	/// letting this be *short* (~2 min) — a crashed node's job is then reclaimed
	/// in minutes, not the old 15. Additive: absent in config → the 2-min default.
	#[serde(default = "defaults::job_lease")]
	pub job_lease: std::time::Duration,

	/// The end-to-end hard deadline for one indexing job. Bounds a hung producer;
	/// must comfortably exceed a normal compile but stay under an operator's
	/// patience. Independent of `job_lease` now that the lease is heartbeat-kept.
	#[serde(default = "defaults::job_deadline")]
	pub job_deadline: std::time::Duration,

	/// The bound on how long graceful shutdown waits for in-flight jobs to finish
	/// after new dequeues stop, before the pollers are aborted. Keeps a stuck job
	/// from wedging shutdown forever while still letting a nearly-done one commit.
	#[serde(default = "defaults::drain_deadline")]
	pub drain_deadline: std::time::Duration,
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
			role: Role::default(),
			deployment: Deployment::default(),
			metadata_data_dir: None,
			compiler_endpoint: defaults::compiler_endpoint(),
			mirror: MirrorConfig::default(),
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
	///
	/// **Boot guard**: when [`Deployment::Production`] any source whose postgres
	/// URL or TerminusDB password is still the well-known development default is
	/// a hard error — the server refuses to start rather than silently connecting
	/// a production graph store with publicly-known credentials.
	pub fn validate(&self) -> Result<(), ConfigError> {
		let mut names = std::collections::HashSet::new();
		for source in std::iter::once(&self.definitive).chain(&self.overlays) {
			if source.name.trim().is_empty() {
				return Err(ConfigError::Validation(ConfigValidationError::EmptySourceName));
			}
			if !names.insert(source.name.clone()) {
				return Err(ConfigError::Validation(ConfigValidationError::DuplicateSourceName {
					name: source.name.clone(),
				}));
			}

			if self.deployment == Deployment::Production {
				source.endpoints.assert_not_default_credentials()?;
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
			job_lease: defaults::job_lease(),
			job_deadline: defaults::job_deadline(),
			drain_deadline: defaults::drain_deadline(),
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

	/// In production, reject well-known default credentials before any network
	/// connection is opened.
	///
	/// The postgres URL default is `postgres://nudox:nudox@127.0.0.1:5432/nudox`
	/// and the TerminusDB password default is `root` — both are public, so they
	/// must not be used in a production deployment.
	fn assert_not_default_credentials(&self) -> Result<(), ConfigError> {
		const DEFAULT_POSTGRES: &str = "postgres://nudox:nudox@127.0.0.1:5432/nudox";
		const DEFAULT_TERMINUS_PASSWORD: &str = "root";

		if self.postgres.expose_secret() == DEFAULT_POSTGRES {
			return Err(ConfigError::Validation(
				ConfigValidationError::DefaultCredentialInProduction {
					field: "endpoints.postgres",
				},
			));
		}
		if self.terminus_password.expose_secret() == DEFAULT_TERMINUS_PASSWORD {
			return Err(ConfigError::Validation(
				ConfigValidationError::DefaultCredentialInProduction {
					field: "endpoints.terminus_password",
				},
			));
		}
		Ok(())
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
	/// Shortened, heartbeat-kept lease (DAEMON-PLAN §2.5: "~2 min").
	pub(super) fn job_lease() -> std::time::Duration { std::time::Duration::from_secs(120) }
	/// End-to-end job deadline (was the module const `JOB_DEADLINE`).
	pub(super) fn job_deadline() -> std::time::Duration { std::time::Duration::from_secs(10 * 60) }
	/// Graceful-drain bound for in-flight jobs at shutdown.
	pub(super) fn drain_deadline() -> std::time::Duration { std::time::Duration::from_secs(30) }
	pub(super) fn compiler_endpoint() -> url::Url { super::parse_static("http://127.0.0.1:8080") }
	/// Mirror queue ceiling: pause catalog ingestion above this many pending
	/// jobs.
	pub(super) fn mirror_queue_ceiling() -> usize { 1000 }
}

/// Why configuration failed to resolve.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
	/// A layer (file/env) could not be read or parsed.
	#[error("failed to load configuration")]
	Load(#[source] figment::Error),

	/// The merged configuration failed structural validation.
	#[error(transparent)]
	Validation(#[from] ConfigValidationError),
}

/// Detailed, typed configuration validation failures. All data carried
/// explicitly; no dynamic strings for the core reason.
#[derive(Debug, thiserror::Error)]
pub enum ConfigValidationError {
	/// A source name (definitive or overlay) is empty or whitespace-only.
	#[error("a source has an empty name")]
	EmptySourceName,

	/// Two sources (definitive or overlays) share the same name. Identities are
	/// derived from names, so duplicates are fatal.
	#[error("duplicate source name {name:?}: source identities are derived from names")]
	DuplicateSourceName { name: smol_str::SmolStr },

	/// A required endpoint URL (after deserialization) was empty or invalid
	/// for its purpose.
	#[error("missing or invalid required endpoint: {endpoint}")]
	MissingRequiredEndpoint { endpoint: &'static str },

	/// A numeric dimension / limit from config is outside the supported range.
	#[error("dimension out of range for {what}: expected {expected}, found {found}")]
	DimensionOutOfRange {
		what: &'static str,
		expected: String,
		found: String,
	},

	/// A URL field (e.g. embeddings, qdrant, terminus) failed to parse or is
	/// unusable.
	#[error("invalid url for {field}: {url}")]
	InvalidUrl {
		field: &'static str,
		url: String,
		#[source]
		source: Option<url::ParseError>,
	},

	/// Terminus organization name failed validation (carries the rich GraphNameError
	/// with position, length, char details etc.).
	#[error("invalid terminus organization")]
	InvalidTerminusOrganization(registry::runtime::graph::GraphNameError),

	/// Terminus database name failed validation.
	#[error("invalid terminus database")]
	// Same source type as organization; no #[from] so From is not ambiguous.
	InvalidTerminusDatabase(registry::runtime::graph::GraphNameError),

	/// A Qdrant collection name failed validation (carries the rich CollectionNameError).
	#[error("invalid qdrant collection name")]
	InvalidQdrantCollection(#[from] registry::runtime::vector::CollectionNameError),

	/// A well-known default credential is present in a production deployment.
	///
	/// The named `field` still holds its development default value; the server
	/// refuses to start so an operator configuration mistake never silently opens
	/// a production store with publicly-known credentials.
	#[error(
		"production deployment must not use default credentials for `{field}`; \
		 set it to a non-default value via config file or environment variable"
	)]
	DefaultCredentialInProduction { field: &'static str },

	/// Generic other validation problem (use only when no more specific variant
	/// fits; prefer extending the enum).
	#[error("invalid configuration: {detail}")]
	Other { detail: String },
}

/// Serde adapter so a secret connection URL round-trips as a plain string in
/// config without ever being `Debug`-printed in the clear.
mod secret_url {
	use secrecy::{ExposeSecret, SecretString};
	use serde::{Deserialize, Deserializer, Serializer};

	pub(crate) fn serialize<S: Serializer>(value: &SecretString, s: S) -> Result<S::Ok, S::Error> {
		s.serialize_str(value.expose_secret())
	}

	pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SecretString, D::Error> {
		Ok(SecretString::from(String::deserialize(d)?))
	}
}

/// The `Option<SecretString>` sibling of [`secret_url`].
mod optional_secret {
	use secrecy::{ExposeSecret, SecretString};
	use serde::{Deserialize, Deserializer, Serializer};

	pub(crate) fn serialize<S: Serializer>(
		value: &Option<SecretString>,
		s: S,
	) -> Result<S::Ok, S::Error> {
		match value {
			Some(secret) => s.serialize_some(secret.expose_secret()),
			None => s.serialize_none(),
		}
	}

	pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
		d: D,
	) -> Result<Option<SecretString>, D::Error> {
		Ok(Option::<String>::deserialize(d)?.map(SecretString::from))
	}
}
