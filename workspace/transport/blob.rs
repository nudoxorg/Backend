//! Opaque content-addressed blob transfer over iroh-blobs.
//!
//! This is the path the IR-sync plane used: hand a change file's bytes to an
//! in-memory blob store, which BLAKE3-hashes them into a transport hash; the
//! receiver fetches the blob by that transport hash and reads the bytes back
//! out (size-capped) for its own `heart::sync::ContentIo::verify`.
//!
//! The transport hash is the iroh-blobs BLAKE3 hash of the raw bytes — a
//! *transport-level* address, distinct from the caller's content-address id
//! (a pijul change hash, a pack member hash, …). The caller announces the
//! `(their_id, transport_hash)` pairing, fetches by transport hash, then
//! verifies the received bytes against *their* id. Content-addressing stays the
//! trust anchor: a passing `ContentIo::verify` is the sole license to write.

use heart::sync::SyncError;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::{BlobsProtocol, HashAndFormat};

pub use iroh_blobs::ALPN as BLOBS_ALPN;

/// The transport-level BLAKE3 hash of a blob's raw bytes (32 bytes).
///
/// Produced by [`Provider::add_bytes`], announced by the caller alongside its
/// own content-address id, and consumed by [`Fetcher::fetch`]. Distinct from
/// the caller's `ContentIo::Id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TransportHash(pub [u8; 32]);

impl From<iroh_blobs::Hash> for TransportHash {
    fn from(hash: iroh_blobs::Hash) -> Self {
        TransportHash(*hash.as_bytes())
    }
}

impl From<TransportHash> for iroh_blobs::Hash {
    fn from(hash: TransportHash) -> Self {
        iroh_blobs::Hash::from_bytes(hash.0)
    }
}

/// The sender side: an in-memory iroh-blobs store served over a router on
/// [`BLOBS_ALPN`]. Add content-addressed bytes; peers fetch them by the
/// returned [`TransportHash`].
pub struct Provider {
    router: iroh::protocol::Router,
    store: MemStore,
}

impl Provider {
    /// Wrap a bound endpoint in a blobs router that serves this provider's
    /// in-memory store. The caller builds the endpoint (via
    /// [`crate::endpoint::bind_endpoint`], advertising [`BLOBS_ALPN`]).
    pub fn serve(endpoint: iroh::Endpoint) -> Self {
        let store = MemStore::new();
        let blobs = BlobsProtocol::new(&store, None);
        let router = iroh::protocol::Router::builder(endpoint)
            .accept(BLOBS_ALPN, blobs)
            .spawn();
        Self { router, store }
    }

    /// The provider's iroh endpoint (tests share its address).
    pub fn endpoint(&self) -> &iroh::Endpoint {
        self.router.endpoint()
    }

    /// Add blob bytes to the store, returning the transport hash peers fetch by.
    pub async fn add_bytes(&self, bytes: Vec<u8>) -> Result<TransportHash, SyncError> {
        let tag = self
            .store
            .add_bytes(bytes)
            .await
            .map_err(|error| SyncError::Transport(error.to_string()))?;
        Ok(TransportHash::from(tag.hash))
    }
}

/// The receiver side: an in-memory iroh-blobs store into which blobs are pulled
/// from a remote provider by [`TransportHash`] and read back for verification.
pub struct Fetcher {
    endpoint: iroh::Endpoint,
    store: MemStore,
}

impl Fetcher {
    /// Wrap a bound endpoint (advertising the caller's control ALPN) with a
    /// fresh in-memory store for pulled blobs. Blob pulls open their own
    /// [`BLOBS_ALPN`] connection to the provider on demand.
    pub fn new(endpoint: iroh::Endpoint) -> Self {
        Self {
            endpoint,
            store: MemStore::new(),
        }
    }

    /// The fetcher's iroh endpoint.
    pub fn endpoint(&self) -> &iroh::Endpoint {
        &self.endpoint
    }

    /// Fetch the blob identified by `transport_hash` from `provider` and return
    /// its raw bytes, refusing anything larger than `max_bytes`.
    ///
    /// The bytes are **not** trusted here — the caller passes them to its own
    /// `ContentIo::verify` against its content-address id. This function only
    /// moves bytes and bounds their size.
    pub async fn fetch(
        &self,
        provider: iroh::EndpointId,
        transport_hash: TransportHash,
        max_bytes: usize,
    ) -> Result<Vec<u8>, SyncError> {
        let blob_hash = iroh_blobs::Hash::from(transport_hash);
        let request = HashAndFormat::raw(blob_hash);

        let connection = self
            .endpoint
            .connect(provider, BLOBS_ALPN)
            .await
            .map_err(|error| SyncError::Transport(error.to_string()))?;

        self.store
            .remote()
            .fetch(connection, request)
            .await
            .map_err(|error| SyncError::Transport(error.to_string()))?;

        let bytes = {
            use tokio::io::AsyncReadExt;
            let mut reader = self.store.reader(blob_hash);
            let mut buffer = Vec::new();
            reader
                .read_to_end(&mut buffer)
                .await
                .map_err(SyncError::Io)?;
            buffer
        };

        if bytes.len() > max_bytes {
            return Err(SyncError::ItemTooLarge {
                id: transport_hash_hex(&transport_hash),
                size: bytes.len(),
                max: max_bytes,
            });
        }

        Ok(bytes)
    }
}

fn transport_hash_hex(hash: &TransportHash) -> String {
    iroh_blobs::Hash::from(*hash).to_string()
}
