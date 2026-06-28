mod config;
mod embedding;
mod git;
mod ingest;
mod package;
mod qdrant;
mod registry;
mod registry_lookup;
mod terminus;
mod text_index;

use std::{io, path::PathBuf};

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
pub use config::ConfigError;
pub use embedding::EmbeddingError;
pub use git::GitError;
pub use ingest::IngestError;
use lang_types::Language;
pub use package::PackageError;
pub use qdrant::QdrantError;
pub use registry::RegistryError;
pub use registry_lookup::RegistryLookupError;
use semver::Version;
use serde_json::json;
pub use terminus::TerminusError;
pub use text_index::TextIndexError;
use thiserror::Error;
use tokio::task::JoinError;

#[derive(Debug, Error)]
pub enum AppError {
	#[error(transparent)]
	Config(#[from] ConfigError),

	#[error("language `{language:?}` is not supported by this deployment")]
	UnsupportedLanguage { language: Language },

	#[error(transparent)]
	RegistryLookup(#[from] RegistryLookupError),

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

	#[error("blocking task join failed during `{action}`")]
	TaskJoin {
		action: &'static str,
		#[source]
		source: JoinError,
	},

	#[error("not implemented: {message}")]
	NotImplemented { message: String },

	#[error(transparent)]
	Io(#[from] io::Error),

	#[error(transparent)]
	Json(#[from] serde_json::Error),

	#[error(transparent)]
	Package(#[from] PackageError),

	#[error(transparent)]
	Git(#[from] GitError),

	#[error(transparent)]
	Ingest(#[from] IngestError),

	#[error(transparent)]
	Backends(#[from] producers::backends::Error),

	#[error(transparent)]
	TextIndex(#[from] TextIndexError),

	#[error(transparent)]
	Embedding(#[from] EmbeddingError),

	#[error(transparent)]
	Qdrant(#[from] QdrantError),

	#[error(transparent)]
	Terminus(#[from] TerminusError),

	#[error("text search index is not available on this server")]
	TextSearchNotConfigured,

	#[error("symbol search is not configured on this server")]
	SymbolSearchNotConfigured,

	#[error("symbol `{uri}` was not found in TerminusDB")]
	SymbolNotFound { uri: String },

	#[error("sync scheduler shut down unexpectedly")]
	SyncShutdown {
		#[source]
		source: tokio::sync::AcquireError,
	},

	#[error("package id counter overflowed")]
	IdExhausted,

	#[error("{kind} source is required but was not provided")]
	MissingSource { kind: &'static str },

	#[error("version {version} not found for package `{package}`")]
	VersionNotFoundForPackage { package: String, version: Version },
}

impl crate::util::retry::Transient for AppError {
	fn is_transient(&self) -> bool { crate::util::retry::is_transient_message(&self.to_string()) }
}

impl IntoResponse for AppError {
	fn into_response(self) -> Response {
		let status = match self {
			AppError::Config(_) | AppError::UnsupportedLanguage { .. } | AppError::RegistryLookup(_) => {
				StatusCode::BAD_REQUEST
			}
			AppError::PackageNotTracked { .. } => StatusCode::NOT_FOUND,
			AppError::Package(PackageError::VersionNotFound(_)) => StatusCode::NOT_FOUND,
			AppError::SymbolNotFound { .. } => StatusCode::NOT_FOUND,
			AppError::NotImplemented { .. } => StatusCode::NOT_IMPLEMENTED,
			_ => StatusCode::INTERNAL_SERVER_ERROR,
		};

		let body = Json(json!({
			"error": self.to_string(),
		}));

		(status, body).into_response()
	}
}
