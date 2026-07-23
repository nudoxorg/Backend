//! `heart::sync` seam implementation for the vector edge-shard artifact plane.
//!
//! [`ShardContentIo`] implements [`ContentIo`] with `type Id = ContentHash`:
//! the content-addressed unit is the **whole packed shard artifact** — the
//! `tar.zst` bytes produced by `registry::vector::local::pack::pack_shard` and
//! stored in `cas/{blake3-hex}` via the registry's [`crate::cas::Store`].
//!
//! `verify` re-derives the BLAKE3 hash of the received bytes and compares it
//! to `id`; a mismatch → [`VerifyError::HashMismatch`]. This is the **sole**
//! write licence: no bytes reach `write` without first passing `verify`.
//!
//! This mirrors [`crate::pack::sync::ObjectPackContentIo`] for the object-pack
//! plane: same seam, same ownership model, different item kind.
//!
//! # Async vs sync
//!
//! [`ContentIo`] is a synchronous trait (so it can be called from transport
//! layers that run on blocking threads). The [`crate::cas::Store`] backend is
//! async (backed by `object_store`). The bridge uses
//! `tokio::runtime::Handle::current().block_on(…)` so `ShardContentIo` is
//! usable from any context that has a tokio runtime — exactly the environment
//! the bakery and dep-shard paths run in.

use std::io;
use std::sync::Arc;

use heart::{content::ContentHash, sync::{ContentIo, VerifyError}};

use crate::blob::creation::PendingSection;
use crate::cas::Store;
use crate::error::StoreError;
use heart::connection::Live;

// ---------------------------------------------------------------------------
// ShardContentIo
// ---------------------------------------------------------------------------

/// [`ContentIo`] adapter over the registry CAS for vector edge-shard artifacts.
///
/// The content-address unit is the **whole packed shard artifact**
/// (`tar.zst` bytes), keyed by the BLAKE3 hash of those bytes — the same
/// `artifact_id` the bakery records in `edgepack_artifacts` and the
/// dep-manifest carries for install-side verification.
///
/// The shard PACK format and the local shard query (vector search over an
/// installed shard directory) are separate concerns and are **not** part of
/// this seam — they are handled by `registry::vector::local::{pack,store}`.
pub struct ShardContentIo {
    cas: Arc<Store<Live>>,
}

impl ShardContentIo {
    /// Wrap an already-live CAS store.
    pub fn new(cas: Arc<Store<Live>>) -> Self {
        Self { cas }
    }

    /// The underlying CAS store.
    pub fn cas(&self) -> &Arc<Store<Live>> {
        &self.cas
    }
}

impl ContentIo for ShardContentIo {
    /// The BLAKE3 hash of the packed artifact bytes — the item's trust anchor.
    type Id = ContentHash;

    /// Read the raw packed shard artifact bytes from CAS.
    fn read(&self, id: &ContentHash) -> io::Result<Vec<u8>> {
        let cas = Arc::clone(&self.cas);
        let hash = *id;
        tokio::runtime::Handle::current()
            .block_on(cas.get_section(hash))
            .map(|b| b.to_vec())
            .map_err(store_err_to_io)
    }

    /// Write raw packed artifact bytes that have already passed [`Self::verify`].
    ///
    /// Delegates to [`Store::put_section`]: the write is idempotent (a
    /// re-put of identical bytes under the same hash is a no-op), so a
    /// concurrent bake of the same artifact races safely.
    fn write(&self, id: &ContentHash, bytes: &[u8]) -> io::Result<()> {
        let cas = Arc::clone(&self.cas);
        let section = PendingSection {
            hash: *id,
            bytes: bytes::Bytes::copy_from_slice(bytes),
        };
        tokio::runtime::Handle::current()
            .block_on(cas.put_section(&section))
            .map(|_| ())
            .map_err(store_err_to_io)
    }

    /// Whether the packed artifact is already present in CAS.
    fn has(&self, id: &ContentHash) -> io::Result<bool> {
        // `cas.get_section` returns `StoreError::NotFound` on a miss; we treat
        // any other error as an I/O failure rather than "not present".
        let cas = Arc::clone(&self.cas);
        let hash = *id;
        match tokio::runtime::Handle::current().block_on(cas.get_section(hash)) {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound { .. }) => Ok(false),
            Err(other) => Err(store_err_to_io(other)),
        }
    }

    /// Verify `bytes` are the packed artifact for `id` by re-deriving the
    /// BLAKE3 hash and comparing it to `id`.
    ///
    /// This IS the full content-address check for the shard-artifact plane: a
    /// packed shard artifact is identified by and ONLY by the BLAKE3 hash of
    /// its `tar.zst` bytes — the same hash `pack_shard` computes and the
    /// bakery records as `artifact_id`. A passing `verify` is the **sole**
    /// licence to call [`Self::write`].
    fn verify(&self, id: &ContentHash, bytes: &[u8]) -> Result<(), VerifyError> {
        let derived = ContentHash::of_bytes(bytes);
        if derived != *id {
            return Err(VerifyError::HashMismatch {
                expected: id.to_string(),
                got: derived.to_string(),
            });
        }
        Ok(())
    }

    /// Hard per-artifact byte cap: 2 GiB.
    ///
    /// A packed edge-shard artifact is a `tar.zst` of one qdrant-edge shard
    /// directory. Shards are bounded by the number of symbols in one package
    /// version and the QP1 int8 quantisation profile; empirically the largest
    /// are well under 512 MiB. 2 GiB is a generous safety ceiling that matches
    /// no known bake output — anything larger indicates a mis-addressed write
    /// or a hostile transfer and must be rejected before bytes are stored.
    fn max_item_bytes(&self) -> usize {
        2 * 1024 * 1024 * 1024
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn store_err_to_io(e: StoreError) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}
