use std::{env, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use url::Url;

use crate::error::{AppError, ConfigError};
use crate::terminusdb::upload::TerminusConfig;

#[derive(Clone, Copy, Debug)]
pub enum VectorDistance {
    Cosine,
    Dot,
    Euclid,
    Manhattan,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
	pub bind_addr:        SocketAddr,
	pub storage_root:     PathBuf,
	pub monitor_interval: Duration,
	pub tracing_filter:   String,
	pub pipeline:         PipelineConfig,
}

#[derive(Clone, Debug)]
pub struct PipelineConfig {
	pub terminus:        Option<TerminusConfig>,
	pub qdrant:          Option<QdrantSettings>,
	pub embedding_model: String,
	pub upload_schema:   bool,
}

#[derive(Clone, Debug)]
pub struct QdrantSettings {
	pub endpoint:          Url,
	pub collection_prefix: String,
	pub vector_size:       u64,
	pub distance:          VectorDistance,
}

impl AppConfig {
	pub fn from_env() -> Result<Self, AppError> {
		let bind_addr = env::var("NUDOX_BIND_ADDR")
			.unwrap_or_else(|_| "0.0.0.0:3000".to_owned())
			.parse()
			.map_err(|source| AppError::Config(ConfigError::AddrParse {
				name: "NUDOX_BIND_ADDR",
				source,
			}))?;

		let storage_root = match env::var_os("NUDOX_DATA_DIR") {
			Some(path) => PathBuf::from(path),
			None => env::current_dir()
				.map_err(|source| AppError::StorageRootDiscovery { source })?
				.join(".nudox-data"),
		};

		let monitor_interval = Duration::from_secs(parse_env_u64("NUDOX_MONITOR_INTERVAL_SECS", 300)?);

		let tracing_filter = env::var("RUST_LOG").unwrap_or_else(|_| "info,nudox=debug".to_owned());

		Ok(Self {
			bind_addr,
			storage_root,
			monitor_interval,
			tracing_filter,
			pipeline: PipelineConfig::from_env()?,
		})
	}
}

impl PipelineConfig {
	fn from_env() -> Result<Self, AppError> {
		Ok(Self {
			terminus:        build_terminus_config()?,
			qdrant:          build_qdrant_settings()?,
			embedding_model: env::var("NUDOX_EMBEDDING_MODEL")
				.unwrap_or_else(|_| "text-embedding-3-small".to_owned()),
			upload_schema:   parse_env_bool("NUDOX_UPLOAD_SCHEMA", false)?,
		})
	}
}

fn build_terminus_config() -> Result<Option<TerminusConfig>, AppError> {
	let endpoint = env::var("NUDOX_TERMINUS_ENDPOINT").ok();
	let user = env::var("NUDOX_TERMINUS_USER").ok();
	let password = env::var("NUDOX_TERMINUS_PASSWORD").ok();
	let org = env::var("NUDOX_TERMINUS_ORG").ok();
	let db = env::var("NUDOX_TERMINUS_DB").ok();

	let any_present =
		endpoint.is_some() || user.is_some() || password.is_some() || org.is_some() || db.is_some();
	if !any_present {
		return Ok(None);
	}

	let endpoint = parse_url("NUDOX_TERMINUS_ENDPOINT", endpoint)?;
	let user = require_env("NUDOX_TERMINUS_USER", user)?;
	let password = require_env("NUDOX_TERMINUS_PASSWORD", password)?;
	let org = require_env("NUDOX_TERMINUS_ORG", org)?;
	let db = require_env("NUDOX_TERMINUS_DB", db)?;

	Ok(Some(TerminusConfig { endpoint, user, password, org, db }))
}

fn build_qdrant_settings() -> Result<Option<QdrantSettings>, AppError> {
	let endpoint = env::var("NUDOX_QDRANT_ENDPOINT").ok();
	if endpoint.is_none() {
		return Ok(None);
	}

	let endpoint = parse_url("NUDOX_QDRANT_ENDPOINT", endpoint)?;
	let collection_prefix =
		env::var("NUDOX_QDRANT_COLLECTION_PREFIX").unwrap_or_else(|_| "nudox".to_owned());
	let vector_size = parse_env_u64("NUDOX_QDRANT_VECTOR_SIZE", 1_536)?;
	let distance =
		parse_distance(&env::var("NUDOX_QDRANT_DISTANCE").unwrap_or_else(|_| "cosine".to_owned()))?;

	Ok(Some(QdrantSettings { endpoint, collection_prefix, vector_size, distance }))
}

fn parse_url(field: &'static str, value: Option<String>) -> Result<Url, AppError> {
	let value = require_env(field, value)?;
	Url::parse(&value)
		.map_err(|source| AppError::Config(ConfigError::InvalidUrl { name: field, source }))
}

fn require_env(field: &'static str, value: Option<String>) -> Result<String, AppError> {
	value.ok_or(AppError::Config(ConfigError::MissingEnv { name: field }))
}

fn parse_env_u64(field: &'static str, default: u64) -> Result<u64, AppError> {
	match env::var(field) {
		Ok(value) => value.parse::<u64>().map_err(|source| AppError::Config(ConfigError::ParseInt {
			name: field,
			source,
		})),
		Err(env::VarError::NotPresent) => Ok(default),
		Err(env::VarError::NotUnicode(_)) => {
			Err(AppError::Config(ConfigError::NotUnicode { name: field }))
		}
	}
}

fn parse_env_bool(field: &'static str, default: bool) -> Result<bool, AppError> {
	match env::var(field) {
		Ok(value) => bool::from_str(&value).map_err(|source| AppError::Config(ConfigError::ParseBool {
			name: field,
			source,
		})),
		Err(env::VarError::NotPresent) => Ok(default),
		Err(env::VarError::NotUnicode(_)) => {
			Err(AppError::Config(ConfigError::NotUnicode { name: field }))
		}
	}
}

fn parse_distance(value: &str) -> Result<VectorDistance, AppError> {
	match value.trim().to_ascii_lowercase().as_str() {
		"cosine" => Ok(VectorDistance::Cosine),
		"dot" => Ok(VectorDistance::Dot),
		"euclid" | "euclidean" => Ok(VectorDistance::Euclid),
		"manhattan" => Ok(VectorDistance::Manhattan),
		other => Err(AppError::Config(ConfigError::InvalidValue {
			name:  "NUDOX_QDRANT_DISTANCE",
			value: other.to_owned(),
		})),
	}
}
