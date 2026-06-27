use thiserror::Error;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Errors produced by the blob store backend.
#[derive(Debug, Error)]
pub enum BlobStoreError {
    /// The stored blob schema version does not match the expected version.
    #[error("schema version mismatch: expected {expected}, got {got}")]
    SchemaVersionMismatch {
        /// The version this build expects.
        expected: u32,
        /// The version found in the stored record.
        got: u32,
    },
    /// The requested blob was not found.
    #[error("blob not found")]
    NotFound,
    /// Failed to create the blob store root directory.
    #[error("create dir: {0}")]
    CreateDir(#[source] BoxError),
    /// Failed to initialise the underlying filesystem backend.
    #[error("filesystem setup: {0}")]
    Filesystem(#[source] BoxError),
    /// Failed to serialise blob data.
    #[error("serialize: {0}")]
    Serialize(#[source] BoxError),
    /// Failed to deserialise blob data.
    #[error("deserialize: {0}")]
    Deserialize(#[source] BoxError),
    /// Failed to write a blob to storage.
    #[error("put: {0}")]
    Put(#[source] BoxError),
    /// Failed to read a blob from storage.
    #[error("get: {0}")]
    Get(#[source] BoxError),
    /// Failed to list blobs in storage.
    #[error("list: {0}")]
    List(#[source] BoxError),
}

/// Errors produced by the global symbol store.
#[derive(Debug, Error)]
pub enum GlobalStoreError {
    /// A UUID stored in the database could not be parsed.
    #[error("malformed UUID in database")]
    InvalidId(#[source] uuid::Error),
    /// Failed to open the SQLite connection pool.
    #[error("connect: {0}")]
    Connect(#[source] BoxError),
    /// Failed to initialise the database schema.
    #[error("schema init: {0}")]
    Schema(#[source] BoxError),
    /// Failed to begin or commit a database transaction.
    #[error("transaction: {0}")]
    Transaction(#[source] BoxError),
    /// Failed to register a symbol in the database.
    #[error("register: {0}")]
    Register(#[source] BoxError),
    /// Failed to query the database for a symbol.
    #[error("query: {0}")]
    Query(#[source] BoxError),
    /// Failed to access a column from a database row.
    #[error("row access: {0}")]
    RowAccess(#[source] BoxError),
    /// Failed to associate an occurrence with a global symbol.
    #[error("associate: {0}")]
    Associate(#[source] BoxError),
    /// Failed to count symbols for a library.
    #[error("count: {0}")]
    Count(#[source] BoxError),
}

/// Errors produced by a queue implementation.
#[derive(Debug, Error)]
pub enum QueueError {
    /// Failed to begin or commit a queue transaction.
    #[error("transaction: {0}")]
    Transaction(#[source] BoxError),
    /// Failed to enqueue a blob reference.
    #[error("enqueue: {0}")]
    Enqueue(#[source] BoxError),
    /// Failed to drain the queue.
    #[error("drain: {0}")]
    Drain(#[source] BoxError),
    /// Failed to access a column from a queue row.
    #[error("row access: {0}")]
    RowAccess(#[source] BoxError),
    /// Failed to count deferred items.
    #[error("count: {0}")]
    Count(#[source] BoxError),
}

/// Errors produced by the full-text search index.
#[derive(Debug, Error)]
pub enum SearchError {
    /// The index writer mutex was poisoned.
    #[error("index writer lock poisoned")]
    LockPoisoned,
    /// Failed to create the index directory.
    #[error("create dir: {0}")]
    CreateDir(#[source] BoxError),
    /// Failed to open the memory-mapped directory.
    #[error("open directory: {0}")]
    OpenDirectory(#[source] BoxError),
    /// Failed to open or create the index.
    #[error("open index: {0}")]
    OpenIndex(#[source] BoxError),
    /// Failed to create an index writer.
    #[error("create writer: {0}")]
    CreateWriter(#[source] BoxError),
    /// Failed to add a document to the index.
    #[error("add document: {0}")]
    AddDocument(#[source] BoxError),
    /// Failed to commit the index writer.
    #[error("commit: {0}")]
    Commit(#[source] BoxError),
    /// Failed to open an index reader.
    #[error("open reader: {0}")]
    OpenReader(#[source] BoxError),
    /// Failed to parse the search query string.
    #[error("parse query: {0}")]
    ParseQuery(#[source] BoxError),
    /// Failed to execute a search query.
    #[error("search: {0}")]
    Search(#[source] BoxError),
    /// Failed to retrieve a document by address.
    #[error("fetch document: {0}")]
    FetchDocument(#[source] BoxError),
}

/// Errors produced by the vector index.
#[derive(Debug, Error)]
pub enum VectorError {
    /// Failed to build the Qdrant client.
    #[error("connect: {0}")]
    Connect(#[source] BoxError),
    /// Failed to check whether a collection exists.
    #[error("collection exists check: {0}")]
    CollectionExists(#[source] BoxError),
    /// Failed to create a collection.
    #[error("create collection: {0}")]
    CreateCollection(#[source] BoxError),
    /// Failed to delete a collection.
    #[error("delete collection: {0}")]
    DeleteCollection(#[source] BoxError),
    /// Failed to count points in the collection.
    #[error("count: {0}")]
    Count(#[source] BoxError),
    /// Failed to upsert points into the collection.
    #[error("upsert: {0}")]
    Upsert(#[source] BoxError),
    /// Failed to search the collection.
    #[error("search: {0}")]
    Search(#[source] BoxError),
}

/// Errors produced by an embedder implementation.
#[derive(Debug, Error)]
pub enum EmbedderError {
    /// The model path supplied to an in-process embedder was empty.
    #[error("model path must not be empty")]
    EmptyPath,
    /// The embedding endpoint returned a non-success HTTP status.
    #[error("HTTP {status}: {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body text.
        body: String,
    },
    /// The response JSON was missing a required field.
    #[error("response missing field: {field}")]
    InvalidResponse {
        /// Name of the missing field.
        field: &'static str,
    },
    /// Failed to send the HTTP request.
    #[error("send: {0}")]
    Send(#[source] BoxError),
    /// Failed to decode the HTTP response.
    #[error("decode: {0}")]
    Decode(#[source] BoxError),
}

/// Errors produced during pipeline execution.
#[derive(Debug, Error)]
pub enum PipelineError {
    /// A pipeline stage failed.
    #[error("pipeline: {0}")]
    Stage(#[source] BoxError),
}

/// The unified error type for all nudox-occurrences operations.
#[derive(Debug, Error)]
pub enum Error {
    /// An error originating from the blob store backend.
    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),
    /// An error originating from the global symbol store.
    #[error(transparent)]
    GlobalStore(#[from] GlobalStoreError),
    /// An error produced by an embedder implementation.
    #[error(transparent)]
    Embedder(#[from] EmbedderError),
    /// An error produced by the full-text search index.
    #[error(transparent)]
    Search(#[from] SearchError),
    /// An error produced by the vector index.
    #[error(transparent)]
    Vector(#[from] VectorError),
    /// An error produced by a queue implementation.
    #[error(transparent)]
    Queue(#[from] QueueError),
    /// An error produced during pipeline execution.
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
}

/// Convenience `Result` alias for [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;
