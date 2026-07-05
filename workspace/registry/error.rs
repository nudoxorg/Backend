//! The single home for every registry error.
//!
//! One `thiserror` enum per logical area, each `#[derive(Debug,
//! thiserror::Error)]`, each with its `#[source]`/`#[from]` chain preserved, and
//! each implementing [`heart::Retryable`] so the queue/sink retry machinery is
//! written once (in heart) and never re-implemented per backend.
//!
//! This module also owns the *rename* of the old `store::StoreError` (which
//! collided with [`heart::StoreError`], a marker trait) — every concrete error
//! type now lives here.

use std::time::Duration;

use heart::{
	BackendKind, ConnectError, FailureKind, PackageId, Retryable,
	content::ContentHash,
};
use thiserror::Error;

/// The top-level registry error — the union every public entry point surfaces.
///
/// Wraps the area-specific errors so a caller can `?` up through the stack and
/// still recover provenance + a retry decision.
#[derive(Debug, Error)]
pub enum RegistryError {
	/// A blob assembly / (de)serialization failure.
	#[error("blob error")]
	Blob(#[from] BlobError),

	/// An object-store / content-addressed store failure.
	#[error("store error")]
	Store(#[from] StoreError),

	/// An untrusted-archive ingest failure.
	#[error("ingest error")]
	Ingest(#[from] IngestError),

	/// A durable job-queue failure.
	#[error("queue error")]
	Queue(#[from] QueueError),

	/// A global-index (postgres) failure.
	#[error("index error")]
	Index(#[from] IndexError),

	/// A transactional-outbox failure.
	#[error("outbox error")]
	Outbox(#[from] OutboxError),

	/// A registry-search failure.
	#[error("search error")]
	Search(#[from] SearchError),

	/// A version-resolution failure.
	#[error("resolve error")]
	Resolve(#[from] ResolveError),

	/// A store/backend failed to come up.
	#[error(transparent)]
	Connect(#[from] ConnectError),

	/// The caller lacks permission for the requested operation.
	#[error("access denied: {reason}")]
	AccessDenied { reason: &'static str },
}

impl Retryable for RegistryError {
	fn is_retryable(&self) -> bool {
		match self {
			RegistryError::Blob(e) => e.is_retryable(),
			RegistryError::Store(e) => e.is_retryable(),
			RegistryError::Ingest(e) => e.is_retryable(),
			RegistryError::Queue(e) => e.is_retryable(),
			RegistryError::Index(e) => e.is_retryable(),
			RegistryError::Outbox(e) => e.is_retryable(),
			RegistryError::Search(e) => e.is_retryable(),
			RegistryError::Resolve(e) => e.is_retryable(),
			RegistryError::Connect(e) => e.is_retryable(),
			RegistryError::AccessDenied { .. } => false,
		}
	}

	fn retry_after(&self) -> Option<Duration> {
		match self {
			RegistryError::Store(e) => e.retry_after(),
			RegistryError::Index(e) => e.retry_after(),
			RegistryError::Connect(e) => e.retry_after(),
			_ => None,
		}
	}
}

/// Failures assembling, hashing, or (de)serializing a [`crate::blob`] manifest
/// or its content-addressed sections.
#[derive(Debug, Error)]
pub enum BlobError {
	/// A file section could not be (de)serialized. `// runs on spawn_blocking`.
	#[error("blob section (de)serialization failed")]
	Codec(#[source] postcard::Error),

	/// A stored section's recomputed BLAKE3 digest did not match its recorded
	/// hash — corruption or tampering.
	#[error("content hash mismatch: manifest declares {expected}, store holds {found}")]
	HashMismatch { expected: String, found: String },

	/// The manifest referenced a section hash the store does not hold.
	#[error("blob references a missing section {0}")]
	MissingSection(String),

	/// The manifest was structurally invalid (empty file set, dangling ir ref).
	#[error("malformed blob manifest: {0}")]
	Malformed(&'static str),

	/// Assembling the manifest touched the object store, which failed.
	#[error("blob store operation failed")]
	Store(#[source] Box<StoreError>),
}

impl Retryable for BlobError {
	fn is_retryable(&self) -> bool {
		match self {
			BlobError::Store(e) => e.is_retryable(),
			// Corruption, structural errors, and codec faults are deterministic.
			_ => false,
		}
	}
}

/// Failures of the content-addressed object [`crate::store`].
///
/// Renamed from the old `store::StoreError` — the name now lives only here.
#[derive(Debug, Error)]
pub enum StoreError {
	/// The underlying object store (S3/GCS/local) returned an error.
	#[error("object store backend failed")]
	Backend(#[from] object_store::Error),

	/// A key could not be encoded safely (e.g. a name that would escape the
	/// `cas/` or pointer layout even after percent-encoding).
	#[error("could not encode object key for {0:?}")]
	KeyEncoding(String),

	/// A blob/section addressed by hash was not present.
	#[error("object {0} not found")]
	NotFound(String),

	/// The bytes read back did not hash to the key they were fetched under.
	#[error("integrity check failed for {key}: expected {expected}, computed {found}")]
	Integrity { key: String, expected: String, found: String },

	/// A blob-level failure surfaced while (de)serializing a manifest.
	#[error("blob (de)serialization failed")]
	Blob(#[source] Box<BlobError>),
}

impl Retryable for StoreError {
	fn is_retryable(&self) -> bool {
		match self {
			// object_store surfaces transient network faults we can retry.
			StoreError::Backend(e) => is_object_store_retryable(e),
			StoreError::Blob(e) => e.is_retryable(),
			_ => false,
		}
	}
}

/// Classify an `object_store::Error` as transient. Kept in one place so both
/// [`StoreError`] and any wrapper agree.
fn is_object_store_retryable(err: &object_store::Error) -> bool {
	let _ = err;
	todo!("map object_store generic/timeout/reset variants onto transient vs terminal")
}

/// Failures extracting an **untrusted** source archive.
///
/// Every safety-limit trip maps onto [`FailureKind::Unsafe`] via
/// [`IngestError::failure_kind`], so the queue can dead-letter a bomb without
/// wasting retries.
#[derive(Debug, Error)]
pub enum IngestError {
	/// The archive tripped a safety limit (bomb, traversal, disallowed entry
	/// type). Terminal + flagged: maps to [`FailureKind::Unsafe`].
	#[error("unsafe archive rejected: {0}")]
	Unsafe(#[from] UnsafeArchive),

	/// The archive framing / compression was malformed.
	#[error("malformed archive")]
	Malformed(#[source] std::io::Error),

	/// Reading the source stream failed (network/disk). Transient.
	#[error("archive read failed")]
	Io(#[source] std::io::Error),

	/// Feeding sanitized bytes into the blob builder failed.
	#[error("blob assembly during ingest failed")]
	Blob(#[from] BlobError),
}

impl IngestError {
	/// The lifecycle failure class this error records into
	/// [`heart::Failure`].
	pub const fn failure_kind(&self) -> FailureKind {
		match self {
			IngestError::Unsafe(_) => FailureKind::Unsafe,
			IngestError::Malformed(_) => FailureKind::Malformed,
			IngestError::Io(_) => FailureKind::Transient,
			IngestError::Blob(_) => FailureKind::Internal,
		}
	}
}

impl Retryable for IngestError {
	fn is_retryable(&self) -> bool { matches!(self, IngestError::Io(_)) }
}

/// The specific way an archive violated the extraction safety policy. Carried by
/// [`IngestError::Unsafe`]; each variant is terminal.
#[derive(Debug, Error)]
pub enum UnsafeArchive {
	/// A path escaped the extraction root (`..`, absolute, symlinked jail).
	#[error("path traversal attempt: {path:?}")]
	PathTraversal { path: String },

	/// A disallowed entry type (symlink / hardlink / device / fifo / socket).
	#[error("disallowed entry type {kind:?} at {path:?}")]
	DisallowedEntry { kind: EntryKind, path: String },

	/// The total uncompressed size exceeded the ceiling (decompression bomb).
	#[error("total size {actual} exceeds limit {limit}")]
	TotalTooLarge { actual: u64, limit: u64 },

	/// A single file exceeded the per-file byte ceiling.
	#[error("file {path:?} size {actual} exceeds per-file limit {limit}")]
	FileTooLarge { path: String, actual: u64, limit: u64 },

	/// The archive contained more entries than allowed.
	#[error("entry count {actual} exceeds limit {limit}")]
	TooManyFiles { actual: usize, limit: usize },

	/// A path nested deeper than the allowed depth.
	#[error("path {path:?} depth {actual} exceeds limit {limit}")]
	PathTooDeep { path: String, actual: usize, limit: usize },
}

/// The tar entry classes the extractor recognizes when rejecting non-regular
/// files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EntryKind {
	Regular,
	Directory,
	Symlink,
	Hardlink,
	Character,
	Block,
	Fifo,
	Other,
}

/// Failures of the durable postgres job [`crate::queue`].
#[derive(Debug, Error)]
pub enum QueueError {
	/// The backing postgres query failed.
	#[error("queue database operation failed")]
	Database(#[source] sqlx::Error),

	/// A job referenced by id was not present.
	#[error("job for package {0:?} not found")]
	NotFound(PackageId),

	/// A completion/fail was attempted on a job whose lease had already expired
	/// and been reclaimed — the worker lost the race.
	#[error("lease lost for package {0:?}; job was reclaimed")]
	LeaseLost(PackageId),
}

impl Retryable for QueueError {
	fn is_retryable(&self) -> bool {
		match self {
			QueueError::Database(e) => is_sqlx_retryable(e),
			QueueError::LeaseLost(_) => true,
			QueueError::NotFound(_) => false,
		}
	}
}

/// Failures of the global [`crate::index`] (postgres).
#[derive(Debug, Error)]
pub enum IndexError {
	/// A backing postgres query failed.
	#[error("index database operation failed")]
	Database(#[source] sqlx::Error),

	/// The `{org}/{db}` terminus-instance token was malformed.
	#[error("invalid terminus instance token {0:?}: expected `org/db`")]
	InvalidInstance(String),

	/// A package expected in the index was absent.
	#[error("package {0:?} not present in the global index")]
	NotFound(PackageId),

	/// Applying the (sea-query-defined) schema DDL on connect failed.
	#[error("index schema initialization failed")]
	SchemaInit(#[source] sqlx::Error),
}

impl Retryable for IndexError {
	fn is_retryable(&self) -> bool {
		matches!(self, IndexError::Database(e) if is_sqlx_retryable(e))
	}

	fn retry_after(&self) -> Option<Duration> { None }
}

/// Failures of the transactional [`crate::coordination`] outbox.
#[derive(Debug, Error)]
pub enum OutboxError {
	/// The backing postgres query failed.
	#[error("outbox database operation failed")]
	Database(#[source] sqlx::Error),

	/// The append raced an equivalent `(package, snapshot, kind)` intent; the
	/// dedupe key already exists (idempotent no-op for the caller).
	#[error("duplicate outbox intent for package {package:?}")]
	Duplicate { package: PackageId },
}

impl Retryable for OutboxError {
	fn is_retryable(&self) -> bool {
		matches!(self, OutboxError::Database(e) if is_sqlx_retryable(e))
	}
}

/// Failures of registry (package) [`crate::search`].
#[derive(Debug, Error)]
pub enum SearchError {
	/// The tantivy replica index errored.
	#[error("tantivy search failed")]
	Tantivy(#[source] tantivy::TantivyError),

	/// A pagination cursor could not be decoded.
	#[error("invalid pagination cursor")]
	Cursor(#[from] heart::cursor::CursorError),

	/// Reading the postgres watermark / source failed.
	#[error("search source read failed")]
	Source(#[source] sqlx::Error),
}

impl Retryable for SearchError {
	fn is_retryable(&self) -> bool {
		matches!(self, SearchError::Source(e) if is_sqlx_retryable(e))
	}
}

/// Failures resolving a version request to a concrete
/// [`heart::PackageVersion`].
#[derive(Debug, Error)]
pub enum ResolveError {
	/// No published version satisfied the request/range.
	#[error("no version of {name} satisfies {request}")]
	NoMatch { name: String, request: String },

	/// The version request string was itself malformed.
	#[error("malformed version request {0:?}")]
	MalformedRequest(String),

	/// Looking up published versions failed (registry unreachable/errored).
	#[error("version lookup failed")]
	Lookup(#[source] reqwest::Error),

	/// The named package does not exist in the source.
	#[error("package {0} not found in source")]
	NotFound(String),
}

impl Retryable for ResolveError {
	fn is_retryable(&self) -> bool { matches!(self, ResolveError::Lookup(_)) }
}

/// Classify a `sqlx::Error` as transient. Connection resets, pool timeouts, and
/// serialization failures are retryable; syntax/constraint errors are not.
fn is_sqlx_retryable(err: &sqlx::Error) -> bool {
	let _ = err;
	todo!("map sqlx pool-timeout/io + SQLSTATE 40001/40P01 (serialization/deadlock) onto transient")
}

/// Bridge from a classified [`FailureKind`] to a [`BackendKind`] tag, for
/// health/outbox provenance. Kept here so the mapping is defined once.
pub const fn backend_of(_kind: FailureKind) -> Option<BackendKind> { None }

/// Assert an object hashes to its declared key; the single integrity gate every
/// content-addressed read passes through. `// runs on spawn_blocking`.
pub fn verify_integrity(_key: &str, _bytes: &[u8], _expected: ContentHash) -> Result<(), StoreError> {
	todo!("blake3 the bytes, compare to expected, raise StoreError::Integrity on mismatch")
}
