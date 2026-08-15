//! Repo-side glue: `FsChangeIo` + `RepoApplyHook` implementing `heart::sync` traits.

use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use libpijul::change::Change;
use libpijul::changestore::filesystem::FileSystem as FsChanges;
use libpijul::pristine::Base32;

use heart::sync::{ApplyHook as HeartApplyHook, ContentIo, SyncError, VerifyError};

use crate::repo::{ChangeHashHex, IrRepository};

use super::types::{ChangeId, ChannelRef, MergeEvent, SyncAck};

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "channel mismatch: sync is for channel '{sync_channel}' but repo is on '{repo_channel}'"
    )]
    ChannelMismatch {
        sync_channel: String,
        repo_channel: String,
    },

    #[error("vcs error: {0}")]
    Vcs(#[from] crate::error::Error),

    #[error("sync error: {0}")]
    Sync(#[from] SyncError),

    #[error("invalid change id: {0}")]
    InvalidChangeId(#[from] VerifyError),

    #[error("nothing to sync: no changes newer than the previous tip")]
    NothingToSync,
}

#[derive(Debug, thiserror::Error)]
pub enum TrustGateError {
    #[error("trusted remote {name:?} is not authorized to receive provides")]
    NotAuthorizedToProvide { name: String },
}

// ---------------------------------------------------------------------------
// hex↔Hash helpers
// ---------------------------------------------------------------------------

fn change_id_to_pijul_hash(id: &ChangeId) -> Result<libpijul::pristine::Hash, Error> {
    let hex = id.as_str();
    if hex.len() != 64 {
        return Err(Error::InvalidChangeId(VerifyError::InvalidLength {
            expected: 64,
            got: hex.len(),
        }));
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16).ok_or_else(|| {
            Error::InvalidChangeId(VerifyError::InvalidChar(chunk[0] as char))
        })? as u8;
        let lo = (chunk[1] as char).to_digit(16).ok_or_else(|| {
            Error::InvalidChangeId(VerifyError::InvalidChar(chunk[1] as char))
        })? as u8;
        bytes[i] = (hi << 4) | lo;
    }
    Ok(libpijul::pristine::Hash::Blake3(bytes))
}

// ---------------------------------------------------------------------------
// FsChangeIo
// ---------------------------------------------------------------------------

/// [`ContentIo`] implementation over the filesystem changestore used by
/// `IrRepository<FsChanges>`.
pub struct FsChangeIo {
    changes_root: PathBuf,
}

impl FsChangeIo {
    pub fn for_repo(repo_root: &std::path::Path) -> Self {
        let changes_dir = repo_root.join("changes").join(".pijul").join("changes");
        Self {
            changes_root: changes_dir,
        }
    }

    pub fn new(changes_root: PathBuf) -> Self {
        Self { changes_root }
    }

    fn path_for(&self, id: &ChangeId) -> Result<PathBuf, Error> {
        let hash = change_id_to_pijul_hash(id)?;
        let b32 = hash.to_base32();
        let (prefix, rest) = b32.split_at(2);
        let mut path = self.changes_root.clone();
        path.push(prefix);
        path.push(rest);
        path.set_extension("change");
        Ok(path)
    }
}

impl ContentIo for FsChangeIo {
    type Id = ChangeId;

    fn read(&self, id: &ChangeId) -> io::Result<Vec<u8>> {
        let path = self
            .path_for(id)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        std::fs::read(&path)
    }

    fn write(&self, id: &ChangeId, bytes: &[u8]) -> io::Result<()> {
        // Contract: verify before write.
        self.verify(id, bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let path = self
            .path_for(id)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(&self.changes_root))?;
        use std::io::Write;
        tmp.write_all(bytes)?;
        tmp.persist(&path).map_err(|e| e.error)?;
        Ok(())
    }

    fn has(&self, id: &ChangeId) -> io::Result<bool> {
        let path = self
            .path_for(id)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        Ok(path.exists())
    }

    /// Full pijul verification: `Change::check_from_buffer` recomputes Blake3
    /// over the canonical representation and returns an error on mismatch.
    fn verify(&self, id: &ChangeId, bytes: &[u8]) -> Result<(), VerifyError> {
        let hash = change_id_to_pijul_hash(id).map_err(|_| VerifyError::InvalidLength {
            expected: 64,
            got: id.as_str().len(),
        })?;

        Change::check_from_buffer(bytes, &hash).map_err(|e| VerifyError::HashMismatch {
            expected: id.to_string(),
            got: e.to_string(),
        })
    }

    fn max_item_bytes(&self) -> usize {
        super::MAX_CHANGE_BYTES
    }
}

// ---------------------------------------------------------------------------
// RepoApplyHook
// ---------------------------------------------------------------------------

pub struct RepoApplyHook {
    repo: Mutex<IrRepository<FsChanges>>,
}

impl RepoApplyHook {
    pub fn new(repo: IrRepository<FsChanges>) -> Self {
        Self {
            repo: Mutex::new(repo),
        }
    }

    pub fn with_repo<R, F: FnOnce(&IrRepository<FsChanges>) -> R>(&self, f: F) -> R {
        f(&self.repo.lock().expect("repo mutex poisoned"))
    }
}

impl HeartApplyHook for RepoApplyHook {
    type Id = ChangeId;
    type Target = ChannelRef;
    type Tip = ChangeId;

    fn apply(&self, channel: &ChannelRef, changes: &[ChangeId]) -> Result<ChangeId, SyncError> {
        let repo = self.repo.lock().expect("repo mutex poisoned");
        let repo = &*repo;

        let repo_channel = repo.current_branch().to_string();
        if channel.channel != repo_channel {
            return Err(SyncError::RemoteRefused(format!(
                "channel mismatch: sync for '{}' but repo is on '{}'",
                channel.channel, repo_channel
            )));
        }

        let hex_hashes: Vec<ChangeHashHex> = changes
            .iter()
            .map(|id| ChangeHashHex(id.as_str().to_owned()))
            .collect();

        repo.apply_external_changes(&hex_hashes)
            .map_err(|e| SyncError::Io(io::Error::other(e)))?;

        changes
            .last()
            .cloned()
            .ok_or_else(|| SyncError::Other("apply called with empty changes list".into()))
    }
}

// ---------------------------------------------------------------------------
// merge_event
// ---------------------------------------------------------------------------

pub fn merge_event(
    repo: &IrRepository<FsChanges>,
    channel: ChannelRef,
    prev_tip: Option<&ChangeHashHex>,
) -> Result<MergeEvent, Error> {
    let log = repo.log()?;

    let new_changes: Vec<ChangeId> = if let Some(prev) = prev_tip {
        let pos = log.iter().position(|h| h.0 == prev.0);
        pos.map_or_else(|| log[..].iter(), |idx| log[idx + 1..].iter())
            .map(|h| h.0.parse::<ChangeId>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::InvalidChangeId)?
    } else {
        log.iter()
            .map(|h| h.0.parse::<ChangeId>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::InvalidChangeId)?
    };

    let tip = new_changes
        .last()
        .cloned()
        .ok_or(Error::NothingToSync)?;

    Ok(MergeEvent {
        channel,
        tip,
        new_changes,
    })
}

// ---------------------------------------------------------------------------
// sync_merged
// ---------------------------------------------------------------------------

pub async fn sync_merged<C: ContentIo<Id = ChangeId> + 'static>(
    syncer: &super::transport::Syncer<C>,
    event: MergeEvent,
) -> Result<SyncAck, Error> {
    syncer.on_merge(event).await.map_err(Error::Sync)
}

// ---------------------------------------------------------------------------
// Trusted-remote gate (INDEX-PLAN ID-18)
// ---------------------------------------------------------------------------

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
