//! nudox-sync — repo-side glue between the IR VCS and iroh change sync.
//!
//! Provides:
//! - [`FsChangeIo`]: a [`crate::ChangeIo`] implementation over the
//!   filesystem changestore used by [`nudox_ir_vcs::IrRepository`]. Uses the
//!   same on-disk path layout as `libpijul`'s `FileSystem` changestore so that
//!   changes recorded by the repo are readable by the sync layer and vice versa.
//! - [`RepoApplyHook`]: an [`crate::ApplyHook`] that wraps an
//!   `IrRepository<FsChanges>` and calls
//!   [`IrRepository::apply_external_changes`] to import received changes.
//! - [`merge_event`]: constructs a [`crate::MergeEvent`] from a repo's log.
//! - [`sync_merged`]: thin wrapper around [`crate::Syncer::on_merge`].

use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use libpijul::changestore::filesystem::FileSystem as FsChanges;
use libpijul::change::Change;
use libpijul::pristine::Base32;

use crate::{ChangeId, ChannelRef, MergeEvent, SyncAck, SyncError, VerifyError};
use crate::io::{ApplyHook, ChangeIo};

use nudox_ir_vcs::repo::{ChangeHashHex, IrRepository};
use nudox_ir_vcs::error::VcsError;


// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors specific to nudox-sync operations.
#[derive(Debug, thiserror::Error)]
pub enum SyncGlueError {
    /// The `ChannelRef` channel name does not match the repository's working
    /// channel. A sync for channel X must not apply to a repo opened on Y.
    #[error("channel mismatch: sync is for channel '{sync_channel}' but repo is on '{repo_channel}'")]
    ChannelMismatch {
        sync_channel: String,
        repo_channel: String,
    },

    /// A VCS operation (apply, log, tip, etc.) failed.
    #[error("vcs error: {0}")]
    Vcs(#[from] VcsError),

    /// An io_sync-layer sync error.
    #[error("sync error: {0}")]
    Sync(#[from] SyncError),

    /// Change hex is not valid (wrong length or non-hex chars).
    #[error("invalid change id: {0}")]
    InvalidChangeId(#[from] VerifyError),

    /// A merge event was requested but nothing is new since `prev_tip`
    /// (or the log is empty). Not a failure of the store — there is simply
    /// nothing to announce; callers should skip the sync.
    #[error("nothing to sync: no changes newer than the previous tip")]
    NothingToSync,
}

// ---------------------------------------------------------------------------
// hex↔Hash helpers (mirrors repo.rs internals, duplicated to avoid pub(crate))
// ---------------------------------------------------------------------------

/// Convert a 64-lowercase-hex `ChangeId` to a `libpijul::pristine::Hash`.
///
/// The pijul hash is a Blake3 variant tagged with an algorithm byte; the hex
/// we store is the raw 32 Blake3 bytes (no algorithm tag). We must convert to
/// `Hash::Blake3([u8; 32])` which `to_base32()` will then encode correctly for
/// the filesystem path layout.
fn change_id_to_pijul_hash(id: &ChangeId) -> Result<libpijul::pristine::Hash, SyncGlueError> {
    let hex = id.as_str();
    if hex.len() != 64 {
        return Err(SyncGlueError::InvalidChangeId(VerifyError::InvalidLength {
            expected: 64,
            got: hex.len(),
        }));
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16).ok_or_else(|| {
            SyncGlueError::InvalidChangeId(VerifyError::InvalidHexChar(chunk[0] as char))
        })? as u8;
        let lo = (chunk[1] as char).to_digit(16).ok_or_else(|| {
            SyncGlueError::InvalidChangeId(VerifyError::InvalidHexChar(chunk[1] as char))
        })? as u8;
        bytes[i] = (hi << 4) | lo;
    }
    Ok(libpijul::pristine::Hash::Blake3(bytes))
}

// ---------------------------------------------------------------------------
// FsChangeIo
// ---------------------------------------------------------------------------

/// [`ChangeIo`] implementation over the filesystem changestore used by
/// `IrRepository<FsChanges>`.
///
/// The path layout mirrors `libpijul::changestore::filesystem::push_filename`:
/// `<changes_root>/<b32[..2]>/<b32[2..]>.change`
/// where `b32` is the base-32 encoding of the pijul `Hash`.
///
/// `verify` performs **full pijul verification**: it calls
/// `libpijul::change::Change::check_from_buffer` which decompresses the
/// hashed section and recomputes the Blake3 hash, rejecting any tampered file.
pub struct FsChangeIo {
    changes_root: PathBuf,
}

impl FsChangeIo {
    /// Create an `FsChangeIo` that reads and writes the same directory that
    /// libpijul's `FileSystem` changestore uses for an `IrRepository` opened
    /// at `repo_root`.
    ///
    /// The path is derived from the same logic as `IrRepository::open`:
    /// `FsChanges::from_root(repo_root.join("changes"), …)` which expands
    /// to `<repo-root>/changes/.pijul/changes/`.
    ///
    /// Pass the same root that you pass to `IrRepository::open`.
    pub fn for_repo(repo_root: &std::path::Path) -> Self {
        let changes_dir = repo_root
            .join("changes")
            .join(".pijul")
            .join("changes");
        Self { changes_root: changes_dir }
    }

    /// Create an `FsChangeIo` with an explicit `changes_dir`.
    ///
    /// Use this when you know the exact `changes_dir` (e.g., you computed it
    /// yourself or are using `FsChanges::from_changes`). Most callers should
    /// prefer [`Self::for_repo`].
    pub fn new(changes_root: PathBuf) -> Self {
        Self { changes_root }
    }

    /// Resolve the on-disk path for a change, using the same layout as
    /// `libpijul::changestore::filesystem::push_filename`:
    /// `<changes_root>/<b32[..2]>/<b32[2..]>.change`.
    fn path_for(&self, id: &ChangeId) -> Result<PathBuf, SyncGlueError> {
        let hash = change_id_to_pijul_hash(id)?;
        let b32 = hash.to_base32();
        // The base32 string is split at position 2: first 2 chars = subdir,
        // remainder (with .change extension) = filename.
        let (prefix, rest) = b32.split_at(2);
        let mut path = self.changes_root.clone();
        path.push(prefix);
        path.push(rest);
        path.set_extension("change");
        Ok(path)
    }
}

impl ChangeIo for FsChangeIo {
    fn read_change(&self, id: &ChangeId) -> io::Result<Vec<u8>> {
        let path = self.path_for(id).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
        })?;
        std::fs::read(&path)
    }

    fn write_change(&self, id: &ChangeId, bytes: &[u8]) -> io::Result<()> {
        // Contract: verify before write.
        self.verify(id, bytes).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e.to_string())
        })?;

        let path = self.path_for(id).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
        })?;

        // Atomic write: tmp file in the same directory + rename.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(
            path.parent().unwrap_or(&self.changes_root),
        )?;
        use std::io::Write;
        tmp.write_all(bytes)?;
        tmp.persist(&path).map_err(|e| e.error)?;
        Ok(())
    }

    fn has_change(&self, id: &ChangeId) -> io::Result<bool> {
        let path = self.path_for(id).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
        })?;
        Ok(path.exists())
    }

    /// Full pijul verification: deserialize the change from `bytes` using
    /// `libpijul::change::Change::check_from_buffer` and verify that the
    /// recomputed Blake3 hash equals the announced `ChangeId`.
    ///
    /// `check_from_buffer` decompresses the hashed section of the change file,
    /// recomputes the BLAKE3 hash of the canonical (decompressed) representation,
    /// and compares it to the claimed hash. Any byte-level tampering that alters
    /// the compressed or uncompressed content will cause a mismatch and return
    /// an error.
    fn verify(&self, id: &ChangeId, bytes: &[u8]) -> Result<(), VerifyError> {
        let hash = change_id_to_pijul_hash(id).map_err(|_| {
            VerifyError::InvalidLength { expected: 64, got: id.as_str().len() }
        })?;

        Change::check_from_buffer(bytes, &hash).map_err(|e| {
            VerifyError::HashMismatch {
                expected: id.clone(),
                got: e.to_string(),
            }
        })
    }
}

// ---------------------------------------------------------------------------
// RepoApplyHook
// ---------------------------------------------------------------------------

/// [`ApplyHook`] implementation that applies received changes to a
/// `IrRepository<FsChanges>` via [`IrRepository::apply_external_changes`].
///
/// Channel-safety: `apply` asserts that the `ChannelRef.channel` field matches
/// the repository's working channel. A sync event for channel X must not be
/// applied to a repo opened on channel Y — this would silently mis-apply
/// changes to the wrong history. The typed `ChannelMismatch` error is returned
/// on mismatch; the sync is aborted and B is left unchanged.
///
/// `IrRepository<FsChanges>` is not `Sync` (libpijul's `FileSystem` changestore
/// contains a `RefCell` cache and `IrRepository` uses `Cell` for the WC tip).
/// We take ownership of the repo and wrap it in a `Mutex` to satisfy the
/// `ApplyHook: Send + Sync` bound; `apply` holds the lock for the duration of
/// the call.
pub struct RepoApplyHook {
    repo: Mutex<IrRepository<FsChanges>>,
}

impl RepoApplyHook {
    /// Create an apply hook taking ownership of the repository.
    ///
    /// The repository must be opened on the channel that the sync events target.
    pub fn new(repo: IrRepository<FsChanges>) -> Self {
        Self { repo: Mutex::new(repo) }
    }

    /// Borrow the repo for non-`apply` operations (e.g., reading the log or
    /// materializing after sync). Panics if the mutex is poisoned.
    pub fn with_repo<R, F: FnOnce(&IrRepository<FsChanges>) -> R>(&self, f: F) -> R {
        f(&self.repo.lock().expect("repo mutex poisoned"))
    }
}

impl ApplyHook for RepoApplyHook {
    /// Apply `changes` (in dependency order) to the repository's working channel.
    ///
    /// # Errors
    ///
    /// - [`SyncError::RemoteRefused`] if `channel.channel` does not match the
    ///   repository's working channel (channel mismatch).
    /// - [`SyncError::Transport`] wrapping [`VcsError`] on apply failure.
    fn apply(&self, channel: &ChannelRef, changes: &[ChangeId]) -> Result<ChangeId, SyncError> {
        let repo = self.repo.lock().expect("repo mutex poisoned");
        let repo = &*repo;

        // Channel-mismatch guard: the sync event must target the same channel
        // that this repo is opened on.
        let repo_channel = repo.current_branch().to_string();
        if channel.channel != repo_channel {
            return Err(SyncError::RemoteRefused(format!(
                "channel mismatch: sync for '{}' but repo is on '{}'",
                channel.channel, repo_channel
            )));
        }

        // Convert ir-sync ChangeIds → nudox-ir-vcs ChangeHashHex.
        let hex_hashes: Vec<ChangeHashHex> = changes
            .iter()
            .map(|id| ChangeHashHex(id.as_str().to_owned()))
            .collect();

        repo.apply_external_changes(&hex_hashes)
            .map_err(|e| SyncError::Transport(e.to_string()))?;

        // Return the last applied change hash as the new tip. The announcement
        // order is dependency-respecting, so the last entry is the new tip.
        changes
            .last()
            .cloned()
            .ok_or_else(|| SyncError::Transport("apply called with empty changes list".into()))
    }
}

// ---------------------------------------------------------------------------
// merge_event
// ---------------------------------------------------------------------------

/// Construct a [`MergeEvent`] from the current state of `repo`.
///
/// `new_changes` is the slice of change hashes that are new since `prev_tip`:
/// - If `prev_tip` is `None`, all changes in the log are included.
/// - If `prev_tip` is `Some(h)`, all changes after `h` in the log are included.
///   Changes are listed in recorded order (oldest first), so the result is
///   already in dependency order.
///
/// `tip` is the current tip of the repo's working channel (the last hash in
/// the log, or the last entry in `new_changes`).
pub fn merge_event(
    repo: &IrRepository<FsChanges>,
    channel: ChannelRef,
    prev_tip: Option<&ChangeHashHex>,
) -> Result<MergeEvent, SyncGlueError> {
    let log = repo.log()?;

    let new_changes: Vec<ChangeId> = if let Some(prev) = prev_tip {
        // Find prev_tip in the log, then take everything after it.
        let pos = log.iter().position(|h| h.0 == prev.0);
        match pos {
            Some(idx) => log[idx + 1..].iter(),
            // prev_tip not found — include the full log (conservative).
            None => log[..].iter(),
        }
        .map(|h| h.0.parse::<ChangeId>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(SyncGlueError::InvalidChangeId)?
    } else {
        log.iter()
            .map(|h| h.0.parse::<ChangeId>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(SyncGlueError::InvalidChangeId)?
    };

    let tip = new_changes.last().cloned().ok_or(SyncGlueError::NothingToSync)?;

    Ok(MergeEvent {
        channel,
        tip,
        new_changes,
    })
}

// ---------------------------------------------------------------------------
// sync_merged
// ---------------------------------------------------------------------------

/// Push a [`MergeEvent`] to the remote via `syncer`.
///
/// Thin wrapper around [`crate::Syncer::on_merge`] so callers have a single
/// obvious entry point that matches the nudox-sync vocabulary.
///
/// Trust model: the receiver side enforces the enrollment gate (INDEX-PLAN
/// ID-18) — see [`trusted_provide_endpoint`] for the push-side capability check
/// and [`crate::SyncService::enroll`] for the accept-side allow-list.
pub async fn sync_merged<C: ChangeIo + 'static>(
    syncer: &crate::Syncer<C>,
    event: MergeEvent,
) -> Result<SyncAck, SyncGlueError> {
    syncer.on_merge(event).await.map_err(SyncGlueError::Sync)
}

// ---------------------------------------------------------------------------
// Trusted-remote gate (INDEX-PLAN ID-18)
// ---------------------------------------------------------------------------

/// The reason a trusted-remote provide was refused before any transfer.
#[derive(Debug, thiserror::Error)]
pub enum TrustGateError {
    /// The remote is on the trust list but not authorized to *receive* provides
    /// from this device (`can_provide = false`).
    #[error("trusted remote {name:?} is not authorized to receive provides")]
    NotAuthorizedToProvide {
        /// The remote's handle.
        name: String,
    },
}

/// Derive the endpoint string this device may push IR changes to, enforcing the
/// same capability gate as the ObjectPack plane (INDEX-PLAN ID-18): a device may
/// only `provide` to a trusted remote whose `can_provide` flag is set.
///
/// This mirrors `object_pack::transport::ProvideTarget::from_trusted_remote`;
/// the two planes share one trust policy. Returns the remote's endpoint id
/// string (the caller resolves it to an `iroh::EndpointId` /
/// `crate::RemoteConfig`).
///
/// # Errors
///
/// [`TrustGateError::NotAuthorizedToProvide`] when `remote.can_provide` is false.
pub fn trusted_provide_endpoint(
    remote: &heart::deployment::TrustedRemote,
) -> Result<String, TrustGateError> {
    if !remote.can_provide {
        return Err(TrustGateError::NotAuthorizedToProvide {
            name: remote.name.clone(),
        });
    }
    Ok(remote.endpoint.clone())
}
