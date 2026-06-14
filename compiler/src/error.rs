use std::{io, path::PathBuf, process::ExitStatus};

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use lang_types::Language;
use semver::Version;
use serde_json::json;
use thiserror::Error;
use tokio::task::JoinError;

use crate::core::rust::ParseError;

pub(crate) fn summarize_command_output(bytes: &[u8]) -> String {
	let text = String::from_utf8_lossy(bytes);
	let trimmed = text.trim();
	if trimmed.is_empty() {
		return String::new();
	}

	const LIMIT: usize = 2_000;
	if trimmed.len() <= LIMIT {
		return trimmed.to_owned();
	}

	let mut end = LIMIT;
	while !trimmed.is_char_boundary(end) {
		end -= 1;
	}

	format!("{}...", &trimmed[..end])
}

#[derive(Debug, Error)]
pub enum AppError {
	#[error("{field}: {message}")]
	Configuration { field: &'static str, message: String },

	#[error("failed to determine storage root")]
	StorageRootDiscovery {
		#[source]
		source: io::Error,
	},

	#[error("storage operation failed at `{path}`")]
	Storage {
		path:   PathBuf,
		#[source]
		source: io::Error,
	},

	#[error("package `{id}` is not tracked")]
	PackageNotTracked { id: u64 },

	#[error("language `{language:?}` is not supported by this deployment")]
	UnsupportedLanguage { language: Language },

	#[error("registry lookup failed for `{package}` in `{language:?}`: {message}")]
	RegistryLookup { language: Language, package: String, message: String },

	#[error("embedding pipeline failed: {0}")]
	Embedding(String),

	#[error("blocking task join failed during `{action}`")]
	TaskJoin {
		action: &'static str,
		#[source]
		source: JoinError,
	},

	#[error("internal error: {message}")]
	Internal { message: String },

	#[error("not implemented: {message}")]
	NotImplemented { message: String },

	#[error(transparent)]
	Io(#[from] io::Error),

	#[error(transparent)]
	Anyhow(#[from] anyhow::Error),

	#[error(transparent)]
	Eyre(#[from] color_eyre::Report),

	#[error(transparent)]
	Json(#[from] serde_json::Error),

	#[error(transparent)]
	Package(#[from] PackageError),

	#[error(transparent)]
	Git(#[from] GitError),
}

impl IntoResponse for AppError {
	fn into_response(self) -> Response {
		let status = match self {
			AppError::Configuration { .. }
			| AppError::UnsupportedLanguage { .. }
			| AppError::RegistryLookup { .. } => StatusCode::BAD_REQUEST,
			AppError::PackageNotTracked { .. } => StatusCode::NOT_FOUND,
			AppError::Package(PackageError::VersionNotFound(_)) => StatusCode::NOT_FOUND,
			AppError::NotImplemented { .. } => StatusCode::NOT_IMPLEMENTED,
			_ => StatusCode::INTERNAL_SERVER_ERROR,
		};

		let body = Json(json!({
			"error": self.to_string(),
		}));

		(status, body).into_response()
	}
}

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
	Io(#[from] io::Error),

	#[error("process `{command}` failed with {status}{details}")]
	Process { command: String, status: ExitStatus, details: String },

	#[error("parse error: {0}")]
	Parse(#[from] ParseError),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("feature not implemented")]
	NotImplemented,

	#[error("registry error: {0}")]
	Registry(#[from] RegistryError),

	#[error("metadata error: {0}")]
	Metadata(String),

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

	#[error("failed to open repository at `{path}`: {source}")]
	Open {
		path:   PathBuf,
		#[source]
		source: Box<dyn std::error::Error + Send + Sync>,
	},

	#[error("fetch failed: {0}")]
	Fetch(#[source] Box<dyn std::error::Error + Send + Sync>),

	#[error("fetch/checkout failed: {0}")]
	Checkout(#[source] Box<dyn std::error::Error + Send + Sync>),

	#[error("failed to resolve git reference `{name}`: {source}")]
	Reference {
		name:   String,
		#[source]
		source: Box<dyn std::error::Error + Send + Sync>,
	},

	#[error("failed to build index from tree `{tree}`: {source}")]
	IndexFromTree {
		tree:   String,
		#[source]
		source: Box<dyn std::error::Error + Send + Sync>,
	},

	#[error("failed to obtain checkout options: {0}")]
	CheckoutOptions(#[source] Box<dyn std::error::Error + Send + Sync>),

	#[error("failed to materialize worktree: {0}")]
	Materialize(#[source] Box<dyn std::error::Error + Send + Sync>),

	#[error("failed to open an Arc-backed object database")]
	OpenArcObjects {
		#[source]
		source: io::Error,
	},

	#[error("failed to read tree entry at `{path}`: {source}")]
	TreeLookup {
		path:   String,
		#[source]
		source: Box<dyn std::error::Error + Send + Sync>,
	},

	#[error("invalid utf-8 in blob at `{path}`")]
	BlobEncoding {
		path:   String,
		#[source]
		source: std::str::Utf8Error,
	},

	#[error("failed to parse `{path}` as TOML: {source}")]
	TomlParse {
		path:   String,
		#[source]
		source: toml::de::Error,
	},

	#[error("invalid version `{version}` in `{path}`: {source}")]
	VersionParse {
		path:    String,
		version: String,
		#[source]
		source:  semver::Error,
	},

	#[error("unsupported workspace glob pattern: {pattern}")]
	UnsupportedGlob { pattern: String },
}
