use semver::Version;
use thiserror::Error;

/// Errors arising from package registry interactions (network, lookup, API).
#[derive(Debug, Error)]
pub enum RegistryError {
	#[error("network error: {0}")]
	Network(String),

	#[error("package not found: {0}")]
	NotFound(String),

	#[error("crates.io API error: {0}")]
	CratesIo(#[from] crates_io_api::Error),

	#[error("invalid configuration")]
	InvalidConfiguration,
}

/// Errors arising from package-level operations (doc generation, parsing, IO).
#[derive(Debug, Error)]
pub enum PackageError {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("process error: {0}")]
	Process(String),

	#[error("parse error: {0}")]
	Parse(String),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("feature not implemented")]
	NotImplemented,

	#[error("registry error: {0}")]
	Registry(#[from] RegistryError),

	#[error("registry error: {0}")]
	VersionNotFound(Version),
}
