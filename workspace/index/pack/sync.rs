//! `heart::sync` seam implementations for the object-pack plane.
//!
//! [`ObjectPackContentIo`] implements [`ContentIo`] with `type Id = ObjectPackId`:
//! read/write/has delegate to any [`ObjectPackStore`]; `verify` re-derives the
//! [`ObjectPackId`] from the received bytes' TOC (the same content-address check
//! the store's `install_pack` does) and returns
//! [`VerifyError::HashMismatch`] on a mismatch.  This is the **sole** write
//! licence: no bytes reach `write` without passing `verify` first.
//!
//! [`ObjectPackApplyHook`] implements [`ApplyHook`] with the same `Id`:
//! "apply" = install the fetched (already-verified) packs into the local store.
//!
//! Both are the seam the object-pack transport plugs into, mirroring how
//! `ir-vcs::sync::{FsChangeIo, RepoApplyHook}` implement the same traits for
//! the IR VCS plane.

use std::io;
use std::sync::Arc;

use bytes::Bytes;

use heart::object_pack::ObjectPackId;
use heart::sync::{ApplyHook, ContentIo, SyncError, VerifyError};

use crate::pack::error::PackError;
use crate::pack::outboard::MemberOutboard;
use crate::pack::reader::ObjectPackReader;
use crate::pack::store::ObjectPackStore;

// ---------------------------------------------------------------------------
// ContentIo — whole-pack read/write/verify over an ObjectPackStore
// ---------------------------------------------------------------------------

/// [`ContentIo`] adapter over any [`ObjectPackStore`].
///
/// The content-address unit is the **whole sealed pack** (`ObjectPackId`); the
/// member-range access pattern is a separate, range-addressable API that does
/// not go through this seam (see [`crate::pack::store::ObjectPackStore::get_member_range`]).
pub struct ObjectPackContentIo<S: ObjectPackStore> {
    store: Arc<S>,
}

impl<S: ObjectPackStore> ObjectPackContentIo<S> {
    /// Wrap an existing store arc.
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    /// Borrow the underlying store.
    pub fn store(&self) -> &Arc<S> {
        &self.store
    }
}

impl<S: ObjectPackStore + 'static> ContentIo for ObjectPackContentIo<S> {
    /// The whole-pack content-address id.
    type Id = ObjectPackId;

    /// Read the raw sealed pack bytes for `id` from the local store.
    fn read(&self, id: &ObjectPackId) -> io::Result<Vec<u8>> {
        self.store
            .read_pack_bytes(id)
            .map(|b| b.to_vec())
            .map_err(pack_err_to_io)
    }

    /// Write raw pack bytes that have already passed [`Self::verify`].
    ///
    /// Delegates to [`ObjectPackStore::install_pack`] with no outboards (the
    /// outboards are generated on the provider side and are a sidecar concern;
    /// the seam contract is raw-bytes only). The store re-derives the id and
    /// rejects a mismatch as a second safety net.
    fn write(&self, id: &ObjectPackId, bytes: &[u8]) -> io::Result<()> {
        self.store
            .install_pack(id, bytes, &[])
            .map_err(pack_err_to_io)
    }

    /// Whether a pack with this id is already present in the store.
    fn has(&self, id: &ObjectPackId) -> io::Result<bool> {
        Ok(self.store.has(id))
    }

    /// Re-derive the [`ObjectPackId`] from `bytes`' TOC and compare to `id`.
    ///
    /// This IS the full content-address check for the object-pack plane: opening
    /// the bytes as an [`ObjectPackReader`] parses the header, decodes + sorts
    /// the TOC, verifies the TOC's BLAKE3 digest, and re-derives the id — an
    /// identical path to what `install_pack` does internally.  A passing
    /// `verify` is the sole licence to write.
    fn verify(&self, id: &ObjectPackId, bytes: &[u8]) -> Result<(), VerifyError> {
        let reader = ObjectPackReader::open_bytes(Bytes::copy_from_slice(bytes))
            .map_err(|e| VerifyError::HashMismatch {
                expected: id.to_string(),
                got: format!("(parse error: {e})"),
            })?;
        let derived = reader.id();
        if derived != *id {
            return Err(VerifyError::HashMismatch {
                expected: id.to_string(),
                got: derived.to_string(),
            });
        }
        Ok(())
    }

    /// Hard cap on a whole-pack transfer: 512 MiB (matches `FRAME_CAP_BYTES` in
    /// the transport module, which guards the declared length before bytes flow).
    fn max_item_bytes(&self) -> usize {
        512 * 1024 * 1024
    }
}

// ---------------------------------------------------------------------------
// ApplyHook — install fetched packs into the local store
// ---------------------------------------------------------------------------

/// Install-side hook: apply a set of fetched (verified + written) packs into
/// the local store together with their outboards.
///
/// For the object-pack plane "apply" means atomic install: the whole-pack bytes
/// are already in the store via [`ContentIo::write`]; the hook's job is to
/// install any accompanying outboards so range-streaming is possible.
///
/// In the common single-pack path (one `WholePack` response), `ids` has exactly
/// one element.  The `Target` is the slice of outboards that arrived alongside
/// the pack bytes; `Tip` is the last installed [`ObjectPackId`] (or the only
/// one in the common case).
pub struct ObjectPackApplyHook<S: ObjectPackStore> {
    store: Arc<S>,
}

impl<S: ObjectPackStore> ObjectPackApplyHook<S> {
    /// Wrap an existing store arc.
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }
}

impl<S: ObjectPackStore + 'static> ApplyHook for ObjectPackApplyHook<S> {
    type Id = ObjectPackId;
    /// The outboards that accompanied the pack transfer (an empty slice is valid
    /// for packs with no large members).
    type Target = Vec<MemberOutboard>;
    /// The last installed [`ObjectPackId`].
    type Tip = ObjectPackId;

    /// Install `ids` (already verified + written as raw bytes) with their
    /// `outboards` into the local store, returning the last id as the new tip.
    ///
    /// Because [`ContentIo::write`] already called `install_pack` (which does
    /// the atomic tempfile+rename and verifies the id), this hook's job is to
    /// re-install only to write the outboard sidecar.  For packs with no large
    /// members `outboards` is empty and `install_pack` is a quick presence check.
    ///
    /// Returns [`SyncError::Other`] if `ids` is empty.
    fn apply(
        &self,
        outboards: &Vec<MemberOutboard>,
        ids: &[ObjectPackId],
    ) -> Result<ObjectPackId, SyncError> {
        let tip = ids
            .last()
            .copied()
            .ok_or_else(|| SyncError::Other("apply called with empty id list".into()))?;

        for id in ids {
            // Read the bytes we already wrote through ContentIo::write so we can
            // re-install together with outboards (the outboard sidecar is the
            // only thing that changes vs. the ContentIo::write path).
            let pack_bytes = self
                .store
                .read_pack_bytes(id)
                .map_err(|e| SyncError::Io(io::Error::other(e.to_string())))?;
            self.store
                .install_pack(id, &pack_bytes, outboards)
                .map_err(|e| SyncError::Io(io::Error::other(e.to_string())))?;
        }

        Ok(tip)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn pack_err_to_io(e: PackError) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}
