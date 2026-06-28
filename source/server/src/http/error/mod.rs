mod text_index;
mod embedding;
mod ingest;
mod qdrant;
mod terminus;
mod config;
mod registry_lookup;
mod registry;
mod package;
mod git;

pub use text_index::TextIndexError;
pub use embedding::EmbeddingError;
pub use ingest::IngestError;
pub use qdrant::QdrantError;
pub use terminus::TerminusError;
pub use config::ConfigError;
pub use registry_lookup::RegistryLookupError;
pub use registry::RegistryError;
pub use package::PackageError;
pub use git::GitError;

use std::io;
use std::path::PathBuf;

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use lang_types::Language;
use semver::Version;
use serde_json::json;
use thiserror::Error;
use tokio::task::JoinError;

use crate::core::ts::TsPackageError;

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
	TsPackage(#[from] TsPackageError),

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

	#[error("could not determine a TypeScript entry point in `{path}`")]
	TypescriptEntryPointDiscovery { path: String },

	#[error("TypeScript entry point `{path}` does not exist")]
	TypescriptEntryPointMissing { path: String },

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
