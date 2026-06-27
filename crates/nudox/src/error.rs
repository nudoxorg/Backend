use std::{io, path::PathBuf, process::ExitStatus};

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use lang_types::Language;
use semver::Version;
use serde_json::json;
use thiserror::Error;
use tokio::task::JoinError;

use crate::core::{rust::ParseError, ts::TsPackageError};

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

// ── Tantivy text-index errors ────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum TextIndexError {
	#[error("mmap directory at `{path}`")]
	MmapDirectory {
		path:   PathBuf,
		#[source]
		source: tantivy::directory::error::OpenDirectoryError,
	},

	#[error("open or create tantivy index")]
	OpenOrCreate {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("create index writer")]
	Writer {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("writer lock poisoned")]
	LockPoisoned,

	#[error("add document")]
	AddDocument {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("commit")]
	Commit {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("create reader")]
	Reader {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("parse query `{query}`")]
	ParseQuery {
		query:  String,
		#[source]
		source: tantivy::query::QueryParserError,
	},

	#[error("execute search")]
	Search {
		#[source]
		source: tantivy::TantivyError,
	},

	#[error("fetch document")]
	DocFetch {
		#[source]
		source: tantivy::TantivyError,
	},
}

// ── Embedding pipeline errors ────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum EmbeddingError {
	#[error("missing text for embedding")]
	MissingText,

	#[error("missing generated vector")]
	MissingVector,

	#[error("missing required field: {field}")]
	MissingField { field: &'static str },

	#[error("invalid field type: {field}")]
	InvalidFieldType { field: &'static str },

	#[error("embedding task join failed")]
	TaskJoin {
		#[source]
		source: JoinError,
	},

	#[error("embedding provider request failed")]
	Provider {
		#[source]
		source: nudox_core::Error,
	},

	#[error("embedding provider returned an empty response")]
	EmptyResponse,
}

// ── Ingest-pipeline errors ───────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum IngestError {
	#[error("IR generation failed for `{package}`")]
	IrGeneration {
		package: String,
		#[source]
		source:  PackageError,
	},

	#[error("IR generation failed for `{package}`")]
	TsIrGeneration {
		package: String,
		#[source]
		source:  TsPackageError,
	},

	#[error("occurrence store register_library failed for `{package}`")]
	StoreRegister {
		package: String,
		#[source]
		source:  nudox_core::Error,
	},

	#[error("TsPackage resolution failed")]
	TsPackageResolution {
		#[source]
		source: reqwest::Error,
	},

	#[error("pipeline error: {details}")]
	Pipeline { details: String },
}

// ── Qdrant errors ────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum QdrantError {
	#[error("failed to connect to Qdrant at `{endpoint}`")]
	ConnectionFailed {
		endpoint: String,
		#[source]
		source:   qdrant_client::QdrantError,
	},

	#[error("query failed on collection `{collection}`")]
	QueryFailed {
		collection: String,
		#[source]
		source:     qdrant_client::QdrantError,
	},

	#[error("failed to list collections with prefix `{prefix}`")]
	ListCollectionsFailed {
		prefix: String,
		#[source]
		source: qdrant_client::QdrantError,
	},

	#[error("upsert points to collection `{collection}` failed")]
	UpsertFailed {
		collection: String,
		#[source]
		source:     qdrant_client::QdrantError,
	},

	#[error("collection `{collection}` existence check failed")]
	CollectionExistenceCheck {
		collection: String,
		#[source]
		source:     qdrant_client::QdrantError,
	},

	#[error("collection `{collection}` creation failed")]
	CollectionCreation {
		collection: String,
		#[source]
		source:     qdrant_client::QdrantError,
	},

	#[error("deterministic point-id generation failed: {details}")]
	PointIdGeneration { details: String },
}

// ── TerminusDB errors ────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum TerminusError {
	#[error("failed to create TerminusDB client for `{org}/{db}`")]
	ClientCreation {
		org:    String,
		db:     String,
		#[source]
		source: anyhow::Error,
	},

	#[error("failed to upload schema to TerminusDB")]
	SchemaUpload {
		#[source]
		source: anyhow::Error,
	},

	#[error("failed to upload documents to TerminusDB")]
	DocumentUpload {
		#[source]
		source: anyhow::Error,
	},

	#[error("failed to fetch document `{uri}` from TerminusDB")]
	DocumentFetch {
		uri:    String,
		#[source]
		source: anyhow::Error,
	},

	#[error("failed to build TerminusDB HTTP client")]
	HttpClient {
		#[source]
		source: anyhow::Error,
	},
}

// ── Configuration errors (→ 400 BAD_REQUEST) ─────────────────────────────────

#[derive(Debug, Error)]
pub enum ConfigError {
	#[error("`{name}`: invalid socket address: {source}")]
	AddrParse {
		name:   &'static str,
		#[source]
		source: std::net::AddrParseError,
	},

	#[error("`{name}`: invalid URL: {source}")]
	InvalidUrl {
		name:   &'static str,
		#[source]
		source: url::ParseError,
	},

	#[error("`{name}`: missing required environment variable")]
	MissingEnv { name: &'static str },

	#[error("`{name}`: invalid integer: {source}")]
	ParseInt {
		name:   &'static str,
		#[source]
		source: std::num::ParseIntError,
	},

	#[error("`{name}`: invalid bool: {source}")]
	ParseBool {
		name:   &'static str,
		#[source]
		source: std::str::ParseBoolError,
	},

	#[error("`{name}`: value is not valid UTF-8")]
	NotUnicode { name: &'static str },

	#[error("`{name}`: unsupported value `{value}`")]
	InvalidValue { name: &'static str, value: String },

	#[error("`{name}` must not be empty")]
	EmptyValue { name: &'static str },

	#[error("Qdrant vector search is not configured")]
	MissingQdrant,

	#[error("TerminusDB graph store is not configured")]
	MissingTerminus,

	#[error("provide either ?uri=<symbol-uri> or ?symbol=<fq_name>&language=<lang>")]
	MissingQueryParams,

	#[error("session value must not be empty")]
	EmptySession,
}

// ── Registry-lookup errors (→ 400 BAD_REQUEST) ────────────────────────────────

#[derive(Debug, Error)]
pub enum RegistryLookupError {
	#[error("crates.io API error for `{package}`: {source}")]
	CratesIo {
		language: Language,
		package:  String,
		#[source]
		source:   crates_io_api::Error,
	},

	#[error("npm registry error for `{package}`: {source}")]
	Npm {
		language: Language,
		package:  String,
		#[source]
		source:   TsPackageError,
	},

	#[error("package `{package}` not found")]
	NotFound { language: Language, package: String },

	#[error("failed to resolve path `{path}` for `{package}`: {source}")]
	PathResolution {
		language: Language,
		package:  String,
		path:     String,
		#[source]
		source:   io::Error,
	},

	#[error("`{path}` is not a valid repository path or URL for `{package}`")]
	InvalidRepositoryPath { language: Language, package: String, path: String },
}

// ── Top-level application error ──────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum AppError {
	// === Configuration (→ 400 BAD_REQUEST) ===
	#[error(transparent)]
	Config(#[from] ConfigError),

	#[error("language `{language:?}` is not supported by this deployment")]
	UnsupportedLanguage { language: Language },

	#[error(transparent)]
	RegistryLookup(#[from] RegistryLookupError),

	// === Storage (→ 500) ===
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

	// === Package tracking (→ 404) ===
	#[error("package `{id}` is not tracked")]
	PackageNotTracked { id: u64 },

	// === Concurrency (→ 500) ===
	#[error("blocking task join failed during `{action}`")]
	TaskJoin {
		action: &'static str,
		#[source]
		source: JoinError,
	},

	// === Not implemented (→ 501) ===
	#[error("not implemented: {message}")]
	NotImplemented { message: String },

	// === Transparent wrappers ===
	#[error(transparent)]
	Io(#[from] io::Error),

	#[error(transparent)]
	Json(#[from] serde_json::Error),

	#[error(transparent)]
	Package(#[from] PackageError),

	#[error(transparent)]
	Git(#[from] GitError),

	// === Domain errors ===
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

	// === Search / API errors ===
	#[error("text search index is not available on this server")]
	TextSearchNotConfigured,

	#[error("symbol search is not configured on this server")]
	SymbolSearchNotConfigured,

	#[error("symbol `{uri}` was not found in TerminusDB")]
	SymbolNotFound { uri: String },

	// === Scheduler / registry errors ===
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
			// ── 400 Bad Request ─────────────────────────────────────────────
			AppError::Config(_) | AppError::UnsupportedLanguage { .. } | AppError::RegistryLookup(_) => {
				StatusCode::BAD_REQUEST
			}

			// ── 404 Not Found ──────────────────────────────────────────────
			AppError::PackageNotTracked { .. } => StatusCode::NOT_FOUND,
			AppError::Package(PackageError::VersionNotFound(_)) => StatusCode::NOT_FOUND,
			AppError::SymbolNotFound { .. } => StatusCode::NOT_FOUND,

			// ── 501 Not Implemented ────────────────────────────────────────
			AppError::NotImplemented { .. } => StatusCode::NOT_IMPLEMENTED,

			// ── Everything else → 500 Internal Server Error ────────────────
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
pub enum RegistryError {
	#[error("crates.io API error: {0}")]
	CratesIo(#[from] crates_io_api::Error),
}

/// Errors arising from package-level operations (doc generation, parsing, IO).
#[derive(Debug, Error)]
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
pub enum GitError {
	#[error("clone from `{url}` failed: {source}")]
	Clone {
		url:     String,
		#[source]
		source:  gix::clone::Error,
	},

	#[error("failed to open repository at `{path}`: {source}")]
	Open {
		path:   PathBuf,
		#[source]
		source: gix::open::Error,
	},

	#[error("IO error at `{path}`: {source}")]
	Io {
		path:   PathBuf,
		#[source]
		source: io::Error,
	},

	#[error("failed to find git remote: {source}")]
	FindRemote {
		#[source]
		source: gix::remote::find::for_fetch::Error,
	},

	#[error("failed to connect to remote: {source}")]
	Connect {
		#[source]
		source: gix::remote::connect::Error,
	},

	#[error("failed to prepare fetch: {source}")]
	PrepareFetch {
		#[source]
		source: gix::remote::fetch::prepare::Error,
	},

	#[error("failed to receive fetch: {source}")]
	ReceiveFetch {
		#[source]
		source: gix::remote::fetch::Error,
	},

	#[error("fetch/checkout failed: {source}")]
	FetchCheckout {
		#[source]
		source: gix::clone::fetch::Error,
	},

	#[error("worktree checkout failed: {source}")]
	WorktreeCheckout {
		#[source]
		source: gix::clone::checkout::main_worktree::Error,
	},

	#[error("failed to resolve git reference `{name}`: {source}")]
	Reference {
		name:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	#[error("failed to decode commit tree id for `{name}`: {source}")]
	CommitDecode {
		name:   String,
		#[source]
		source: gix_object::decode::Error,
	},

	#[error("failed to build index from tree `{tree}`: {source}")]
	IndexFromTree {
		tree:   String,
		#[source]
		source: gix::repository::index_from_tree::Error,
	},

	#[error("failed to obtain checkout options: {0}")]
	CheckoutOptions(#[source] gix::config::checkout_options::Error),

	#[error("failed to materialize worktree: {0}")]
	Materialize(#[source] gix_worktree_state::checkout::Error),

	#[error("failed to open Arc-backed object database: {source}")]
	OpenArcObjects {
		#[source]
		source: io::Error,
	},

	#[error("failed to find tree entry at `{path}`: {source}")]
	TreeLookupEntry {
		path:   String,
		#[source]
		source: gix::object::find::existing::Error,
	},

	#[error("failed to find blob at `{path}`: {source}")]
	FindBlob {
		path:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	#[error("failed to find tree at `{path}`: {source}")]
	FindTree {
		path:   String,
		#[source]
		source: gix::object::find::existing::with_conversion::Error,
	},

	#[error("failed to traverse tree: {source}")]
	TreeTraverse {
		#[source]
		source: gix::diff::object::decode::Error,
	},

	#[error("failed to lookup git HEAD: {source}")]
	Head {
		#[source]
		source: gix::reference::find::existing::Error,
	},

	#[error("failed to find git object: {source}")]
	ObjectLookup {
		#[source]
		source: gix::head::peel::to_object::Error,
	},

	#[error("failed to enumerate git references: {source}")]
	ReferencesOpen {
		#[source]
		source: gix::reference::iter::Error,
	},

	#[error("failed to iterate all git references: {source}")]
	ReferencesAll {
		#[source]
		source: gix::reference::iter::init::Error,
	},

	#[error("failed to peel git references: {source}")]
	ReferencesPeeled {
		#[source]
		source: gix_ref::packed::buffer::open::Error,
	},

	#[error("invalid utf-8 in blob at `{path}`: {source}")]
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
}
