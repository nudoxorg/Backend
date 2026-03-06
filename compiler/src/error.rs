use std::process::ExitStatus;

use semver::Version;
use thiserror::Error;

use crate::core::rust::ParseError;

/// Errors arising from package registry interactions (network, lookup, API).
#[derive(Debug, Error)]
#[allow(dead_code)]
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
#[allow(dead_code)]
pub enum PackageError {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("process `{command}` failed with {status}")]
	Process { command: String, status: ExitStatus },

	#[error("parse error: {0}")]
	Parse(#[from] ParseError),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("feature not implemented")]
	NotImplemented,

	#[error("registry error: {0}")]
	Registry(#[from] RegistryError),

	#[error("version {0} not found")]
	VersionNotFound(Version),

	#[error("git error: {0}")]
	Git(#[from] GitError),
}

/// Errors arising from git operations (clone, checkout, version lookup).
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum GitError {
	#[error("clone failed: {0}")]
	Clone(#[source] Box<dyn std::error::Error + Send + Sync>),

	#[error("fetch/checkout failed: {0}")]
	Checkout(#[source] Box<dyn std::error::Error + Send + Sync>),

	#[error("failed to read tree entry at `{path}`: {source}")]
	TreeLookup { path: String, #[source] source: Box<dyn std::error::Error + Send + Sync> },

	#[error("invalid utf-8 in blob at `{path}`")]
	BlobEncoding { path: String, #[source] source: std::str::Utf8Error },

	#[error("failed to parse `{path}` as TOML: {source}")]
	TomlParse { path: String, #[source] source: toml::de::Error },

	#[error("invalid version `{version}` in `{path}`: {source}")]
	VersionParse { path: String, version: String, #[source] source: semver::Error },

	#[error("unsupported workspace glob pattern: {pattern}")]
	UnsupportedGlob { pattern: String },
}
