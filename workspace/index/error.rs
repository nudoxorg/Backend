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

use std::path::PathBuf;
use std::time::Duration;

use heart::{BackendKind, ConnectError, FailureKind, PackageId, Retryable, content::ContentHash};
use object_store::path::Path as StorePath;
use thiserror::Error;

use crate::coordination::SinkKind;
use crate::schema::codec;

/// The top-level registry error — the union every public entry point surfaces.
///
/// Wraps the area-specific errors so a caller can `?` up through the stack and
/// still recover provenance + a retry decision. All source chains are preserved
/// with concrete types (no Box<dyn Error>).
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

    /// A global-index (catalog) failure.
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
    #[error("access denied")]
    AccessDenied(#[from] AccessDeniedReason),
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
            RegistryError::AccessDenied(_) => false,
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
    Codec(#[from] postcard::Error),

    /// A stored section's recomputed BLAKE3 digest did not match its recorded
    /// hash — corruption or tampering. Uses typed hashes so full data available
    /// for tracing without string formatting at construction.
    #[error("content hash mismatch")]
    HashMismatch {
        expected: ContentHash,
        found: ContentHash,
    },

    /// The manifest referenced a section hash the store does not hold.
    #[error("blob references a missing section")]
    MissingSection { hash: ContentHash },

    // --- Explicit fine-grained structural malformation variants (no dynamic
    // strings in error data; all literal messages + typed fields).
    #[error("duplicate file path in manifest")]
    DuplicateFilePathInManifest,

    #[error("manifest files are not sorted by path")]
    ManifestFilesNotSorted,

    #[error("manifest is missing its ir section")]
    MissingIrSection,

    #[error("manifest is missing its references section")]
    MissingReferencesSection,

    #[error("manifest has no files")]
    ManifestHasNoFiles,

    #[error("duplicate file path pushed into builder")]
    DuplicateFilePathInBuilder,

    #[error("ir section attached twice")]
    IrSectionAttachedTwice,

    #[error("references section attached twice")]
    ReferencesSectionAttachedTwice,

    #[error("reference span is inverted")]
    InvertedReferenceSpan,

    #[error("unknown reference kind discriminant")]
    UnknownReferenceKindDiscriminant { wire: u8 },

    #[error("non-UTF-8 path in resolved reference target")]
    NonUtf8ReferencePath,

    // End fine-grained malformations.
    /// Assembling the manifest touched the object store, which failed.
    #[error("blob store operation failed")]
    Store(#[source] Box<StoreError>),

    /// Outbox fan-out failure during emit (proper source, no fake Backend).
    #[error("outbox failure during blob emit")]
    Outbox(#[source] OutboxError),
}

impl From<StoreError> for BlobError {
    fn from(e: StoreError) -> Self {
        BlobError::Store(Box::new(e))
    }
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
/// All variants carry typed data (StorePath, ContentHash, PackageId) and
/// concrete #[source] for full chaining (e.g. an object-store error inside a
/// blob error inside registry).
#[derive(Debug, Error)]
pub enum StoreError {
    // --- Fine-grained mappings for object_store::Error (specific variants
    // instead of always opaque Backend; preserves original error as source).
    #[error("object store not found")]
    ObjectStoreNotFound {
        path: StorePath,
        #[source]
        source: object_store::Error,
    },

    #[error("object store permission denied")]
    ObjectStorePermissionDenied {
        path: Option<StorePath>,
        #[source]
        source: object_store::Error,
    },

    #[error("object store unauthenticated")]
    ObjectStoreUnauthenticated {
        #[source]
        source: object_store::Error,
    },

    #[error("object store precondition failed")]
    ObjectStorePreconditionFailed {
        path: StorePath,
        #[source]
        source: object_store::Error,
    },

    #[error("object store already exists")]
    ObjectStoreAlreadyExists {
        path: StorePath,
        #[source]
        source: object_store::Error,
    },

    #[error("object store generic failure")]
    ObjectStoreGeneric {
        store: &'static str,
        #[source]
        source: object_store::Error,
    },

    #[error("object store join failure")]
    ObjectStoreJoin {
        #[source]
        source: object_store::Error,
    },

    // Catch-all for other object_store cases (e.g. new variants in dep).
    #[error("object store backend failed")]
    Backend(#[from] object_store::Error),

    /// A key could not be encoded safely (typed path + explicit reason enum).
    #[error("could not encode object key")]
    KeyEncoding {
        path: StorePath,
        reason: KeyEncodingFailure,
    },

    /// A blob/section addressed by hash or pointer was not present.
    #[error("object not found at {path}")]
    NotFound { path: StorePath },

    /// The bytes read back did not hash to the key they were fetched under.
    /// Carries typed ContentHash (no hex strings in data) + path.
    #[error("integrity check failed")]
    Integrity {
        path: StorePath,
        expected: ContentHash,
        found: ContentHash,
    },

    /// Pointer bytes had wrong length for ContentHash.
    #[error("pointer bytes had invalid length for content hash")]
    InvalidPointerLength { path: StorePath, len: usize },

    /// A blob-level failure surfaced while (de)serializing a manifest.
    #[error("blob (de)serialization failed")]
    Blob(#[source] Box<BlobError>),
}

/// Distinct reasons a CAS key could not be encoded (fine grained, no strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KeyEncodingFailure {
    #[error("pointer bytes not 32 bytes")]
    PointerNot32Bytes,

    #[error("path contained invalid characters after encoding")]
    InvalidChar,

    #[error("path too long for layout")]
    TooLong,
}

impl Retryable for StoreError {
    fn is_retryable(&self) -> bool {
        match self {
            // object_store surfaces transient network faults we can retry.
            StoreError::Backend(e)
            | StoreError::ObjectStoreGeneric { source: e, .. }
            | StoreError::ObjectStoreJoin { source: e } => is_object_store_retryable(e),
            StoreError::Blob(e) => e.is_retryable(),
            // NotFound, integrity, key encode, permission etc are permanent.
            _ => false,
        }
    }
}

/// Classify an `object_store::Error` as transient. Kept in one place so both
/// [`StoreError`] and any wrapper agree.
fn is_object_store_retryable(err: &object_store::Error) -> bool {
    match err {
        // `Generic` is the catch-all the HTTP backends surface network faults,
        // 5xx responses, and timeouts through; a task-join failure is likewise a
        // runtime hiccup, not a property of the request.
        object_store::Error::Generic { .. } | object_store::Error::JoinError { .. } => true,
        // Everything else (NotFound, InvalidPath, Precondition, auth, ...) is a
        // deterministic property of the key or credentials.
        _ => false,
    }
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
    #[error("unsafe archive rejected")]
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

    /// Zstd or gzip decoder init failed (distinct from generic malformed io).
    #[error("decompressor init failed")]
    DecompressorInit(#[source] std::io::Error),
}

impl IngestError {
    /// The lifecycle failure class this error records into
    /// [`heart::Failure`].
    pub const fn failure_kind(&self) -> FailureKind {
        match self {
            IngestError::Unsafe(_) => FailureKind::Unsafe,
            IngestError::Malformed(_) => FailureKind::Malformed,
            IngestError::Io(_) => FailureKind::Transient,
            IngestError::Blob(_) | IngestError::DecompressorInit(_) => FailureKind::Internal,
        }
    }
}

impl Retryable for IngestError {
    fn is_retryable(&self) -> bool {
        matches!(self, IngestError::Io(_))
    }
}

/// The specific way an archive violated the extraction safety policy. Carried by
/// [`IngestError::Unsafe`]; each variant is terminal. Paths are PathBuf (typed),
/// sizes u64, no dynamic strings created via format/to_string for the data.
#[derive(Debug, Error)]
pub enum UnsafeArchive {
    /// A path escaped the extraction root (`..`, absolute, symlinked jail).
    #[error("path traversal attempt")]
    PathTraversal { path: PathBuf },

    /// Non-UTF-8 path bytes (separate from traversal for richer tracing).
    #[error("non utf8 path in archive")]
    NonUtf8Path { bytes: Vec<u8> },

    /// A disallowed entry type (symlink / hardlink / device / fifo / socket).
    #[error("disallowed entry type")]
    DisallowedEntry { kind: EntryKind, path: PathBuf },

    /// The total uncompressed size exceeded the ceiling (decompression bomb).
    #[error("total size exceeds limit")]
    TotalTooLarge { actual: u64, limit: u64 },

    /// A single file exceeded the per-file byte ceiling.
    #[error("file size exceeds per-file limit")]
    FileTooLarge {
        path: PathBuf,
        actual: u64,
        limit: u64,
    },

    /// The archive contained more entries than allowed.
    #[error("entry count exceeds limit")]
    TooManyFiles { actual: usize, limit: usize },

    /// A path nested deeper than the allowed depth.
    #[error("path depth exceeds limit")]
    PathTooDeep {
        path: PathBuf,
        actual: usize,
        limit: usize,
    },

    // Expanded variants for other modes found in extract.rs / ingest (header
    // sizes, budget charge variants surfaced differently, etc.).
    #[error("archive entry header size read failed")]
    HeaderSizeRead,

    #[error("compressed stream exceeded total byte ceiling before decompress")]
    CompressedSizeExceeded { actual: u64, limit: u64 },

    #[error("archive entry count exceeded during budget charge")]
    EntryCountExceededDuringCharge { actual: usize, limit: usize },
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

/// Failures of the durable scratch-backed job [`crate::queue`] (INDEX-PLAN IP-4).
#[derive(Debug, Error)]
pub enum QueueError {
    /// The backing scratch-store operation failed.
    #[error("queue scratch-store operation failed")]
    Scratch(#[source] crate::scratch::ScratchError),

    /// A job payload row could not be decoded into typed fields.
    #[error("queue job payload decode failed")]
    Codec(#[source] serde_json::Error),

    /// A job referenced by id was not present.
    #[error("job for package not found")]
    NotFound { package: PackageId },

    /// A completion/fail was attempted on a job whose lease had already expired
    /// and been reclaimed — the worker lost the race.
    #[error("lease lost; job was reclaimed")]
    LeaseLost { package: PackageId },
}

impl Retryable for QueueError {
    fn is_retryable(&self) -> bool {
        match self {
            // A local sqlite fault (e.g. a transient lock) is worth a retry.
            QueueError::Scratch(_) | QueueError::LeaseLost { .. } => true,
            QueueError::Codec(_) | QueueError::NotFound { .. } => false,
        }
    }
}

/// Failures of the global [`crate::index`] (the versioned catalog).
#[derive(Debug, Error)]
pub enum IndexError {
    /// A catalog store operation failed (engine, codec, or watermark layer).
    #[error("catalog operation failed")]
    Catalog(#[from] crate::store::MetaError),

    /// The runtime↔catalog mapping law was violated by a stored row.
    #[error("catalog row mapping failed")]
    Map(#[from] crate::schema::catalog_map::CatalogMapError),

    /// A toolchain payload failed to (de)serialize.
    #[error("toolchain JSON codec failed")]
    ToolchainJson(#[source] serde_json::Error),

    /// The `{org}/{db}` instance token (identity salt) was malformed.
    #[error("invalid instance token: expected `org/db`")]
    InvalidInstance { token: String },

    /// A package expected in the index was absent.
    #[error("package not present in the global index")]
    NotFound { package: PackageId },

    /// A stored `symbols.kind` token did not match any known [`heart::SymbolKind`].
    #[error("unknown symbol kind token in symbols row: {token}")]
    UnknownSymbolKind { token: String },

    /// Codec failures (e.g. from schema rows) now carried with concrete source
    /// so trace is not lost to to_string + Decode.
    #[error("index codec failure")]
    Codec(#[from] codec::CodecError),
}

impl Retryable for IndexError {
    fn is_retryable(&self) -> bool {
        // The catalog engine is a local store: failures are structural
        // (schema/codec/mapping), not transient network weather.
        false
    }

    fn retry_after(&self) -> Option<Duration> {
        None
    }
}

/// Failures of the transactional [`crate::coordination`] outbox.
#[derive(Debug, Error)]
pub enum OutboxError {
    /// A catalog store operation failed (engine, codec, watermark).
    #[error("outbox catalog operation failed")]
    Catalog(#[from] crate::store::MetaError),

    /// The runtime↔catalog mapping law was violated.
    #[error("outbox row mapping failed")]
    Map(#[from] crate::schema::catalog_map::CatalogMapError),

    /// An outbox row had no version id where one is required.
    #[error("outbox row {seq} carries no version id")]
    MissingVersion { seq: i64 },

    /// The append raced an equivalent `(package, snapshot, kind)` intent; the
    /// dedupe key already exists (idempotent no-op for the caller).
    #[error("duplicate outbox intent")]
    Duplicate {
        package: PackageId,
        generation: ContentHash,
        kind: SinkKind,
    },

    /// A codec failure while decoding an outbox / facets row (corrupt data or
    /// schema drift) — not a transport fault, not retryable.
    #[error("outbox codec failure")]
    Codec(#[from] codec::CodecError),

    /// An index error surfaced inside an outbox sequence (state transition
    /// half of `record_stored`).
    #[error("outbox index operation failed")]
    Index(#[from] IndexError),
}

impl Retryable for OutboxError {
    fn is_retryable(&self) -> bool {
        match self {
            OutboxError::Index(e) => e.is_retryable(),
            _ => false,
        }
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

    /// Reading the catalog watermark / source failed.
    #[error("search source read failed")]
    Source(#[from] crate::store::MetaError),

    // Fine-grained tantivy + internal + codec to avoid losing source via to_string + InternalError.
    #[error("tantivy query parse failed")]
    TantivyQueryParse(#[source] tantivy::TantivyError),

    #[error("tantivy internal invariant")]
    TantivyInternal(#[source] tantivy::TantivyError),

    #[error("search row decode failed")]
    RowDecode {
        column: &'static str,
        detail: String,
    },

    /// Reading/decoding the catalog outbox feed failed (carried as rendered
    /// detail: the outbox error is not `Sync`-clean across this boundary).
    #[error("search outbox feed read failed: {detail}")]
    OutboxRead { detail: String },

    #[error("search json decode failed")]
    JsonDecode {
        domain: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("stored package_id not a uuid")]
    StoredIdNotUuid,

    /// Codec failures preserved.
    #[error("search codec failure")]
    Codec(#[from] codec::CodecError),
}

impl Retryable for SearchError {
    fn is_retryable(&self) -> bool {
        // Search read errors are structural, not transient network weather.
        false
    }
}

/// Failures resolving a version request to a concrete
/// [`heart::PackageVersion`].
#[derive(Debug, Error)]
pub enum ResolveError {
    // Fine-grained NoMatch by request kind (distinct modes, typed name where
    // possible; request strings unavoidable for user specs but not via format!).
    #[error("no version satisfies latest request")]
    NoMatchLatest { name: String },

    #[error("no version satisfies exact pin")]
    NoMatchExact { name: String, pin: String },

    #[error("no version satisfies semver range")]
    NoMatchSemver { name: String, range: String },

    #[error("no version satisfies ecosystem range")]
    NoMatchRange { name: String, spec: String },

    /// The version request string was itself malformed.
    #[error("malformed version request")]
    MalformedRequest { spec: String },

    /// Looking up published versions failed (registry unreachable/errored).
    #[error("version lookup failed")]
    Lookup(#[from] reqwest::Error),

    /// The shared upstream client failed (retries exhausted, rate-limited,
    /// transport). Retryability delegates to the inner error's own class.
    #[error("upstream fetch failed")]
    Upstream(#[from] crate::upstream::UpstreamError),

    /// The named package does not exist in the source.
    #[error("package not found in source")]
    NotFound { name: String },
}

impl Retryable for ResolveError {
    fn is_retryable(&self) -> bool {
        match self {
            ResolveError::Lookup(_) => true,
            ResolveError::Upstream(inner) => inner.is_retryable(),
            _ => false,
        }
    }
}

/// Bridge from a classified [`FailureKind`] to a [`BackendKind`] tag, for
/// health/outbox provenance. Kept here so the mapping is defined once.
pub const fn backend_of(_kind: FailureKind) -> Option<BackendKind> {
    None
}

/// Explicit reasons for access denial (no more opaque &'static str; all cases
/// are first-class variants so callers can match and logs have structure).
#[derive(Debug, Error)]
pub enum AccessDeniedReason {
    #[error("insufficient permissions for package {package:?}")]
    Package { package: PackageId },

    #[error("insufficient permissions for tenant or org operation")]
    Tenant,

    #[error("operation not allowed in current state")]
    State,

    #[error("custom access policy violation")]
    Policy { detail: &'static str },
}

/// Assert an object hashes to its declared key; the single integrity gate every
/// content-addressed read passes through. `// runs on spawn_blocking`.
/// Carries typed StorePath + ContentHash (no strings created for error data).
pub fn verify_integrity(
    key: StorePath,
    bytes: &[u8],
    expected: ContentHash,
) -> Result<(), StoreError> {
    let found = ContentHash::of_bytes(bytes);
    if found == expected {
        Ok(())
    } else {
        tracing::warn!(key = %key, "content-addressed read failed its integrity check");
        Err(StoreError::Integrity {
            path: key,
            expected,
            found,
        })
    }
}

/// The lowercase hex form of a [`ContentHash`] — the display convention every
/// error message and object key in this crate shares. Used only for *path*
/// construction and logging, never to populate error fields (typed hashes kept).
pub(crate) fn hash_hex(hash: &ContentHash) -> String {
    data_encoding::HEXLOWER.encode(hash.as_bytes())
}
