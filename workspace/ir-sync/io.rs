//! [`ChangeIo`] trait — the repo abstraction wired by the host.
//!
//! This crate never imports `nudox-ir-vcs` directly. Instead the host provides
//! a `ChangeIo` implementation that bridges to `FsChanges` (libpijul's
//! filesystem changestore under `<repo-root>/changes/`).

use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex},
};

use crate::types::{ChangeId, ChannelRef, SyncError, VerifyError};

/// Maximum allowed change blob size (64 MiB).
///
/// This is a hard cap applied **before writing**. Any change blob from an
/// untrusted source that exceeds this limit is rejected with
/// [`SyncError::ChangeTooLarge`] and the sync is aborted entirely — we never
/// trust sizes declared in the announcement, only the actual bytes received.
pub const MAX_CHANGE_BYTES: usize = 64 * 1024 * 1024;

/// Repo-side I/O abstraction for reading and writing pijul change files.
///
/// # Contract
///
/// **Implementations MUST verify before write.**
/// `write_change` should only be called AFTER `verify` has succeeded. The
/// `SyncService` enforces this ordering, but a `ChangeIo` implementation that
/// skips verification before writing would undermine the trust model:
/// content-addressing is the only trust anchor. There is no transport-level
/// authority — even a connection from a known iroh node ID can serve corrupt
/// bytes.
///
/// # Full pijul-hash verification
///
/// The default `verify` impl below does a BLAKE3 content check, which is
/// useful for tests. **A production implementation over `FsChanges` MUST
/// additionally call `libpijul::change::Change::deserialize` and compare the
/// recomputed pijul hash to the announced `ChangeId`.** Only that gives you
/// full change validation. Document this requirement in your implementation.
pub trait ChangeIo: Send + Sync {
    /// Read the raw bytes of the change file for `id`.
    fn read_change(&self, id: &ChangeId) -> io::Result<Vec<u8>>;

    /// Write raw change bytes to the store.
    ///
    /// The caller guarantees that `verify(id, bytes)` has already succeeded.
    /// Implementations may assume the bytes are valid but MUST NOT write if
    /// they detect corruption (e.g. filename / content mismatch).
    fn write_change(&self, id: &ChangeId, bytes: &[u8]) -> io::Result<()>;

    /// Returns `true` if the change is already present in the store.
    fn has_change(&self, id: &ChangeId) -> io::Result<bool>;

    /// Verify that `bytes` are consistent with `id`.
    ///
    /// The default implementation checks only that the raw bytes BLAKE3-hash
    /// to the first 32 bytes of the hex in `id`. This is a useful property
    /// for test doubles but **not** sufficient for production: pijul's change
    /// hash is computed over the canonicalized change structure, not raw bytes.
    ///
    /// Production implementations over `FsChanges` MUST override this with a
    /// call to `libpijul::change::Change::deserialize` + compare the resulting
    /// `Hash` to `id`.
    fn verify(&self, id: &ChangeId, bytes: &[u8]) -> Result<(), VerifyError> {
        // Default: BLAKE3 of raw bytes → lowercase hex → first 64 chars == id.
        // This is valid for test doubles where the ChangeId *is* the blake3 hex.
        let hash = blake3::hash(bytes);
        let hex = hex_of_blake3(hash.as_bytes());
        if hex != id.as_str() {
            return Err(VerifyError::HashMismatch {
                expected: id.clone(),
                got: hex,
            });
        }
        Ok(())
    }
}

/// Apply hook: called on the receiver after all change bytes have been written,
/// to apply the changes to the local libpijul channel and advance the tip.
///
/// # Integration point
///
/// Implement this over `IrRepository` in `nudox-ir-vcs`. Specifically, call
/// libpijul's `apply_change` (or `IrRepository::apply_change` if exposed) for
/// each change in `changes` in order, then return the new channel tip as a
/// `ChangeId`.
pub trait ApplyHook: Send + Sync {
    /// Apply `changes` (in dependency order) to the given channel.
    ///
    /// Returns the new tip `ChangeId` after all changes have been applied.
    fn apply(&self, channel: &ChannelRef, changes: &[ChangeId]) -> Result<ChangeId, SyncError>;
}

/// In-memory [`ChangeIo`] test double.
///
/// Stores change bytes in a `HashMap` keyed by `ChangeId`. The `verify` method
/// uses BLAKE3 — test change IDs must be the 64-hex BLAKE3 hash of their bytes
/// (see the test helpers in `tests.rs`).
#[derive(Clone, Debug, Default)]
pub struct MemChangeIo {
    inner: Arc<Mutex<HashMap<ChangeId, Vec<u8>>>>,
}

impl MemChangeIo {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-populate with a `(ChangeId, bytes)` pair. Useful in test setup.
    pub fn insert(&self, id: ChangeId, bytes: Vec<u8>) {
        self.inner.lock().expect("poisoned").insert(id, bytes);
    }

    /// Return all stored change IDs (for assertions).
    pub fn keys(&self) -> Vec<ChangeId> {
        self.inner.lock().expect("poisoned").keys().cloned().collect()
    }
}

impl ChangeIo for MemChangeIo {
    fn read_change(&self, id: &ChangeId) -> io::Result<Vec<u8>> {
        self.inner
            .lock()
            .expect("poisoned")
            .get(id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("change {id} not found")))
    }

    fn write_change(&self, id: &ChangeId, bytes: &[u8]) -> io::Result<()> {
        self.inner
            .lock()
            .expect("poisoned")
            .insert(id.clone(), bytes.to_vec());
        Ok(())
    }

    fn has_change(&self, id: &ChangeId) -> io::Result<bool> {
        Ok(self.inner.lock().expect("poisoned").contains_key(id))
    }

    // Uses the trait default verify (BLAKE3) — matches the test ChangeId construction.
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format 32 raw bytes as 64 lowercase hex characters.
pub(crate) fn hex_of_blake3(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        let hi = char::from_digit((b >> 4) as u32, 16).expect("hex digit");
        let lo = char::from_digit((b & 0xf) as u32, 16).expect("hex digit");
        out.push(hi);
        out.push(lo);
    }
    out
}

/// Construct a test `ChangeId` from arbitrary bytes: compute BLAKE3, encode as
/// 64 lowercase hex chars.
pub fn change_id_for_bytes(bytes: &[u8]) -> ChangeId {
    let hash = blake3::hash(bytes);
    ChangeId(hex_of_blake3(hash.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_change_io_roundtrip() {
        let store = MemChangeIo::new();
        let data = b"hello pijul";
        let id = change_id_for_bytes(data);

        assert!(!store.has_change(&id).unwrap());
        store.verify(&id, data).unwrap();
        store.write_change(&id, data).unwrap();
        assert!(store.has_change(&id).unwrap());
        assert_eq!(store.read_change(&id).unwrap(), data);
    }

    #[test]
    fn verify_tampered_bytes_fails() {
        let store = MemChangeIo::new();
        let data = b"good bytes";
        let id = change_id_for_bytes(data);
        let tampered = b"bad bytes!!";
        let err = store.verify(&id, tampered).unwrap_err();
        assert!(matches!(err, VerifyError::HashMismatch { .. }));
    }
}
